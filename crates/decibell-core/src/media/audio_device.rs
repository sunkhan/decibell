use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use cpal::{FromSample, Sample, SizedSample};
use smallvec::SmallVec;
use std::time::Instant;
use std::sync::Arc;
use arc_swap::ArcSwap;
use ringbuf::{HeapProd, HeapCons, traits::{Consumer, Observer, Producer}};
use rubato::{SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction};

use super::codec::SAMPLE_RATE;
use super::peer::PeerOutput;
use super::pipeline::VoiceEvent;

/// Shared peer list used by the output callback. Swapped atomically by the
/// main loop whenever peers join/leave so the callback never blocks.
pub type PeerList = Arc<ArcSwap<Vec<PeerOutput>>>;


// ── Sinc resampler helper ────────────────────────────────────────────────────

pub fn make_sinc_resampler(from_rate: u32, to_rate: u32, chunk_size: usize, channels: usize) -> SincFixedOut<f64> {
    let params = SincInterpolationParameters {
        sinc_len: 24,
        f_cutoff: 0.925,
        interpolation: SincInterpolationType::Cubic,
        oversampling_factor: 32,
        window: WindowFunction::Blackman2,
    };
    SincFixedOut::<f64>::new(
        to_rate as f64 / from_rate as f64,
        1.1, // max relative input size variation
        params,
        chunk_size,
        channels,
    ).expect("failed to create sinc resampler")
}

// ── Windows: default communications device ───────────────────────────────────
//
// CPAL's default_input_device / default_output_device use the eConsole role,
// which is the "Default Device" in Windows Sound settings. For a voice chat app
// we want the "Default Communications Device" (eCommunications) instead, since
// many users set their headset as comms device and speakers as default.

#[cfg(target_os = "windows")]
fn default_comms_device_name(input: bool) -> Option<String> {
    use windows::Win32::Media::Audio::*;
    use windows::Win32::System::Com::{*, STGM};
    use windows::Win32::Devices::FunctionDiscovery::PKEY_Device_FriendlyName;
    unsafe {
        // COM must be initialized on this thread
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);

        let enumerator: IMMDeviceEnumerator =
            CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL).ok()?;

        let flow = if input { eCapture } else { eRender };
        let device = enumerator.GetDefaultAudioEndpoint(flow, eCommunications).ok()?;

        // STGM_READ = 0
        let store = device.OpenPropertyStore(STGM(0)).ok()?;
        let prop = store.GetValue(&PKEY_Device_FriendlyName).ok()?;

        // The friendly name is a VT_LPWSTR PROPVARIANT
        let name = prop.to_string();
        if name.is_empty() { None } else { Some(name) }
    }
}

/// Get the default device for voice — on Windows, prefer the communications
/// device; on Linux, just use CPAL's default.
///
/// Last-resort fallback: if both the platform-specific path AND CPAL's
/// `default_*_device()` come up empty, pick the first device from the
/// host's enumeration. We've seen this on Windows with SteelSeries Sonar
/// (and similar virtual-driver setups) where Windows' default endpoint
/// lookup fails even though the host has perfectly usable devices. Same
/// shape fires on any OS where the user's audio state is unusual but
/// real devices exist. Without this final fallback the voice pipeline
/// gives up before the user has a chance to pick a device from
/// Settings → Audio.
pub fn get_default_device(host: &cpal::Host, input: bool) -> Option<cpal::Device> {
    #[cfg(target_os = "windows")]
    {
        // Try to find the communications device by name in CPAL's list
        if let Some(comms_name) = default_comms_device_name(input) {
            let devices = if input {
                host.input_devices()
            } else {
                host.output_devices()
            };
            if let Ok(mut devs) = devices {
                if let Some(d) = devs.find(|d| d.name().map(|n| n == comms_name).unwrap_or(false)) {
                    log::info!("[pipeline] Using Windows communications device: {}", comms_name);
                    return Some(d);
                }
            }
            log::info!("[pipeline] Communications device '{}' not found in CPAL, falling back to default", comms_name);
        }
    }

    let cpal_default = if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    };
    if let Some(d) = cpal_default {
        return Some(d);
    }

    // Final fallback: any device the host knows about.
    let mut devices = if input {
        host.input_devices().ok()?
    } else {
        host.output_devices().ok()?
    };
    let picked = devices.next()?;
    if let Ok(name) = picked.name() {
        log::info!(
            "[pipeline] No default {} device reported by host; falling back to first available: {}",
            if input { "input" } else { "output" },
            name,
        );
    }
    Some(picked)
}

