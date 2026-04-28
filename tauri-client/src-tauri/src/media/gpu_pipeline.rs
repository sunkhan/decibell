//! GPU-only sender pipeline for Windows: capture → BGRA→NV12 → NVENC.
//! No GPU↔CPU readback. Single thread runs the whole loop.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D::*;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::*;
use windows::Win32::Foundation::HMODULE;

use super::capture_dxgi;
use super::capture_wgc;
use super::caps::CodecKind;
use super::encoder::{EncodedFrame, EncoderConfig, H264Encoder};
use super::gpu_capture::{CaptureError, GpuCaptureSource};
use super::video_processor::VideoProcessor;

pub struct GpuStreamingPipeline {
    // Hold the device + context so they outlive any encoder-internal refs.
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    capture: Box<dyn GpuCaptureSource>,
    converter: VideoProcessor,
    encoder: H264Encoder,
}

pub struct GpuPipelineCallbacks {
    /// Called for each encoded packet ready to ship.
    pub on_encoded: Box<dyn FnMut(EncodedFrame) + Send>,
    /// Called when the pipeline exits with a fatal error.
    pub on_error: Box<dyn FnOnce(String) + Send>,
}

impl GpuStreamingPipeline {
    /// Build the pipeline. Returns Err if any D3D11 / NVENC step fails;
    /// caller should fall back to the CPU path on Err.
    pub fn build(
        target_codec: CodecKind,
        source_id: &str,
        config: EncoderConfig,
    ) -> Result<Self, String> {
        // Source IDs from capture::list_sources():
        //   "monitor:{adapter_idx}:{output_idx}"  → DxgiSource
        //   "window:{hwnd}"                       → WgcSource
        let (capture, context, device): (Box<dyn GpuCaptureSource>, _, _) =
            if let Some(rest) = source_id.strip_prefix("monitor:") {
                let mut parts = rest.splitn(2, ':');
                let adapter_idx: u32 = parts
                    .next().and_then(|s| s.parse().ok())
                    .ok_or("monitor source ID malformed (adapter_idx)")?;
                let output_idx: u32 = parts
                    .next().and_then(|s| s.parse().ok())
                    .ok_or("monitor source ID malformed (output_idx)")?;

                // Build the shared device on the same adapter as the output.
                let factory: IDXGIFactory1 = unsafe {
                    CreateDXGIFactory1().map_err(|e| format!("CreateDXGIFactory1: {}", e))?
                };
                let adapter: IDXGIAdapter1 = unsafe {
                    factory.EnumAdapters1(adapter_idx)
                        .map_err(|e| format!("EnumAdapters1: {}", e))?
                };
                let (device, context) = capture_dxgi::create_device_for_adapter(&adapter)?;
                let src = capture_dxgi::DxgiSource::new(adapter_idx, output_idx, &device)?;
                (Box::new(src) as Box<dyn GpuCaptureSource>, context, device)
            } else if source_id.starts_with("window:") {
                // Build a generic device on any HW adapter.
                let mut device: Option<ID3D11Device> = None;
                let mut context: Option<ID3D11DeviceContext> = None;
                let mut actual_level = D3D_FEATURE_LEVEL_11_0;
                unsafe {
                    D3D11CreateDevice(
                        None,
                        D3D_DRIVER_TYPE_HARDWARE,
                        HMODULE::default(),
                        D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                        Some(&[D3D_FEATURE_LEVEL_11_0]),
                        D3D11_SDK_VERSION,
                        Some(&mut device),
                        Some(&mut actual_level),
                        Some(&mut context),
                    )
                    .map_err(|e| format!("D3D11CreateDevice: {}", e))?;
                }
                let device = device.ok_or("D3D11CreateDevice returned None")?;
                let context = context.ok_or("D3D11CreateDevice context None")?;
                let src = capture_wgc::WgcSource::new(source_id, &device)?;
                (Box::new(src) as Box<dyn GpuCaptureSource>, context, device)
            } else {
                return Err(format!("Unknown source ID prefix: {}", source_id));
            };

        // Source native dims for the VideoProcessor input view; may differ
        // from encoder target dims when the user requests scaling.
        let src_w = capture.width();
        let src_h = capture.height();

        // GPU color converter (BGRA src_w×src_h → NV12 config.width×config.height).
        // Same blit handles both color conversion and scaling in one op.
        let converter = VideoProcessor::new(&device, src_w, src_h, config.width, config.height)
            .map_err(|e| format!("VideoProcessor::new: {}", e))?;

        // Encoder via D3D11VA hwaccel.
        let device_raw = device.as_raw() as *mut std::ffi::c_void;
        let encoder = H264Encoder::new_d3d11(target_codec, &config, device_raw)
            .map_err(|e| format!("H264Encoder::new_d3d11: {}", e))?;

        Ok(GpuStreamingPipeline {
            _device: device,
            context,
            capture,
            converter,
            encoder,
        })
    }

    /// Run the capture→convert→encode loop on the calling thread.
    /// Returns when shutdown is requested (via shutdown_flag) or on fatal error.
    pub fn run(
        mut self,
        shutdown_flag: Arc<AtomicBool>,
        mut callbacks: GpuPipelineCallbacks,
    ) {
        loop {
            if shutdown_flag.load(Ordering::Relaxed) { break; }

            let frame = match self.capture.next_frame() {
                Ok(Some(f)) => f,
                Ok(None) => continue, // timeout, no new frame
                Err(CaptureError::AccessLost) => {
                    (callbacks.on_error)("DXGI access lost".into());
                    return;
                }
                Err(CaptureError::Disconnected) => {
                    eprintln!("[gpu-pipeline] capture source disconnected");
                    return;
                }
                Err(CaptureError::Other(msg)) => {
                    (callbacks.on_error)(format!("Capture error: {}", msg));
                    return;
                }
                Err(CaptureError::Timeout) => continue,
            };

            // Acquire pool frame, blit BGRA→NV12 directly into it, submit.
            let pool_frame = match self.encoder.acquire_pool_frame() {
                Ok(f) => f,
                Err(e) => {
                    self.capture.release_current_frame();
                    (callbacks.on_error)(format!("acquire_pool_frame: {}", e));
                    return;
                }
            };
            let nv12_tex = pool_frame.texture();
            if let Err(e) = self.converter.blit_into(&self.context, &frame.texture, &nv12_tex) {
                self.capture.release_current_frame();
                (callbacks.on_error)(format!("blit_into: {}", e));
                return;
            }
            // Drop the cloned texture handle BEFORE submit (encode takes pool_frame).
            drop(nv12_tex);

            self.capture.release_current_frame();

            match self.encoder.encode_d3d11_frame(pool_frame) {
                Ok(Some(encoded)) => (callbacks.on_encoded)(encoded),
                Ok(None) => {} // encoder buffering
                Err(e) => {
                    (callbacks.on_error)(format!("encode_d3d11_frame: {}", e));
                    return;
                }
            }
        }
        eprintln!("[gpu-pipeline] shutdown");
    }
}
