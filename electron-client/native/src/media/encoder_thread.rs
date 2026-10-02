//! Encoder thread orchestration.
//!
//! Owns the FFmpeg D3D11VA encoder + the bitrate adjustment cadence.
//! Pulls BGRA D3D11 textures from the WGC capture channel, hands them
//! to the encoder, drains encoded packets into:
//!  - the existing UDP `VideoSender` (the wire — same path renderer-
//!    encoded chunks took before),
//!  - the renderer self-preview TSFN via `events::send_stream_frame`
//!    with the local user's username (so the user's own tile renders
//!    via the unified stream-frame bus).
//!
//! Runs on a single OS thread; all D3D11 state stays on this thread.
//! NACK-ratio readback is a TODO — bitrate adjustment currently passes
//! 0.0 (no adjustment); proper plumbing will land in a follow-up that
//! exposes VideoSender's NACK counters.
//!
//! Exit path: on a fatal error / too many failed frames / panic the
//! thread emits `native_stream_failed` FIRST (unless it was asked to
//! stop) and never drains the encoder (send_eof + receive): a wedged
//! encoder — e.g. AMF after a TDR removed the device — may never return
//! from that, and the event must not hang behind it.

#![cfg(target_os = "windows")]

use std::cell::Cell;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::encoder::{Encoder, FrameOutcome};
use super::gpu_pipeline::GpuDevice;
use super::thumbnail::ThumbnailGenerator;
use super::video_pipeline::VideoSender;
use crate::events;

const THUMBNAIL_INTERVAL: Duration = Duration::from_secs(3);

/// Consecutive frames that may fail (dropped by the encoder, failed blit,
/// failed packet receive) before the stream is declared dead. ~1 s at
/// 60 fps: a transient hiccup (pool momentarily exhausted, encoder input
/// full) recovers well inside it; a dead device fails every frame.
const MAX_CONSECUTIVE_FAILURES: u32 = 60;

/// How long `stop()` waits for the thread before detaching it. The loop
/// polls its stop flag every ≤100 ms; only a thread wedged inside the
/// encoder/driver overruns this, and joining it would hang the caller
/// (app quit runs this under the AppState lock).
const STOP_JOIN_TIMEOUT: Duration = Duration::from_secs(3);

