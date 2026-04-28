use std::sync::atomic::Ordering;
use std::sync::Arc;
use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;
use tauri::{AppHandle, Emitter, State};

use crate::config;
use crate::state::SharedState;

#[derive(Debug, Clone, Serialize)]
pub struct AudioDevice {
    pub name: String,
    pub label: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct AudioDeviceList {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
}

#[tauri::command]
pub async fn load_config(
    app: AppHandle,
) -> Result<config::LoadedConfig, String> {
    config::load(&app)
}

#[tauri::command]
pub async fn save_settings(
    settings: config::AppSettings,
    app: AppHandle,
) -> Result<(), String> {
    config::save(&app, None, &settings)
}

#[tauri::command]
pub async fn list_audio_devices() -> Result<AudioDeviceList, String> {
    #[cfg(target_os = "linux")]
    {
        return list_audio_devices_pactl();
    }

    #[cfg(not(target_os = "linux"))]
    {
        let host = cpal::default_host();

        let inputs: Vec<AudioDevice> = host
            .input_devices()
            .map_err(|e| format!("Failed to list input devices: {}", e))?
            .filter_map(|d| d.name().ok().map(|name| AudioDevice { name: name.clone(), label: name }))
            .collect();

        let outputs: Vec<AudioDevice> = host
            .output_devices()
            .map_err(|e| format!("Failed to list output devices: {}", e))?
            .filter_map(|d| d.name().ok().map(|name| AudioDevice { name: name.clone(), label: name }))
            .collect();

        Ok(AudioDeviceList { inputs, outputs })
    }
}

/// On Linux, enumerate audio devices via `pactl` to get PipeWire/PulseAudio devices
/// with friendly descriptions — same list as Chromium-based apps like Vesktop.
#[cfg(target_os = "linux")]
fn list_audio_devices_pactl() -> Result<AudioDeviceList, String> {
    fn parse_pactl_devices(kind: &str) -> Vec<AudioDevice> {
        // kind = "sinks" or "sources"
        let output = match std::process::Command::new("pactl")
            .args(["list", kind])
            .output()
        {
            Ok(o) => String::from_utf8_lossy(&o.stdout).to_string(),
            Err(_) => return Vec::new(),
        };

        let mut devices = Vec::new();
        let mut current_name: Option<String> = None;
        let mut current_desc: Option<String> = None;

        for line in output.lines() {
            let trimmed = line.trim();
            if trimmed.starts_with("Name: ") {
                // Save previous device if complete
                if let (Some(name), Some(desc)) = (current_name.take(), current_desc.take()) {
                    devices.push(AudioDevice { name, label: desc });
                }
                current_name = Some(trimmed.strip_prefix("Name: ").unwrap().to_string());
                current_desc = None;
            } else if trimmed.starts_with("Description: ") {
                current_desc = Some(trimmed.strip_prefix("Description: ").unwrap().to_string());
            }
        }
        // Don't forget the last device
        if let (Some(name), Some(desc)) = (current_name, current_desc) {
            devices.push(AudioDevice { name, label: desc });
        }

        devices
    }

    let outputs = parse_pactl_devices("sinks");
    let inputs: Vec<AudioDevice> = parse_pactl_devices("sources")
        .into_iter()
        .filter(|d| !d.name.ends_with(".monitor")) // skip loopback monitors
        .collect();

    Ok(AudioDeviceList { inputs, outputs })
}

#[tauri::command]
pub async fn set_input_device(
    name: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let current = config::load(&app)?;
    let mut settings = current.settings;
    settings.input_device = name.clone();
    config::save(&app, None, &settings)?;

    #[cfg(target_os = "linux")]
    {
        // Route only *our* capture streams to the selected source. Don't
        // change the system-wide default — that would move every other app too.
        if let Some(ref pa_name) = name {
            crate::audio_routing::route_inputs_to(pa_name);
        }
        let _ = state;
        return Ok(());
    }

    #[cfg(not(target_os = "linux"))]
    {
        let s = state.lock().await;
        if let Some(ref engine) = s.voice_engine {
            engine.set_input_device(name);
        }
        Ok(())
    }
}

#[tauri::command]
pub async fn set_output_device(
    name: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let current = config::load(&app)?;
    let mut settings = current.settings;
    settings.output_device = name.clone();
    config::save(&app, None, &settings)?;

    #[cfg(target_os = "linux")]
    {
        if let Some(ref pa_name) = name {
            crate::audio_routing::route_outputs_to(pa_name);
        }
        let _ = state;
        return Ok(());
    }

    #[cfg(not(target_os = "linux"))]
    {
        let s = state.lock().await;
        if let Some(ref engine) = s.voice_engine {
            engine.set_output_device(name);
        }
        Ok(())
    }
}

#[tauri::command]
pub async fn set_separate_stream_output(
    enabled: bool,
    device: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let current = config::load(&app)?;
    let mut settings = current.settings;
    settings.separate_stream_output = enabled;
    settings.stream_output_device = device.clone();
    config::save(&app, None, &settings)?;

    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        // On Linux, stream output device selection uses pactl — pass None to engine
        #[cfg(target_os = "linux")]
        engine.set_separate_stream_output(enabled, None);
        #[cfg(not(target_os = "linux"))]
        engine.set_separate_stream_output(enabled, device);
    }
    Ok(())
}

