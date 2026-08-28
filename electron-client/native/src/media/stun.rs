//! Minimal STUN Binding client (RFC 5389) — just enough to learn the
//! public (server-reflexive) address of one of our UDP sockets.
//!
//! The request MUST be sent from the socket that will carry media: a NAT
//! mapping belongs to the internal socket, and on cone NATs that mapping
//! is reused for every destination, so the address the STUN server sees
//! is the one the peer must send to.

use std::net::{SocketAddr, SocketAddrV4, ToSocketAddrs, UdpSocket};
use std::time::{Duration, Instant};

/// Used when central doesn't hand out a list (older central, or the
/// operator left DECIBELL_STUN_SERVERS unset).
pub const DEFAULT_STUN_SERVERS: &[&str] = &["stun.l.google.com:19302", "stun.cloudflare.com:3478"];

const MAGIC_COOKIE: u32 = 0x2112_A442;
const BINDING_REQUEST: u16 = 0x0001;
const BINDING_RESPONSE: u16 = 0x0101;
const ATTR_MAPPED_ADDRESS: u16 = 0x0001;
const ATTR_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const FAMILY_IPV4: u8 = 0x01;

/// Retransmit schedule (ms after start) — RFC 5389 §7.2.1 style backoff,
/// truncated to fit the 2.5 s budget the call setup allows.
const RETRANSMIT_AT_MS: &[u64] = &[0, 300, 900, 2100];
pub const QUERY_BUDGET: Duration = Duration::from_millis(2500);

pub fn build_binding_request(txid: &[u8; 12]) -> [u8; 20] {
    let mut m = [0u8; 20];
    m[0..2].copy_from_slice(&BINDING_REQUEST.to_be_bytes());
    m[2..4].copy_from_slice(&0u16.to_be_bytes());
    m[4..8].copy_from_slice(&MAGIC_COOKIE.to_be_bytes());
    m[8..20].copy_from_slice(txid);
    m
}

/// True for anything shaped like a STUN message (top two bits 00 + magic
/// cookie) — sealed media datagrams start with 0xE5 and can never match.
pub fn is_stun_message(buf: &[u8]) -> bool {
    buf.len() >= 20
        && buf[0] & 0xC0 == 0
        && u32::from_be_bytes([buf[4], buf[5], buf[6], buf[7]]) == MAGIC_COOKIE
}

/// Parse a Binding Success Response for `txid`; returns the reflexive
/// IPv4 address from XOR-MAPPED-ADDRESS (preferred) or MAPPED-ADDRESS.
pub fn parse_binding_response(buf: &[u8], txid: &[u8; 12]) -> Option<SocketAddrV4> {
    if !is_stun_message(buf) || &buf[8..20] != txid {
        return None;
    }
    if u16::from_be_bytes([buf[0], buf[1]]) != BINDING_RESPONSE {
        return None;
    }
    let len = u16::from_be_bytes([buf[2], buf[3]]) as usize;
    let body = buf.get(20..20 + len)?;
    let mut mapped: Option<SocketAddrV4> = None;
    let mut off = 0;
    while off + 4 <= body.len() {
        let t = u16::from_be_bytes([body[off], body[off + 1]]);
        let l = u16::from_be_bytes([body[off + 2], body[off + 3]]) as usize;
        let v = body.get(off + 4..off + 4 + l)?;
        match t {
            ATTR_XOR_MAPPED_ADDRESS if l >= 8 && v[1] == FAMILY_IPV4 => {
                let port = u16::from_be_bytes([v[2], v[3]]) ^ (MAGIC_COOKIE >> 16) as u16;
                let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]) ^ MAGIC_COOKIE;
                return Some(SocketAddrV4::new(ip.into(), port));
            }
            ATTR_MAPPED_ADDRESS if l >= 8 && v[1] == FAMILY_IPV4 && mapped.is_none() => {
                let port = u16::from_be_bytes([v[2], v[3]]);
                let ip = u32::from_be_bytes([v[4], v[5], v[6], v[7]]);
                mapped = Some(SocketAddrV4::new(ip.into(), port));
            }
            _ => {}
        }
        // attributes are padded to 4-byte boundaries
        off += 4 + ((l + 3) & !3);
    }
    mapped
}

fn resolve_v4(server: &str) -> Vec<SocketAddr> {
    match server.to_socket_addrs() {
        Ok(it) => it.filter(|a| a.is_ipv4()).collect(),
        Err(e) => {
            log::warn!("[stun] cannot resolve {server}: {e}");
            Vec::new()
        }
    }
}

