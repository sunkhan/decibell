//! Linux native encoder thread — the Linux counterpart to the Windows
//! `encoder_thread`. Pulls captured frames (CPU `RawFrame` from PipeWire
//! SHM / wlr-screencopy, or zero-copy `DmaBufFrame` when the compositor
//! advertises DMA-BUF) and drives the FFmpeg `H264Encoder`
//! (NVENC/VAAPI/libx264), draining encoded packets into:
//!  - the UDP `VideoSender` (the wire — same path renderer-encoded
//!    chunks took), with the HEVC/AV1 keyframe description prefixed via
//!    `WIRE_DESCRIPTION_MAGIC` exactly like `send_video_frame` does;
//!  - the renderer self-preview TSFN via `events::send_stream_frame`
//!    keyed by the local username.
//!
//! Runs on one OS thread; all FFmpeg + GPU state stays on it.
//!
//! Note on "zero-copy": the revived `capture_pipewire` currently does not
//! advertise DMA-BUF (the gpu_receiver stays empty) because of the
//! NVIDIA-proprietary + mesa-allocated-DMA-BUF black-frame issue
//! documented in `gpu_interop`. The live path is therefore CPU-captured
//! BGRA → `encode_bgra_direct`, where NVENC still does BGRA→NV12 on the
//! GPU (no CPU `sws_scale`). The DMA-BUF branch below is kept wired so
//! it lights up automatically if a capture backend starts delivering
//! `DmaBufFrame`s again.
#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::RecvTimeoutError;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use jpeg_encoder::{ColorType, Encoder as JpegEncoder};

use super::capture::{CaptureOutput, PixelFormat, RawFrame};
use super::encoder_linux::{EncodedFrame, EncoderConfig, H264Encoder};
use super::gpu_interop::{GpuBackendType, GpuContext};
use super::video_pipeline::VideoSender;
use crate::events;
use crate::media::video_packet::WIRE_DESCRIPTION_MAGIC;

const THUMBNAIL_INTERVAL: Duration = Duration::from_secs(3);
/// GOP length in seconds — keyframe every 2s, matching the renderer path.
const KEYFRAME_INTERVAL_SECS: u32 = 2;

