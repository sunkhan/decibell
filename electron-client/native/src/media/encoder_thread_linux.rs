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
//!
//! Exit path (as on Windows): when the stream ends without being asked to,
//! the thread tells the renderer FIRST — `stream_capture_ended` when the
//! source went away (portal session closed from the desktop's tray, window
//! closed, monitor gone), `native_stream_failed` when the capture or the
//! encoder broke or panicked — and the renderer's one stop path tears the
//! stream down and tells the community, so watchers see it end. Nothing is
//! reported for a requested stop: a late event would end the NEXT session.
#![cfg(target_os = "linux")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use jpeg_encoder::ColorType;

use super::capture::{
    CaptureEnd, CaptureEndSlot, CaptureOutput, CaptureStopper, DmaBufFrame, PixelFormat, RawFrame,
};
use super::encoder_linux::{EncodedFrame, EncoderConfig, H264Encoder};
use super::thumb_encode::{ThumbnailJob, ThumbnailSchedule, ThumbnailWorker};
use super::gpu_interop::{GpuBackendType, GpuContext};
use super::video_pipeline::VideoSender;
use crate::events;
use crate::media::video_packet::WIRE_DESCRIPTION_MAGIC;

/// GOP length in seconds — keyframe every 2s, matching the renderer path.
const KEYFRAME_INTERVAL_SECS: u32 = 2;

/// Consecutive frames that may fail to encode before the stream is declared
/// dead (as on Windows): ~1 s at 60 fps. A transient failure recovers well
/// inside it; a dead encoder fails every frame.
const MAX_CONSECUTIVE_FAILURES: u32 = 60;

/// How long `stop()` waits for the thread before detaching it. The loop
/// checks its stop flag at least every 50 ms; only a thread wedged inside
/// the encoder/driver overruns this.
const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(3);

pub struct LinuxEncoderThread {
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    /// Ends the capture feeding the thread (taken by `stop`).
    capture_stopper: Option<CaptureStopper>,
    thread: Option<JoinHandle<()>>,
}

/// Why the encode loop returned.
#[derive(Debug, PartialEq, Eq)]
enum LoopExit {
    /// The stop flag was set (or the capture ended because of it).
    Stopped,
    /// The capture source went away; the stream ends normally.
    SourceEnded(String),
    /// The stream is dead; the reason goes to the renderer.
    Failed(String),
}

/// What the renderer is told about an exit.
#[derive(Debug, PartialEq, Eq)]
enum ExitReport {
    Quiet,
    /// `stream_capture_ended`.
    CaptureEnded(String),
    /// `native_stream_failed` with the reason.
    Failed(String),
}

/// The loop's exit when the capture's frame channel disconnects. The flag
/// is checked first: stop() flags the loop before it ends the capture.
fn exit_on_capture_disconnect(stop_requested: bool, end: Option<CaptureEnd>) -> LoopExit {
    if stop_requested {
        return LoopExit::Stopped;
    }
    match end {
        Some(CaptureEnd::SourceGone(why)) => LoopExit::SourceEnded(why),
        Some(CaptureEnd::Failed(why)) => LoopExit::Failed(format!("capture failed: {why}")),
        None => LoopExit::Failed("capture stopped unexpectedly".to_string()),
    }
}

/// Whether (and how) to tell the renderer. Suppressed when we were asked to
/// stop, whatever the loop saw: that exit is expected.
fn report_for(exit: &LoopExit, stop_requested: bool) -> ExitReport {
    if stop_requested {
        return ExitReport::Quiet;
    }
    match exit {
        LoopExit::Stopped => ExitReport::Quiet,
        LoopExit::SourceEnded(why) => ExitReport::CaptureEnded(why.clone()),
        LoopExit::Failed(why) => ExitReport::Failed(why.clone()),
    }
}