// ── Linux: route a stored device selection through PulseAudio / PipeWire ──────
//
// On Linux we enumerate devices with `pactl` (commands/settings.rs), so the
// stored device name is a Pulse/PipeWire *node* name (e.g.
// "alsa_input.pci-0000_00_1f.3.analog-stereo"). CPAL's ALSA host cannot open a
// device by that name, so the old `find(|d| d.name() == name)` never matched and
// device selection silently fell back to the system default.
//
// Fix: set PULSE_SOURCE / PULSE_SINK (honored by PulseAudio and pipewire-pulse)
// to the chosen node, then open CPAL's "pulse" PCM, which routes through that
// server. Falls back to "default" and then the host default if the pulse PCM
// isn't present — so the worst case is exactly the pre-fix behaviour (no
// regression), while the common pipewire-pulse setup now honours the selection.
#[cfg(target_os = "linux")]
fn pulse_routed_device(host: &cpal::Host, node_name: &str, input: bool) -> Option<cpal::Device> {
    // Edition 2021: set_var is safe. Streams are (re)built sequentially on the
    // pipeline thread, so the input path has no env race; two output sinks
    // (voice + separate stream) can still contend — documented as a known edge.
    if input {
        std::env::set_var("PULSE_SOURCE", node_name);
    } else {
        std::env::set_var("PULSE_SINK", node_name);
    }
    let devices = if input { host.input_devices() } else { host.output_devices() };
    if let Ok(devs) = devices {
        let mut pulse = None;
        let mut dflt = None;
        for d in devs {
            match d.name().ok().as_deref() {
                Some("pulse") => { pulse = Some(d); break; }
                Some("default") if dflt.is_none() => { dflt = Some(d); }
                _ => {}
            }
        }
        if let Some(d) = pulse {
            log::info!("[pipeline] Routing {} to Pulse/PipeWire node '{}' via pulse PCM",
                if input { "input" } else { "output" }, node_name);
            return Some(d);
        }
        if let Some(d) = dflt {
            log::info!("[pipeline] Routing {} to Pulse/PipeWire node '{}' via default PCM",
                if input { "input" } else { "output" }, node_name);
            return Some(d);
        }
    }
    get_default_device(host, input)
}

/// Resolve a stored input-device name to a CPAL capture device. On Linux this
/// routes the pactl node name through Pulse/PipeWire (see `pulse_routed_device`);
/// elsewhere it matches CPAL's own device names directly.
fn resolve_capture_device(host: &cpal::Host, device_name: Option<&str>) -> Option<cpal::Device> {
    let Some(name) = device_name else {
        #[cfg(target_os = "linux")]
        std::env::remove_var("PULSE_SOURCE");
        return get_default_device(host, true);
    };
    #[cfg(target_os = "linux")]
    { pulse_routed_device(host, name, true) }
    #[cfg(not(target_os = "linux"))]
    {
        match host.input_devices().ok()?.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
            Some(d) => Some(d),
            None => {
                log::info!("[pipeline] Input device '{}' not found, falling back to default", name);
                get_default_device(host, true)
            }
        }
    }
}