#[tauri::command]
pub async fn set_stream_output_device(
    name: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let current = config::load(&app)?;
    let mut settings = current.settings;
    settings.stream_output_device = name.clone();
    config::save(&app, None, &settings)?;

    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        // On Linux, can't route separate stream output via pactl easily — use default
        #[cfg(target_os = "linux")]
        { let _ = name; engine.set_stream_output_device(None); }
        #[cfg(not(target_os = "linux"))]
        engine.set_stream_output_device(name);
    }
    Ok(())
}

/// Start a temporary mic capture for the settings UI level meter.
/// Only starts if not already in a voice channel (the pipeline emits levels itself).
/// Runs on a dedicated thread since cpal::Stream is not Send.
#[tauri::command]
pub async fn start_mic_test(
    device_name: Option<String>,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // Don't start if voice engine is active (it already emits levels)
    if s.voice_engine.is_some() {
        return Ok(());
    }

    // Stop any existing test
    if let Some(ref stop) = s.mic_test_stop {
        stop.store(true, Ordering::Relaxed);
    }

    let stop_flag = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_flag_thread = stop_flag.clone();
    s.mic_test_stop = Some(stop_flag);

    // On Linux, device names are PulseAudio names — cpal sees only ALSA
    // device names, so open the default and move the stream via pactl.
    #[cfg(target_os = "linux")]
    let pa_source: Option<String> = device_name.clone();
    #[cfg(target_os = "linux")]
    let device_name: Option<String> = None;

    std::thread::Builder::new()
        .name("decibell-mic-test".to_string())
        .spawn(move || {
            if let Err(e) = run_mic_test(device_name, app, stop_flag_thread) {
                eprintln!("[mic-test] Error: {}", e);
            }
        })
        .map_err(|e| format!("Spawn mic test thread: {}", e))?;

    #[cfg(target_os = "linux")]
    if let Some(name) = pa_source {
        // Give the stream a moment to appear in pactl, then route it.
        std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(150));
            crate::audio_routing::route_inputs_to(&name);
        });
    }

    Ok(())
}

