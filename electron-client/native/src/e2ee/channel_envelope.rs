//! Sealed bodies for encrypted text channels (envelope v2) and the key
//! blobs that carry a channel's epoch keys to members.
//!
//! Message:
//!   key_e   = the channel's epoch key
//!   salt    = 32 random bytes per message
//!   prk     = HKDF-Extract(salt, key_e)
//!   okm     = HKDF-Expand(prk, "decibell-channel-v1" ‖ 0 ‖ channel_id ‖ 0 ‖ sender, 44)
//!   header  = 0x02 ‖ epoch u32 LE ‖ salt
//!   wire    = header ‖ AES-256-GCM(okm.key, okm.nonce, 0x01 ‖ utf8, aad = header ‖ channel_id ‖ sender) ‖ tag
//!
//! The server-stamped sender and the channel are in both the key schedule
//! and the AAD, so a sealed body can't be relabelled or moved between
//! channels; the epoch in the header tells the reader which key to use.
//!
//! Key blob (what members escrow for each other on the server): the DM
//! envelope (`envelope::seal`, pairwise identity keys) around the inner
//! content `0x02 ‖ epoch u32 LE ‖ key[32] ‖ channel_id`, so only the named
//! recipient can open it and it can't be replayed into another channel.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ring::hkdf;

use super::identity::random_bytes;

pub const VERSION: u8 = 2;
pub const SALT_LEN: usize = 32;
pub const HEADER_LEN: usize = 1 + 4 + SALT_LEN;
pub const TAG_LEN: usize = 16;
pub const CONTENT_TEXT: u8 = 1;
/// Inner content tag of a key blob inside a DM envelope.
pub const CONTENT_CHANNEL_KEY: u8 = 2;
const INFO_DOMAIN: &[u8] = b"decibell-channel-v1";
const OKM_LEN: usize = 44;
pub const MAX_TEXT_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    Malformed,
    Bad,
    UnsupportedContent(u8),
}

struct OkmLen;
impl hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        OKM_LEN
    }
}

/// The epoch a sealed body was written under (no key needed).
pub fn parse_epoch(wire: &[u8]) -> Result<u32, OpenError> {
    if wire.len() < HEADER_LEN + TAG_LEN || wire[0] != VERSION {
        return Err(OpenError::Malformed);
    }
    Ok(u32::from_le_bytes([wire[1], wire[2], wire[3], wire[4]]))
}

fn schedule(epoch_key: &[u8; 32], salt: &[u8], channel_id: &str, sender: &str) -> ([u8; 32], [u8; 12]) {
    let mut info = Vec::with_capacity(INFO_DOMAIN.len() + channel_id.len() + sender.len() + 2);
    info.extend_from_slice(INFO_DOMAIN);
    info.push(0);
    info.extend_from_slice(channel_id.as_bytes());
    info.push(0);
    info.extend_from_slice(sender.as_bytes());
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, salt).extract(epoch_key);
    let mut okm = [0u8; OKM_LEN];
    prk.expand(&[&info], OkmLen)
        .and_then(|o| o.fill(&mut okm))
        .expect("HKDF expand of 44 bytes cannot fail");
    let mut key = [0u8; 32];
    let mut nonce = [0u8; 12];
    key.copy_from_slice(&okm[..32]);
    nonce.copy_from_slice(&okm[32..]);
    (key, nonce)
}

fn aad(header: &[u8], channel_id: &str, sender: &str) -> Vec<u8> {
    let mut a = Vec::with_capacity(header.len() + channel_id.len() + sender.len() + 1);
    a.extend_from_slice(header);
    a.extend_from_slice(channel_id.as_bytes());
    a.push(0);
    a.extend_from_slice(sender.as_bytes());
    a
}

