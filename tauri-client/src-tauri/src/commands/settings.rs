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
    let host = cpal::default_host();

    let inputs: Vec<AudioDevice> = host
        .input_devices()
        .map_err(|e| format!("Failed to list input devices: {}", e))?
        .filter_map(|d| d.name().ok().map(|name| AudioDevice { name }))
        .collect();

    let outputs: Vec<AudioDevice> = host
        .output_devices()
        .map_err(|e| format!("Failed to list output devices: {}", e))?
        .filter_map(|d| d.name().ok().map(|name| AudioDevice { name }))
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

    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_input_device(name);
    }
    Ok(())
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

    let s = state.lock().await;
    if let Some(ref engine) = s.voice_engine {
        engine.set_output_device(name);
    }
    Ok(())
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

    std::thread::Builder::new()
        .name("decibell-mic-test".to_string())
        .spawn(move || {
            if let Err(e) = run_mic_test(device_name, app, stop_flag_thread) {
                eprintln!("[mic-test] Error: {}", e);
            }
        })
        .map_err(|e| format!("Spawn mic test thread: {}", e))?;

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
