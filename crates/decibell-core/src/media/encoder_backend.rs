//! Encoder backends: which ones exist, in which order a machine tries them,
//! what the probe reports, and what a stream start falls through. Pure data
//! and decisions, no FFmpeg and no OS calls, so every rule here is unit
//! tested on any machine (`encoder_probe.rs` does the real opens).
//!
//! The owner's rule (CLAUDE.md, "Video codecs", 2026-11-26): hardware on
//! NVIDIA, AMD and Intel on both OSes, software only when no hardware
//! encoder is usable. Order:
//!
//!  - Linux: NVENC, then VAAPI on each AMD / Intel render node (the
//!    compositor's GPU first), then software.
//!  - Windows: NVENC and AMF (AMF first when the primary adapter is AMD,
//!    as before), then Quick Sync, then software.
//!
//! Software is SVT-AV1 and therefore AV1 only: it is offered only when no
//! hardware backend encodes anything, or when it is forced.
//!
//! `DECIBELL_ENCODER=nvenc|amf|qsv|vaapi|software` forces one backend (every
//! build, release included): only that backend is probed and used, nothing
//! falls through to another. It is how a backend is verified on hardware
//! (`crates/decibell-core/tools/verify-encoders.md`).

use serde::{Deserialize, Serialize};

use super::caps::CodecKind;

pub const VENDOR_NVIDIA: u32 = 0x10DE;
pub const VENDOR_AMD: u32 = 0x1002;
pub const VENDOR_INTEL: u32 = 0x8086;

/// The environment variable that forces one backend.
pub const FORCE_ENV: &str = "DECIBELL_ENCODER";

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Backend {
    Nvenc,
    Amf,
    Qsv,
    Vaapi,
    Software,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Os {
    Linux,
    Windows,
}

impl Os {
    pub fn current() -> Option<Os> {
        if cfg!(target_os = "linux") {
            Some(Os::Linux)
        } else if cfg!(target_os = "windows") {
            Some(Os::Windows)
        } else {
            None
        }
    }
}

/// The codecs a stream can be encoded in, in the order "auto" prefers them.
pub const STREAM_CODECS: [CodecKind; 3] = [CodecKind::H264Hw, CodecKind::H265, CodecKind::Av1];

impl Backend {
    /// The name used in `DECIBELL_ENCODER`, logs and the probe's result.
    pub fn id(self) -> &'static str {
        match self {
            Backend::Nvenc => "nvenc",
            Backend::Amf => "amf",
            Backend::Qsv => "qsv",
            Backend::Vaapi => "vaapi",
            Backend::Software => "software",
        }
    }

    pub fn from_id(id: &str) -> Option<Backend> {
        match id.trim().to_ascii_lowercase().as_str() {
            "nvenc" => Some(Backend::Nvenc),
            "amf" => Some(Backend::Amf),
            "qsv" | "quicksync" => Some(Backend::Qsv),
            "vaapi" => Some(Backend::Vaapi),
            "software" | "sw" | "svtav1" => Some(Backend::Software),
            _ => None,
        }
    }

    pub fn is_hardware(self) -> bool {
        self != Backend::Software
    }

    /// Does this backend exist on `os`? (VAAPI is Linux's AMD / Intel path,
    /// AMF and Quick Sync are Windows'.)
    pub fn exists_on(self, os: Os) -> bool {
        match (self, os) {
            (Backend::Nvenc | Backend::Software, _) => true,
            (Backend::Vaapi, Os::Linux) => true,
            (Backend::Amf | Backend::Qsv, Os::Windows) => true,
            _ => false,
        }
    }

    /// The GPU vendors whose adapters this backend encodes on.
    pub fn vendors(self) -> &'static [u32] {
        match self {
            Backend::Nvenc => &[VENDOR_NVIDIA],
            Backend::Amf => &[VENDOR_AMD],
            Backend::Qsv => &[VENDOR_INTEL],
            Backend::Vaapi => &[VENDOR_AMD, VENDOR_INTEL],
            Backend::Software => &[],
        }
    }

    /// The FFmpeg encoder this backend uses for `codec`; None when it has
    /// none (software encodes AV1 only: no software H.264 by owner rule).
    pub fn encoder_name(self, codec: CodecKind) -> Option<&'static str> {
        let names: [&'static str; 3] = match self {
            Backend::Nvenc => ["h264_nvenc", "hevc_nvenc", "av1_nvenc"],
            Backend::Amf => ["h264_amf", "hevc_amf", "av1_amf"],
            Backend::Qsv => ["h264_qsv", "hevc_qsv", "av1_qsv"],
            Backend::Vaapi => ["h264_vaapi", "hevc_vaapi", "av1_vaapi"],
            Backend::Software => return (codec == CodecKind::Av1).then_some(SOFTWARE_ENCODER),
        };
        match codec {
            CodecKind::H264Hw => Some(names[0]),
            CodecKind::H265 => Some(names[1]),
            CodecKind::Av1 => Some(names[2]),
            CodecKind::H264Sw | CodecKind::Unknown => None,
        }
    }

    /// The backend an FFmpeg encoder name belongs to.
    pub fn of_encoder(name: &str) -> Option<Backend> {
        if name == SOFTWARE_ENCODER {
            Some(Backend::Software)
        } else if name.ends_with("_nvenc") {
            Some(Backend::Nvenc)
        } else if name.ends_with("_amf") {
            Some(Backend::Amf)
        } else if name.ends_with("_qsv") {
            Some(Backend::Qsv)
        } else if name.ends_with("_vaapi") {
            Some(Backend::Vaapi)
        } else {
            None
        }
    }
}

/// FFmpeg's SVT-AV1 wrapper.
pub const SOFTWARE_ENCODER: &str = "libsvtav1";

