// Per-remote-peer audio state.
//
// Each peer owns an independent {jitter buffer → Opus decoder → resampler →
// ring buffer} chain. The output audio callback holds a snapshot (via ArcSwap)
// of all peers' ring-buffer consumers and sums them sample-by-sample. This
// replaces the older "decode-all-then-mix-into-one-shared-ring" design that
// stalled every listener on the slowest decoder.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Instant;

use ringbuf::{HeapCons, HeapProd, HeapRb, traits::{Observer, Split}};
use rubato::{Resampler, SincFixedOut};

use super::audio_device::{make_sinc_resampler, soft_clip};
use super::codec::{OpusDecoder, StereoOpusDecoder, FRAME_SIZE, SAMPLE_RATE};
use super::jitter::JitterBuffer;
use super::speaking::SpeakingDetector;

// The Linux-only STREAM_AUDIO_DELAY_MS hold queue (1s) is gone. It
// compensated for the Tauri-era WebKitGTK MSE video player's ~1s buffer
// cushion; the Electron client decodes video with WebCodecs and paints
// immediately on every platform, so the hold had become a pure 1s+ audio
// lag behind video (reported as "stream audio is seconds behind").

/// ~1s of headroom at 48kHz. Generous — the callback drains continuously.
pub const PEER_RING_CAP: usize = FRAME_SIZE * 48;

/// Consumer handle exposed to the output callback. Shared via ArcSwap list.
pub type PeerCons = Arc<std::sync::Mutex<HeapCons<i16>>>;

/// Lightweight snapshot entry for the output callback. Contains just what the
/// callback needs: the ring-buffer consumer. (Username kept for diagnostics.)
#[derive(Clone)]
pub struct PeerOutput {
    pub username: String,
    pub cons: PeerCons,
}

pub struct PeerAudio {
    pub decoder: OpusDecoder,
    pub speaking: SpeakingDetector,
    pub last_packet_time: Instant,

    pub voice_jitter: JitterBuffer,
    pub voice_drain_time: Instant,
    pub voice_underrun_logged: bool,
    /// Last time the speaking detector was fed a silent tick while idle
    /// (rate-limits those ticks to frame cadence under ring-level pacing).
    pub last_silent_tick: Instant,
    /// Decoded 20ms frames (mono 48kHz, gain applied) queued for the AEC
    /// render reference. Decoding is paced by the playback ring and can
    /// burst several frames in one tick; the render flush (pipeline §4c)
    /// consumes exactly one per 20ms so the reference keeps playback timing.
    pub render_fifo: VecDeque<[f32; FRAME_SIZE]>,
    /// Set when the jitter buffer went idle; the next frame pushed into an
    /// empty ring gets a short fade-in so a talkspurt never starts with a
    /// step from digital silence (a 20ms VAD gate opens mid-waveform).
    pub onset_pending: bool,
    /// Last (muted, deafened) forwarded to the UI — used to emit the state
    /// event only when it changes rather than on every audio packet.
    pub last_reported_state: Option<(bool, bool)>,

    // Stream audio (screen-share audio) — unchanged location for now.
    pub stream_audio_decoder: Option<StereoOpusDecoder>,
    pub stream_jitter: JitterBuffer,
    pub stream_drain_time: Instant,

    /// Producer into this peer's voice ring. Consumed by output callback.
    prod: HeapProd<i16>,
    cons: PeerCons,

    /// 48kHz → output device rate. None when rates already match.
    resampler: Option<SincFixedOut<f64>>,
    resamp_accum: Vec<f64>,
    /// Reusable chunk handed to the resampler — drained from
    /// resamp_accum each iteration. Persistent so the per-20ms decode
    /// path stops allocating a fresh Vec per chunk.
    resamp_scratch: Vec<f64>,
    output_rate: u32,
}

