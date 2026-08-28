//! UDP hole punch for one sealed socket.
//!
//! Both sides already hold the shared keys (`call_crypto`) and each
//! other's candidate list (host + STUN-reflexive addresses, exchanged
//! through central). The punch sprays sealed PING datagrams at every
//! remote candidate on a fast-then-slower cadence and listens with
//! `recv_probe`: any datagram that *opens* proves a live, authenticated
//! path from its observed source — either the peer's own PING reaching us
//! (which we reflect, so the peer learns the same) or our echo coming
//! back. After the first validated source we keep listening for a short
//! grace window so a LAN (host) path can win over a NAT (srflx) one, then
//! lock the peer address into the socket.
//!
//! Why it works on cone NATs: the reflexive address was obtained by
//! *this* socket, cone NATs reuse the mapping for every destination, and
//! our outbound PINGs open the inbound pinhole. Why symmetric NATs fail:
//! they allocate a fresh external port per destination, so the STUN-seen
//! port is not the one used toward the peer. v1 has no relay; that pair
//! ends in `PunchError::NoPath`.

use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use super::media_socket::MediaSocket;
use super::packet::{UdpAudioPacket, PACKET_TYPE_PING, SENDER_ID_SIZE};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CandidateKind {
    Host,
    Srflx,
}

impl CandidateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            CandidateKind::Host => "host",
            CandidateKind::Srflx => "srflx",
        }
    }
}

#[derive(Clone, Debug)]
pub struct RemoteCandidate {
    pub addr: SocketAddr,
    pub kind: CandidateKind,
}

#[derive(Debug)]
pub struct PunchResult {
    pub peer: SocketAddr,
    pub kind: CandidateKind,
    /// Round trip measured from our own reflected PING, when one came back.
    pub rtt_ms: Option<f64>,
}

#[derive(Debug)]
pub enum PunchError {
    NoPath,
    Aborted,
    Io(String),
}

/// Grace window after the first validated path during which a host
/// (LAN) path may still displace a reflexive one.
const GRACE: Duration = Duration::from_millis(300);
const RECV_SLICE: Duration = Duration::from_millis(10);

fn tick_interval(tick: u32) -> Duration {
    if tick < 10 {
        Duration::from_millis(50)
    } else if tick < 20 {
        Duration::from_millis(200)
    } else {
        Duration::from_millis(500)
    }
}

pub fn is_private_v4(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_loopback() || v4.is_link_local(),
        IpAddr::V6(_) => false,
    }
}

fn now_ns() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0)
}

