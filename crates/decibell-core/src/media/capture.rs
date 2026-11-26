//! Audio-frame type shared by stream-audio capture (PipeWire on Linux,
//! WASAPI on Windows) and the stream-audio Opus pipeline.
//!
//! Video capture moved to the renderer in PR8 — Chromium's
//! `getDisplayMedia` + `WebCodecs.VideoEncoder` replace the native
//! capture-and-encode stack on macOS, and remain the non-native
//! fallback on Linux.
//!
//! The Linux *native* video frame types below were revived from the
//! Tauri client for the native PipeWire/portal capture → FFmpeg
//! (NVENC/VAAPI) encode path (see docs/superpowers/specs/2026-05-15-
//! linux-native-ffmpeg-encoder-design.md). They're Linux-gated:
//! Windows feeds the encoder D3D11 textures directly and never builds
//! a `RawFrame`.

/// Raw audio frame from platform audio capture.
#[derive(Debug)]
pub struct AudioFrame {
    /// Interleaved stereo f32 PCM samples (L, R, L, R, ...)
    pub data: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}

/// A selectable capture source (screen or window). Used by the
/// wlr-screencopy output enumeration and the portal restore path; the
/// native default flow lets the XDG portal dialog pick the source.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSource {
    pub id: String,
    pub name: String,
    pub source_type: CaptureSourceType,
    pub width: u32,
    pub height: u32,
    /// Base64-encoded preview thumbnail data URI, if available.
    pub thumbnail: Option<String>,
}

#[cfg(target_os = "linux")]
#[derive(Debug, Clone, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSourceType {
    Screen,
    Window,
}

/// Pixel layout of a captured CPU video frame.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat {
    /// NV12 (Y + interleaved UV), tightly packed.
    NV12,
    /// BGRA 32-bit, with per-row stride. Default for PipeWire SHM frames.
    BGRA,
    /// RGBA 32-bit, with per-row stride.
    RGBA,
}

/// CPU-resident captured video frame (PipeWire SHM, or DMA-BUF after a
/// CPU-readback fallback).
#[cfg(target_os = "linux")]
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

/// GPU-resident frame from PipeWire DMA-BUF capture. The fd is a dup'd
/// DMA-BUF descriptor — the kernel keeps the underlying buffer alive via
/// refcount even after PipeWire reclaims its slot.
#[cfg(target_os = "linux")]
#[derive(Debug)]
pub struct DmaBufFrame {
    /// DMA-BUF file descriptor (dup'd from PipeWire, closed on drop).
    pub fd: std::os::fd::OwnedFd,
    pub width: u32,
    pub height: u32,
    /// Row stride in bytes.
    pub stride: u32,
    /// DRM fourcc format code (e.g. DRM_FORMAT_ARGB8888 for BGRA).
    pub drm_format: u32,
    /// DRM format modifier (DRM_FORMAT_MOD_INVALID if unknown).
    pub modifier: u64,
    pub timestamp_us: u64,
}

/// Target geometry/rate for a capture session.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub target_fps: u32,
    pub target_width: u32,
    pub target_height: u32,
    /// Embed the mouse cursor in the captured frames. Maps to the XDG
    /// portal's EMBEDDED vs HIDDEN cursor_mode (and wlr-screencopy's
    /// overlay_cursor flag).
    pub include_cursor: bool,
}

/// Result of starting a capture — the CPU frame receiver plus the actual
/// negotiated output dimensions, and an optional DMA-BUF receiver for the
/// zero-copy GPU encode path.
#[cfg(target_os = "linux")]
pub struct CaptureOutput {
    pub receiver: std::sync::mpsc::Receiver<RawFrame>,
    pub width: u32,
    pub height: u32,
    pub gpu_receiver: Option<std::sync::mpsc::Receiver<DmaBufFrame>>,
    /// Why the capture ended on its own, recorded before its frame sender
    /// drops; the encoder thread reads it when the channel disconnects.
    pub end: std::sync::Arc<CaptureEndSlot>,
    /// Ends the capture from outside (stream stop, failed encoder open).
    /// None for a backend that ends once `receiver` is dropped.
    pub stopper: Option<CaptureStopper>,
}

/// Why a native capture ended without being asked to.
#[cfg(target_os = "linux")]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CaptureEnd {
    /// The source went away: the portal session was closed (the desktop's
    /// own "stop sharing"), the shared window closed, the monitor was
    /// unplugged. An ordinary end of the stream, not an error.
    SourceGone(String),
    /// The capture broke (PipeWire stream error, lost connection).
    Failed(String),
}

/// Where a capture records its `CaptureEnd`. Several observers can report
/// one end (the portal's Session.Closed, the node leaving the registry, the
/// stream state); a source that went away outranks a failure seen around
/// the same moment, since losing the source is what makes PipeWire complain.
#[cfg(target_os = "linux")]
#[derive(Debug, Default)]
pub struct CaptureEndSlot(std::sync::Mutex<Option<CaptureEnd>>);

#[cfg(target_os = "linux")]
impl CaptureEndSlot {
    pub fn record(&self, end: CaptureEnd) {
        let mut slot = self.0.lock().unwrap_or_else(|e| e.into_inner());
        let replace = matches!(
            (&*slot, &end),
            (None, _) | (Some(CaptureEnd::Failed(_)), CaptureEnd::SourceGone(_))
        );
        if replace {
            *slot = Some(end);
        }
    }

    pub fn get(&self) -> Option<CaptureEnd> {
        self.0.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }
}

/// Ends a running capture from another thread. Cheap to clone; calling it
/// after the capture already ended is a no-op.
#[cfg(target_os = "linux")]
#[derive(Clone)]
pub struct CaptureStopper(std::sync::Arc<dyn Fn() + Send + Sync>);

#[cfg(target_os = "linux")]
impl CaptureStopper {
    pub fn new(stop: impl Fn() + Send + Sync + 'static) -> Self {
        Self(std::sync::Arc::new(stop))
    }

    pub fn stop(&self) {
        (self.0)()
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use super::*;

    #[test]
    fn a_lost_source_outranks_a_failure_but_not_the_reverse() {
        let slot = CaptureEndSlot::default();
        assert_eq!(slot.get(), None);
        slot.record(CaptureEnd::Failed("stream error".into()));
        slot.record(CaptureEnd::SourceGone("session closed".into()));
        assert_eq!(slot.get(), Some(CaptureEnd::SourceGone("session closed".into())));
        // The first source-gone reason stays; a later failure doesn't
        // turn a stopped share into an error.
        slot.record(CaptureEnd::SourceGone("node removed".into()));
        slot.record(CaptureEnd::Failed("disconnected".into()));
        assert_eq!(slot.get(), Some(CaptureEnd::SourceGone("session closed".into())));
    }
}
