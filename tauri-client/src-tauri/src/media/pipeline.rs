use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::collections::HashMap;
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};
use ringbuf::{HeapRb, HeapProd, HeapCons, traits::{Consumer, Producer, Split, Observer}};
use rubato::{SincFixedOut, SincInterpolationParameters, SincInterpolationType, WindowFunction, Resampler};

use super::codec::{
    OpusDecoder, OpusEncoder, StereoOpusDecoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE,
    SAMPLE_RATE, STEREO_FRAME_SAMPLES, STEREO_FRAME_SIZE,
};
use super::packet::{
    UdpAudioPacket, AUDIO_HEADER_SIZE, PACKET_TOTAL_SIZE, PACKET_TYPE_AUDIO, PACKET_TYPE_PING,
    PACKET_TYPE_STREAM_AUDIO,
};
use super::speaking::SpeakingDetector;
use super::video_receiver::ReassembledFrame;

// ── Sinc resampler helper ────────────────────────────────────────────────────

fn make_sinc_resampler(from_rate: u32, to_rate: u32, chunk_size: usize, channels: usize) -> SincFixedOut<f64> {
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

// ── Control / Event messages ──────────────────────────────────────────────────

pub enum ControlMessage {
    SetMute(bool),
    SetDeafen(bool),
    SetVoiceThreshold(f32), // dB threshold (-60 to 0); below this, send silence
    SetStreamVolume(f32),   // 0.0 to 1.0 — viewer-side stream audio volume
    SetStreamStereo(bool),  // true = preserve L/R stereo in stream audio
    SetUserVolume(String, f32), // username, linear gain (dB-converted on frontend)
    SetInputDevice(Option<String>),  // None = system default
    SetOutputDevice(Option<String>), // None = system default
    /// (enabled, device_name) — when enabled, stream audio goes to a separate output device
    SetSeparateStreamOutput(bool, Option<String>),
    /// Change the stream output device (only effective when separate stream output is on)
    SetStreamOutputDevice(Option<String>),
    /// Voice processing toggles (AEC, NS, AGC)
    SetAecEnabled(bool),
    /// 0=off, 1=light(6dB), 2=moderate(12dB), 3=aggressive(18dB), 4=very aggressive(21dB)
    SetNoiseSuppressionLevel(u8),
    SetAgcEnabled(bool),
    Shutdown,
}

pub enum VoiceEvent {
    SpeakingChanged(String, bool),
    UserStateChanged(String, bool, bool), // username, muted, deafened
    /// Local microphone input level in dB (emitted ~every 50ms for the UI meter)
    InputLevel(f32),
    PingMeasured(u32),
    VideoFrameReady(ReassembledFrame),
    /// Linux only: H.264 frame decoded to JPEG in the video recv thread.
    /// (username, jpeg_bytes, frame_id, is_keyframe)
    #[cfg(target_os = "linux")]
    VideoFrameDecoded(String, Vec<u8>, u32, bool),
    KeyframeRequested,
    Error(String),
}

// Flags byte prepended to audio payload
const FLAG_MUTED: u8 = 0x01;
const FLAG_DEAFENED: u8 = 0x02;

// ── Jitter buffer ────────────────────────────────────────────────────────────
//
// Holds incoming packets for a short time before decoding, so that late/
// out-of-order packets can be reordered. When a packet is truly lost, the
// Opus decoder's PLC (Packet Loss Concealment) fills the gap smoothly.

const JITTER_DEPTH: usize = 3; // packets to buffer before starting playback (60ms)
const JITTER_MAX: usize = 30;  // safety cap — force-drain if buffer grows past this

struct JitterBuffer {
    packets: HashMap<u16, Vec<u8>>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
    /// Consecutive drain() calls that returned a missing packet (PLC).
    /// When this exceeds the threshold, the buffer resets to re-sync.
    consecutive_losses: u32,
}

impl JitterBuffer {
    fn new() -> Self {
        Self { packets: HashMap::new(), next_seq: 0, initialized: false, ready: false, consecutive_losses: 0 }
    }

    /// Insert a packet. Ignores packets behind the play cursor.
    fn push(&mut self, seq: u16, data: Vec<u8>) {
        if !self.initialized {
            self.next_seq = seq;
            self.initialized = true;
        }
        // Detect sequence reset (user left and rejoined): if the incoming seq
        // appears to be far behind next_seq, it's actually a fresh sequence
        // starting from 0. Reinitialize the buffer to accept the new stream.
        let diff = seq.wrapping_sub(self.next_seq);
        if diff >= 32768 {
            // seq is "behind" next_seq by more than half the u16 range —
            // this is a wraparound/reset, not a late packet.
            self.packets.clear();
            self.next_seq = seq;
            self.ready = false;
        }
        let diff = seq.wrapping_sub(self.next_seq);
        if diff < 32768 {
            self.packets.insert(seq, data);
        }
        if !self.ready && self.packets.len() >= JITTER_DEPTH {
            self.ready = true;
        }
        // Force-drain excess so the buffer can't grow unbounded.
        // If next_seq points to a gap (no packet), jump to the earliest actual
        // entry first — otherwise the while loop would spin through thousands
        // of empty sequence numbers before hitting a real packet.
        while self.packets.len() > JITTER_MAX {
            if !self.packets.contains_key(&self.next_seq) {
                if let Some(&earliest) = self.packets.keys()
                    .min_by_key(|&&s| s.wrapping_sub(self.next_seq))
                {
                    self.next_seq = earliest;
                } else {
                    break;
                }
            }
            self.packets.remove(&self.next_seq);
            self.next_seq = self.next_seq.wrapping_add(1);
        }
    }

    /// Pop the next frame. Returns:
    /// - `Some(Some(data))` — packet present, decode normally
    /// - `Some(None)` — packet missing, caller should do PLC
    /// - `None` — buffer not ready (initial fill or post-reset re-buffering)
    fn drain(&mut self) -> Option<Option<Vec<u8>>> {
        if !self.ready { return None; }

        // Auto-recovery: if we've produced 10+ consecutive PLC frames (200ms),
        // the audio is already unintelligible. Reset and re-buffer from scratch
        // so playback can resume cleanly once packets arrive.
        if self.consecutive_losses >= 10 {
            self.reset();
            return None;
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let result = self.packets.remove(&seq);
        if result.is_some() {
            self.consecutive_losses = 0;
        } else {
            self.consecutive_losses += 1;
        }
        Some(result)
    }

    /// Reset the buffer to its initial state, forcing a re-buffering period.
    /// Called automatically after prolonged packet loss.
    fn reset(&mut self) {
        self.packets.clear();
        self.initialized = false;
        self.ready = false;
        self.consecutive_losses = 0;
    }

    /// Peek at the next packet (next_seq) without consuming it.
    /// Used for FEC: when current packet is missing, check if the next
    /// packet is available to decode with fec=true.
    fn peek_next(&self) -> Option<&Vec<u8>> {
        self.packets.get(&self.next_seq)
    }
}

// ── Per-remote-peer state ─────────────────────────────────────────────────────

struct RemotePeer {
    decoder: OpusDecoder,
    speaking: SpeakingDetector,
    last_packet_time: Instant,
    voice_jitter: JitterBuffer,
    voice_drain_time: Instant,
    stream_audio_decoder: Option<StereoOpusDecoder>,
    stream_jitter: JitterBuffer,
    stream_drain_time: Instant,
    /// Decoded voice samples (f32, -1..1) waiting to be mixed with other peers.
    /// Accumulated during jitter drain, consumed by the mixing step each tick.
    decoded_voice: Vec<f32>,
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
fn get_default_device(host: &cpal::Host, input: bool) -> Option<cpal::Device> {
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
                    eprintln!("[pipeline] Using Windows communications device: {}", comms_name);
                    return Some(d);
                }
            }
            eprintln!("[pipeline] Communications device '{}' not found in CPAL, falling back to default", comms_name);
        }
    }

    if input {
        host.default_input_device()
    } else {
        host.default_output_device()
    }
}

