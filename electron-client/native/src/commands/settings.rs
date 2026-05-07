//! Settings-related commands. PR5 ships the voice-relevant subset:
//! audio-device enumeration. The richer settings surface (load/save
//! of the encrypted `AppSettings` blob, mic-test, codec capabilities
//! probing) ports with the settings-modal PR — until then the
//! renderer keeps preferences in `useUiStore` only and re-applies
//! them on each session.

use cpal::traits::{DeviceTrait, HostTrait};

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
