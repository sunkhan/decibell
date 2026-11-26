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
use super::cue::{CueBus, CueMix, CueVoices};
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

/// Held while a stream is opened. On Linux a device is chosen through the
/// process environment (PULSE_SINK / PULSE_SOURCE, below), and streams are
/// opened from more than one thread (the voice pipeline, the interface
/// sounds' stream, the microphone test): one at a time, so each opens on the
/// device it set.
static DEVICE_OPEN: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn device_open_lock() -> std::sync::MutexGuard<'static, ()> {
    DEVICE_OPEN.lock().unwrap_or_else(|e| e.into_inner())
}

// ── Linux: route a stored device selection through PulseAudio / PipeWire ──────
//
// On Linux we enumerate devices with `pactl` (api/settings.rs), so the
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
    wake: Option<std::thread::Thread>,
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
        drop(prod);
        if let Some(t) = &wake {
            t.unpark();
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
    wake: Option<std::thread::Thread>,
) -> Result<cpal::Stream, cpal::BuildStreamError> {
    use cpal::SampleFormat as F;
    match fmt {
        F::I16 => device.build_input_stream(cfg, input_capture_cb::<i16>(in_ch, cap_prod, wake), capture_stream_err, None),
        F::U16 => device.build_input_stream(cfg, input_capture_cb::<u16>(in_ch, cap_prod, wake), capture_stream_err, None),
        F::I32 => device.build_input_stream(cfg, input_capture_cb::<i32>(in_ch, cap_prod, wake), capture_stream_err, None),
        // F32 and any other format: use the f32 path (the common case, and the
        // safest best-effort fallback for exotic formats).
        _ => device.build_input_stream(cfg, input_capture_cb::<f32>(in_ch, cap_prod, wake), capture_stream_err, None),
    }
}

// ── Input stream builder ─────────────────────────────────────────────────────

