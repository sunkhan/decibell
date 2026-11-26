//! VAAPI input for the Linux encoder (AMD radeonsi, Intel iHD / i965): turns
//! a captured BGRA / RGBA CPU frame (PipeWire SHM, the same input NVENC
//! gets) into the NV12 VAAPI surface `h264_vaapi` / `hevc_vaapi` /
//! `av1_vaapi` encode.
//!
//! Two tiers, chosen once when the encoder opens and logged:
//!
//!  1. **GPU convert** (the intended path). The frame is copied once, into a
//!     BGRX / RGBX VAAPI surface (`av_hwframe_transfer_data`), and the
//!     driver's video processor (VPP) converts to NV12 and scales to the
//!     encode size: FFmpeg's `scale_vaapi` filter, i.e. what
//!     `ffmpeg -vf hwupload,scale_vaapi=format=nv12` does. Needs
//!     `--enable-filter=scale_vaapi` in the FFmpeg build.
//!  2. **CPU convert**, when the build has no `scale_vaapi`, the driver
//!     takes no RGB surfaces or has no VPP: swscale converts to NV12 (the
//!     caller's scaler) and the NV12 frame is uploaded. Every VAAPI encoder
//!     driver takes NV12 uploads; it costs a CPU conversion per frame.
//!
//! No DMA-BUF import: PipeWire's DMA-BUF path is not used by the capture
//! (it produced black frames with mesa-allocated buffers on NVIDIA, and an
//! SHM frame has to be read for the thumbnail anyway).
//!
//! **Nothing here has run on AMD or Intel hardware** (only NVIDIA is on
//! hand, and its VA-API driver decodes only). What the code assumes about
//! FFmpeg and the drivers is stated where it is used, with the source;
//! `crates/decibell-core/tools/verify-encoders.md` is the check.
#![cfg(target_os = "linux")]

use std::ffi::{c_void, CString};
use std::ptr;

use ffmpeg_next::sys::*;

/// One owned AVBufferRef, unref'd on drop.
pub(super) struct BufferRef(*mut AVBufferRef);

impl BufferRef {
    pub(super) fn as_ptr(&self) -> *mut AVBufferRef {
        self.0
    }
}

impl Drop for BufferRef {
    fn drop(&mut self) {
        if !self.0.is_null() {
            unsafe { av_buffer_unref(&mut self.0) };
        }
    }
}

/// One owned AVFrame.
struct Frame(*mut AVFrame);

impl Frame {
    fn alloc() -> Result<Self, String> {
        let f = unsafe { av_frame_alloc() };
        if f.is_null() {
            return Err("av_frame_alloc failed".to_string());
        }
        Ok(Self(f))
    }
}

impl Drop for Frame {
    fn drop(&mut self) {
        unsafe { av_frame_free(&mut self.0) };
    }
}

fn averr(rc: i32) -> String {
    ffmpeg_next::Error::from(rc).to_string()
}

/// BGRA-family capture formats as VAAPI surface formats. The X variants
/// (VA_FOURCC_BGRX / RGBX in libavutil/hwcontext_vaapi.c's format map) come
/// first: a monitor capture's 4th byte is padding, often 0, and a video
/// processor that honoured it as alpha would output black. BGRA / RGBA are
/// the fallback for a driver that lists only those.
fn upload_formats(is_bgra: bool) -> [AVPixelFormat; 2] {
    if is_bgra {
        [AVPixelFormat::AV_PIX_FMT_BGR0, AVPixelFormat::AV_PIX_FMT_BGRA]
    } else {
        [AVPixelFormat::AV_PIX_FMT_RGB0, AVPixelFormat::AV_PIX_FMT_RGBA]
    }
}

/// The GPU tier's per-source-size state: the upload pool and the filter
/// graph `buffer -> scale_vaapi -> buffersink`.
struct GpuConvert {
    src_w: u32,
    src_h: u32,
    is_bgra: bool,
    sw_format: AVPixelFormat,
    upload_frames: BufferRef,
    graph: *mut AVFilterGraph,
    src: *mut AVFilterContext,
    sink: *mut AVFilterContext,
}

