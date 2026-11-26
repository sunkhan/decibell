//! The encoder probe: which backend really encodes which codec on this
//! machine. The decisions (order, what a result means, what a start falls
//! through) are `encoder_backend.rs`; this file lists the GPUs, runs the
//! test encodes and keeps the result.
//!
//! **A test encode is the stream's own path at a small size**: the encoder
//! is opened exactly as a stream opens it (same options, same device, same
//! input conversion) at 640×360 and three frames go through it; it counts
//! only if a packet comes out. `avcodec_find_encoder_by_name` says what
//! FFmpeg was built with, and a bare `avcodec_open2` without the device and
//! frames a backend needs says nothing (VAAPI never opened that way, so the
//! old Linux probe never reported it).
//!
//! **When it runs**: never at startup. `probe()` runs when the client asks
//! for the encoder list (`api::streaming::probe_native_encoders`), and
//! `cached()` on the first stream start that finds no result. Both are
//! blocking and are called on the blocking pool. On the development
//! machine (RTX 4080) the three NVENC test encodes took 0.8 s together in a
//! debug build, most of it starting the first NVENC session.
//!
//! **Cache**: the result is kept for the process and in
//! `<user data>/encoder_probe.json`, keyed by `encoder_backend::fingerprint`
//! (every GPU's ids and driver build, `DECIBELL_ENCODER`, the core's and
//! the app's versions and the FFmpeg build's encoders). A driver update, a
//! different GPU or a new release re-probes; a stream start that sees a
//! backend fail corrects the entry (`note_failed` / `note_working`). A
//! result with no encoder at all is never trusted from the cache: the next
//! question probes again (the GPU may only have been busy).
#![cfg(any(target_os = "linux", target_os = "windows"))]

use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::caps::CodecKind;
pub use super::encoder_backend::{Adapter, Backend, Candidate, EncoderCap, ProbeReport, StartCandidate};
use super::encoder_backend::{self as backend, Os, ProbeResult, STREAM_CODECS};

/// The size and rate of a test encode: above every vendor's minimum
/// (NVENC's AV1 wants 128×128, HEVC on some AMD parts 130 wide), not a
/// multiple of 16 high (so a driver's cropping is exercised), and small
/// enough that three frames cost a few milliseconds.
const PROBE_W: u32 = 640;
const PROBE_H: u32 = 360;
const PROBE_FPS: u32 = 30;
const PROBE_KBPS: u32 = 1000;
const PROBE_FRAMES: usize = 3;

fn os() -> Os {
    if cfg!(target_os = "windows") {
        Os::Windows
    } else {
        Os::Linux
    }
}

pub fn logical_cpus() -> usize {
    std::thread::available_parallelism().map_or(4, |n| n.get())
}

// ─── The machine's GPUs ────────────────────────────────────────────────

/// Linux: every DRM render node (`/sys/class/drm/renderD*`), with the PCI
/// ids of its device, its kernel driver, and whether it is the boot VGA
/// device (the one the firmware, and so normally the compositor, uses).
#[cfg(target_os = "linux")]
pub fn adapters() -> Vec<Adapter> {
    adapters_from_sysfs(std::path::Path::new("/sys/class/drm"), std::path::Path::new("/dev/dri"))
}