pub fn vendor_name(vendor_id: u32) -> &'static str {
    match vendor_id {
        VENDOR_NVIDIA => "NVIDIA",
        VENDOR_AMD => "AMD",
        VENDOR_INTEL => "Intel",
        _ => "GPU",
    }
}

/// What a UI shows for "Encoding: …": "NVENC (NVIDIA)", "VAAPI (AMD)",
/// "Quick Sync (Intel)", "AMF (AMD)", "Software (SVT-AV1)".
pub fn backend_label(backend: Backend, vendor_id: u32) -> String {
    match backend {
        Backend::Nvenc => "NVENC (NVIDIA)".to_string(),
        Backend::Amf => "AMF (AMD)".to_string(),
        Backend::Qsv => "Quick Sync (Intel)".to_string(),
        Backend::Vaapi => format!("VAAPI ({})", vendor_name(vendor_id)),
        Backend::Software => "Software (SVT-AV1)".to_string(),
    }
}

/// One GPU as the OS lists it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Adapter {
    pub vendor_id: u32,
    pub device_id: u32,
    /// Shown in logs only ("AMD Radeon RX 7800 XT", or the kernel driver
    /// name on Linux).
    pub name: String,
    /// Whatever identifies the driver build, for the probe cache: the
    /// NVIDIA module version, the VA driver file's size and date, the DXGI
    /// user-mode driver version.
    pub driver: String,
    /// The adapter the desktop is composited on (Linux `boot_vga`, the
    /// first DXGI adapter on Windows).
    pub primary: bool,
    /// Linux: the DRM render node VAAPI opens (`/dev/dri/renderD128`).
    pub render_node: Option<String>,
}

/// One thing to try: a backend on one adapter (`adapter` indexes the list
/// the order was built from; None for software, and for a hardware backend
/// tried without a matching adapter in the list).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Candidate {
    pub backend: Backend,
    pub adapter: Option<usize>,
}

/// `DECIBELL_ENCODER`, if it names a backend. An unknown value is ignored
/// with a warning, never an error: the app must still stream.
pub fn forced_backend() -> Option<Backend> {
    let raw = std::env::var(FORCE_ENV).ok()?;
    if raw.trim().is_empty() {
        return None;
    }
    let parsed = Backend::from_id(&raw);
    if parsed.is_none() {
        log::warn!("[encoder] {FORCE_ENV}={raw:?} names no backend (nvenc, amf, qsv, vaapi, software); ignored");
    }
    parsed
}

/// The order backends are tried in on this machine.
///
/// Adapters of a backend's vendors come primary first. NVENC is tried even
/// when no NVIDIA adapter is listed only if the list is empty (enumeration
/// failed): on a machine known to have no NVIDIA GPU, opening it would only
/// load and fail.
pub fn backend_order(os: Os, adapters: &[Adapter], forced: Option<Backend>) -> Vec<Candidate> {
    let on = |backend: Backend| -> Vec<Candidate> {
        let mut idx: Vec<usize> = (0..adapters.len())
            .filter(|&i| backend.vendors().contains(&adapters[i].vendor_id))
            .collect();
        // Stable: primary first, then the OS's order.
        idx.sort_by_key(|&i| !adapters[i].primary);
        idx.into_iter().map(|i| Candidate { backend, adapter: Some(i) }).collect()
    };
    let software = Candidate { backend: Backend::Software, adapter: None };

    if let Some(backend) = forced {
        if !backend.exists_on(os) {
            log::warn!("[encoder] {FORCE_ENV}={} does not exist on this OS", backend.id());
            return Vec::new();
        }
        if backend == Backend::Software {
            return vec![software];
        }
        let found = on(backend);
        // Forced with no matching adapter listed: one attempt anyway, so the
        // log carries the real reason it fails.
        return if found.is_empty() { vec![Candidate { backend, adapter: None }] } else { found };
    }

    let hardware: &[Backend] = match os {
        Os::Linux => &[Backend::Nvenc, Backend::Vaapi],
        // The dedicated GPUs' encoders first, the primary adapter's of the
        // two leading (what the probe did before Quick Sync existed: an
        // Intel-primary laptop with an NVIDIA or AMD GPU keeps NVENC / AMF),
        // then Quick Sync.
        Os::Windows => {
            let amd_primary = adapters.iter().any(|a| a.primary && a.vendor_id == VENDOR_AMD);
            if amd_primary {
                &[Backend::Amf, Backend::Nvenc, Backend::Qsv]
            } else {
                &[Backend::Nvenc, Backend::Amf, Backend::Qsv]
            }
        }
    };
    let mut order = Vec::new();
    for &backend in hardware {
        if adapters.is_empty() {
            order.push(Candidate { backend, adapter: None });
        } else {
            order.extend(on(backend));
        }
    }
    order.push(software);
    order
}

/// A backend that encodes `codec` on this machine, as the probe found it.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct EncoderCap {
    pub codec: CodecKind,
    pub backend: Backend,
    /// 0 for software.
    pub vendor_id: u32,
    pub encoder_name: String,
    /// `backend_label`.
    pub label: String,
    pub hardware: bool,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    /// Software only: the pixel rate (width × height × fps) it accepts, so
    /// a UI can grey out what the CPU cannot do (`software_accepts`). 0 for
    /// hardware: its ceiling is `max_*` alone.
    pub max_pixel_rate: u64,
    /// Linux VAAPI: the render node it runs on.
    pub render_node: Option<String>,
}

/// A backend the probe tried and did not take, with the reason.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Skipped {
    pub codec: CodecKind,
    pub backend: Backend,
    pub vendor_id: u32,
    pub reason: String,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeReport {
    /// One entry per codec that has a working backend, in `STREAM_CODECS`
    /// order.
    pub caps: Vec<EncoderCap>,
    pub skipped: Vec<Skipped>,
}

