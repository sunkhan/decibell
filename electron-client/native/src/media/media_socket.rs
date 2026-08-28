//! Transport wrapper for the voice + media UDP sockets.
//!
//! `Plain` is a zero-cost passthrough used on the community-relay path —
//! byte-identical to the bare `UdpSocket` it replaces. `Sealed` is the P2P
//! DM-call transport: every datagram is AES-256-GCM sealed with a
//! per-direction key, carries a 64-bit counter as its nonce, and is
//! replay-checked on receipt. The *inner* plaintext is the unchanged
//! existing datagram (type byte + `sender_id` + …), so the audio pipeline,
//! the video receiver, PING/RTT, NACK, PLI and FEC all work peer-to-peer
//! without knowing which mode they run on.
//!
//! Outer envelope (Sealed):
//!
//!   [0xE5][u64 LE counter][ciphertext][16-byte GCM tag]     = 25 B overhead
//!
//! Nonce = 4-byte direction salt ‖ 8-byte counter (salt is derived, never
//! sent). AAD = the 9-byte outer header. `0xE5` has its top two bits set so
//! a late STUN response (top bits 00) on the same socket can never be
//! mistaken for a sealed datagram.
//!
//! Peer address: a sealed socket is never `connect()`ed. It learns the
//! peer from the source of the last *authenticated* datagram (the same
//! endpoint-learning the community relay does) and sends there. Because
//! only the key holder can produce a datagram that opens, a spoofed
//! source can't hijack the path; a captured datagram can't either (replay
//! window). If both a LAN and a NAT path work, whichever the peer settles
//! on wins, and a silent peer address is abandoned after
//! `PEER_MIGRATE_SILENCE` so a mid-call path change is followed.
//!
//! PING reflection: the community server echoes PING (type 5) datagrams so
//! clients measure RTT and keep NAT bindings warm. In Sealed mode `recv`
//! reflects a peer's PING straight back (re-sealed) and never surfaces it,
//! while our own echo is returned to the caller — so the pipeline's RTT
//! logic and 3 s keepalives work unchanged, and the reflection doubles as
//! the hole-punch responder.

use std::io;
use std::net::{SocketAddr, UdpSocket};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use aes_gcm::aead::{AeadInPlace, KeyInit};
use aes_gcm::{Aes256Gcm, Nonce, Tag};

use super::packet::{PACKET_TYPE_PING, SENDER_ID_SIZE};

pub const OUTER_TYPE_SEALED: u8 = 0xE5;
const OUTER_HEADER: usize = 1 + 8;
const TAG_LEN: usize = 16;
/// Bytes a sealed datagram adds on top of the inner packet.
pub const SEAL_OVERHEAD: usize = OUTER_HEADER + TAG_LEN;
/// Largest inner datagram we ever carry (legacy fixed audio size).
const MAX_INNER: usize = 1437;
const MAX_OUTER: usize = MAX_INNER + SEAL_OVERHEAD;
/// Counters this far behind the highest seen are rejected outright.
const REPLAY_WINDOW: u64 = 1024;
/// Highest counter we will ever send — refuse past this rather than
/// risk a nonce reuse.
const TX_COUNTER_LIMIT: u64 = 1 << 63;
/// A learned peer address that has been silent this long is replaced by
/// the source of the next authenticated datagram.
const PEER_MIGRATE_SILENCE: Duration = Duration::from_secs(2);

/// Per-socket keying material. `tx_*` seals what we send, `rx_*` opens
/// what the peer sends — mirrored on the other side.
#[derive(Clone)]
pub struct SocketKeys {
    pub tx_key: [u8; 32],
    pub rx_key: [u8; 32],
    pub tx_salt: [u8; 4],
    pub rx_salt: [u8; 4],
}

/// 1024-bit sliding replay window keyed on the sender's counter.
struct ReplayWindow {
    seen_any: bool,
    highest: u64,
    /// bit i (LSB-first across words) == counter `highest - i` was seen.
    bits: [u64; 16],
}

impl ReplayWindow {
    fn new() -> Self {
        Self { seen_any: false, highest: 0, bits: [0; 16] }
    }

    fn get(&self, i: u64) -> bool {
        let i = i as usize;
        (self.bits[i / 64] >> (i % 64)) & 1 == 1
    }

    fn set(&mut self, i: u64) {
        let i = i as usize;
        self.bits[i / 64] |= 1u64 << (i % 64);
    }