#[cfg(target_os = "linux")]
fn adapters_from_sysfs(sys: &std::path::Path, dev: &std::path::Path) -> Vec<Adapter> {
    let read = |p: std::path::PathBuf| std::fs::read_to_string(p).ok().map(|s| s.trim().to_string());
    let hex = |s: Option<String>| s.and_then(|s| u32::from_str_radix(s.trim_start_matches("0x"), 16).ok()).unwrap_or(0);
    let mut nodes: Vec<String> = match std::fs::read_dir(sys) {
        Ok(dir) => dir
            .filter_map(|e| e.ok())
            .filter_map(|e| e.file_name().into_string().ok())
            .filter(|n| n.starts_with("renderD"))
            .collect(),
        Err(_) => Vec::new(),
    };
    // renderD128, renderD129, … in number order.
    nodes.sort_by_key(|n| n.trim_start_matches("renderD").parse::<u32>().unwrap_or(u32::MAX));
    let mut out = Vec::new();
    for node in nodes {
        let device = sys.join(&node).join("device");
        let vendor_id = hex(read(device.join("vendor")));
        if vendor_id == 0 {
            // Not a PCI device (a platform / virtual GPU): nothing here encodes on it.
            continue;
        }
        let kernel_driver = std::fs::read_link(device.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n.to_string_lossy().into_owned()))
            .unwrap_or_default();
        out.push(Adapter {
            vendor_id,
            device_id: hex(read(device.join("device"))),
            driver: linux_driver_build(vendor_id, &kernel_driver),
            name: kernel_driver,
            primary: read(device.join("boot_vga")).as_deref() == Some("1"),
            render_node: Some(dev.join(&node).to_string_lossy().into_owned()),
        });
    }
    // One GPU is the primary whatever boot_vga says (it is absent on some
    // ARM / virtual setups).
    if out.len() == 1 {
        out[0].primary = true;
    }
    out
}

/// What identifies the driver build on Linux, for the cache key. NVIDIA:
/// the kernel module's version (the user-space libraries ship with it).
/// AMD / Intel: the kernel release plus the size and date of the VA-API
/// driver file, which is what a Mesa or intel-media-driver update replaces.
#[cfg(target_os = "linux")]
fn linux_driver_build(vendor_id: u32, kernel_driver: &str) -> String {
    if vendor_id == backend::VENDOR_NVIDIA {
        if let Ok(v) = std::fs::read_to_string("/sys/module/nvidia/version") {
            return format!("nvidia-{}", v.trim());
        }
    }
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").unwrap_or_default();
    let mut s = format!("{kernel_driver}-{}", kernel.trim());
    let files: &[&str] = match vendor_id {
        backend::VENDOR_AMD => &["radeonsi_drv_video.so"],
        backend::VENDOR_INTEL => &["iHD_drv_video.so", "i965_drv_video.so"],
        _ => &[],
    };
    let mut dirs: Vec<String> = std::env::var("LIBVA_DRIVERS_PATH")
        .map(|p| p.split(':').map(str::to_string).collect())
        .unwrap_or_default();
    dirs.extend(
        ["/usr/lib/dri", "/usr/lib64/dri", "/usr/lib/x86_64-linux-gnu/dri", "/usr/lib/aarch64-linux-gnu/dri"]
            .map(str::to_string),
    );
    for dir in &dirs {
        for file in files {
            if let Ok(meta) = std::fs::metadata(std::path::Path::new(dir).join(file)) {
                let mtime = meta
                    .modified()
                    .ok()
                    .and_then(|t| t.duration_since(std::time::UNIX_EPOCH).ok())
                    .map_or(0, |d| d.as_secs());
                s.push_str(&format!("-{file}:{}:{mtime}", meta.len()));
            }
        }
    }
    s
}

/// Windows: every hardware DXGI adapter. The first one is the primary (it
/// has the primary desktop output). The driver build is the user-mode
/// driver version DXGI reports.
#[cfg(target_os = "windows")]
pub fn adapters() -> Vec<Adapter> {
    use windows::core::Interface;
    use windows::Win32::Graphics::Dxgi::{
        CreateDXGIFactory1, IDXGIDevice, IDXGIFactory1, DXGI_ADAPTER_FLAG_SOFTWARE,
    };
    let mut out = Vec::new();
    unsafe {
        let factory: IDXGIFactory1 = match CreateDXGIFactory1() {
            Ok(f) => f,
            Err(_) => return out,
        };
        let mut i = 0u32;
        // EnumAdapters1 returns DXGI_ERROR_NOT_FOUND past the last adapter.
        while let Ok(adapter) = factory.EnumAdapters1(i) {
            i += 1;
            let Ok(desc) = adapter.GetDesc1() else {
                continue;
            };
            // desc.Flags is u32 in windows-rs 0.61; the flag's inner value
            // is i32 — cast before AND.
            if desc.Flags & (DXGI_ADAPTER_FLAG_SOFTWARE.0 as u32) != 0 {
                continue;
            }
            let len = desc.Description.iter().position(|&c| c == 0).unwrap_or(desc.Description.len());
            let driver = match adapter.CheckInterfaceSupport(&IDXGIDevice::IID) {
                Ok(v) => format!("{v:#x}"),
                Err(_) => String::new(),
            };
            out.push(Adapter {
                vendor_id: desc.VendorId,
                device_id: desc.DeviceId,
                name: String::from_utf16_lossy(&desc.Description[..len]),
                driver,
                primary: out.is_empty(),
                render_node: None,
            });
        }
    }
    out
}

