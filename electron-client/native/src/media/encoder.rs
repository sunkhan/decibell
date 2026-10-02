//! FFmpeg D3D11VA hardware encoder wrapper.
//!
//! Mining Tauri's encoder.rs `new_d3d11` constructor (~250 LOC of the
//! 1977-line original). Supports NVENC + AMF via the shared D3D11
//! device. QSV is deferred to a follow-up — it needs a D3D11VA→QSV
//! derived hwframes context which adds complexity without
//! corresponding initial-release value.

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::Arc;

use ffmpeg_next::ffi::{
    av_buffer_ref, av_buffer_unref, av_hwdevice_ctx_alloc, av_hwdevice_ctx_init,
    av_hwframe_ctx_alloc, av_hwframe_ctx_init, av_hwframe_get_buffer,
    av_log_get_level, av_log_set_level, AVBufferRef, AVHWDeviceContext,
    AVHWDeviceType, AVHWFramesContext, AVPictureType, AVPixelFormat,
    AV_LOG_QUIET,
};
use ffmpeg_next as ff;
use windows::core::Interface;
use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::bitrate_preset::{dynamic_opts, preset_for};
use super::encode_util::{self, NalCodec};
use super::gpu_pipeline::GpuDevice;
use super::video_processor::VideoProcessor;

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
    // on both hw contexts) and the video processor go before our refs.
    context: ff::codec::encoder::Video,
    video_processor: VideoProcessor,
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
    ) -> Result<Self, String> {
        if !(encoder_name.contains("nvenc") || encoder_name.contains("_amf")) {
            return Err(format!(
                "encoder '{}' not supported by this initial build (QSV requires \
                 D3D11VA→QSV derived hwframes — follow-up)",
                encoder_name
            ));
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
                (*frames_ctx).initial_pool_size = 6;

                let frames_hwctx = (*frames_ctx).hwctx as *mut u8;
                let bind_flags_ptr = frames_hwctx.add(8) as *mut u32;
                *bind_flags_ptr = flags;

                let rc = av_hwframe_ctx_init(r.as_ptr());
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
            "[encoder] D3D11VA hw_frames_ctx initialized ({}x{}, pool=6, BindFlags={})",
            width, height, chosen_label
        );

        // ── Build encoder context + apply low-latency preset opts ──────
        let mut context = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("encoder().video(): {e:?}"))?;

        // GOP: keyframe every 4 seconds plus on-demand via force_keyframe.
        let gop = fps * 4;
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
        // for NVENC/AMF.
        unsafe {
            let ctx_ptr = context.as_mut_ptr();
            (*ctx_ptr).pix_fmt = AVPixelFormat::AV_PIX_FMT_D3D11;
            (*ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ref.as_ptr());
            (*ctx_ptr).hw_frames_ctx = av_buffer_ref(hw_frames_ref.as_ptr());
            // VBV ~4 frames of headroom for rate control.
            let vbv_bits = (bitrate_kbps as i32) * 1000 / (fps as i32) * 4;
            (*ctx_ptr).rc_buffer_size = vbv_bits;

            // AV1 GLOBAL_HEADER: NVENC then also fills extradata with the
            // sequence header (nvenc_setup_extradata). It does NOT move the
            // header out of band — av1_nvenc repeats it in-band on every
            // keyframe unconditionally (repeatSeqHdr = 1), and that in-band
            // copy is what receivers decode from: this native path never
            // ships extradata. amfenc ignores the flag entirely (it always
            // fills extradata; in-band headers follow header_insertion_mode,
            // set in the AMF presets). Kept because NVENC AV1 sessions have
            // only ever been validated with it set.
            if codec_id == ff::codec::Id::AV1 {
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