/// Build a CPAL input (capture) stream that pushes mono i16 samples at the
/// device's native sample rate into `capture_prod`.
/// Returns (stream, device_sample_rate) or None if no usable device is found.
///
/// The callback does NO resampling — just downmixes to mono and converts to i16.
/// Resampling from device rate → 48kHz happens in the main pipeline loop.
/// `wake`: a thread to unpark after each callback (the microphone test sleeps
/// until there are samples; the voice pipeline has its own tick and passes None).
pub fn build_input_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    capture_prod: Arc<std::sync::Mutex<HeapProd<i16>>>,
    wake: Option<std::thread::Thread>,
) -> Option<(cpal::Stream, u32)> {
    let _open = device_open_lock();
    let input_device = resolve_capture_device(host, device_name)?;
    let (input_cfg, in_ch, fmt) = input_config(&input_device);
    let input_sample_rate = input_cfg.sample_rate.0;

    let built = build_with_buffer_fallback!(input_cfg, "Capture",
        build_input_stream_fmt(&input_device, input_cfg, fmt, in_ch, Arc::clone(&capture_prod), wake.clone()));
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

/// What the pipeline tells a live output stream's callback, and what the
/// callback reports back. Each switch is a 5 ms linear gain ramp inside the
/// callback, so nothing the pipeline does to a stream is a step in what the
/// device plays.
pub struct OutputCtl {
    /// Voice and stream audio play (false once the session is ending and
    /// only the interface sounds still playing are let finish).
    program_on: std::sync::atomic::AtomicBool,
    /// The stream is in use. False: everything fades out, then the callback
    /// writes silence and leaves the rings alone (the stream is about to be
    /// closed; closing it mid-signal would cut the waveform).
    live: std::sync::atomic::AtomicBool,
    /// The fade to silence is complete.
    faded: std::sync::atomic::AtomicBool,
    /// Frames of silence written since.
    silent_frames: std::sync::atomic::AtomicU64,
    /// The pipeline thread: woken when the fade completes.
    waiter: std::thread::Thread,
    rate: u32,
}

impl OutputCtl {
    pub(crate) fn new(rate: u32) -> Arc<Self> {
        use std::sync::atomic::{AtomicBool, AtomicU64};
        Arc::new(OutputCtl {
            program_on: AtomicBool::new(true),
            live: AtomicBool::new(true),
            faded: AtomicBool::new(false),
            silent_frames: AtomicU64::new(0),
            waiter: std::thread::current(),
            rate,
        })
    }

    pub fn set_program(&self, on: bool) {
        self.program_on.store(on, std::sync::atomic::Ordering::Release);
    }

    /// Start the fade to silence. One way: a stream is closed after it.
    pub fn fade_out(&self) {
        self.live.store(false, std::sync::atomic::Ordering::Release);
    }

    pub(crate) fn is_live(&self) -> bool {
        self.live.load(std::sync::atomic::Ordering::Acquire)
    }

    pub fn is_faded(&self) -> bool {
        self.faded.load(std::sync::atomic::Ordering::Acquire)
    }

    /// Milliseconds of silence the device has been handed since the fade.
    pub fn silent_ms(&self) -> u64 {
        self.silent_frames.load(std::sync::atomic::Ordering::Acquire) * 1000 / self.rate.max(1) as u64
    }

    /// Fade out and wait for the callback to report it (it takes 5 ms of
    /// audio, so one or two callbacks), then for `flush_ms` of silence to be
    /// handed to the device, at most `max` altogether (a device whose callback
    /// has stopped never reports). Called on the thread that built the stream.
    pub fn fade_out_and_wait(&self, flush_ms: u64, max: std::time::Duration) {
        self.fade_out();
        let deadline = Instant::now() + max;
        while !(self.is_faded() && self.silent_ms() >= flush_ms) {
            let now = Instant::now();
            if now >= deadline {
                break;
            }
            std::thread::park_timeout(deadline - now);
        }
    }

    #[inline]
    fn note_silence(&self, frames: usize) {
        use std::sync::atomic::Ordering::{AcqRel, Release};
        let before = self.silent_frames.fetch_add(frames as u64, AcqRel);
        if !self.faded.swap(true, Release) {
            self.waiter.unpark();
        }
        // Wake the waiter again as each further 10 ms of silence goes out
        // (it may be waiting for the device's buffer to play out).
        let step = (self.rate as u64 / 100).max(1);
        if before / step != (before + frames as u64) / step {
            self.waiter.unpark();
        }
    }
}

/// The callback's own gain state for an `OutputCtl`.
struct Gains {
    program: f32,
    master: f32,
    /// Per-frame step of a 5 ms ramp.
    step: f32,
}

impl Gains {
    fn new(rate: u32) -> Self {
        Gains { program: 1.0, master: 1.0, step: 200.0 / rate.max(1) as f32 }
    }
    #[inline]
    fn approach(g: &mut f32, target: f32, step: f32) {
        if *g < target {
            *g = (*g + step).min(target);
        } else if *g > target {
            *g = (*g - step).max(target);
        }
    }
}

/// `x` (an i16-scale sum) at gain `g`; exactly `x` at unity.
#[inline]
fn scaled(x: i32, g: f32) -> i32 {
    if g == 1.0 { x } else { (x as f32 * g) as i32 }
}

/// The main output's mix, one device callback at a time: every peer's voice
/// ring summed, plus stream audio when it plays on this device (`stream`)
/// and the interface sounds (`cues`), soft-limited and converted to the
/// device's format. Held by the callback; `render` is the callback's whole
/// body, so tests drive it without a device.
pub(crate) struct OutputMix {
    n: usize,
    peers: PeerList,
    /// Stream audio's ring and its stereo flag; None for the voice-only
    /// output (stream audio on a separate device).
    stream: Option<(Arc<std::sync::Mutex<HeapCons<i16>>>, Arc<std::sync::atomic::AtomicBool>)>,
    pull_stats: Arc<OutputPullStats>,
    ctl: Arc<OutputCtl>,
    gains: Gains,
    cues: CueVoices,
}

impl OutputMix {
    pub(crate) fn new(
        out_ch: u16,
        peers: PeerList,
        stream: Option<(Arc<std::sync::Mutex<HeapCons<i16>>>, Arc<std::sync::atomic::AtomicBool>)>,
        pull_stats: Arc<OutputPullStats>,
        ctl: Arc<OutputCtl>,
        cues: CueVoices,
    ) -> Self {
        // `max(1)` guards against a 0-channel device reaching `chunks_exact_mut`.
        let gains = Gains::new(ctl.rate);
        OutputMix { n: out_ch.max(1) as usize, peers, stream, pull_stats, ctl, gains, cues }
    }

    pub(crate) fn render<T>(&mut self, data: &mut [T])
    where
        T: SizedSample + FromSample<f32>,
    {
        use std::sync::atomic::Ordering::Acquire;
        let n = self.n;
        let pull_stats = &self.pull_stats;
        let frames = data.len() / n;
        pull_stats.record(frames);
        self.cues.begin();
        let target_program = if self.ctl.program_on.load(Acquire) { 1.0 } else { 0.0 };
        let target_master = if self.ctl.live.load(Acquire) { 1.0 } else { 0.0 };
        if self.gains.master == 0.0 && target_master == 0.0 {
            // Faded out and about to be closed: silence, rings untouched.
            let silence = T::from_sample(0.0f32);
            for sample in data.iter_mut() { *sample = silence; }
            self.ctl.note_silence(frames);
            self.cues.end(frames);
            return;
        }
        let peer_snapshot = self.peers.load();
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

        let mut stream_guard = match &self.stream {
            None => None,
            Some((stream_cons_out, _)) => match stream_cons_out.try_lock() {
                Ok(g) => Some(g),
                Err(_) => {
                    drop(voice_guards);
                    let silence = T::from_sample(0.0f32);
                    for sample in data.iter_mut() { *sample = silence; }
                    self.cues.end(frames);
                    return;
                }
            },
        };
        let stereo = n >= 2
            && self.stream.as_ref().is_some_and(|(_, flag)| flag.load(std::sync::atomic::Ordering::Relaxed));

        let step = self.gains.step;
        for frame in data.chunks_exact_mut(n) {
            Gains::approach(&mut self.gains.program, target_program, step);
            Gains::approach(&mut self.gains.master, target_master, step);
            let (gp, gm) = (self.gains.program, self.gains.master);
            let v = scaled(pop_voice_sum(&mut voice_guards), gp);
            let c = self.cues.sample();
            match (&mut stream_guard, stereo) {
                (Some(g), true) => {
                    let sl = scaled(g.try_pop().unwrap_or(0) as i32, gp);
                    let sr = scaled(g.try_pop().unwrap_or(0) as i32, gp);
                    let mut ml = soft_clip((v + sl + c) as f32 / 32768.0);
                    let mut mr = soft_clip((v + sr + c) as f32 / 32768.0);
                    if gm != 1.0 {
                        ml *= gm;
                        mr *= gm;
                    }
                    pull_stats.tap_push((ml + mr) * 0.5);
                    let left = T::from_sample(ml);
                    let right = T::from_sample(mr);
                    frame[0] = left;
                    frame[1] = right;
                    for ch in &mut frame[2..] { *ch = left; }
                }
                (g, _) => {
                    let s = match g {
                        Some(g) => scaled(g.try_pop().unwrap_or(0) as i32, gp),
                        None => 0,
                    };
                    let mut m = soft_clip((v + s + c) as f32 / 32768.0);
                    if gm != 1.0 {
                        m *= gm;
                    }
                    pull_stats.tap_push(m);
                    let mixed = T::from_sample(m);
                    for ch in frame.iter_mut() { *ch = mixed; }
                }
            }
        }
        self.cues.end(frames);
    }
}

/// Voice + stream mix (main output).
fn output_mix_cb<T>(mix: &SharedMix<OutputMix>) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: SizedSample + FromSample<f32>,
{
    let mut mix = mix.take();
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| mix.render(data)
}

/// Stream audio only (separate stream-output device), with stereo support.
pub(crate) struct StreamMix {
    n: usize,
    stream_cons_out: Arc<std::sync::Mutex<HeapCons<i16>>>,
    pb_stereo: Arc<std::sync::atomic::AtomicBool>,
    pull_stats: Arc<OutputPullStats>,
    ctl: Arc<OutputCtl>,
    gains: Gains,
}

impl StreamMix {
    pub(crate) fn new(
        out_ch: u16,
        stream_cons_out: Arc<std::sync::Mutex<HeapCons<i16>>>,
        pb_stereo: Arc<std::sync::atomic::AtomicBool>,
        pull_stats: Arc<OutputPullStats>,
        ctl: Arc<OutputCtl>,
    ) -> Self {
        let gains = Gains::new(ctl.rate);
        StreamMix { n: out_ch.max(1) as usize, stream_cons_out, pb_stereo, pull_stats, ctl, gains }
    }

    pub(crate) fn render<T>(&mut self, data: &mut [T])
    where
        T: SizedSample + FromSample<f32>,
    {
        let n = self.n;
        self.pull_stats.record(data.len() / n);
        // The stream is being closed (a device switch, the session's end):
        // a 5 ms fade, then silence with the ring left alone.
        let target_master = if self.ctl.live.load(std::sync::atomic::Ordering::Acquire) { 1.0 } else { 0.0 };
        if self.gains.master == 0.0 && target_master == 0.0 {
            let silence = T::from_sample(0.0f32);
            for sample in data.iter_mut() { *sample = silence; }
            self.ctl.note_silence(data.len() / n);
            return;
        }
        let Ok(mut guard) = self.stream_cons_out.try_lock() else {
            let silence = T::from_sample(0.0f32);
            for sample in data.iter_mut() { *sample = silence; }
            return;
        };
        let is_stereo = self.pb_stereo.load(std::sync::atomic::Ordering::Relaxed);
        let step = self.gains.step;
        // Exactly the ring's sample at unity gain.
        let gain = |g: &mut Gains| {
            Gains::approach(&mut g.master, target_master, step);
            g.master
        };
        let at = |s: f32, g: f32| if g == 1.0 { s } else { s * g };
        if n == 1 {
            for sample in data.iter_mut() {
                let g = gain(&mut self.gains);
                let s = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                if is_stereo { let _ = guard.try_pop(); }
                *sample = T::from_sample(at(s, g));
            }
        } else {
            for frame in data.chunks_exact_mut(n) {
                let g = gain(&mut self.gains);
                if is_stereo && n >= 2 {
                    let sl = T::from_sample(at(guard.try_pop().unwrap_or(0) as f32 / 32768.0, g));
                    let sr = T::from_sample(at(guard.try_pop().unwrap_or(0) as f32 / 32768.0, g));
                    frame[0] = sl;
                    frame[1] = sr;
                    for ch in &mut frame[2..] { *ch = sl; }
                } else {
                    let s = T::from_sample(at(guard.try_pop().unwrap_or(0) as f32 / 32768.0, g));
                    for ch in frame.iter_mut() { *ch = s; }
                }
            }
        }
    }
}

fn stream_only_cb<T>(mix: &SharedMix<StreamMix>) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: SizedSample + FromSample<f32>,
{
    let mut mix = mix.take();
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| mix.render(data)
}

/// A mix on its way into a stream's callback. The build macros below make
/// one callback per sample format and per buffer-size attempt, each from the
/// same arguments, but only the stream that is kept runs: the mix moves into
/// a callback when it is made and back out if that stream is not built.
pub(crate) struct SharedMix<M>(Arc<std::sync::Mutex<Option<M>>>);

impl<M> Clone for SharedMix<M> {
    fn clone(&self) -> Self {
        SharedMix(Arc::clone(&self.0))
    }
}

impl<M> SharedMix<M> {
    pub(crate) fn new(mix: M) -> Self {
        SharedMix(Arc::new(std::sync::Mutex::new(Some(mix))))
    }
    fn take(&self) -> Returning<M> {
        Returning { mix: self.0.lock().unwrap().take(), home: Arc::clone(&self.0) }
    }
}

/// The mix inside a callback; goes back to its `SharedMix` when the callback
/// is dropped (a failed build, or the stream's end).
struct Returning<M> {
    mix: Option<M>,
    home: Arc<std::sync::Mutex<Option<M>>>,
}

impl<M> std::ops::Deref for Returning<M> {
    type Target = M;
    fn deref(&self) -> &M {
        self.mix.as_ref().expect("one callback holds the mix")
    }
}

impl<M> std::ops::DerefMut for Returning<M> {
    fn deref_mut(&mut self) -> &mut M {
        self.mix.as_mut().expect("one callback holds the mix")
    }
}

impl<M> Drop for Returning<M> {
    fn drop(&mut self) {
        if let (Some(m), Ok(mut home)) = (self.mix.take(), self.home.lock()) {
            *home = Some(m);
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
            F::I16 => $device.build_output_stream($cfg, $mk::<i16>($($arg),*), playback_stream_err, None),
            F::U16 => $device.build_output_stream($cfg, $mk::<u16>($($arg),*), playback_stream_err, None),
            F::I32 => $device.build_output_stream($cfg, $mk::<i32>($($arg),*), playback_stream_err, None),
            _ => $device.build_output_stream($cfg, $mk::<f32>($($arg),*), playback_stream_err, None),
        }
    }};
}