/// Resolve a stored output-device name to a CPAL playback device. On Linux this
/// routes the pactl node name through Pulse/PipeWire; elsewhere it matches
/// CPAL's own device names directly.
fn resolve_playback_device(host: &cpal::Host, device_name: Option<&str>) -> Option<cpal::Device> {
    let Some(name) = device_name else {
        #[cfg(target_os = "linux")]
        std::env::remove_var("PULSE_SINK");
        return get_default_device(host, false);
    };
    #[cfg(target_os = "linux")]
    { pulse_routed_device(host, name, false) }
    #[cfg(not(target_os = "linux"))]
    {
        match host.output_devices().ok()?.find(|d| d.name().map(|n| n == name).unwrap_or(false)) {
            Some(d) => Some(d),
            None => {
                log::info!("[pipeline] Output device '{}' not found, falling back to default", name);
                get_default_device(host, false)
            }
        }
    }
}

// ── Device-format handling ───────────────────────────────────────────────────
//
// CPAL streams are typed by sample format (f32 / i16 / u16 / …). We used to
// force an f32 stream and ignore the device's real format, which silently
// failed to open on devices whose WASAPI shared-mode format isn't f32 — the
// SteelSeries Sonar / Voicemeeter / VB-Cable class of virtual drivers. Now we
// read the device's reported format and build a stream in THAT format,
// converting at the callback boundary. `dasp`/CPAL's FromSample handles the
// per-sample conversion both ways.

/// Soft-knee output limiter. Exactly unity below the knee (transparent for
/// normal speech — a single speaker is untouched) and smoothly compresses
/// everything above it toward a ±1 ceiling, so it NEVER clips. Replaces the
/// hard i16 clamp that produced harsh distortion once more than one person
/// spoke at once (or a per-user volume boost pushed a single voice past full
/// scale). C1-continuous at the knee (unity slope), so there's no audible kink.
#[inline]
pub(crate) fn soft_clip(x: f32) -> f32 {
    const KNEE: f32 = 0.75; // ≈ -2.5 dBFS; speech peaks sit well below this
    let a = x.abs();
    if a <= KNEE {
        x
    } else {
        let over = a - KNEE;
        let headroom = 1.0 - KNEE;
        x.signum() * (KNEE + headroom * (over / (over + headroom)))
    }
}

/// What the output callback actually pulls per call, reported back to the
/// pipeline. The pipeline keeps each playback ring at least one pull (plus a
/// loop-tick margin) deep, so a callback never finds the ring short and
/// splices zeros into speech. `recent_max` is read-and-reset by the pipeline
/// every couple of seconds; the very first callback is skipped because many
/// backends prime their whole device buffer in one oversized pull.
pub struct OutputPullStats {
    recent_max_frames: std::sync::atomic::AtomicUsize,
    callbacks: std::sync::atomic::AtomicU64,
    /// Callbacks where some peer ring had audio but less than the pull —
    /// i.e. zeros got spliced into that peer's voice. Should stay 0.
    short_pulls: std::sync::atomic::AtomicU64,
    /// Longest gap between consecutive callbacks (µs) since last read. A gap
    /// of several periods means the device stalled/xrun'd underneath us.
    max_gap_us: std::sync::atomic::AtomicU64,
    last_cb_ns: std::sync::atomic::AtomicU64,
    base: Instant,
    /// Optional tap of the mono mix handed to the device (debug dump).
    tap: Option<Arc<std::sync::Mutex<HeapProd<i16>>>>,
}

