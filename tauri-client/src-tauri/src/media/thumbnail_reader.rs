//! Periodic GPU→CPU thumbnail readback for the zero-copy pipeline.
//!
//! The encoder loop runs entirely on the GPU; thumbnails for the stream
//! presence list still need raw bytes on the CPU. We pay this cost only
//! every ~5s so the JPEG encode is amortized into noise.
//!
//! Two-phase ASYNC pipeline (no GPU sync in the encode loop):
//!   Phase 1 — start_capture(): VideoProcessorBlt source BGRA → small
//!     DEFAULT BGRA, CopyResource DEFAULT → STAGING. Both are submit-only,
//!     they return immediately without waiting for the GPU.
//!   Phase 2 — try_finish_capture(): Map STAGING with DO_NOT_WAIT. Returns
//!     None while the GPU is still processing the copy; once ready, walks
//!     the rows and emits a JPEG.
//!
//! The synchronous Map(READ) flow we used before drained the entire GPU
//! command queue, blocking the encode loop for ~hundreds of ms — visible
//! in the per-second fps log as one big drop every 5s. Phase-splitting the
//! readback removes that stall: the encode loop only ever does cheap
//! submit-and-poll work, never a sync point.
//!
//! Width is fixed at THUMB_WIDTH (320 px); height tracks source aspect.

#![cfg(target_os = "windows")]

use std::io::Cursor;
use windows::core::{Interface, BOOL, HRESULT};
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;

const THUMB_WIDTH: u32 = 320;

/// HRESULT returned by Map() when the GPU hasn't finished writing yet.
const DXGI_ERROR_WAS_STILL_DRAWING: HRESULT = HRESULT(0x887A000Au32 as i32);

pub struct ThumbnailReader {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    processor: ID3D11VideoProcessor,
    enumerator: ID3D11VideoProcessorEnumerator,
    /// Render-target BGRA texture the VideoProcessor writes to.
    dst: ID3D11Texture2D,
    /// CPU-readable BGRA texture; CopyResource lands here so we can Map it.
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
    /// True between start_capture and try_finish_capture-success. Pipeline
    /// uses this to know whether the staging texture is "in flight" or
    /// idle. We don't kick off another start_capture until the previous
    /// one has been read out.
    pending: bool,
}

unsafe impl Send for ThumbnailReader {}

impl ThumbnailReader {
    pub fn new(device: &ID3D11Device, src_w: u32, src_h: u32) -> Result<Self, String> {
        if src_w == 0 || src_h == 0 {
            return Err("ThumbnailReader: zero source dims".into());
        }
        let thumb_w = THUMB_WIDTH.min(src_w);
        let thumb_h = ((thumb_w as u64 * src_h as u64) / src_w as u64) as u32;
        if thumb_h == 0 {
            return Err("ThumbnailReader: computed thumb_h is 0".into());
        }

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
                OutputFrameRate: DXGI_RATIONAL { Numerator: 1, Denominator: 1 },
                OutputWidth: thumb_w,
                OutputHeight: thumb_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator (thumb): {}", e))?;
            let processor = video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor (thumb): {}", e))?;

            // BGRA → BGRA, both sRGB. Identical color space.
            if let Ok(vc1) = video_context.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor, 0, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor, DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
            }