pub fn seal(epoch: u32, epoch_key: &[u8; 32], channel_id: &str, sender: &str, text: &str) -> Result<Vec<u8>, String> {
    if text.len() > MAX_TEXT_LEN {
        return Err("message too long to seal".into());
    }
    let salt: [u8; SALT_LEN] = random_bytes()?;
    let mut header = [0u8; HEADER_LEN];
    header[0] = VERSION;
    header[1..5].copy_from_slice(&epoch.to_le_bytes());
    header[5..].copy_from_slice(&salt);
    let (key, nonce) = schedule(epoch_key, &salt, channel_id, sender);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let mut plain = Vec::with_capacity(1 + text.len());
    plain.push(CONTENT_TEXT);
    plain.extend_from_slice(text.as_bytes());
    let body = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &aad(&header, channel_id, sender) })
        .map_err(|_| "AEAD seal failed".to_string())?;
    let mut wire = Vec::with_capacity(HEADER_LEN + body.len());
    wire.extend_from_slice(&header);
    wire.extend_from_slice(&body);
    Ok(wire)
}

/// Open `wire` with the epoch key the header names (the caller resolves it
/// from `parse_epoch`).
pub fn open(epoch_key: &[u8; 32], channel_id: &str, sender: &str, wire: &[u8]) -> Result<String, OpenError> {
    parse_epoch(wire)?;
    let header = &wire[..HEADER_LEN];
    let (key, nonce) = schedule(epoch_key, &header[5..], channel_id, sender);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| OpenError::Bad)?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &wire[HEADER_LEN..], aad: &aad(header, channel_id, sender) })
        .map_err(|_| OpenError::Bad)?;
    match plain.first() {
        Some(&CONTENT_TEXT) => String::from_utf8(plain[1..].to_vec()).map_err(|_| OpenError::Bad),
        Some(&other) => Err(OpenError::UnsupportedContent(other)),
        None => Err(OpenError::Malformed),
    }
}

/// Inner content of a key blob: `0x02 ‖ epoch ‖ key ‖ channel_id`.
pub fn key_blob_content(channel_id: &str, epoch: u32, key: &[u8; 32]) -> Vec<u8> {
    let mut c = Vec::with_capacity(1 + 4 + 32 + channel_id.len());
    c.push(CONTENT_CHANNEL_KEY);
    c.extend_from_slice(&epoch.to_le_bytes());
    c.extend_from_slice(key);
    c.extend_from_slice(channel_id.as_bytes());
    c
}

/// Parse a blob's inner content; `None` unless it names `channel_id`.
pub fn parse_key_blob_content(content: &[u8], channel_id: &str) -> Option<(u32, [u8; 32])> {
    if content.len() != 1 + 4 + 32 + channel_id.len() || content[0] != CONTENT_CHANNEL_KEY {
        return None;
    }
    if &content[37..] != channel_id.as_bytes() {
        return None;
    }
    let epoch = u32::from_le_bytes([content[1], content[2], content[3], content[4]]);
    let mut key = [0u8; 32];
    key.copy_from_slice(&content[5..37]);
    Some((epoch, key))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_bindings() {
        let k = [7u8; 32];
        let wire = seal(3, &k, "general", "alice", "hello #general 🔒").unwrap();
        assert_eq!(parse_epoch(&wire).unwrap(), 3);
        assert_eq!(open(&k, "general", "alice", &wire).unwrap(), "hello #general 🔒");
        assert_eq!(open(&k, "random", "alice", &wire), Err(OpenError::Bad));
        assert_eq!(open(&k, "general", "bob", &wire), Err(OpenError::Bad));
        assert_eq!(open(&[8u8; 32], "general", "alice", &wire), Err(OpenError::Bad));
        let mut t = wire.clone();
        *t.last_mut().unwrap() ^= 1;
        assert_eq!(open(&k, "general", "alice", &t), Err(OpenError::Bad));
        assert_eq!(parse_epoch(&[]), Err(OpenError::Malformed));
        let w2 = seal(3, &k, "general", "alice", "hello #general 🔒").unwrap();
        assert_ne!(wire, w2, "fresh salt per message");
    }

    #[test]
    fn key_blob_content_round_trip() {
        let key = [9u8; 32];
        let c = key_blob_content("dev-chat", 4, &key);
        assert_eq!(parse_key_blob_content(&c, "dev-chat"), Some((4, key)));
        assert_eq!(parse_key_blob_content(&c, "other"), None);
        assert_eq!(parse_key_blob_content(&c[..10], "dev-chat"), None);
    }
}