// ── Input stream builder ─────────────────────────────────────────────────────

/// Build a CPAL input (capture) stream that pushes mono i16 samples at the
/// device's native sample rate into `capture_prod`.
/// Returns (stream, device_sample_rate) or None if no usable device is found.
///
/// The callback does NO resampling — just downmixes to mono and converts to i16.
/// Resampling from device rate → 48kHz happens in the main pipeline loop.
fn build_input_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    capture_prod: Arc<std::sync::Mutex<HeapProd<i16>>>,
) -> Option<(cpal::Stream, u32)> {
    let input_device = match device_name {
        Some(name) => {
            let found = host.input_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Input device '{}' not found, falling back to default", name);
                    get_default_device(host, true)?
                }
            }
        }
        None => get_default_device(host, true)?,
    };

    let (input_cfg, input_channels) = match input_device.default_input_config() {
        Ok(default_cfg) => {
            let rate = default_cfg.sample_rate();
            let channels = default_cfg.channels();
            eprintln!(
                "[pipeline] Input device: {}ch @ {}Hz (sample format: {:?})",
                channels, rate.0, default_cfg.sample_format()
            );
            (cpal::StreamConfig {
                channels,
                sample_rate: rate,
                buffer_size: cpal::BufferSize::Default,
            }, channels)
        }
        Err(_) => {
            (cpal::StreamConfig {
                channels: 1,
                sample_rate: cpal::SampleRate(SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Default,
            }, 1u16)
        }
    };

    let in_ch = input_channels;
    let input_sample_rate = input_cfg.sample_rate.0;
    let cap_prod = capture_prod;

    // The callback only does: downmix to mono + f32→i16. No resampling, no allocations.
    // Uses try_lock to never block the real-time audio thread.
    match input_device.build_input_stream(
        &input_cfg,
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let Ok(mut prod) = cap_prod.try_lock() else { return };
            if in_ch == 1 {
                for &s in data {
                    let _ = prod.try_push((s * 32767.0).clamp(-32768.0, 32767.0) as i16);
                }
            } else {
                for frame in data.chunks_exact(in_ch as usize) {
                    let sum: f32 = frame.iter().sum();
                    let mono = sum / in_ch as f32;
                    let _ = prod.try_push((mono * 32767.0).clamp(-32768.0, 32767.0) as i16);
                }
            }
        },
        |e| {
            eprintln!("[pipeline] capture stream error: {}", e);
        },
        None,
    ) {
        Ok(stream) => {
            if let Err(e) = stream.play() {
                eprintln!("[pipeline] failed to start capture stream: {}", e);
                None
            } else {
                eprintln!("[pipeline] Capture stream started: mono @ {}Hz (no callback resampling)", input_sample_rate);
                Some((stream, input_sample_rate))
            }
        }
        Err(e) => {
            eprintln!("[pipeline] build_input_stream failed: {}", e);
            None
        }
    }
}

// ── Output stream builder ────────────────────────────────────────────────────

/// Build a CPAL output (playback) stream that mixes voice + stream audio from
/// their respective ring buffer consumers. The ring buffers carry i16 samples
/// at the output device's native rate — all resampling happens in the main loop.
fn build_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    voice_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    _render_ref_prod: Arc<std::sync::Mutex<HeapProd<f32>>>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = match device_name {
        Some(name) => {
            let found = host.output_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Output device '{}' not found, falling back to default", name);
                    get_default_device(host, false)?
                }
            }
        }
        None => get_default_device(host, false)?,
    };

    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Output device: {}ch @ {}Hz (sample format: {:?})",
                cfg.channels, cfg.sample_rate.0, default_cfg.sample_format()
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            eprintln!("[pipeline] default_output_config failed ({}), trying 48kHz stereo", e);
            let cfg = cpal::StreamConfig {
                channels: 2,
                sample_rate: cpal::SampleRate(SAMPLE_RATE),
                buffer_size: cpal::BufferSize::Default,
            };
            (cfg, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;

    let voice_cons_out = voice_cons;
    let stream_cons_out = stream_cons;
    let pb_stream_stereo = stream_stereo;
    let out_ch = output_channels;

    // No resampling in the callback — ring buffers already carry samples at device rate.
    // Just read i16, convert to f32, mix voice+stream, write to device.
    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let Ok(mut voice_guard) = voice_cons_out.try_lock() else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };
            let Ok(mut stream_guard) = stream_cons_out.try_lock() else {
                drop(voice_guard);
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };

            if out_ch == 1 {
                for sample in data.iter_mut() {
                    let v = voice_guard.try_pop().unwrap_or(0) as i32;
                    let s = stream_guard.try_pop().unwrap_or(0) as i32;
                    let mixed = (v + s).clamp(-32768, 32767);
                    *sample = mixed as f32 / 32768.0;
                }
            } else {
                for frame in data.chunks_exact_mut(out_ch as usize) {
                    let v = voice_guard.try_pop().unwrap_or(0) as i32;
                    if pb_stream_stereo.load(std::sync::atomic::Ordering::Relaxed) && out_ch >= 2 {
                        let sl = stream_guard.try_pop().unwrap_or(0) as i32;
                        let sr = stream_guard.try_pop().unwrap_or(0) as i32;
                        let left = (v + sl).clamp(-32768, 32767) as f32 / 32768.0;
                        let right = (v + sr).clamp(-32768, 32767) as f32 / 32768.0;
                        frame[0] = left;
                        frame[1] = right;
                        for ch in &mut frame[2..] {
                            *ch = left;
                        }
                    } else {
                        let s = stream_guard.try_pop().unwrap_or(0) as i32;
                        let mixed = (v + s).clamp(-32768, 32767) as f32 / 32768.0;
                        for ch in frame.iter_mut() {
                            *ch = mixed;
                        }
                    }
                }
            }
        },
        |e| {
            eprintln!("[pipeline] playback stream error: {}", e);
        },
        None,
    ) {
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

    Some((stream, output_sample_rate, output_channels))
}

// ── Voice-only output stream builder ─────────────────────────────────────────

/// Build a CPAL output stream that plays only voice audio (no stream mixing).
/// Used when stream audio is routed to a separate device.
/// Ring buffer carries i16 at device native rate — no callback resampling.
fn build_voice_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    voice_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    _render_ref_prod: Arc<std::sync::Mutex<HeapProd<f32>>>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = match device_name {
        Some(name) => {
            let found = host.output_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Voice output device '{}' not found, falling back to default", name);
                    get_default_device(host, false)?
                }
            }
        }
        None => get_default_device(host, false)?,
    };

    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Voice output device: {}ch @ {}Hz",
                cfg.channels, cfg.sample_rate.0
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            eprintln!("[pipeline] voice output default_output_config failed ({}), trying 48kHz stereo", e);
            (cpal::StreamConfig { channels: 2, sample_rate: cpal::SampleRate(SAMPLE_RATE), buffer_size: cpal::BufferSize::Default }, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;
    let out_ch = output_channels;
    let voice_cons_out = voice_cons;

    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let Ok(mut guard) = voice_cons_out.try_lock() else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };
            for frame in data.chunks_exact_mut(out_ch as usize) {
                let v = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                for ch in frame.iter_mut() { *ch = v; }
            }
        },
        |e| eprintln!("[pipeline] voice output stream error: {}", e),
        None,
    ) {
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

    Some((stream, output_sample_rate, output_channels))
}