            let dst_desc = D3D11_TEXTURE2D_DESC {
                Width: thumb_w,
                Height: thumb_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_DEFAULT,
                BindFlags: D3D11_BIND_RENDER_TARGET.0 as u32,
                CPUAccessFlags: 0,
                MiscFlags: 0,
            };
            let mut dst: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&dst_desc, None, Some(&mut dst))
                .map_err(|e| format!("CreateTexture2D (thumb dst): {}", e))?;
            let dst = dst.ok_or("CreateTexture2D (thumb dst) returned None")?;

            let staging_desc = D3D11_TEXTURE2D_DESC {
                Width: thumb_w,
                Height: thumb_h,
                MipLevels: 1,
                ArraySize: 1,
                Format: DXGI_FORMAT_B8G8R8A8_UNORM,
                SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
                Usage: D3D11_USAGE_STAGING,
                BindFlags: 0,
                CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
                MiscFlags: 0,
            };
            let mut staging: Option<ID3D11Texture2D> = None;
            device
                .CreateTexture2D(&staging_desc, None, Some(&mut staging))
                .map_err(|e| format!("CreateTexture2D (thumb staging): {}", e))?;
            let staging = staging.ok_or("CreateTexture2D (thumb staging) returned None")?;

            Ok(ThumbnailReader {
                video_device,
                video_context,
                processor,
                enumerator,
                dst,
                staging,
                width: thumb_w,
                height: thumb_h,
                pending: false,
            })
        }
    }

    /// True if a previous start_capture is still in flight on the GPU
    /// and we haven't successfully drained it via try_finish_capture yet.
    /// Pipeline checks this to avoid kicking off a second capture while
    /// the staging texture is still being written.
    pub fn is_pending(&self) -> bool { self.pending }

    /// Phase 1 — submit GPU work and return immediately.
    /// VideoProcessorBlt downscales src BGRA → our small dst, then
    /// CopyResource lands it in our CPU-readable staging texture. Neither
    /// call waits for the GPU. The pixels become readable some unknown
    /// number of frames later; try_finish_capture polls for that.
    ///
    /// Caller must check is_pending() == false before calling — kicking
    /// off a second capture while one is in flight would race with the
    /// GPU's writes to staging.
    pub fn start_capture(
        &mut self,
        context: &ID3D11DeviceContext,
        bgra_src: &ID3D11Texture2D,
    ) -> Result<(), String> {
        unsafe {
            let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV { MipSlice: 0, ArraySlice: 0 },
                },
            };
            let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    bgra_src, &self.enumerator, &input_view_desc, Some(&mut input_view),
                )
                .map_err(|e| format!("CreateVideoProcessorInputView (thumb): {}", e))?;
            let input_view = input_view.ok_or("thumb input_view None")?;

            let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
                ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
                },
            };
            let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
            self.video_device
                .CreateVideoProcessorOutputView(
                    &self.dst, &self.enumerator, &output_view_desc, Some(&mut output_view),
                )
                .map_err(|e| format!("CreateVideoProcessorOutputView (thumb): {}", e))?;
            let output_view = output_view.ok_or("thumb output_view None")?;

            let stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: std::mem::ManuallyDrop::new(Some(input_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: std::mem::ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };

            self.video_context
                .VideoProcessorBlt(
                    &self.processor, &output_view, 0, std::slice::from_ref(&stream),
                )
                .map_err(|e| format!("VideoProcessorBlt (thumb): {}", e))?;

            std::mem::ManuallyDrop::into_inner(std::ptr::read(&stream.pInputSurface));

            // Submit-only copy. CPU does NOT block here.
            context.CopyResource(&self.staging, &self.dst);

            self.pending = true;
            Ok(())
        }
    }

    /// Phase 2 — non-blocking poll. Returns:
    ///   Ok(Some(jpeg))  — staging was ready, JPEG built, state cleared.
    ///   Ok(None)        — GPU still working OR no capture pending; try later.
    ///   Err(_)          — fatal Map error (state cleared so we recover).
    pub fn try_finish_capture(
        &mut self,
        context: &ID3D11DeviceContext,
    ) -> Result<Option<Vec<u8>>, String> {
        if !self.pending {
            return Ok(None);
        }
        unsafe {
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            // DO_NOT_WAIT: returns DXGI_ERROR_WAS_STILL_DRAWING immediately
            // if the GPU hasn't finished writing the staging texture yet,
            // instead of blocking the calling thread.
            match context.Map(
                &self.staging, 0, D3D11_MAP_READ,
                D3D11_MAP_FLAG_DO_NOT_WAIT.0 as u32, Some(&mut mapped),
            ) {
                Ok(()) => {}
                Err(e) if e.code() == DXGI_ERROR_WAS_STILL_DRAWING => {
                    return Ok(None);
                }
                Err(e) => {
                    self.pending = false;
                    return Err(format!("Map staging (thumb, DO_NOT_WAIT): {}", e));
                }
            }

            let row_pitch = mapped.RowPitch as usize;
            let w = self.width as usize;
            let h = self.height as usize;
            let mut rgb = vec![0u8; w * h * 3];
            let src_base = mapped.pData as *const u8;
            for y in 0..h {
                let row = std::slice::from_raw_parts(src_base.add(y * row_pitch), w * 4);
                let dst_row = &mut rgb[y * w * 3..(y + 1) * w * 3];
                for x in 0..w {
                    let s = x * 4;
                    let d = x * 3;
                    // BGRA → RGB
                    dst_row[d] = row[s + 2];
                    dst_row[d + 1] = row[s + 1];
                    dst_row[d + 2] = row[s];
                }
            }
            context.Unmap(&self.staging, 0);
            self.pending = false;

            use image::ImageEncoder;
            let mut buf = Cursor::new(Vec::with_capacity(16 * 1024));
            let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut buf, 60);
            encoder
                .write_image(&rgb, self.width, self.height, image::ColorType::Rgb8.into())
                .map_err(|e| format!("JPEG encode (thumb): {}", e))?;
            Ok(Some(buf.into_inner()))
        }
    }
}
