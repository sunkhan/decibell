//! Per-encoder option strings for low-latency screen-share encoding.
//! Values from the design spec §6 — kept as plain data so the
//! encoder.rs initialization code stays small.
//!
//! Every name AND value here is checked against the FFmpeg n8.0.1 option
//! tables (libavcodec/nvenc_{h264,hevc,av1}.c, amfenc_{h264,hevc,av1}.c).
//! Two failure modes to keep in mind when editing:
//!  - an unknown option NAME is silently left unconsumed by avcodec_open2
//!    (e.g. NVENC spells it `forced-idr`, AMF/QSV `forced_idr` — the wrong
//!    spelling does nothing at all);
//!  - an invalid VALUE fails avcodec_open2, and the stream falls back to
//!    the renderer's WebCodecs path.
//!
//! Not cfg(windows) only so the tests below run on every platform; the
//! Windows encoder is the only consumer.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

pub struct PresetOptions {
    /// Key/value pairs forwarded to AVDictionary at avcodec_open2 time.
    pub opts: &'static [(&'static str, &'static str)],
}

pub fn preset_for(encoder_name: &str) -> PresetOptions {
    match encoder_name {
        // forced-idr: a forced keyframe (pict_type = I) is otherwise a
        // non-IDR intra frame (NV_ENC_PIC_FLAG_FORCEINTRA) — a watcher
        // joining on it can't start decoding until the next GOP IDR.
        // SPS/PPS repeat on every IDR without GLOBAL_HEADER (repeatSPSPPS).
        "h264_nvenc" | "hevc_nvenc" => PresetOptions {
            opts: &[
                ("preset", "p4"),
                ("tune", "ull"),
                ("rc", "cbr"),
                ("b_ref_mode", "disabled"),
                ("zerolatency", "1"),
                ("forced-idr", "1"),
            ],
        },
        // NVENC repeats the AV1 sequence header on every keyframe
        // unconditionally (repeatSeqHdr = 1).
        "av1_nvenc" => PresetOptions {
            opts: &[
                ("preset", "p4"),
                ("tune", "ull"),
                ("rc", "cbr"),
                ("tile_columns", "2"),
                ("tile_rows", "1"),
                ("forced-idr", "1"),
            ],
        },
        // AMF, common to all three:
        //  - usage=lowlatency, NOT ultralowlatency: AMF Init fails with ULL
        //    on pre-RDNA parts (AMF issue #410).
        //  - forced_idr=1: the default (0) turns a forced keyframe into a
        //    plain I (AV1: intra-only) frame that amfenc doesn't flag as key.
        //  - frame skipping off: rate-control skips can starve amfenc's
        //    hwsurfaces accounting and spin it in its blocking loop.
        //  - parameter sets / sequence header on every keyframe: AMF does
        //    not repeat them by default, and AV_CODEC_FLAG_GLOBAL_HEADER has
        //    no effect on amfenc. H.264's header_spacing depends on the GOP,
        //    so it lives in `dynamic_opts`.
        "h264_amf" => PresetOptions {
            opts: &[
                ("usage", "lowlatency"),
                ("quality", "speed"),
                ("rc", "cbr"),
                ("enforce_hrd", "true"),
                ("forced_idr", "1"),
                // AMF_VIDEO_ENCODER_LOWLATENCY_MODE (bool).
                ("latency", "1"),
                ("frame_skipping", "0"),
            ],
        },
        "hevc_amf" => PresetOptions {
            opts: &[
                ("usage", "lowlatency"),
                ("quality", "speed"),
                ("rc", "cbr"),
                ("enforce_hrd", "true"),
                ("forced_idr", "1"),
                // AMF_VIDEO_ENCODER_HEVC_LOWLATENCY_MODE (bool).
                ("latency", "1"),
                ("skip_frame", "0"),
                // VPS/SPS/PPS in front of every IDR (default: none).
                ("header_insertion_mode", "idr"),
            ],
        },
        "av1_amf" => PresetOptions {
            opts: &[
                ("usage", "lowlatency"),
                ("quality", "speed"),
                ("rc", "cbr"),
                ("enforce_hrd", "true"),
                ("forced_idr", "1"),
                // AMF_VIDEO_ENCODER_AV1_ENCODING_LATENCY_MODE (enum, not bool).
                ("latency", "lowest_latency"),
                ("skip_frame", "0"),
                // Sequence header on every key frame (KEY_FRAME_ALIGNED).
                ("header_insertion_mode", "frame"),
            ],
        },
        "h264_qsv" | "hevc_qsv" | "av1_qsv" => PresetOptions {
            opts: &[
                ("preset", "veryfast"),
                ("look_ahead", "0"),
                ("rdo", "0"),
                ("low_power", "1"),
            ],
        },
        _ => PresetOptions { opts: &[] },
    }
}