pub struct EncoderThread {
    stop: Arc<AtomicBool>,
    force_keyframe: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

pub struct EncoderThreadConfig {
    pub encoder_name: String,
    /// Wire codec byte: 1=H264_HW, 3=H265, 4=AV1. Stamped into every
    /// UdpVideoPacket so receivers pick the right decoder.
    pub codec_wire_byte: u8,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub local_username: String,
    pub video_sender: Arc<VideoSender>,
    /// Sender for thumbnail JPEG bytes. The encoder thread produces a
    /// thumbnail every THUMBNAIL_INTERVAL and pushes through this; a
    /// tokio task (set up in `VideoEngine::start_windows`) drains it
    /// and ships the bytes to the community server. Bounded depth=1
    /// drop-newest (via try_send) so a slow server doesn't back up
    /// the encoder thread.
    pub thumbnail_tx: tokio::sync::mpsc::Sender<Vec<u8>>,
}

/// Why the encode loop returned.
enum LoopExit {
    /// The stop flag was set (or the capture ended because of it).
    Stopped,
    /// The stream is dead; the reason goes to the renderer.
    Failed(String),
}

impl EncoderThread {
    pub fn start(
        gpu: GpuDevice,
        cfg: EncoderThreadConfig,
        rx: mpsc::Receiver<ID3D11Texture2D>,
    ) -> Result<Self, String> {
        // Open encoder up front so any failure surfaces in start() not
        // deep in the thread.
        let mut encoder = Encoder::open(
            &gpu,
            &cfg.encoder_name,
            cfg.width,
            cfg.height,
            cfg.fps,
            cfg.bitrate_kbps,
        )?;
        let force_keyframe = encoder.force_keyframe_handle();

        // The thumbnail generator owns its own readback resources; keeps
        // the encoder loop free of D3D11 readback bookkeeping.
        let mut thumb = ThumbnailGenerator::new(gpu);

        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-encoder".to_string())
            .spawn(move || {
                // catch_unwind: a panic anywhere in the encode loop
                // (FFmpeg binding, D3D11 interop, driver quirk) must
                // not silently kill this thread while the app keeps
                // claiming to stream. Contained, logged, and surfaced
                // to the renderer below.
                let exit = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    run_encode_loop(&mut encoder, &mut thumb, rx, &cfg, &stop_t)
                }));
                let failure = match exit {
                    Ok(LoopExit::Stopped) => None,
                    Ok(LoopExit::Failed(reason)) => Some(reason),
                    Err(_) => {
                        log::error!("[encoder] encode loop panicked — stream is dead");
                        Some("encoder panicked".to_string())
                    }
                };
                // Tell the renderer FIRST — before anything below (the
                // encoder's teardown on drop) gets a chance to block on a
                // dead device — so it can tear the stream state down and
                // toast instead of showing "Streaming" over nothing.
                // Suppressed when we were asked to stop: that exit is
                // expected, and a late event would kill the NEXT session.
                if let Some(reason) = failure {
                    if stop_t.load(Ordering::Relaxed) {
                        log::info!("[encoder] loop ended during stop: {reason}");
                    } else {
                        log::error!("[encoder] stream failed: {reason}");
                        events::send("native_stream_failed", serde_json::Value::String(reason));
                    }
                }
                // Deliberately no drain (send_eof + receive loop): it
                // only flushes the last frame or two of a stream that is
                // ending anyway, and a wedged encoder never returns from
                // it. `encoder` / `thumb` drop here.
            })
            .map_err(|e| format!("spawn encoder thread: {e}"))?;

        Ok(Self {
            stop,
            force_keyframe,
            thread: Some(thread),
        })
    }

    pub fn force_keyframe_handle(&self) -> Arc<AtomicBool> {
        self.force_keyframe.clone()
    }

    /// Mark the thread as stopping without waiting for it. Call before
    /// tearing down the capture that feeds it: the loop then reads the
    /// capture channel's disconnect as the expected end, not a failure.
    pub fn signal_stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }

    /// Signal stop and join, bounded by STOP_JOIN_TIMEOUT. A thread still
    /// running after that is wedged inside the encoder/driver: it is
    /// detached (keeps its resources, can't fire a failure event — the
    /// stop flag is set) rather than hanging the caller forever.
    pub fn stop(mut self) {
        self.signal_stop();
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
                "[encoder] encoder thread did not exit within {:?} — detaching it (wedged in the encoder/driver?)",
                STOP_JOIN_TIMEOUT
            );
        }
    }
}

