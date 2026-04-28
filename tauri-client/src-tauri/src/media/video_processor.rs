//! BGRA → NV12 GPU color conversion via D3D11 Video Processor.
//!
//! Owns no destination texture — `blit_into` writes directly into a
//! caller-supplied NV12 texture (the encoder's hw_frames_ctx pool
//! texture). No CPU readback, no private NV12 buffer. The CPU-readback
//! variant lives as `LegacyVideoProcessor` in `capture_dxgi.rs`.

#![cfg(target_os = "windows")]

use windows::core::{Interface, BOOL};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

pub struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
}

// Safety: created and used on the single GpuStreamingPipeline thread.
unsafe impl Send for VideoProcessor {}

impl VideoProcessor {
    pub fn new(
        device: &ID3D11Device,
        src_w: u32,
        src_h: u32,
        dst_w: u32,
        dst_h: u32,
    ) -> Result<Self, String> {
        unsafe {
            let video_device: ID3D11VideoDevice = device
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoDevice: {}", e))?;

            let base_context = device
                .GetImmediateContext()
                .map_err(|e| format!("GetImmediateContext: {}", e))?;

            let video_context: ID3D11VideoContext = base_context
                .cast()
                .map_err(|e| format!("Cast to ID3D11VideoContext: {}", e))?;

            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                InputWidth: src_w,
                InputHeight: src_h,
                OutputFrameRate: DXGI_RATIONAL { Numerator: 60, Denominator: 1 },
                OutputWidth: dst_w,
                OutputHeight: dst_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator: {}", e))?;

            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {}", e))?;

            // Set explicit color spaces for correct BGRA→NV12 conversion.
            // Input is BGRA/sRGB (DuplicateOutput1 handles HDR→SDR for us).
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor, 0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }

            Ok(VideoProcessor {
                video_device,
                video_context,
                processor,
                enumerator,
            })
        }
    }

    /// Blit BGRA source into a caller-provided NV12 destination texture.
    /// The destination is the encoder's hw_frames_ctx pool texture and
    /// changes per frame. Filled in Task 2.
    pub fn blit_into(
        &self,
        context: &ID3D11DeviceContext,
        bgra_texture: &ID3D11Texture2D,
        nv12_dst: &ID3D11Texture2D,
    ) -> Result<(), String> {
        let _ = (context, bgra_texture, nv12_dst, BOOL(0));
        Ok(()) // implemented in Task 2
    }
}