pub struct LinuxEncoderThread {
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub struct LinuxEncoderThreadConfig {
    pub target_codec: crate::media::caps::CodecKind,
    /// Wire codec byte (1=H264_HW, 2=H264_SW, 3=H265, 4=AV1) stamped into
    /// every UdpVideoPacket so receivers pick the right decoder.
    pub codec_wire_byte: u8,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub local_username: String,
    pub video_sender: Arc<VideoSender>,
    /// Bounded depth=1 drop-newest (via try_send) — a slow server never
    /// back-pressures the encoder thread.
    pub thumbnail_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

impl LinuxEncoderThread {
    pub fn start(
        cfg: LinuxEncoderThreadConfig,
        capture: CaptureOutput,
    ) -> Result<Self, String> {
        let config = EncoderConfig {
            width: cfg.width,
            height: cfg.height,
            fps: cfg.fps,
            bitrate_kbps: cfg.bitrate_kbps,
            keyframe_interval_secs: KEYFRAME_INTERVAL_SECS,
        };
        // Open the encoder up front so a failure surfaces here in start()
        // rather than deep inside the thread. `find_hw_encoder` logs which
        // backend it picked (h264_nvenc / *_vaapi / libx264) — that line
        // is the signal for whether we're on the GPU fast path or the CPU
        // sws_scale fallback.
        let encoder = H264Encoder::new(cfg.target_codec, &config)?;

        let stop = Arc::new(AtomicBool::new(false));
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let fk_t = force_keyframe.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-encoder-linux".to_string())
            .spawn(move || {
                // Catch panics so a codec-specific bug (e.g. malformed
                // bitstream parsing) logs a clear message instead of
                // silently unwinding the thread, dropping the capture
                // receiver, and stopping the whole stream with no trace.
                let r = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_encode_loop(encoder, config, capture, &cfg, &stop_t, &fk_t);
                }));
                if let Err(panic) = r {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "<non-string panic payload>".to_string());
                    log::error!("[encoder-linux] encode thread PANICKED: {msg}");
                }
            })
            .map_err(|e| format!("spawn linux encoder thread: {e}"))?;

        Ok(Self {
            stop,
            force_keyframe,
            thread: Some(thread),
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Packetise + ship one encoded frame: UDP wire (with HEVC/AV1 keyframe
/// description prefix) and the renderer self-preview TSFN.
fn emit_encoded(
    ef: &EncodedFrame,
    cfg: &LinuxEncoderThreadConfig,
    stream_start: Instant,
    frames_sent: &mut u32,
) {
    let codec = cfg.codec_wire_byte;

    // Wire: HEVC(3)/AV1(4) keyframes carry an out-of-band decoder
    // description (hvcC / av1C). Prefix it with the magic tag + BE length
    // so receivers strip it back out — identical to the renderer path's
    // send_video_frame. H.264 carries SPS/PPS inline in Annex B.
    let prefixed;
    let wire: &[u8] = if ef.is_keyframe && (codec == 3 || codec == 4) {
        if let Some(desc) = ef.avcc_description.as_ref() {
            let mut buf = Vec::with_capacity(
                WIRE_DESCRIPTION_MAGIC.len() + 4 + desc.len() + ef.data.len(),
            );
            buf.extend_from_slice(&WIRE_DESCRIPTION_MAGIC);
            buf.extend_from_slice(&(desc.len() as u32).to_be_bytes());
            buf.extend_from_slice(desc);
            buf.extend_from_slice(&ef.data);
            prefixed = buf;
            &prefixed
        } else {
            &ef.data
        }
    } else {
        &ef.data
    };
    cfg.video_sender.send_frame(codec, ef.is_keyframe, wire);

    // Self-preview: same encoded bytes to the renderer keyed by local
    // username (StreamVideoPlayer renders the user's own tile via the
    // unified stream-frame bus). Description travels as a separate field.
    events::send_stream_frame(events::StreamFrame {
        username: cfg.local_username.clone(),
        codec,
        keyframe: ef.is_keyframe,
        timestamp: stream_start.elapsed().as_micros() as i64,
        data: ef.data.clone(),
        description: ef.avcc_description.clone(),
    });
    *frames_sent += 1;
}

fn run_encode_loop(
    mut encoder: H264Encoder,
    config: EncoderConfig,
    capture: CaptureOutput,
    cfg: &LinuxEncoderThreadConfig,
    stop: &AtomicBool,
    force_keyframe: &AtomicBool,
) {
    let frame_rx = capture.receiver;
    let gpu_frame_rx = capture.gpu_receiver;

    // Lazily created on the first DMA-BUF frame; stays None on the CPU
    // path so no GPU context is allocated when it isn't needed.
    let mut gpu_ctx: Option<GpuContext> = None;
    let mut gpu_initialized = false;

    // Re-encode the last frame if capture stalls (static screen) so the
    // GOP keeps flowing and receivers don't time out the stream.
    let repeat_interval = Duration::from_millis((1000 / cfg.fps.max(1)).max(1) as u64);
    let mut last_frame: Option<RawFrame> = None;
    let mut last_frame_time = Instant::now();

    let stream_start = Instant::now();
    let mut last_telemetry = Instant::now();
    let mut last_thumbnail = Instant::now() - THUMBNAIL_INTERVAL;
    let mut frames_sent = 0u32;
    let mut thumbnails_sent = 0u32;

    while !stop.load(Ordering::Relaxed) {
        if force_keyframe.swap(false, Ordering::Relaxed) {
            encoder.force_keyframe();
        }

        // Prefer a zero-copy DMA-BUF frame if the capture backend offers
        // one; otherwise fall to the CPU SHM frame.
        let mut gpu_frame_opt = None;
        if let Some(ref gpu_rx) = gpu_frame_rx {
            if let Ok(f) = gpu_rx.try_recv() {
                gpu_frame_opt = Some(f);
            }
        }
        let got_gpu_frame = gpu_frame_opt.is_some();

        if !got_gpu_frame {
            match frame_rx.recv_timeout(Duration::from_millis(50)) {
                Ok(f) => last_frame = Some(f),
                Err(RecvTimeoutError::Timeout) => {
                    // Only re-encode the last frame once per frame interval;
                    // otherwise spin lightly until the next real frame.
                    if !(last_frame_time.elapsed() >= repeat_interval && last_frame.is_some()) {
                        continue;
                    }
                }
                Err(RecvTimeoutError::Disconnected) => {
                    log::info!("[encoder-linux] capture channel disconnected");
                    break;
                }
            }
        }
        last_frame_time = Instant::now();

        // Thumbnail (CPU frames only; the DMA-BUF path has no CPU-side
        // pixels to downscale without a readback — TODO if a compositor
        // re-enables DMA-BUF capture).
        if last_thumbnail.elapsed() >= THUMBNAIL_INTERVAL {
            if let Some(frame) = last_frame.as_ref() {
                if let Some(jpeg) = frame_to_jpeg_thumbnail(frame) {
                    let _ = cfg.thumbnail_tx.try_send(jpeg);
                    thumbnails_sent += 1;
                }
            }
            last_thumbnail = Instant::now();
        }

        let encode_result: Result<Option<EncodedFrame>, String> =
            if let Some(dmabuf) = gpu_frame_opt {
                encode_dmabuf(&mut encoder, &mut gpu_ctx, &mut gpu_initialized, &config, &dmabuf)
            } else {
                let frame = match last_frame.as_ref() {
                    Some(f) => f,
                    None => continue,
                };
                match frame.pixel_format {
                    PixelFormat::NV12 => {
                        encoder.encode_nv12_frame(&frame.data, frame.width, frame.height)
                    }
                    PixelFormat::BGRA | PixelFormat::RGBA => {
                        let is_bgra = frame.pixel_format == PixelFormat::BGRA;
                        encoder.encode_bgra_frame(
                            &frame.data,
                            frame.width,
                            frame.height,
                            frame.stride,
                            is_bgra,
                        )
                    }
                }
            };

        match encode_result {
            Ok(Some(ef)) => emit_encoded(&ef, cfg, stream_start, &mut frames_sent),
            Ok(None) => {}
            Err(e) => {
                log::error!("[encoder-linux] encode failed: {e}");
                break;
            }
        }

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::info!(
                "[encoder-linux] codec_byte={} {}x{}@{} {}kbps frames_sent={} thumbs_sent={}",
                cfg.codec_wire_byte,
                cfg.width,
                cfg.height,
                cfg.fps,
                cfg.bitrate_kbps,
                frames_sent,
                thumbnails_sent,
            );
            frames_sent = 0;
            thumbnails_sent = 0;
            last_telemetry = Instant::now();
        }
    }

    log::info!(
        "[encoder-linux] encode loop exiting (stop_requested={})",
        stop.load(Ordering::Relaxed),
    );

    // Flush the encoder's internal queue on stop.
    for ef in encoder.flush() {
        emit_encoded(&ef, cfg, stream_start, &mut frames_sent);
    }
}

/// Zero-copy DMA-BUF encode: lazily detects the GPU backend on the first
/// frame (swapping the encoder to a CUDA/VAAPI hw context) then imports +
/// encodes. Dormant unless a capture backend advertises DMA-BUF.
fn encode_dmabuf(
    encoder: &mut H264Encoder,
    gpu_ctx: &mut Option<GpuContext>,
    gpu_initialized: &mut bool,
    config: &EncoderConfig,
    dmabuf: &super::capture::DmaBufFrame,
) -> Result<Option<EncodedFrame>, String> {
    use std::os::fd::AsRawFd;

    if !*gpu_initialized {
        *gpu_initialized = true;
        *gpu_ctx = GpuContext::new();
        if let Some(gpu) = gpu_ctx.as_mut() {
            match gpu.backend_type() {
                GpuBackendType::Vaapi => {
                    if let Err(e) = gpu.init_vaapi_frames(config.width, config.height) {
                        log::warn!("[encoder-linux] VAAPI frames init failed ({e}); CPU fallback");
                        *gpu_ctx = None;
                    } else {
                        match H264Encoder::new_vaapi(
                            config,
                            gpu.vaapi_device_ref(),
                            gpu.vaapi_frames_ref(),
                        ) {
                            Ok(e) => {
                                log::info!("[encoder-linux] VA-API zero-copy encoding enabled");
                                *encoder = e;
                            }
                            Err(e) => {
                                log::warn!("[encoder-linux] VAAPI encoder failed ({e}); CPU fallback");
                                *gpu_ctx = None;
                            }
                        }
                    }
                }
                GpuBackendType::Cuda => {
                    let shared_ctx = gpu.cuda_ctx_raw();
                    match H264Encoder::new_cuda(config, shared_ctx) {
                        Ok(e) => {
                            log::info!("[encoder-linux] CUDA zero-copy encoding enabled");
                            *encoder = e;
                        }
                        Err(e) => {
                            log::warn!("[encoder-linux] CUDA encoder init failed ({e}); CPU fallback");
                            *gpu_ctx = None;
                        }
                    }
                }
            }
        }
    }

    let Some(gpu) = gpu_ctx.as_mut() else {
        // GPU init failed — drop this DMA-BUF frame; the CPU SHM channel
        // (if any) keeps the stream alive.
        return Ok(None);
    };

    match gpu.backend_type() {
        GpuBackendType::Cuda => {
            if !encoder.has_cuda_hw() {
                return Ok(None);
            }
            match gpu.import_dmabuf_cuda(
                dmabuf.fd.as_raw_fd(),
                dmabuf.width,
                dmabuf.height,
                dmabuf.stride,
                dmabuf.drm_format,
                dmabuf.modifier,
            ) {
                Some(dev_ptr) => {
                    gpu.push_cuda_ctx();
                    let r = encoder.encode_cuda_frame(dev_ptr, dmabuf.width, dmabuf.height);
                    gpu.pop_cuda_ctx();
                    r
                }
                None => Ok(None),
            }
        }
        GpuBackendType::Vaapi => match gpu.map_dmabuf_vaapi(
            dmabuf.fd.as_raw_fd(),
            dmabuf.width,
            dmabuf.height,
            dmabuf.stride,
            dmabuf.drm_format,
            dmabuf.modifier,
        ) {
            Some(mut vaapi_frame) => encoder.encode_vaapi_frame(&mut vaapi_frame),
            None => Ok(None),
        },
    }
}

/// Downscale a captured CPU frame to a ~320px-wide JPEG for the stream
/// tile. jpeg-encoder takes BGRA/RGBA directly, so no manual colour
/// conversion — we just nearest-neighbour subsample into a tight buffer.
fn frame_to_jpeg_thumbnail(frame: &RawFrame) -> Option<Vec<u8>> {
    // The Linux capture backends only ever produce BGRA/RGBA; NV12 would
    // need a YUV→RGB pass we don't bother with here.
    let (color, is_bgra) = match frame.pixel_format {
        PixelFormat::BGRA => (ColorType::Bgra, true),
        PixelFormat::RGBA => (ColorType::Rgba, false),
        PixelFormat::NV12 => return None,
    };
    let _ = is_bgra;

    let w = frame.width as usize;
    let h = frame.height as usize;
    if w == 0 || h == 0 {
        return None;
    }
    let thumb_w = 320usize.min(w);
    let thumb_h = (thumb_w * h) / w;
    if thumb_w == 0 || thumb_h == 0 {
        return None;
    }

    let stride = frame.stride;
    let mut packed = vec![0u8; thumb_w * thumb_h * 4];
    for ty in 0..thumb_h {
        let sy = (ty * h) / thumb_h;
        for tx in 0..thumb_w {
            let sx = (tx * w) / thumb_w;
            let src = sy * stride + sx * 4;
            let dst = (ty * thumb_w + tx) * 4;
            if src + 4 <= frame.data.len() {
                packed[dst..dst + 4].copy_from_slice(&frame.data[src..src + 4]);
            }
        }
    }

    let mut jpeg = Vec::with_capacity(16 * 1024);
    JpegEncoder::new(&mut jpeg, 60)
        .encode(&packed, thumb_w as u16, thumb_h as u16, color)
        .ok()?;
    Some(jpeg)
}