// Event emits sit behind these so the unit tests never reach the napi event
// bus (its threadsafe-function symbols only exist inside Node).
#[cfg(not(test))]
fn emit_capture_ended() {
    events::emit_stream_capture_ended();
}
#[cfg(test)]
fn emit_capture_ended() {}

#[cfg(not(test))]
fn emit_stream_failed(reason: String) {
    events::send("native_stream_failed", serde_json::Value::String(reason));
}
#[cfg(test)]
fn emit_stream_failed(_reason: String) {}

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
        let CaptureOutput { receiver, gpu_receiver, end, stopper, .. } = capture;
        // Open the encoder up front so a failure surfaces here in start()
        // rather than deep inside the thread. `find_hw_encoder` logs which
        // backend it picked (h264_nvenc / *_vaapi / libx264) — that line
        // is the signal for whether we're on the GPU fast path or the CPU
        // sws_scale fallback. On failure the capture is ended here: the
        // portal session would otherwise stay open behind a failed start.
        let mut encoder = H264Encoder::new(cfg.target_codec, &config).inspect_err(|_| {
            if let Some(s) = &stopper {
                s.stop();
            }
        })?;

        let stop = Arc::new(AtomicBool::new(false));
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let fk_t = force_keyframe.clone();
        let spawned = std::thread::Builder::new()
            .name("decibell-encoder-linux".to_string())
            .spawn(move || {
                let input = CaptureInput { frames: receiver, gpu_frames: gpu_receiver, end };
                let stream_start = Instant::now();
                // catch_unwind: a panic anywhere in the encode loop (FFmpeg
                // binding, bitstream parsing) must not silently kill this
                // thread while the app keeps claiming to stream. Contained,
                // logged, and surfaced to the renderer below.
                let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_encode_loop(&mut encoder, &config, input, &cfg, &stop_t, &fk_t, stream_start)
                }))
                .unwrap_or_else(|panic| {
                    let msg = panic
                        .downcast_ref::<&str>()
                        .map(|s| s.to_string())
                        .or_else(|| panic.downcast_ref::<String>().cloned())
                        .unwrap_or_else(|| "<non-string panic payload>".to_string());
                    log::error!("[encoder-linux] encode thread PANICKED: {msg}");
                    LoopExit::Failed(format!("encoder panicked: {msg}"))
                });
                // Tell the renderer FIRST, before the flush below or the
                // encoder's teardown can block on a wedged device.
                match report_for(&exit, stop_t.load(Ordering::Relaxed)) {
                    ExitReport::Quiet => {
                        if exit != LoopExit::Stopped {
                            log::info!("[encoder-linux] loop ended during stop: {exit:?}");
                        }
                    }
                    ExitReport::CaptureEnded(why) => {
                        log::info!("[encoder-linux] capture source ended ({why}); ending the stream");
                        emit_capture_ended();
                    }
                    ExitReport::Failed(why) => {
                        log::error!("[encoder-linux] stream failed: {why}");
                        emit_stream_failed(why);
                    }
                }
                // Flush only a requested stop. A stream that died is ending
                // anyway, and a broken encoder may never return from it.
                if exit == LoopExit::Stopped {
                    let mut frames_sent = 0;
                    for ef in encoder.flush() {
                        emit_encoded(&ef, &cfg, stream_start, &mut frames_sent);
                    }
                }
            });
        let thread = match spawned {
            Ok(t) => t,
            Err(e) => {
                if let Some(s) = &stopper {
                    s.stop();
                }
                return Err(format!("spawn linux encoder thread: {e}"));
            }
        };

        Ok(Self {
            stop,
            force_keyframe,
            capture_stopper: stopper,
            thread: Some(thread),
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    /// Flag the loop, end the capture, and join, bounded by
    /// STOP_JOIN_TIMEOUT. The flag goes first: ending the capture
    /// disconnects the frame channel, and a loop that saw that before the
    /// flag would report a failure. A thread still running after the
    /// timeout is wedged in the encoder/driver and is detached (the flag
    /// keeps it from reporting anything) rather than hanging the caller.
    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = self.capture_stopper.take() {
            s.stop();
        }
        let Some(t) = self.thread.take() else {
            return;
        };
        let deadline = Instant::now() + STOP_JOIN_TIMEOUT;
        while !t.is_finished() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(5));
        }
        if t.is_finished() {
            let _ = t.join();
        } else {
            log::error!(
                "[encoder-linux] encoder thread did not exit within {:?} — detaching it (wedged in the encoder/driver?)",
                STOP_JOIN_TIMEOUT
            );
        }
    }
}