impl OutputPullStats {
    pub fn new(tap: Option<Arc<std::sync::Mutex<HeapProd<i16>>>>) -> Self {
        Self {
            recent_max_frames: std::sync::atomic::AtomicUsize::new(0),
            callbacks: std::sync::atomic::AtomicU64::new(0),
            short_pulls: std::sync::atomic::AtomicU64::new(0),
            max_gap_us: std::sync::atomic::AtomicU64::new(0),
            last_cb_ns: std::sync::atomic::AtomicU64::new(0),
            base: Instant::now(),
            tap,
        }
    }
    #[inline]
    fn record(&self, frames: usize) {
        use std::sync::atomic::Ordering::Relaxed;
        let now_ns = self.base.elapsed().as_nanos() as u64;
        let prev = self.last_cb_ns.swap(now_ns, Relaxed);
        if self.callbacks.fetch_add(1, Relaxed) == 0 {
            return; // initial device-buffer fill, not the steady-state pull
        }
        self.recent_max_frames.fetch_max(frames, Relaxed);
        if prev > 0 {
            self.max_gap_us.fetch_max((now_ns - prev) / 1000, Relaxed);
        }
    }
    #[inline]
    fn note_short(&self) {
        self.short_pulls.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
    /// Largest pull (in device frames) since the previous call; 0 if none.
    pub fn take_recent_max(&self) -> usize {
        self.recent_max_frames.swap(0, std::sync::atomic::Ordering::Relaxed)
    }
    pub fn take_max_gap_us(&self) -> u64 {
        self.max_gap_us.swap(0, std::sync::atomic::Ordering::Relaxed)
    }
    pub fn short_pulls(&self) -> u64 {
        self.short_pulls.load(std::sync::atomic::Ordering::Relaxed)
    }
    pub fn callbacks(&self) -> u64 {
        self.callbacks.load(std::sync::atomic::Ordering::Relaxed)
    }
    #[inline]
    fn tap_push(&self, mono: f32) {
        if let Some(t) = &self.tap {
            if let Ok(mut p) = t.try_lock() {
                let _ = p.try_push((mono.clamp(-1.0, 1.0) * 32767.0) as i16);
            }
        }
    }
}

fn capture_stream_err(e: cpal::StreamError) {
    log::warn!("[pipeline] capture stream error: {}", e);
}
fn playback_stream_err(e: cpal::StreamError) {
    log::warn!("[pipeline] playback stream error: {}", e);
}

/// Resolve a capture device's stream config, guarding against a virtual driver
/// reporting a 0-channel / 0-rate config (which would later panic a resampler
/// or `chunks_exact`). Returns (config, channels, sample_format).
fn input_config(device: &cpal::Device) -> (cpal::StreamConfig, u16, cpal::SampleFormat) {
    match device.default_input_config() {
        Ok(cfg) => {
            let ch = cfg.channels();
            let rate = cfg.sample_rate();
            let fmt = cfg.sample_format();
            log::info!(
                "[pipeline] Input device: {}ch @ {}Hz (sample format: {:?})",
                ch, rate.0, fmt
            );
            if ch == 0 || rate.0 == 0 {
                log::warn!("[pipeline] Input device reports invalid config ({}ch @ {}Hz) — falling back to mono 48kHz f32", ch, rate.0);
                return (default_stream_config(1), 1, cpal::SampleFormat::F32);
            }
            (
                cpal::StreamConfig { channels: ch, sample_rate: rate, buffer_size: cpal::BufferSize::Default },
                ch,
                fmt,
            )
        }
        Err(e) => {
            log::warn!("[pipeline] default_input_config failed ({}), trying mono 48kHz f32", e);
            (default_stream_config(1), 1, cpal::SampleFormat::F32)
        }
    }
}

/// Resolve a playback device's stream config with the same 0-channel / 0-rate
/// guard. `label` distinguishes the three output roles in the logs.
fn output_config(device: &cpal::Device, label: &str) -> (cpal::StreamConfig, u16, cpal::SampleFormat) {
    match device.default_output_config() {
        Ok(cfg) => {
            let ch = cfg.channels();
            let rate = cfg.sample_rate();
            let fmt = cfg.sample_format();
            log::info!("[pipeline] {} device: {}ch @ {}Hz (sample format: {:?})", label, ch, rate.0, fmt);
            if ch == 0 || rate.0 == 0 {
                log::warn!("[pipeline] {} device reports invalid config ({}ch @ {}Hz) — falling back to stereo 48kHz f32", label, ch, rate.0);
                return (default_stream_config(2), 2, cpal::SampleFormat::F32);
            }
            (
                cpal::StreamConfig { channels: ch, sample_rate: rate, buffer_size: cpal::BufferSize::Default },
                ch,
                fmt,
            )
        }
        Err(e) => {
            log::warn!("[pipeline] {} default_output_config failed ({}), trying stereo 48kHz f32", label, e);
            (default_stream_config(2), 2, cpal::SampleFormat::F32)
        }
    }
}

fn default_stream_config(channels: u16) -> cpal::StreamConfig {
    cpal::StreamConfig {
        channels,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    }
}

/// Device buffer we ask for on capture and playback streams: 40ms total,
/// which on ALSA (cpal sets period = buffer/4) means ~10ms callbacks. cpal's default is a
/// 100ms buffer with ~25ms periods — that alone was ~60ms of output latency,
/// and a 25ms pull is what the per-peer rings must be kept primed against
/// (see OutputPullStats). Backends that reject a fixed size (WASAPI shared
/// mode) fall back to the default; the pipeline measures the real pull size
/// either way.
fn preferred_buffer_frames(rate: u32) -> u32 {
    rate / 25
}

/// Try the stream with our preferred fixed buffer first, then the backend
/// default. `$build` is an expression over `$cfg` producing
/// `Result<cpal::Stream, cpal::BuildStreamError>`.
macro_rules! build_with_buffer_fallback {
    ($cfg:ident, $label:expr, $build:expr) => {{
        let mut fixed = $cfg.clone();
        fixed.buffer_size = cpal::BufferSize::Fixed(preferred_buffer_frames($cfg.sample_rate.0));
        let attempt = { let $cfg = &fixed; $build };
        match attempt {
            Ok(s) => Ok(s),
            Err(e) => {
                log::info!("[pipeline] {}: fixed {}-frame buffer rejected ({}), using backend default",
                    $label, preferred_buffer_frames($cfg.sample_rate.0), e);
                let $cfg = &$cfg;
                $build
            }
        }
    }};
}

// ── Generic capture callback ─────────────────────────────────────────────────

/// Build the capture callback for sample type `T`. Downmixes to mono and
/// converts to i16 (the pipeline's internal capture format). `in_ch.max(1)`
/// guards against a 0-channel device slipping past `input_config`.
fn input_capture_cb<T>(
    in_ch: u16,
    cap_prod: Arc<std::sync::Mutex<HeapProd<i16>>>,
) -> impl FnMut(&[T], &cpal::InputCallbackInfo)
where
    T: SizedSample,
    f32: FromSample<T>,
{
    let n = in_ch.max(1) as usize;
    move |data: &[T], _: &cpal::InputCallbackInfo| {
        let Ok(mut prod) = cap_prod.try_lock() else { return };
        if n == 1 {
            for &s in data {
                let f = f32::from_sample(s);
                let _ = prod.try_push((f * 32767.0).clamp(-32768.0, 32767.0) as i16);
            }
        } else {
            for frame in data.chunks_exact(n) {
                let mut sum = 0.0f32;
                for &s in frame {
                    sum += f32::from_sample(s);
                }
                let mono = sum / n as f32;
                let _ = prod.try_push((mono * 32767.0).clamp(-32768.0, 32767.0) as i16);
            }
        }
    }
}

/// Build an input stream in the device's native sample format, dispatching to
/// `input_capture_cb::<T>`. Returns None on build/play failure.
fn build_input_stream_fmt(
    device: &cpal::Device,
    cfg: &cpal::StreamConfig,
    fmt: cpal::SampleFormat,
    in_ch: u16,
    cap_prod: Arc<std::sync::Mutex<HeapProd<i16>>>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    use cpal::SampleFormat as F;
    match fmt {
        F::I16 => device.build_input_stream(cfg, input_capture_cb::<i16>(in_ch, cap_prod), capture_stream_err, None),
        F::U16 => device.build_input_stream(cfg, input_capture_cb::<u16>(in_ch, cap_prod), capture_stream_err, None),
        F::I32 => device.build_input_stream(cfg, input_capture_cb::<i32>(in_ch, cap_prod), capture_stream_err, None),
        // F32 and any other format: use the f32 path (the common case, and the
        // safest best-effort fallback for exotic formats).
        _ => device.build_input_stream(cfg, input_capture_cb::<f32>(in_ch, cap_prod), capture_stream_err, None),
    }
}

// ── Input stream builder ─────────────────────────────────────────────────────

/// Build a CPAL input (capture) stream that pushes mono i16 samples at the
/// device's native sample rate into `capture_prod`.
/// Returns (stream, device_sample_rate) or None if no usable device is found.
///
/// The callback does NO resampling — just downmixes to mono and converts to i16.
/// Resampling from device rate → 48kHz happens in the main pipeline loop.
pub fn build_input_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    capture_prod: Arc<std::sync::Mutex<HeapProd<i16>>>,
) -> Option<(cpal::Stream, u32)> {
    let input_device = resolve_capture_device(host, device_name)?;
    let (input_cfg, in_ch, fmt) = input_config(&input_device);
    let input_sample_rate = input_cfg.sample_rate.0;

    let built = build_with_buffer_fallback!(input_cfg, "Capture",
        build_input_stream_fmt(&input_device, input_cfg, fmt, in_ch, Arc::clone(&capture_prod)));
    match built {
        Ok(stream) => {
            if let Err(e) = stream.play() {
                log::warn!("[pipeline] failed to start capture stream: {}", e);
                None
            } else {
                log::info!("[pipeline] Capture stream started: mono @ {}Hz ({:?} device)", input_sample_rate, fmt);
                Some((stream, input_sample_rate))
            }
        }
        Err(e) => {
            log::warn!("[pipeline] build_input_stream failed: {}", e);
            None
        }
    }
}

