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
//! Windows encoder uses `preset_for` / `dynamic_opts`, the Linux encoder
//! `vaapi_opts`, and both `detail_opts` and `software_opts`.

#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

pub struct PresetOptions {
    /// Key/value pairs forwarded to AVDictionary at avcodec_open2 time.
    pub opts: &'static [(&'static str, &'static str)],
}

/// Overrides for a 5 fps stream (`detail`: Slides, or 5 fps under Advanced),
/// laid over `preset_for` here and over the Linux encoder's own options: each
/// encoder's quality preset in place of its fastest. At 5 fps a frame has
/// 200 ms to encode; zero B-frames and the low-latency rate control stay.
pub fn detail_opts(encoder_name: &str) -> &'static [(&'static str, &'static str)] {
    match encoder_name {
        "h264_nvenc" | "hevc_nvenc" | "av1_nvenc" => &[("preset", "p7"), ("tune", "hq")],
        "h264_amf" | "hevc_amf" | "av1_amf" => &[("quality", "quality")],
        // qsvenc.h "preset": TargetUsage 1 (best quality) .. 7 (speed).
        "h264_qsv" | "hevc_qsv" | "av1_qsv" => &[("preset", "slower")],
        // VAAPI has no portable quality knob (compression_level means a
        // different scale per driver): the driver's default stays. Software
        // picks its preset in `software_opts`.
        _ => &[],
    }
}

/// A `detail` stream's keyframe interval. Every keyframe briefly softens a
/// still slide (it has to fit the rate-control buffer, while the frames
/// before it had refined the picture), and the usual 2 to 4 s made that a
/// pulse. A viewer joining or losing packets asks for a keyframe anyway;
/// this is only the safety net.
pub const DETAIL_KEYFRAME_SECS: u32 = 60;

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
        // Quick Sync (libavcodec/qsvenc.h QSV_COMMON_OPTS, qsvenc_{h264,hevc,
        // av1}.c; names checked against n8.0.1 and 8.1.3). NOT verified on
        // Intel hardware (tools/verify-encoders.md).
        //  - async_depth=1: the default (4) lets the runtime hold four
        //    frames before the first packet comes out.
        //  - forced_idr=1: a forced keyframe (pict_type I) is an IDR; the
        //    default (0) makes it a plain I frame a watcher can't join on.
        //  - low_power stays at its default (-1, the runtime decides):
        //    Arc / Xe / Meteor Lake only have the low-power (VDEnc) encoder
        //    and pick it themselves, while forcing 1 fails on generations
        //    whose VDEnc lacks the codec or the rate control.
        //  - scenario=displayremoting: the runtime's hint for screen
        //    content (mfxExtCodingOption3.ScenarioInfo).
        //  - CBR comes from bit_rate == rc_max_rate (qsvenc.c select_rc_mode)
        //    and zero B-frames from max_b_frames; no option needed.
        //  - h264 only: look_ahead=0 (its default is on, which is VBR-LA and
        //    buffers frames).
        // Parameter sets: the runtime writes SPS/PPS (VPS) with every IDR,
        // and qsvenc always fills extradata with them in Annex B, so the
        // Windows encoder's safety net is armed as well.
        "h264_qsv" => PresetOptions {
            opts: &[
                ("preset", "veryfast"),
                ("async_depth", "1"),
                ("forced_idr", "1"),
                ("scenario", "displayremoting"),
                ("look_ahead", "0"),
            ],
        },
        "hevc_qsv" | "av1_qsv" => PresetOptions {
            opts: &[
                ("preset", "veryfast"),
                ("async_depth", "1"),
                ("forced_idr", "1"),
                ("scenario", "displayremoting"),
            ],
        },
        _ => PresetOptions { opts: &[] },
    }
}

