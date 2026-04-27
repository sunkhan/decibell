//! Codec capability probing and persistence.
//!
//! Encoders are probed via FFmpeg (try to construct a codec context for each
//! candidate). Decoders are probed in the React layer using WebCodecs and
//! shipped here via the `set_decoder_caps` Tauri command. The merged
//! `ClientCapabilities` is what gets sent over the wire.
//!
//! Persistence: probed encoder caps are cached to `<app_data_dir>/encoder_caps.json`
//! after the first launch. Subsequent launches read from disk; the user can
//! force a re-probe via the "Refresh codec capabilities" button in Settings.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::sync::RwLock;
use tauri::{AppHandle, Manager};

/// Wire-compatible numeric values from proto/messages.proto VideoCodec enum.
/// Repr is u8 to match the byte stamped in UdpVideoPacket.codec.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[repr(u8)]
pub enum CodecKind {
    Unknown = 0,
    H264Hw = 1,
    H264Sw = 2,
    H265 = 3,
    Av1 = 4,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CodecCap {
    pub codec: CodecKind,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

/// Process-wide cache of the probed encoder caps. Populated lazily on the
/// first call to `get_or_probe_encoders` and rebuilt on `refresh_encoders`.
/// Decoder caps live separately in AppState (set from JS via Tauri command).
static ENCODER_CACHE: RwLock<Option<Vec<CodecCap>>> = RwLock::new(None);

fn caps_path(app: &AppHandle) -> Result<PathBuf, String> {
    let dir = app
        .path()
        .app_data_dir()
        .map_err(|e| format!("Failed to get app data dir: {}", e))?;
    let _ = std::fs::create_dir_all(&dir);
    Ok(dir.join("encoder_caps.json"))
}

/// Per-codec encode policy ceilings (spec §3.2). Caps are clamped to these
/// regardless of what the encoder claims to support — keeps reliability
/// defensible across hardware generations.
fn encode_ceiling(codec: CodecKind) -> (u32, u32, u32) {
    match codec {
        CodecKind::Av1 => (3840, 2160, 60),
        CodecKind::H265 => (3840, 2160, 60),
        CodecKind::H264Hw => (2560, 1440, 60),
        CodecKind::H264Sw => (1920, 1080, 60),
        CodecKind::Unknown => (0, 0, 0),
    }
}

/// (codec, list of FFmpeg encoder name candidates in priority order)
fn encoder_candidates() -> Vec<(CodecKind, Vec<&'static str>)> {
    vec![
        (CodecKind::Av1, vec!["av1_nvenc", "av1_amf", "av1_qsv", "av1_vaapi"]),
        (CodecKind::H265, vec!["hevc_nvenc", "hevc_amf", "hevc_qsv", "hevc_mf", "hevc_vaapi"]),
        (CodecKind::H264Hw, vec!["h264_nvenc", "h264_amf", "h264_qsv", "h264_mf", "h264_vaapi"]),
        (CodecKind::H264Sw, vec!["libx264"]),
    ]
}

/// Try to construct a tiny FFmpeg encoder context for the given codec name.
/// Returns true if the encoder opens successfully on this machine. We do not
/// feed any frames — construction + open is enough to verify usability.
fn probe_one_encoder(name: &str) -> bool {
    use ffmpeg_next as ffmpeg;
    let codec = match ffmpeg::encoder::find_by_name(name) {
        Some(c) => c,
        None => return false,
    };
    let ctx = ffmpeg::codec::context::Context::new_with_codec(codec);
    let mut enc = match ctx.encoder().video() {
        Ok(e) => e,
        Err(_) => return false,
    };
    enc.set_width(640);
    enc.set_height(360);
    enc.set_format(ffmpeg::format::Pixel::NV12);
    enc.set_time_base((1, 30));
    enc.set_frame_rate(Some((30, 1)));
    enc.set_bit_rate(1_000_000);
    enc.open_as(codec).is_ok()
}

/// Probe all candidate encoders. Returns a CodecCap per codec for which at
/// least one backend opened successfully, clamped to that codec's policy
/// ceiling.
pub fn probe_encoders() -> Vec<CodecCap> {
    let mut out = Vec::new();
    for (kind, names) in encoder_candidates() {
        let mut found_via: Option<&str> = None;
        for name in &names {
            if probe_one_encoder(name) {
                found_via = Some(*name);
                break;
            }
        }
        if let Some(name) = found_via {
            eprintln!("[caps] encoder available: {:?} via {}", kind, name);
            let (w, h, fps) = encode_ceiling(kind);
            out.push(CodecCap { codec: kind, max_width: w, max_height: h, max_fps: fps });
        } else {
            eprintln!("[caps] encoder NOT available: {:?}", kind);
        }
    }
    out
}

pub fn load_cached_encoders(app: &AppHandle) -> Option<Vec<CodecCap>> {
    let path = caps_path(app).ok()?;
    let data = std::fs::read(&path).ok()?;
    serde_json::from_slice::<Vec<CodecCap>>(&data).ok()
}

pub fn save_encoders(app: &AppHandle, caps: &[CodecCap]) {
    let Ok(path) = caps_path(app) else { return };
    if let Ok(data) = serde_json::to_vec_pretty(caps) {
        let _ = std::fs::write(&path, data);
    }
}

/// Get encoder caps: serve from in-memory cache if populated, otherwise load
/// from disk if present, otherwise probe + persist.
pub fn get_or_probe_encoders(app: &AppHandle) -> Vec<CodecCap> {
    if let Some(c) = ENCODER_CACHE.read().ok().and_then(|g| g.clone()) {
        return c;
    }
    let caps = match load_cached_encoders(app) {
        Some(c) => c,
        None => {
            let probed = probe_encoders();
            save_encoders(app, &probed);
            probed
        }
    };
    if let Ok(mut g) = ENCODER_CACHE.write() {
        *g = Some(caps.clone());
    }
    caps
}

/// Force re-probe — used by the Settings → Codecs "Refresh capabilities" button.
pub fn refresh_encoders(app: &AppHandle) -> Vec<CodecCap> {
    let probed = probe_encoders();
    save_encoders(app, &probed);
    if let Ok(mut g) = ENCODER_CACHE.write() {
        *g = Some(probed.clone());
    }
    probed
}

/// Build the proto ClientCapabilities message from current encoder probe
/// (filtered by user toggles) + decoder caps held in state. Used by
/// commands that send JoinVoiceRequest or UpdateCapabilitiesRequest.
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_encoders_does_not_panic() {
        // Smoke test — we don't assert any particular codec is present
        // because that depends on the FFmpeg build's enabled encoders.
        // Plan B Task 1 is responsible for ensuring the FFmpeg build
        // ships with libx264 + the hardware encoders we need; this test
        // just verifies the probe call returns at all.
        let caps = probe_encoders();
        eprintln!("probed encoders: {:?}", caps);
    }
}
