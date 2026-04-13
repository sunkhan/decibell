use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use std::sync::Arc;
use arc_swap::ArcSwap;
use ringbuf::{HeapProd, HeapCons, traits::{Consumer, Producer}};
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
pub fn build_input_stream(
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
pub fn build_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    peers: PeerList,
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

    let peers_out = peers;
    let stream_cons_out = stream_cons;
    let pb_stream_stereo = stream_stereo;
    let out_ch = output_channels;

    // Per-sample mixing: pop one i16 from each peer's ring (lock held for the
    // whole callback), sum as i32, add stream audio, clamp, write.
    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            // Lock-free snapshot of the current peer list.
            let peer_snapshot = peers_out.load();
            // Try to lock each peer's consumer once per callback.
            // Peers whose lock is contended contribute silence for this callback.
            let mut voice_guards: Vec<std::sync::MutexGuard<HeapCons<i16>>> = peer_snapshot
                .iter()
                .filter_map(|p| p.cons.try_lock().ok())
                .collect();

            let Ok(mut stream_guard) = stream_cons_out.try_lock() else {
                drop(voice_guards);
                for sample in data.iter_mut() { *sample = 0.0; }
                return;
            };

            let pop_voice = |guards: &mut Vec<std::sync::MutexGuard<HeapCons<i16>>>| -> i32 {
                let mut sum: i32 = 0;
                for g in guards.iter_mut() {
                    if let Some(s) = g.try_pop() {
                        sum += s as i32;
                    }
                }
                sum
            };

            if out_ch == 1 {
                for sample in data.iter_mut() {
                    let v = pop_voice(&mut voice_guards);
                    let s = stream_guard.try_pop().unwrap_or(0) as i32;
                    let mixed = (v + s).clamp(-32768, 32767);
                    *sample = mixed as f32 / 32768.0;
                }
            } else {
                for frame in data.chunks_exact_mut(out_ch as usize) {
                    let v = pop_voice(&mut voice_guards);
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
pub fn build_voice_output_stream(
    host: &cpal::Host,
    device_name: Option<&str>,
    peers: PeerList,
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
    let peers_out = peers;

    let stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [f32], _: &cpal::OutputCallbackInfo| {
            let peer_snapshot = peers_out.load();
            let mut voice_guards: Vec<std::sync::MutexGuard<HeapCons<i16>>> = peer_snapshot
                .iter()
                .filter_map(|p| p.cons.try_lock().ok())
                .collect();

            for frame in data.chunks_exact_mut(out_ch as usize) {
                let mut sum: i32 = 0;
                for g in voice_guards.iter_mut() {
                    if let Some(s) = g.try_pop() {
                        sum += s as i32;
                    }
                }
                let v = sum.clamp(-32768, 32767) as f32 / 32768.0;
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
pub fn build_stream_output_stream(
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
