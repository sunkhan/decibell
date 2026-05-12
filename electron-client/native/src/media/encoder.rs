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

use super::bitrate_preset::preset_for;
use super::gpu_pipeline::GpuDevice;
use super::video_processor::VideoProcessor;

pub struct Encoder {
    encoder_name: String,
    width: u32,
    height: u32,
    bitrate_kbps: AtomicU32,
    configured_bitrate_kbps: u32,
    min_bitrate_kbps: u32,
    force_keyframe: Arc<AtomicBool>,
    context: ff::codec::encoder::Video,
    /// D3D11VA hwdevice_ctx buffer ref. Released on drop.
    hw_device_ref: *mut AVBufferRef,
    /// D3D11VA hwframes_ctx buffer ref. Released on drop.
    hw_frames_ref: *mut AVBufferRef,
    video_processor: VideoProcessor,
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

        let codec = ff::codec::encoder::find_by_name(encoder_name)
            .ok_or_else(|| format!("encoder not found: {encoder_name}"))?;
        let codec_id = codec.id();

        // ── D3D11VA hwdevice_ctx wrapping the shared device ────────────
        //
        // FFmpeg's AVHWDeviceContext free callback calls
        // ID3D11Device::Release on the stored device pointer, so it
        // assumes it owns one COM ref. The convention is "you AddRef
        // before assigning". We clone the device wrapper (which calls
        // AddRef internally) and wrap it in ManuallyDrop so its Drop
        // doesn't Release the ref — FFmpeg now owns that ref and will
        // Release on hwdevice_ctx free. Without this we'd over-Release
        // during teardown and crash with 0xC0000005 from a dangling
        // pointer inside VideoProcessor / video_context drops.
        let hw_device_ref = unsafe {
            let r = av_hwdevice_ctx_alloc(AVHWDeviceType::AV_HWDEVICE_TYPE_D3D11VA);
            if r.is_null() {
                return Err("av_hwdevice_ctx_alloc(D3D11VA) failed".into());
            }
            let hw_dev_ctx = (*r).data as *mut AVHWDeviceContext;
            let d3d_ctx = (*hw_dev_ctx).hwctx as *mut std::ffi::c_void;
            // AddRef via clone, then ManuallyDrop so the ref transfers
            // to FFmpeg ownership cleanly. AVD3D11VADeviceContext.device
            // is the first field — the cast lands on it.
            let device_for_ffmpeg = std::mem::ManuallyDrop::new(gpu.device.clone());
            *(d3d_ctx as *mut *mut std::ffi::c_void) = device_for_ffmpeg.as_raw();

            let rc = av_hwdevice_ctx_init(r);
            if rc < 0 {
                // av_hwdevice_ctx_init never succeeded so the free
                // callback won't run. Release the ref we just AddRef'd
                // by extracting from ManuallyDrop and letting it drop.
                let _ = std::mem::ManuallyDrop::into_inner(device_for_ffmpeg);
                let mut rr = r;
                av_buffer_unref(&mut rr);
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
        // @8, MiscFlags @12.
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
        let mut hw_frames_ref: *mut AVBufferRef = std::ptr::null_mut();
        let mut chosen_label = "";

        for &(label, flags) in bind_flag_attempts {
            unsafe {
                let r = av_hwframe_ctx_alloc(hw_device_ref);
                if r.is_null() {
                    last_err = "av_hwframe_ctx_alloc(D3D11VA) failed".into();
                    continue;
                }
                let frames_ctx = (*r).data as *mut AVHWFramesContext;
                (*frames_ctx).format = AVPixelFormat::AV_PIX_FMT_D3D11;
                (*frames_ctx).sw_format = AVPixelFormat::AV_PIX_FMT_NV12;
                (*frames_ctx).width = width as i32;
                (*frames_ctx).height = height as i32;
                (*frames_ctx).initial_pool_size = 6;

                let frames_hwctx = (*frames_ctx).hwctx as *mut u8;
                let bind_flags_ptr = frames_hwctx.add(8) as *mut u32;
                *bind_flags_ptr = flags;

                let rc = av_hwframe_ctx_init(r);
                if rc == 0 {
                    hw_frames_ref = r;
                    chosen_label = label;
                    break;
                }
                last_err = format!("av_hwframe_ctx_init(D3D11VA, {}): {}", label, rc);
                let mut rr = r;
                av_buffer_unref(&mut rr);
            }
        }

        unsafe {
            av_log_set_level(saved_log_level);
        }

        if hw_frames_ref.is_null() {
            unsafe {
                let mut hd = hw_device_ref;
                av_buffer_unref(&mut hd);
            }
            return Err(format!(
                "av_hwframe_ctx_init(D3D11VA) failed for all BindFlags combos; last: {}",
                last_err
            ));
        }

        eprintln!(
            "[encoder] D3D11VA hw_frames_ctx initialized ({}x{}, pool=6, BindFlags={})",
            width, height, chosen_label
        );

        // ── Build encoder context + apply low-latency preset opts ──────
        let mut context = ff::codec::context::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("encoder().video(): {e:?}"))?;

        context.set_width(width);
        context.set_height(height);
        context.set_frame_rate(Some(ff::Rational::new(fps as i32, 1)));
        context.set_time_base(ff::Rational::new(1, fps as i32));
        context.set_bit_rate((bitrate_kbps as usize) * 1000);
        context.set_max_bit_rate((bitrate_kbps as usize) * 1000);
        // GOP: keyframe every 4 seconds plus on-demand via force_keyframe.
        context.set_gop(fps * 4);
        context.set_max_b_frames(0);

        // Hook the hw contexts into the codec context. pix_fmt = D3D11
        // for NVENC/AMF.
        unsafe {
            let ctx_ptr = context.as_mut_ptr();
            (*ctx_ptr).pix_fmt = AVPixelFormat::AV_PIX_FMT_D3D11;
            (*ctx_ptr).hw_device_ctx = av_buffer_ref(hw_device_ref);
            (*ctx_ptr).hw_frames_ctx = av_buffer_ref(hw_frames_ref);
            // VBV ~4 frames of headroom for rate control.
            let vbv_bits = (bitrate_kbps as i32) * 1000 / (fps as i32) * 4;
            (*ctx_ptr).rc_buffer_size = vbv_bits;

            // AV1: GLOBAL_HEADER so FFmpeg emits the av1C config in
            // extradata instead of inline. Receiver's WebCodecs decoder
            // wants the config in description metadata.
            if codec_id == ff::codec::Id::AV1 {
                const AV_CODEC_FLAG_GLOBAL_HEADER: i32 = 1 << 22;
                (*ctx_ptr).flags |= AV_CODEC_FLAG_GLOBAL_HEADER;
            }
        }

        context.set_colorspace(ff::color::Space::BT709);
        context.set_color_range(ff::color::Range::MPEG);

        // Open with low-latency preset options.
        let mut opts = ff::Dictionary::new();
        for (k, v) in preset_for(encoder_name).opts {
            opts.set(k, v);
        }
        let context = context
            .open_with(opts)
            .map_err(|e| format!("avcodec_open2 ({encoder_name}): {e:?}"))?;

        let video_processor = VideoProcessor::new(&gpu.device, width, height, width, height)?;

        Ok(Self {
            encoder_name: encoder_name.to_string(),
            width,
            height,
            bitrate_kbps: AtomicU32::new(bitrate_kbps),
            configured_bitrate_kbps: bitrate_kbps,
            min_bitrate_kbps: 300,
            force_keyframe: Arc::new(AtomicBool::new(false)),
            context,
            hw_device_ref,
            hw_frames_ref,
            video_processor,
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
    pub fn send_bgra(
        &mut self,
        bgra: &ID3D11Texture2D,
        pts: i64,
    ) -> Result<(), String> {
        // 1. Allocate an NV12 frame from the encoder's D3D11 pool.
        let mut frame = ff::frame::Video::empty();
        let rc = unsafe {
            av_hwframe_get_buffer(self.hw_frames_ref, frame.as_mut_ptr(), 0)
        };
        if rc < 0 {
            return Err(format!("av_hwframe_get_buffer: {rc}"));
        }

        // 2. Blit BGRA → NV12 into the pool-allocated texture. The
        //    NV12 ID3D11Texture2D* is stored at frame.data[0] (interpreted
        //    as the texture pointer) with frame.data[1] = array slice index.
        let nv12_texture: ID3D11Texture2D = unsafe {
            let frame_ptr = frame.as_ptr();
            let texture_raw = (*frame_ptr).data[0] as *mut std::ffi::c_void;
            if texture_raw.is_null() {
                return Err("av_hwframe_get_buffer returned frame with null texture".into());
            }
            ID3D11Texture2D::from_raw_borrowed(&texture_raw)
                .ok_or("ID3D11Texture2D::from_raw_borrowed returned None")?
                .clone()
        };
        self.video_processor.blit_into(bgra, &nv12_texture)?;

        // 3. Set pts, optional keyframe flag, and submit.
        frame.set_pts(Some(pts));
        if self.force_keyframe.swap(false, Ordering::Relaxed) {
            unsafe {
                (*frame.as_mut_ptr()).pict_type = AVPictureType::AV_PICTURE_TYPE_I;
            }
        }
        self.context
            .send_frame(&frame)
            .map_err(|e| format!("send_frame: {e:?}"))?;
        Ok(())
    }

    /// Drain ready packets. Caller invokes after each send_bgra and on stop.
    pub fn for_each_packet<F>(&mut self, mut cb: F) -> Result<(), String>
    where
        F: FnMut(&[u8], bool, i64),
    {
        loop {
            let mut packet = ff::Packet::empty();
            match self.context.receive_packet(&mut packet) {
                Ok(_) => {
                    let data = packet.data().unwrap_or(&[]);
                    let is_key = packet.is_key();
                    let pts = packet.pts().unwrap_or(0);
                    cb(data, is_key, pts);
                }
                Err(ff::Error::Other { errno })
                    if errno == ff::ffi::AVERROR(ff::ffi::EAGAIN)
                        || errno == ff::ffi::AVERROR_EOF =>
                {
                    break Ok(());
                }
                Err(ff::Error::Eof) => break Ok(()),
                Err(e) => break Err(format!("receive_packet: {e:?}")),
            }
        }
    }

    /// Adjust target bitrate based on NACK ratio. Called once per second
    /// from the encoder thread.
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
        // NVENC + AMF accept runtime bitrate updates by direct mutation
        // of AVCodecContext.bit_rate. The encoder picks up the new value
        // on the next packet.
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

    pub fn drain(&mut self) -> Result<(), String> {
        self.context
            .send_eof()
            .map_err(|e| format!("send_eof: {e:?}"))?;
        Ok(())
    }
}

impl Drop for Encoder {
    fn drop(&mut self) {
        unsafe {
            if !self.hw_frames_ref.is_null() {
                av_buffer_unref(&mut self.hw_frames_ref);
            }
            if !self.hw_device_ref.is_null() {
                av_buffer_unref(&mut self.hw_device_ref);
            }
        }
    }
}