// ── Output stream builders ───────────────────────────────────────────────────

/// A live output stream and the handles to its callback.
pub struct OutputHandle {
    /// The open stream (a `cpal::Stream`): dropping it closes the device.
    pub(crate) stream: Box<dyn std::any::Any>,
    pub rate: u32,
    pub ctl: Arc<OutputCtl>,
    /// Where interface sounds are sent while this stream is the voice
    /// output; None for the stream-audio-only output.
    pub(crate) cues: Option<Arc<CueBus>>,
}

/// The device's buffer as we ask for it (`preferred_buffer_frames`), in ms:
/// how much silence follows a fade-out before a stream is closed, so the
/// fade itself has been played.
pub(crate) const DEVICE_BUFFER_MS: u64 = 40;

fn started(stream: cpal::Stream, what: &str, event_tx: &std::sync::mpsc::Sender<VoiceEvent>) -> Option<cpal::Stream> {
    if let Err(e) = stream.play() {
        let _ = event_tx.send(VoiceEvent::Error(format!("Failed to start {}: {}", what, e)));
        return None;
    }
    Some(stream)
}

/// The main output: voice, interface sounds, and stream audio too when
/// `stream` is given (else it plays on a separate device). The ring buffers
/// carry i16 samples at the output device's native rate — all resampling
/// happens in the main loop. Built in the device's native sample format (see
/// build_output_dispatch!): the mix sums each peer's ring as i32, adds stream
/// audio, soft-limits, and converts to the device format.
pub fn build_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    peers: PeerList,
    stream: Option<(Arc<std::sync::Mutex<HeapCons<i16>>>, Arc<std::sync::atomic::AtomicBool>)>,
    pull_stats: Arc<OutputPullStats>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<OutputHandle> {
    let label = if stream.is_some() { "Output" } else { "Voice output" };
    let what = if stream.is_some() { "output stream" } else { "voice output stream" };
    let _open = device_open_lock();
    let output_device = resolve_playback_device(host, device_name)?;
    let (stream_config, out_ch, fmt) = output_config(&output_device, label);
    let rate = stream_config.sample_rate.0;
    let ctl = OutputCtl::new(rate);
    let (cues, voices) = CueBus::new(rate, std::thread::current(), u64::MAX);
    let mix = SharedMix::new(OutputMix::new(out_ch, peers, stream, pull_stats, Arc::clone(&ctl), voices));
    let built = match build_with_buffer_fallback!(stream_config, label, build_output_dispatch!(
        &output_device, stream_config, fmt,
        output_mix_cb(&mix)
    )) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build {}: {}", what, e)));
            return None;
        }
    };
    let stream = started(built, what, event_tx)?;
    Some(OutputHandle { stream: Box::new(stream), rate, ctl, cues: Some(cues) })
}