/// The core's and the app's versions (a release that fixes a backend must
/// not keep believing an older one's failure) and which of the encoders
/// this FFmpeg build has (the Electron addon's has no libsvtav1, the GPUI
/// client's does).
pub fn build_id() -> String {
    let _ = ffmpeg_next::init();
    let mut bits = String::new();
    for b in [Backend::Nvenc, Backend::Amf, Backend::Qsv, Backend::Vaapi, Backend::Software] {
        for codec in STREAM_CODECS {
            if let Some(name) = b.encoder_name(codec) {
                bits.push(if ffmpeg_next::encoder::find_by_name(name).is_some() { '1' } else { '0' });
            }
        }
    }
    let app = crate::state::boot_opt().map_or("", |b| b.app_version.as_str());
    format!("{}-{app}-{bits}", env!("CARGO_PKG_VERSION"))
}

// ─── Test encodes ──────────────────────────────────────────────────────

/// Where a candidate's encoder opens on Linux.
#[cfg(target_os = "linux")]
pub fn target_for(candidate: Candidate, adapters: &[Adapter]) -> super::encoder_linux::EncoderTarget {
    let render_node = if candidate.backend == Backend::Vaapi {
        // DECIBELL_VAAPI_DEVICE=/dev/dri/renderD129 picks the node by hand
        // (verification on a machine whose sysfs is unusual).
        std::env::var("DECIBELL_VAAPI_DEVICE")
            .ok()
            .filter(|s| !s.is_empty())
            .or_else(|| candidate.adapter.and_then(|i| adapters.get(i)).and_then(|a| a.render_node.clone()))
    } else {
        None
    };
    super::encoder_linux::EncoderTarget { backend: candidate.backend, render_node }
}

/// A small picture with a gradient and hard edges (so a rate-controlled
/// encoder has something to do), `stride` bytes a row, BGRA.
fn probe_picture(w: u32, h: u32, stride: usize, tick: u32) -> Vec<u8> {
    let mut px = vec![0u8; stride * h as usize];
    for y in 0..h as usize {
        for x in 0..w as usize {
            let block = ((x / 16 + y / 16 + tick as usize) % 2) as u8 * 96;
            let p = &mut px[y * stride + x * 4..][..4];
            p[0] = (x as u8).wrapping_add(block);
            p[1] = (y as u8).wrapping_add(block);
            p[2] = block.wrapping_add(tick as u8 * 8);
            p[3] = 255;
        }
    }
    px
}

#[cfg(target_os = "linux")]
fn test_encode(candidate: Candidate, adapters: &[Adapter], codec: CodecKind) -> ProbeResult {
    use super::encoder_linux::{EncoderConfig, H264Encoder};
    let target = target_for(candidate, adapters);
    let config = EncoderConfig {
        width: PROBE_W,
        height: PROBE_H,
        fps: PROBE_FPS,
        bitrate_kbps: PROBE_KBPS,
        keyframe_interval_secs: 2,
        detail: false,
        source_size: (PROBE_W, PROBE_H),
    };
    let mut encoder = H264Encoder::open(&target, codec, &config)?;
    let stride = PROBE_W as usize * 4;
    let mut packets = 0usize;
    let mut first_is_key = None;
    for tick in 0..PROBE_FRAMES as u32 {
        let picture = probe_picture(PROBE_W, PROBE_H, stride, tick);
        if let Some(frame) = encoder.encode_bgra_frame(&picture, PROBE_W, PROBE_H, stride, true)? {
            first_is_key.get_or_insert(frame.is_keyframe);
            packets += 1;
        }
    }
    for frame in encoder.flush() {
        first_is_key.get_or_insert(frame.is_keyframe);
        packets += 1;
    }
    check_probe_output(packets, first_is_key)
}

