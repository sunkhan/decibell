//! The sealed DM body (envelope v1).
//!
//!   root   = X25519(my_dh_priv, peer_dh_pub)
//!   salt   = 32 random bytes per message
//!   prk    = HKDF-Extract(SHA-256, salt, root)
//!   okm    = HKDF-Expand(prk, "decibell-dm-v1" ‖ 0 ‖ lower ‖ 0 ‖ higher ‖ 0 ‖ lower_kid ‖ higher_kid, 44)
//!   key    = okm[0..32]   nonce = okm[32..44]
//!   header = 0x01 ‖ sender_kid u32 LE ‖ recipient_kid u32 LE ‖ salt
//!   aad    = header ‖ sender_username
//!   wire   = header ‖ AES-256-GCM(key, nonce, 0x01 ‖ utf8(text), aad) ‖ tag
//!
//! Both usernames and both key ids are in the key schedule, so a
//! ciphertext can only open under the pair and key generation it was
//! sealed for; the sender's name in the AAD (checked against the
//! server-stamped sender) defeats reflection. A fresh key per message from
//! a 256-bit salt means nonce reuse can't happen. Static X25519 is
//! symmetric, so the sender opens its own echo/history with the same call.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ring::hkdf;
use x25519_dalek::{PublicKey, StaticSecret};

use super::identity::{random_bytes, KEY_LEN};

pub const VERSION: u8 = 1;
pub const SALT_LEN: usize = 32;
pub const HEADER_LEN: usize = 1 + 4 + 4 + SALT_LEN;
pub const TAG_LEN: usize = 16;
/// Inner content tag: UTF-8 text.
pub const CONTENT_TEXT: u8 = 1;
const INFO_DOMAIN: &[u8] = b"decibell-dm-v1";
const OKM_LEN: usize = 32 + 12;
/// Largest plaintext we'll seal — the server's 64 KiB body cap.
pub const MAX_TEXT_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub sender_key_id: u32,
    pub recipient_key_id: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    /// Not an envelope we understand (bad version / too short).
    Malformed,
    /// The header names key ids other than the ones supplied.
    KeyMismatch,
    /// Authentication failed (wrong keys, tampered, wrong sender).
    Bad,
    /// Decrypted fine but the inner content tag isn't text.
    UnsupportedContent(u8),
}

/// Peek at the key ids without any key material.
pub fn parse_header(wire: &[u8]) -> Result<Header, OpenError> {
    if wire.len() < HEADER_LEN + TAG_LEN || wire[0] != VERSION {
        return Err(OpenError::Malformed);
    }
    let kid = |at: usize| u32::from_le_bytes([wire[at], wire[at + 1], wire[at + 2], wire[at + 3]]);
    Ok(Header {
        sender_key_id: kid(1),
        recipient_key_id: kid(5),
    })
}

struct OkmLen;
impl hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        OKM_LEN
    }
}

/// (key, nonce) for one message.
fn schedule(
    my_priv: &[u8; KEY_LEN],
    peer_pub: &[u8; KEY_LEN],
    salt: &[u8],
    sender: &str,
    sender_kid: u32,
    recipient: &str,
    recipient_kid: u32,
) -> Result<([u8; 32], [u8; 12]), String> {
    let secret = StaticSecret::from(*my_priv);
    let shared = secret.diffie_hellman(&PublicKey::from(*peer_pub));
    // Reject the low-order points: an all-zero shared secret would make
    // every message under this pair openable by anyone.
    if !shared.was_contributory() {
        return Err("peer public key is not contributory".into());
    }
    let ((lo, lo_kid), (hi, hi_kid)) = if sender <= recipient {
        ((sender, sender_kid), (recipient, recipient_kid))
    } else {
        ((recipient, recipient_kid), (sender, sender_kid))
    };
    let mut info = Vec::with_capacity(INFO_DOMAIN.len() + lo.len() + hi.len() + 11);
    info.extend_from_slice(INFO_DOMAIN);
    info.push(0);
    info.extend_from_slice(lo.as_bytes());
    info.push(0);
    info.extend_from_slice(hi.as_bytes());
    info.push(0);
    info.extend_from_slice(&lo_kid.to_le_bytes());
    info.extend_from_slice(&hi_kid.to_le_bytes());

    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(shared.as_bytes());
    let mut okm = [0u8; OKM_LEN];
    prk.expand(&[&info], OkmLen)
        .and_then(|o| o.fill(&mut okm))
        .map_err(|_| "HKDF expand failed".to_string())?;
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    Ok((key, nonce))
}

fn header_bytes(sender_kid: u32, recipient_kid: u32, salt: &[u8; SALT_LEN]) -> [u8; HEADER_LEN] {
    let mut h = [0u8; HEADER_LEN];
    h[0] = VERSION;
    h[1..5].copy_from_slice(&sender_kid.to_le_bytes());
    h[5..9].copy_from_slice(&recipient_kid.to_le_bytes());
    h[9..].copy_from_slice(salt);
    h
}