fn run_mic_test(
    device_name: Option<String>,
    app: AppHandle,
    stop_flag: Arc<std::sync::atomic::AtomicBool>,
) -> Result<(), String> {
    use cpal::traits::StreamTrait;

    let host = cpal::default_host();
    let device = if let Some(ref name) = device_name {
        host.input_devices()
            .map_err(|e| format!("Input devices: {}", e))?
            .find(|d| d.name().ok().as_deref() == Some(name.as_str()))
            .ok_or_else(|| format!("Input device '{}' not found", name))?
    } else {
        host.default_input_device()
            .ok_or("No default input device")?
    };

    let config = device.default_input_config()
        .map_err(|e| format!("Default input config: {}", e))?;
    let channels = config.channels() as usize;

    let (level_tx, level_rx) = std::sync::mpsc::sync_channel::<f32>(4);

    let stream = device.build_input_stream(
        &config.into(),
        move |data: &[f32], _: &cpal::InputCallbackInfo| {
            let mut sum_sq: f64 = 0.0;
            let mut count = 0usize;
            if channels == 1 {
                for &s in data {
                    sum_sq += (s as f64) * (s as f64);
                    count += 1;
                }
            } else {
                for chunk in data.chunks(channels) {
                    let mono: f64 = chunk.iter().map(|&s| s as f64).sum::<f64>() / channels as f64;
                    sum_sq += mono * mono;
                    count += 1;
                }
            }
            if count > 0 {
                let rms = (sum_sq / count as f64).sqrt() as f32;
                let rms_db = if rms > 0.0 { 20.0 * rms.log10() } else { -96.0 };
                let _ = level_tx.try_send(rms_db);
            }
        },
        |err| eprintln!("[mic-test] Stream error: {}", err),
        None,
    ).map_err(|e| format!("Build input stream: {}", e))?;

    stream.play().map_err(|e| format!("Play: {}", e))?;
    eprintln!("[mic-test] Started");

    // Emit levels until stopped
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            break;
        }
        match level_rx.recv_timeout(std::time::Duration::from_millis(60)) {
            Ok(db) => {
                // Drain to keep latest
                let mut latest = db;
                while let Ok(d) = level_rx.try_recv() { latest = d; }
                let _ = app.emit("voice_input_level", serde_json::json!({ "db": latest }));
            }
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        }
    }

    drop(stream);
    eprintln!("[mic-test] Stopped");
    Ok(())
}

/// Stop the temporary mic test capture.
#[tauri::command]
pub async fn stop_mic_test(
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    if let Some(ref stop) = s.mic_test_stop {
        stop.store(true, Ordering::Relaxed);
    }
    s.mic_test_stop = None;
    Ok(())
}

// ──────────────────────────────────────────────────────────────────────
// Codec capability commands (Plan A Group 5)
// ──────────────────────────────────────────────────────────────────────

use crate::media::caps::{self, CodecCap, CodecKind};
use crate::net::connection::build_packet;
use crate::net::proto::{packet, UpdateCapabilitiesRequest};

#[derive(Debug, Clone, Serialize)]
pub struct CapsResponse {
    pub encode: Vec<CodecCap>,
    pub decode: Vec<CodecCap>,
}

