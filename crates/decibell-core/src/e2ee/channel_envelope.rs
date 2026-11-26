//! Sealed bodies for encrypted text channels and the key blobs that carry
//! a channel's epoch keys to members.
//!
//! Message (header byte 0x04 binds the sender's uid; 0x02, from before the
//! uid binding, bound the name it was sent under and stays readable):
//!   key_e   = the channel's epoch key
//!   salt    = 32 random bytes per message
//!   prk     = HKDF-Extract(salt, key_e)
//!   0x04    okm = HKDF-Expand(prk, "decibell-channel-v2" ‖ 0 ‖ channel_id ‖ 0 ‖ sender_uid i64 LE, 44)
//!           aad = header ‖ channel_id ‖ 0 ‖ sender_uid i64 LE
//!   0x02    okm = HKDF-Expand(prk, "decibell-channel-v1" ‖ 0 ‖ channel_id ‖ 0 ‖ sender, 44)
//!           aad = header ‖ channel_id ‖ 0 ‖ sender
//!   header  = version ‖ epoch u32 LE ‖ salt
//!   wire    = header ‖ AES-256-GCM(okm.key, okm.nonce, tag ‖ content, aad) ‖ tag
//!
//! The server-stamped sender and the channel are in both the key schedule
//! and the AAD, so a sealed body can't be relabelled or moved between
//! channels; the epoch in the header tells the reader which key to use.
//!
//! Key blob (what members escrow for each other on the server): the DM
//! envelope (`envelope::seal`, pairwise identity keys) around the inner
//! content `0x02 ‖ epoch u32 LE ‖ key[32] ‖ channel_id`, so only the named
//! recipient can open it and it can't be replayed into another channel.
//!
//! Key check (published once, with the epoch): HMAC-SHA256(key_e,
//! "decibell-channel-key-check-v1" ‖ 0 ‖ channel_id ‖ 0 ‖ epoch u32 LE). Every
//! blob's key must match it, so a member can tell a wrong key someone sealed
//! to them from the epoch's key (review S14). It reveals nothing of the key.

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use ring::hkdf;

use super::identity::random_bytes;

/// Header byte of a name-bound body (history only).
pub const VERSION_V1: u8 = 2;
/// Header byte of a uid-bound body (every new one).
pub const VERSION: u8 = 4;
pub const SALT_LEN: usize = 32;
pub const HEADER_LEN: usize = 1 + 4 + SALT_LEN;
pub const TAG_LEN: usize = 16;
pub const CONTENT_TEXT: u8 = 1;
/// Inner content tag of a key blob inside a DM envelope.
pub const CONTENT_CHANNEL_KEY: u8 = 2;
/// Inner content tag of a prost-encoded `EncryptedMessageBody` (text +
/// encrypted attachment metadata).
pub const CONTENT_BODY: u8 = 3;
const INFO_DOMAIN_V1: &[u8] = b"decibell-channel-v1";
const KEY_CHECK_DOMAIN: &[u8] = b"decibell-channel-key-check-v1";
pub const KEY_CHECK_LEN: usize = 32;
const INFO_DOMAIN_V2: &[u8] = b"decibell-channel-v2";
const OKM_LEN: usize = 44;
pub const MAX_TEXT_LEN: usize = 64 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OpenError {
    Malformed,
    Bad,
    UnsupportedContent(u8),
}

/// Who sent a body, as the server stamped it: v2 binds the uid, v1 the
/// name it was sent under (`sealed_sender`, which can predate a rename).
#[derive(Debug, Clone, Copy)]
pub struct Sender<'a> {
    pub uid: i64,
    pub sealed_name: &'a str,
}

struct OkmLen;
impl hkdf::KeyType for OkmLen {
    fn len(&self) -> usize {
        OKM_LEN
    }
}

/// The epoch a sealed body was written under (no key needed).
pub fn parse_epoch(wire: &[u8]) -> Result<u32, OpenError> {
    if wire.len() < HEADER_LEN + TAG_LEN || (wire[0] != VERSION && wire[0] != VERSION_V1) {
        return Err(OpenError::Malformed);
    }
    Ok(u32::from_le_bytes([wire[1], wire[2], wire[3], wire[4]]))
}

/// The sender bytes a version binds: the uid (v2) or the name (v1).
fn sender_bytes(version: u8, sender: &Sender) -> Result<Vec<u8>, OpenError> {
    match version {
        VERSION if sender.uid > 0 => Ok(sender.uid.to_le_bytes().to_vec()),
        VERSION_V1 if !sender.sealed_name.is_empty() => Ok(sender.sealed_name.as_bytes().to_vec()),
        _ => Err(OpenError::Bad),
    }
}