impl Drop for EncoderThread {
    /// Dropped without `stop()` (an error path): at least make sure the
    /// thread doesn't report the capture's disconnect as a failure.
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn run_encode_loop(
    encoder: &mut Encoder,
    thumb: &mut ThumbnailGenerator,
    rx: mpsc::Receiver<ID3D11Texture2D>,
    cfg: &EncoderThreadConfig,
    stop: &AtomicBool,
) -> LoopExit {
    let mut last_telemetry = Instant::now();
    // Thumbnail on the first frame (checked_sub: Instant can't go below
    // the clock's origin).
    let mut last_thumbnail = Instant::now()
        .checked_sub(THUMBNAIL_INTERVAL)
        .unwrap_or_else(Instant::now);
    let frames_sent = Cell::new(0u32);
    let mut thumbnails_sent = 0u32;
    let mut frames_dropped = 0u32;
    let mut consecutive_failures = 0u32;
    let mut last_drop_warn: Option<Instant> = None;
    // Wall-clock anchor for pts. The encoder time_base is 1/fps, so
    // each frame's pts = elapsed_us * fps / 1_000_000. This makes
    // timestamps track real time instead of encoded-frame count —
    // critical for the receiver's lag check (StreamVideoPlayer drops
    // any non-keyframe more than 500ms behind wall-clock). A monotonic
    // pts breaks that whenever capture stalls (NVENC buffer hiccup,
    // GPU contention with a game, scheduling jitter) because pts
    // falls behind real time and accumulates as lag until the next
    // GOP keyframe re-syncs the receiver clock.
    let stream_start = Instant::now();

    // Every encoded packet — from the per-frame drain and from the drain
    // send_bgra does when the encoder input is full — goes through here.
    let mut on_packet = |data: &[u8], is_key: bool, pkt_pts: i64| {
        // Wire: packetise + UDP send.
        cfg.video_sender.send_frame(cfg.codec_wire_byte, is_key, data);
        // Self-preview: ship same encoded bytes to renderer via per-
        // stream Buffer TSFN keyed by local username. Convert
        // packet pts (in time_base = 1/fps units) back to microseconds.
        // Because we set pts from wall-clock above, this round-trips
        // to ~ stream_start.elapsed().as_micros().
        // Only when a player is subscribed to our own stream — checked
        // before to_vec() so an unwatched self-preview copies nothing.
        // The player force_keyframe()s on subscribe to resume on an IDR.
        if events::is_stream_frame_sink(&cfg.local_username) {
            let timestamp_us = pkt_pts.saturating_mul(1_000_000) / cfg.fps.max(1) as i64;
            events::send_stream_frame(events::StreamFrame {
                username: cfg.local_username.clone(),
                codec: cfg.codec_wire_byte,
                keyframe: is_key,
                timestamp: timestamp_us,
                data: data.to_vec(),
                description: None,
                discontinuity: false,
            });
        }
        // Wire frames (telemetry), preview or not.
        frames_sent.set(frames_sent.get() + 1);
    };

    loop {
        if stop.load(Ordering::Relaxed) {
            return LoopExit::Stopped;
        }
        let bgra = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(t) => t,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => {
                // stop_windows signals us before stopping the capture, so
                // a disconnect without the flag means the capture died.
                return if stop.load(Ordering::Relaxed) {
                    LoopExit::Stopped
                } else {
                    LoopExit::Failed("capture stopped unexpectedly".to_string())
                };
            }
        };

        let elapsed_us = stream_start.elapsed().as_micros() as i64;
        let pts = elapsed_us.saturating_mul(cfg.fps as i64) / 1_000_000;

        // Thumbnail: ticked every frame BEFORE send_bgra, while the BGRA
        // texture is current. `due` asks the generator to start a new
        // (asynchronous, GPU-downscaled) readback if none is in flight; a
        // finished one comes back on whichever later tick it completes.
        let due = last_thumbnail.elapsed() >= THUMBNAIL_INTERVAL;
        match thumb.tick(&bgra, due) {
            Some(Ok(jpeg)) => {
                // try_send drops the JPEG on the floor if the tokio sender
                // task hasn't drained the previous one yet (depth=1
                // channel). That's fine — we'd rather drop a thumbnail
                // than back up the encoder thread waiting for the network.
                let _ = cfg.thumbnail_tx.try_send(jpeg);
                thumbnails_sent += 1;
            }
            Some(Err(e)) => log::warn!("[encoder/thumb] capture failed: {e}"),
            None => {}
        }
        if due {
            last_thumbnail = Instant::now();
        }

        let mut failure = match encoder.send_bgra(&bgra, pts, &mut on_packet) {
            Ok(FrameOutcome::Submitted) => None,
            Ok(FrameOutcome::Dropped(reason)) => Some(reason),
            Err(fatal) => return LoopExit::Failed(format!("encoder error: {fatal}")),
        };
        // Drain even after a drop: an exhausted pool frees up as packets
        // come out.
        if let Err(e) = encoder.for_each_packet(&mut on_packet) {
            failure.get_or_insert(e);
        }
        match failure {
            None => consecutive_failures = 0,
            Some(reason) => {
                consecutive_failures += 1;
                frames_dropped += 1;
                if consecutive_failures >= MAX_CONSECUTIVE_FAILURES {
                    return LoopExit::Failed(format!(
                        "{consecutive_failures} consecutive frames failed (last: {reason})"
                    ));
                }
                let warn_now = match last_drop_warn {
                    None => true,
                    Some(t) => t.elapsed() >= Duration::from_secs(1),
                };
                if warn_now {
                    log::warn!(
                        "[encoder] frame dropped: {reason} (consecutive={consecutive_failures})"
                    );
                    last_drop_warn = Some(Instant::now());
                }
            }
        }

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::info!(
                "[encoder] codec={} {}x{}@{} target={}kbps frames_sent={} dropped={} thumbs_sent={}",
                cfg.encoder_name,
                cfg.width,
                cfg.height,
                cfg.fps,
                cfg.bitrate_kbps,
                frames_sent.replace(0),
                frames_dropped,
                thumbnails_sent,
            );
            frames_dropped = 0;
            thumbnails_sent = 0;
            last_telemetry = Instant::now();
            // TODO(follow-up): plumb VideoSender NACK ratio readback so
            // we can pass the real value here. 0.0 = no adjustment.
            encoder.maybe_adjust_bitrate(0.0);
        }
    }
}