    /// Shift the whole 1024-bit map towards older indices by `n`.
    fn shift_up(&mut self, n: u64) {
        if n >= REPLAY_WINDOW {
            self.bits = [0; 16];
            return;
        }
        let n = n as usize;
        let ws = n / 64;
        let bs = n % 64;
        let mut out = [0u64; 16];
        for w in (0..16).rev() {
            if w < ws {
                break;
            }
            let src = w - ws;
            let mut v = self.bits[src] << bs;
            if bs > 0 && src >= 1 {
                v |= self.bits[src - 1] >> (64 - bs);
            }
            out[w] = v;
        }
        self.bits = out;
    }

    /// Accept (and record) `ctr`, or reject a replay / too-old counter.
    /// Only call after the datagram authenticated — an attacker must not
    /// be able to poison the window with forged counters.
    fn accept(&mut self, ctr: u64) -> bool {
        if !self.seen_any {
            self.seen_any = true;
            self.highest = ctr;
            self.bits = [0; 16];
            self.set(0);
            return true;
        }
        if ctr > self.highest {
            self.shift_up(ctr - self.highest);
            self.highest = ctr;
            self.set(0);
            return true;
        }
        let behind = self.highest - ctr;
        if behind >= REPLAY_WINDOW || self.get(behind) {
            return false;
        }
        self.set(behind);
        true
    }
}

struct SealState {
    tx: Aes256Gcm,
    rx: Aes256Gcm,
    tx_salt: [u8; 4],
    rx_salt: [u8; 4],
    tx_ctr: AtomicU64,
    rx_win: Mutex<ReplayWindow>,
    /// Where `send` goes: the source of the last authenticated datagram
    /// (or what the punch phase locked in). None until then.
    peer: Mutex<Option<(SocketAddr, Instant)>>,
}

impl SealState {
    fn nonce(salt: &[u8; 4], ctr: u64) -> Nonce<aes_gcm::aead::consts::U12> {
        let mut n = [0u8; 12];
        n[..4].copy_from_slice(salt);
        n[4..].copy_from_slice(&ctr.to_be_bytes());
        *Nonce::from_slice(&n)
    }

    /// Seal `inner` into `out`; returns the outer length.
    fn seal(&self, inner: &[u8], out: &mut [u8; MAX_OUTER]) -> io::Result<usize> {
        if inner.len() > MAX_INNER {
            return Err(io::Error::new(io::ErrorKind::InvalidInput, "datagram too large to seal"));
        }
        let ctr = self.tx_ctr.fetch_add(1, Ordering::Relaxed);
        if ctr >= TX_COUNTER_LIMIT {
            return Err(io::Error::new(io::ErrorKind::Other, "sealed counter exhausted"));
        }
        out[0] = OUTER_TYPE_SEALED;
        out[1..9].copy_from_slice(&ctr.to_le_bytes());
        let body_end = OUTER_HEADER + inner.len();
        out[OUTER_HEADER..body_end].copy_from_slice(inner);
        let mut aad = [0u8; OUTER_HEADER];
        aad.copy_from_slice(&out[..OUTER_HEADER]);
        let tag = self
            .tx
            .encrypt_in_place_detached(&Self::nonce(&self.tx_salt, ctr), &aad, &mut out[OUTER_HEADER..body_end])
            .map_err(|_| io::Error::new(io::ErrorKind::Other, "seal failed"))?;
        out[body_end..body_end + TAG_LEN].copy_from_slice(tag.as_slice());
        Ok(body_end + TAG_LEN)
    }

    /// Open a sealed datagram in place. On success the inner plaintext is
    /// `buf[OUTER_HEADER..OUTER_HEADER + len]`.
    fn open(&self, buf: &mut [u8]) -> Option<usize> {
        let n = buf.len();
        if n < SEAL_OVERHEAD || buf[0] != OUTER_TYPE_SEALED {
            return None;
        }
        let ctr = u64::from_le_bytes(buf[1..9].try_into().ok()?);
        let mut aad = [0u8; OUTER_HEADER];
        aad.copy_from_slice(&buf[..OUTER_HEADER]);
        let tag = *Tag::<aes_gcm::aead::consts::U16>::from_slice(&buf[n - TAG_LEN..n]);
        self.rx
            .decrypt_in_place_detached(&Self::nonce(&self.rx_salt, ctr), &aad, &mut buf[OUTER_HEADER..n - TAG_LEN], &tag)
            .ok()?;
        if !self.rx_win.lock().ok()?.accept(ctr) {
            return None;
        }
        Some(n - SEAL_OVERHEAD)
    }
}

enum Mode {
    Plain,
    Sealed(Box<SealState>),
}