fn check_probe_output(packets: usize, first_is_key: Option<bool>) -> ProbeResult {
    match (packets, first_is_key) {
        (0, _) => Err(format!("opened, but {PROBE_FRAMES} frames produced no packet")),
        (_, Some(false)) => Err("opened, but the first packet is not a keyframe".to_string()),
        _ => Ok(()),
    }
}

#[cfg(target_os = "windows")]
fn test_encode(candidate: Candidate, adapters: &[Adapter], codec: CodecKind) -> ProbeResult {
    use super::encoder::StreamEncoder;
    use super::gpu_pipeline::GpuDevice;
    let name = candidate
        .backend
        .encoder_name(codec)
        .ok_or_else(|| format!("{} has no encoder for {codec:?}", candidate.backend.id()))?;
    let vendor_id = candidate.adapter.and_then(|i| adapters.get(i)).map_or(0, |a| a.vendor_id);
    // The device the stream itself would use: the backend's vendor's
    // adapter (the default adapter for software).
    let gpu = GpuDevice::create_for_vendor(vendor_id)?;
    let mut encoder = StreamEncoder::open(&gpu, name, PROBE_W, PROBE_H, PROBE_FPS, PROBE_KBPS, false)?;
    let texture = probe_texture(&gpu)?;
    let mut packets = 0usize;
    let mut first_is_key = None;
    let mut on_packet = |_data: &[u8], key: bool, _pts: i64| {
        first_is_key.get_or_insert(key);
        packets += 1;
    };
    for tick in 0..PROBE_FRAMES as i64 {
        // A dropped frame is not a failure by itself (software reads the
        // picture back a frame later); no packet at the end is.
        encoder.send_bgra(&texture, tick, &mut on_packet)?;
        encoder.for_each_packet(&mut on_packet)?;
    }
    encoder.finish(&mut on_packet)?;
    check_probe_output(packets, first_is_key)
}

/// A BGRA texture of the probe size holding `probe_picture`, bound the way
/// a captured frame is (shader resource + render target).
#[cfg(target_os = "windows")]
fn probe_texture(
    gpu: &super::gpu_pipeline::GpuDevice,
) -> Result<windows::Win32::Graphics::Direct3D11::ID3D11Texture2D, String> {
    use windows::Win32::Graphics::Direct3D11::{
        ID3D11Texture2D, D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_SUBRESOURCE_DATA,
        D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};
    let stride = PROBE_W as usize * 4;
    let picture = probe_picture(PROBE_W, PROBE_H, stride, 0);
    let desc = D3D11_TEXTURE2D_DESC {
        Width: PROBE_W,
        Height: PROBE_H,
        MipLevels: 1,
        ArraySize: 1,
        Format: DXGI_FORMAT_B8G8R8A8_UNORM,
        SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
        Usage: D3D11_USAGE_DEFAULT,
        BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
        CPUAccessFlags: 0,
        MiscFlags: 0,
    };
    let init = D3D11_SUBRESOURCE_DATA {
        pSysMem: picture.as_ptr() as *const std::ffi::c_void,
        SysMemPitch: stride as u32,
        SysMemSlicePitch: 0,
    };
    let mut tex: Option<ID3D11Texture2D> = None;
    // `picture` outlives the call: CreateTexture2D copies the initial data.
    unsafe { gpu.device.CreateTexture2D(&desc, Some(&init), Some(&mut tex)) }
        .map_err(|e| format!("CreateTexture2D (probe picture): {e:?}"))?;
    tex.ok_or_else(|| "CreateTexture2D (probe picture) returned None".to_string())
}

// ─── The result, kept ──────────────────────────────────────────────────

/// What is known about this machine's encoders.
#[derive(Clone, Debug)]
pub struct Probed {
    pub adapters: Vec<Adapter>,
    pub order: Vec<Candidate>,
    pub report: ProbeReport,
    fingerprint: String,
}

#[derive(Serialize, Deserialize)]
struct CacheFile {
    fingerprint: String,
    report: ProbeReport,
}

