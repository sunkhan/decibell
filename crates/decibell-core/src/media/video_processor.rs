//! BGRA → NV12 GPU color conversion (+ scaling) via the D3D11 Video
//! Processor.
//!
//! Owns no destination texture — `blit_into` writes directly into a
//! caller-supplied NV12 texture (the FFmpeg encoder's hw_frames_ctx
//! pool texture). No CPU readback, no private NV12 buffer.
//!
//! The processor is built for the OUTPUT (encode) size and lazily for the
//! INPUT size: the captured BGRA textures are the native monitor / window
//! size, which differs from the encode size and can change mid-stream
//! (window resize). A content desc that doesn't match the real input is a
//! known driver-crash trigger (Chromium's MF encoder builds it from the
//! input texture desc for the same reason), so `blit_into` reads the
//! source desc every frame and rebuilds the enumerator + processor when
//! the input size changes.
//!
//! Every rect is set explicitly. Per the D3D11 docs the default stream
//! destination rect is EMPTY, meaning "no data is written from this
//! stream" — NVIDIA ignores that, but a driver that follows the contract
//! would output nothing but background.

#![cfg(target_os = "windows")]

use std::mem::ManuallyDrop;

use windows::core::Interface;
use windows::Win32::Foundation::RECT;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows_core::BOOL;

use super::encode_util::aspect_fit_rect;

/// Output views are cached per destination texture: FFmpeg's D3D11 pool
/// recycles a handful of textures (6 pre-allocated), so this stays tiny.
/// The cap is only a backstop against an unexpectedly growing pool.
const MAX_CACHED_OUTPUT_VIEWS: usize = 16;

pub struct VideoProcessor {
    video_device: ID3D11VideoDevice,
    video_context: ID3D11VideoContext,
    /// ID3D11Multithread of the immediate context. Held (Enter/Leave,
    /// re-entrant) across view creation + Blt so the immediate-context
    /// sequence is atomic w.r.t. the encoder runtime's own threads.
    multithread: Option<ID3D11Multithread>,
    out_w: u32,
    out_h: u32,
    fps: u32,
    /// Built on the first frame and rebuilt when the input size changes.
    state: Option<ProcessorState>,
}

struct ProcessorState {
    in_w: u32,
    in_h: u32,
    enumerator: ID3D11VideoProcessorEnumerator,
    processor: ID3D11VideoProcessor,
    /// (pool texture, its output view). Holding the texture keeps its
    /// pointer from being reused by another texture while cached. Views are
    /// tied to the enumerator, so the cache lives and dies with it.
    output_views: Vec<(ID3D11Texture2D, ID3D11VideoProcessorOutputView)>,
}

// Created and used on the single encoder thread.
unsafe impl Send for VideoProcessor {}

/// Enter()s the device critical section; Leave()s on drop (every path).
struct MultithreadGuard(Option<ID3D11Multithread>);

impl MultithreadGuard {
    fn enter(mt: Option<ID3D11Multithread>) -> Self {
        if let Some(m) = &mt {
            unsafe { m.Enter() };
        }
        Self(mt)
    }
}

impl Drop for MultithreadGuard {
    fn drop(&mut self) {
        if let Some(m) = &self.0 {
            unsafe { m.Leave() };
        }
    }
}