fn aad(header: &[u8], sender: &str) -> Vec<u8> {
    let mut a = Vec::with_capacity(header.len() + sender.len());
    a.extend_from_slice(header);
    a.extend_from_slice(sender.as_bytes());
    a
}

/// Seal `text` from `sender` (our key `sender_kid`) to `recipient` (their
/// key `recipient_kid`).
pub fn seal(
    my_priv: &[u8; KEY_LEN],
    sender: &str,
    sender_kid: u32,
    peer_pub: &[u8; KEY_LEN],
    recipient: &str,
    recipient_kid: u32,
    text: &str,
) -> Result<Vec<u8>, String> {
    if text.len() > MAX_TEXT_LEN {
        return Err("message too long to seal".into());
    }
    let mut plain = Vec::with_capacity(1 + text.len());
    plain.push(CONTENT_TEXT);
    plain.extend_from_slice(text.as_bytes());
    seal_bytes(my_priv, sender, sender_kid, peer_pub, recipient, recipient_kid, &plain)
}

/// Seal an already-tagged inner content (`CONTENT_*` first byte) — used
/// for channel key blobs, which ride the same pairwise envelope.
pub fn seal_bytes(
    my_priv: &[u8; KEY_LEN],
    sender: &str,
    sender_kid: u32,
    peer_pub: &[u8; KEY_LEN],
    recipient: &str,
    recipient_kid: u32,
    plain: &[u8],
) -> Result<Vec<u8>, String> {
    if plain.len() > MAX_TEXT_LEN + 1 {
        return Err("content too long to seal".into());
    }
    let salt: [u8; SALT_LEN] = random_bytes()?;
    let (key, nonce) = schedule(my_priv, peer_pub, &salt, sender, sender_kid, recipient, recipient_kid)?;
    let header = header_bytes(sender_kid, recipient_kid, &salt);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let body = cipher
        .encrypt(
            Nonce::from_slice(&nonce),
            Payload { msg: plain, aad: &aad(&header, sender) },
        )
        .map_err(|_| "AEAD seal failed".to_string())?;
    let mut wire = Vec::with_capacity(HEADER_LEN + body.len());
    wire.extend_from_slice(&header);
    wire.extend_from_slice(&body);
    Ok(wire)
}

/// Open `wire`. `sender`/`recipient` are the server-stamped names;
/// `my_priv` is whichever of the two we are, `my_kid` its key id,
/// `peer_pub`/`peer_kid` the other side's. `i_am_sender` says which slot
/// the local key fills.
#[allow(clippy::too_many_arguments)]
pub fn open(
    my_priv: &[u8; KEY_LEN],
    my_kid: u32,
    peer_pub: &[u8; KEY_LEN],
    peer_kid: u32,
    sender: &str,
    recipient: &str,
    i_am_sender: bool,
    wire: &[u8],
) -> Result<String, OpenError> {
    let plain = open_bytes(my_priv, my_kid, peer_pub, peer_kid, sender, recipient, i_am_sender, wire)?;
    match plain.first() {
        Some(&CONTENT_TEXT) => {
            String::from_utf8(plain[1..].to_vec()).map_err(|_| OpenError::Bad)
        }
        Some(&other) => Err(OpenError::UnsupportedContent(other)),
        None => Err(OpenError::Malformed),
    }
}