static STATE: Mutex<Option<Probed>> = Mutex::new(None);
/// Serialises probes: they open real hardware encoder sessions, and two at
/// once (the client's request and a stream start) would double them.
static PROBE_LOCK: Mutex<()> = Mutex::new(());

fn cache_path() -> Option<std::path::PathBuf> {
    crate::state::boot_opt().map(|b| b.user_data_dir.join("encoder_probe.json"))
}

fn load_cache_file(fingerprint: &str) -> Option<ProbeReport> {
    let bytes = std::fs::read(cache_path()?).ok()?;
    let file: CacheFile = serde_json::from_slice(&bytes).ok()?;
    (file.fingerprint == fingerprint).then_some(file.report)
}

fn store_cache_file(probed: &Probed) {
    let Some(path) = cache_path() else {
        return;
    };
    let file = CacheFile { fingerprint: probed.fingerprint.clone(), report: probed.report.clone() };
    let Ok(bytes) = serde_json::to_vec_pretty(&file) else {
        return;
    };
    let tmp = path.with_extension("json.tmp");
    if std::fs::write(&tmp, bytes).and_then(|()| std::fs::rename(&tmp, &path)).is_err() {
        let _ = std::fs::remove_file(&tmp);
        log::debug!("[encoder-probe] could not write {}", path.display());
    }
}

fn machine() -> (Vec<Adapter>, Vec<Candidate>, String) {
    let adapters = adapters();
    let forced = backend::forced_backend();
    let order = backend::backend_order(os(), &adapters, forced);
    let fingerprint = backend::fingerprint(os(), &adapters, forced, &build_id());
    (adapters, order, fingerprint)
}

fn describe(candidate: Candidate, adapters: &[Adapter]) -> String {
    let vendor_id = candidate.adapter.and_then(|i| adapters.get(i)).map_or(0, |a| a.vendor_id);
    backend::backend_label(candidate.backend, vendor_id)
}

fn log_report(probed: &Probed, source: &str) {
    for a in &probed.adapters {
        log::info!(
            "[encoder-probe] GPU {:04x}:{:04x} {}{}{}",
            a.vendor_id,
            a.device_id,
            a.name,
            if a.primary { " (primary)" } else { "" },
            a.render_node.as_deref().map(|n| format!(" {n}")).unwrap_or_default()
        );
    }
    if let Some(forced) = backend::forced_backend() {
        log::info!("[encoder-probe] {}={}: only this backend is used", backend::FORCE_ENV, forced.id());
    }
    for s in &probed.report.skipped {
        log::info!(
            "[encoder-probe] skipped {} for {:?}: {}",
            backend::backend_label(s.backend, s.vendor_id),
            s.codec,
            s.reason
        );
    }
    if probed.report.caps.is_empty() {
        log::warn!("[encoder-probe] no usable encoder on this machine ({source})");
    }
    for c in &probed.report.caps {
        log::info!(
            "[encoder-probe] {:?}: {} via {} up to {}x{}@{} ({source})",
            c.codec,
            c.label,
            c.encoder_name,
            c.max_width,
            c.max_height,
            c.max_fps
        );
    }
}

fn run_locked(adapters: Vec<Adapter>, order: Vec<Candidate>, fingerprint: String) -> Probed {
    let started = std::time::Instant::now();
    let report = backend::run_probe(os(), &order, &adapters, logical_cpus(), |candidate, codec| {
        let t = std::time::Instant::now();
        // A panic inside a driver's bindings must not take the caller down.
        let result = std::panic::catch_unwind(|| test_encode(candidate, &adapters, codec))
            .unwrap_or_else(|_| Err("the test encode panicked".to_string()));
        log::debug!(
            "[encoder-probe] {} {:?}: {} in {:?}",
            describe(candidate, &adapters),
            codec,
            if result.is_ok() { "ok" } else { "failed" },
            t.elapsed()
        );
        result
    });
    let probed = Probed { adapters, order, report, fingerprint };
    log_report(&probed, &format!("probed in {:?}", started.elapsed()));
    store_cache_file(&probed);
    *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(probed.clone());
    probed
}