impl VideoProcessor {
    /// `out_w`×`out_h` is the encode size (the NV12 pool texture size).
    /// The input side is configured from the first source texture.
    pub fn new(device: &ID3D11Device, out_w: u32, out_h: u32, fps: u32) -> Result<Self, String> {
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

            // Same lock the capture side's cursor compositor takes (same
            // IID as ID3D10Multithread, answered by the device); the
            // immediate context is the documented fallback.
            let multithread = device
                .cast::<ID3D11Multithread>()
                .or_else(|_| base_context.cast::<ID3D11Multithread>())
                .ok();

            Ok(VideoProcessor {
                video_device,
                video_context,
                multithread,
                out_w,
                out_h,
                fps: fps.max(1),
                state: None,
            })
        }
    }

    /// (Re)build the enumerator + processor for an `in_w`×`in_h` input.
    fn ensure_processor(&mut self, in_w: u32, in_h: u32) -> Result<(), String> {
        if let Some(s) = &self.state {
            if s.in_w == in_w && s.in_h == in_h {
                return Ok(());
            }
            log::info!(
                "[video-processor] input size {}x{} -> {}x{}; rebuilding processor",
                s.in_w,
                s.in_h,
                in_w,
                in_h
            );
        }
        // Drop the old processor (and its cached views) before building the
        // new one.
        self.state = None;

        let (out_w, out_h) = (self.out_w, self.out_h);
        unsafe {
            let content_desc = D3D11_VIDEO_PROCESSOR_CONTENT_DESC {
                InputFrameFormat: D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE,
                InputFrameRate: DXGI_RATIONAL {
                    Numerator: self.fps,
                    Denominator: 1,
                },
                InputWidth: in_w,
                InputHeight: in_h,
                OutputFrameRate: DXGI_RATIONAL {
                    Numerator: self.fps,
                    Denominator: 1,
                },
                OutputWidth: out_w,
                OutputHeight: out_h,
                Usage: D3D11_VIDEO_USAGE_PLAYBACK_NORMAL,
            };

            let enumerator = self
                .video_device
                .CreateVideoProcessorEnumerator(&content_desc)
                .map_err(|e| format!("CreateVideoProcessorEnumerator({in_w}x{in_h} -> {out_w}x{out_h}): {}", e))?;

            let processor = self
                .video_device
                .CreateVideoProcessor(&enumerator, 0)
                .map_err(|e| format!("CreateVideoProcessor: {}", e))?;

            let vc = &self.video_context;
            // Auto processing (driver "enhancements") off — it costs power
            // and can alter colours; progressive input, no deinterlacing.
            vc.VideoProcessorSetStreamAutoProcessingMode(&processor, 0, false);
            vc.VideoProcessorSetStreamFrameFormat(&processor, 0, D3D11_VIDEO_FRAME_FORMAT_PROGRESSIVE);

            // Whole source texture …
            let source_rect = RECT {
                left: 0,
                top: 0,
                right: in_w as i32,
                bottom: in_h as i32,
            };
            vc.VideoProcessorSetStreamSourceRect(&processor, 0, true, Some(&source_rect as *const RECT));

            // … scaled without distortion, centred in the output …
            let (left, top, right, bottom) = aspect_fit_rect(in_w, in_h, out_w, out_h);
            let dest_rect = RECT {
                left,
                top,
                right,
                bottom,
            };
            vc.VideoProcessorSetStreamDestRect(&processor, 0, true, Some(&dest_rect as *const RECT));

            // … into the whole output surface.
            let target_rect = RECT {
                left: 0,
                top: 0,
                right: out_w as i32,
                bottom: out_h as i32,
            };
            vc.VideoProcessorSetOutputTargetRect(&processor, true, Some(&target_rect as *const RECT));

            // Bars (if the aspect differs) are black. Given as RGB so the
            // driver converts it into the output colour space itself — how a
            // YCbCr background value maps onto a studio-range output (is
            // "0" code 0 or code 16?) is left to the driver and varies.
            let black = D3D11_VIDEO_COLOR {
                Anonymous: D3D11_VIDEO_COLOR_0 {
                    RGBA: D3D11_VIDEO_COLOR_RGBA {
                        R: 0.0,
                        G: 0.0,
                        B: 0.0,
                        A: 1.0,
                    },
                },
            };
            vc.VideoProcessorSetOutputBackgroundColor(&processor, false, &black as *const D3D11_VIDEO_COLOR);

            // Explicit color spaces for correct BGRA→NV12 conversion.
            // WGC delivers BGRA/sRGB; the encoder expects studio-range
            // YCbCr (Rec.709). Setting these prevents washed-out colour
            // shifts that some Intel/AMD drivers produce by default.
            if let Ok(vc1) = vc.cast::<ID3D11VideoContext1>() {
                vc1.VideoProcessorSetStreamColorSpace1(
                    &processor,
                    0,
                    DXGI_COLOR_SPACE_RGB_FULL_G22_NONE_P709,
                );
                vc1.VideoProcessorSetOutputColorSpace1(
                    &processor,
                    DXGI_COLOR_SPACE_YCBCR_STUDIO_G22_LEFT_P709,
                );
            }

            log::info!(
                "[video-processor] {}x{} -> {}x{} (dest rect {},{} {}x{})",
                in_w,
                in_h,
                out_w,
                out_h,
                left,
                top,
                right - left,
                bottom - top
            );

            self.state = Some(ProcessorState {
                in_w,
                in_h,
                enumerator,
                processor,
                output_views: Vec::new(),
            });
        }
        Ok(())
    }

    /// Blit a BGRA source texture into a caller-provided NV12 destination
    /// (an FFmpeg hw_frames_ctx pool texture, recycled across frames).
    pub fn blit_into(
        &mut self,
        bgra_texture: &ID3D11Texture2D,
        nv12_dst: &ID3D11Texture2D,
    ) -> Result<(), String> {
        let _lock = MultithreadGuard::enter(self.multithread.clone());

        let mut src_desc = D3D11_TEXTURE2D_DESC::default();
        unsafe { bgra_texture.GetDesc(&mut src_desc) };
        if src_desc.Width == 0 || src_desc.Height == 0 {
            return Err("source texture has zero size".into());
        }
        self.ensure_processor(src_desc.Width, src_desc.Height)?;
        let Some(state) = self.state.as_mut() else {
            return Err("video processor not initialised".into());
        };

        unsafe {
            let output_view = output_view_for(&self.video_device, state, nv12_dst)?;

            // Per-frame input view — the captured texture changes each
            // iteration.
            let input_view_desc = D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC {
                FourCC: 0,
                ViewDimension: D3D11_VPIV_DIMENSION_TEXTURE2D,
                Anonymous: D3D11_VIDEO_PROCESSOR_INPUT_VIEW_DESC_0 {
                    Texture2D: D3D11_TEX2D_VPIV {
                        MipSlice: 0,
                        ArraySlice: 0,
                    },
                },
            };
            let mut input_view: Option<ID3D11VideoProcessorInputView> = None;
            self.video_device
                .CreateVideoProcessorInputView(
                    bgra_texture,
                    &state.enumerator,
                    &input_view_desc,
                    Some(&mut input_view),
                )
                .map_err(|e| format!("CreateVideoProcessorInputView: {}", e))?;
            let input_view =
                input_view.ok_or("CreateVideoProcessorInputView returned None")?;

            // pInputSurface is ManuallyDrop<Option<T>> in windows-rs 0.61
            // (the C struct doesn't own COM refs); it is released below on
            // every path, including a failed Blt.
            let mut stream = D3D11_VIDEO_PROCESSOR_STREAM {
                Enable: BOOL(1),
                OutputIndex: 0,
                InputFrameOrField: 0,
                PastFrames: 0,
                FutureFrames: 0,
                ppPastSurfaces: std::ptr::null_mut(),
                pInputSurface: ManuallyDrop::new(Some(input_view)),
                ppFutureSurfaces: std::ptr::null_mut(),
                ppPastSurfacesRight: std::ptr::null_mut(),
                pInputSurfaceRight: ManuallyDrop::new(None),
                ppFutureSurfacesRight: std::ptr::null_mut(),
            };

            let blt = self.video_context.VideoProcessorBlt(
                &state.processor,
                &output_view,
                0,
                std::slice::from_ref(&stream),
            );

            // Release the input view's COM ref before looking at the result.
            ManuallyDrop::drop(&mut stream.pInputSurface);

            blt.map_err(|e| format!("VideoProcessorBlt: {}", e))
        }
    }
}