/// Ask every server (in parallel, with retransmits) for this socket's
/// reflexive address; first valid answer wins. Blocking — call from a
/// blocking-pool thread. The socket must be bound and unconnected.
pub fn query(socket: &UdpSocket, servers: &[String], txid: &[u8; 12]) -> Option<SocketAddrV4> {
    let targets: Vec<SocketAddr> = servers.iter().flat_map(|s| resolve_v4(s)).collect();
    if targets.is_empty() {
        log::warn!("[stun] no resolvable STUN servers");
        return None;
    }
    let req = build_binding_request(txid);
    let start = Instant::now();
    let mut next_tx = 0usize;
    let old_timeout = socket.read_timeout().ok().flatten();
    let _ = socket.set_read_timeout(Some(Duration::from_millis(50)));
    let mut buf = [0u8; 256];
    let result = loop {
        let elapsed = start.elapsed();
        if elapsed >= QUERY_BUDGET {
            break None;
        }
        if next_tx < RETRANSMIT_AT_MS.len()
            && elapsed >= Duration::from_millis(RETRANSMIT_AT_MS[next_tx])
        {
            for t in &targets {
                let _ = socket.send_to(&req, t);
            }
            next_tx += 1;
        }
        match socket.recv_from(&mut buf) {
            Ok((n, _src)) => {
                if let Some(addr) = parse_binding_response(&buf[..n], txid) {
                    break Some(addr);
                }
            }
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut => {}
            Err(e) => {
                log::warn!("[stun] recv error: {e}");
                break None;
            }
        }
    };
    let _ = socket.set_read_timeout(old_timeout);
    result
}

#[cfg(test)]
mod tests {
    use super::*;

    // RFC 5769 §2.2 "Sample IPv4 Response" — only the header +
    // XOR-MAPPED-ADDRESS matter to us; SOFTWARE / MESSAGE-INTEGRITY /
    // FINGERPRINT are carried to prove the attribute walk skips them.
    const TXID: [u8; 12] = [0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86, 0xfa, 0x87, 0xdf, 0xae];
    const RESPONSE: &[u8] = &[
        0x01, 0x01, 0x00, 0x3c, 0x21, 0x12, 0xa4, 0x42, 0xb7, 0xe7, 0xa7, 0x01, 0xbc, 0x34, 0xd6, 0x86,
        0xfa, 0x87, 0xdf, 0xae, 0x80, 0x22, 0x00, 0x0b, 0x74, 0x65, 0x73, 0x74, 0x20, 0x76, 0x65, 0x63,
        0x74, 0x6f, 0x72, 0x20, 0x00, 0x20, 0x00, 0x08, 0x00, 0x01, 0xa1, 0x47, 0xe1, 0x12, 0xa6, 0x43,
        0x00, 0x08, 0x00, 0x14, 0x2b, 0x91, 0xf5, 0x99, 0xfd, 0x9e, 0x90, 0xc3, 0x8c, 0x74, 0x89, 0xf9,
        0x2a, 0xf9, 0xba, 0x53, 0xf0, 0x6b, 0xe7, 0xd7, 0x80, 0x28, 0x00, 0x04, 0xc0, 0x7d, 0x4c, 0x96,
    ];

    #[test]
    fn parses_rfc5769_xor_mapped() {
        let addr = parse_binding_response(RESPONSE, &TXID).unwrap();
        assert_eq!(addr, "192.0.2.1:32853".parse().unwrap());
    }

    #[test]
    fn parses_mapped_fallback() {
        // Hand-built response with only MAPPED-ADDRESS 10.1.2.3:4000.
        let mut m = Vec::new();
        m.extend_from_slice(&BINDING_RESPONSE.to_be_bytes());
        m.extend_from_slice(&12u16.to_be_bytes());
        m.extend_from_slice(&MAGIC_COOKIE.to_be_bytes());
        m.extend_from_slice(&TXID);
        m.extend_from_slice(&ATTR_MAPPED_ADDRESS.to_be_bytes());
        m.extend_from_slice(&8u16.to_be_bytes());
        m.extend_from_slice(&[0, FAMILY_IPV4]);
        m.extend_from_slice(&4000u16.to_be_bytes());
        m.extend_from_slice(&[10, 1, 2, 3]);
        let addr = parse_binding_response(&m, &TXID).unwrap();
        assert_eq!(addr, "10.1.2.3:4000".parse().unwrap());
    }

    #[test]
    fn rejects_bad_cookie_and_txid() {
        let mut bad = RESPONSE.to_vec();
        bad[4] = 0x00;
        assert!(parse_binding_response(&bad, &TXID).is_none());
        let mut other = TXID;
        other[0] ^= 1;
        assert!(parse_binding_response(RESPONSE, &other).is_none());
        assert!(!is_stun_message(&[0xE5; 32]));
        assert!(is_stun_message(RESPONSE));
    }

    #[test]
    fn request_shape() {
        let r = build_binding_request(&TXID);
        assert_eq!(&r[0..4], &[0x00, 0x01, 0x00, 0x00]);
        assert!(is_stun_message(&r));
    }
}