/// Stream audio only (with stereo support), for when it is routed to a
/// separate device. Ring buffer carries i16 at device native rate.
pub fn build_stream_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    pull_stats: Arc<OutputPullStats>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<OutputHandle> {
    let _open = device_open_lock();
    let output_device = resolve_playback_device(host, device_name)?;
    let (stream_config, out_ch, fmt) = output_config(&output_device, "Stream output");
    let rate = stream_config.sample_rate.0;
    let ctl = OutputCtl::new(rate);
    let mix = SharedMix::new(StreamMix::new(out_ch, stream_cons, stream_stereo, pull_stats, Arc::clone(&ctl)));
    let built = match build_with_buffer_fallback!(stream_config, "Stream output", build_output_dispatch!(
        &output_device, stream_config, fmt,
        stream_only_cb(&mix)
    )) {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build stream output: {}", e)));
            return None;
        }
    };
    let stream = started(built, "stream output", event_tx)?;
    Some(OutputHandle { stream: Box::new(stream), rate, ctl, cues: None })
}

fn cue_cb<T>(mix: &SharedMix<CueMix>) -> impl FnMut(&mut [T], &cpal::OutputCallbackInfo)
where
    T: SizedSample + FromSample<f32>,
{
    let mut mix = mix.take();
    move |data: &mut [T], _: &cpal::OutputCallbackInfo| mix.render(data)
}