fn schedule(version: u8, epoch_key: &[u8; 32], salt: &[u8], channel_id: &str, sender: &[u8]) -> ([u8; 32], [u8; 12]) {
    let domain = if version == VERSION { INFO_DOMAIN_V2 } else { INFO_DOMAIN_V1 };
    let mut info = Vec::with_capacity(domain.len() + channel_id.len() + sender.len() + 2);
    info.extend_from_slice(domain);
    info.push(0);
    info.extend_from_slice(channel_id.as_bytes());
    info.push(0);
    info.extend_from_slice(sender);
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

fn aad(header: &[u8], channel_id: &str, sender: &[u8]) -> Vec<u8> {
    let mut a = Vec::with_capacity(header.len() + channel_id.len() + sender.len() + 1);
    a.extend_from_slice(header);
    a.extend_from_slice(channel_id.as_bytes());
    a.push(0);
    a.extend_from_slice(sender);
    a
}

pub fn seal(epoch: u32, epoch_key: &[u8; 32], channel_id: &str, sender_uid: i64, text: &str) -> Result<Vec<u8>, String> {
    seal_tagged(epoch, epoch_key, channel_id, sender_uid, CONTENT_TEXT, text.as_bytes())
}

/// Seal arbitrary inner content under a content tag, bound to the
/// sender's uid.
pub fn seal_tagged(
    epoch: u32,
    epoch_key: &[u8; 32],
    channel_id: &str,
    sender_uid: i64,
    tag: u8,
    content: &[u8],
) -> Result<Vec<u8>, String> {
    if sender_uid <= 0 {
        return Err("can't seal without your account uid".into());
    }
    seal_with(VERSION, epoch, epoch_key, channel_id, &sender_uid.to_le_bytes(), tag, content)
}

fn seal_with(
    version: u8,
    epoch: u32,
    epoch_key: &[u8; 32],
    channel_id: &str,
    sender: &[u8],
    tag: u8,
    content: &[u8],
) -> Result<Vec<u8>, String> {
    if content.len() > MAX_TEXT_LEN {
        return Err("message too long to seal".into());
    }
    let salt: [u8; SALT_LEN] = random_bytes()?;
    let mut header = [0u8; HEADER_LEN];
    header[0] = version;
    header[1..5].copy_from_slice(&epoch.to_le_bytes());
    header[5..].copy_from_slice(&salt);
    let (key, nonce) = schedule(version, epoch_key, &salt, channel_id, sender);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|e| e.to_string())?;
    let mut plain = Vec::with_capacity(1 + content.len());
    plain.push(tag);
    plain.extend_from_slice(content);
    let body = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: &plain, aad: &aad(&header, channel_id, sender) })
        .map_err(|_| "AEAD seal failed".to_string())?;
    let mut wire = Vec::with_capacity(HEADER_LEN + body.len());
    wire.extend_from_slice(&header);
    wire.extend_from_slice(&body);
    Ok(wire)
}

/// Open `wire` with the epoch key the header names (the caller resolves it
/// from `parse_epoch`). Text only; see `open_tagged` for bodies.
pub fn open(epoch_key: &[u8; 32], channel_id: &str, sender: &Sender, wire: &[u8]) -> Result<String, OpenError> {
    let (tag, plain) = open_tagged(epoch_key, channel_id, sender, wire)?;
    match tag {
        CONTENT_TEXT => String::from_utf8(plain).map_err(|_| OpenError::Bad),
        other => Err(OpenError::UnsupportedContent(other)),
    }
}