/// What one test encode said.
pub type ProbeResult = Result<(), String>;

fn vendor_of(candidate: Candidate, adapters: &[Adapter]) -> u32 {
    candidate.adapter.and_then(|i| adapters.get(i)).map_or(0, |a| a.vendor_id)
}

/// The largest stream a hardware backend is offered at. These are the
/// ceilings the two platforms advertised before backends were named
/// (Windows: 1440p for H.264, 4K otherwise, 120 fps; Linux: 4K60), kept so
/// nothing changes for a machine that streamed before.
pub fn hardware_ceiling(os: Os, codec: CodecKind) -> (u32, u32, u32) {
    match (os, codec) {
        (Os::Windows, CodecKind::H264Hw) => (2560, 1440, 120),
        (Os::Windows, _) => (3840, 2160, 120),
        (Os::Linux, _) => (3840, 2160, 60),
    }
}

/// Software (SVT-AV1) limits, from the measurements in
/// docs/superpowers/specs/2026-11-26-encoders-every-vendor-design.md §6 (an
/// i7-12700K, 20 threads: SVT-AV1 plus the conversion cost 18 to 25 ms of
/// CPU a 1080p frame at the presets used, whether the screen moves or not:
/// about three quarters of a core at 30 fps, a little over one at 60).
///
/// The budget is a pixel rate, width × height × fps, that grows with the
/// logical CPU count: 1080p30 worth for every 6 of them, at least 720p30,
/// at most 1440p60, and never above 1440p or 60 fps. On a laptop whose
/// cores are half as fast as the measured ones that keeps the encoder to
/// roughly a third of the machine: the rest is what is being shared.
///
/// | logical CPUs | accepts |
/// |---|---|
/// | up to 5 | 720p30 (720p up to 45 fps at 4), 1080p at 20 fps or less |
/// | 6 to 11 | 1080p30, 720p60 |
/// | 12 to 19 | 1440p30, 1080p60 |
/// | 20 to 23 | the same (1440p up to 56 fps at 20: measured at the edge of its frame interval at 60) |
/// | 24 and up | 1440p60 |
pub fn software_limits(logical_cpus: usize) -> SoftwareLimits {
    const P720_30: u64 = 1280 * 720 * 30;
    const P1080_30: u64 = 1920 * 1080 * 30;
    const P1440_30: u64 = 2560 * 1440 * 30;
    const P1440_60: u64 = 2560 * 1440 * 60;
    let max_pixel_rate = (P1080_30 * logical_cpus.max(1) as u64 / 6).clamp(P720_30, P1440_60);
    let (max_width, max_height) = if max_pixel_rate >= P1440_30 { (2560, 1440) } else { (1920, 1080) };
    SoftwareLimits { max_width, max_height, max_fps: 60, max_pixel_rate }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SoftwareLimits {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub max_pixel_rate: u64,
}

impl SoftwareLimits {
    /// Can software encode this stream? A 5 fps `detail` stream (Slides) is
    /// always within the pixel rate at the size ceiling.
    pub fn accepts(&self, width: u32, height: u32, fps: u32) -> bool {
        let (long, short) = (width.max(height), width.min(height));
        long <= self.max_width
            && short <= self.max_height
            && fps <= self.max_fps
            && width as u64 * height as u64 * fps as u64 <= self.max_pixel_rate
    }

    /// The frame rate a `width`×`height` software stream may run at.
    pub fn max_fps_at(&self, width: u32, height: u32) -> u32 {
        let pixels = (width as u64 * height as u64).max(1);
        ((self.max_pixel_rate / pixels) as u32).min(self.max_fps)
    }
}

/// Why software cannot take a stream of this size and rate on this CPU;
/// None when it can. A start skips the software encoder with this reason
/// (it is not a failure of the encoder, so the probe cache keeps it).
pub fn software_refusal(width: u32, height: u32, fps: u32, logical_cpus: usize) -> Option<String> {
    let limits = software_limits(logical_cpus);
    (!limits.accepts(width, height, fps)).then(|| {
        format!(
            "software encoding stops at {}x{}, and at {} fps for this size, on this CPU; the stream is {width}x{height}@{fps}",
            limits.max_width,
            limits.max_height,
            limits.max_fps_at(width, height),
        )
    })
}

/// What a stream started on: the codec that is announced, and the backend
/// for the UI ("Encoding: VAAPI (AMD)").
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartedEncoder {
    pub codec: CodecKind,
    pub backend: Backend,
    pub label: String,
}

/// SVT-AV1's preset for a stream: the slowest of 8, 10 and 12 the CPU can
/// afford, judged by the pixel rate each logical CPU would carry.
///
/// Measured (design record §6, scrolling text): preset 8 keeps the
/// screen-content tools and returns the text nearly lossless (PSNR-Y 56 dB)
/// but takes 2 to 3 times the CPU of preset 10 (37 dB) and at 1080p30 ran
/// past its frame interval on 20 threads; 12 is a tenth to a quarter
/// cheaper than 10 for 1 dB. 9 costs twice what 10 does for little, and 11
/// is 12 within noise, so neither is used.
pub fn software_preset(width: u32, height: u32, fps: u32, logical_cpus: usize, detail: bool) -> u32 {
    let cpus = logical_cpus.max(1) as u64;
    if detail {
        // 5 fps: 200 ms a frame. Spend it on the text (a 1440p frame took
        // 26 ms, 50 at worst, at preset 8), unless the CPU is small.
        return if cpus >= 8 { 8 } else { 10 };
    }
    let per_cpu = width as u64 * height as u64 * fps as u64 / cpus;
    // 720p30 on 20 threads is 1.4 M pixels a second per thread: preset 8
    // took 11 ms a frame (20 at worst) there. 1080p30 (3.1 M) gets 10, as
    // does an 8-thread laptop at 1080p30 (7.8 M); a 12-thread machine at
    // 1080p60 (10.4 M) and a 4-thread one at 720p30 (6.9 M -> 10) or at
    // anything larger get the fastest.
    if per_cpu <= 1_500_000 {
        8
    } else if per_cpu <= 8_000_000 {
        10
    } else {
        12
    }
}

/// Run the probe over `order`. `test(candidate, codec)` opens that backend
/// for that codec on that adapter and encodes a frame or two.
///
/// Per codec the first hardware candidate that works wins and later ones
/// are not opened (a stream start falls through to them if it fails,
/// `start_plan`). Software is tested only when no hardware cap came out, or
/// when it is the only candidate (forced).
pub fn run_probe(
    os: Os,
    order: &[Candidate],
    adapters: &[Adapter],
    logical_cpus: usize,
    mut test: impl FnMut(Candidate, CodecKind) -> ProbeResult,
) -> ProbeReport {
    let mut report = ProbeReport::default();
    for codec in STREAM_CODECS {
        for &candidate in order.iter().filter(|c| c.backend.is_hardware()) {
            let Some(name) = candidate.backend.encoder_name(codec) else {
                continue;
            };
            let vendor_id = vendor_of(candidate, adapters);
            match test(candidate, codec) {
                Ok(()) => {
                    let (max_width, max_height, max_fps) = hardware_ceiling(os, codec);
                    report.caps.push(EncoderCap {
                        codec,
                        backend: candidate.backend,
                        vendor_id,
                        encoder_name: name.to_string(),
                        label: backend_label(candidate.backend, vendor_id),
                        hardware: true,
                        max_width,
                        max_height,
                        max_fps,
                        max_pixel_rate: 0,
                        render_node: candidate
                            .adapter
                            .and_then(|i| adapters.get(i))
                            .and_then(|a| a.render_node.clone()),
                    });
                    break;
                }
                Err(reason) => report.skipped.push(Skipped { codec, backend: candidate.backend, vendor_id, reason }),
            }
        }
    }
    let software = Candidate { backend: Backend::Software, adapter: None };
    if report.caps.is_empty() && order.contains(&software) {
        match test(software, CodecKind::Av1) {
            Ok(()) => report.caps.push(software_cap(logical_cpus)),
            Err(reason) => {
                report.skipped.push(Skipped { codec: CodecKind::Av1, backend: Backend::Software, vendor_id: 0, reason })
            }
        }
    }
    report
}

pub fn software_cap(logical_cpus: usize) -> EncoderCap {
    let limits = software_limits(logical_cpus);
    EncoderCap {
        codec: CodecKind::Av1,
        backend: Backend::Software,
        vendor_id: 0,
        encoder_name: SOFTWARE_ENCODER.to_string(),
        label: backend_label(Backend::Software, 0),
        hardware: false,
        max_width: limits.max_width,
        max_height: limits.max_height,
        max_fps: limits.max_fps,
        max_pixel_rate: limits.max_pixel_rate,
        render_node: None,
    }
}

/// One encoder a stream start may open.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct StartCandidate {
    pub candidate: Candidate,
    pub codec: CodecKind,
    pub encoder_name: &'static str,
}

