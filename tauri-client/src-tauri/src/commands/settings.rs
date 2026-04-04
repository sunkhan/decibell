use cpal::traits::{DeviceTrait, HostTrait};
use serde::Serialize;
use tauri::{AppHandle, State};

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