impl Drop for GpuConvert {
    fn drop(&mut self) {
        // Frees the filter contexts too; they hold their own refs on the
        // frames contexts.
        unsafe { avfilter_graph_free(&mut self.graph) };
    }
}

impl GpuConvert {
    fn build(
        device: &BufferRef,
        src_w: u32,
        src_h: u32,
        is_bgra: bool,
        enc_w: u32,
        enc_h: u32,
        fps: u32,
    ) -> Result<Self, String> {
        let scale = unsafe { avfilter_get_by_name(c"scale_vaapi".as_ptr()) };
        let buffer = unsafe { avfilter_get_by_name(c"buffer".as_ptr()) };
        let buffersink = unsafe { avfilter_get_by_name(c"buffersink".as_ptr()) };
        if scale.is_null() || buffer.is_null() || buffersink.is_null() {
            return Err("this FFmpeg build has no scale_vaapi filter".to_string());
        }

        // Upload pool: RGB surfaces at the capture's size. A dynamic pool
        // (initial_pool_size 0), as vf_hwupload.c makes by default.
        let mut last_err = String::new();
        let mut upload: Option<(BufferRef, AVPixelFormat)> = None;
        for sw_format in upload_formats(is_bgra) {
            unsafe {
                let r = av_hwframe_ctx_alloc(device.as_ptr());
                if r.is_null() {
                    return Err("av_hwframe_ctx_alloc(VAAPI upload) failed".to_string());
                }
                let r = BufferRef(r);
                let fc = (*r.as_ptr()).data as *mut AVHWFramesContext;
                (*fc).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
                (*fc).sw_format = sw_format;
                (*fc).width = src_w as i32;
                (*fc).height = src_h as i32;
                (*fc).initial_pool_size = 0;
                let rc = av_hwframe_ctx_init(r.as_ptr());
                if rc == 0 {
                    upload = Some((r, sw_format));
                    break;
                }
                last_err = format!("{sw_format:?} surfaces: {}", averr(rc));
            }
        }
        let Some((upload_frames, sw_format)) = upload else {
            return Err(format!("the driver takes no RGB upload surfaces ({last_err})"));
        };

        unsafe {
            let mut graph = avfilter_graph_alloc();
            if graph.is_null() {
                return Err("avfilter_graph_alloc failed".to_string());
            }
            // From here `graph` is freed on every error path.
            let fail = |graph: &mut *mut AVFilterGraph, what: String| -> Result<Self, String> {
                avfilter_graph_free(graph);
                Err(what)
            };

            let src = avfilter_graph_alloc_filter(graph, buffer, c"in".as_ptr());
            if src.is_null() {
                return fail(&mut graph, "alloc buffer source failed".to_string());
            }
            let params = av_buffersrc_parameters_alloc();
            if params.is_null() {
                return fail(&mut graph, "av_buffersrc_parameters_alloc failed".to_string());
            }
            (*params).format = AVPixelFormat::AV_PIX_FMT_VAAPI as i32;
            (*params).width = src_w as i32;
            (*params).height = src_h as i32;
            (*params).time_base = AVRational { num: 1, den: fps.max(1) as i32 };
            (*params).frame_rate = AVRational { num: fps.max(1) as i32, den: 1 };
            // av_buffersrc_parameters_set takes its own reference.
            (*params).hw_frames_ctx = upload_frames.as_ptr();
            // The capture is full-range sRGB; vaapi_vpp.c reads these from
            // the link / frames to pick the VPP input colour standard.
            (*params).color_space = AVColorSpace::AVCOL_SPC_RGB;
            (*params).color_range = AVColorRange::AVCOL_RANGE_JPEG;
            let rc = av_buffersrc_parameters_set(src, params);
            av_free(params as *mut c_void);
            if rc < 0 {
                return fail(&mut graph, format!("buffer source parameters: {}", averr(rc)));
            }
            let rc = avfilter_init_dict(src, ptr::null_mut());
            if rc < 0 {
                return fail(&mut graph, format!("buffer source init: {}", averr(rc)));
            }

            // BT.709 limited range out, as the encoder's VUI says (and as
            // NVENC's own BGRA conversion produces). Option names:
            // libavfilter/vf_scale_vaapi.c.
            let args = CString::new(format!(
                "w={enc_w}:h={enc_h}:format=nv12:out_color_matrix=bt709:out_range=tv:\
                 out_color_primaries=bt709:out_color_transfer=bt709"
            ))
            .map_err(|e| e.to_string())?;
            let mut scale_ctx: *mut AVFilterContext = ptr::null_mut();
            let rc = avfilter_graph_create_filter(
                &mut scale_ctx,
                scale,
                c"scale".as_ptr(),
                args.as_ptr(),
                ptr::null_mut(),
                graph,
            );
            if rc < 0 {
                return fail(&mut graph, format!("scale_vaapi: {}", averr(rc)));
            }
            let mut sink: *mut AVFilterContext = ptr::null_mut();
            let rc = avfilter_graph_create_filter(
                &mut sink,
                buffersink,
                c"out".as_ptr(),
                ptr::null(),
                ptr::null_mut(),
                graph,
            );
            if rc < 0 {
                return fail(&mut graph, format!("buffersink: {}", averr(rc)));
            }
            let rc = avfilter_link(src, 0, scale_ctx, 0);
            if rc < 0 {
                return fail(&mut graph, format!("link buffer -> scale_vaapi: {}", averr(rc)));
            }
            let rc = avfilter_link(scale_ctx, 0, sink, 0);
            if rc < 0 {
                return fail(&mut graph, format!("link scale_vaapi -> sink: {}", averr(rc)));
            }
            // Where a driver without VPP (VAEntrypointVideoProc) or without
            // RGB -> NV12 in it says so: vaapi_vpp.c's config_output.
            let rc = avfilter_graph_config(graph, ptr::null_mut());
            if rc < 0 {
                return fail(&mut graph, format!("the driver's video processor refused BGRA -> NV12: {}", averr(rc)));
            }
            if av_buffersink_get_hw_frames_ctx(sink).is_null() {
                return fail(&mut graph, "scale_vaapi produced no hardware frames context".to_string());
            }
            Ok(Self { src_w, src_h, is_bgra, sw_format, upload_frames, graph, src, sink })
        }
    }