/// What a stream start tries, in order, for a requested codec.
///
/// Hardware first: every candidate in `order` that has an encoder for the
/// codec and that the probe did not see fail for it. (Candidates after the
/// probe's winner were never opened, so they stay in: they are what a
/// start falls through to.) Then software, when it is in the order and the
/// stream may be AV1: the requested codec is AV1, or the caller did not pin
/// the codec (`codec_locked`: the announced codec may not change).
pub fn start_plan(
    order: &[Candidate],
    adapters: &[Adapter],
    report: &ProbeReport,
    codec: CodecKind,
    codec_locked: bool,
) -> Vec<StartCandidate> {
    let failed = |c: Candidate, codec: CodecKind| {
        let vendor_id = vendor_of(c, adapters);
        report.skipped.iter().any(|s| s.backend == c.backend && s.vendor_id == vendor_id && s.codec == codec)
    };
    let mut plan = Vec::new();
    for &candidate in order.iter().filter(|c| c.backend.is_hardware()) {
        if let Some(encoder_name) = candidate.backend.encoder_name(codec) {
            if !failed(candidate, codec) {
                plan.push(StartCandidate { candidate, codec, encoder_name });
            }
        }
    }
    let software = Candidate { backend: Backend::Software, adapter: None };
    if order.contains(&software)
        && (codec == CodecKind::Av1 || !codec_locked)
        && !failed(software, CodecKind::Av1)
    {
        plan.push(StartCandidate { candidate: software, codec: CodecKind::Av1, encoder_name: SOFTWARE_ENCODER });
    }
    plan
}

impl ProbeReport {
    pub fn cap(&self, codec: CodecKind) -> Option<&EncoderCap> {
        self.caps.iter().find(|c| c.codec == codec)
    }

    /// A stream start saw `candidate` fail for `codec`: remember it, and
    /// drop the cap that promised it.
    pub fn mark_failed(&mut self, candidate: Candidate, adapters: &[Adapter], codec: CodecKind, reason: &str) {
        let vendor_id = vendor_of(candidate, adapters);
        self.caps.retain(|c| !(c.codec == codec && c.backend == candidate.backend && c.vendor_id == vendor_id));
        if !self.skipped.iter().any(|s| s.backend == candidate.backend && s.vendor_id == vendor_id && s.codec == codec) {
            self.skipped.push(Skipped { codec, backend: candidate.backend, vendor_id, reason: reason.to_string() });
        }
    }