/// The interface sounds' own stream, for when no voice output is live
/// (cue.rs). `make(rate, channels)` builds its mix.
pub(crate) fn build_cue_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    make: Box<dyn FnOnce(u32, u16) -> CueMix + Send>,
) -> Option<cpal::Stream> {
    let _open = device_open_lock();
    let output_device = resolve_playback_device(host, device_name)?;
    let (stream_config, out_ch, fmt) = output_config(&output_device, "Sounds output");
    let mix = SharedMix::new(make(stream_config.sample_rate.0, out_ch));
    let built = build_with_buffer_fallback!(stream_config, "Sounds output", build_output_dispatch!(
        &output_device, stream_config, fmt,
        cue_cb(&mix)
    ));
    match built {
        Ok(stream) => match stream.play() {
            Ok(()) => Some(stream),
            Err(e) => {
                log::warn!("[cue] failed to start the sounds output: {}", e);
                None
            }
        },
        Err(e) => {
            log::warn!("[cue] failed to build the sounds output: {}", e);
            None
        }
    }
}

#[cfg(test)]
pub(crate) mod mix_tests {
    //! The output mixes driven as the device would drive them, without one.
    use super::*;
    use crate::media::cue::tests::file;
    use ringbuf::{traits::Split, HeapRb};
    use std::sync::atomic::AtomicBool;

