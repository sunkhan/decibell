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

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// NV12 (Y + interleaved UV), tightly packed. Used by Windows capture.
    NV12,
    /// BGRA 32-bit, with per-row stride. Used by Linux PipeWire capture.
    BGRA,
    /// RGBA 32-bit, with per-row stride.
    RGBA,
}

#[derive(Debug)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes. Only meaningful for BGRA/RGBA formats.
    pub stride: usize,
    pub pixel_format: PixelFormat,
    pub timestamp_us: u64,
}

/// GPU-resident frame from PipeWire DMA-BUF capture (Linux only).
/// The fd is a dup'd DMA-BUF file descriptor — the kernel keeps the
/// underlying buffer alive via refcount even after PipeWire reclaims it.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DmaBufFrame {
    /// DMA-BUF file descriptor (dup'd from PipeWire, closed on drop)
    pub fd: std::os::fd::OwnedFd,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes
    pub stride: u32,
    /// DRM fourcc format code (e.g. DRM_FORMAT_ARGB8888 for BGRA)
    pub drm_format: u32,
    /// DRM format modifier (DRM_FORMAT_MOD_INVALID if unknown)
    pub modifier: u64,
    pub timestamp_us: u64,
}

/// Raw audio frame from platform audio capture.
#[derive(Debug)]
pub struct AudioFrame {
    /// Interleaved stereo f32 PCM samples (L, R, L, R, ...)
    pub data: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

/// Result of starting a capture — the frame receiver plus the actual output dimensions.
pub struct CaptureOutput {
    pub receiver: std::sync::mpsc::Receiver<RawFrame>,
    pub width: u32,
    pub height: u32,
    /// Linux-only: optional DMA-BUF frame receiver for zero-copy GPU encoding.
    #[cfg(target_os = "linux")]
    pub gpu_receiver: Option<std::sync::mpsc::Receiver<DmaBufFrame>>,
}

/// List available capture sources (screens and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    #[cfg(target_os = "linux")]
    {
        // On wlroots compositors (Niri, Sway, Hyprland, river) we go
        // straight to wlr-screencopy and list each wl_output as a source.
        // Avoids xdg-desktop-portal entirely — no NVIDIA-Wayland buffer
        // allocation drama, no portal proxy in the middle. Mutter (GNOME)
        // and KWin (KDE Plasma) don't advertise wlr-screencopy, so they
        // fall through to the existing portal path.
        if super::capture_wlr_screencopy::is_available() {
            return super::capture_wlr_screencopy::list_sources().await;
        }
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
        // wlr-screencopy source ids are prefixed with "wlr:" — the wlroots
        // backend listed them. Anything else (notably "portal") goes
        // through the PipeWire-portal backend.
        if super::capture_wlr_screencopy::owns_source(source_id) {
            return super::capture_wlr_screencopy::start_capture(source_id, config).await;
        }
        super::capture_pipewire::start_capture(source_id, config).await
    }
    #[cfg(target_os = "windows")]
    {
        if source_id.starts_with("monitor:") {
            let sid = source_id.to_string();
            let cfg = config.clone();
            // Try DXGI Desktop Duplication first (lower overhead, handles HDR).
            // If it fails (e.g. legacy DX9 exclusive fullscreen), fall back to
            // WGC CreateForMonitor which can capture those applications.
            let dxgi_result = tokio::task::spawn_blocking(move || {
                super::capture_dxgi::start_capture(&sid, &cfg)
            })
            .await
            .map_err(|e| format!("Join error: {}", e))?;

            match dxgi_result {
                Ok(output) => Ok(output),
                Err(e) => {
                    eprintln!("[capture] DXGI DD failed ({}), falling back to WGC for monitor", e);
                    super::capture_wgc::start_capture(source_id, config).await
                }
            }
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