    /// A stream start is running on `candidate` for `codec`: that is the
    /// cap for the codec from now on (it replaces one that failed, or fills
    /// a gap the probe left because it stopped at an earlier backend).
    pub fn mark_working(
        &mut self,
        os: Os,
        candidate: Candidate,
        adapters: &[Adapter],
        codec: CodecKind,
        logical_cpus: usize,
    ) {
        let vendor_id = vendor_of(candidate, adapters);
        if self.caps.iter().any(|c| c.codec == codec && c.backend == candidate.backend && c.vendor_id == vendor_id) {
            return;
        }
        let Some(name) = candidate.backend.encoder_name(codec) else {
            return;
        };
        let cap = if candidate.backend == Backend::Software {
            // Software is reported only while no hardware cap is: a start
            // that fell to it with hardware caps left changes nothing.
            if self.caps.iter().any(|c| c.hardware) {
                return;
            }
            software_cap(logical_cpus)
        } else {
            let (max_width, max_height, max_fps) = hardware_ceiling(os, codec);
            EncoderCap {
                codec,
                backend: candidate.backend,
                vendor_id,
                encoder_name: name.to_string(),
                label: backend_label(candidate.backend, vendor_id),
                hardware: true,
                max_width,
                max_height,
                max_fps,
                max_pixel_rate: 0,
                render_node: candidate.adapter.and_then(|i| adapters.get(i)).and_then(|a| a.render_node.clone()),
            }
        };
        if cap.hardware {
            // Replaces the codec's cap, and ends a software-only state.
            self.caps.retain(|c| c.hardware && c.codec != codec);
        } else {
            self.caps.clear();
        }
        self.caps.push(cap);
        self.caps.sort_by_key(|c| STREAM_CODECS.iter().position(|k| *k == c.codec).unwrap_or(usize::MAX));
    }
}

/// What the probe cache is keyed by: the machine's GPUs and their drivers,
/// the forced backend, and the build (crate version + which encoders the
/// linked FFmpeg has). Any change re-probes.
pub fn fingerprint(os: Os, adapters: &[Adapter], forced: Option<Backend>, build: &str) -> String {
    let mut s = format!("v1|{os:?}|{build}|force={}", forced.map_or("none", Backend::id));
    for a in adapters {
        s.push_str(&format!(
            "|{:04x}:{:04x}:{}:{}:{}",
            a.vendor_id,
            a.device_id,
            a.driver,
            a.primary as u8,
            a.render_node.as_deref().unwrap_or("")
        ));
    }
    s
}

#[cfg(test)]
mod tests {
    use super::*;

    fn gpu(vendor_id: u32, primary: bool, node: Option<&str>) -> Adapter {
        Adapter {
            vendor_id,
            device_id: 1,
            name: vendor_name(vendor_id).to_string(),
            driver: "d1".to_string(),
            primary,
            render_node: node.map(str::to_string),
        }
    }

    fn ids(order: &[Candidate], adapters: &[Adapter]) -> Vec<String> {
        order
            .iter()
            .map(|c| match c.adapter {
                Some(i) => format!("{}@{}", c.backend.id(), vendor_name(adapters[i].vendor_id)),
                None => c.backend.id().to_string(),
            })
            .collect()
    }

    /// The owner's order, per machine shape.
    #[test]
    fn order_per_machine() {
        use Os::*;
        let (nv, amd, intel) = (VENDOR_NVIDIA, VENDOR_AMD, VENDOR_INTEL);
        #[rustfmt::skip]
        let table: &[(Os, &[(u32, bool)], &[&str])] = &[
            // Linux: NVENC, VAAPI (AMD / Intel), software.
            (Linux, &[(nv, true)], &["nvenc@NVIDIA", "software"]),
            (Linux, &[(amd, true)], &["vaapi@AMD", "software"]),
            (Linux, &[(intel, true)], &["vaapi@Intel", "software"]),
            // Hybrid laptop: NVENC, then the iGPU's VAAPI.
            (Linux, &[(intel, true), (nv, false)], &["nvenc@NVIDIA", "vaapi@Intel", "software"]),
            // Two VAAPI GPUs: the compositor's first, whatever the node order.
            (Linux, &[(amd, false), (intel, true)], &["vaapi@Intel", "vaapi@AMD", "software"]),
            (Linux, &[(intel, false), (amd, true)], &["vaapi@AMD", "vaapi@Intel", "software"]),
            // No GPU the kernel lists (a VM): software only.
            (Linux, &[(0x1234, true)], &["software"]),
            // Windows: NVENC / AMF by adapter, then Quick Sync, then software.
            (Windows, &[(nv, true)], &["nvenc@NVIDIA", "software"]),
            (Windows, &[(amd, true)], &["amf@AMD", "software"]),
            (Windows, &[(intel, true)], &["qsv@Intel", "software"]),
            (Windows, &[(amd, true), (nv, false)], &["amf@AMD", "nvenc@NVIDIA", "software"]),
            (Windows, &[(nv, true), (amd, false)], &["nvenc@NVIDIA", "amf@AMD", "software"]),
            // Intel-primary laptops keep the dedicated GPU's encoder first.
            (Windows, &[(intel, true), (nv, false)], &["nvenc@NVIDIA", "qsv@Intel", "software"]),
            (Windows, &[(intel, true), (amd, false)], &["amf@AMD", "qsv@Intel", "software"]),
            // An AMD APU with an NVIDIA GPU: the primary's encoder leads.
            (Windows, &[(amd, true), (nv, false), (intel, false)], &["amf@AMD", "nvenc@NVIDIA", "qsv@Intel", "software"]),
        ];
        for (os, gpus, want) in table {
            let adapters: Vec<Adapter> = gpus.iter().map(|&(v, p)| gpu(v, p, Some("/dev/dri/renderD128"))).collect();
            let order = backend_order(*os, &adapters, None);
            assert_eq!(ids(&order, &adapters), *want, "{os:?} {gpus:x?}");
        }
    }