/// Probe now, whatever is cached (the client asked for a fresh list).
/// Blocking: hundreds of milliseconds of encoder opens.
pub fn probe() -> Probed {
    let _g = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (adapters, order, fingerprint) = machine();
    run_locked(adapters, order, fingerprint)
}

/// The cached result for this machine as it is now: the process's, else
/// the file's, else a probe. Blocking only on that last case.
pub fn cached() -> Probed {
    let (adapters, order, fingerprint) = machine();
    let from_state = |fingerprint: &str| {
        STATE
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .filter(|p| p.fingerprint == fingerprint && !p.report.caps.is_empty())
            .cloned()
    };
    if let Some(p) = from_state(&fingerprint) {
        return p;
    }
    // A probe may be running: wait for it, then look again.
    let _g = PROBE_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(p) = from_state(&fingerprint) {
        return p;
    }
    if let Some(report) = load_cache_file(&fingerprint).filter(|r| !r.caps.is_empty()) {
        let probed = Probed { adapters, order, report, fingerprint };
        log_report(&probed, "cached");
        *STATE.lock().unwrap_or_else(|e| e.into_inner()) = Some(probed.clone());
        return probed;
    }
    run_locked(adapters, order, fingerprint)
}

/// The codec an "auto" request (0) means: the first one with a working
/// backend (H.264 before HEVC before AV1), H.264 when nothing is known.
pub fn auto_codec(report: &ProbeReport) -> CodecKind {
    report.caps.first().map_or(CodecKind::H264Hw, |c| c.codec)
}

/// What a stream start opens, in order (`encoder_backend::start_plan`).
pub fn plan(probed: &Probed, codec: CodecKind, codec_locked: bool) -> Vec<StartCandidate> {
    backend::start_plan(&probed.order, &probed.adapters, &probed.report, codec, codec_locked)
}

fn update(apply: impl FnOnce(&mut Probed)) {
    let mut state = STATE.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(probed) = state.as_mut() {
        let before = probed.report.clone();
        apply(probed);
        if probed.report != before {
            store_cache_file(probed);
        }
    }
}

/// A stream start saw this candidate fail: the cache stops promising it.
pub fn note_failed(candidate: Candidate, codec: CodecKind, reason: &str) {
    update(|p| {
        log::warn!("[encoder] {} failed for {codec:?}: {reason}", describe(candidate, &p.adapters));
        let adapters = p.adapters.clone();
        p.report.mark_failed(candidate, &adapters, codec, reason);
    });
}