/// Cached (or newly created + cached) output view for a pool texture.
fn output_view_for(
    video_device: &ID3D11VideoDevice,
    state: &mut ProcessorState,
    dst: &ID3D11Texture2D,
) -> Result<ID3D11VideoProcessorOutputView, String> {
    if let Some((_, view)) = state
        .output_views
        .iter()
        .find(|(tex, _)| tex.as_raw() == dst.as_raw())
    {
        return Ok(view.clone());
    }

    let output_view_desc = D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC {
        ViewDimension: D3D11_VPOV_DIMENSION_TEXTURE2D,
        Anonymous: D3D11_VIDEO_PROCESSOR_OUTPUT_VIEW_DESC_0 {
            Texture2D: D3D11_TEX2D_VPOV { MipSlice: 0 },
        },
    };
    let mut output_view: Option<ID3D11VideoProcessorOutputView> = None;
    unsafe {
        video_device
            .CreateVideoProcessorOutputView(
                dst,
                &state.enumerator,
                &output_view_desc,
                Some(&mut output_view),
            )
            .map_err(|e| format!("CreateVideoProcessorOutputView: {}", e))?;
    }
    let output_view = output_view.ok_or("CreateVideoProcessorOutputView returned None")?;

    if state.output_views.len() >= MAX_CACHED_OUTPUT_VIEWS {
        state.output_views.clear();
    }
    state.output_views.push((dst.clone(), output_view.clone()));
    Ok(output_view)
}
