use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSource {
    pub id: String,
    pub name: String,
    pub source_type: CaptureSourceType,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded BMP data URI for preview thumbnail.
    pub thumbnail: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSourceType {
    Screen,
    Window,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub target_fps: u32,
    pub target_width: u32,
    pub target_height: u32,
}

#[derive(Debug)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
}

/// Result of starting a capture — the frame receiver plus the actual output dimensions.
pub struct CaptureOutput {
    pub receiver: std::sync::mpsc::Receiver<RawFrame>,
    pub width: u32,
    pub height: u32,
}

/// List available capture sources (screens and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::list_sources().await
    }
    #[cfg(target_os = "windows")]
    {
        tokio::task::spawn_blocking(|| {
            let mut sources = super::capture_dxgi::list_sources().unwrap_or_default();
            let mut windows = super::capture_wgc::list_window_sources().unwrap_or_default();
            sources.append(&mut windows);
            Ok(sources)
        })
        .await
        .map_err(|e| format!("Join error: {}", e))?
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err("Screen capture not supported on this platform".to_string())
    }
}

/// Start capturing from a source.
/// Returns a CaptureOutput with the frame receiver and resolved output dimensions.
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<CaptureOutput, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::start_capture(source_id, config).await
    }
    #[cfg(target_os = "windows")]
    {
        if source_id.starts_with("monitor:") {
            let source_id = source_id.to_string();
            let config = config.clone();
            tokio::task::spawn_blocking(move || {
                super::capture_dxgi::start_capture(&source_id, &config)
            })
            .await
            .map_err(|e| format!("Join error: {}", e))?
        } else {
            super::capture_wgc::start_capture(source_id, config).await
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (source_id, config);
        Err("Screen capture not supported on this platform".to_string())
    }
}
