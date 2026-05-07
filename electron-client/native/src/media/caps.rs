//! Codec capability types (PR8 — encoder probing moved to renderer).
//!
//! After PR8, the renderer probes encoders via Chromium's
//! `WebCodecs.VideoEncoder.isConfigSupported` and ships the result here
//! through the `set_encoder_caps` command. Decoder caps come the same
//! way (via `set_decoder_caps`). The native side just holds the lists
//! and produces `ClientCapabilities` for the JoinVoiceRequest.
//!
//! Removed in PR8 (along with the FFmpeg-in-Rust encode path):
//! `probe_encoders`, `probe_one_encoder`, `encoder_candidates`,
//! `encode_ceiling`, the disk-persisted encoder cache, the FFmpeg
//! probe lifecycle. Renderer-side probing is faster, more accurate (it
//! tests the *actual* path frames will travel through), and avoids the
//! dual-libffmpeg crash that killed the native FFmpeg approach.

use serde::{de::Error as DeError, Deserialize, Deserializer, Serialize, Serializer};

/// Wire-compatible numeric values from proto/messages.proto VideoCodec enum.
/// Repr is u8 to match the byte stamped in UdpVideoPacket.codec, AND the
/// serde impls below force serialization as u8 so values cross the
/// Rust↔JS boundary as numbers (matching the TS VideoCodec enum) rather
/// than variant-name strings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum CodecKind {
    Unknown = 0,
    H264Hw = 1,
    H264Sw = 2,
    H265 = 3,
    Av1 = 4,
}

impl Serialize for CodecKind {
    fn serialize<S: Serializer>(&self, s: S) -> Result<S::Ok, S::Error> {
        s.serialize_u8(*self as u8)
    }
}

impl<'de> Deserialize<'de> for CodecKind {
    fn deserialize<D: Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        let v = u8::deserialize(d)?;
        match v {
            0 => Ok(CodecKind::Unknown),
            1 => Ok(CodecKind::H264Hw),
            2 => Ok(CodecKind::H264Sw),
            3 => Ok(CodecKind::H265),
            4 => Ok(CodecKind::Av1),
            other => Err(D::Error::custom(format!("invalid CodecKind value: {}", other))),
        }
    }
}

/// camelCase serde so JS-side `CodecCapability` (maxWidth / maxHeight /
/// maxFps) round-trips cleanly across the napi boundary.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecCap {
    pub codec: CodecKind,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

/// Plan C: per-user encode + decode capability snapshot held in
/// AppState.voice_caps_cache. Populated from VoicePresenceUpdate
/// payloads; the streamer reads watcher decode caps from here when a
/// STREAM_WATCHER_NOTIFY event arrives so the LCD picker can choose
/// the best codec all watchers can decode.
#[derive(Clone, Debug, Default)]
pub struct PeerCaps {
    pub encode: Vec<CodecCap>,
    pub decode: Vec<CodecCap>,
}

/// Build the proto ClientCapabilities message from current encoder + decoder
/// caps held in state. Used by `join_voice_channel` to advertise capability
/// in JoinVoiceRequest.
pub fn build_client_capabilities(
    encoder_caps: &[CodecCap],
    decoder_caps: &[CodecCap],
) -> crate::net::proto::ClientCapabilities {
    crate::net::proto::ClientCapabilities {
        encode: encoder_caps.iter().map(cap_to_proto).collect(),
        decode: decoder_caps.iter().map(cap_to_proto).collect(),
    }
}

fn cap_to_proto(c: &CodecCap) -> crate::net::proto::CodecCapability {
    crate::net::proto::CodecCapability {
        codec: c.codec as i32,
        max_width: c.max_width,
        max_height: c.max_height,
        max_fps: c.max_fps,
    }
}
