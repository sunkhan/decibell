//! The Windows stream encoders.
//!
//! `Encoder`: FFmpeg hardware encoders fed D3D11 textures: NVENC and AMF
//! take the NV12 pool texture as it is (`AV_PIX_FMT_D3D11`), Quick Sync
//! takes it mapped to a QSV frame (below). `SoftwarePath`: SVT-AV1 fed the
//! same NV12 picture read back from the GPU (`nv12_readback.rs`).
//! `StreamEncoder` is whichever of the two a stream runs on.
//!
//! **Quick Sync** (`h264_qsv` / `hevc_qsv` / `av1_qsv`) follows FFmpeg's own
//! D3D11 → QSV route, the one behind
//! `ffmpeg -f lavfi -i ddagrab -vf hwmap=derive_device=qsv,format=qsv -c:v h264_qsv`
//! (libavfilter/vf_hwmap.c):
//!  1. a QSV device derived from the D3D11 device
//!     (`av_hwdevice_ctx_create_derived`);
//!  2. a QSV frames context derived from the D3D11 NV12 pool
//!     (`av_hwframe_ctx_create_derived`), which the encoder is opened with;
//!  3. per frame, the pool texture the video processor filled is mapped to
//!     a QSV frame (`av_hwframe_map`): no copy, the runtime encodes the
//!     texture itself.
//! The D3D11 pool is **dynamic** (`initial_pool_size = 0`) for Quick Sync:
//! with a fixed pool of render-target textures FFmpeg n8.0 creates the
//! textures lazily but derives the QSV surface table from the (still
//! unset) texture list at once (hwcontext_d3d11va.c d3d11va_frames_init,
//! hwcontext_qsv.c qsv_fixed_frames_derive_to), so no frame would ever map.
//! A dynamic pool takes the per-frame path (qsv_dynamic_pool_map_to), which
//! reads the texture from the frame. Not verified on Intel hardware:
//! `crates/decibell-core/tools/verify-encoders.md`.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use ffmpeg_next::ffi::{
    av_buffer_ref, av_buffer_unref, av_hwdevice_ctx_alloc, av_hwdevice_ctx_create_derived,
    av_hwdevice_ctx_init, av_hwframe_ctx_alloc, av_hwframe_ctx_create_derived, av_hwframe_ctx_init,
    av_hwframe_get_buffer, av_hwframe_map, av_log_get_level, av_log_set_level, AVBufferRef,
    AVHWDeviceContext, AVHWDeviceType, AVHWFramesContext, AVPictureType, AVPixelFormat,
    AV_LOG_QUIET,
};
use ffmpeg_next as ff;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::bitrate_preset::{dynamic_opts, preset_for};
use super::encode_util::{self, NalCodec};
use super::encoder_backend::SOFTWARE_ENCODER;
use super::encoder_software::SoftwareEncoder;
use super::gpu_pipeline::GpuDevice;
use super::nv12_readback::Nv12Readback;
use super::video_processor::VideoProcessor;

/// `AV_HWFRAME_MAP_READ | AV_HWFRAME_MAP_WRITE` (libavutil/hwcontext.h),
/// vf_hwmap.c's default mode for a derived frames context and its maps.
const HWFRAME_MAP_READ_WRITE: i32 = 1 | 2;

/// One owned AVBufferRef reference, unref'd on drop — so every early
/// return out of `Encoder::open` releases what it already allocated.
struct BufferRef(*mut AVBufferRef);