/// VAAPI encoder options, the same for h264_vaapi / hevc_vaapi / av1_vaapi
/// (libavcodec/vaapi_encode.h VAAPI_ENCODE_COMMON_OPTIONS, hw_base_encode.h
/// HW_BASE_ENCODE_COMMON_OPTIONS; checked against FFmpeg 8.0 and 8.1.3).
/// NOT verified on AMD or Intel hardware (tools/verify-encoders.md).
///
///  - async_depth=1: with the default (2) hw_base_encode.c's receive_packet
///    returns EAGAIN until two frames are queued, i.e. one frame of latency.
///  - idr_interval=0: every GOP's I frame is an IDR (the default, stated).
///  - B-frames are off through `max_b_frames = 0` (b_depth follows it).
///  - low_power is left at 0: vaapi_encode.c then accepts VAEntrypointEncSlice
///    or EncSliceLP, whichever the driver has (radeonsi: EncSlice only;
///    newer Intel, Arc / Meteor Lake: EncSliceLP only), where low_power=1
///    would insist on EncSliceLP and fail on AMD.
///  - A forced keyframe needs no option: pict_type I on the input frame is a
///    forced IDR (hw_base_encode.c).
///  - Parameter sets: vaapi_encode.c writes the packed sequence header
///    (SPS/PPS, VPS, the AV1 sequence header) with every IDR when the driver
///    takes packed headers (iHD, recent radeonsi); a driver that
///    does not writes its own with every IDR. The encoder's H.264 / HEVC
///    caches cover a keyframe that still arrives without.
///  - The bitrate cannot change while the encoder runs (vaapi_encode reads
///    it at init only), as with NVENC on Linux.
pub fn vaapi_opts() -> &'static [(&'static str, &'static str)] {
    &[("async_depth", "1"), ("idr_interval", "0")]
}

/// `rc_mode` values the VAAPI open tries in order (None = let FFmpeg pick:
/// CBR / VBR / CQP by what the driver lists). CBR is what the presets mean;
/// radeonsi lists CQP / CBR / VBR and iHD CQP / CBR / VBR / ICQ / QVBR, so the
/// first normally opens. Intel generations whose low-power encoder needs HuC
/// firmware for CBR fail it without, and get VBR capped at the target.
pub const VAAPI_RC_LADDER: [Option<&str>; 3] = [Some("CBR"), Some("VBR"), None];