/// Returns the merged ClientCapabilities the next JoinVoiceRequest would
/// send. React uses this to render the Codecs settings panel summary.
///
/// Lock discipline: snapshot the toggles + decoder caps under the AppState
/// lock briefly, then run the (potentially-slow) probe outside the lock.
#[tauri::command]
pub async fn get_caps(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<CapsResponse, String> {
    // Read toggles from persisted settings (no AppState lock needed).
    let settings = config::load(&app)?.settings;
    let use_av1 = settings.use_av1;
    let use_h265 = settings.use_h265;

    // Read decoder caps under the AppState lock, then drop the guard.
    let decoder_caps = {
        let s = state.lock().await;
        s.decoder_caps.clone()
    };

    // Probe (cached) outside any lock — first call may take ~hundreds of ms.
    let encode = caps::get_or_probe_encoders(&app);
    let filtered_encode: Vec<CodecCap> = encode
        .into_iter()
        .filter(|c| match c.codec {
            CodecKind::Av1 => use_av1,
            CodecKind::H265 => use_h265,
            _ => true,
        })
        .collect();

    Ok(CapsResponse { encode: filtered_encode, decode: decoder_caps })
}

/// Re-runs the encoder probe (force, ignoring cache) and returns the new
/// merged caps. Settings → "Refresh codec capabilities" button.
///
/// Also broadcasts UpdateCapabilitiesRequest to every connected community
/// server so peers see the new caps without a leave/rejoin (spec §4.6).
#[tauri::command]
pub async fn refresh_caps(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<CapsResponse, String> {
    caps::refresh_encoders(&app);
    let resp = get_caps(app.clone(), state.clone()).await?;
    broadcast_update_capabilities(&state, &resp).await;
    Ok(resp)
}

/// React calls this at app boot (and again after a refresh) with the
/// WebCodecs decoder probe result.
#[tauri::command]
pub async fn set_decoder_caps(
    decoder_caps: Vec<CodecCap>,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    s.decoder_caps = decoder_caps;
    Ok(())
}

/// Linux-only: probe what the Rust ffmpeg decoder can actually open on
/// this machine. JS calls this when WebCodecs is unavailable (WebKitGTK)
/// instead of returning an empty/H.264-only cap set, so the LCD picker
/// can converge on AV1 / HEVC when the Linux watcher's GPU supports them.
/// On non-Linux this returns an empty Vec — JS shouldn't call it there.
#[tauri::command]
pub async fn probe_decoders_native() -> Result<Vec<CodecCap>, String> {
    #[cfg(target_os = "linux")]
    {
        Ok(crate::media::video_decoder::probe_decoders())
    }
    #[cfg(not(target_os = "linux"))]
    {
        Ok(Vec::new())
    }
}

#[derive(Debug, Clone, Serialize, serde::Deserialize)]
pub struct CodecSettingsPayload {
    pub use_av1: bool,
    pub use_h265: bool,
}

/// Read the codec preference toggles. Lives in AppSettings (config.json),
/// not AppState — avoids a sync problem and keeps AppState lean.
#[tauri::command]
pub async fn get_codec_settings(
    app: AppHandle,
) -> Result<CodecSettingsPayload, String> {
    let settings = config::load(&app)?.settings;
    Ok(CodecSettingsPayload {
        use_av1: settings.use_av1,
        use_h265: settings.use_h265,
    })
}

/// Write the codec preference toggles to disk and push the new caps to
/// every connected community server (spec §4.6).
#[tauri::command]
pub async fn set_codec_settings(
    settings: CodecSettingsPayload,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut current = config::load(&app)?.settings;
    current.use_av1 = settings.use_av1;
    current.use_h265 = settings.use_h265;
    config::save(&app, None, &current)?;

    // Build new caps with the toggles applied and broadcast.
    let resp = get_caps(app, state.clone()).await?;
    broadcast_update_capabilities(&state, &resp).await;
    Ok(())
}

/// Send UpdateCapabilitiesRequest to every connected community server.
/// Used by refresh_caps and set_codec_settings.
///
/// Lock discipline: snapshot (write_tx, jwt) per community under the
/// AppState lock, drop the lock, then send packets outside the lock.
async fn broadcast_update_capabilities(
    state: &State<'_, SharedState>,
    caps_resp: &CapsResponse,
) {
    let proto_caps = caps::build_client_capabilities(&caps_resp.encode, &caps_resp.decode);
    let payload = packet::Payload::UpdateCapabilitiesReq(UpdateCapabilitiesRequest {
        capabilities: Some(proto_caps),
    });

    let sends: Vec<(tokio::sync::mpsc::Sender<Vec<u8>>, Vec<u8>)> = {
        let s = state.lock().await;
        s.communities
            .values()
            .filter_map(|c| {
                c.connection_write_tx().map(|tx| {
                    let data = build_packet(
                        packet::Type::UpdateCapabilitiesReq,
                        payload.clone(),
                        Some(&c.jwt),
                    );
                    (tx, data)
                })
            })
            .collect()
    };

    for (tx, data) in sends {
        let _ = tx.send(data).await;
    }
}
