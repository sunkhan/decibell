//! Per-sender media sealing for MLS-encrypted voice channels.
//!
//! Every epoch of the channel's MLS group exports one secret; from it each
//! member derives every sender's keys locally:
//!
//!   K(member, kind) = HKDF-Expand(exporter, "decibell-media-key" ‖ 0 ‖ username ‖ 0 ‖ kind, 32)
//!   kinds: "voice" | "stream-audio" | "video"
//!
//! Sealed packet types keep the plain twin's header (the relay reads it)
//! and seal only the payload:
//!
//!   AUDIO_SEALED / STREAM_AUDIO_SEALED payload:
//!       [epoch u32 LE][counter u64 LE][AES-256-GCM ct][tag]      (28 B overhead)
//!       nonce = counter (LE, zero-prefixed to 12 B); AAD = type ‖ username(32) ‖ sequence
//!   VIDEO_SEALED: one sealed *frame* [epoch u32][stream_salt u32][ct][tag] (24 B per frame),
//!       chunked into packets like a plain frame; nonce = stream_salt ‖ frame_id ‖ 0;
//!       AAD = type ‖ username(32) ‖ frame_id ‖ is_keyframe ‖ codec
//!
//! `stream_salt` is random per stream start, so a frame counter that restarts
//! at 0 never reuses a nonce inside an epoch. The previous epoch's keys are
//! kept for a grace period so in-flight packets still open across a rotation.
//!
//! The `KeyRing` is shared with the audio pipeline, the stream-audio
//! pipeline, the video sender and the video receive thread through an
//! `ArcSwap`, so the hot paths never lock.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use arc_swap::ArcSwap;
use ring::hkdf;

use super::packet::SENDER_ID_SIZE;

pub const PACKET_TYPE_AUDIO_SEALED: u8 = 7;
pub const PACKET_TYPE_STREAM_AUDIO_SEALED: u8 = 8;
pub const PACKET_TYPE_VIDEO_SEALED: u8 = 9;

pub const AUDIO_SEAL_OVERHEAD: usize = 4 + 8 + 16;
pub const VIDEO_SEAL_OVERHEAD: usize = 4 + 4 + 16;
/// How long the previous epoch's keys stay usable after a rotation.
pub const PREVIOUS_EPOCH_GRACE: Duration = Duration::from_secs(2);
const KEY_DOMAIN: &[u8] = b"decibell-media-key";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Kind {
    Voice,
    StreamAudio,
    Video,
}

impl Kind {
    fn label(self) -> &'static [u8] {
        match self {
            Kind::Voice => b"voice",
            Kind::StreamAudio => b"stream-audio",
            Kind::Video => b"video",
        }
    }
    pub fn packet_type(self) -> u8 {
        match self {
            Kind::Voice => PACKET_TYPE_AUDIO_SEALED,
            Kind::StreamAudio => PACKET_TYPE_STREAM_AUDIO_SEALED,
            Kind::Video => PACKET_TYPE_VIDEO_SEALED,
        }
    }
}

struct Len32;
impl hkdf::KeyType for Len32 {
    fn len(&self) -> usize {
        32
    }
}

/// One member's keys for one epoch.
#[derive(Clone)]
pub struct MemberKeys {
    voice: [u8; 32],
    stream_audio: [u8; 32],
    video: [u8; 32],
}

impl MemberKeys {
    fn derive(exporter: &[u8], username: &str) -> MemberKeys {
        let prk = hkdf::Prk::new_less_safe(hkdf::HKDF_SHA256, exporter);
        let one = |kind: Kind| -> [u8; 32] {
            let mut info = Vec::with_capacity(KEY_DOMAIN.len() + username.len() + 16);
            info.extend_from_slice(KEY_DOMAIN);
            info.push(0);
            info.extend_from_slice(username.as_bytes());
            info.push(0);
            info.extend_from_slice(kind.label());
            let mut out = [0u8; 32];
            prk.expand(&[&info], Len32)
                .and_then(|o| o.fill(&mut out))
                .expect("HKDF expand of a 32-byte key cannot fail");
            out
        };
        MemberKeys { voice: one(Kind::Voice), stream_audio: one(Kind::StreamAudio), video: one(Kind::Video) }
    }
    fn key(&self, kind: Kind) -> &[u8; 32] {
        match kind {
            Kind::Voice => &self.voice,
            Kind::StreamAudio => &self.stream_audio,
            Kind::Video => &self.video,
        }
    }
}

/// Every member's keys for one epoch.
#[derive(Clone)]
pub struct EpochKeys {
    pub epoch: u32,
    members: HashMap<String, MemberKeys>,
}

