//! Codec-aware bitrate preset table.
//!
//! Spec §8: bitrate = bpp_s × width × height × fps,
//! where bpp_s is the bits-per-pixel-per-second multiplier per (codec, quality).
//! AV1 ≈ 50% of H.264, H.265 ≈ 65% — so the same Quality preset produces
//! a lower bitrate for AV1 than for H.264 at the same resolution / fps.

use crate::media::caps::CodecKind;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Quality {
    Low,
    Medium,
    High,
    /// Explicit bitrate (kbps); bypasses the per-codec table entirely.
    /// When the user types in a number, that's the number we use.
    Custom(u32),
}

/// Returns kbps for the given quality + codec + resolution + fps.
/// Result is clamped to a sane absolute range so degenerate inputs
/// (tiny resolution + Low) don't underrun the encoder.
pub fn bitrate_kbps(quality: Quality, codec: CodecKind, width: u32, height: u32, fps: u32) -> u32 {
    if let Quality::Custom(kbps) = quality {
        return kbps;
    }
    let bpp_s = bpp_s_for(quality, codec);
    let total_bits_per_sec = bpp_s * (width as f64) * (height as f64) * (fps as f64);
    let kbps = (total_bits_per_sec / 1000.0).round() as u32;
    kbps.clamp(300, 50_000)
}

fn bpp_s_for(quality: Quality, codec: CodecKind) -> f64 {
    let row = match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => (0.020, 0.050, 0.080),
        CodecKind::H265 => (0.013, 0.033, 0.054),
        CodecKind::Av1 => (0.010, 0.025, 0.040),
        // Fall back to H.264 multipliers — caller should never pass UNKNOWN
        // to this function, but be permissive.
        CodecKind::Unknown => (0.020, 0.050, 0.080),
    };
    match quality {
        Quality::Low => row.0,
        Quality::Medium => row.1,
        Quality::High => row.2,
        Quality::Custom(_) => unreachable!("handled above"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn h264_1080p60_medium_is_about_6mbps() {
        let kbps = bitrate_kbps(Quality::Medium, CodecKind::H264Hw, 1920, 1080, 60);
        // 1920*1080*60 = 124.4M; * 0.050 = 6.22 Mbps
        assert!((6000..=6300).contains(&kbps), "got {} kbps", kbps);
    }

    #[test]
    fn av1_4k60_high_is_about_20mbps() {
        let kbps = bitrate_kbps(Quality::High, CodecKind::Av1, 3840, 2160, 60);
        // 3840*2160*60 = 497.7M; * 0.040 = 19.9 Mbps
        assert!((19_500..=20_500).contains(&kbps), "got {} kbps", kbps);
    }

    #[test]
    fn h265_1440p60_high_is_about_12mbps() {
        let kbps = bitrate_kbps(Quality::High, CodecKind::H265, 2560, 1440, 60);
        // 2560*1440*60 = 221M; * 0.054 = 11.94 Mbps
        assert!((11_500..=12_500).contains(&kbps), "got {} kbps", kbps);
    }

    #[test]
    fn custom_bypasses_table() {
        let kbps = bitrate_kbps(Quality::Custom(8500), CodecKind::Av1, 3840, 2160, 60);
        assert_eq!(kbps, 8500);
    }

    #[test]
    fn av1_lower_than_h264_for_same_quality() {
        let h264 = bitrate_kbps(Quality::Medium, CodecKind::H264Hw, 1920, 1080, 60);
        let av1 = bitrate_kbps(Quality::Medium, CodecKind::Av1, 1920, 1080, 60);
        assert!(av1 < h264, "AV1 {} should be less than H.264 {}", av1, h264);
        assert!((h264 as f64 / av1 as f64) > 1.5, "ratio is {}", h264 as f64 / av1 as f64);
    }

    #[test]
    fn h264_sw_uses_same_curve_as_h264_hw() {
        let hw = bitrate_kbps(Quality::Medium, CodecKind::H264Hw, 1920, 1080, 60);
        let sw = bitrate_kbps(Quality::Medium, CodecKind::H264Sw, 1920, 1080, 60);
        assert_eq!(hw, sw, "x264 should use the same bitrate curve as hardware H.264");
    }

    #[test]
    fn floor_clamp_kicks_in_at_tiny_resolutions() {
        let kbps = bitrate_kbps(Quality::Low, CodecKind::Av1, 320, 240, 15);
        assert_eq!(kbps, 300, "should clamp to 300 kbps floor");
    }

    #[test]
    fn unknown_codec_falls_back_to_h264_multipliers() {
        let h264 = bitrate_kbps(Quality::Medium, CodecKind::H264Hw, 1920, 1080, 60);
        let unknown = bitrate_kbps(Quality::Medium, CodecKind::Unknown, 1920, 1080, 60);
        assert_eq!(h264, unknown);
    }
}