    /// The NV12 pool scale_vaapi writes into (owned by the graph).
    fn output_frames(&self) -> *mut AVBufferRef {
        unsafe { av_buffersink_get_hw_frames_ctx(self.sink) }
    }
}

enum Tier {
    Gpu(GpuConvert),
    /// NV12 surfaces at the encode size, uploaded from the CPU.
    Cpu { frames: BufferRef },
}

pub(super) struct VaapiInput {
    tier: Tier,
    enc_w: u32,
    enc_h: u32,
    fps: u32,
    /// The surface handed to the encoder; valid until the next frame.
    out: Frame,
    /// Reused shells: the upload surface, and a non-owning view of the
    /// caller's pixels for `av_hwframe_transfer_data`.
    hw: Frame,
    view: Frame,
    // Last: every frames context above holds its own device reference.
    device: BufferRef,
}

impl VaapiInput {
    /// Open the render node and set up the conversion for a `src_w`×`src_h`
    /// capture encoded at `enc_w`×`enc_h`. `render_node` None lets libva
    /// pick its default (the first DRM node / the display's).
    pub(super) fn open(
        render_node: Option<&str>,
        enc_w: u32,
        enc_h: u32,
        src_w: u32,
        src_h: u32,
        fps: u32,
    ) -> Result<Self, String> {
        let node = match render_node {
            Some(n) => Some(CString::new(n).map_err(|e| e.to_string())?),
            None => None,
        };
        let device = unsafe {
            let mut dev: *mut AVBufferRef = ptr::null_mut();
            // hwcontext_vaapi.c opens the DRM node, vaInitialize()s it and
            // reads the driver's quirks. Fails here when no VA driver
            // serves the node (missing mesa-va-drivers / intel-media-driver).
            let rc = av_hwdevice_ctx_create(
                &mut dev,
                AVHWDeviceType::AV_HWDEVICE_TYPE_VAAPI,
                node.as_ref().map_or(ptr::null(), |n| n.as_ptr()),
                ptr::null_mut(),
                0,
            );
            if rc < 0 || dev.is_null() {
                return Err(format!(
                    "no VA-API driver on {}: {}",
                    render_node.unwrap_or("the default device"),
                    averr(rc)
                ));
            }
            BufferRef(dev)
        };

        // DECIBELL_VAAPI_CONVERT=cpu forces the second tier (to compare, or
        // to work around a broken video processor).
        let force_cpu = std::env::var("DECIBELL_VAAPI_CONVERT").is_ok_and(|v| v.eq_ignore_ascii_case("cpu"));
        let tier = if force_cpu {
            log::info!("[encoder/vaapi] DECIBELL_VAAPI_CONVERT=cpu: converting on the CPU");
            None
        } else {
            match GpuConvert::build(&device, src_w, src_h, true, enc_w, enc_h, fps) {
                Ok(g) => {
                    log::info!(
                        "[encoder/vaapi] GPU convert: {src_w}x{src_h} {:?} surfaces -> scale_vaapi -> {enc_w}x{enc_h} NV12",
                        g.sw_format
                    );
                    Some(Tier::Gpu(g))
                }
                Err(e) => {
                    log::warn!("[encoder/vaapi] GPU convert unavailable ({e}); converting to NV12 on the CPU");
                    None
                }
            }
        };
        let tier = match tier {
            Some(t) => t,
            None => unsafe {
                let r = av_hwframe_ctx_alloc(device.as_ptr());
                if r.is_null() {
                    return Err("av_hwframe_ctx_alloc(VAAPI NV12) failed".to_string());
                }
                let r = BufferRef(r);
                let fc = (*r.as_ptr()).data as *mut AVHWFramesContext;
                (*fc).format = AVPixelFormat::AV_PIX_FMT_VAAPI;
                (*fc).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*fc).width = enc_w as i32;
                (*fc).height = enc_h as i32;
                (*fc).initial_pool_size = 0;
                let rc = av_hwframe_ctx_init(r.as_ptr());
                if rc < 0 {
                    return Err(format!("the driver takes no NV12 surfaces: {}", averr(rc)));
                }
                Tier::Cpu { frames: r }
            },
        };
        Ok(Self { tier, enc_w, enc_h, fps, out: Frame::alloc()?, hw: Frame::alloc()?, view: Frame::alloc()?, device })
    }

    /// Does the GPU convert (tier 1)? Otherwise the caller converts to NV12
    /// and calls `upload_nv12`.
    pub(super) fn gpu_convert(&self) -> bool {
        matches!(self.tier, Tier::Gpu(_))
    }

    pub(super) fn tier_name(&self) -> &'static str {
        if self.gpu_convert() {
            "BGRA upload + VPP convert"
        } else {
            "CPU convert + NV12 upload"
        }
    }

    pub(super) fn device(&self) -> *mut AVBufferRef {
        self.device.as_ptr()
    }

    /// The frames context the encoder is opened with (NV12 at the encode
    /// size). After a capture size change the graph is rebuilt and frames
    /// come from a new pool on the same device, with the same format and
    /// size: the encoder only reads the surface id (vaapi_encode.c takes
    /// `frame->data[3]`), as when the ffmpeg tool reconfigures a filter
    /// graph in front of a VAAPI encoder.
    pub(super) fn encoder_frames(&self) -> *mut AVBufferRef {
        match &self.tier {
            Tier::Gpu(g) => g.output_frames(),
            Tier::Cpu { frames } => frames.as_ptr(),
        }
    }

    /// Tier 1: upload the captured frame and let the VPP convert + scale.
    /// Returns the NV12 surface (owned here, valid until the next call).
    /// `data` was validated by the caller: `stride >= w * 4` and at least
    /// `(h - 1) * stride + w * 4` bytes.
    pub(super) fn convert(
        &mut self,
        data: &[u8],
        w: u32,
        h: u32,
        stride: usize,
        is_bgra: bool,
    ) -> Result<*mut AVFrame, String> {
        let rebuild = match &self.tier {
            Tier::Gpu(g) => g.src_w != w || g.src_h != h || g.is_bgra != is_bgra,
            Tier::Cpu { .. } => return Err("VAAPI input is in CPU-convert mode".to_string()),
        };
        if rebuild {
            // The window was resized, or the first frame is not what the
            // open assumed (size / byte order).
            let g = GpuConvert::build(&self.device, w, h, is_bgra, self.enc_w, self.enc_h, self.fps)?;
            log::info!("[encoder/vaapi] capture is now {w}x{h} {:?}; conversion rebuilt", g.sw_format);
            self.tier = Tier::Gpu(g);
        }
        let Tier::Gpu(g) = &self.tier else { unreachable!() };
        unsafe {
            let hw = self.hw.0;
            av_frame_unref(hw);
            let rc = av_hwframe_get_buffer(g.upload_frames.as_ptr(), hw, 0);
            if rc < 0 {
                return Err(format!("no upload surface: {}", averr(rc)));
            }
            // A view of the caller's pixels: transfer_data only reads
            // data / linesize / format / size (hwcontext_vaapi.c maps the
            // surface and av_frame_copy()s into it). One copy, CPU -> the
            // surface's mapping.
            let view = self.view.0;
            av_frame_unref(view);
            (*view).format = g.sw_format as i32;
            (*view).width = w as i32;
            (*view).height = h as i32;
            (*view).data[0] = data.as_ptr() as *mut u8;
            (*view).linesize[0] = stride as i32;
            let rc = av_hwframe_transfer_data(hw, view, 0);
            (*view).data[0] = ptr::null_mut();
            if rc < 0 {
                return Err(format!("upload to the surface: {}", averr(rc)));
            }
            (*hw).color_range = AVColorRange::AVCOL_RANGE_JPEG;
            (*hw).colorspace = AVColorSpace::AVCOL_SPC_RGB;
            (*hw).color_primaries = AVColorPrimaries::AVCOL_PRI_BT709;
            (*hw).color_trc = AVColorTransferCharacteristic::AVCOL_TRC_IEC61966_2_1;
            // The source takes over the frame's references (flags 0).
            let rc = av_buffersrc_add_frame_flags(g.src, hw, 0);
            if rc < 0 {
                return Err(format!("video processor input: {}", averr(rc)));
            }
            let out = self.out.0;
            av_frame_unref(out);
            // scale_vaapi is one frame in, one frame out, synchronously
            // (vaapi_vpp.c renders in filter_frame).
            let rc = av_buffersink_get_frame(g.sink, out);
            if rc < 0 {
                return Err(format!("video processor output: {}", averr(rc)));
            }
            Ok(out)
        }
    }

    /// Tier 2: upload an NV12 CPU frame at the encode size.
    pub(super) fn upload_nv12(&mut self, nv12: *const AVFrame) -> Result<*mut AVFrame, String> {
        let Tier::Cpu { frames } = &self.tier else {
            return Err("VAAPI input is in GPU-convert mode".to_string());
        };
        unsafe {
            let out = self.out.0;
            av_frame_unref(out);
            let rc = av_hwframe_get_buffer(frames.as_ptr(), out, 0);
            if rc < 0 {
                return Err(format!("no NV12 surface: {}", averr(rc)));
            }
            let rc = av_hwframe_transfer_data(out, nv12, 0);
            if rc < 0 {
                return Err(format!("NV12 upload: {}", averr(rc)));
            }
            Ok(out)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn padding_formats_are_tried_before_alpha_ones() {
        assert_eq!(upload_formats(true), [AVPixelFormat::AV_PIX_FMT_BGR0, AVPixelFormat::AV_PIX_FMT_BGRA]);
        assert_eq!(upload_formats(false), [AVPixelFormat::AV_PIX_FMT_RGB0, AVPixelFormat::AV_PIX_FMT_RGBA]);
    }

    /// A node with no VA-API driver (or none at all) is an error with the
    /// node's name in it, never a crash: this is what the probe logs.
    #[test]
    fn a_missing_render_node_is_a_plain_error() {
        ffmpeg_next::init().unwrap();
        let err = VaapiInput::open(Some("/dev/dri/renderD999-none"), 640, 360, 640, 360, 30).err().expect("no such node");
        assert!(err.contains("renderD999-none"), "{err}");
    }
}