impl EpochKeys {
    /// Derive from the epoch's exporter secret for the given members.
    pub fn derive(epoch: u64, exporter: &[u8], members: &[String]) -> EpochKeys {
        EpochKeys {
            epoch: epoch as u32,
            members: members.iter().map(|m| (m.clone(), MemberKeys::derive(exporter, m))).collect(),
        }
    }
}

/// What the pipelines read: the current epoch, the previous one during its
/// grace period, and our own name (the relay rewrites sender ids to it, so
/// AADs use it on both ends).
pub struct KeyRing {
    pub username: String,
    current: Option<EpochKeys>,
    previous: Option<(EpochKeys, Instant)>,
    /// True while an unverified leaf sits in the group: nothing is sealed.
    pub quarantined: bool,
}

impl KeyRing {
    pub fn empty(username: &str) -> KeyRing {
        KeyRing { username: username.to_string(), current: None, previous: None, quarantined: false }
    }

    /// Install a new epoch; the old current becomes the grace-period previous.
    pub fn rotated(&self, next: EpochKeys) -> KeyRing {
        KeyRing {
            username: self.username.clone(),
            previous: self.current.clone().map(|c| (c, Instant::now())),
            current: Some(next),
            quarantined: false,
        }
    }

    pub fn quarantine(&self) -> KeyRing {
        KeyRing {
            username: self.username.clone(),
            current: self.current.clone(),
            previous: self.previous.clone(),
            quarantined: true,
        }
    }

    pub fn epoch(&self) -> Option<u32> {
        self.current.as_ref().map(|c| c.epoch)
    }

    /// Can we seal right now?
    pub fn can_send(&self) -> bool {
        self.current.is_some() && !self.quarantined
    }

    fn own_key(&self, kind: Kind) -> Option<(u32, &[u8; 32])> {
        if self.quarantined {
            return None;
        }
        let cur = self.current.as_ref()?;
        Some((cur.epoch, cur.members.get(&self.username)?.key(kind)))
    }

    fn peer_key(&self, username: &str, epoch: u32, kind: Kind) -> Option<&[u8; 32]> {
        if let Some(cur) = &self.current {
            if cur.epoch == epoch {
                return cur.members.get(username).map(|m| m.key(kind));
            }
        }
        if let Some((prev, since)) = &self.previous {
            if prev.epoch == epoch && since.elapsed() <= PREVIOUS_EPOCH_GRACE {
                return prev.members.get(username).map(|m| m.key(kind));
            }
        }
        None
    }
}

/// Shared handle: pipelines `load()` it per packet (an atomic pointer read).
pub type SharedKeyRing = Arc<ArcSwap<KeyRing>>;

pub fn new_shared(username: &str) -> SharedKeyRing {
    Arc::new(ArcSwap::from_pointee(KeyRing::empty(username)))
}

fn name_bytes(username: &str) -> [u8; SENDER_ID_SIZE] {
    let mut out = [0u8; SENDER_ID_SIZE];
    let b = username.as_bytes();
    let n = b.len().min(SENDER_ID_SIZE);
    out[..n].copy_from_slice(&b[..n]);
    out
}

fn audio_aad(kind: Kind, username: &str, sequence: u16) -> [u8; 1 + SENDER_ID_SIZE + 2] {
    let mut aad = [0u8; 1 + SENDER_ID_SIZE + 2];
    aad[0] = kind.packet_type();
    aad[1..1 + SENDER_ID_SIZE].copy_from_slice(&name_bytes(username));
    aad[1 + SENDER_ID_SIZE..].copy_from_slice(&sequence.to_le_bytes());
    aad
}

fn video_aad(username: &str, frame_id: u32, is_keyframe: bool, codec: u8) -> [u8; 1 + SENDER_ID_SIZE + 6] {
    let mut aad = [0u8; 1 + SENDER_ID_SIZE + 6];
    aad[0] = PACKET_TYPE_VIDEO_SEALED;
    aad[1..1 + SENDER_ID_SIZE].copy_from_slice(&name_bytes(username));
    aad[1 + SENDER_ID_SIZE..1 + SENDER_ID_SIZE + 4].copy_from_slice(&frame_id.to_le_bytes());
    aad[1 + SENDER_ID_SIZE + 4] = is_keyframe as u8;
    aad[1 + SENDER_ID_SIZE + 5] = codec;
    aad
}

/// Per-sender-per-kind counter for audio nonces. One per pipeline instance;
/// reset is unnecessary because the key changes with the epoch and the
/// counter never repeats within a process.
pub struct AudioSealer {
    kind: Kind,
    counter: AtomicU64,
}