pub struct MediaSocket {
    inner: UdpSocket,
    mode: Mode,
    /// Our `sender_id` as it appears on the wire (NUL-padded to 32 bytes),
    /// used to tell our own PING echo from a peer's PING.
    self_id: [u8; SENDER_ID_SIZE],
    epoch: Instant,
    /// Millis since `epoch` of the last authenticated datagram (Sealed) or
    /// any datagram (Plain). Feeds the peer-loss watchdog.
    last_rx_ms: AtomicU64,
}

fn pad_id(id: &str) -> [u8; SENDER_ID_SIZE] {
    let mut out = [0u8; SENDER_ID_SIZE];
    let b = id.as_bytes();
    let n = b.len().min(SENDER_ID_SIZE);
    out[..n].copy_from_slice(&b[..n]);
    out
}

impl MediaSocket {
    /// Community-relay mode: a transparent wrapper around a connected socket.
    pub fn plain(socket: UdpSocket, self_id: &str) -> Self {
        Self::build(socket, Mode::Plain, self_id)
    }

    /// P2P mode: seal/open every datagram with `keys`. The socket must be
    /// bound but NOT connected — the peer address is learned (see module
    /// docs) or set by the punch phase via `set_peer`.
    pub fn sealed(socket: UdpSocket, keys: SocketKeys, self_id: &str) -> Self {
        let st = SealState {
            tx: Aes256Gcm::new_from_slice(&keys.tx_key).expect("32-byte key"),
            rx: Aes256Gcm::new_from_slice(&keys.rx_key).expect("32-byte key"),
            tx_salt: keys.tx_salt,
            rx_salt: keys.rx_salt,
            tx_ctr: AtomicU64::new(0),
            rx_win: Mutex::new(ReplayWindow::new()),
            peer: Mutex::new(None),
        };
        Self::build(socket, Mode::Sealed(Box::new(st)), self_id)
    }

    fn build(socket: UdpSocket, mode: Mode, self_id: &str) -> Self {
        Self {
            inner: socket,
            mode,
            self_id: pad_id(self_id),
            epoch: Instant::now(),
            last_rx_ms: AtomicU64::new(0),
        }
    }

    pub fn is_sealed(&self) -> bool {
        matches!(self.mode, Mode::Sealed(_))
    }

    fn touch_rx(&self) {
        self.last_rx_ms
            .store(self.epoch.elapsed().as_millis() as u64, Ordering::Relaxed);
    }

    /// Time since the last (authenticated, when sealed) datagram arrived.
    pub fn last_rx_age(&self) -> Duration {
        let now = self.epoch.elapsed().as_millis() as u64;
        Duration::from_millis(now.saturating_sub(self.last_rx_ms.load(Ordering::Relaxed)))
    }

    pub fn set_read_timeout(&self, d: Option<Duration>) -> io::Result<()> {
        self.inner.set_read_timeout(d)
    }

    pub fn local_addr(&self) -> io::Result<SocketAddr> {
        self.inner.local_addr()
    }

    /// Sealed only: the currently learned/locked peer address.
    pub fn peer(&self) -> Option<SocketAddr> {
        match &self.mode {
            Mode::Plain => self.inner.peer_addr().ok(),
            Mode::Sealed(st) => st.peer.lock().ok().and_then(|p| p.map(|(a, _)| a)),
        }
    }

    /// Sealed only: lock the peer address (punch outcome).
    pub fn set_peer(&self, addr: SocketAddr) {
        if let Mode::Sealed(st) = &self.mode {
            if let Ok(mut p) = st.peer.lock() {
                *p = Some((addr, Instant::now()));
            }
        }
    }

    /// Send one inner datagram to the peer.
    pub fn send(&self, inner: &[u8]) -> io::Result<usize> {
        match &self.mode {
            Mode::Plain => self.inner.send(inner),
            Mode::Sealed(st) => {
                let peer = st
                    .peer
                    .lock()
                    .ok()
                    .and_then(|p| p.map(|(a, _)| a))
                    .ok_or_else(|| io::Error::new(io::ErrorKind::NotConnected, "no peer yet"))?;
                let mut out = [0u8; MAX_OUTER];
                let n = st.seal(inner, &mut out)?;
                self.inner.send_to(&out[..n], peer)
            }
        }
    }

    /// Punch phase (Sealed only): seal `inner` and send it to an explicit
    /// candidate address without touching the learned peer.
    pub fn send_probe_to(&self, inner: &[u8], to: SocketAddr) -> io::Result<usize> {
        match &self.mode {
            Mode::Plain => Err(io::Error::new(io::ErrorKind::Unsupported, "plain socket")),
            Mode::Sealed(st) => {
                let mut out = [0u8; MAX_OUTER];
                let n = st.seal(inner, &mut out)?;
                self.inner.send_to(&out[..n], to)
            }
        }
    }