// ── Generic playback callbacks ───────────────────────────────────────────────
//
// Each returns a callback for sample type `T`. All mixing is done in f32,
// soft-clipped, then converted to the device's format via `T::from_sample`.
// `out_ch.max(1)` guards against a 0-channel device reaching `chunks_exact_mut`.

fn pop_voice_sum(guards: &mut [std::sync::MutexGuard<HeapCons<i16>>]) -> i32 {
    let mut sum: i32 = 0;
    for g in guards.iter_mut() {
        if let Some(s) = g.try_pop() {
            sum += s as i32;
        }
    }
    sum
}

/// Voice + stream mix (main output).
fn output_mix_cb<T>(
    out_ch: u16,
    peers_out: PeerList,
    stream_cons_out: Arc<std::sync::Mutex<HeapCons<i16>>>,
    pb_stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    pull_stats: Arc<OutputPullStats>,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: SizedSample + FromSample<f32>,
{
    let n = out_ch.max(1) as usize;
    let silence = T::from_sample(0.0f32);
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / n;
        pull_stats.record(frames);
        let peer_snapshot = peers_out.load();
        // Stack-inline for the typical ≤8-peer channel so the real-time output
        // callback does no heap allocation (only a >8-peer channel spills to the
        // heap). Guards live only for this callback, so `pop_voice_sum` still
        // takes it as a `&mut [_]` slice via Deref.
        let mut voice_guards: SmallVec<[std::sync::MutexGuard<HeapCons<i16>>; 8]> = peer_snapshot
            .iter()
            .filter_map(|p| p.cons.try_lock().ok())
            .collect();
        if voice_guards.iter().any(|g| { let a = g.occupied_len(); a > 0 && a < frames }) {
            pull_stats.note_short();
        }

        let Ok(mut stream_guard) = stream_cons_out.try_lock() else {
            drop(voice_guards);
            for sample in data.iter_mut() { *sample = silence; }
            return;
        };

        if n == 1 {
            for sample in data.iter_mut() {
                let v = pop_voice_sum(&mut voice_guards);
                let s = stream_guard.try_pop().unwrap_or(0) as i32;
                let m = soft_clip((v + s) as f32 / 32768.0);
                pull_stats.tap_push(m);
                *sample = T::from_sample(m);
            }
        } else {
            for frame in data.chunks_exact_mut(n) {
                let v = pop_voice_sum(&mut voice_guards);
                if pb_stream_stereo.load(std::sync::atomic::Ordering::Relaxed) && n >= 2 {
                    let sl = stream_guard.try_pop().unwrap_or(0) as i32;
                    let sr = stream_guard.try_pop().unwrap_or(0) as i32;
                    let ml = soft_clip((v + sl) as f32 / 32768.0);
                    let mr = soft_clip((v + sr) as f32 / 32768.0);
                    pull_stats.tap_push((ml + mr) * 0.5);
                    let left = T::from_sample(ml);
                    let right = T::from_sample(mr);
                    frame[0] = left;
                    frame[1] = right;
                    for ch in &mut frame[2..] { *ch = left; }
                } else {
                    let s = stream_guard.try_pop().unwrap_or(0) as i32;
                    let m = soft_clip((v + s) as f32 / 32768.0);
                    pull_stats.tap_push(m);
                    let mixed = T::from_sample(m);
                    for ch in frame.iter_mut() { *ch = mixed; }
                }
            }
        }
    }
}