    #[test]
    fn unknown_adapters_try_every_backend_of_the_os() {
        let order = backend_order(Os::Linux, &[], None);
        assert_eq!(ids(&order, &[]), ["nvenc", "vaapi", "software"]);
        let order = backend_order(Os::Windows, &[], None);
        assert_eq!(ids(&order, &[]), ["nvenc", "amf", "qsv", "software"]);
    }

    #[test]
    fn forcing_a_backend_leaves_only_it() {
        let adapters = [gpu(VENDOR_NVIDIA, true, None), gpu(VENDOR_INTEL, false, Some("/dev/dri/renderD129"))];
        for (forced, want) in [
            (Backend::Nvenc, vec!["nvenc@NVIDIA"]),
            (Backend::Vaapi, vec!["vaapi@Intel"]),
            (Backend::Software, vec!["software"]),
        ] {
            assert_eq!(ids(&backend_order(Os::Linux, &adapters, Some(forced)), &adapters), want);
        }
        // Forced without its hardware: still one attempt, so the reason is logged.
        let nv_only = [gpu(VENDOR_NVIDIA, true, None)];
        assert_eq!(ids(&backend_order(Os::Linux, &nv_only, Some(Backend::Vaapi)), &nv_only), ["vaapi"]);
        // A backend of the other OS is nothing to try.
        assert!(backend_order(Os::Linux, &nv_only, Some(Backend::Amf)).is_empty());
        assert!(backend_order(Os::Windows, &nv_only, Some(Backend::Vaapi)).is_empty());
    }

    #[test]
    fn backend_ids_round_trip_and_name_encoders() {
        for b in [Backend::Nvenc, Backend::Amf, Backend::Qsv, Backend::Vaapi, Backend::Software] {
            assert_eq!(Backend::from_id(b.id()), Some(b));
            for codec in STREAM_CODECS {
                if let Some(name) = b.encoder_name(codec) {
                    assert_eq!(Backend::of_encoder(name), Some(b), "{name}");
                }
            }
        }
        assert_eq!(Backend::from_id(" NVENC "), Some(Backend::Nvenc));
        assert_eq!(Backend::from_id("x264"), None);
        // No software H.264 or HEVC, by owner rule.
        assert_eq!(Backend::Software.encoder_name(CodecKind::H264Hw), None);
        assert_eq!(Backend::Software.encoder_name(CodecKind::H264Sw), None);
        assert_eq!(Backend::Software.encoder_name(CodecKind::H265), None);
        assert_eq!(Backend::Software.encoder_name(CodecKind::Av1), Some("libsvtav1"));
        assert_eq!(backend_label(Backend::Vaapi, VENDOR_AMD), "VAAPI (AMD)");
        assert_eq!(backend_label(Backend::Qsv, VENDOR_INTEL), "Quick Sync (Intel)");
    }

    /// Fake probe: `works` lists (backend, vendor, codec) that encode.
    fn probe(os: Os, adapters: &[Adapter], works: &[(Backend, u32, CodecKind)]) -> (Vec<Candidate>, ProbeReport, Vec<String>) {
        let order = backend_order(os, adapters, None);
        let mut opened = Vec::new();
        let report = run_probe(os, &order, adapters, 8, |c, codec| {
            let vendor = c.adapter.map_or(0, |i| adapters[i].vendor_id);
            opened.push(format!("{}:{}", c.backend.id(), codec as u8));
            if works.contains(&(c.backend, vendor, codec)) {
                Ok(())
            } else {
                Err(format!("{} cannot open", c.backend.id()))
            }
        });
        (order, report, opened)
    }

    fn summary(report: &ProbeReport) -> Vec<String> {
        report.caps.iter().map(|c| format!("{}:{}", c.codec as u8, c.backend.id())).collect()
    }

    #[test]
    fn probe_takes_the_first_working_hardware_backend_per_codec() {
        use CodecKind::*;
        let (nv, intel) = (VENDOR_NVIDIA, VENDOR_INTEL);
        let adapters = [gpu(intel, true, Some("/dev/dri/renderD128")), gpu(nv, false, None)];
        // An old NVIDIA card (no AV1) next to an Intel iGPU that has it.
        let works = [
            (Backend::Nvenc, nv, H264Hw),
            (Backend::Nvenc, nv, H265),
            (Backend::Vaapi, intel, H264Hw),
            (Backend::Vaapi, intel, Av1),
        ];
        let (_, report, opened) = probe(Os::Linux, &adapters, &works);
        assert_eq!(summary(&report), ["1:nvenc", "3:nvenc", "4:vaapi"]);
        // VAAPI was only opened for the codec NVENC could not do, and
        // software not at all.
        assert_eq!(opened, ["nvenc:1", "nvenc:3", "nvenc:4", "vaapi:4"]);
        assert_eq!(report.skipped.len(), 1);
        assert_eq!((report.skipped[0].backend, report.skipped[0].codec), (Backend::Nvenc, Av1));
        let av1 = report.cap(Av1).unwrap();
        assert_eq!((av1.label.as_str(), av1.render_node.as_deref()), ("VAAPI (Intel)", Some("/dev/dri/renderD128")));
        assert!(av1.hardware && av1.max_pixel_rate == 0);
    }