impl AudioSealer {
    pub fn new(kind: Kind) -> AudioSealer {
        AudioSealer { kind, counter: AtomicU64::new(1) }
    }

    /// Seal one Opus payload. `None` when the ring can't send (no epoch yet,
    /// or quarantined) — the caller skips the packet.
    pub fn seal(&self, ring: &KeyRing, sequence: u16, plain: &[u8]) -> Option<Vec<u8>> {
        let (epoch, key) = ring.own_key(self.kind)?;
        let counter = self.counter.fetch_add(1, Ordering::Relaxed);
        let mut nonce = [0u8; 12];
        nonce[4..].copy_from_slice(&counter.to_le_bytes());
        let cipher = Aes256Gcm::new_from_slice(key).ok()?;
        let aad = audio_aad(self.kind, &ring.username, sequence);
        let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: plain, aad: &aad }).ok()?;
        let mut out = Vec::with_capacity(12 + ct.len());
        out.extend_from_slice(&epoch.to_le_bytes());
        out.extend_from_slice(&counter.to_le_bytes());
        out.extend_from_slice(&ct);
        Some(out)
    }
}

/// Open a sealed audio payload from `sender`. `None` = wrong/unknown epoch or
/// member, or tampered.
pub fn open_audio(ring: &KeyRing, kind: Kind, sender: &str, sequence: u16, sealed: &[u8]) -> Option<Vec<u8>> {
    if sealed.len() < AUDIO_SEAL_OVERHEAD {
        return None;
    }
    let epoch = u32::from_le_bytes(sealed[0..4].try_into().ok()?);
    let counter = u64::from_le_bytes(sealed[4..12].try_into().ok()?);
    let key = ring.peer_key(sender, epoch, kind)?;
    let mut nonce = [0u8; 12];
    nonce[4..].copy_from_slice(&counter.to_le_bytes());
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let aad = audio_aad(kind, sender, sequence);
    cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed[12..], aad: &aad }).ok()
}

/// Seal one encoded video frame (before chunking). `stream_salt` is fixed
/// for the life of one stream. `None` when the ring can't send.
pub fn seal_video_frame(
    ring: &KeyRing,
    stream_salt: u32,
    frame_id: u32,
    is_keyframe: bool,
    codec: u8,
    frame: &[u8],
) -> Option<Vec<u8>> {
    let (epoch, key) = ring.own_key(Kind::Video)?;
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&stream_salt.to_le_bytes());
    nonce[4..8].copy_from_slice(&frame_id.to_le_bytes());
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let aad = video_aad(&ring.username, frame_id, is_keyframe, codec);
    let ct = cipher.encrypt(Nonce::from_slice(&nonce), Payload { msg: frame, aad: &aad }).ok()?;
    let mut out = Vec::with_capacity(8 + ct.len());
    out.extend_from_slice(&epoch.to_le_bytes());
    out.extend_from_slice(&stream_salt.to_le_bytes());
    out.extend_from_slice(&ct);
    Some(out)
}

/// Open a reassembled sealed frame from `sender`.
pub fn open_video_frame(
    ring: &KeyRing,
    sender: &str,
    frame_id: u32,
    is_keyframe: bool,
    codec: u8,
    sealed: &[u8],
) -> Option<Vec<u8>> {
    if sealed.len() < VIDEO_SEAL_OVERHEAD {
        return None;
    }
    let epoch = u32::from_le_bytes(sealed[0..4].try_into().ok()?);
    let salt = u32::from_le_bytes(sealed[4..8].try_into().ok()?);
    let key = ring.peer_key(sender, epoch, Kind::Video)?;
    let mut nonce = [0u8; 12];
    nonce[..4].copy_from_slice(&salt.to_le_bytes());
    nonce[4..8].copy_from_slice(&frame_id.to_le_bytes());
    let cipher = Aes256Gcm::new_from_slice(key).ok()?;
    let aad = video_aad(sender, frame_id, is_keyframe, codec);
    cipher.decrypt(Nonce::from_slice(&nonce), Payload { msg: &sealed[8..], aad: &aad }).ok()
}