/// Voice only (used when stream audio is on a separate device).
fn voice_only_cb<T>(
    out_ch: u16,
    peers_out: PeerList,
    pull_stats: Arc<OutputPullStats>,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: SizedSample + FromSample<f32>,
{
    let n = out_ch.max(1) as usize;
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        let frames = data.len() / n;
        pull_stats.record(frames);
        let peer_snapshot = peers_out.load();
        // Stack-inline for the typical ≤8-peer channel so the real-time output
        // callback does no heap allocation (only a >8-peer channel spills to the
        // heap). Guards live only for this callback, so `pop_voice_sum` still
        // takes it as a `&mut [_]` slice via Deref.
        let mut voice_guards: SmallVec<[std::sync::MutexGuard<HeapCons<i16>>; 8]> = peer_snapshot
            .iter()
            .filter_map(|p| p.cons.try_lock().ok())
            .collect();
        if voice_guards.iter().any(|g| { let a = g.occupied_len(); a > 0 && a < frames }) {
            pull_stats.note_short();
        }
        for frame in data.chunks_exact_mut(n) {
            let v = pop_voice_sum(&mut voice_guards);
            let m = soft_clip(v as f32 / 32768.0);
            pull_stats.tap_push(m);
            let out = T::from_sample(m);
            for ch in frame.iter_mut() { *ch = out; }
        }
    }
}