impl Drop for LinuxEncoderThread {
    /// Dropped without `stop()` (an error path): flag the loop so the
    /// capture's end isn't reported as a failure, then end the capture.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(s) = self.capture_stopper.take() {
            s.stop();
        }
    }
}

/// The capture side of the loop: frame receivers plus the slot where the
/// capture records why it ended.
struct CaptureInput {
    frames: Receiver<RawFrame>,
    gpu_frames: Option<Receiver<DmaBufFrame>>,
    end: Arc<CaptureEndSlot>,
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
    // Only when a player is actually subscribed to our own stream — checked
    // before cloning so an unwatched self-preview costs nothing. The player
    // force_keyframe()s on subscribe, so resuming starts on an IDR.
    if events::is_stream_frame_sink(&cfg.local_username) {
        events::send_stream_frame(events::StreamFrame {
            key: cfg.local_username.clone(),
            codec,
            keyframe: ef.is_keyframe,
            timestamp: stream_start.elapsed().as_micros() as i64,
            data: ef.data.clone(),
            description: ef.avcc_description.clone(),
            discontinuity: false,
        });
    }
    // Counts frames put on the wire (telemetry), preview or not.
    *frames_sent += 1;
}

fn run_encode_loop(
    encoder: &mut H264Encoder,
    config: &EncoderConfig,
    capture: CaptureInput,
    cfg: &LinuxEncoderThreadConfig,
    stop: &AtomicBool,
    force_keyframe: &AtomicBool,
    stream_start: Instant,
) -> LoopExit {
    let frame_rx = capture.frames;
    let gpu_frame_rx = capture.gpu_frames;

    // Lazily created on the first DMA-BUF frame; stays None on the CPU
    // path so no GPU context is allocated when it isn't needed.
    let mut gpu_ctx: Option<GpuContext> = None;
    let mut gpu_initialized = false;

    // Re-encode the last frame if capture stalls (static screen) so the
    // GOP keeps flowing and receivers don't time out the stream. A source
    // that is gone ends the capture (see capture_pipewire), so this never
    // repeats a frame for a share that no longer exists.
    let repeat_interval = Duration::from_millis((1000 / cfg.fps.max(1)).max(1) as u64);
    let mut last_frame: Option<RawFrame> = None;
    let mut last_frame_time = Instant::now();

    let mut last_telemetry = Instant::now();
    let mut thumb_schedule = ThumbnailSchedule::new();
    let thumb_worker = ThumbnailWorker::spawn(cfg.thumbnail_tx.clone());
    let mut frames_sent = 0u32;
    let mut thumbnails_sent = 0u32;
    let mut consecutive_failures = 0u32;
    let mut last_failure_warn: Option<Instant> = None;

    loop {
        if stop.load(Ordering::Relaxed) {
            return LoopExit::Stopped;
        }
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
                    return exit_on_capture_disconnect(
                        stop.load(Ordering::Relaxed),
                        capture.end.get(),
                    );
                }
            }
        }
        last_frame_time = Instant::now();

        // Thumbnail (CPU frames only; the DMA-BUF path has no CPU-side
        // pixels to downscale without a readback — TODO if a compositor
        // re-enables DMA-BUF capture). Only the copy of the frame happens
        // here; the downscale + JPEG encode run on the thumbnail worker.
        let now = Instant::now();
        if thumb_schedule.due(now) {
            if let Some(job) = last_frame.as_ref().and_then(thumbnail_job) {
                if thumb_worker.submit(job) {
                    thumbnails_sent += 1;
                }
            }
            thumb_schedule.taken(now);
        }

        let encode_result: Result<Option<EncodedFrame>, String> =
            if let Some(dmabuf) = gpu_frame_opt {
                encode_dmabuf(encoder, &mut gpu_ctx, &mut gpu_initialized, config, &dmabuf)
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
            Ok(encoded) => {
                consecutive_failures = 0;
                if let Some(ef) = encoded {
                    emit_encoded(&ef, cfg, stream_start, &mut frames_sent);
                }
            }
            Err(e) => {
                consecutive_failures += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return LoopExit::Failed(format!(
                        "{consecutive_failures} consecutive frames failed to encode (last: {e})"
                    ));
                }
                if last_failure_warn.is_none_or(|t| t.elapsed() >= Duration::from_secs(1)) {
                    log::warn!(
                        "[encoder-linux] encode failed: {e} (consecutive={consecutive_failures})"
                    );
                    last_failure_warn = Some(Instant::now());
                }
            }
        }

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::debug!(
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

/// Copy a captured CPU frame out for the thumbnail worker (which does the
/// downscale + JPEG). jpeg-encoder takes BGRA/RGBA directly, so no colour
/// conversion; NV12 would need a YUV→RGB pass we don't bother with here.
fn thumbnail_job(frame: &RawFrame) -> Option<ThumbnailJob> {
    let color = match frame.pixel_format {
        PixelFormat::BGRA => ColorType::Bgra,
        PixelFormat::RGBA => ColorType::Rgba,
        PixelFormat::NV12 => return None,
    };
    let (w, h) = (frame.width as usize, frame.height as usize);
    if w == 0 || h == 0 || frame.stride < w * 4 {
        return None;
    }
    Some(ThumbnailJob {
        pixels: frame.data.clone(),
        width: w,
        height: h,
        stride: frame.stride,
        color,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Review B9: why the capture channel disconnected decides what the
    /// renderer hears. Before, every exit only logged, so the UI stayed
    /// "Streaming" over a dead stream.
    #[test]
    fn capture_disconnect_maps_to_an_exit() {
        let gone = || Some(CaptureEnd::SourceGone("portal session closed".into()));
        // A requested stop wins over whatever the capture recorded.
        assert_eq!(exit_on_capture_disconnect(true, gone()), LoopExit::Stopped);
        assert_eq!(exit_on_capture_disconnect(true, None), LoopExit::Stopped);
        assert_eq!(
            exit_on_capture_disconnect(false, gone()),
            LoopExit::SourceEnded("portal session closed".into())
        );
        assert!(matches!(
            exit_on_capture_disconnect(false, Some(CaptureEnd::Failed("stream error".into()))),
            LoopExit::Failed(why) if why.contains("stream error")
        ));
        // A capture that vanished without saying why is a failure.
        assert!(matches!(exit_on_capture_disconnect(false, None), LoopExit::Failed(_)));
    }

    #[test]
    fn exits_report_once_and_never_during_a_stop() {
        let ended = LoopExit::SourceEnded("window closed".into());
        let failed = LoopExit::Failed("encoder panicked".into());
        assert_eq!(report_for(&ended, false), ExitReport::CaptureEnded("window closed".into()));
        assert_eq!(report_for(&failed, false), ExitReport::Failed("encoder panicked".into()));
        assert_eq!(report_for(&LoopExit::Stopped, false), ExitReport::Quiet);
        // stop() flagged the thread: a late event would end the NEXT
        // session, so nothing is reported whatever the loop saw.
        for exit in [ended, failed, LoopExit::Stopped] {
            assert_eq!(report_for(&exit, true), ExitReport::Quiet);
        }
    }
}
