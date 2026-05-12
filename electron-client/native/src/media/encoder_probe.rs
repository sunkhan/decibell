//! Native FFmpeg encoder probe (Windows-only).
//!
//! Replaces the renderer-side WebCodecs probe on Windows. For each
//! (codec, vendor) tuple we try `avcodec_find_encoder_by_name` and a
//! 64×64 throwaway `avcodec_open2`. Any combination that opens
//! cleanly is reported as HW-capable.
//!
//! Vendor priority is auto-detected from the GPU vendor id (NVIDIA →
//! NVENC first, AMD → AMF first, Intel → QSV first). The probe still
//! tries the other vendors as a fallback in case the user has a
//! mixed-GPU system.
//!
//! Result shape matches the existing CodecCap used by Linux/macOS so
//! the renderer can use one cached structure regardless of platform.

use ffmpeg_next as ff;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EncoderCap {
    /// VideoCodec wire id (1=H264_HW, 3=H265, 4=AV1).
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub hardware: bool,
    /// Which FFmpeg encoder name actually opens (e.g. "h264_nvenc").
    pub encoder_name: String,
}

// NVENC's H.264 minimum dimensions are ~145×96 and AV1 wants 128×128+.
// Using 1280×720 keeps the probe well above every vendor's floor without
// noticeably slowing init (no frame is actually encoded — we open + close).
const PROBE_W: u32 = 1280;
const PROBE_H: u32 = 720;
const PROBE_FPS: u32 = 30;
const PROBE_BR: i64 = 2_000_000;

/// (codec_id, encoder_name) tuples in vendor priority order.
fn candidates_for(codec_id: i32, vendor_id: u32) -> Vec<&'static str> {
    let nvenc_first = vendor_id == 0x10DE;
    let amf_first = vendor_id == 0x1002;
    let qsv_first = vendor_id == 0x8086;

    let nvenc = match codec_id {
        1 => "h264_nvenc",
        3 => "hevc_nvenc",
        4 => "av1_nvenc",
        _ => return Vec::new(),
    };
    let amf = match codec_id {
        1 => "h264_amf",
        3 => "hevc_amf",
        4 => "av1_amf",
        _ => return Vec::new(),
    };
    let qsv = match codec_id {
        1 => "h264_qsv",
        3 => "hevc_qsv",
        4 => "av1_qsv",
        _ => return Vec::new(),
    };

    match (nvenc_first, amf_first, qsv_first) {
        (true, _, _) => vec![nvenc, amf, qsv],
        (_, true, _) => vec![amf, nvenc, qsv],
        (_, _, true) => vec![qsv, nvenc, amf],
        _ => vec![nvenc, amf, qsv],
    }
}

/// Try to open the named encoder at PROBE_W×PROBE_H. Returns true on
/// successful open (immediately closed before returning).
fn try_open(name: &str) -> bool {
    use ff::codec::Id;
    use ff::format::Pixel;
    let expected_id = match name {
        n if n.starts_with("h264_") => Id::H264,
        n if n.starts_with("hevc_") => Id::HEVC,
        n if n.starts_with("av1_") => Id::AV1,
        _ => return false,
    };
    let codec = match ff::codec::encoder::find_by_name(name) {
        Some(c) => c,
        None => return false,
    };
    if codec.id() != expected_id {
        return false;
    }
    let context = ff::codec::context::Context::new_with_codec(codec);
    let mut enc = match context.encoder().video() {
        Ok(v) => v,
        Err(_) => return false,
    };
    enc.set_width(PROBE_W);
    enc.set_height(PROBE_H);
    // Most HW encoders accept NV12 input even at probe time without a
    // hwframes context — they fall back to sysmem input + internal copy.
    // Good enough to confirm the encoder is wired up.
    enc.set_format(Pixel::NV12);
    enc.set_frame_rate(Some((PROBE_FPS as i32, 1)));
    enc.set_time_base((1, PROBE_FPS as i32));
    enc.set_bit_rate(PROBE_BR as usize);
    enc.open_as(codec).is_ok()
}

pub fn run(vendor_id: u32) -> Vec<EncoderCap> {
    let mut out = Vec::new();
    for codec_id in [1, 3, 4] {
        for name in candidates_for(codec_id, vendor_id) {
            if try_open(name) {
                let (mw, mh, mf) = ceiling_for(codec_id);
                out.push(EncoderCap {
                    codec: codec_id,
                    max_width: mw,
                    max_height: mh,
                    max_fps: mf,
                    hardware: true,
                    encoder_name: name.to_string(),
                });
                break; // first working vendor wins per codec
            }
        }
    }
    out
}

fn ceiling_for(codec_id: i32) -> (u32, u32, u32) {
    match codec_id {
        1 => (2560, 1440, 120), // H.264_HW
        3 => (3840, 2160, 120), // H.265
        4 => (3840, 2160, 120), // AV1
        _ => (1280, 720, 30),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_priority_for_nvidia() {
        let v = candidates_for(1, 0x10DE);
        assert_eq!(v[0], "h264_nvenc");
    }

    #[test]
    fn amf_priority_for_amd() {
        let v = candidates_for(1, 0x1002);
        assert_eq!(v[0], "h264_amf");
    }

    #[test]
    fn qsv_priority_for_intel() {
        let v = candidates_for(1, 0x8086);
        assert_eq!(v[0], "h264_qsv");
    }

    #[test]
    fn unknown_vendor_defaults_to_nvenc_first() {
        let v = candidates_for(1, 0x0000);
        assert_eq!(v[0], "h264_nvenc");
    }

    #[test]
    fn empty_for_unknown_codec() {
        let v = candidates_for(999, 0x10DE);
        assert!(v.is_empty());
    }
}