/// Open `wire` and return `(content tag, inner bytes)`.
pub fn open_tagged(epoch_key: &[u8; 32], channel_id: &str, sender: &Sender, wire: &[u8]) -> Result<(u8, Vec<u8>), OpenError> {
    parse_epoch(wire)?;
    let header = &wire[..HEADER_LEN];
    let sender = sender_bytes(wire[0], sender)?;
    let (key, nonce) = schedule(wire[0], epoch_key, &header[5..], channel_id, &sender);
    let cipher = Aes256Gcm::new_from_slice(&key).map_err(|_| OpenError::Bad)?;
    let plain = cipher
        .decrypt(Nonce::from_slice(&nonce), Payload { msg: &wire[HEADER_LEN..], aad: &aad(header, channel_id, &sender) })
        .map_err(|_| OpenError::Bad)?;
    match plain.first() {
        Some(&tag) => Ok((tag, plain[1..].to_vec())),
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

/// The epoch's key check (see the module docs).
pub fn key_check(channel_id: &str, epoch: u32, key: &[u8; 32]) -> [u8; KEY_CHECK_LEN] {
    let mut msg = Vec::with_capacity(KEY_CHECK_DOMAIN.len() + channel_id.len() + 6);
    msg.extend_from_slice(KEY_CHECK_DOMAIN);
    msg.push(0);
    msg.extend_from_slice(channel_id.as_bytes());
    msg.push(0);
    msg.extend_from_slice(&epoch.to_le_bytes());
    let tag = ring::hmac::sign(&ring::hmac::Key::new(ring::hmac::HMAC_SHA256, key), &msg);
    let mut out = [0u8; KEY_CHECK_LEN];
    out.copy_from_slice(tag.as_ref());
    out
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

/// A v1 (name-bound) body, as clients before the uid binding sealed them —
/// tests only, to prove history stays readable.
#[cfg(test)]
pub fn seal_v1(epoch: u32, epoch_key: &[u8; 32], channel_id: &str, sender: &str, tag: u8, content: &[u8]) -> Result<Vec<u8>, String> {
    seal_with(VERSION_V1, epoch, epoch_key, channel_id, sender.as_bytes(), tag, content)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ALICE: Sender = Sender { uid: 10, sealed_name: "alice" };
    const BOB: Sender = Sender { uid: 20, sealed_name: "bob" };

    #[test]
    fn round_trip_and_bindings() {
        let k = [7u8; 32];
        let wire = seal(3, &k, "general", 10, "hello #general 🔒").unwrap();
        assert_eq!(wire[0], VERSION);
        assert_eq!(parse_epoch(&wire).unwrap(), 3);
        assert_eq!(open(&k, "general", &ALICE, &wire).unwrap(), "hello #general 🔒");
        assert_eq!(open(&k, "random", &ALICE, &wire), Err(OpenError::Bad));
        assert_eq!(open(&k, "general", &BOB, &wire), Err(OpenError::Bad));
        assert_eq!(open(&[8u8; 32], "general", &ALICE, &wire), Err(OpenError::Bad));
        let mut t = wire.clone();
        *t.last_mut().unwrap() ^= 1;
        assert_eq!(open(&k, "general", &ALICE, &t), Err(OpenError::Bad));
        assert_eq!(parse_epoch(&[]), Err(OpenError::Malformed));
        let w2 = seal(3, &k, "general", 10, "hello #general 🔒").unwrap();
        assert_ne!(wire, w2, "fresh salt per message");
        assert!(seal(3, &k, "general", 0, "x").is_err());
    }

    #[test]
    fn v2_survives_a_rename_and_v1_needs_the_sealed_name() {
        let k = [5u8; 32];
        let wire = seal(1, &k, "general", 10, "new").unwrap();
        let renamed = Sender { uid: 10, sealed_name: "alice2" };
        assert_eq!(open(&k, "general", &renamed, &wire).unwrap(), "new");
        let old = seal_v1(1, &k, "general", "alice", CONTENT_TEXT, b"old").unwrap();
        assert_eq!(old[0], VERSION_V1);
        assert_eq!(parse_epoch(&old).unwrap(), 1);
        assert_eq!(open(&k, "general", &ALICE, &old).unwrap(), "old");
        assert_eq!(open(&k, "general", &renamed, &old), Err(OpenError::Bad));
        // The version byte can't be swapped to change the binding.
        let mut relabelled = old.clone();
        relabelled[0] = VERSION;
        assert_eq!(open(&k, "general", &ALICE, &relabelled), Err(OpenError::Bad));
    }

    #[test]
    fn tagged_bodies() {
        let k = [1u8; 32];
        let body = b"\x0a\x02hi".to_vec();
        let w = seal_tagged(1, &k, "c", 10, CONTENT_BODY, &body).unwrap();
        assert_eq!(open_tagged(&k, "c", &ALICE, &w).unwrap(), (CONTENT_BODY, body));
        assert_eq!(open(&k, "c", &ALICE, &w), Err(OpenError::UnsupportedContent(CONTENT_BODY)));
        assert_eq!(open_tagged(&k, "c", &BOB, &w), Err(OpenError::Bad));
    }

    #[test]
    fn key_blob_content_round_trip() {
        let key = [9u8; 32];
        let c = key_blob_content("dev-chat", 4, &key);
        assert_eq!(parse_key_blob_content(&c, "dev-chat"), Some((4, key)));
        assert_eq!(parse_key_blob_content(&c, "other"), None);
        assert_eq!(parse_key_blob_content(&c[..10], "dev-chat"), None);
    }

    #[test]
    fn key_check_binds_key_channel_and_epoch() {
        let key = [9u8; 32];
        let kc = key_check("dev-chat", 4, &key);
        assert_eq!(kc, key_check("dev-chat", 4, &key));
        assert_ne!(kc, key_check("dev-chat", 4, &[8u8; 32]));
        assert_ne!(kc, key_check("dev-chat", 5, &key));
        assert_ne!(kc, key_check("dev-chat2", 4, &key));
        // The separators keep channel and epoch apart.
        assert_ne!(key_check("a", 0x0000_0100, &key), key_check("a\0", 1, &key));
    }
}
