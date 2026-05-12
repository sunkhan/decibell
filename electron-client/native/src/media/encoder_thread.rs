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

#![cfg(target_os = "windows")]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use windows::Win32::Graphics::Direct3D11::ID3D11Texture2D;

use super::encoder::Encoder;
use super::gpu_pipeline::GpuDevice;
use super::video_pipeline::VideoSender;
use crate::events;

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

        let stop = Arc::new(AtomicBool::new(false));
        let stop_t = stop.clone();
        let thread = std::thread::Builder::new()
            .name("decibell-encoder".to_string())
            .spawn(move || {
                run_encode_loop(&mut encoder, rx, &cfg, &stop_t);
                // Drain remaining packets on stop.
                let _ = encoder.drain();
                let _ = encoder.for_each_packet(|data, is_key, _pts| {
                    cfg.video_sender.send_frame(cfg.codec_wire_byte, is_key, data);
                });
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

    pub fn stop(mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

fn run_encode_loop(
    encoder: &mut Encoder,
    rx: mpsc::Receiver<ID3D11Texture2D>,
    cfg: &EncoderThreadConfig,
    stop: &AtomicBool,
) {
    let mut last_telemetry = Instant::now();
    let mut frames_sent = 0u32;
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
    while !stop.load(Ordering::Relaxed) {
        let bgra = match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(t) => t,
            Err(mpsc::RecvTimeoutError::Timeout) => continue,
            Err(mpsc::RecvTimeoutError::Disconnected) => break,
        };

        let elapsed_us = stream_start.elapsed().as_micros() as i64;
        let pts = elapsed_us.saturating_mul(cfg.fps as i64) / 1_000_000;

        if let Err(e) = encoder.send_bgra(&bgra, pts) {
            log::error!("[encoder] send_bgra: {e}");
            break;
        }
        let _ = encoder.for_each_packet(|data, is_key, pkt_pts| {
            // Wire: packetise + UDP send.
            cfg.video_sender.send_frame(cfg.codec_wire_byte, is_key, data);
            // Self-preview: ship same encoded bytes to renderer via per-
            // stream Buffer TSFN keyed by local username. Convert
            // packet pts (in time_base = 1/fps units) back to microseconds.
            // Because we set pts from wall-clock above, this round-trips
            // to ~ stream_start.elapsed().as_micros().
            let timestamp_us = pkt_pts.saturating_mul(1_000_000) / cfg.fps.max(1) as i64;
            events::send_stream_frame(events::StreamFrame {
                username: cfg.local_username.clone(),
                codec: cfg.codec_wire_byte,
                keyframe: is_key,
                timestamp: timestamp_us,
                data: data.to_vec(),
                description: None,
            });
            frames_sent += 1;
        });

        if last_telemetry.elapsed() >= Duration::from_secs(1) {
            log::info!(
                "[encoder] codec={} {}x{}@{} target={}kbps frames_sent={}",
                cfg.encoder_name,
                cfg.width,
                cfg.height,
                cfg.fps,
                cfg.bitrate_kbps,
                frames_sent,
            );
            frames_sent = 0;
            last_telemetry = Instant::now();
            // TODO(follow-up): plumb VideoSender NACK ratio readback so
            // we can pass the real value here. 0.0 = no adjustment.
            encoder.maybe_adjust_bitrate(0.0);
        }
    }
}
