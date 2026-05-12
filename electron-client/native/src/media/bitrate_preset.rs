//! Per-encoder option strings for low-latency screen-share encoding.
//! Values from the design spec §6 — kept as plain data so the
//! encoder.rs initialization code stays small.

#![cfg(target_os = "windows")]

pub struct PresetOptions {
    /// Key/value pairs forwarded to AVDictionary at avcodec_open2 time.
    pub opts: &'static [(&'static str, &'static str)],
}

pub fn preset_for(encoder_name: &str) -> PresetOptions {
    match encoder_name {
        "h264_nvenc" | "hevc_nvenc" => PresetOptions {
            opts: &[
                ("preset", "p4"),
                ("tune", "ull"),
                ("rc", "cbr"),
                ("b_ref_mode", "disabled"),
                ("zerolatency", "1"),
            ],
        },
        "av1_nvenc" => PresetOptions {
            opts: &[
                ("preset", "p4"),
                ("tune", "ull"),
                ("rc", "cbr"),
                ("tile_columns", "2"),
                ("tile_rows", "1"),
            ],
        },
        "h264_amf" | "hevc_amf" | "av1_amf" => PresetOptions {
            opts: &[
                ("usage", "lowlatency"),
                ("quality", "speed"),
                ("rc", "cbr"),
                ("enforce_hrd", "true"),
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn nvenc_h264_uses_p4_preset() {
        let p = preset_for("h264_nvenc");
        assert!(p.opts.iter().any(|(k, v)| *k == "preset" && *v == "p4"));
        assert!(p.opts.iter().any(|(k, v)| *k == "rc" && *v == "cbr"));
    }

    #[test]
    fn amf_uses_lowlatency() {
        let p = preset_for("h264_amf");
        assert!(p.opts.iter().any(|(k, v)| *k == "usage" && *v == "lowlatency"));
    }

    #[test]
    fn qsv_uses_low_power() {
        let p = preset_for("h264_qsv");
        assert!(p.opts.iter().any(|(k, v)| *k == "low_power" && *v == "1"));
    }

    #[test]
    fn unknown_encoder_empty_opts() {
        let p = preset_for("unknown_x");
        assert!(p.opts.is_empty());
    }
}
