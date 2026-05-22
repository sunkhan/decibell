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
}