    pub(crate) struct Rig {
        pub mix: OutputMix,
        pub ctl: Arc<OutputCtl>,
        pub cues: Arc<CueBus>,
        pub voice: HeapProd<i16>,
        pub stream: HeapProd<i16>,
        pub stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
        pub stereo: Arc<AtomicBool>,
        pub pull: Arc<OutputPullStats>,
    }

    /// A main output at 48 kHz with one peer and (if `with_stream`) stream
    /// audio in its mix.
    pub(crate) fn rig(channels: u16, with_stream: bool) -> Rig {
        let (voice, vcons) = HeapRb::<i16>::new(48_000 * 4).split();
        let (stream, scons) = HeapRb::<i16>::new(48_000 * 4).split();
        let peers: PeerList = Arc::new(ArcSwap::from_pointee(vec![PeerOutput {
            username: "101".into(),
            cons: Arc::new(std::sync::Mutex::new(vcons)),
        }]));
        let stream_cons = Arc::new(std::sync::Mutex::new(scons));
        let stereo = Arc::new(AtomicBool::new(false));
        let ctl = OutputCtl::new(48_000);
        let (cues, voices) = CueBus::new(48_000, std::thread::current(), u64::MAX);
        let pull = Arc::new(OutputPullStats::new(None));
        let mix = OutputMix::new(
            channels,
            peers,
            with_stream.then(|| (Arc::clone(&stream_cons), Arc::clone(&stereo))),
            Arc::clone(&pull),
            Arc::clone(&ctl),
            voices,
        );
        Rig { mix, ctl, cues, voice, stream, stream_cons, stereo, pull }
    }

    pub(crate) fn tone(freq: f64, amp: f64, n: usize) -> Vec<i16> {
        (0..n).map(|i| ((2.0 * std::f64::consts::PI * freq * i as f64 / 48_000.0).sin() * amp) as i16).collect()
    }

    /// Render `frames` frames in 10 ms callbacks; channel 0 (and 1).
    pub(crate) fn render(mix: &mut OutputMix, channels: usize, frames: usize) -> (Vec<f32>, Vec<f32>) {
        let (mut l, mut r) = (Vec::new(), Vec::new());
        let mut buf = vec![0f32; channels * 480];
        let mut left = frames;
        while left > 0 {
            let n = left.min(480);
            mix.render(&mut buf[..channels * n]);
            for f in buf[..channels * n].chunks_exact(channels) {
                l.push(f[0]);
                r.push(f[channels - 1]);
            }
            left -= n;
        }
        (l, r)
    }

    /// The largest step between neighbouring samples.
    pub(crate) fn max_step(x: &[f32]) -> f32 {
        x.windows(2).map(|w| (w[1] - w[0]).abs()).fold(0.0, f32::max)
    }

