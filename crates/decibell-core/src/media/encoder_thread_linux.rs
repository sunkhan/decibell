//! Linux native encoder thread — the Linux counterpart to the Windows
//! `encoder_thread`. Pulls captured CPU frames (`RawFrame` from PipeWire
//! SHM / wlr-screencopy) and drives the FFmpeg `H264Encoder` (NVENC, VAAPI
//! or software SVT-AV1), draining encoded packets into:
//!  - the UDP `VideoSender` (the wire — same path renderer-encoded
//!    chunks took), with the HEVC/AV1 keyframe description prefixed via
//!    `WIRE_DESCRIPTION_MAGIC` exactly like `send_video_frame` does;
//!  - the renderer self-preview TSFN via `events::send_stream_frame`
//!    keyed by the local username.
//!
//! Runs on one OS thread; all FFmpeg + GPU state stays on it.
//!
//! Frames are CPU memory on every backend: `capture_pipewire` does not
//! advertise DMA-BUF (mesa-allocated DMA-BUFs came out black on the NVIDIA
//! proprietary driver). NVENC and VAAPI still convert BGRA → NV12 on the
//! GPU; only the software encoder converts on the CPU.
//!
//! **Fallthrough**: `start` is given the backends to try in order
//! (`encoder_probe::plan`). The first that opens runs the stream. If it
//! then fails before its first packet (a driver that opens and cannot
//! encode), the loop opens the next one in place and carries on; only when
//! the last one has failed does the stream end with `native_stream_failed`.
//! Each failure and the backend that ends up working are told to the probe
//! cache. One pass over the list: no retries, no timers.
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

use super::capture::{CaptureEnd, CaptureEndSlot, CaptureOutput, CaptureStopper, PixelFormat, RawFrame};
use super::caps::CodecKind;
use super::encoder_backend::{software_refusal, Backend, Candidate};
pub use super::encoder_backend::StartedEncoder;
use super::encoder_linux::{EncodedFrame, EncoderConfig, EncoderTarget, H264Encoder};
use super::thumb_encode::{ThumbnailJob, ThumbnailSchedule, ThumbnailWorker};
use super::video_pipeline::VideoSender;
use crate::events;
use crate::media::video_packet::WIRE_DESCRIPTION_MAGIC;

/// GOP length in seconds — keyframe every 2s, matching the renderer path.
const KEYFRAME_INTERVAL_SECS: u32 = 2;

/// Consecutive frames that may fail to encode before the stream is declared
/// dead (as on Windows): ~1 s at 60 fps. A transient failure recovers well
/// inside it; a dead encoder fails every frame.
const MAX_CONSECUTIVE_FAILURES: u32 = 60;

/// Consecutive failed frames, before the encoder has produced anything,
/// after which the next backend is opened (a transient hiccup on the very
/// first frame is tolerated; a backend that cannot encode fails them all).
const EARLY_FAILURES: u32 = 3;

/// Seconds of frames an encoder may swallow without producing a packet
/// before it counts as failed. Every backend here answers within a frame or
/// two (software: a handful).
const SILENT_SECS: u32 = 2;

/// How long `stop()` waits for the thread before detaching it. The loop
/// checks its stop flag at least every 50 ms; only a thread wedged inside
/// the encoder/driver overruns this.
const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(3);

pub struct LinuxEncoderThread {
    started: StartedEncoder,
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

/// One encoder the stream may run on.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct EncoderChoice {
    /// What the probe cache knows it as.
    pub candidate: Candidate,
    pub target: EncoderTarget,
    /// The codec it encodes; its wire byte (1=H264_HW, 3=H265, 4=AV1) is
    /// stamped into every UdpVideoPacket so receivers pick the decoder.
    pub codec: CodecKind,
    /// "NVENC (NVIDIA)", "VAAPI (AMD)", "Software (SVT-AV1)".
    pub label: String,
}

