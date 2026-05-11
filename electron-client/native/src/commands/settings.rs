//! Settings-related commands.

use cpal::traits::{DeviceTrait, HostTrait};

/// Read the persisted config blob from disk. Returns
/// `{ credentials, settings }` as a JSON value so the renderer can
/// hydrate its zustand stores without us having to re-declare the
/// whole AppSettings shape as a #[napi(object)] mirror (it has a
/// HashMap field which napi(object) doesn't support natively).
#[napi]
pub async fn load_config() -> napi::Result<serde_json::Value> {
    let loaded = crate::config::load().map_err(napi::Error::from_reason)?;
    serde_json::to_value(loaded)
        .map_err(|e| napi::Error::from_reason(format!("Serialize config: {}", e)))
}

/// Persist a complete AppSettings blob to disk, preserving any
/// existing credentials. Renderer is the source of truth for the
/// settings shape; native just deserializes via serde and writes.
/// Mirrors tauri-client's save_settings: settings come from the
/// renderer as an object, we round-trip through serde_json::Value
/// into AppSettings.
#[napi]
pub async fn save_settings(settings: serde_json::Value) -> napi::Result<()> {
    let app_settings: crate::config::AppSettings = serde_json::from_value(settings)
        .map_err(|e| napi::Error::from_reason(format!("Invalid settings: {}", e)))?;
    crate::config::save(None, &app_settings).map_err(napi::Error::from_reason)
}

#[napi(object)]
pub struct SetDmPrivacyArgs {
    pub friends_only: bool,
}

/// Tell the central server whether to deliver DMs from non-friends.
/// Mirrors the tauri command of the same name (defined there in
/// commands/messaging.rs). Lives in settings.rs here because the DM
/// command surface hasn't ported yet — when it does, this can move.
#[napi]
pub async fn set_dm_privacy(args: SetDmPrivacyArgs) -> napi::Result<()> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, DmPrivacySetting};

    let state_arc = crate::state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let token = s.token.clone();
        let central = s
            .central
            .as_ref()
            .ok_or_else(|| napi::Error::from_reason("Not connected to central server"))?;
        let tx = central
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Central connection lost"))?;
        let pkt = build_packet(
            packet::Type::DmPrivacy,
            packet::Payload::DmPrivacy(DmPrivacySetting {
                friends_only: args.friends_only,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct SetTransferLimitsArgs {
    pub upload_bps: u32,
    pub download_bps: u32,
}

/// Persist the user's per-file upload/download caps. PR8 attachment
/// transfers happen renderer-side via Electron main's netFetch, so the
/// caps would have to be enforced there to actually rate-limit — for
/// now this command just writes them to AppSettings so they survive
/// across restarts. Live enforcement is a follow-up; the renderer's
/// saveSettings() will already include these fields, but exposing the
/// command keeps the call shape compatible with tauri-client's
/// NetworkTab.
#[napi]
pub async fn set_transfer_limits(args: SetTransferLimitsArgs) -> napi::Result<()> {
    let loaded = crate::config::load().map_err(napi::Error::from_reason)?;
    let mut settings = loaded.settings;
    settings.upload_limit_bps = args.upload_bps as u64;
    settings.download_limit_bps = args.download_bps as u64;
    crate::config::save(None, &settings).map_err(napi::Error::from_reason)
}

#[napi(object)]
pub struct AudioDevice {
    pub name: String,
    pub label: String,
}

#[napi(object)]
pub struct AudioDeviceList {
    pub inputs: Vec<AudioDevice>,
    pub outputs: Vec<AudioDevice>,
}

#[napi]
pub async fn list_audio_devices() -> napi::Result<AudioDeviceList> {
    #[cfg(target_os = "linux")]
    {
        return Ok(list_audio_devices_pactl());
    }

    #[cfg(not(target_os = "linux"))]
    {
        let host = cpal::default_host();

        let inputs: Vec<AudioDevice> = host
            .input_devices()
            .map_err(|e| napi::Error::from_reason(format!("Failed to list input devices: {}", e)))?
            .filter_map(|d| {
                d.name().ok().map(|name| AudioDevice {
                    name: name.clone(),
                    label: name,
                })
            })
            .collect();

        let outputs: Vec<AudioDevice> = host
            .output_devices()
            .map_err(|e| napi::Error::from_reason(format!("Failed to list output devices: {}", e)))?
            .filter_map(|d| {
                d.name().ok().map(|name| AudioDevice {
                    name: name.clone(),
                    label: name,
                })
            })
            .collect();

        Ok(AudioDeviceList { inputs, outputs })
    }
}

/// Linux: enumerate audio devices via `pactl` so PipeWire/PulseAudio
/// devices show up with their friendly descriptions — same list as
/// Chromium-based apps (Vesktop / Discord). Falls back silently to an
/// empty list when pactl isn't available.
#[cfg(target_os = "linux")]
fn list_audio_devices_pactl() -> AudioDeviceList {
    fn parse_pactl_devices(kind: &str) -> Vec<AudioDevice> {
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
                if let (Some(name), Some(desc)) = (current_name.take(), current_desc.take()) {
                    devices.push(AudioDevice { name, label: desc });
                }
                current_name = Some(trimmed.strip_prefix("Name: ").unwrap().to_string());
                current_desc = None;
            } else if trimmed.starts_with("Description: ") {
                current_desc = Some(trimmed.strip_prefix("Description: ").unwrap().to_string());
            }
        }
        if let (Some(name), Some(desc)) = (current_name, current_desc) {
            devices.push(AudioDevice { name, label: desc });
        }
        devices
    }

    let outputs = parse_pactl_devices("sinks");
    let inputs: Vec<AudioDevice> = parse_pactl_devices("sources")
        .into_iter()
        .filter(|d| !d.name.ends_with(".monitor"))
        .collect();

    AudioDeviceList { inputs, outputs }
}