// ── Stream-only output stream builder ────────────────────────────────────────

/// Build a CPAL output stream that plays only stream audio (with stereo support).
/// Used when stream audio is routed to a separate device.
/// Ring buffer carries i16 at device native rate — no callback resampling.
fn build_stream_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    stream_cons: Arc<std::sync::Mutex<HeapCons<i16>>>,
    stream_stereo: Arc<std::sync::atomic::AtomicBool>,
    event_tx: &std::sync::mpsc::Sender<VoiceEvent>,
) -> Option<(cpal::Stream, u32, u16)> {
    let output_device = match device_name {
        Some(name) => {
            let found = host.output_devices().ok()?.find(|d| {
                d.name().map(|n| n == name).unwrap_or(false)
            });
            match found {
                Some(d) => d,
                None => {
                    eprintln!("[pipeline] Stream output device '{}' not found, falling back to default", name);
                    get_default_device(host, false)?
                }
            }
        }
        None => get_default_device(host, false)?,
    };

    let (stream_config, output_channels) = match output_device.default_output_config() {
        Ok(default_cfg) => {
            let cfg = cpal::StreamConfig {
                channels: default_cfg.channels(),
                sample_rate: default_cfg.sample_rate(),
                buffer_size: cpal::BufferSize::Default,
            };
            eprintln!(
                "[pipeline] Stream output device: {}ch @ {}Hz",
                cfg.channels, cfg.sample_rate.0
            );
            (cfg, default_cfg.channels())
        }
        Err(e) => {
            eprintln!("[pipeline] stream output default_output_config failed ({}), trying 48kHz stereo", e);
            (cpal::StreamConfig { channels: 2, sample_rate: cpal::SampleRate(SAMPLE_RATE), buffer_size: cpal::BufferSize::Default }, 2)
        }
    };
    let output_sample_rate = stream_config.sample_rate.0;
    let out_ch = output_channels;
    let stream_cons_out = stream_cons;
    let pb_stereo = stream_stereo;

    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let Ok(mut guard) = stream_cons_out.try_lock() else {
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };
            let is_stereo = pb_stereo.load(std::sync::atomic::Ordering::Relaxed);

            if out_ch == 1 {
                for sample in data.iter_mut() {
                    let s = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                    if is_stereo { let _ = guard.try_pop(); }
                    *sample = s;
                }
            } else {
                for frame in data.chunks_exact_mut(out_ch as usize) {
                    if is_stereo && out_ch >= 2 {
                        let sl = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                        let sr = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                        frame[0] = sl;
                        frame[1] = sr;
                        for ch in &mut frame[2..] { *ch = sl; }
                    } else {
                        let s = guard.try_pop().unwrap_or(0) as f32 / 32768.0;
                        for ch in frame.iter_mut() { *ch = s; }
                    }
                }
            }
        },
        |e| eprintln!("[pipeline] stream output error: {}", e),
        None,
    ) {
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

    Some((stream, output_sample_rate, output_channels))
}

// ── Main blocking pipeline entry-point ───────────────────────────────────────