    #[test]
    fn software_is_reported_only_without_any_hardware() {
        use CodecKind::*;
        let adapters = [gpu(VENDOR_AMD, true, Some("/dev/dri/renderD128"))];
        // Hardware H.264 only: AV1 is NOT filled in by software.
        let (_, report, opened) = probe(Os::Linux, &adapters, &[(Backend::Vaapi, VENDOR_AMD, H264Hw)]);
        assert_eq!(summary(&report), ["1:vaapi"]);
        assert!(!opened.iter().any(|o| o.starts_with("software")));
        // Nothing in hardware: software AV1, with its limits.
        let (_, report, _) = probe(Os::Linux, &adapters, &[(Backend::Software, 0, Av1)]);
        assert_eq!(summary(&report), ["4:software"]);
        let cap = &report.caps[0];
        assert!(!cap.hardware);
        assert_eq!((cap.max_width, cap.max_height, cap.max_fps), (1920, 1080, 60));
        // 8 logical CPUs: 1080p30 and a third.
        assert_eq!(cap.max_pixel_rate, 1920 * 1080 * 30 * 8 / 6);
        assert_eq!(cap.label, "Software (SVT-AV1)");
        assert_eq!(report.skipped.len(), 3);
        // Nothing at all (the Electron build: no libsvtav1): no caps, and
        // the reasons are kept.
        let (_, report, _) = probe(Os::Linux, &adapters, &[]);
        assert!(report.caps.is_empty());
        assert_eq!(report.skipped.len(), 4);
        assert_eq!(report.skipped[3].backend, Backend::Software);
    }

    #[test]
    fn windows_caps_keep_their_ceilings() {
        use CodecKind::*;
        let adapters = [gpu(VENDOR_INTEL, true, None)];
        let works = [(Backend::Qsv, VENDOR_INTEL, H264Hw), (Backend::Qsv, VENDOR_INTEL, H265)];
        let (_, report, _) = probe(Os::Windows, &adapters, &works);
        assert_eq!(summary(&report), ["1:qsv", "3:qsv"]);
        let h264 = report.cap(H264Hw).unwrap();
        assert_eq!((h264.max_width, h264.max_height, h264.max_fps), (2560, 1440, 120));
        assert_eq!(h264.encoder_name, "h264_qsv");
        let hevc = report.cap(H265).unwrap();
        assert_eq!((hevc.max_width, hevc.max_height, hevc.max_fps), (3840, 2160, 120));
        assert_eq!(hardware_ceiling(Os::Linux, H264Hw), (3840, 2160, 60));
    }

    fn plan_ids(plan: &[StartCandidate]) -> Vec<String> {
        plan.iter().map(|p| format!("{}:{}", p.encoder_name, p.codec as u8)).collect()
    }

    #[test]
    fn a_start_falls_through_in_order_and_skips_known_failures() {
        use CodecKind::*;
        let (nv, intel) = (VENDOR_NVIDIA, VENDOR_INTEL);
        let adapters = [gpu(intel, true, Some("/dev/dri/renderD128")), gpu(nv, false, None)];
        let works = [(Backend::Nvenc, nv, H264Hw), (Backend::Vaapi, intel, Av1)];
        let (order, mut report, _) = probe(Os::Linux, &adapters, &works);

        // H.264: NVENC (the cap), then VAAPI (never opened by the probe),
        // then software AV1 unless the codec is pinned.
        let plan = start_plan(&order, &adapters, &report, H264Hw, false);
        assert_eq!(plan_ids(&plan), ["h264_nvenc:1", "h264_vaapi:1", "libsvtav1:4"]);
        let plan = start_plan(&order, &adapters, &report, H264Hw, true);
        assert_eq!(plan_ids(&plan), ["h264_nvenc:1", "h264_vaapi:1"]);
        // AV1: NVENC failed in the probe, so the start does not open it again.
        let plan = start_plan(&order, &adapters, &report, Av1, true);
        assert_eq!(plan_ids(&plan), ["av1_vaapi:4", "libsvtav1:4"]);
        // HEVC: nothing probed OK; NVENC known bad, VAAPI known bad.
        assert_eq!(plan_ids(&start_plan(&order, &adapters, &report, H265, true)), [] as [&str; 0]);

        // NVENC H.264 breaks at a start (session limit, driver update):
        // the cap goes, the next start begins at VAAPI.
        report.mark_failed(order[0], &adapters, H264Hw, "open failed");
        assert!(report.cap(H264Hw).is_none());
        assert_eq!(plan_ids(&start_plan(&order, &adapters, &report, H264Hw, true)), ["h264_vaapi:1"]);
        // … and VAAPI carried the stream: it is the cap now.
        report.mark_working(Os::Linux, order[1], &adapters, H264Hw, 8);
        assert_eq!(summary(&report), ["1:vaapi", "4:vaapi"]);
        // Marking twice changes nothing.
        let before = report.clone();
        report.mark_failed(order[0], &adapters, H264Hw, "again");
        report.mark_working(Os::Linux, order[1], &adapters, H264Hw, 8);
        assert_eq!(report, before);
    }

    #[test]
    fn losing_the_last_hardware_cap_and_running_on_software_reports_software() {
        use CodecKind::*;
        let adapters = [gpu(VENDOR_NVIDIA, true, None)];
        let (order, mut report, _) = probe(Os::Linux, &adapters, &[(Backend::Nvenc, VENDOR_NVIDIA, H264Hw)]);
        assert_eq!(summary(&report), ["1:nvenc"]);
        let software = *order.last().unwrap();
        // Software ran while hardware caps exist: not reported.
        report.mark_working(Os::Linux, software, &adapters, Av1, 8);
        assert_eq!(summary(&report), ["1:nvenc"]);
        report.mark_failed(order[0], &adapters, H264Hw, "gone");
        report.mark_working(Os::Linux, software, &adapters, Av1, 8);
        assert_eq!(summary(&report), ["4:software"]);
        // Hardware came back: software leaves the list.
        report.mark_working(Os::Linux, order[0], &adapters, H265, 8);
        assert_eq!(summary(&report), ["3:nvenc"]);
    }