    /// Punch phase (Sealed only): receive + open one datagram, returning
    /// the inner bytes and the observed source. Unauthenticated datagrams
    /// are skipped. No PING reflection, no peer learning — the punch loop
    /// decides both.
    pub fn recv_probe(&self, buf: &mut [u8]) -> io::Result<(usize, SocketAddr)> {
        let st = match &self.mode {
            Mode::Plain => return Err(io::Error::new(io::ErrorKind::Unsupported, "plain socket")),
            Mode::Sealed(st) => st,
        };
        let mut outer = [0u8; MAX_OUTER];
        loop {
            let (n, src) = self.inner.recv_from(&mut outer)?;
            if let Some(len) = st.open(&mut outer[..n]) {
                self.touch_rx();
                let len = len.min(buf.len());
                buf[..len].copy_from_slice(&outer[OUTER_HEADER..OUTER_HEADER + len]);
                return Ok((len, src));
            }
        }
    }

    /// Receive one inner datagram from the peer. In Sealed mode this loops
    /// past anything that fails to authenticate, reflects peer PINGs, and
    /// learns/migrates the peer address (see module docs). Timeouts and
    /// WouldBlock propagate exactly like `UdpSocket::recv`.
    pub fn recv(&self, buf: &mut [u8]) -> io::Result<usize> {
        let st = match &self.mode {
            Mode::Plain => {
                let n = self.inner.recv(buf)?;
                self.touch_rx();
                return Ok(n);
            }
            Mode::Sealed(st) => st,
        };
        let mut outer = [0u8; MAX_OUTER];
        loop {
            let (n, src) = self.inner.recv_from(&mut outer)?;
            let Some(len) = st.open(&mut outer[..n]) else {
                continue;
            };
            self.touch_rx();
            self.learn_peer(st, src);
            let inner = &outer[OUTER_HEADER..OUTER_HEADER + len];
            if self.is_peer_ping(inner) {
                // Echo it back re-sealed (fresh counter) to the address it
                // came from; never surface it to the pipeline.
                let mut echo = [0u8; MAX_OUTER];
                if let Ok(m) = st.seal(inner, &mut echo) {
                    let _ = self.inner.send_to(&echo[..m], src);
                }
                continue;
            }
            let len = len.min(buf.len());
            buf[..len].copy_from_slice(&outer[OUTER_HEADER..OUTER_HEADER + len]);
            return Ok(len);
        }
    }

    fn is_peer_ping(&self, inner: &[u8]) -> bool {
        inner.len() > SENDER_ID_SIZE
            && inner[0] == PACKET_TYPE_PING
            && inner[1..1 + SENDER_ID_SIZE] != self.self_id
    }

    fn learn_peer(&self, st: &SealState, src: SocketAddr) {
        let Ok(mut p) = st.peer.lock() else { return };
        let now = Instant::now();
        match *p {
            None => {
                log::info!("[media-socket] peer learned: {}", src);
                *p = Some((src, now));
            }
            Some((cur, _)) if cur == src => {
                *p = Some((cur, now));
            }
            Some((cur, last)) => {
                if now.duration_since(last) > PEER_MIGRATE_SILENCE {
                    log::info!("[media-socket] peer migrated {} -> {}", cur, src);
                    *p = Some((src, now));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use super::super::packet::UdpAudioPacket;

    fn keys(a: u8, b: u8) -> (SocketKeys, SocketKeys) {
        let ka = SocketKeys {
            tx_key: [a; 32],
            rx_key: [b; 32],
            tx_salt: [a; 4],
            rx_salt: [b; 4],
        };
        let kb = SocketKeys {
            tx_key: [b; 32],
            rx_key: [a; 32],
            tx_salt: [b; 4],
            rx_salt: [a; 4],
        };
        (ka, kb)
    }

    fn pair() -> (MediaSocket, MediaSocket) {
        let (ka, kb) = keys(1, 2);
        let a = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), ka, "alice");
        let b = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), kb, "bob");
        a.set_peer(b.local_addr().unwrap());
        b.set_peer(a.local_addr().unwrap());
        a.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        b.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        (a, b)
    }