/// Run the punch on the calling (blocking) thread. `deadline` bounds the
/// whole attempt; `stop` aborts early (hang-up while connecting).
pub fn punch(
    sock: &MediaSocket,
    self_id: &str,
    remote: &[RemoteCandidate],
    deadline: Duration,
    stop: &AtomicBool,
) -> Result<PunchResult, PunchError> {
    if remote.is_empty() {
        return Err(PunchError::NoPath);
    }
    sock.set_read_timeout(Some(RECV_SLICE))
        .map_err(|e| PunchError::Io(e.to_string()))?;

    let start = Instant::now();
    let mut tick = 0u32;
    let mut next_send = start;
    let mut validated: Vec<(SocketAddr, CandidateKind, Option<f64>)> = Vec::new();
    let mut first_validated_at: Option<Instant> = None;
    let mut self_padded = [0u8; SENDER_ID_SIZE];
    let b = self_id.as_bytes();
    let n = b.len().min(SENDER_ID_SIZE);
    self_padded[..n].copy_from_slice(&b[..n]);
    let mut buf = [0u8; 1500];

    loop {
        if stop.load(Ordering::Relaxed) {
            return Err(PunchError::Aborted);
        }
        let now = Instant::now();
        if now.duration_since(start) >= deadline {
            break;
        }
        if let Some(t0) = first_validated_at {
            if now.duration_since(t0) >= GRACE {
                break;
            }
        }
        if now >= next_send {
            let ping = UdpAudioPacket::new_ping(self_id, now_ns()).to_bytes();
            for c in remote {
                let _ = sock.send_probe_to(&ping, c.addr);
            }
            next_send = now + tick_interval(tick);
            tick += 1;
        }
        match sock.recv_probe(&mut buf) {
            Ok((len, src)) => {
                let inner = &buf[..len];
                let is_ping = len > SENDER_ID_SIZE && inner[0] == PACKET_TYPE_PING;
                let from_self = is_ping && inner[1..1 + SENDER_ID_SIZE] == self_padded;
                let mut rtt = None;
                if is_ping && !from_self {
                    // Peer's probe: reflect so it validates this path too.
                    let _ = sock.send_probe_to(inner, src);
                } else if from_self && len >= 37 + 8 {
                    let sent = u64::from_le_bytes(inner[37..45].try_into().unwrap_or([0; 8]));
                    let now_ns = now_ns();
                    if now_ns > sent {
                        rtt = Some((now_ns - sent) as f64 / 1_000_000.0);
                    }
                }
                let kind = remote
                    .iter()
                    .find(|c| c.addr == src)
                    .map(|c| c.kind)
                    .unwrap_or(CandidateKind::Srflx);
                match validated.iter_mut().find(|(a, _, _)| *a == src) {
                    Some(entry) => {
                        if entry.2.is_none() {
                            entry.2 = rtt;
                        }
                    }
                    None => {
                        log::info!("[punch] validated path {} ({})", src, kind.as_str());
                        validated.push((src, kind, rtt));
                        if first_validated_at.is_none() {
                            first_validated_at = Some(Instant::now());
                        }
                    }
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::ConnectionReset => {}
            Err(e) => return Err(PunchError::Io(e.to_string())),
        }
    }

    if validated.is_empty() {
        return Err(PunchError::NoPath);
    }
    // Prefer a matched host candidate on a private address, else the
    // first path that proved itself.
    let best = validated
        .iter()
        .find(|(a, k, _)| *k == CandidateKind::Host && is_private_v4(a.ip()))
        .or_else(|| validated.first())
        .cloned()
        .expect("non-empty");
    sock.set_peer(best.0);
    Ok(PunchResult { peer: best.0, kind: best.1, rtt_ms: best.2 })
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::media_socket::SocketKeys;
    use std::net::UdpSocket;
    use std::sync::Arc;

    fn mirrored() -> (SocketKeys, SocketKeys) {
        let ka = SocketKeys { tx_key: [1; 32], rx_key: [2; 32], tx_salt: [1; 4], rx_salt: [2; 4] };
        let kb = SocketKeys { tx_key: [2; 32], rx_key: [1; 32], tx_salt: [2; 4], rx_salt: [1; 4] };
        (ka, kb)
    }

    #[test]
    fn loopback_two_sockets_connect_host_only() {
        let (ka, kb) = mirrored();
        let a = Arc::new(MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), ka, "alice"));
        let b = Arc::new(MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), kb, "bob"));
        let a_addr = a.local_addr().unwrap();
        let b_addr = b.local_addr().unwrap();
        let stop = Arc::new(AtomicBool::new(false));
        // Decoy candidate nobody listens on, to prove selection ignores it.
        let decoy: SocketAddr = "127.0.0.1:9".parse().unwrap();

        let (sa, sb) = (stop.clone(), stop.clone());
        let ta = {
            let a = a.clone();
            std::thread::spawn(move || {
                punch(
                    &a,
                    "alice",
                    &[
                        RemoteCandidate { addr: decoy, kind: CandidateKind::Srflx },
                        RemoteCandidate { addr: b_addr, kind: CandidateKind::Host },
                    ],
                    Duration::from_secs(3),
                    &sa,
                )
            })
        };
        let tb = {
            let b = b.clone();
            std::thread::spawn(move || {
                punch(
                    &b,
                    "bob",
                    &[RemoteCandidate { addr: a_addr, kind: CandidateKind::Host }],
                    Duration::from_secs(3),
                    &sb,
                )
            })
        };
        let ra = ta.join().unwrap().unwrap();
        let rb = tb.join().unwrap().unwrap();
        assert_eq!(ra.peer, b_addr);
        assert_eq!(rb.peer, a_addr);
        assert_eq!(ra.kind, CandidateKind::Host);
        assert_eq!(a.peer(), Some(b_addr));
        assert_eq!(b.peer(), Some(a_addr));
        // The locked sockets now exchange normal datagrams.
        a.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        b.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let pkt = UdpAudioPacket::new_audio("alice", 1, b"voice").unwrap().to_bytes();
        a.send(&pkt).unwrap();
        let mut buf = [0u8; 1500];
        let n = b.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &pkt[..]);
    }

    #[test]
    fn times_out_without_peer() {
        let (ka, _) = mirrored();
        let a = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), ka, "alice");
        let stop = AtomicBool::new(false);
        let dead: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let t = Instant::now();
        let r = punch(
            &a,
            "alice",
            &[RemoteCandidate { addr: dead, kind: CandidateKind::Host }],
            Duration::from_millis(400),
            &stop,
        );
        assert!(matches!(r, Err(PunchError::NoPath)));
        assert!(t.elapsed() < Duration::from_secs(2));
    }

    #[test]
    fn aborts_on_stop_flag() {
        let (ka, _) = mirrored();
        let a = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), ka, "alice");
        let stop = AtomicBool::new(true);
        let dead: SocketAddr = "127.0.0.1:9".parse().unwrap();
        let r = punch(
            &a,
            "alice",
            &[RemoteCandidate { addr: dead, kind: CandidateKind::Host }],
            Duration::from_secs(5),
            &stop,
        );
        assert!(matches!(r, Err(PunchError::Aborted)));
    }
}
