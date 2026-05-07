//! Audio-frame type shared by stream-audio capture (PipeWire on Linux,
//! WASAPI on Windows) and the stream-audio Opus pipeline.
//!
//! Video capture moved to the renderer in PR8 — Chromium's
//! `getDisplayMedia` + `WebCodecs.VideoEncoder` replace the native
//! capture-and-encode stack. Encoded chunks travel from renderer →
//! native via IPC for UDP packetization. Removed from this file:
//! `RawFrame`, `DmaBufFrame`, `CaptureConfig`, `CaptureSource`,
//! `CaptureSourceType`, `PixelFormat`, `CaptureOutput`, `list_sources`,
//! `start_capture` — all gone with the FFmpeg encoder + per-platform
//! capture backends.

/// Raw audio frame from platform audio capture.
#[derive(Debug)]
pub struct AudioFrame {
    /// Interleaved stereo f32 PCM samples (L, R, L, R, ...)
    pub data: Vec<f32>,
    pub channels: u16,
    pub sample_rate: u32,
}
