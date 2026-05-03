//! GStreamer hardware-decoder availability probe.
//!
//! WebKitGTK's `<video>` MSE pipeline picks decoders from GStreamer's
//! plugin registry at construction. If the right hardware-backed plugin
//! isn't installed (typical missing piece on minimal Arch / Debian:
//! `gst-plugins-bad` with the nvcodec or va backend), playback silently
//! falls back to software decode (`avdec_h264` / `dav1ddec` / etc.) —
//! still works, but burns CPU. We probe the registry at app start and
//! warn the user when their setup is missing hardware decode for any
//! codec they're likely to receive.

use std::process::{Command, Stdio};
use std::sync::OnceLock;

#[derive(Debug, Clone, PartialEq)]
pub enum HwDecoderSource {
    /// NVIDIA NVDEC via gst-plugins-bad nvcodec
    Nvidia,
    /// AMD/Intel via the modern `va` plugin (gst-plugins-bad >= 1.20)
    VaapiNew,
    /// AMD/Intel via the legacy `vaapi` plugin (gstreamer-vaapi)
    VaapiLegacy,
    /// No hardware decoder registered for this codec — decode will be
    /// software (avdec_*/dav1ddec). Still works; uses more CPU.
    None,
}

#[derive(Debug, Clone)]
pub struct HwDecoderReport {
    pub h264: HwDecoderSource,
    pub hevc: HwDecoderSource,
    pub av1: HwDecoderSource,
}

static REPORT: OnceLock<HwDecoderReport> = OnceLock::new();

fn plugin_available(name: &str) -> bool {
    Command::new("gst-inspect-1.0")
        .arg(name)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

fn pick_h264() -> HwDecoderSource {
    if plugin_available("nvh264dec") { HwDecoderSource::Nvidia }
    else if plugin_available("vah264dec") { HwDecoderSource::VaapiNew }
    else if plugin_available("vaapih264dec") { HwDecoderSource::VaapiLegacy }
    else { HwDecoderSource::None }
}

fn pick_hevc() -> HwDecoderSource {
    if plugin_available("nvh265dec") { HwDecoderSource::Nvidia }
    else if plugin_available("vah265dec") { HwDecoderSource::VaapiNew }
    else if plugin_available("vaapih265dec") { HwDecoderSource::VaapiLegacy }
    else { HwDecoderSource::None }
}

fn pick_av1() -> HwDecoderSource {
    if plugin_available("nvav1dec") { HwDecoderSource::Nvidia }
    else if plugin_available("vaav1dec") { HwDecoderSource::VaapiNew }
    else { HwDecoderSource::None }
}

/// Probe once and cache the result. Subsequent calls return the cached
/// value. Probing runs `gst-inspect-1.0` 3-9 times (small subprocess
/// each, ~5-20ms total typically); doing it once at startup is fine.
pub fn report() -> &'static HwDecoderReport {
    REPORT.get_or_init(|| {
        // If gst-inspect-1.0 isn't on PATH, we can't probe at all.
        let probe_works = Command::new("gst-inspect-1.0")
            .arg("--version")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status()
            .map(|s| s.success())
            .unwrap_or(false);
        if !probe_works {
            eprintln!("[gst-probe] gst-inspect-1.0 not available on PATH — \
                       can't determine hardware decode availability. \
                       MSE playback will use whatever decoders WebKitGTK's \
                       GStreamer pipeline picks at runtime.");
            return HwDecoderReport {
                h264: HwDecoderSource::None,
                hevc: HwDecoderSource::None,
                av1: HwDecoderSource::None,
            };
        }
        let report = HwDecoderReport {
            h264: pick_h264(),
            hevc: pick_hevc(),
            av1: pick_av1(),
        };
        eprintln!(
            "[gst-probe] decoder backends: h264={:?} hevc={:?} av1={:?}",
            report.h264, report.hevc, report.av1
        );
        for (codec, source, install_hint) in [
            ("H.264", &report.h264, "gst-plugins-bad with nvcodec or va"),
            ("HEVC",  &report.hevc, "gst-plugins-bad with nvcodec or va"),
            ("AV1",   &report.av1,  "gst-plugins-bad with nvcodec or va (Ada/RDNA3+ for HW; software via dav1ddec is the fallback)"),
        ] {
            if *source == HwDecoderSource::None {
                eprintln!(
                    "[gst-probe] WARNING: no hardware decoder for {} — \
                     playback will fall back to software (more CPU). \
                     Install: {}",
                    codec, install_hint
                );
            }
        }
        report
    })
}

/// Codec byte (1=H264_HW, 2=H264_SW, 3=H265, 4=AV1) → hardware decoder
/// source available on this machine.
pub fn source_for_codec_byte(codec: u8) -> HwDecoderSource {
    let r = report();
    match codec {
        1 | 2 => r.h264.clone(),
        3 => r.hevc.clone(),
        4 => r.av1.clone(),
        _ => HwDecoderSource::None,
    }
}