pub fn random_salt() -> u32 {
    use ring::rand::SecureRandom;
    let mut b = [0u8; 4];
    let _ = ring::rand::SystemRandom::new().fill(&mut b);
    u32::from_le_bytes(b)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ring_for(username: &str, epoch: u64, exporter: &[u8]) -> KeyRing {
        let members = vec!["alice".to_string(), "bob".to_string()];
        KeyRing::empty(username).rotated(EpochKeys::derive(epoch, exporter, &members))
    }

    #[test]
    fn audio_round_trip_and_tamper() {
        let ex = [9u8; 32];
        let alice = ring_for("alice", 3, &ex);
        let bob = ring_for("bob", 3, &ex);
        let sealer = AudioSealer::new(Kind::Voice);
        let sealed = sealer.seal(&alice, 17, b"opus-frame").unwrap();
        assert_eq!(sealed.len(), b"opus-frame".len() + AUDIO_SEAL_OVERHEAD);
        assert_eq!(open_audio(&bob, Kind::Voice, "alice", 17, &sealed).unwrap(), b"opus-frame");
        // wrong kind, sequence, sender, or a flipped byte all fail
        assert!(open_audio(&bob, Kind::StreamAudio, "alice", 17, &sealed).is_none());
        assert!(open_audio(&bob, Kind::Voice, "alice", 18, &sealed).is_none());
        assert!(open_audio(&bob, Kind::Voice, "bob", 17, &sealed).is_none());
        let mut t = sealed.clone();
        *t.last_mut().unwrap() ^= 1;
        assert!(open_audio(&bob, Kind::Voice, "alice", 17, &t).is_none());
        // two seals never share a counter
        let s2 = sealer.seal(&alice, 18, b"x").unwrap();
        assert_ne!(&sealed[4..12], &s2[4..12]);
    }

    #[test]
    fn epoch_mismatch_and_grace() {
        let alice3 = ring_for("alice", 3, &[1u8; 32]);
        let bob4 = ring_for("bob", 4, &[2u8; 32]);
        let sealed = AudioSealer::new(Kind::Voice).seal(&alice3, 1, b"a").unwrap();
        assert!(open_audio(&bob4, Kind::Voice, "alice", 1, &sealed).is_none());
        // bob rotates 3 → 4: epoch 3 still opens during the grace period
        let bob3 = ring_for("bob", 3, &[1u8; 32]);
        let bob34 = bob3.rotated(EpochKeys::derive(4, &[2u8; 32], &["alice".into(), "bob".into()]));
        assert_eq!(bob34.epoch(), Some(4));
        assert!(open_audio(&bob34, Kind::Voice, "alice", 1, &sealed).is_some());
    }

    #[test]
    fn video_frame_round_trip() {
        let ex = [5u8; 32];
        let alice = ring_for("alice", 1, &ex);
        let bob = ring_for("bob", 1, &ex);
        let frame = vec![7u8; 5000];
        let salt = random_salt();
        let sealed = seal_video_frame(&alice, salt, 42, true, 3, &frame).unwrap();
        assert_eq!(sealed.len(), frame.len() + VIDEO_SEAL_OVERHEAD);
        assert_eq!(open_video_frame(&bob, "alice", 42, true, 3, &sealed).unwrap(), frame);
        assert!(open_video_frame(&bob, "alice", 43, true, 3, &sealed).is_none());
        assert!(open_video_frame(&bob, "alice", 42, false, 3, &sealed).is_none());
        assert!(open_video_frame(&bob, "alice", 42, true, 4, &sealed).is_none());
    }

    #[test]
    fn quarantine_and_missing_epoch_block_sending() {
        let ex = [3u8; 32];
        assert!(AudioSealer::new(Kind::Voice).seal(&KeyRing::empty("alice"), 1, b"a").is_none());
        let q = ring_for("alice", 1, &ex).quarantine();
        assert!(!q.can_send());
        assert!(AudioSealer::new(Kind::Voice).seal(&q, 1, b"a").is_none());
        assert!(seal_video_frame(&q, 1, 1, false, 1, b"f").is_none());
        // receiving still works while quarantined
        let alice = ring_for("alice", 1, &ex);
        let sealed = AudioSealer::new(Kind::Voice).seal(&alice, 1, b"a").unwrap();
        let bobq = ring_for("bob", 1, &ex).quarantine();
        assert!(open_audio(&bobq, Kind::Voice, "alice", 1, &sealed).is_some());
    }

    #[test]
    fn keys_differ_per_member_kind_and_epoch() {
        let a = MemberKeys::derive(&[1u8; 32], "alice");
        let b = MemberKeys::derive(&[1u8; 32], "bob");
        let a2 = MemberKeys::derive(&[2u8; 32], "alice");
        assert_ne!(a.voice, b.voice);
        assert_ne!(a.voice, a.video);
        assert_ne!(a.voice, a.stream_audio);
        assert_ne!(a.voice, a2.voice);
    }
}