impl PeerAudio {
    pub fn new(output_rate: u32, now: Instant) -> Self {
        let rb = HeapRb::<i16>::new(PEER_RING_CAP);
        let (prod, cons) = rb.split();
        let cons = Arc::new(std::sync::Mutex::new(cons));
        let resampler = if output_rate == SAMPLE_RATE {
            None
        } else {
            Some(make_sinc_resampler(SAMPLE_RATE, output_rate, 480, 1))
        };
        Self {
            decoder: OpusDecoder::new().expect("OpusDecoder::new failed"),
            speaking: SpeakingDetector::new(),
            last_packet_time: now,
            voice_jitter: JitterBuffer::new(),
            voice_drain_time: now,
            voice_underrun_logged: false,
            last_silent_tick: now,
            render_fifo: VecDeque::new(),
            onset_pending: true,
            last_reported_state: None,
            stream_audio_decoder: None,
            stream_jitter: JitterBuffer::new(),
            stream_drain_time: now,
            prod,
            cons,
            resampler,
            resamp_accum: Vec::with_capacity(1024),
            resamp_scratch: Vec::with_capacity(1024),
            output_rate,
        }
    }

    pub fn output_handle(&self, username: &str) -> PeerOutput {
        PeerOutput {
            username: username.to_string(),
            cons: Arc::clone(&self.cons),
        }
    }

    /// Rebuild the resampler for a new output device sample rate and drop any
    /// pending samples. Also clears the ring so playback resumes cleanly.
    pub fn set_output_rate(&mut self, output_rate: u32) {
        if output_rate == self.output_rate {
            return;
        }
        self.output_rate = output_rate;
        self.resampler = if output_rate == SAMPLE_RATE {
            None
        } else {
            Some(make_sinc_resampler(SAMPLE_RATE, output_rate, 480, 1))
        };
        self.resamp_accum.clear();
        if let Ok(mut cons) = self.cons.lock() {
            use ringbuf::traits::Consumer;
            while cons.try_pop().is_some() {}
        }
    }

    /// Samples (at the output device rate) queued in this peer's ring and not
    /// yet consumed by the output callback.
    pub fn ring_len(&self) -> usize {
        self.prod.occupied_len()
    }

    /// Output device sample rate this peer's ring is filled at.
    pub fn output_rate(&self) -> u32 {
        self.output_rate
    }

    /// Push a decoded 20ms frame (960 f32 samples at 48kHz, per-user gain
    /// already applied) through the resampler (if any) and into the peer's
    /// ring buffer. The ring is i16, so the gain-boosted signal is
    /// soft-limited here — the output mixer's limiter only sees the sum and
    /// could never undo a hard clip that already happened at this conversion.
    pub fn push_voice_frame(&mut self, pcm_f32: &[f32]) {
        use ringbuf::traits::Producer;
        if self.resampler.is_none() {
            // Direct 48kHz → i16
            for &s in pcm_f32 {
                let q = (soft_clip(s) * 32767.0) as i16;
                let _ = self.prod.try_push(q);
            }
            return;
        }

        self.resamp_accum.extend(pcm_f32.iter().map(|&s| soft_clip(s) as f64));
        let resampler = self.resampler.as_mut().unwrap();
        let mut needed = resampler.input_frames_next();
        while self.resamp_accum.len() >= needed {
            self.resamp_scratch.clear();
            self.resamp_scratch.extend(self.resamp_accum.drain(..needed));
            if let Ok(out) = resampler.process(&[&self.resamp_scratch], None) {
                for &s in &out[0] {
                    let q = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                    let _ = self.prod.try_push(q);
                }
            }
            needed = resampler.input_frames_next();
        }
    }

    /// The talkspurt ended: push whatever is still inside the resampler's
    /// input accumulator through to the ring (zero-padded) so the tail is
    /// played out completely now, instead of being glued onto the front of
    /// the next talkspurt ten milliseconds of stale audio later.
    pub fn flush_tail(&mut self) {
        use ringbuf::traits::Producer;
        let Some(resampler) = self.resampler.as_mut() else { return };
        if self.resamp_accum.is_empty() { return; }
        let needed = resampler.input_frames_next();
        self.resamp_accum.resize(needed, 0.0);
        self.resamp_scratch.clear();
        self.resamp_scratch.extend(self.resamp_accum.drain(..));
        if let Ok(out) = resampler.process(&[&self.resamp_scratch], None) {
            for &s in &out[0] {
                let q = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                let _ = self.prod.try_push(q);
            }
        }
    }

    /// Drop any queued-but-not-yet-played samples (used when deafening or on
    /// device hot-swap).
    pub fn drain_ring(&mut self) {
        use ringbuf::traits::Consumer;
        if let Ok(mut cons) = self.cons.lock() {
            while cons.try_pop().is_some() {}
        }
        self.resamp_accum.clear();
    }
}
