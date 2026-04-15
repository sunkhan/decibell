// Per-remote-peer audio state.
//
// Each peer owns an independent {jitter buffer → Opus decoder → resampler →
// ring buffer} chain. The output audio callback holds a snapshot (via ArcSwap)
// of all peers' ring-buffer consumers and sums them sample-by-sample. This
// replaces the older "decode-all-then-mix-into-one-shared-ring" design that
// stalled every listener on the slowest decoder.

use std::sync::Arc;
use std::time::Instant;

use ringbuf::{HeapCons, HeapProd, HeapRb, traits::Split};
use rubato::{Resampler, SincFixedOut};

use super::audio_device::make_sinc_resampler;
use super::codec::{OpusDecoder, StereoOpusDecoder, FRAME_SIZE, SAMPLE_RATE};
use super::jitter::JitterBuffer;
use super::speaking::SpeakingDetector;

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
            stream_audio_decoder: None,
            stream_jitter: JitterBuffer::new(),
            stream_drain_time: now,
            prod,
            cons,
            resampler,
            resamp_accum: Vec::with_capacity(1024),
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

    /// Push a decoded 20ms frame (960 f32 samples at 48kHz) through the
    /// resampler (if any) and into the peer's ring buffer.
    pub fn push_voice_frame(&mut self, pcm_f32: &[f32]) {
        use ringbuf::traits::Producer;
        if self.resampler.is_none() {
            // Direct 48kHz → i16
            for &s in pcm_f32 {
                let q = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                let _ = self.prod.try_push(q);
            }
            return;
        }

        self.resamp_accum.extend(pcm_f32.iter().map(|&s| s as f64));
        let resampler = self.resampler.as_mut().unwrap();
        let mut needed = resampler.input_frames_next();
        while self.resamp_accum.len() >= needed {
            let chunk: Vec<f64> = self.resamp_accum.drain(..needed).collect();
            if let Ok(out) = resampler.process(&[&chunk], None) {
                for &s in &out[0] {
                    let q = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
                    let _ = self.prod.try_push(q);
                }
            }
            needed = resampler.input_frames_next();
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