/// Stream audio only (separate stream-output device), with stereo support.
fn stream_only_cb<T>(
    out_ch: u16,
    stream_cons_out: Arc<std::sync::Mutex<HeapCons<i16>>>,
    pb_stereo: Arc<std::sync::atomic::AtomicBool>,
    pull_stats: Arc<OutputPullStats>,
) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: SizedSample + FromSample<f32>,
{
    let n = out_ch.max(1) as usize;
    let silence = T::from_sample(0.0f32);
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| {
        pull_stats.record(data.len() / n);
        let Ok(mut guard) = stream_cons_out.try_lock() else {
            for sample in data.iter_mut() { *sample = silence; }
            return;
        };
        let is_stereo = pb_stereo.load(std::sync::atomic::Ordering::Relaxed);
        if n == 1 {
            for sample in data.iter_mut() {
                let s = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                if is_stereo { let _ = guard.try_pop(); }
                *sample = T::from_sample(s);
            }
        } else {
            for frame in data.chunks_exact_mut(n) {
                if is_stereo && n >= 2 {
                    let sl = T::from_sample(guard.try_pop().unwrap_or(0) as f32 / 32768.0);
                    let sr = T::from_sample(guard.try_pop().unwrap_or(0) as f32 / 32768.0);
                    frame[0] = sl;
                    frame[1] = sr;
                    for ch in &mut frame[2..] { *ch = sl; }
                } else {
                    let s = T::from_sample(guard.try_pop().unwrap_or(0) as f32 / 32768.0);
                    for ch in frame.iter_mut() { *ch = s; }
                }
            }
        }
    }
}

