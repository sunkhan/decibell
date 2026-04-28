//! Trait + types shared by the Windows GPU capture sources (DXGI Desktop
//! Duplication and Windows Graphics Capture). Both produce BGRA D3D11
//! textures into the GpuStreamingPipeline's shared device.

#![cfg(target_os = "windows")]

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

#[derive(Debug)]
pub struct GpuFrame {
    /// BGRA texture in the shared device's VRAM. Caller must release
    /// (via release_current_frame for DXGI; WGC manages implicit lifetime
    /// via the FramePool drop).
    pub texture: ID3D11Texture2D,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
}

#[derive(Debug)]
pub enum CaptureError {
    Timeout,        // no new frame ready; caller should poll again
    AccessLost,     // device removed / output reconfigured
    Disconnected,   // source ended (window closed, monitor unplugged)
    Other(String),
}

pub trait GpuCaptureSource: Send {
    /// Native width of the source (monitor or window) in luma pixels.
    /// Used by the VideoProcessor to size the input view; may differ
    /// from the encoder's target width when the user requests scaling.
    fn width(&self) -> u32;

    /// Native height of the source. See width().
    fn height(&self) -> u32;

    /// Returns the next available frame, or Ok(None) on timeout. The
    /// returned GpuFrame is valid until release_current_frame() is called.
    fn next_frame(&mut self) -> Result<Option<GpuFrame>, CaptureError>;

    /// Release the most recently acquired frame back to the source.
    /// Required for DXGI (one frame in flight at a time); no-op for WGC.
    fn release_current_frame(&mut self);
}