    #[test]
    fn software_limits_scale_with_the_cpu_and_stop_at_1440p60() {
        let rate = |w: u64, h: u64, f: u64| w * h * f;
        let p1080_30 = rate(1920, 1080, 30);
        #[rustfmt::skip]
        let table: &[(usize, (u32, u32), u64)] = &[
            (1,  (1920, 1080), rate(1280, 720, 30)),
            (2,  (1920, 1080), rate(1280, 720, 30)),
            (4,  (1920, 1080), p1080_30 * 4 / 6),
            (6,  (1920, 1080), p1080_30),
            (8,  (1920, 1080), p1080_30 * 8 / 6),
            (12, (2560, 1440), p1080_30 * 2),
            (16, (2560, 1440), p1080_30 * 16 / 6),
            (20, (2560, 1440), p1080_30 * 20 / 6),
            (24, (2560, 1440), rate(2560, 1440, 60)),
            (64, (2560, 1440), rate(2560, 1440, 60)),
        ];
        for &(cpus, (w, h), pixel_rate) in table {
            let l = software_limits(cpus);
            assert_eq!((l.max_width, l.max_height, l.max_pixel_rate), (w, h, pixel_rate), "{cpus} cpus");
            assert_eq!(l.max_fps, 60);
        }
        // What a UI greys out on a 4-thread laptop: 720p30 is the stream.
        let l = software_limits(4);
        assert!(l.accepts(1280, 720, 30));
        assert!(!l.accepts(1920, 1080, 30));
        assert!(!l.accepts(1280, 720, 60));
        assert!(l.accepts(1920, 1080, 5), "Slides");
        assert_eq!(l.max_fps_at(1280, 720), 45);
        assert_eq!(l.max_fps_at(1920, 1080), 20);
        // Portrait is held against the ceiling on its side.
        assert!(l.accepts(720, 1280, 30));
        // 6 and 8 threads: 1080p30 and 720p60, no 1080p60, no 1440p.
        for cpus in [6, 8] {
            let l = software_limits(cpus);
            assert!(l.accepts(1920, 1080, 30) && l.accepts(1280, 720, 60), "{cpus}");
            assert!(!l.accepts(1920, 1080, 60) && !l.accepts(2560, 1440, 30), "{cpus}");
        }
        // 12 threads: 1080p60 and 1440p30.
        let l = software_limits(12);
        assert!(l.accepts(1920, 1080, 60) && l.accepts(2560, 1440, 30) && !l.accepts(2560, 1440, 60));
        // 20 threads (the machine measured): 1440p30 and 1080p60; 1440p60
        // measured at the edge of its frame interval, so not offered.
        let l = software_limits(20);
        assert!(l.accepts(2560, 1440, 30) && l.accepts(1920, 1080, 60));
        assert!(!l.accepts(2560, 1440, 60));
        assert!(!l.accepts(3840, 2160, 30));
        assert!(!l.accepts(1920, 1080, 120));
        assert_eq!(l.max_fps_at(2560, 1440), 56);
        // 2 threads: 720p30, the floor.
        let l = software_limits(2);
        assert!(l.accepts(1280, 720, 30) && !l.accepts(1920, 1080, 30) && !l.accepts(1280, 720, 60));
        // The refusal says what would be accepted.
        let why = software_refusal(2560, 1440, 60, 8).expect("refused");
        assert!(why.contains("1920x1080") && why.contains("2560x1440@60"), "{why}");
        assert!(software_refusal(1920, 1080, 30, 8).is_none());
    }

    #[test]
    fn software_preset_follows_the_cpu() {
        // The measured machine (20 threads).
        assert_eq!(software_preset(1280, 720, 30, 20, false), 8);
        assert_eq!(software_preset(1920, 1080, 30, 20, false), 10);
        assert_eq!(software_preset(1920, 1080, 60, 20, false), 10);
        assert_eq!(software_preset(2560, 1440, 30, 20, false), 10);
        // An 8-thread laptop at 1080p30; 12 threads at 1080p60; 4 at 720p30.
        assert_eq!(software_preset(1920, 1080, 30, 8, false), 10);
        assert_eq!(software_preset(1920, 1080, 60, 12, false), 12);
        assert_eq!(software_preset(1920, 1080, 30, 6, false), 12);
        assert_eq!(software_preset(1280, 720, 30, 4, false), 10);
        // Slides: quality where the CPU can finish a frame in 200 ms.
        assert_eq!(software_preset(2560, 1440, 5, 8, true), 8);
        assert_eq!(software_preset(2560, 1440, 5, 4, true), 10);
    }

    #[test]
    fn fingerprint_changes_with_gpu_driver_force_and_build() {
        let a = [gpu(VENDOR_NVIDIA, true, None)];
        let base = fingerprint(Os::Linux, &a, None, "b1");
        assert_eq!(base, fingerprint(Os::Linux, &a, None, "b1"));
        let mut newer = a.clone();
        newer[0].driver = "d2".to_string();
        assert_ne!(base, fingerprint(Os::Linux, &newer, None, "b1"));
        assert_ne!(base, fingerprint(Os::Linux, &a, Some(Backend::Software), "b1"));
        assert_ne!(base, fingerprint(Os::Linux, &a, None, "b2"));
        assert_ne!(base, fingerprint(Os::Linux, &[], None, "b1"));
    }
}