/// `open` without interpreting the inner content tag.
#[allow(clippy::too_many_arguments)]
pub fn open_bytes(
    my_priv: &[u8; KEY_LEN],
    my_kid: u32,
    peer_pub: &[u8; KEY_LEN],
    peer_kid: u32,
    sender: &str,
    recipient: &str,
    i_am_sender: bool,
    wire: &[u8],
) -> Result<Vec<u8>, OpenError> {
    let hdr = parse_header(wire)?;
    let (sender_kid, recipient_kid) = if i_am_sender {
        (my_kid, peer_kid)
    } else {
        (peer_kid, my_kid)
    };
    if hdr.sender_key_id != sender_kid || hdr.recipient_key_id != recipient_kid {
        return Err(OpenError::KeyMismatch);
    }
    let header = &wire[..HEADER_LEN];
    let salt = &header[9..];
    let (key, nonce) =
        schedule(my_priv, peer_pub, salt, sender, sender_kid, recipient, recipient_kid)
            .map_err(|_| OpenError::Bad)?;
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| OpenError::Bad)?;
    let plain = cipher
        .decrypt(
            Nonce::from_slice(&nonce),
            Payload { msg: &wire[HEADER_LEN..], aad: &aad(header, sender) },
        )
        .map_err(|_| OpenError::Bad)?;
    if plain.is_empty() {
        return Err(OpenError::Malformed);
    }
    Ok(plain)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::e2ee::identity::IdentityKeys;

    fn pair() -> (IdentityKeys, IdentityKeys) {
        let mut a = IdentityKeys::generate(1).unwrap();
        let mut b = IdentityKeys::generate(1).unwrap();
        a.key_id = 3;
        b.key_id = 7;
        (a, b)
    }

    fn priv32(k: &IdentityKeys) -> [u8; 32] {
        let mut p = [0u8; 32];
        p.copy_from_slice(&k.dh_priv);
        p
    }

    #[test]
    fn round_trip_both_directions() {
        let (a, b) = pair();
        let (ap, bp) = (a.dh_public().unwrap(), b.dh_public().unwrap());
        let wire = seal(&priv32(&a), "alice", 3, &bp, "bob", 7, "hello bob 🔒").unwrap();
        assert_eq!(wire.len(), HEADER_LEN + 1 + "hello bob 🔒".len() + TAG_LEN);
        assert_eq!(parse_header(&wire).unwrap(), Header { sender_key_id: 3, recipient_key_id: 7 });
        // bob opens as recipient
        let t = open(&priv32(&b), 7, &ap, 3, "alice", "bob", false, &wire).unwrap();
        assert_eq!(t, "hello bob 🔒");
        // alice opens her own echo as sender
        let t = open(&priv32(&a), 3, &bp, 7, "alice", "bob", true, &wire).unwrap();
        assert_eq!(t, "hello bob 🔒");
        // and the reverse direction
        let wire2 = seal(&priv32(&b), "bob", 7, &ap, "alice", 3, "hi").unwrap();
        assert_eq!(open(&priv32(&a), 3, &bp, 7, "bob", "alice", false, &wire2).unwrap(), "hi");
    }

    #[test]
    fn fresh_salt_every_message() {
        let (a, b) = pair();
        let bp = b.dh_public().unwrap();
        let w1 = seal(&priv32(&a), "alice", 3, &bp, "bob", 7, "x").unwrap();
        let w2 = seal(&priv32(&a), "alice", 3, &bp, "bob", 7, "x").unwrap();
        assert_ne!(w1, w2);
        assert_ne!(&w1[9..HEADER_LEN], &w2[9..HEADER_LEN]);
    }

    #[test]
    fn tamper_and_reflection_fail() {
        let (a, b) = pair();
        let (ap, bp) = (a.dh_public().unwrap(), b.dh_public().unwrap());
        let wire = seal(&priv32(&a), "alice", 3, &bp, "bob", 7, "secret").unwrap();
        let mut t = wire.clone();
        *t.last_mut().unwrap() ^= 1;
        assert_eq!(open(&priv32(&b), 7, &ap, 3, "alice", "bob", false, &t), Err(OpenError::Bad));
        let mut t = wire.clone();
        t[HEADER_LEN] ^= 1;
        assert_eq!(open(&priv32(&b), 7, &ap, 3, "alice", "bob", false, &t), Err(OpenError::Bad));
        // Reflection: bob's server bounces alice's ciphertext back to her
        // claiming bob sent it. The AAD names the real sender, and the key
        // ids are the wrong way round for that claim.
        assert_eq!(
            open(&priv32(&a), 3, &bp, 7, "bob", "alice", false, &wire),
            Err(OpenError::KeyMismatch)
        );
        // Same key ids on both sides so the header matches — the AAD
        // still refuses it.
        let mut a2 = IdentityKeys::generate(1).unwrap();
        let mut b2 = IdentityKeys::generate(1).unwrap();
        a2.key_id = 1;
        b2.key_id = 1;
        let (a2p, b2p) = (a2.dh_public().unwrap(), b2.dh_public().unwrap());
        let w = seal(&priv32(&a2), "alice", 1, &b2p, "bob", 1, "secret").unwrap();
        assert_eq!(open(&priv32(&a2), 1, &b2p, 1, "bob", "alice", false, &w), Err(OpenError::Bad));
        let _ = a2p;
    }

    #[test]
    fn wrong_pair_or_generation_fails() {
        let (a, b) = pair();
        let c = IdentityKeys::generate(1).unwrap();
        let (ap, bp, cp) = (a.dh_public().unwrap(), b.dh_public().unwrap(), c.dh_public().unwrap());
        let wire = seal(&priv32(&a), "alice", 3, &bp, "bob", 7, "secret").unwrap();
        // carol can't open it
        assert_eq!(open(&priv32(&c), 7, &ap, 3, "alice", "bob", false, &wire), Err(OpenError::Bad));
        // bob with the wrong generation of alice's key
        assert_eq!(open(&priv32(&b), 7, &cp, 3, "alice", "bob", false, &wire), Err(OpenError::Bad));
        // header says other key ids
        assert_eq!(open(&priv32(&b), 8, &ap, 3, "alice", "bob", false, &wire), Err(OpenError::KeyMismatch));
    }

    #[test]
    fn malformed_inputs() {
        assert_eq!(parse_header(&[]), Err(OpenError::Malformed));
        assert_eq!(parse_header(&[2u8; HEADER_LEN + TAG_LEN]), Err(OpenError::Malformed));
        let (a, b) = pair();
        let bp = b.dh_public().unwrap();
        let long = "x".repeat(MAX_TEXT_LEN + 1);
        assert!(seal(&priv32(&a), "alice", 3, &bp, "bob", 7, &long).is_err());
        // A low-order peer key is refused.
        assert!(seal(&priv32(&a), "alice", 3, &[0u8; 32], "bob", 7, "x").is_err());
    }
}