/// h264_amf's `header_spacing` option range is -1..=1000; a larger value
/// makes avcodec_open2 fail outright.
const AMF_MAX_HEADER_SPACING: u32 = 1000;

/// Options whose value depends on the session (GOP length), applied after
/// `preset_for`.
pub fn dynamic_opts(encoder_name: &str, gop: u32) -> Vec<(&'static str, String)> {
    match encoder_name {
        // SPS/PPS every `gop` frames — i.e. on the periodic IDRs. Forced
        // IDRs get them from forced_idr (amfenc sets INSERT_SPS/PPS on those
        // frames); anything that still slips through is patched by
        // Encoder::for_each_packet.
        "h264_amf" => vec![(
            "header_spacing",
            gop.clamp(1, AMF_MAX_HEADER_SPACING).to_string(),
        )],
        _ => Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn has(name: &str, key: &str, value: &str) -> bool {
        preset_for(name).opts.iter().any(|(k, v)| *k == key && *v == value)
    }

    #[test]
    fn nvenc_h264_uses_p4_preset() {
        assert!(has("h264_nvenc", "preset", "p4"));
        assert!(has("h264_nvenc", "rc", "cbr"));
    }

    #[test]
    fn nvenc_forces_idr_with_hyphenated_name() {
        for name in ["h264_nvenc", "hevc_nvenc", "av1_nvenc"] {
            assert!(has(name, "forced-idr", "1"), "{name}");
            // The underscore spelling is not an NVENC option (silently ignored).
            assert!(!preset_for(name).opts.iter().any(|(k, _)| *k == "forced_idr"), "{name}");
        }
    }

    #[test]
    fn amf_uses_lowlatency_not_ultralowlatency() {
        for name in ["h264_amf", "hevc_amf", "av1_amf"] {
            assert!(has(name, "usage", "lowlatency"), "{name}");
            assert!(!has(name, "usage", "ultralowlatency"), "{name}");
        }
    }

    #[test]
    fn amf_forces_idr_with_underscore_name() {
        for name in ["h264_amf", "hevc_amf", "av1_amf"] {
            assert!(has(name, "forced_idr", "1"), "{name}");
        }
    }

    #[test]
    fn amf_low_latency_and_no_frame_skipping() {
        assert!(has("h264_amf", "latency", "1"));
        assert!(has("h264_amf", "frame_skipping", "0"));
        assert!(has("hevc_amf", "latency", "1"));
        assert!(has("hevc_amf", "skip_frame", "0"));
        assert!(has("av1_amf", "latency", "lowest_latency"));
        assert!(has("av1_amf", "skip_frame", "0"));
    }

    #[test]
    fn amf_repeats_headers_on_keyframes() {
        assert!(has("hevc_amf", "header_insertion_mode", "idr"));
        assert!(has("av1_amf", "header_insertion_mode", "frame"));
        // H.264 uses header_spacing (dynamic), not header_insertion_mode.
        assert!(!preset_for("h264_amf").opts.iter().any(|(k, _)| *k == "header_insertion_mode"));
    }

    #[test]
    fn h264_amf_header_spacing_follows_gop_within_range() {
        assert_eq!(dynamic_opts("h264_amf", 240), vec![("header_spacing", "240".to_string())]);
        assert_eq!(dynamic_opts("h264_amf", 5000), vec![("header_spacing", "1000".to_string())]);
        assert_eq!(dynamic_opts("h264_amf", 0), vec![("header_spacing", "1".to_string())]);
        assert!(dynamic_opts("hevc_amf", 240).is_empty());
        assert!(dynamic_opts("h264_nvenc", 240).is_empty());
    }

    #[test]
    fn no_duplicate_keys() {
        for name in ["h264_nvenc", "hevc_nvenc", "av1_nvenc", "h264_amf", "hevc_amf", "av1_amf"] {
            let opts = preset_for(name).opts;
            for (i, (k, _)) in opts.iter().enumerate() {
                assert!(!opts[i + 1..].iter().any(|(k2, _)| k2 == k), "{name}: duplicate {k}");
            }
        }
    }

    #[test]
    fn qsv_uses_low_power() {
        assert!(has("h264_qsv", "low_power", "1"));
    }

    #[test]
    fn unknown_encoder_empty_opts() {
        assert!(preset_for("unknown_x").opts.is_empty());
        assert!(dynamic_opts("unknown_x", 240).is_empty());
    }
}