/// A stream is running on this candidate.
pub fn note_working(candidate: Candidate, codec: CodecKind) {
    update(|p| {
        let adapters = p.adapters.clone();
        p.report.mark_working(os(), candidate, &adapters, codec, logical_cpus());
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_output_must_start_with_a_keyframe() {
        assert!(check_probe_output(0, None).is_err());
        assert!(check_probe_output(2, Some(false)).is_err());
        assert!(check_probe_output(1, Some(true)).is_ok());
    }

    #[test]
    fn probe_pictures_differ_between_ticks() {
        let a = probe_picture(64, 32, 64 * 4 + 16, 0);
        let b = probe_picture(64, 32, 64 * 4 + 16, 1);
        assert_eq!(a.len(), (64 * 4 + 16) * 32);
        assert_ne!(a, b);
    }

    /// The sysfs layout of a hybrid laptop: an Intel iGPU that is the boot
    /// VGA device on renderD129 and an NVIDIA GPU on renderD128.
    #[cfg(target_os = "linux")]
    #[test]
    fn adapters_are_read_from_sysfs() {
        let root = std::env::temp_dir().join(format!("decibell-drm-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        let node = |name: &str, vendor: &str, device: &str, boot_vga: Option<&str>| {
            let d = root.join("sys").join(name).join("device");
            std::fs::create_dir_all(&d).unwrap();
            std::fs::write(d.join("vendor"), format!("{vendor}\n")).unwrap();
            std::fs::write(d.join("device"), format!("{device}\n")).unwrap();
            if let Some(v) = boot_vga {
                std::fs::write(d.join("boot_vga"), format!("{v}\n")).unwrap();
            }
        };
        node("renderD129", "0x8086", "0xa7a0", Some("1"));
        node("renderD128", "0x10de", "0x2704", Some("0"));
        // A card node and a connector are not render nodes.
        std::fs::create_dir_all(root.join("sys").join("card1")).unwrap();
        std::fs::create_dir_all(root.join("sys").join("card1-DP-1")).unwrap();
        // A render node that is not a PCI device is left out.
        std::fs::create_dir_all(root.join("sys").join("renderD130").join("device")).unwrap();

        let found = adapters_from_sysfs(&root.join("sys"), std::path::Path::new("/dev/dri"));
        assert_eq!(found.len(), 2);
        assert_eq!((found[0].vendor_id, found[0].device_id, found[0].primary), (0x10de, 0x2704, false));
        assert_eq!(found[0].render_node.as_deref(), Some("/dev/dri/renderD128"));
        assert_eq!((found[1].vendor_id, found[1].primary), (0x8086, true));
        assert_eq!(found[1].render_node.as_deref(), Some("/dev/dri/renderD129"));
        // NVENC, then the iGPU's VAAPI on its own node, then software.
        let order = backend::backend_order(Os::Linux, &found, None);
        assert_eq!(order.len(), 3);
        let target = target_for(order[1], &found);
        if std::env::var_os("DECIBELL_VAAPI_DEVICE").is_none() {
            assert_eq!(target.render_node.as_deref(), Some("/dev/dri/renderD129"));
        }
        assert_eq!(target_for(order[0], &found).render_node, None);
        let _ = std::fs::remove_dir_all(&root);

        // No DRM at all: nothing listed, and the order then tries every backend.
        assert!(adapters_from_sysfs(&root.join("missing"), std::path::Path::new("/dev/dri")).is_empty());
    }

    /// The real probe on whatever this machine has: every cap it reports
    /// names a backend of the machine's order with that backend's encoder,
    /// a second run agrees with the first, and it prints what it found and
    /// how long it took. (No GPU and no SVT-AV1: an empty list, with a
    /// reason per backend.)
    #[cfg(target_os = "linux")]
    #[test]
    fn the_real_probe_is_consistent() {
        let _one_at_a_time = crate::media::encoder_checks::encoder_lock();
        let started = std::time::Instant::now();
        let first = probe();
        let took = started.elapsed();
        for cap in &first.report.caps {
            assert!(first.order.iter().any(|c| c.backend == cap.backend), "{cap:?}");
            assert_eq!(cap.backend.encoder_name(cap.codec), Some(cap.encoder_name.as_str()));
            assert_eq!(cap.hardware, cap.backend.is_hardware());
            println!("{:?}: {} via {}", cap.codec, cap.label, cap.encoder_name);
        }
        for s in &first.report.skipped {
            println!("skipped {} {:?}: {}", s.backend.id(), s.codec, s.reason);
        }
        println!("probed {} backend(s) in {took:?}", first.order.len());
        // Software is listed only when nothing in hardware is.
        if first.report.caps.iter().any(|c| c.hardware) {
            assert!(first.report.caps.iter().all(|c| c.hardware));
        }
        // The process cache answers the next question without probing.
        let started = std::time::Instant::now();
        let again = cached();
        assert_eq!(again.report, first.report);
        // (An empty result is probed again on purpose.)
        if !first.report.caps.is_empty() {
            assert!(started.elapsed() < std::time::Duration::from_millis(100), "cached() probed again");
        }
        // A plan for the first codec starts with the cap's backend.
        if let Some(cap) = first.report.caps.first() {
            let plan = plan(&first, cap.codec, true);
            assert_eq!(plan[0].candidate.backend, cap.backend);
            assert_eq!(auto_codec(&first.report), cap.codec);
        }
    }

    #[test]
    fn the_build_id_names_the_version_and_the_encoders() {
        let id = build_id();
        assert!(id.starts_with(concat!(env!("CARGO_PKG_VERSION"), "-")));
        // 4 hardware backends × 3 codecs + software AV1.
        assert_eq!(id.rsplit('-').next().unwrap().len(), 13);
    }
}
