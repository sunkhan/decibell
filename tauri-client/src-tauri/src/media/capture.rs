use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSource {
    pub id: String,
    pub name: String,
    pub source_type: CaptureSourceType,
    pub width: u32,
    pub height: u32,
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

/// List available capture sources (screens and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::list_sources().await
    }
    #[cfg(target_os = "windows")]
    {
        super::capture_wgc::list_sources().await
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err("Screen capture not supported on this platform".to_string())
    }
}

/// Start capturing from a source.
/// Returns a channel that receives RawFrames.
pub async fn start_capture(
    source_id: &str,
    config: &CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<RawFrame>, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::start_capture(source_id, config).await
    }
    #[cfg(target_os = "windows")]
    {
        super::capture_wgc::start_capture(source_id, config).await
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        let _ = (source_id, config);
        Err("Screen capture not supported on this platform".to_string())
    }
}