    /// With no interface sound playing and nothing fading, the mix is what
    /// the callbacks computed before they had either: voice plus stream audio
    /// through the limiter, bit for bit (mono, stereo with a mono and with a
    /// stereo stream, voice only).
    #[test]
    fn the_mix_is_unchanged_at_rest() {
        let v = tone(180.0, 9_000.0, 4_800);
        let s = tone(410.0, 21_000.0, 9_600);
        for (channels, with_stream, stereo) in [(1, true, false), (2, true, false), (2, true, true), (2, false, false), (6, true, true)] {
            let mut rig = rig(channels, with_stream);
            rig.stereo.store(stereo, std::sync::atomic::Ordering::Relaxed);
            rig.voice.push_slice(&v);
            rig.stream.push_slice(&s);
            let (l, r) = render(&mut rig.mix, channels as usize, 4_800);
            for i in 0..4_800 {
                let vi = v[i] as i32;
                let (want_l, want_r) = if !with_stream {
                    let m = soft_clip(vi as f32 / 32768.0);
                    (m, m)
                } else if stereo {
                    (soft_clip((vi + s[2 * i] as i32) as f32 / 32768.0), soft_clip((vi + s[2 * i + 1] as i32) as f32 / 32768.0))
                } else {
                    let m = soft_clip((vi + s[i] as i32) as f32 / 32768.0);
                    (m, m)
                };
                // Past stereo, the other channels repeat the left one.
                let want_last = if channels > 2 { want_l } else { want_r };
                assert_eq!((l[i], r[i]), (want_l, want_last), "{channels} ch, stream {with_stream}, stereo {stereo}, frame {i}");
            }
        }
    }

    /// In voice an interface sound is one more term of the sum the limiter
    /// sees: the peer's voice goes on exactly as it was, with the file's
    /// samples added while the sound plays.
    #[test]
    fn a_sound_is_added_to_the_voice_mix() {
        let mut rig = rig(2, true);
        let v = tone(180.0, 4_000.0, 48_000);
        rig.voice.push_slice(&v);
        let (before, _) = render(&mut rig.mix, 2, 960);
        let sound = file("user_join");
        let pcm = sound.at_rate(48_000);
        crate::media::cue::tests::play_on(&rig.cues, &sound);
        let (during, _) = render(&mut rig.mix, 2, pcm.len() + 960);
        for i in 0..960 {
            assert_eq!(before[i], v[i] as f32 / 32768.0);
        }
        for i in 0..pcm.len() + 960 {
            let c = pcm.get(i).copied().unwrap_or(0) as i32;
            assert_eq!(during[i], soft_clip((v[960 + i] as i32 + c) as f32 / 32768.0), "frame {i}");
        }
        assert_eq!(rig.cues.busy(), 0);
    }

    /// A stream that is about to be closed: everything fades to silence over
    /// 5 ms (no step larger than the tone's own slope plus the ramp's), the
    /// callback then writes silence, leaves the rings alone and reports how
    /// much silence the device got.
    #[test]
    fn closing_an_output_fades_it_out() {
        let mut rig = rig(2, true);
        let amp = 12_000.0;
        rig.voice.push_slice(&tone(300.0, amp, 48_000));
        let (a, _) = render(&mut rig.mix, 2, 4_800);
        rig.ctl.fade_out();
        assert!(!rig.ctl.is_faded());
        let (b, _) = render(&mut rig.mix, 2, 4_800);
        let all: Vec<f32> = a.iter().chain(&b).copied().collect();
        // The tone's largest step, plus the 240-sample ramp's share.
        let slope = (amp as f32 / 32768.0) * (2.0 * std::f32::consts::PI * 300.0 / 48_000.0);
        let bound = slope + (amp as f32 / 32768.0) / 240.0 + 2.0 / 32768.0;
        assert!(max_step(&all) <= bound, "step {} > {}", max_step(&all), bound);
        assert!(b[..200].iter().any(|&s| s != 0.0), "still fading");
        assert!(b[241..].iter().all(|&s| s == 0.0), "silent 5 ms after the fade began");
        assert!(rig.ctl.is_faded());
        // 4,800 frames rendered since: the fade's 480-frame callback, then
        // nine of silence.
        assert_eq!(rig.ctl.silent_ms(), 90);
        // The ring is left alone once silent: 480 frames taken by the fade's
        // callback, none after.
        use ringbuf::traits::Observer;
        assert_eq!(rig.voice.occupied_len(), 48_000 - 4_800 - 480);
    }
}