/// Runs the audio pipeline on the calling thread (should be a dedicated OS thread).
/// The socket must already be connected to the server (bind + connect done by caller).
/// Video packets are forwarded to `video_packet_tx` for processing on a separate thread,
/// keeping the audio loop fast and preventing video reassembly from causing audio choppiness.
pub fn run_audio_pipeline(
    socket: Arc<UdpSocket>,
    sender_id: String,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // Socket timeout is set by the dedicated recv thread — not needed here.
    // The audio loop uses channel-based recv (non-blocking try_recv).

    // ── Opus encoder ──────────────────────────────────────────────────────────
    let encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!(
                "Opus encoder init failed: {}",
                e
            )));
            return;
        }
    };

    // ── CPAL host + devices ───────────────────────────────────────────────────
    let host = cpal::default_host();

    // ── Lock-free ring buffers ──────────────────────────────────────────────
    // SPSC ring buffers eliminate try_lock failures that caused silence pops.
    const BUF_CAP: usize = FRAME_SIZE * 48; // ~1s at 48kHz

    let capture_rb = HeapRb::<i16>::new(BUF_CAP);
    let (capture_prod, capture_cons) = capture_rb.split();
    let capture_prod = Arc::new(std::sync::Mutex::new(capture_prod));
    let capture_cons = Arc::new(std::sync::Mutex::new(capture_cons));

    let voice_rb = HeapRb::<i16>::new(BUF_CAP);
    let (voice_prod, voice_cons) = voice_rb.split();
    let voice_prod = Arc::new(std::sync::Mutex::new(voice_prod));
    let voice_cons = Arc::new(std::sync::Mutex::new(voice_cons));

    let stream_rb = HeapRb::<i16>::new(BUF_CAP);
    let (stream_prod, stream_cons) = stream_rb.split();
    let stream_prod = Arc::new(std::sync::Mutex::new(stream_prod));
    let stream_cons = Arc::new(std::sync::Mutex::new(stream_cons));

    // ── AEC render reference ring buffer ─────────────────────────────────────
    // Mono f32 at 48kHz — fed from the voice decode path in the main loop,
    // consumed by VoipAec3::handle_render_frame() before capture processing.
    let render_ref_rb = HeapRb::<f32>::new(BUF_CAP);
    let (render_ref_prod, render_ref_cons) = render_ref_rb.split();
    let render_ref_prod = Arc::new(std::sync::Mutex::new(render_ref_prod));
    let render_ref_cons = Arc::new(std::sync::Mutex::new(render_ref_cons));

    // ── Build output stream first (we need its sample rate for input matching) ─
    let stream_stereo = Arc::new(std::sync::atomic::AtomicBool::new(false));

    let (mut output_stream, mut output_sample_rate) = match build_output_stream(
        &host,
        None, // system default on startup
        Arc::clone(&voice_cons),
        Arc::clone(&stream_cons),
        Arc::clone(&stream_stereo),
        Arc::clone(&render_ref_prod),
        &event_tx,
    ) {
        Some((stream, rate, _ch)) => (Some(stream), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No audio output device found".to_string(),
            ));
            return;
        }
    };

    // ── Build input (capture) stream ──────────────────────────────────────────
    let (mut input_stream_opt, mut input_sample_rate): (Option<cpal::Stream>, u32) = match build_input_stream(
        &host,
        None, // system default on startup
        Arc::clone(&capture_prod),
    ) {
        Some((s, rate)) => (Some(s), rate),
        None => {
            let _ = event_tx.send(VoiceEvent::Error(
                "No microphone found — running in listen-only mode".to_string(),
            ));
            (None, SAMPLE_RATE)
        }
    };

    // ── Main-loop resamplers (all DSP off the audio callback threads) ─────────
    // Capture: input_device_rate → 48kHz (for Opus encoding)
    let mut capture_resampler: Option<SincFixedOut<f64>> = if input_sample_rate == SAMPLE_RATE {
        None
    } else {
        eprintln!("[pipeline] Capture resampler: {}Hz → {}Hz", input_sample_rate, SAMPLE_RATE);
        Some(make_sinc_resampler(input_sample_rate, SAMPLE_RATE, 480, 1))
    };
    let mut capture_accum: Vec<f64> = Vec::new();

    // Playback voice: 48kHz → output_device_rate (mono)
    let mut playback_voice_resampler: Option<SincFixedOut<f64>> = if output_sample_rate == SAMPLE_RATE {
        None
    } else {
        eprintln!("[pipeline] Playback voice resampler: {}Hz → {}Hz", SAMPLE_RATE, output_sample_rate);
        Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 1))
    };
    let mut playback_voice_accum: Vec<f64> = Vec::new();

    // Playback stream: 48kHz → output_device_rate (stereo)
    let mut playback_stream_resampler: Option<SincFixedOut<f64>> = if output_sample_rate == SAMPLE_RATE {
        None
    } else {
        eprintln!("[pipeline] Playback stream resampler: {}Hz → {}Hz (stereo)", SAMPLE_RATE, output_sample_rate);
        Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2))
    };
    let mut playback_stream_accum_l: Vec<f64> = Vec::new();
    let mut playback_stream_accum_r: Vec<f64> = Vec::new();

    // Separate stream output (None = disabled, stream mixed into main output)
    let mut stream_output: Option<cpal::Stream> = None;
    let mut separate_stream_enabled = false;
    let mut stream_output_device_name: Option<String> = None;
    // When separate stream output is enabled, stream audio may play on a device
    // with a different sample rate than the voice output. Track it separately.
    let mut stream_output_sample_rate: u32 = output_sample_rate;

    // ── Local state ───────────────────────────────────────────────────────────
    let mut muted = false;
    let mut deafened = false;
    let mut was_muted_before_deafen = false;
    let mut voice_threshold_db: f32 = -50.0; // dB threshold; below this, send silence
    let mut stream_volume: f32 = 1.0; // 0.0–1.0 viewer-side stream audio volume
    let mut user_volumes: HashMap<String, f32> = HashMap::new(); // username → linear gain

    let mut sequence: u16 = 0;
    let mut local_speaking = SpeakingDetector::new();
    let mut input_level_counter: u32 = 0; // throttle InputLevel events (~every 3 frames = 60ms)
    let mut remote_peers: HashMap<String, RemotePeer> = HashMap::new();

    // Accumulator for resampled 48kHz capture PCM — persists across loop iterations
    let mut capture_48k_buf: Vec<i16> = Vec::with_capacity(FRAME_SIZE * 4);

    let mut last_ping_time = Instant::now();
    let ping_interval = Duration::from_secs(3);

    // Reusable buffer for mixing decoded voice from all peers each tick
    let mut mix_buffer: Vec<f32> = Vec::with_capacity(FRAME_SIZE * 4);

    // ── Voice processing (AEC / NS / AGC) ──────────────────────────────────
    // VoipAec3 bundles all three processors. We rebuild it when toggles change.
    let mut aec_enabled = false;
    let mut ns_level: u8 = 0; // 0=off, 1=light, 2=moderate, 3=aggressive, 4=very aggressive
    let mut agc_enabled = false;
    let mut voice_processor: Option<aec3::voip::VoipAec3> = None;

    // RNNoise deep-learning noise suppressor — much better than WebRTC's spectral NS.
    // Created when ns_level > 0, processes 480-sample (10ms) frames at 48kHz.
    let mut rnnoise: Option<Box<nnnoiseless::DenoiseState<'static>>> = None;

    // Helper: (re)build the voice processor based on current toggle state.
    // WebRTC NS is disabled when RNNoise handles suppression (avoids double-filtering).
    fn build_voice_processor(aec: bool, ns_level: u8, agc: bool) -> Option<aec3::voip::VoipAec3> {
        // Only use WebRTC processor for AEC and/or AGC — RNNoise handles NS
        if !aec && !agc {
            return None;
        }
        // 48kHz mono, 10ms frames = 480 samples
        let builder = aec3::voip::VoipAec3::builder(48000, 1, 1)
            .enable_noise_suppression(false) // RNNoise handles this
            .enable_gain_controller2(agc);
        match builder.build() {
            Ok(processor) => {
                eprintln!("[pipeline] Voice processor built: aec={}, ns_level={}, agc={}", aec, ns_level, agc);
                Some(processor)
            }
            Err(e) => {
                eprintln!("[pipeline] Failed to build voice processor: {}", e);
                None
            }
        }
    }

    fn build_rnnoise(ns_level: u8) -> Option<Box<nnnoiseless::DenoiseState<'static>>> {
        if ns_level == 0 {
            return None;
        }
        eprintln!("[pipeline] RNNoise deep-learning noise suppressor enabled (level={})", ns_level);
        Some(nnnoiseless::DenoiseState::new())
    }

    // AEC render reference accumulator (10ms = 480 samples at 48kHz)
    const AEC_FRAME_SIZE: usize = 480; // 10ms at 48kHz
    let mut render_ref_accum: Vec<f32> = Vec::with_capacity(AEC_FRAME_SIZE * 2);

    // ── Dedicated voice UDP recv thread ─────────────────────────────────────
    // Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice socket
    // and forwards them to the audio processing thread. Video packets arrive
    // on a separate media socket handled by the video recv thread in mod.rs.
    let (audio_pkt_tx, audio_pkt_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    let recv_socket = Arc::clone(&socket);
    let recv_event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("decibell-voice-recv".to_string())
        .spawn(move || {
            voice_recv_thread(recv_socket, audio_pkt_tx, recv_event_tx);
        })
        .expect("spawn voice recv thread");

    // ── Main loop ─────────────────────────────────────────────────────────────
    'main: loop {
        let loop_start = Instant::now();

        // 1. Drain control messages ────────────────────────────────────────────
        loop {
            match control_rx.try_recv() {
                Ok(ControlMessage::Shutdown) => break 'main,
                Ok(ControlMessage::SetMute(m)) => {
                    if !deafened {
                        muted = m;
                    } else {
                        // While deafened, track what the user wants
                        was_muted_before_deafen = m;
                    }
                }
                Ok(ControlMessage::SetDeafen(d)) => {
                    if d && !deafened {
                        // Entering deafen: remember mute state, force mute
                        was_muted_before_deafen = muted;
                        deafened = true;
                        muted = true;
                    } else if !d && deafened {
                        // Leaving deafen: restore mute state
                        deafened = false;
                        muted = was_muted_before_deafen;
                    }
                }
                Ok(ControlMessage::SetVoiceThreshold(db)) => {
                    voice_threshold_db = db;
                }
                Ok(ControlMessage::SetStreamVolume(v)) => {
                    stream_volume = v.clamp(0.0, 1.0);
                }
                Ok(ControlMessage::SetStreamStereo(enabled)) => {
                    stream_stereo.store(enabled, std::sync::atomic::Ordering::Relaxed);
                }
                Ok(ControlMessage::SetUserVolume(username, gain)) => {
                    eprintln!("[pipeline] SetUserVolume: '{}' → gain={:.3}", username, gain);
                    user_volumes.insert(username, gain);
                }
                Ok(ControlMessage::SetInputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping input device to: {:?}", name);
                    input_stream_opt = None; // drop old stream
                    { let mut g = capture_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    capture_48k_buf.clear();
                    capture_accum.clear();
                    match build_input_stream(&host, name.as_deref(), Arc::clone(&capture_prod)) {
                        Some((stream, rate)) => {
                            input_stream_opt = Some(stream);
                            input_sample_rate = rate;
                            if rate == SAMPLE_RATE {
                                capture_resampler = None;
                                eprintln!("[pipeline] Input device {}Hz — passthrough", rate);
                            } else {
                                capture_resampler = Some(make_sinc_resampler(rate, SAMPLE_RATE, 480, 1));
                                eprintln!("[pipeline] Input device {}Hz — resampler to {}Hz", rate, SAMPLE_RATE);
                            }
                        }
                        None => {
                            input_stream_opt = None;
                            eprintln!("[pipeline] Warning: no input device after hot-swap");
                        }
                    }
                    // Reset voice processor state on device change
                    if voice_processor.is_some() {
                        voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                        render_ref_accum.clear();
                        if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                    }
                }
                Ok(ControlMessage::SetOutputDevice(name)) => {
                    eprintln!("[pipeline] Hot-swapping output device to: {:?}", name);
                    output_stream = None; // drop old stream
                    { let mut g = voice_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    playback_voice_accum.clear();
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
                    if separate_stream_enabled {
                        match build_voice_output_stream(&host, name.as_deref(), Arc::clone(&voice_cons), Arc::clone(&render_ref_prod), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => eprintln!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    } else {
                        match build_output_stream(&host, name.as_deref(), Arc::clone(&voice_cons), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), &event_tx) {
                            Some((stream, rate, _ch)) => {
                                output_sample_rate = rate;
                                output_stream = Some(stream);
                            }
                            None => eprintln!("[pipeline] Warning: no output device after hot-swap"),
                        }
                    }
                    // Rebuild playback resamplers for new output rate
                    if output_sample_rate == SAMPLE_RATE {
                        playback_voice_resampler = None;
                    } else {
                        playback_voice_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 1));
                    }
                    // Stream resampler: only update if stream plays on the same device
                    if !separate_stream_enabled {
                        stream_output_sample_rate = output_sample_rate;
                        if output_sample_rate == SAMPLE_RATE {
                            playback_stream_resampler = None;
                        } else {
                            playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 2));
                        }
                    }
                }
                Ok(ControlMessage::SetSeparateStreamOutput(enabled, device)) => {
                    eprintln!("[pipeline] Separate stream output: enabled={}, device={:?}", enabled, device);
                    separate_stream_enabled = enabled;
                    stream_output_device_name = device.clone();
                    // Rebuild main output and stream output
                    output_stream = None;
                    stream_output = None;
                    // Drain ring buffers to prevent stale audio causing delay
                    { let mut g = voice_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                    playback_voice_accum.clear();
                    playback_stream_accum_l.clear();
                    playback_stream_accum_r.clear();
                    if enabled {
                        // Main output: voice-only
                        if let Some((stream, rate, _ch)) = build_voice_output_stream(&host, None, Arc::clone(&voice_cons), Arc::clone(&render_ref_prod), &event_tx) {
                            output_sample_rate = rate;
                            output_stream = Some(stream);
                        }
                        // Stream output: stream-only on separate device (may have different rate)
                        if let Some((stream, rate, _ch)) = build_stream_output_stream(&host, device.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), &event_tx) {
                            stream_output_sample_rate = rate;
                            stream_output = Some(stream);
                        }
                    } else {
                        // Back to mixed mode — stream plays on same device as voice
                        if let Some((stream, rate, _ch)) = build_output_stream(&host, None, Arc::clone(&voice_cons), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), Arc::clone(&render_ref_prod), &event_tx) {
                            output_sample_rate = rate;
                            stream_output_sample_rate = rate;
                            output_stream = Some(stream);
                        }
                    }
                    // Rebuild playback resamplers — voice uses voice device rate, stream uses stream device rate
                    if output_sample_rate == SAMPLE_RATE {
                        playback_voice_resampler = None;
                    } else {
                        playback_voice_resampler = Some(make_sinc_resampler(SAMPLE_RATE, output_sample_rate, 480, 1));
                    }
                    if stream_output_sample_rate == SAMPLE_RATE {
                        playback_stream_resampler = None;
                    } else {
                        playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, stream_output_sample_rate, 480, 2));
                    }
                }
                Ok(ControlMessage::SetStreamOutputDevice(name)) => {
                    if separate_stream_enabled {
                        eprintln!("[pipeline] Hot-swapping stream output device to: {:?}", name);
                        stream_output_device_name = name.clone();
                        stream_output = None;
                        // Drain stream ring buffer
                        { let mut g = stream_cons.lock().unwrap(); while g.try_pop().is_some() {} }
                        playback_stream_accum_l.clear();
                        playback_stream_accum_r.clear();
                        if let Some((stream, rate, _ch)) = build_stream_output_stream(&host, name.as_deref(), Arc::clone(&stream_cons), Arc::clone(&stream_stereo), &event_tx) {
                            stream_output_sample_rate = rate;
                            stream_output = Some(stream);
                            // Rebuild stream resampler for the new device rate
                            if rate == SAMPLE_RATE {
                                playback_stream_resampler = None;
                            } else {
                                playback_stream_resampler = Some(make_sinc_resampler(SAMPLE_RATE, rate, 480, 2));
                            }
                        }
                    }
                }
                Ok(ControlMessage::SetAecEnabled(enabled)) => {
                    eprintln!("[pipeline] AEC enabled={}", enabled);
                    aec_enabled = enabled;
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Ok(ControlMessage::SetNoiseSuppressionLevel(level)) => {
                    eprintln!("[pipeline] NS level={}", level);
                    ns_level = level;
                    rnnoise = build_rnnoise(ns_level);
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Ok(ControlMessage::SetAgcEnabled(enabled)) => {
                    eprintln!("[pipeline] AGC enabled={}", enabled);
                    agc_enabled = enabled;
                    voice_processor = build_voice_processor(aec_enabled, ns_level, agc_enabled);
                    render_ref_accum.clear();
                    if let Ok(mut c) = render_ref_cons.lock() { while c.try_pop().is_some() {} }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // 1b. Feed AEC render reference from decoded voice audio ──────────────
        // Drain the render ref ring buffer and process 10ms chunks through AEC.
        if voice_processor.is_some() {
            if let Ok(mut rr_cons) = render_ref_cons.lock() {
                while let Some(s) = rr_cons.try_pop() {
                    render_ref_accum.push(s);
                }
            }
            // Feed complete 10ms render frames to AEC
            while render_ref_accum.len() >= AEC_FRAME_SIZE {
                let chunk: Vec<f32> = render_ref_accum.drain(..AEC_FRAME_SIZE).collect();
                if let Some(ref mut proc) = voice_processor {
                    let _ = proc.handle_render_frame(&chunk);
                }
            }
        }

        // 2. Capture & encode → send UDP ──────────────────────────────────────
        //
        // The capture ring buffer carries i16 at the input device's native rate.
        // We drain available samples, resample to 48kHz if needed, and accumulate
        // in capture_48k_buf. When we have a full 960-sample frame, encode + send.
        {
            // Drain all available samples from the capture ring buffer
            {
                let mut cons = capture_cons.lock().unwrap();
                let avail = cons.occupied_len();
                if avail > 0 {
                    if capture_resampler.is_none() {
                        // Input device is 48kHz — copy directly to the 48k buffer
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_48k_buf.push(s);
                            }
                        }
                    } else {
                        // Drain raw samples into the resampler accumulator
                        for _ in 0..avail {
                            if let Some(s) = cons.try_pop() {
                                capture_accum.push(s as f64 / 32768.0);
                            }
                        }
                    }
                }
            } // release capture_cons lock

            // Resample accumulated raw samples: input_rate → 48kHz
            if let Some(ref mut resampler) = capture_resampler {
                let mut needed = resampler.input_frames_next();
                while capture_accum.len() >= needed {
                    let chunk: Vec<f64> = capture_accum.drain(..needed).collect();
                    if let Ok(out) = resampler.process(&[&chunk], None) {
                        for &s in &out[0] {
                            capture_48k_buf.push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                        }
                    }
                    needed = resampler.input_frames_next();
                }
            }

            // Try to assemble a full Opus frame (960 samples at 48kHz = 20ms)
            let frame_opt: Option<[i16; FRAME_SIZE]> = if capture_48k_buf.len() >= FRAME_SIZE {
                let mut frame = [0i16; FRAME_SIZE];
                frame.copy_from_slice(&capture_48k_buf[..FRAME_SIZE]);
                capture_48k_buf.drain(..FRAME_SIZE);
                Some(frame)
            } else {
                None
            };

            if let Some(mut frame) = frame_opt {
                // ── Voice processing (AEC / NS / AGC) ────────────────────────
                // Process BEFORE threshold check so AGC-boosted levels are what
                // the threshold sees. Previous order caused AGC to boost quiet
                // audio after the gate had already decided to fade it out,
                // producing robotic popping artifacts.
                if !muted && (voice_processor.is_some() || rnnoise.is_some()) {
                    let mut f32_frame: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();

                    // Pass 1: WebRTC AEC + AGC (if enabled)
                    if let Some(ref mut proc) = voice_processor {
                        let mut output_buf = vec![0.0f32; AEC_FRAME_SIZE];
                        for chunk_idx in 0..2 {
                            let start = chunk_idx * AEC_FRAME_SIZE;
                            let end = start + AEC_FRAME_SIZE;
                            if let Ok(_) = proc.process_capture_frame(
                                &f32_frame[start..end],
                                false,
                                &mut output_buf,
                            ) {
                                f32_frame[start..end].copy_from_slice(&output_buf);
                            }
                        }
                    }

                    // Pass 2: RNNoise deep-learning noise suppression
                    if let Some(ref mut rnn) = rnnoise {
                        let mut rnn_in = [0.0f32; AEC_FRAME_SIZE];
                        let mut rnn_out = [0.0f32; AEC_FRAME_SIZE];
                        for chunk_idx in 0..2 {
                            let start = chunk_idx * AEC_FRAME_SIZE;
                            let end = start + AEC_FRAME_SIZE;
                            // RNNoise expects samples in [-32768, 32767] range
                            for (j, s) in f32_frame[start..end].iter().enumerate() {
                                rnn_in[j] = s * 32768.0;
                            }
                            let _vad = rnn.process_frame(&mut rnn_out, &rnn_in);
                            // Convert back to [-1, 1] range
                            for (j, s) in rnn_out.iter().enumerate() {
                                f32_frame[start + j] = s / 32768.0;
                            }
                        }
                    }

                    for (i, s) in f32_frame.iter().enumerate() {
                        frame[i] = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                    }
                }

                // Compute RMS in dB on the PROCESSED frame (post-AGC/NS)
                let rms = {
                    let sum_sq: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
                    (sum_sq / frame.len() as f64).sqrt() as f32
                };
                let rms_db = if rms > 0.0 {
                    20.0 * (rms / 32768.0).log10()
                } else {
                    -96.0
                };
                let above_threshold = !muted && rms_db >= voice_threshold_db;

                // Emit input level for the UI meter (~every 60ms)
                input_level_counter += 1;
                if input_level_counter >= 3 {
                    input_level_counter = 0;
                    let _ = event_tx.send(VoiceEvent::InputLevel(rms_db));
                }

                // Speaking detection based on threshold
                if let Some(state) = local_speaking.process_threshold(above_threshold) {
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), state));
                }

                // Encode: when muted or below threshold, send true silence so
                // nothing leaks to listeners. Opus DTX makes silence frames tiny.
                let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                let encode_result = if muted || !above_threshold {
                    encoder.encode_silence(&mut opus_out)
                } else {
                    encoder.encode(&frame, &mut opus_out)
                };

                match encode_result {
                    Ok(len) => {
                        // Prepend flags byte: muted | deafened
                        let flags = if muted { FLAG_MUTED } else { 0 }
                            | if deafened { FLAG_DEAFENED } else { 0 };
                        let mut flagged = [0u8; MAX_OPUS_FRAME_SIZE + 1];
                        flagged[0] = flags;
                        flagged[1..1 + len].copy_from_slice(&opus_out[..len]);
                        let packet =
                            UdpAudioPacket::new_audio(&sender_id, sequence, &flagged[..1 + len]);
                        let _ = socket.send(&packet.to_bytes());
                        sequence = sequence.wrapping_add(1);
                    }
                    Err(e) => {
                        let _ = event_tx.send(VoiceEvent::Error(format!("Encode error: {}", e)));
                    }
                }
            } else if input_stream_opt.is_none() {
                // No mic — still send silence to keep the UDP session alive
                let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
                if let Ok(len) = encoder.encode_silence(&mut opus_out) {
                    let flags = FLAG_MUTED; // no mic = effectively muted
                    let mut flagged = [0u8; MAX_OPUS_FRAME_SIZE + 1];
                    flagged[0] = flags;
                    flagged[1..1 + len].copy_from_slice(&opus_out[..len]);
                    let packet =
                        UdpAudioPacket::new_audio(&sender_id, sequence, &flagged[..1 + len]);
                    let _ = socket.send(&packet.to_bytes());
                    sequence = sequence.wrapping_add(1);
                }
            }
        }

        // 3. Send ping every 3s ────────────────────────────────────────────────
        if last_ping_time.elapsed() >= ping_interval {
            let ts_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let ping_pkt = UdpAudioPacket::new_ping(&sender_id, ts_ns);
            let _ = socket.send(&ping_pkt.to_bytes());
            last_ping_time = Instant::now();
        }

        // 4. Drain audio packets from recv thread ─────────────────────────────
        // The dedicated recv thread reads the UDP socket and dispatches packets
        // by type. We only see audio/ping/keyframe-request packets here — video
        // packets go directly to the video thread, never touching this loop.
        let mut pkt_count_this_iter = 0u32;
        loop {
            match audio_pkt_rx.try_recv() {
                Ok(raw) => {
                    pkt_count_this_iter += 1;
                    if raw.len() >= AUDIO_HEADER_SIZE {
                        if let Some(pkt) = UdpAudioPacket::from_bytes(&raw) {
                            let username = pkt.sender_username();

                            if pkt.packet_type == PACKET_TYPE_PING {
                                let payload = pkt.payload_data();
                                if payload.len() >= 8 {
                                    let sent_ns = u64::from_le_bytes(payload[..8].try_into().unwrap_or([0; 8]));
                                    let now_ns = std::time::SystemTime::now()
                                        .duration_since(std::time::UNIX_EPOCH)
                                        .unwrap_or_default()
                                        .as_nanos() as u64;
                                    let rtt_ms = (now_ns.saturating_sub(sent_ns) / 1_000_000) as u32;
                                    let _ = event_tx.send(VoiceEvent::PingMeasured(rtt_ms));
                                }
                            } else if username == sender_id {
                                // Ignore our own reflected audio packets
                            } else if pkt.packet_type == PACKET_TYPE_AUDIO {
                                let now = Instant::now();
                                let is_new = !remote_peers.contains_key(&username);
                                if is_new {
                                    eprintln!("[pipeline] New remote peer '{}' detected (type={}, seq={}, payload={}B)",
                                        username, pkt.packet_type, pkt.sequence, pkt.payload_size);
                                }
                                let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                    RemotePeer {
                                        decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                            OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                        }),
                                        speaking: SpeakingDetector::new(),
                                        last_packet_time: now,
                                        voice_jitter: JitterBuffer::new(),
                                        voice_drain_time: now,
                                        stream_audio_decoder: None,
                                        stream_jitter: JitterBuffer::new(),
                                        stream_drain_time: now,
                                        decoded_voice: Vec::new(),
                                    }
                                });
                                peer.last_packet_time = now;

                                let raw_payload = pkt.payload_data();
                                let (flags, opus_data) = if raw_payload.len() > 1 {
                                    (raw_payload[0], &raw_payload[1..])
                                } else {
                                    (0u8, raw_payload)
                                };
                                let peer_muted = flags & FLAG_MUTED != 0;
                                let peer_deafened = flags & FLAG_DEAFENED != 0;
                                let _ = event_tx.send(VoiceEvent::UserStateChanged(
                                    username.clone(), peer_muted, peer_deafened,
                                ));

                                peer.voice_jitter.push(pkt.sequence, opus_data.to_vec());
                            } else if pkt.packet_type == PACKET_TYPE_STREAM_AUDIO {
                                if username != sender_id {
                                    let now = Instant::now();
                                    let peer = remote_peers.entry(username.clone()).or_insert_with(|| {
                                        RemotePeer {
                                            decoder: OpusDecoder::new().unwrap_or_else(|_| {
                                                OpusDecoder::new().expect("OpusDecoder::new failed twice")
                                            }),
                                            speaking: SpeakingDetector::new(),
                                            last_packet_time: now,
                                            voice_jitter: JitterBuffer::new(),
                                            voice_drain_time: now,
                                            stream_audio_decoder: None,
                                            stream_jitter: JitterBuffer::new(),
                                            stream_drain_time: now,
                                            decoded_voice: Vec::new(),
                                        }
                                    });
                                    peer.last_packet_time = now;

                                    if peer.stream_audio_decoder.is_none() {
                                        peer.stream_audio_decoder = StereoOpusDecoder::new().ok();
                                    }

                                    peer.stream_jitter.push(pkt.sequence, pkt.payload_data().to_vec());
                                }
                            }
                        }
                    }
                }
                Err(std::sync::mpsc::TryRecvError::Empty) => break,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => break 'main,
            }
        }

        // 4b. Drain jitter buffers → decode → push to playback ──────────────
        let drain_now = Instant::now();
        let frame_dur = Duration::from_millis(20);
        // Cap how far behind drain times can fall. Without this, a peer whose
        // jitter buffer empties (e.g. network stutter) accumulates a large
        // time debt, and when packets resume the loop fires dozens of PLC
        // frames in a single burst — audible as a glitch.
        let max_behind = Duration::from_millis(100);
        for (username, peer) in remote_peers.iter_mut() {
            if drain_now.duration_since(peer.voice_drain_time) > max_behind {
                peer.voice_drain_time = drain_now - max_behind;
            }
            if drain_now.duration_since(peer.stream_drain_time) > max_behind {
                peer.stream_drain_time = drain_now - max_behind;
            }
            // ── Voice jitter buffer ──
            while drain_now.duration_since(peer.voice_drain_time) >= frame_dur {
                peer.voice_drain_time += frame_dur;
                let opus_opt = match peer.voice_jitter.drain() {
                    Some(v) => v,
                    None => {
                        // Buffer not ready (initial fill or auto-recovery reset).
                        if peer.voice_drain_time != drain_now {
                            eprintln!("[pipeline] Jitter buffer reset for peer '{}' — re-buffering", username);
                        }
                        peer.voice_drain_time = drain_now;
                        peer.decoded_voice.clear();
                        break;
                    }
                };
                let mut pcm = [0i16; FRAME_SIZE];
                let decode_ok = match &opus_opt {
                    Some(data) => peer.decoder.decode(data, &mut pcm).is_ok(),
                    None => {
                        // Packet lost — try FEC recovery using the next packet
                        if let Some(next_data) = peer.voice_jitter.peek_next() {
                            peer.decoder.decode_fec(next_data, &mut pcm).is_ok()
                        } else {
                            // No next packet available — fall back to basic PLC
                            peer.decoder.decode(&[], &mut pcm).is_ok()
                        }
                    }
                };
                if decode_ok {
                    let rms = {
                        let sum_sq: f64 = pcm.iter().map(|&s| (s as f64) * (s as f64)).sum();
                        (sum_sq / pcm.len() as f64).sqrt() as f32
                    };
                    let rms_db = if rms > 0.0 { 20.0 * (rms / 32768.0).log10() } else { -96.0 };
                    if let Some(state) = peer.speaking.process_threshold(rms_db >= -50.0) {
                        let _ = event_tx.send(VoiceEvent::SpeakingChanged(username.clone(), state));
                    }
                    if !deafened {
                        let gain = user_volumes.get(username.as_str()).copied().unwrap_or(1.0);
                        for &s in &pcm {
                            peer.decoded_voice.push((s as f32 / 32768.0) * gain);
                        }
                    }
                }
            }

            // ── Stream audio jitter buffer ──
            if let Some(ref mut decoder) = peer.stream_audio_decoder {
                while drain_now.duration_since(peer.stream_drain_time) >= frame_dur {
                    peer.stream_drain_time += frame_dur;
                    let opus_opt = match peer.stream_jitter.drain() {
                        Some(v) => v,
                        None => {
                            peer.stream_drain_time = drain_now;
                            break;
                        }
                    };
                    let mut pcm = [0i16; STEREO_FRAME_SAMPLES];
                    let decode_ok = match &opus_opt {
                        Some(data) => decoder.decode(data, &mut pcm).is_ok(),
                        None => decoder.decode(&[], &mut pcm).is_ok(), // PLC
                    };
                    if decode_ok {
                        if playback_stream_resampler.is_none() {
                            // Output device is 48kHz — push directly
                            if let Ok(mut prod) = stream_prod.lock() {
                                for i in 0..STEREO_FRAME_SIZE {
                                    let l = pcm[i * 2] as i32;
                                    let r = pcm[i * 2 + 1] as i32;
                                    if stream_stereo.load(std::sync::atomic::Ordering::Relaxed) {
                                        let sl = ((l as f32) * stream_volume) as i32;
                                        let sr = ((r as f32) * stream_volume) as i32;
                                        let _ = prod.try_push(sl.clamp(-32768, 32767) as i16);
                                        let _ = prod.try_push(sr.clamp(-32768, 32767) as i16);
                                    } else {
                                        let mono = (l + r) / 2;
                                        let scaled = ((mono as f32) * stream_volume) as i32;
                                        let _ = prod.try_push(scaled.clamp(-32768, 32767) as i16);
                                    }
                                }
                            }
                        } else if let Some(ref mut resampler) = playback_stream_resampler {
                            // Resample stereo 48kHz → output device rate, then push
                            for i in 0..STEREO_FRAME_SIZE {
                                let l = pcm[i * 2] as f32 * stream_volume / 32768.0;
                                let r = pcm[i * 2 + 1] as f32 * stream_volume / 32768.0;
                                playback_stream_accum_l.push(l as f64);
                                playback_stream_accum_r.push(r as f64);
                            }
                            let mut needed = resampler.input_frames_next();
                            while playback_stream_accum_l.len() >= needed && playback_stream_accum_r.len() >= needed {
                                let cl: Vec<f64> = playback_stream_accum_l.drain(..needed).collect();
                                let cr: Vec<f64> = playback_stream_accum_r.drain(..needed).collect();
                                if let Ok(out) = resampler.process(&[&cl, &cr], None) {
                                    if let Ok(mut prod) = stream_prod.lock() {
                                        let is_stereo = stream_stereo.load(std::sync::atomic::Ordering::Relaxed);
                                        let len = out[0].len().min(out[1].len());
                                        for i in 0..len {
                                            if is_stereo {
                                                let _ = prod.try_push((out[0][i] * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                                let _ = prod.try_push((out[1][i] * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                            } else {
                                                let mono = (out[0][i] + out[1][i]) / 2.0;
                                                let _ = prod.try_push((mono * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                            }
                                        }
                                    }
                                }
                                needed = resampler.input_frames_next();
                            }
                        }
                        // Feed stream audio to AEC render reference (mono, with volume applied)
                        if aec_enabled {
                            if let Ok(mut rr_prod) = render_ref_prod.lock() {
                                for i in 0..STEREO_FRAME_SIZE {
                                    let l = pcm[i * 2] as f32;
                                    let r = pcm[i * 2 + 1] as f32;
                                    let mono = ((l + r) / 2.0) * stream_volume / 32768.0;
                                    let _ = rr_prod.try_push(mono);
                                }
                            }
                        }
                    }
                }
            }
        }

        // 4c. Mix decoded voice from all peers → resample → push to playback buffer
        // Each peer's jitter drain accumulated f32 samples in decoded_voice.
        // Sum them sample-by-sample, then resample from 48kHz to the output device
        // rate before pushing to the voice ring buffer.
        {
            let max_samples = remote_peers.values().map(|p| p.decoded_voice.len()).max().unwrap_or(0);
            if max_samples > 0 {
                mix_buffer.clear();
                mix_buffer.resize(max_samples, 0.0);
                for peer in remote_peers.values() {
                    for (i, &s) in peer.decoded_voice.iter().enumerate() {
                        mix_buffer[i] += s;
                    }
                }

                // Feed mixed voice to AEC render reference (what the speaker actually plays)
                if aec_enabled {
                    if let Ok(mut rr_prod) = render_ref_prod.lock() {
                        for &s in &mix_buffer {
                            let _ = rr_prod.try_push(s);
                        }
                    }
                }

                // Resample 48kHz → output device rate, then push to playback ring buffer
                if playback_voice_resampler.is_none() {
                    if let Ok(mut prod) = voice_prod.lock() {
                        for &s in &mix_buffer {
                            let _ = prod.try_push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                        }
                    }
                } else if let Some(ref mut resampler) = playback_voice_resampler {
                    for &s in &mix_buffer {
                        playback_voice_accum.push(s as f64);
                    }
                    let mut needed = resampler.input_frames_next();
                    while playback_voice_accum.len() >= needed {
                        let chunk: Vec<f64> = playback_voice_accum.drain(..needed).collect();
                        if let Ok(out) = resampler.process(&[&chunk], None) {
                            if let Ok(mut prod) = voice_prod.lock() {
                                for &s in &out[0] {
                                    let _ = prod.try_push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                                }
                            }
                        }
                        needed = resampler.input_frames_next();
                    }
                }

                // Clear per-peer buffers
                for peer in remote_peers.values_mut() {
                    peer.decoded_voice.clear();
                }
            }
        }

        // 5. Clean up stale remote peers (no packet for > 5s) ─────────────────
        let stale_timeout = Duration::from_secs(5);
        let mut to_remove: Vec<String> = Vec::new();
        for (name, peer) in &remote_peers {
            if peer.last_packet_time.elapsed() > stale_timeout {
                to_remove.push(name.clone());
            }
        }
        for name in to_remove {
            if let Some(mut peer) = remote_peers.remove(&name) {
                // Emit speaking-stopped if they were still marked speaking
                if peer.speaking.is_speaking() {
                    peer.speaking.reset();
                    let _ =
                        event_tx.send(VoiceEvent::SpeakingChanged(name, false));
                }
            }
        }

        // 6. Periodic diagnostics (every 5s) ────────────────────────────────
        static DIAG_COUNTER: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
        let iter_num = DIAG_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        if iter_num % 1000 == 999 { // ~every 5s (5ms per iter)
            let voice_fill = if let Ok(c) = voice_cons.try_lock() { c.occupied_len() } else { 0 };
            let stream_fill = if let Ok(c) = stream_cons.try_lock() { c.occupied_len() } else { 0 };
            let peers: Vec<String> = remote_peers.keys().cloned().collect();
            eprintln!("[pipeline] diag: peers={:?}, voice_buf={}/{}, stream_buf={}/{}, pkts_this_iter={}",
                peers, voice_fill, BUF_CAP, stream_fill, BUF_CAP, pkt_count_this_iter);
        }

        // 7. Sleep if loop finished under 5ms ──────────────────────────────────
        let elapsed = loop_start.elapsed();
        let target = Duration::from_millis(5);
        if elapsed < target {
            std::thread::sleep(target - elapsed);
        }
    }

    // Streams are dropped here, which stops CPAL.
    // Dropping audio_pkt_rx disconnects the channel, causing the recv thread to exit.
    drop(audio_pkt_rx);
    drop(stream_output);    // Option<cpal::Stream> — separate stream output
    drop(output_stream);    // Option<cpal::Stream>
    drop(input_stream_opt); // Option<cpal::Stream>
}

// ── Dedicated UDP receive thread ─────────────────────────────────────────────
//
// Reads all incoming packets from the shared UDP socket and dispatches them
// by type into separate channels. This thread does NO processing — it just
// classifies and forwards, keeping the socket buffer drained at all times.
//
// Architecture (matching Discord's model):
//   recv thread → audio channel → audio processing thread
//                → video channel → video reassembly thread

// ── Dedicated voice UDP receive thread ──────────────────────────────────────
//
// Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice UDP socket
// and forwards them to the audio processing thread. Video packets arrive on
// a separate media socket handled by the video recv thread in mod.rs.

fn voice_recv_thread(
    socket: Arc<UdpSocket>,
    audio_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    const RECV_BUF_SIZE: usize = PACKET_TOTAL_SIZE;

    if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(1))) {
        let _ = event_tx.send(VoiceEvent::Error(format!("voice recv thread: set_read_timeout: {}", e)));
        return;
    }

    let mut buf = [0u8; RECV_BUF_SIZE];
    let mut recv_count: u64 = 0;
    let mut recv_log_time = Instant::now();

    loop {
        match socket.recv(&mut buf) {
            Ok(n) if n >= 1 => {
                recv_count += 1;
                if recv_log_time.elapsed() >= Duration::from_secs(5) {
                    eprintln!("[voice-recv] 5s stats: packets={}", recv_count);
                    recv_count = 0;
                    recv_log_time = Instant::now();
                }

                // Forward all packets on the voice socket to the audio thread.
                // No type classification needed — only voice packets arrive here.
                match audio_tx.try_send(buf[..n].to_vec()) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        // Audio thread is behind — drop this packet.
                        // The jitter buffer's PLC will smooth the gap.
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::ConnectionReset
                    || e.raw_os_error() == Some(997)
                    || e.raw_os_error() == Some(10054)
            => {}
            Err(e) => {
                eprintln!("[voice-recv] Socket error: {}", e);
                break;
            }
        }
    }

    eprintln!("[voice-recv] Exiting");
}