pub struct LinuxEncoderThreadConfig {
    /// The encoders to try, in order (never empty).
    pub choices: Vec<EncoderChoice>,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// A 5 fps stream: tune the encoder for detail (see bitrate_preset).
    pub detail: bool,
    /// Our media key: the self-preview's `StreamFrame::key`, shared by
    /// every frame.
    pub local_username: Arc<str>,
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
            keyframe_interval_secs: if cfg.detail {
                super::bitrate_preset::DETAIL_KEYFRAME_SECS
            } else {
                KEYFRAME_INTERVAL_SECS
            },
            detail: cfg.detail,
            source_size: (0, 0),
        };
        // DMA-BUF frames are not consumed (the capture never sends any).
        let CaptureOutput { receiver, end, stopper, width: source_w, height: source_h, .. } = capture;
        let config = EncoderConfig { source_size: (source_w, source_h), ..config };
        // Open the encoder up front so a failure surfaces here in start()
        // rather than deep inside the thread: the first choice that opens.
        // On failure the capture is ended here: the portal session would
        // otherwise stay open behind a failed start.
        let mut remaining: std::collections::VecDeque<EncoderChoice> = cfg.choices.iter().cloned().collect();
        let mut errors = Vec::new();
        let (mut encoder, mut current) = match open_next(&mut remaining, &config, &mut errors) {
            Some(opened) => opened,
            None => {
                if let Some(s) = &stopper {
                    s.stop();
                }
                return Err(format!("No encoder could be opened ({})", errors.join("; ")));
            }
        };
        let started = StartedEncoder { codec: current.codec, backend: current.candidate.backend, label: current.label.clone() };

        let stop = Arc::new(AtomicBool::new(false));
        let force_keyframe = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let fk_t = force_keyframe.clone();
        let spawned = std::thread::Builder::new()
            .name("decibell-encoder-linux".to_string())
            .spawn(move || {
                let input = CaptureInput { frames: receiver, end };
                let stream_start = Instant::now();
                // catch_unwind: a panic anywhere in the encode loop (FFmpeg
                // binding, bitstream parsing) must not silently kill this
                // thread while the app keeps claiming to stream. Contained,
                // logged, and surfaced to the renderer below.
                let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    let mut next = |errors: &mut Vec<String>| open_next(&mut remaining, &config, errors);
                    run_encode_loop(&mut encoder, &mut current, &mut next, input, &cfg, &stop_t, &fk_t, stream_start)
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
                        events::emit_stream_capture_ended();
                    }
                    ExitReport::Failed(why) => {
                        log::error!("[encoder-linux] stream failed: {why}");
                        events::emit_native_stream_failed(why);
                    }
                }
                // Flush only a requested stop. A stream that died is ending
                // anyway, and a broken encoder may never return from it.
                if exit == LoopExit::Stopped {
                    let mut frames_sent = 0;
                    for ef in encoder.flush() {
                        emit_encoded(ef, current.codec as u8, &cfg, stream_start, &mut frames_sent);
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
            started,
            stop,
            force_keyframe,
            capture_stopper: stopper,
            thread: Some(thread),
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    /// The encoder the stream started on (what is announced). A later
    /// fallthrough inside the loop keeps the codec unless it lands on
    /// software, which the choices only allow when the codec may change.
    pub fn started(&self) -> &StartedEncoder {
        &self.started
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
    end: Arc<CaptureEndSlot>,
}

/// Has the encoder on trial (opened, no packet out yet) failed? A proven
/// encoder never fails this way: its failures go by the 60-frame rule.
/// `error` is this frame's encode error, if it had one.
fn trial_verdict(
    proven: bool,
    consecutive_failures: u32,
    frames_in: u32,
    fps: u32,
    error: Option<&str>,
) -> Option<String> {
    if proven {
        return None;
    }
    match error {
        Some(e) if consecutive_failures >= EARLY_FAILURES => Some(e.to_string()),
        Some(_) => None,
        None if frames_in >= fps.max(1) * SILENT_SECS => Some(format!("{frames_in} frames in, no packet out")),
        None => None,
    }
}

/// Open the next choice that opens, telling the probe cache about each one
/// that does not. None when the list is used up; `errors` collects why.
fn open_next(
    remaining: &mut std::collections::VecDeque<EncoderChoice>,
    config: &EncoderConfig,
    errors: &mut Vec<String>,
) -> Option<(H264Encoder, EncoderChoice)> {
    while let Some(choice) = remaining.pop_front() {
        // Software above what the CPU is allowed: skipped, not a failure.
        let refusal = (choice.candidate.backend == Backend::Software)
            .then(|| software_refusal(config.width, config.height, config.fps, super::encoder_probe::logical_cpus()))
            .flatten();
        if let Some(why) = refusal {
            log::warn!("[encoder-linux] {} not used: {why}", choice.label);
            errors.push(format!("{}: {why}", choice.label));
            continue;
        }
        match H264Encoder::open(&choice.target, choice.codec, config) {
            Ok(encoder) => {
                log::info!("[encoder-linux] encoding {:?} with {}", choice.codec, choice.label);
                return Some((encoder, choice));
            }
            Err(e) => {
                super::encoder_probe::note_failed(choice.candidate, choice.codec, &e);
                errors.push(format!("{}: {e}", choice.label));
            }
        }
    }
    None
}

/// Packetise + ship one encoded frame: UDP wire (with HEVC/AV1 keyframe
/// description prefix) and the renderer self-preview TSFN. Takes the frame:
/// the preview gets the encoder's own buffer once the wire is done with it.
fn emit_encoded(
    ef: EncodedFrame,
    codec: u8,
    cfg: &LinuxEncoderThreadConfig,
    stream_start: Instant,
    frames_sent: &mut u32,
) {

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
    cfg.video_sender.send_frame_paced(codec, ef.is_keyframe, wire);

    // Self-preview: same encoded bytes to the renderer keyed by local
    // username (StreamVideoPlayer renders the user's own tile via the
    // unified stream-frame bus). Description travels as a separate field.
    // Only when a player is actually subscribed to our own stream. The
    // bytes are the encoder's own buffer, moved (no copy, no allocation);
    // an unwatched self-preview just drops it. The player force_keyframe()s
    // on subscribe, so resuming starts on an IDR.
    if events::is_stream_frame_sink(&cfg.local_username) {
        events::send_stream_frame(events::StreamFrame {
            key: cfg.local_username.clone(),
            codec,
            keyframe: ef.is_keyframe,
            timestamp: stream_start.elapsed().as_micros() as i64,
            data: ef.data.into(),
            description: ef.avcc_description,
            discontinuity: false,
        });
    }
    // Counts frames put on the wire (telemetry), preview or not.
    *frames_sent += 1;
}

/// What the encode loop needs of an encoder (the real one is `H264Encoder`;
/// the loop's fallthrough is tested with stand-ins that fail on cue).
trait FrameEncoder {
    fn encode(&mut self, frame: &RawFrame) -> Result<Option<EncodedFrame>, String>;
    fn receive_pending(&mut self) -> Option<EncodedFrame>;
    fn force_keyframe(&mut self);
}

impl FrameEncoder for H264Encoder {
    fn encode(&mut self, frame: &RawFrame) -> Result<Option<EncodedFrame>, String> {
        match frame.pixel_format {
            PixelFormat::NV12 => self.encode_nv12_frame(&frame.data, frame.width, frame.height),
            PixelFormat::BGRA | PixelFormat::RGBA => {
                let is_bgra = frame.pixel_format == PixelFormat::BGRA;
                self.encode_bgra_frame(&frame.data, frame.width, frame.height, frame.stride, is_bgra)
            }
        }
    }

    fn receive_pending(&mut self) -> Option<EncodedFrame> {
        H264Encoder::receive_pending(self)
    }

    fn force_keyframe(&mut self) {
        H264Encoder::force_keyframe(self)
    }
}

/// `open_next` opens the next choice when the encoder on trial fails (None:
/// the list is used up); it appends why each one it skipped did not open.
#[allow(clippy::too_many_arguments)]
fn run_encode_loop<E: FrameEncoder>(
    encoder: &mut E,
    current: &mut EncoderChoice,
    open_next: &mut dyn FnMut(&mut Vec<String>) -> Option<(E, EncoderChoice)>,
    capture: CaptureInput,
    cfg: &LinuxEncoderThreadConfig,
    stop: &AtomicBool,
    force_keyframe: &AtomicBool,
    stream_start: Instant,
) -> LoopExit {
    let frame_rx = capture.frames;
    // Until the open encoder has produced a packet it is on trial: failing
    // now falls through to the next choice instead of ending the stream.
    let mut proven = false;
    let mut frames_in = 0u32;

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
        last_frame_time = Instant::now();

        // Thumbnail. Only the copy of the frame happens here; the
        // downscale + JPEG encode run on the thumbnail worker.
        let now = Instant::now();
        if thumb_schedule.due(now) {
            if let Some(job) = last_frame.as_ref().and_then(thumbnail_job) {
                if thumb_worker.submit(job) {
                    thumbnails_sent += 1;
                }
            }
            thumb_schedule.taken(now);
        }

        let encode_result: Result<Option<EncodedFrame>, String> = {
            let frame = match last_frame.as_ref() {
                Some(f) => f,
                None => continue,
            };
            encoder.encode(frame)
        };
        frames_in += 1;

        // What ended this encoder's trial, if anything did.
        let trial_failure: Option<String>;
        match encode_result {
            Ok(encoded) => {
                consecutive_failures = 0;
                let mut next = encoded;
                // An encoder may hand back more than one packet for a call
                // (one it could not finish last time): take every packet
                // that is ready, so its queue never grows.
                while let Some(ef) = next {
                    if !proven {
                        proven = true;
                        super::encoder_probe::note_working(current.candidate, current.codec);
                    }
                    emit_encoded(ef, current.codec as u8, cfg, stream_start, &mut frames_sent);
                    next = encoder.receive_pending();
                }
                trial_failure = trial_verdict(proven, consecutive_failures, frames_in, cfg.fps, None);
            }
            Err(e) => {
                consecutive_failures += 1;
                trial_failure = trial_verdict(proven, consecutive_failures, frames_in, cfg.fps, Some(&e));
                if trial_failure.is_none() {
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
        }
        if let Some(why) = trial_failure {
            // The backend opened and cannot encode: the next one takes
            // over, on the same capture. Only the last one's failure ends
            // the stream.
            super::encoder_probe::note_failed(current.candidate, current.codec, &why);
            let mut errors = vec![format!("{}: {why}", current.label)];
            match open_next(&mut errors) {
                Some((next_encoder, next_choice)) => {
                    log::warn!(
                        "[encoder-linux] {} failed before its first packet ({why}); continuing with {}",
                        current.label,
                        next_choice.label
                    );
                    *encoder = next_encoder;
                    *current = next_choice;
                    consecutive_failures = 0;
                    frames_in = 0;
                }
                None => {
                    return LoopExit::Failed(format!("no encoder works on this machine ({})", errors.join("; ")));
                }
            }
        }

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::debug!(
                "[encoder-linux] {} codec_byte={} {}x{}@{} {}kbps frames_sent={} thumbs_sent={}",
                current.label,
                current.codec as u8,
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
    use crate::media::capture::CaptureEndSlot;

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

    /// An encoder that opened is on trial until its first packet: three
    /// failed frames in a row, or two seconds of frames swallowed, hand the
    /// stream to the next backend. Once it has produced a packet its
    /// failures are the old rule's business (60 in a row end the stream).
    #[test]
    fn an_encoder_is_on_trial_until_its_first_packet() {
        // Failing from the first frame: the third failure ends the trial.
        assert_eq!(trial_verdict(false, 1, 1, 30, Some("send failed")), None);
        assert_eq!(trial_verdict(false, 2, 2, 30, Some("send failed")), None);
        assert_eq!(trial_verdict(false, 3, 3, 30, Some("send failed")), Some("send failed".to_string()));
        // Accepting frames and returning nothing: two seconds' worth.
        assert_eq!(trial_verdict(false, 0, 59, 30, None), None);
        assert!(trial_verdict(false, 0, 60, 30, None).unwrap().contains("no packet out"));
        assert_eq!(trial_verdict(false, 0, 9, 5, None), None);
        assert!(trial_verdict(false, 0, 10, 5, None).is_some());
        // Proven: never.
        assert_eq!(trial_verdict(true, 59, 1000, 30, Some("send failed")), None);
        assert_eq!(trial_verdict(true, 0, 1000, 30, None), None);
    }

    fn choice(backend: Backend, node: Option<&str>) -> EncoderChoice {
        EncoderChoice {
            candidate: Candidate { backend, adapter: None },
            target: EncoderTarget { backend, render_node: node.map(str::to_string) },
            codec: if backend == Backend::Software { CodecKind::Av1 } else { CodecKind::H264Hw },
            label: backend.id().to_string(),
        }
    }

    fn config(width: u32, height: u32, fps: u32) -> EncoderConfig {
        EncoderConfig {
            width,
            height,
            fps,
            bitrate_kbps: 2000,
            keyframe_interval_secs: 2,
            detail: false,
            source_size: (width, height),
        }
    }

    /// The start walks the choices in order: a backend that cannot open
    /// (here VAAPI on a render node that does not exist) is skipped with
    /// its reason, and the next one that opens runs the stream. Runs where
    /// NVENC or SVT-AV1 is usable; elsewhere every choice fails and the
    /// reasons are all reported.
    #[test]
    fn a_start_falls_through_to_the_next_backend_that_opens() {
        let _one_at_a_time = crate::media::encoder_checks::encoder_lock();
        let cfg = config(640, 360, 30);
        let mut remaining: std::collections::VecDeque<EncoderChoice> = [
            choice(Backend::Vaapi, Some("/dev/dri/renderD999-none")),
            choice(Backend::Nvenc, None),
            choice(Backend::Software, None),
        ]
        .into();
        let mut errors = Vec::new();
        match open_next(&mut remaining, &cfg, &mut errors) {
            Some((encoder, opened)) => {
                assert_ne!(opened.candidate.backend, Backend::Vaapi);
                assert_eq!(encoder.backend, opened.candidate.backend);
                assert!(errors[0].starts_with("vaapi: "), "{errors:?}");
                println!("fell through {errors:?} to {}", encoder.encoder_name());
            }
            None => {
                assert_eq!(errors.len(), 3, "{errors:?}");
                assert!(remaining.is_empty());
                println!("skipped: no backend opens here ({errors:?})");
            }
        }
    }

    /// A stand-in encoder: fails every frame, or hands back one packet a
    /// frame whose bytes are its codec's wire byte.
    struct Fake {
        works: bool,
        codec: u8,
        frames: u64,
    }

    impl FrameEncoder for Fake {
        fn encode(&mut self, _frame: &RawFrame) -> Result<Option<EncodedFrame>, String> {
            if !self.works {
                return Err("the driver refused the frame".to_string());
            }
            self.frames += 1;
            Ok(Some(EncodedFrame {
                data: vec![self.codec; 32],
                is_keyframe: self.frames == 1,
                pts: self.frames,
                avcc_description: None,
            }))
        }

        fn receive_pending(&mut self) -> Option<EncodedFrame> {
            None
        }

        fn force_keyframe(&mut self) {}
    }

    /// Run the real loop over stand-in encoders: `first` is open, `later`
    /// are what the fallthrough can open next. Returns the loop's exit and
    /// the codec byte of every packet that reached the relay.
    fn run_loop_with(first: Fake, later: Vec<(Fake, EncoderChoice)>, frames: u32) -> (LoopExit, Vec<u8>) {
        use crate::media::media_socket::MediaSocket;
        use crate::media::video_packet::UdpVideoPacket;
        use std::net::UdpSocket;
        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay.set_read_timeout(Some(Duration::from_millis(300))).unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(relay.local_addr().unwrap()).unwrap();
        let sender = Arc::new(VideoSender::new(Arc::new(MediaSocket::plain(sock, "7")), "7".into(), None));
        let (thumbnail_tx, _thumbnail_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let cfg = LinuxEncoderThreadConfig {
            choices: Vec::new(),
            width: 64,
            height: 64,
            fps: 30,
            bitrate_kbps: 500,
            detail: false,
            local_username: Arc::from("7"),
            video_sender: sender,
            thumbnail_tx,
        };
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<RawFrame>(64);
        for n in 0..frames {
            frame_tx
                .send(RawFrame {
                    data: vec![n as u8; 64 * 64 * 4],
                    width: 64,
                    height: 64,
                    stride: 64 * 4,
                    pixel_format: PixelFormat::BGRA,
                    timestamp_us: 0,
                })
                .unwrap();
        }
        let end = Arc::new(CaptureEndSlot::default());
        // The capture "ends" once the frames are consumed: the loop then
        // returns, and how it returns is what is checked.
        end.record(CaptureEnd::SourceGone("test over".into()));
        drop(frame_tx);
        let mut encoder = first;
        let mut current = choice(Backend::Nvenc, None);
        let mut later: std::collections::VecDeque<(Fake, EncoderChoice)> = later.into();
        let mut next = |errors: &mut Vec<String>| {
            let opened = later.pop_front();
            if opened.is_none() {
                errors.push("software: not in this build".to_string());
            }
            opened
        };
        let exit = run_encode_loop(
            &mut encoder,
            &mut current,
            &mut next,
            CaptureInput { frames: frame_rx, end },
            &cfg,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            Instant::now(),
        );
        let mut codecs = Vec::new();
        let mut buf = vec![0u8; 65536];
        while let Ok(n) = relay.recv(&mut buf) {
            codecs.push(UdpVideoPacket::from_bytes(&buf[..n]).expect("a video packet").codec);
        }
        (exit, codecs)
    }

    /// The encoder that opened cannot encode: after three failed frames the
    /// next backend takes over on the same capture, its frames go out with
    /// its own codec byte, and the stream goes on to its normal end.
    #[test]
    fn an_encoder_that_fails_its_trial_hands_over_to_the_next() {
        let failing = Fake { works: false, codec: 1, frames: 0 };
        let software = (Fake { works: true, codec: 4, frames: 0 }, choice(Backend::Software, None));
        let (exit, codecs) = run_loop_with(failing, vec![software], 12);
        // 3 frames lost to the trial, 9 encoded by the second backend.
        assert_eq!(codecs, vec![4u8; 9]);
        assert_eq!(exit, LoopExit::SourceEnded("test over".into()), "not a failure");
    }

    /// Nothing left to fall to: the stream fails, and the reason names
    /// every backend that was tried.
    #[test]
    fn the_last_backend_failing_its_trial_fails_the_stream() {
        let failing = Fake { works: false, codec: 1, frames: 0 };
        let (exit, codecs) = run_loop_with(failing, Vec::new(), 12);
        assert!(codecs.is_empty());
        match exit {
            LoopExit::Failed(why) => {
                assert!(why.contains("nvenc: the driver refused the frame"), "{why}");
                assert!(why.contains("software: not in this build"), "{why}");
            }
            other => panic!("expected a failure, got {other:?}"),
        }
    }

    /// A proven encoder is not replaced: its failures count towards the
    /// 60-in-a-row rule and the stream continues meanwhile.
    #[test]
    fn a_proven_encoder_keeps_the_stream_through_a_few_failures() {
        struct Flaky(u64);
        impl FrameEncoder for Flaky {
            fn encode(&mut self, _frame: &RawFrame) -> Result<Option<EncodedFrame>, String> {
                self.0 += 1;
                // Frames 4 to 8 fail (five in a row), after three good ones.
                if (4..=8).contains(&self.0) {
                    return Err("busy".to_string());
                }
                Ok(Some(EncodedFrame { data: vec![1; 16], is_keyframe: self.0 == 1, pts: self.0, avcc_description: None }))
            }
            fn receive_pending(&mut self) -> Option<EncodedFrame> {
                None
            }
            fn force_keyframe(&mut self) {}
        }
        // Same scaffolding as run_loop_with, with a counter on the opener.
        let mut opened = 0;
        let mut next = |_: &mut Vec<String>| -> Option<(Flaky, EncoderChoice)> {
            opened += 1;
            None
        };
        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<RawFrame>(64);
        for _ in 0..12 {
            frame_tx
                .send(RawFrame { data: vec![0; 64 * 64 * 4], width: 64, height: 64, stride: 256, pixel_format: PixelFormat::BGRA, timestamp_us: 0 })
                .unwrap();
        }
        drop(frame_tx);
        let end = Arc::new(CaptureEndSlot::default());
        end.record(CaptureEnd::SourceGone("test over".into()));
        let sock = std::net::UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(sock.local_addr().unwrap()).unwrap();
        let (thumbnail_tx, _rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let cfg = LinuxEncoderThreadConfig {
            choices: Vec::new(),
            width: 64,
            height: 64,
            fps: 30,
            bitrate_kbps: 500,
            detail: false,
            local_username: Arc::from("7"),
            video_sender: Arc::new(VideoSender::new(
                Arc::new(crate::media::media_socket::MediaSocket::plain(sock, "7")),
                "7".into(),
                None,
            )),
            thumbnail_tx,
        };
        let exit = run_encode_loop(
            &mut Flaky(0),
            &mut choice(Backend::Nvenc, None),
            &mut next,
            CaptureInput { frames: frame_rx, end },
            &cfg,
            &AtomicBool::new(false),
            &AtomicBool::new(false),
            Instant::now(),
        );
        assert_eq!(exit, LoopExit::SourceEnded("test over".into()));
        assert_eq!(opened, 0, "no other backend was opened");
    }

    /// The Linux stream pipeline without the capture: generated frames in a
    /// `CaptureOutput`, the real encode thread, the real packetiser, and a
    /// loopback socket standing in for the relay. The first choice cannot
    /// open (VAAPI on a node that does not exist), so the stream starts on
    /// the next one, says so, puts frames of that codec on the wire with a
    /// keyframe first, and stops without reporting a failure. Runs where
    /// NVENC or SVT-AV1 is usable.
    #[test]
    fn the_thread_streams_a_generated_capture_on_the_first_backend_that_opens() {
        let _one_at_a_time = crate::media::encoder_checks::encoder_lock();
        use crate::events::{testing, CoreEvent};
        use crate::media::media_socket::MediaSocket;
        use crate::media::video_packet::UdpVideoPacket;
        use std::net::UdpSocket;
        testing::install_recording_sink();
        const W: u32 = 640;
        const H: u32 = 360;

        let relay = UdpSocket::bind("127.0.0.1:0").unwrap();
        relay.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
        let sock = UdpSocket::bind("127.0.0.1:0").unwrap();
        sock.connect(relay.local_addr().unwrap()).unwrap();
        let sender = Arc::new(VideoSender::new(Arc::new(MediaSocket::plain(sock, "7")), "7".into(), None));
        sender.set_frame_rate(30);

        let (frame_tx, frame_rx) = std::sync::mpsc::sync_channel::<RawFrame>(2);
        let capture = CaptureOutput {
            receiver: frame_rx,
            width: W,
            height: H,
            gpu_receiver: None,
            end: Arc::new(CaptureEndSlot::default()),
            stopper: None,
        };
        let (thumbnail_tx, _thumbnail_rx) = tokio::sync::mpsc::channel::<Vec<u8>>(1);
        let cfg = LinuxEncoderThreadConfig {
            choices: vec![
                choice(Backend::Vaapi, Some("/dev/dri/renderD999-none")),
                choice(Backend::Nvenc, None),
                choice(Backend::Software, None),
            ],
            width: W,
            height: H,
            fps: 30,
            bitrate_kbps: 1500,
            detail: false,
            local_username: Arc::from("7"),
            video_sender: sender,
            thumbnail_tx,
        };
        let thread = match LinuxEncoderThread::start(cfg, capture) {
            Ok(t) => t,
            Err(e) => {
                assert!(e.contains("vaapi") && e.contains("nvenc") && e.contains("software"), "{e}");
                println!("skipped: no backend opens here ({e})");
                return;
            }
        };
        let started = thread.started().clone();
        assert_ne!(started.backend, Backend::Vaapi, "the node does not exist");
        assert_eq!(started.codec, if started.backend == Backend::Software { CodecKind::Av1 } else { CodecKind::H264Hw });

        let stride = W as usize * 4;
        for n in 0..20u32 {
            let mut data = vec![0u8; stride * H as usize];
            for (i, px) in data.chunks_exact_mut(4).enumerate() {
                let v = ((i as u32 / 16 + n * 3) % 255) as u8;
                px.copy_from_slice(&[v, v ^ 0x55, n as u8 * 8, 255]);
            }
            frame_tx
                .send(RawFrame { data, width: W, height: H, stride, pixel_format: PixelFormat::BGRA, timestamp_us: n as u64 * 33_333 })
                .expect("the encode thread takes frames");
        }
        // Frames reach the relay: the first is a keyframe, all carry the
        // started codec's wire byte.
        let mut buf = vec![0u8; 65536];
        let mut frames_seen = std::collections::BTreeSet::new();
        let mut first = true;
        while frames_seen.len() < 10 {
            let n = relay.recv(&mut buf).expect("encoded frames on the wire within 5 s");
            let packet = UdpVideoPacket::from_bytes(&buf[..n]).expect("a video packet");
            assert_eq!(packet.codec, started.codec as u8);
            if first {
                assert!(packet.is_keyframe(), "the stream opens with a keyframe");
                first = false;
            }
            let frame_id = packet.frame_id;
            frames_seen.insert(frame_id);
        }
        thread.stop();
        let failures = testing::take_events(|e| matches!(e, CoreEvent::NativeStreamFailed { .. } | CoreEvent::StreamCaptureEnded));
        assert!(failures.is_empty(), "a requested stop reports nothing: {failures:?}");
        println!("streamed {:?} with {} after skipping VAAPI", started.codec, started.label);
    }

    /// Software above the CPU's limit is not opened, and that is not a
    /// failure of the encoder (nothing is told to the probe cache).
    #[test]
    fn software_is_not_opened_above_its_limits() {
        let mut remaining: std::collections::VecDeque<EncoderChoice> = [choice(Backend::Software, None)].into();
        let mut errors = Vec::new();
        assert!(open_next(&mut remaining, &config(3840, 2160, 60), &mut errors).is_none());
        assert_eq!(errors.len(), 1);
        assert!(errors[0].contains("software encoding stops at"), "{errors:?}");
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