impl BufferRef {
    fn as_ptr(&self) -> *mut AVBufferRef {
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

/// What happened to one captured frame in `send_bgra`.
pub enum FrameOutcome {
    /// Handed to the encoder.
    Submitted,
    /// Dropped for a (probably) transient reason. The caller counts
    /// consecutive drops and gives up past a limit.
    Dropped(String),
}

pub struct Encoder {
    encoder_name: String,
    bitrate_kbps: AtomicU32,
    configured_bitrate_kbps: u32,
    min_bitrate_kbps: u32,
    force_keyframe: Arc<AtomicBool>,
    /// H.264 / HEVC: NAL layout for the keyframe parameter-set check.
    nal_codec: Option<NalCodec>,
    /// Annex B SPS/PPS(/VPS) from the codec extradata, spliced in front of
    /// any keyframe that arrives without them (see `for_each_packet`).
    parameter_sets: Option<Vec<u8>>,
    logged_parameter_set_patch: bool,
    // Field order is drop order: the codec context (holding its own refs
    // on the hw contexts) and the video processor go before our refs, and
    // the QSV contexts (derived from the D3D11 ones) before those.
    context: ff::codec::encoder::Video,
    video_processor: VideoProcessor,
    /// Quick Sync only: the QSV frames context derived from `hw_frames_ref`
    /// and the QSV device derived from `hw_device_ref`.
    qsv_frames_ref: Option<BufferRef>,
    _qsv_device_ref: Option<BufferRef>,
    hw_frames_ref: BufferRef,
    hw_device_ref: BufferRef,
}

// Encoder is owned by the encoder thread which never shares it.
unsafe impl Send for Encoder {}

impl Encoder {
    /// Open the named encoder backed by the given D3D11 device. Width,
    /// height, fps, and bitrate match what's passed to configure(). The
    /// returned encoder accepts NV12 D3D11 textures (caller blits BGRA→
    /// NV12 into the pool's textures via video_processor before calling
    /// send_frame).
    pub fn open(
        gpu: &GpuDevice,
        encoder_name: &str,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        detail: bool,
    ) -> Result<Self, String> {
        let is_qsv = encoder_name.ends_with("_qsv");
        if !(encoder_name.contains("nvenc") || encoder_name.contains("_amf") || is_qsv) {
            return Err(format!("encoder '{}' is not a D3D11 hardware encoder", encoder_name));
        }
        let fps = fps.max(1);

        let codec = ff::codec::encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("encoder not found: {encoder_name}"))?;
        let codec_id = codec.id();

        // ── D3D11VA hwdevice_ctx wrapping the shared device ────────────
        //
        // FFmpeg's AVHWDeviceContext free path (hwdevice_ctx_free →
        // d3d11va_device_uninit) calls ID3D11Device::Release on the stored
        // device pointer, so it owns one COM ref. We clone the device
        // wrapper (AddRef) and wrap it in ManuallyDrop so its Drop doesn't
        // Release — that ref now belongs to FFmpeg. Without this we'd
        // over-Release during teardown and crash with 0xC0000005 from a
        // dangling pointer inside VideoProcessor / video_context drops.
        let hw_device_ref = unsafe {
            let r = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if r.is_null() {
                return Err("av_hwdevice_ctx_alloc(D3D11VA) failed".into());
            }
            let r = BufferRef(r);
            let hw_dev_ctx = (*r.as_ptr()).data as *mut AVHWDeviceContext;
            let d3d_ctx = (*hw_dev_ctx).hwctx as *mut std::ffi::c_void;
            // AVD3D11VADeviceContext.device is the first field — the cast
            // lands on it.
            let device_for_ffmpeg = std::mem::ManuallyDrop::new(gpu.device.clone());
            *(d3d_ctx as *mut *mut std::ffi::c_void) = device_for_ffmpeg.as_raw();

            let rc = av_hwdevice_ctx_init(r.as_ptr());
            if rc < 0 {
                // Do NOT release `device_for_ffmpeg` here: dropping `r` runs
                // hwdevice_ctx_free → d3d11va_device_uninit, which Releases
                // the stored device whether or not init succeeded
                // (hwcontext.c / hwcontext_d3d11va.c). Releasing it here as
                // well was a double Release.
                return Err(format!("av_hwdevice_ctx_init(D3D11VA) failed: {}", rc));
            }
            r
        };

        // ── D3D11VA hwframes_ctx (NV12 pool, size 6, BindFlags ladder) ─
        //
        // NVIDIA's driver accepts different BindFlags combos depending
        // on the texture format, ArraySize, and FFmpeg build. The
        // Tauri-era code documented:
        //   - FFmpeg 8 / Gyan / local builds:  RT|SR works, RT|SR|DEC fails
        //   - FFmpeg 8 / vcpkg (CI):           RT|SR fails, needs RT|SR|DEC
        // We try RT|SR first (cheapest), fall back to RT|SR|DEC, then
        // RT|DEC. AVD3D11VAFramesContext layout: texture* @0, BindFlags
        // @8, MiscFlags @12. Every attempt includes RENDER_TARGET, so
        // FFmpeg allocates one texture per pool slot (ArraySize 1) rather
        // than one texture array — `send_bgra` relies on that.
        const D3D11_BIND_SHADER_RESOURCE: u32 = 0x8;
        const D3D11_BIND_RENDER_TARGET: u32 = 0x20;
        const D3D11_BIND_DECODER: u32 = 0x200;
        let bind_flag_attempts: &[(&str, u32)] = &[
            ("RT|SR", D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE),
            ("RT|SR|DEC", D3D11_BIND_RENDER_TARGET | D3D11_BIND_SHADER_RESOURCE | D3D11_BIND_DECODER),
            ("RT|DEC", D3D11_BIND_RENDER_TARGET | D3D11_BIND_DECODER),
        ];

        let saved_log_level = unsafe { av_log_get_level() };
        unsafe {
            av_log_set_level(AV_LOG_QUIET as i32);
        }

        let mut last_err = String::new();
        let mut hw_frames_ref: Option<BufferRef> = None;
        let mut chosen_label = "";

        for &(label, flags) in bind_flag_attempts {
            unsafe {
                let r = av_hwframe_ctx_alloc(hw_device_ref.as_ptr());
                if r.is_null() {
                    last_err = "av_hwframe_ctx_alloc(D3D11VA) failed".into();
                    continue;
                }
                // Unref'd on drop unless it's kept below.
                let r = BufferRef(r);
                let frames_ctx = (*r.as_ptr()).data as *mut AVHWFramesContext;
                (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_D3D11;
                (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*frames_ctx).width = width as i32;
                (*frames_ctx).height = height as i32;
                // NVENC / AMF: the fixed pool of 6 they have always had.
                // Quick Sync: a dynamic pool (module doc).
                (*frames_ctx).initial_pool_size = if is_qsv { 0 } else { 6 };

                let frames_hwctx = (*frames_ctx).hwctx as *mut u8;
                let bind_flags_ptr = frames_hwctx.add(8) as *mut u32;
                *bind_flags_ptr = flags;

                let mut rc = av_hwframe_ctx_init(r.as_ptr());
                if rc == 0 && is_qsv {
                    // A dynamic pool creates no texture at init, so the
                    // bind flags are only tried by allocating one frame
                    // (it goes back to the pool when `test` drops).
                    let mut test = ff::frame::Video::empty();
                    rc = av_hwframe_get_buffer(r.as_ptr(), test.as_mut_ptr(), 0);
                }
                if rc == 0 {
                    hw_frames_ref = Some(r);
                    chosen_label = label;
                    break;
                }
                last_err = format!("av_hwframe_ctx_init(D3D11VA, {}): {}", label, rc);
            }
        }

        unsafe {
            av_log_set_level(saved_log_level);
        }

        let Some(hw_frames_ref) = hw_frames_ref else {
            return Err(format!(
                "av_hwframe_ctx_init(D3D11VA) failed for all BindFlags combos; last: {}",
                last_err
            ));
        };

        log::info!(
            "[encoder] D3D11VA hw_frames_ctx initialized ({}x{}, pool={}, BindFlags={})",
            width,
            height,
            if is_qsv { "dynamic" } else { "6" },
            chosen_label
        );

        // ── Quick Sync: the QSV device and frames derived from D3D11 ───
        //
        // hwcontext_qsv.c: the derived device creates the oneVPL / Media
        // SDK session on the D3D11 device's adapter (it fails here on an
        // adapter that is not Intel's, or without the Intel media driver);
        // the derived frames context registers the D3D11 pool as the
        // session's video memory (render-target textures become
        // MFX_MEMTYPE_VIDEO_MEMORY_PROCESSOR_TARGET).
        let (qsv_device_ref, qsv_frames_ref) = if is_qsv {
            unsafe {
                let mut dev: *mut AVBufferRef = std::ptr::null_mut();
                let rc = av_hwdevice_ctx_create_derived(
                    &mut dev,
                    AVHWDeviceType::AV_HWDEVICE_TYPE_QSV,
                    hw_device_ref.as_ptr(),
                    0,
                );
                if rc < 0 || dev.is_null() {
                    return Err(format!("av_hwdevice_ctx_create_derived(QSV from D3D11VA) failed: {rc}"));
                }
                let dev = BufferRef(dev);
                let mut frames: *mut AVBufferRef = std::ptr::null_mut();
                let rc = av_hwframe_ctx_create_derived(
                    &mut frames,
                    AVPixelFormat::AV_PIX_FMT_QSV,
                    dev.as_ptr(),
                    hw_frames_ref.as_ptr(),
                    HWFRAME_MAP_READ_WRITE,
                );
                if rc < 0 || frames.is_null() {
                    return Err(format!("av_hwframe_ctx_create_derived(QSV frames) failed: {rc}"));
                }
                log::info!("[encoder] QSV device and frames derived from the D3D11VA pool");
                (Some(dev), Some(BufferRef(frames)))
            }
        } else {
            (None, None)
        };

        // ── Build encoder context + apply low-latency preset opts ──────
        let mut context = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("encoder().video(): {e:?}"))?;

        // GOP: keyframe every 4 seconds plus on-demand via force_keyframe
        // (a `detail` stream: once a minute, see DETAIL_KEYFRAME_SECS).
        let gop = fps * if detail { super::bitrate_preset::DETAIL_KEYFRAME_SECS } else { 4 };
        context.set_width(width);
        context.set_height(height);
        context.set_frame_rate(Some(ff::Rational::new(fps as i32, 1)));
        context.set_time_base(ff::Rational::new(1, fps as i32));
        context.set_bit_rate((bitrate_kbps as usize) * 1000);
        context.set_max_bit_rate((bitrate_kbps as usize) * 1000);
        context.set_gop(gop);
        context.set_max_b_frames(0);

        // Hook the hw contexts into the codec context (it takes its own
        // references, released by avcodec_free_context). pix_fmt = D3D11
        // with the D3D11 pool for NVENC/AMF; QSV with the derived QSV
        // contexts for Quick Sync (qsvenc.c reads its session and the
        // IN_VIDEO_MEMORY pattern from hw_frames_ctx).
        unsafe {
            let ctx_ptr = context.as_mut_ptr();
            match (&qsv_device_ref, &qsv_frames_ref) {
                (Some(dev), Some(frames)) => {
                    (*ctx_ptr).pix_fmt = AVPixelFormat::AV_PIX_FMT_QSV;
                    (*ctx_ptr).hw_device_ctx = av_buffer_ref(dev.as_ptr());
                    (*ctx_ptr).hw_frames_ctx = av_buffer_ref(frames.as_ptr());
                }
                _ => {
                    (*ctx_ptr).pix_fmt = AVPixelFormat::AV_PIX_FMT_D3D11;
                    (*ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ref.as_ptr());
                    (*ctx_ptr).hw_frames_ctx = av_buffer_ref(hw_frames_ref.as_ptr());
                }
            }
            // VBV: four frames of headroom for rate control, at most 400 ms.
            (*ctx_ptr).rc_buffer_size = super::encode_util::vbv_bits(bitrate_kbps, fps);

            // AV1 GLOBAL_HEADER: NVENC then also fills extradata with the
            // sequence header (nvenc_setup_extradata). It does NOT move the
            // header out of band — av1_nvenc repeats it in-band on every
            // keyframe unconditionally (repeatSeqHdr = 1), and that in-band
            // copy is what receivers decode from: this native path never
            // ships extradata. amfenc ignores the flag entirely (it always
            // fills extradata; in-band headers follow header_insertion_mode,
            // set in the AMF presets). Kept because NVENC AV1 sessions have
            // only ever been validated with it set.
            // Not for Quick Sync: qsvenc_av1.c would then need the
            // extract_extradata bitstream filter and run every packet
            // through it, for extradata nothing here ships.
            if codec_id == ff::codec::Id::AV1 && !is_qsv {
                const AV_CODEC_FLAG_GLOBAL_HEADER: i32 = 1 << 22;
                (*ctx_ptr).flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
            }
        }

        context.set_colorspace(ff::color::Space::BT709);
        context.set_color_range(ff::color::Range::MPEG);

        // Open with low-latency preset options. An invalid option VALUE
        // fails avcodec_open2 (→ the renderer falls back to WebCodecs); an
        // unknown NAME is silently ignored, so names are tested against the
        // FFmpeg option tables in bitrate_preset.rs.
        let mut opts = ff::Dictionary::new();
        for (k, v) in preset_for(encoder_name).opts {
            opts.set(k, v);
        }
        for (k, v) in dynamic_opts(encoder_name, gop) {
            opts.set(k, &v);
        }
        if detail {
            for (k, v) in super::bitrate_preset::detail_opts(encoder_name) {
                opts.set(k, v);
            }
        }
        // On failure `context` is freed inside open_with (dropping its hw
        // refs); our own refs drop with the BufferRef guards.
        let context = context
            .open_with(opts)
            .map_err(|e| format!("avcodec_open2 ({encoder_name}): {e:?}"))?;

        // Parameter-set safety net (H.264 / HEVC). AMF doesn't repeat
        // SPS/PPS (VPS) on every keyframe by default and its extradata is
        // always Annex B; NVENC repeats them in-band and leaves extradata
        // empty without GLOBAL_HEADER, so the net simply stays off there.
        let nal_codec = match codec_id {
            ff::codec::Id::H264 => Some(NalCodec::H264),
            ff::codec::Id::HEVC => Some(NalCodec::Hevc),
            _ => None,
        };
        let parameter_sets = nal_codec.and_then(|nc| unsafe {
            let p = context.as_ptr();
            let data = (*p).extradata;
            let size = (*p).extradata_size;
            if data.is_null() || size <= 0 {
                return None;
            }
            let bytes = std::slice::from_raw_parts(data as *const u8, size as usize);
            encode_util::usable_parameter_sets(bytes, nc).then(|| bytes.to_vec())
        });
        if nal_codec.is_some() {
            log::info!(
                "[encoder/{}] keyframe parameter-set fallback: {}",
                encoder_name,
                match &parameter_sets {
                    Some(ps) => format!("armed ({} bytes of Annex B extradata)", ps.len()),
                    None => "off (no Annex B extradata)".to_string(),
                }
            );
        }

        let video_processor = VideoProcessor::new(&gpu.device, width, height, fps)?;

        Ok(Self {
            encoder_name: encoder_name.to_string(),
            bitrate_kbps: AtomicU32::new(bitrate_kbps),
            configured_bitrate_kbps: bitrate_kbps,
            min_bitrate_kbps: 300,
            force_keyframe: Arc::new(AtomicBool::new(false)),
            nal_codec,
            parameter_sets,
            logged_parameter_set_patch: false,
            context,
            video_processor,
            qsv_frames_ref,
            _qsv_device_ref: qsv_device_ref,
            hw_frames_ref,
            hw_device_ref,
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    /// Encode one BGRA source texture. `pts` is in the encoder's
    /// time_base (1/fps seconds) — caller computes it from wall-clock
    /// so frame timestamps track real time rather than encoded-frame
    /// count. (A monotonic frame-count pts breaks the receiver's
    /// wall-clock lag check whenever capture stalls — see comment in
    /// encoder_thread.rs.)
    ///
    /// `on_packet` receives packets drained to make room when the encoder
    /// input is full (EAGAIN) — the same sink `for_each_packet` feeds.
    ///
    /// Transient trouble (pool exhausted, encoder busy, a failed blit or
    /// send) drops the frame: `Ok(FrameOutcome::Dropped)`. `Err` is fatal.
    /// A pending force-keyframe request survives a dropped frame.
    pub fn send_bgra<F>(
        &mut self,
        bgra: &ID3D11Texture2D,
        pts: i64,
        on_packet: &mut F,
    ) -> Result<FrameOutcome, String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        // 1. Allocate an NV12 frame from the encoder's D3D11 pool. Fails
        //    transiently when every pool texture is still held by the
        //    encoder — draining output (the caller does, after this) frees
        //    them.
        let mut frame = ff::frame::Video::empty();
        let rc = unsafe {
            av_hwframe_get_buffer(self.hw_frames_ref.as_ptr(), frame.as_mut_ptr(), 0)
        };
        if rc < 0 {
            return Ok(FrameOutcome::Dropped(format!("av_hwframe_get_buffer: {rc}")));
        }

        // 2. Blit BGRA → NV12 into the pool-allocated texture. The NV12
        //    ID3D11Texture2D* is frame.data[0]; frame.data[1] is the array
        //    slice index.
        let (texture_raw, array_index) = unsafe {
            let f = frame.as_ptr();
            ((*f).data[0] as *mut std::ffi::c_void, (*f).data[1] as usize)
        };
        if texture_raw.is_null() {
            return Ok(FrameOutcome::Dropped(
                "av_hwframe_get_buffer returned a frame with no texture".into(),
            ));
        }
        // The video processor's output view targets slice 0. With our
        // RENDER_TARGET bind flags this FFmpeg (n8.0.1) allocates one
        // texture per pool slot, so the index is always 0. A build that
        // hands out slices of one texture array would make us silently
        // encode slice 0 over and over — refuse instead of corrupting.
        if array_index != 0 {
            return Err(format!(
                "D3D11 pool frame uses texture-array slice {array_index}; \
                 per-slice output views are not implemented"
            ));
        }
        let nv12_texture: ID3D11Texture2D = unsafe {
            match ID3D11Texture2D::from_raw_borrowed(&texture_raw) {
                Some(t) => t.clone(),
                None => {
                    return Ok(FrameOutcome::Dropped(
                        "pool texture pointer could not be borrowed".into(),
                    ))
                }
            }
        };
        if let Err(e) = self.video_processor.blit_into(bgra, &nv12_texture) {
            return Ok(FrameOutcome::Dropped(e));
        }

        // 2b. Quick Sync: the filled texture as a QSV frame. av_hwframe_map
        //     makes `mapped` reference the D3D11 frame (hwcontext_qsv.c
        //     qsv_map_to -> qsv_dynamic_pool_map_to builds its
        //     mfxFrameSurface1 from frame.data[0]); nothing is copied, and
        //     the texture returns to the pool once the encoder lets go of
        //     the mapped frame. As vf_hwmap.c's hwmap_filter_frame does.
        if let Some(qsv_frames) = &self.qsv_frames_ref {
            let mut mapped = ff::frame::Video::empty();
            let rc = unsafe {
                let m = mapped.as_mut_ptr();
                (*m).format = AVPixelFormat::AV_PIX_FMT_QSV as i32;
                (*m).hw_frames_ctx = av_buffer_ref(qsv_frames.as_ptr());
                av_hwframe_map(m, frame.as_ptr(), HWFRAME_MAP_READ_WRITE)
            };
            if rc < 0 {
                return Ok(FrameOutcome::Dropped(format!("av_hwframe_map(D3D11 -> QSV): {rc}")));
            }
            frame = mapped;
        }

        // 3. Set pts, optional keyframe flag, and submit.
        frame.set_pts(Some(pts));
        let force_key = self.force_keyframe.swap(false, Ordering::Relaxed);
        if force_key {
            unsafe {
                (*frame.as_mut_ptr()).pict_type = AVPictureType::AV_PICTURE_TYPE_I;
            }
        }
        let outcome = match self.context.send_frame(&frame) {
            Ok(()) => Ok(FrameOutcome::Submitted),
            // Input full: drain what's ready through the normal sink, then
            // retry once.
            Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => {
                match self.for_each_packet(&mut *on_packet) {
                    Err(e) => Ok(FrameOutcome::Dropped(format!("send_frame EAGAIN, then {e}"))),
                    Ok(()) => match self.context.send_frame(&frame) {
                        Ok(()) => Ok(FrameOutcome::Submitted),
                        Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => {
                            Ok(FrameOutcome::Dropped("encoder input full (EAGAIN)".into()))
                        }
                        Err(e) => classify_send_error(e),
                    },
                }
            }
            Err(e) => classify_send_error(e),
        };
        if force_key && !matches!(outcome, Ok(FrameOutcome::Submitted)) {
            // The keyframe request rides the next frame instead.
            self.force_keyframe.store(true, Ordering::Relaxed);
        }
        outcome
    }

    /// Drain ready packets. Caller invokes after each send_bgra.
    ///
    /// H.264 / HEVC keyframes that arrive without an SPS get the codec's
    /// Annex B parameter sets spliced in (after a leading AUD), so a
    /// watcher can always start decoding on a keyframe — independent of
    /// the driver's header-repeat defaults.
    pub fn for_each_packet<F>(&mut self, mut cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        loop {
            let mut packet = ff::Packet::empty();
            match self.context.receive_packet(&mut packet) {
                Ok(()) => {
                    let data = packet.data().unwrap_or(&[]);
                    let is_key = packet.is_key();
                    let pts = packet.pts().unwrap_or(0);
                    match (is_key, self.nal_codec, self.parameter_sets.as_deref()) {
                        (true, Some(nc), Some(ps))
                            if !encode_util::contains_nal_type(data, nc, nc.sps_type()) =>
                        {
                            if !self.logged_parameter_set_patch {
                                self.logged_parameter_set_patch = true;
                                log::info!(
                                    "[encoder/{}] keyframe without SPS — prepending extradata \
                                     parameter sets (logged once)",
                                    self.encoder_name
                                );
                            }
                            let patched = encode_util::insert_parameter_sets(data, ps, nc);
                            cb(&patched, true, pts);
                        }
                        _ => cb(data, is_key, pts),
                    }
                }
                // ffmpeg-next maps AVERROR(EAGAIN) to Other { errno: EAGAIN }
                // (positive errno, via AVUNERROR) and AVERROR_EOF to Eof.
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => break Ok(()),
                Err(ff::Error::Eof) => break Ok(()),
                Err(e) => break Err(format!("receive_packet: {e:?}")),
            }
        }
    }

    /// End the stream and hand out what is left. Only for a healthy,
    /// short-lived encoder (the probe's test encode, the tests): a live
    /// stream that is stopping drops the encoder instead, because a wedged
    /// one (AMF after a device removal) may never return from this.
    pub fn finish<F>(&mut self, cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        self.context.send_eof().map_err(|e| format!("send_eof: {e:?}"))?;
        self.for_each_packet(cb)
    }

    /// Adjust target bitrate based on NACK ratio. Called once per second
    /// from the encoder thread.
    ///
    /// Inert today: the caller has no NACK readback and passes 0.0, which
    /// asks for +10% — clamped to the configured rate it already runs at,
    /// so nothing changes. If it is ever wired up, note the encoders
    /// differ: NVENC picks up a changed AVCodecContext.bit_rate on the
    /// next frame (nvenc.c reconfig_encoder → nvEncReconfigureEncoder),
    /// while amfenc reads bit_rate only at init — on AMF this mutation
    /// does nothing.
    pub fn maybe_adjust_bitrate(&mut self, nack_ratio: f32) {
        let current = self.bitrate_kbps.load(Ordering::Relaxed);
        let new_rate = if nack_ratio > 0.05 {
            ((current as f32) * 0.75) as u32
        } else if nack_ratio < 0.01 {
            ((current as f32) * 1.10) as u32
        } else {
            return;
        };
        let clamped = new_rate
            .max(self.min_bitrate_kbps)
            .min(self.configured_bitrate_kbps);
        if clamped == current {
            return;
        }
        self.bitrate_kbps.store(clamped, Ordering::Relaxed);
        unsafe {
            let ptr = self.context.as_mut_ptr();
            (*ptr).bit_rate = (clamped as i64) * 1000;
            (*ptr).rc_max_rate = (clamped as i64) * 1500;
        }
        log::info!(
            "[encoder/{}] bitrate adjusted to {} kbps (ratio={:.3})",
            self.encoder_name,
            clamped,
            nack_ratio
        );
    }
}

/// A send_frame error other than EAGAIN: drop the frame unless it is one
/// that can't get better on the next frame.
fn classify_send_error(e: ff::Error) -> Result<FrameOutcome, String> {
    match e {
        // The encoder is flushed / the frame parameters are wrong / the
        // codec says it's broken: every following frame fails the same way.
        ff::Error::Eof | ff::Error::Bug | ff::Error::Bug2 => Err(format!("send_frame: {e:?}")),
        ff::Error::Other { errno } if errno == ff::error::EINVAL => {
            Err(format!("send_frame: {e:?}"))
        }
        _ => Ok(FrameOutcome::Dropped(format!("send_frame: {e:?}"))),
    }
}

/// SVT-AV1 on Windows: the captured texture is converted and scaled to NV12
/// on the GPU, read back without waiting (`Nv12Readback`), and encoded on
/// the CPU. A picture reaches the encoder on the call after the one that
/// submitted it (when the GPU has finished the copy), so this path runs one
/// frame behind the hardware ones.
pub struct SoftwarePath {
    encoder: SoftwareEncoder,
    readback: Nv12Readback,
    force_keyframe: Arc<AtomicBool>,
}

// Owned by the encoder thread which never shares it.
unsafe impl Send for SoftwarePath {}

impl SoftwarePath {
    pub fn open(gpu: &GpuDevice, width: u32, height: u32, fps: u32, bitrate_kbps: u32, detail: bool) -> Result<Self, String> {
        let fps = fps.max(1);
        // The GOP of the hardware path: 4 s, or a minute for a `detail` stream.
        let gop = fps * if detail { super::bitrate_preset::DETAIL_KEYFRAME_SECS } else { 4 };
        let encoder = SoftwareEncoder::open(width, height, fps, bitrate_kbps, gop, detail)?;
        let readback = Nv12Readback::new(gpu, width, height, fps)?;
        Ok(Self { encoder, readback, force_keyframe: Arc::new(AtomicBool::new(false)) })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    /// Readbacks the GPU has not finished yet: while this is not zero the
    /// caller polls again soon instead of waiting for the next capture.
    pub fn pending(&self) -> usize {
        self.readback.pending()
    }

    /// Feed the encoder every picture the GPU has finished. Never waits.
    fn encode_ready(&mut self) -> Result<(), String> {
        let encoder = &mut self.encoder;
        self.readback
            .poll(|y, y_stride, uv, uv_stride, pts, key| encoder.send_nv12(y, y_stride, uv, uv_stride, pts, key))
            .map(|_| ())
    }

    /// Queue `bgra` for conversion + readback and encode what is ready.
    /// Same contract as `Encoder::send_bgra`.
    pub fn send_bgra<F>(&mut self, bgra: &ID3D11Texture2D, pts: i64, on_packet: &mut F) -> Result<FrameOutcome, String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        self.encode_ready()?;
        let force_key = self.force_keyframe.swap(false, Ordering::Relaxed);
        let outcome = match self.readback.submit(bgra, pts, force_key) {
            Ok(true) => FrameOutcome::Submitted,
            Ok(false) => FrameOutcome::Dropped("every readback texture is still in flight".into()),
            Err(e) => FrameOutcome::Dropped(e),
        };
        if force_key && !matches!(outcome, FrameOutcome::Submitted) {
            // The keyframe request rides the next frame instead.
            self.force_keyframe.store(true, Ordering::Relaxed);
        }
        self.encoder.for_each_packet(&mut *on_packet)?;
        Ok(outcome)
    }

    pub fn for_each_packet<F>(&mut self, cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        self.encode_ready()?;
        self.encoder.for_each_packet(cb)
    }

    /// The probe's and the tests' end of stream: wait (bounded) for the
    /// readbacks in flight, encode them, and drain the encoder.
    pub fn finish<F>(&mut self, cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while self.readback.pending() > 0 && std::time::Instant::now() < deadline {
            self.encode_ready()?;
            if self.readback.pending() > 0 {
                std::thread::sleep(std::time::Duration::from_millis(2));
            }
        }
        self.encoder.finish(cb)
    }
}

/// The encoder a Windows stream runs on.
pub enum StreamEncoder {
    Hardware(Encoder),
    Software(SoftwarePath),
}

impl StreamEncoder {
    /// Open `encoder_name` ("h264_nvenc", "av1_amf", "hevc_qsv", …, or
    /// "libsvtav1") on `gpu`.
    pub fn open(
        gpu: &GpuDevice,
        encoder_name: &str,
        width: u32,
        height: u32,
        fps: u32,
        bitrate_kbps: u32,
        detail: bool,
    ) -> Result<Self, String> {
        if encoder_name == SOFTWARE_ENCODER {
            SoftwarePath::open(gpu, width, height, fps, bitrate_kbps, detail).map(StreamEncoder::Software)
        } else {
            Encoder::open(gpu, encoder_name, width, height, fps, bitrate_kbps, detail).map(StreamEncoder::Hardware)
        }
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        match self {
            StreamEncoder::Hardware(e) => e.force_keyframe_handle(),
            StreamEncoder::Software(e) => e.force_keyframe_handle(),
        }
    }

    pub fn send_bgra<F>(&mut self, bgra: &ID3D11Texture2D, pts: i64, on_packet: &mut F) -> Result<FrameOutcome, String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        match self {
            StreamEncoder::Hardware(e) => e.send_bgra(bgra, pts, on_packet),
            StreamEncoder::Software(e) => e.send_bgra(bgra, pts, on_packet),
        }
    }

    pub fn for_each_packet<F>(&mut self, cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        match self {
            StreamEncoder::Hardware(e) => e.for_each_packet(cb),
            StreamEncoder::Software(e) => e.for_each_packet(cb),
        }
    }

    pub fn finish<F>(&mut self, cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        match self {
            StreamEncoder::Hardware(e) => e.finish(cb),
            StreamEncoder::Software(e) => e.finish(cb),
        }
    }

    /// Pictures submitted and not yet encoded for lack of the GPU's answer
    /// (software only; 0 for hardware).
    pub fn pending_readbacks(&self) -> usize {
        match self {
            StreamEncoder::Hardware(_) => 0,
            StreamEncoder::Software(e) => e.pending(),
        }
    }

    /// See `Encoder::maybe_adjust_bitrate` (inert today; software has no
    /// live bitrate change through libsvtav1).
    pub fn maybe_adjust_bitrate(&mut self, nack_ratio: f32) {
        if let StreamEncoder::Hardware(e) = self {
            e.maybe_adjust_bitrate(nack_ratio);
        }
    }
}

/// The Windows side of the encoder harness (`encoder_harness.rs` is the
/// Linux one): the synthetic screen of `encoder_checks.rs` uploaded to a
/// D3D11 texture and pushed through one backend by the stream's own path
/// (`StreamEncoder::send_bgra`), then every check on the packets and, when
/// the FFmpeg build has the decoder, a decode of the whole stream.
///
/// One command per backend, on the hardware that has it:
/// `$env:DECIBELL_ENCODER="qsv"; cargo test --lib forced_backend -- --ignored --nocapture`
/// (`amf`, `nvenc`, `software` likewise;
/// `crates/decibell-core/tools/verify-encoders.md`).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::caps::CodecKind;
    use crate::media::encoder_backend::{self as backend, Backend, Os};
    use crate::media::encoder_checks::{check_packets, decode, DecodeError, Expect, Framing, Packet, Screen};
    use windows::Win32::Graphics::Direct3D11::{
        D3D11_BIND_RENDER_TARGET, D3D11_BIND_SHADER_RESOURCE, D3D11_TEXTURE2D_DESC, D3D11_USAGE_DEFAULT,
    };
    use windows::Win32::Graphics::Dxgi::Common::{DXGI_FORMAT_B8G8R8A8_UNORM, DXGI_SAMPLE_DESC};

    const W: u32 = 1280;
    const H: u32 = 720;
    const FPS: u32 = 30;
    const KBPS: u32 = 3000;
    const FRAMES: usize = 150;

    /// A BGRA texture bound the way a captured frame is.
    fn bgra_texture(gpu: &GpuDevice) -> Result<ID3D11Texture2D, String> {
        let desc = D3D11_TEXTURE2D_DESC {
            Width: W,
            Height: H,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_DEFAULT,
            BindFlags: (D3D11_BIND_SHADER_RESOURCE.0 | D3D11_BIND_RENDER_TARGET.0) as u32,
            CPUAccessFlags: 0,
            MiscFlags: 0,
        };
        let mut tex: Option<ID3D11Texture2D> = None;
        unsafe { gpu.device.CreateTexture2D(&desc, None, Some(&mut tex)) }
            .map_err(|e| format!("CreateTexture2D: {e:?}"))?;
        tex.ok_or_else(|| "CreateTexture2D returned None".to_string())
    }

    /// Five seconds of 720p30 scrolling text at 3 Mbps with a keyframe
    /// requested mid-GOP. `Ok(None)`: the backend cannot open here.
    fn sound(b: Backend, codec: CodecKind) -> Result<Option<String>, String> {
        let adapters = crate::media::encoder_probe::adapters();
        let order = backend::backend_order(Os::Windows, &adapters, Some(b));
        let vendor_id = order.first().and_then(|c| c.adapter).map_or(0, |i| adapters[i].vendor_id);
        let label = backend::backend_label(b, vendor_id);
        let Some(name) = b.encoder_name(codec) else {
            return Ok(None);
        };
        let gpu = GpuDevice::create_for_vendor(vendor_id)?;
        let mut encoder = match StreamEncoder::open(&gpu, name, W, H, FPS, KBPS, false) {
            Ok(e) => e,
            Err(e) => {
                println!("{label} {name}: cannot run here: {e}");
                return Ok(None);
            }
        };
        let keyframe = encoder.force_keyframe_handle();
        let screen = Screen::new(W, H, 3);
        let mut pixels = vec![0u8; screen.frame_bytes()];
        let texture = bgra_texture(&gpu)?;
        let mut packets: Vec<Packet> = Vec::new();
        let mut sink = |data: &[u8], key: bool, pts: i64| {
            packets.push(Packet { data: data.to_vec(), key, pts, description: None });
        };
        for n in 0..FRAMES {
            screen.render(n as u32, &mut pixels);
            unsafe {
                gpu.context.UpdateSubresource(
                    &texture,
                    0,
                    None,
                    pixels.as_ptr() as *const std::ffi::c_void,
                    screen.stride as u32,
                    0,
                );
            }
            if n == 77 {
                keyframe.store(true, Ordering::Relaxed);
            }
            // A dropped frame (the software path's readback ring is full,
            // a pool texture is still with the encoder) is retried: this
            // test wants every frame, where a stream would skip one.
            let mut tries = 0;
            loop {
                match encoder.send_bgra(&texture, n as i64, &mut sink)? {
                    FrameOutcome::Submitted => break,
                    FrameOutcome::Dropped(why) => {
                        tries += 1;
                        if tries > 500 {
                            return Err(format!("{name}: frame {n} kept being dropped: {why}"));
                        }
                        std::thread::sleep(std::time::Duration::from_millis(2));
                        encoder.for_each_packet(&mut sink)?;
                    }
                }
            }
            encoder.for_each_packet(&mut sink)?;
        }
        encoder.finish(&mut sink)?;

        let expect = Expect {
            codec,
            // The Windows encoders send Annex B for H.264 and HEVC.
            framing: Framing::AnnexB,
            frames: FRAMES,
            fps: FPS,
            bitrate_kbps: KBPS,
            forced_at: vec![77],
            forced_within: 3,
            max_overshoot: 0.15,
            min_share: 0.3,
        };
        let summary = check_packets(&packets, &expect).map_err(|e| format!("{name}: {e}"))?;
        let decoded = match decode(codec, Framing::AnnexB, &packets, |_, _, _, _, _| {}) {
            Ok(n) if n == FRAMES => format!("decoded {n}/{FRAMES}"),
            Ok(n) => return Err(format!("{name}: {FRAMES} frames encoded, {n} decoded")),
            Err(DecodeError::NoDecoder(d)) => format!("decode check skipped (no {d} decoder in this FFmpeg build)"),
            Err(DecodeError::Failed(e)) => return Err(format!("{name}: the output does not decode: {e}")),
        };
        Ok(Some(format!(
            "{label} {name} {W}x{H}@{FPS}: {} packets, {} keyframes, {:.0} kbps of {KBPS}, largest packet {:.1}x the average, {decoded} — OK",
            summary.packets, summary.keyframes, summary.kbps, summary.peak_ratio,
        )))
    }

    /// Fails unless at least one codec of the forced backend passes every
    /// check; prints a line per codec.
    #[test]
    #[ignore]
    fn forced_backend_streams_are_sound() {
        let b = backend::forced_backend()
            .unwrap_or_else(|| panic!("set {}=nvenc|amf|qsv|software for this test", backend::FORCE_ENV));
        let mut passed = 0;
        let mut failures = Vec::new();
        for codec in backend::STREAM_CODECS {
            match sound(b, codec) {
                Ok(Some(line)) => {
                    println!("{line}");
                    passed += 1;
                }
                Ok(None) => {}
                Err(e) => failures.push(e),
            }
        }
        assert!(failures.is_empty(), "{} FAILED: {}", b.id(), failures.join("; "));
        assert!(passed > 0, "{}: no codec could be opened on this machine (reasons above)", b.id());
        println!("{}: {passed} codec(s) verified", b.id());
    }
}