/// Dispatch a playback-callback maker across the device's sample format. The
/// maker is invoked with the concrete `T`; the format arm picks it. `$mk` is a
/// generic fn and each of its owned args is cloned per arm (Arc clones are
/// cheap; only one arm is ever built at runtime).
macro_rules! build_output_dispatch {
    ($device:expr, $cfg:expr, $fmt:expr, $mk:ident ( $($arg:expr),* $(,)? )) => {{
        use cpal::SampleFormat as F;
        match $fmt {
            F::I16 => $device.build_output_stream($cfg, $mk::<i16>($($arg.clone()),*), playback_stream_err, None),
            F::U16 => $device.build_output_stream($cfg, $mk::<u16>($($arg.clone()),*), playback_stream_err, None),
            F::I32 => $device.build_output_stream($cfg, $mk::<i32>($($arg.clone()),*), playback_stream_err, None),
            _ => $device.build_output_stream($cfg, $mk::<f32>($($arg.clone()),*), playback_stream_err, None),
        }
    }};
}

// ── Output stream builder ────────────────────────────────────────────────────

/// Build a CPAL output (playback) stream that mixes voice + stream audio from
/// their respective ring buffer consumers. The ring buffers carry i16 samples
/// at the output device's native rate — all resampling happens in the main loop.
pub fn build_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    peers: PeerList,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    _render_ref_prod: Arc<std::sync::Mutex<HeapProd<f32>>>,
    pull_stats: Arc<OutputPullStats>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = resolve_playback_device(host, device_name)?;
    let (stream_config, out_ch, fmt) = output_config(&output_device, "Output");
    let output_sample_rate = stream_config.sample_rate.0;

    // Build in the device's native sample format (see build_output_dispatch!).
    // Mixing sums each peer's ring as i32, adds stream audio, soft-limits, and
    // converts to the device format.
    let stream = match build_with_buffer_fallback!(stream_config, "Output", build_output_dispatch!(
        &output_device, stream_config, fmt,
        output_mix_cb(out_ch, peers, stream_cons, stream_stereo, pull_stats)
    )) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!(
                "Failed to build output stream: {}", e
            )));
            return None;
        }
    };
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!(
            "Failed to start output stream: {}", e
        )));
        return None;
    }

    Some((stream, output_sample_rate, out_ch))
}

// ── Voice-only output stream builder ─────────────────────────────────────────

/// Build a CPAL output stream that plays only voice audio (no stream mixing).
/// Used when stream audio is routed to a separate device.
/// Ring buffer carries i16 at device native rate — no callback resampling.
pub fn build_voice_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    peers: PeerList,
    _render_ref_prod: Arc<std::sync::Mutex<HeapProd<f32>>>,
    pull_stats: Arc<OutputPullStats>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = resolve_playback_device(host, device_name)?;
    let (stream_config, out_ch, fmt) = output_config(&output_device, "Voice output");
    let output_sample_rate = stream_config.sample_rate.0;

    let stream = match build_with_buffer_fallback!(stream_config, "Voice output", build_output_dispatch!(
        &output_device, stream_config, fmt,
        voice_only_cb(out_ch, peers, pull_stats)
    )) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build voice output stream: {}", e)));
            return None;
        }
    };
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!("Failed to start voice output stream: {}", e)));
        return None;
    }

    Some((stream, output_sample_rate, out_ch))
}

// ── Stream-only output stream builder ────────────────────────────────────────

/// Build a CPAL output stream that plays only stream audio (with stereo support).
/// Used when stream audio is routed to a separate device.
/// Ring buffer carries i16 at device native rate — no callback resampling.
pub fn build_stream_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    pull_stats: Arc<OutputPullStats>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = resolve_playback_device(host, device_name)?;
    let (stream_config, out_ch, fmt) = output_config(&output_device, "Stream output");
    let output_sample_rate = stream_config.sample_rate.0;

    let stream = match build_with_buffer_fallback!(stream_config, "Stream output", build_output_dispatch!(
        &output_device, stream_config, fmt,
        stream_only_cb(out_ch, stream_cons, stream_stereo, pull_stats)
    )) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build stream output: {}", e)));
            return None;
        }
    };
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!("Failed to start stream output: {}", e)));
        return None;
    }

    Some((stream, output_sample_rate, out_ch))
}