    #[test]
    fn seal_open_roundtrip() {
        let (a, b) = pair();
        let pkt = UdpAudioPacket::new_audio("alice", 7, b"hello opus").to_bytes();
        a.send(&pkt).unwrap();
        let mut buf = [0u8; 1500];
        let n = b.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &pkt[..]);
    }

    #[test]
    fn plain_passthrough_bytes_identical() {
        let a = UdpSocket::bind("127.0.0.1:0").unwrap();
        let b = UdpSocket::bind("127.0.0.1:0").unwrap();
        a.connect(b.local_addr().unwrap()).unwrap();
        let raw_b = b.try_clone().unwrap();
        let a = MediaSocket::plain(a, "alice");
        a.send(b"raw bytes").unwrap();
        let mut buf = [0u8; 64];
        raw_b.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        let n = raw_b.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], b"raw bytes");
    }

    #[test]
    fn tamper_rejected() {
        let (ka, kb) = keys(3, 4);
        let a = SealState {
            tx: Aes256Gcm::new_from_slice(&ka.tx_key).unwrap(),
            rx: Aes256Gcm::new_from_slice(&ka.rx_key).unwrap(),
            tx_salt: ka.tx_salt,
            rx_salt: ka.rx_salt,
            tx_ctr: AtomicU64::new(0),
            rx_win: Mutex::new(ReplayWindow::new()),
            peer: Mutex::new(None),
        };
        let b = SealState {
            tx: Aes256Gcm::new_from_slice(&kb.tx_key).unwrap(),
            rx: Aes256Gcm::new_from_slice(&kb.rx_key).unwrap(),
            tx_salt: kb.tx_salt,
            rx_salt: kb.rx_salt,
            tx_ctr: AtomicU64::new(0),
            rx_win: Mutex::new(ReplayWindow::new()),
            peer: Mutex::new(None),
        };
        let mut out = [0u8; MAX_OUTER];
        let n = a.seal(b"payload", &mut out).unwrap();
        let mut ok = out[..n].to_vec();
        assert_eq!(b.open(&mut ok), Some(7));
        // flip a ciphertext bit
        let mut bad = out[..n].to_vec();
        bad[OUTER_HEADER] ^= 0x01;
        assert_eq!(b.open(&mut bad), None);
        // wrong outer type
        let mut bad2 = out[..n].to_vec();
        bad2[0] = 0x00;
        assert_eq!(b.open(&mut bad2), None);
        // truncated
        let mut short = out[..SEAL_OVERHEAD - 1].to_vec();
        assert_eq!(b.open(&mut short), None);
    }

    #[test]
    fn replay_rejected_and_reorder_within_window_ok() {
        let mut w = ReplayWindow::new();
        assert!(w.accept(10));
        assert!(!w.accept(10)); // exact replay
        assert!(w.accept(12));
        assert!(w.accept(11)); // late but unseen
        assert!(!w.accept(11)); // replayed late packet
        assert!(w.accept(12 + 500)); // jump ahead within window
        assert!(w.accept(12 + 1)); // still inside the 1024 window
        assert!(!w.accept(12)); // seen before the jump
        assert!(w.accept(12 + 500 + 5000)); // big jump clears the map
        assert!(!w.accept(12 + 500)); // now too old
    }

    #[test]
    fn ping_reflected_for_peer_not_self() {
        let (a, b) = pair();
        let ping = UdpAudioPacket::new_ping("alice", 12345).to_bytes();
        a.send(&ping).unwrap();
        // b's recv must consume + reflect it, then time out (nothing else).
        let mut buf = [0u8; 1500];
        assert!(b.recv(&mut buf).is_err());
        // a receives its own echo, byte-identical inner.
        let n = a.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &ping[..]);
        assert!(a.last_rx_age() < Duration::from_secs(1));
    }

    #[test]
    fn peer_is_learned_from_authenticated_source() {
        let (ka, kb) = keys(5, 6);
        let a = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), ka, "alice");
        let b = MediaSocket::sealed(UdpSocket::bind("127.0.0.1:0").unwrap(), kb, "bob");
        b.set_read_timeout(Some(Duration::from_millis(200))).unwrap();
        // b has no peer yet: sending fails, but a probe from a teaches it.
        assert!(b.send(b"x").is_err());
        let pkt = UdpAudioPacket::new_audio("alice", 1, b"hi").to_bytes();
        a.send_probe_to(&pkt, b.local_addr().unwrap()).unwrap();
        let mut buf = [0u8; 1500];
        let n = b.recv(&mut buf).unwrap();
        assert_eq!(&buf[..n], &pkt[..]);
        assert_eq!(b.peer(), Some(a.local_addr().unwrap()));
        // an unauthenticated datagram from elsewhere must not move it
        let rogue = UdpSocket::bind("127.0.0.1:0").unwrap();
        rogue.send_to(b"garbage garbage garbage garbage garbage", b.local_addr().unwrap()).unwrap();
        assert!(b.recv(&mut buf).is_err()); // dropped, then timeout
        assert_eq!(b.peer(), Some(a.local_addr().unwrap()));
    }
}