/// Options for the software encoder (FFmpeg's libsvtav1, SVT-AV1 4.x): a
/// low-delay, screen-content configuration. Every choice here was measured
/// (docs/superpowers/specs/2026-11-26-encoders-every-vendor-design.md §6).
///
/// libsvtav1.c takes the rest from the codec context: CBR because
/// `rc_max_rate == bit_rate`, the keyframe distance from `gop_size`, the
/// buffer from `rc_buffer_size`, and a key frame for an input frame whose
/// pict_type is I (SVT-AV1 4.x honours that in low-delay mode without any
/// flag. Only 4.2 was run: FFmpeg's libsvtav1.c says earlier versions
/// take no keyframe requests, so the build is to use 4.0 or later, and the
/// harness's forced-keyframe check tells if one does not).
///
///  - pred-struct=1: low delay. Frames come out in order, and SVT-AV1's
///    packet call then blocks until the frame just sent is encoded
///    (enc_handle.c svt_av1_enc_get_packet), so a frame's packet is back
///    when the encode call returns, as with a hardware encoder.
///  - NOT rtc=1. SVT-AV1's real-time mode is tuned for camera video: on a
///    page of text it measured 9 dB PSNR-Y worse at the same preset and
///    bitrate, cost more CPU, and undershot the bitrate by a sixth.
///  - scm=1: screen-content tools (palette, intra block copy) on. They
///    are the difference between preset 8 (near lossless text at 4 Mbps)
///    and the faster presets, where SVT-AV1 drops them.
///  - lookahead=0, enable-tf=0: nothing that holds frames back or blends
///    them.
///  - No `lp`: SVT-AV1 picks its parallelism from the core count.
///  - The sequence header is in band on every key frame (SVT-AV1 writes one
///    at the start of each key frame's temporal unit), so no description is
///    sent.
pub fn software_opts(width: u32, height: u32, fps: u32, logical_cpus: usize, detail: bool) -> Vec<(&'static str, String)> {
    use super::encoder_backend::software_preset;
    // DECIBELL_SVT_PRESET overrides the measured choice, for measuring it
    // again (encoder_harness::software_measurements).
    let preset = std::env::var("DECIBELL_SVT_PRESET")
        .ok()
        .and_then(|v| v.trim().parse::<u32>().ok())
        .filter(|p| (4..=13).contains(p))
        .unwrap_or_else(|| software_preset(width, height, fps, logical_cpus, detail));
    let mut params = "pred-struct=1:scm=1:lookahead=0:enable-tf=0".to_string();
    // DECIBELL_SVT_PARAMS: extra `key=value:…` appended (a later key wins),
    // for trying settings without a rebuild.
    if let Ok(extra) = std::env::var("DECIBELL_SVT_PARAMS") {
        if !extra.trim().is_empty() {
            params.push(':');
            params.push_str(extra.trim());
        }
    }
    vec![("preset", preset.to_string()), ("svtav1-params", params)]
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
        for name in [
            "h264_nvenc", "hevc_nvenc", "av1_nvenc", "h264_amf", "hevc_amf", "av1_amf", "h264_qsv", "hevc_qsv",
            "av1_qsv",
        ] {
            let opts = preset_for(name).opts;
            for (i, (k, _)) in opts.iter().enumerate() {
                assert!(!opts[i + 1..].iter().any(|(k2, _)| k2 == k), "{name}: duplicate {k}");
            }
        }
    }

    #[test]
    fn qsv_is_low_latency_and_leaves_low_power_to_the_runtime() {
        for name in ["h264_qsv", "hevc_qsv", "av1_qsv"] {
            assert!(has(name, "async_depth", "1"), "{name}");
            assert!(has(name, "forced_idr", "1"), "{name}");
            assert!(has(name, "scenario", "displayremoting"), "{name}");
            // Forcing low_power=1 fails where VDEnc lacks the codec.
            assert!(!preset_for(name).opts.iter().any(|(k, _)| *k == "low_power"), "{name}");
            assert_eq!(detail_opts(name), &[("preset", "slower")], "{name}");
        }
        // look_ahead is an h264_qsv option only.
        assert!(has("h264_qsv", "look_ahead", "0"));
        assert!(!preset_for("hevc_qsv").opts.iter().any(|(k, _)| *k == "look_ahead"));
    }

    #[test]
    fn vaapi_is_low_latency_and_tries_cbr_first() {
        assert!(vaapi_opts().contains(&("async_depth", "1")));
        assert!(vaapi_opts().contains(&("idr_interval", "0")));
        // low_power=1 would refuse AMD (EncSlice only).
        assert!(!vaapi_opts().iter().any(|(k, _)| *k == "low_power"));
        assert_eq!(VAAPI_RC_LADDER, [Some("CBR"), Some("VBR"), None]);
        assert!(detail_opts("h264_vaapi").is_empty());
    }

    #[test]
    fn software_is_low_delay_screen_content_and_not_rtc() {
        let opts = software_opts(1920, 1080, 30, 8, false);
        let get = |k: &str| opts.iter().find(|(key, _)| *key == k).map(|(_, v)| v.clone()).unwrap();
        let preset: u32 = get("preset").parse().unwrap();
        assert!((8..=12).contains(&preset));
        if std::env::var_os("DECIBELL_SVT_PARAMS").is_none() {
            let params = get("svtav1-params");
            assert_eq!(params, "pred-struct=1:scm=1:lookahead=0:enable-tf=0");
            // Measured: the real-time mode is worse on text in every respect.
            assert!(!params.contains("rtc"));
        }
        // Slides: the quality preset.
        if std::env::var_os("DECIBELL_SVT_PRESET").is_none() {
            assert_eq!(software_opts(1920, 1080, 5, 8, true)[0], ("preset", "8".to_string()));
        }
    }

    #[test]
    fn unknown_encoder_empty_opts() {
        assert!(preset_for("unknown_x").opts.is_empty());
        assert!(dynamic_opts("unknown_x", 240).is_empty());
    }

    #[test]
    fn detail_uses_the_quality_presets_and_keeps_low_latency() {
        for name in ["h264_nvenc", "hevc_nvenc", "av1_nvenc"] {
            assert_eq!(detail_opts(name), &[("preset", "p7"), ("tune", "hq")], "{name}");
            // Overrides only: the B-frame and IDR rules underneath stay.
            assert!(preset_for(name).opts.iter().any(|(k, v)| *k == "forced-idr" && *v == "1"));
        }
        for name in ["h264_amf", "hevc_amf", "av1_amf"] {
            assert_eq!(detail_opts(name), &[("quality", "quality")], "{name}");
        }
        assert!(detail_opts("libx264").is_empty());
    }
}
