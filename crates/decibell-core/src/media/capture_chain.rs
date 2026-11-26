//! The microphone's path from the capture ring to a frame ready to encode:
//! resample to 48 kHz, cut into 20 ms frames, AEC / AGC (WebRTC) and noise
//! suppression (RNNoise), then the transmit gate with its edge shaping.
//!
//! Shared by the voice pipeline (`pipeline.rs`, which encodes and sends what
//! comes out) and the microphone test outside voice (`mic_test.rs`, which
//! only meters it), so the test shows the level voice would send.

use ringbuf::{traits::{Consumer, Observer}, HeapCons};
use rubato::{Resampler, SincFixedOut};

use super::audio_device::make_sinc_resampler;
use super::codec::{FRAME_SIZE, SAMPLE_RATE};
use super::voice_gate::Gate as VoiceGate;

/// The WebRTC processor and RNNoise both work on 10 ms at 48 kHz.
pub(crate) const AEC_FRAME_SIZE: usize = 480;

/// RMS of a frame in dBFS; -96 for digital silence.
pub(crate) fn rms_db(frame: &[i16]) -> f32 {
    let sum_sq: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / frame.len() as f64).sqrt() as f32;
    if rms > 0.0 {
        20.0 * (rms / 32768.0).log10()
    } else {
        -96.0
    }
}

// WebRTC NS is disabled: RNNoise handles suppression (avoids double-filtering).
fn build_voice_processor(aec: bool, ns_level: u8, agc: bool) -> Option<aec3::voip::VoipAec3> {
    // Only use WebRTC processor for AEC and/or AGC — RNNoise handles NS
    if !aec && !agc {
        return None;
    }
    // 48kHz mono, 10ms frames = 480 samples
    let builder = aec3::voip::VoipAec3::builder(48000, 1, 1)
        .enable_noise_suppression(false) // RNNoise handles this
        .enable_gain_controller2(agc);
    match builder.build() {
        Ok(processor) => {
            log::info!("[pipeline] Voice processor built: aec={}, ns_level={}, agc={}", aec, ns_level, agc);
            Some(processor)
        }
        Err(e) => {
            log::warn!("[pipeline] Failed to build voice processor: {}", e);
            None
        }
    }
}

fn build_rnnoise(ns_level: u8) -> Option<Box<nnnoiseless::DenoiseState<'static>>> {
    if ns_level == 0 {
        return None;
    }
    log::info!("[pipeline] RNNoise deep-learning noise suppressor enabled (level={})", ns_level);
    Some(nnnoiseless::DenoiseState::new())
}

pub(crate) struct CaptureChain {
    /// input device rate → 48 kHz; None when the device runs at 48 kHz.
    resampler: Option<SincFixedOut<f64>>,
    accum: Vec<f64>,
    /// Resampled 48 kHz capture PCM awaiting a full frame.
    buf_48k: Vec<i16>,
    aec_enabled: bool,
    /// 0=off, 1=light, 2=moderate, 3=aggressive, 4=very aggressive
    ns_level: u8,
    agc_enabled: bool,
    /// VoipAec3 bundles AEC and AGC; rebuilt when a toggle changes.
    processor: Option<aec3::voip::VoipAec3>,
    /// RNNoise, when ns_level > 0: 480-sample (10 ms) frames at 48 kHz.
    rnnoise: Option<Box<nnnoiseless::DenoiseState<'static>>>,
    /// AEC render reference awaiting a full 10 ms frame.
    render_accum: Vec<f32>,
    /// A toggle just replaced the processor / the suppressor: what was there
    /// before (None inside: nothing was), for one more frame, which is the
    /// crossfade from the old processing to the new.
    outgoing_processor: Option<Option<aec3::voip::VoipAec3>>,
    outgoing_rnnoise: Option<Option<Box<nnnoiseless::DenoiseState<'static>>>>,
    /// The frame before the one being processed, as captured: what a new
    /// processor is warmed up on.
    last_raw: [i16; FRAME_SIZE],
}

impl CaptureChain {
    pub fn new(input_rate: u32) -> Self {
        let mut c = CaptureChain {
            resampler: None,
            accum: Vec::new(),
            buf_48k: Vec::with_capacity(FRAME_SIZE * 4),
            aec_enabled: false,
            ns_level: 0,
            agc_enabled: false,
            processor: None,
            rnnoise: None,
            render_accum: Vec::with_capacity(AEC_FRAME_SIZE * 2),
            outgoing_processor: None,
            outgoing_rnnoise: None,
            last_raw: [0; FRAME_SIZE],
        };
        c.set_input_rate(input_rate);
        c
    }

    /// The capture device's rate (a new device): rebuilds the resampler.
    pub fn set_input_rate(&mut self, rate: u32) {
        if rate == SAMPLE_RATE {
            self.resampler = None;
            log::info!("[pipeline] Input device {}Hz — passthrough", rate);
        } else {
            self.resampler = Some(make_sinc_resampler(rate, SAMPLE_RATE, 480, 1));
            log::info!("[pipeline] Input device {}Hz — resampler to {}Hz", rate, SAMPLE_RATE);
        }
    }

    /// Drop captured audio not yet cut into a frame (the device is changing).
    pub fn clear_capture(&mut self) {
        self.buf_48k.clear();
        self.accum.clear();
    }

    pub fn aec_enabled(&self) -> bool {
        self.aec_enabled
    }

    pub fn has_processor(&self) -> bool {
        self.processor.is_some()
    }

    /// A new processor for the current toggles. The one it replaces (echo
    /// canceller and gain control are one object: its adapted filter and the
    /// gain it had settled on can't be carried over) processes the next frame
    /// too, and that frame is the crossfade between them. `crossfade`: false
    /// when the input itself is changing (a new device).
    fn rebuild_processor(&mut self, crossfade: bool) {
        let old = self.processor.take();
        if crossfade && self.outgoing_processor.is_none() {
            self.outgoing_processor = Some(old);
        }
        self.processor = build_voice_processor(self.aec_enabled, self.ns_level, self.agc_enabled);
        self.render_accum.clear();
    }

    /// Each of these says whether the processor was rebuilt; the caller then
    /// empties the render reference ring (what is queued there belongs to
    /// the old one). Setting what is already set does nothing.
    pub fn set_aec(&mut self, enabled: bool) -> bool {
        if self.aec_enabled == enabled {
            return false;
        }
        self.aec_enabled = enabled;
        self.rebuild_processor(true);
        true
    }

    /// The level only says on (1–4) or off (0): RNNoise has no strength
    /// setting and the WebRTC processor does no suppression. So a change
    /// between two "on" levels changes nothing, and none touches the echo
    /// canceller or gain control (it used to rebuild all three).
    pub fn set_ns_level(&mut self, level: u8) -> bool {
        self.ns_level = level;
        if (level > 0) != self.rnnoise.is_some() {
            let old = self.rnnoise.take();
            if self.outgoing_rnnoise.is_none() {
                self.outgoing_rnnoise = Some(old);
            }
            self.rnnoise = build_rnnoise(level);
        }
        false
    }

    pub fn set_agc(&mut self, enabled: bool) -> bool {
        if self.agc_enabled == enabled {
            return false;
        }
        self.agc_enabled = enabled;
        self.rebuild_processor(true);
        true
    }

    /// The input device changed: a processor in use starts from scratch.
    /// True when it was rebuilt (the caller empties the render reference ring).
    pub fn reset_processor(&mut self) -> bool {
        self.outgoing_processor = None;
        self.outgoing_rnnoise = None;
        if self.processor.is_none() {
            return false;
        }
        self.rebuild_processor(false);
        true
    }

    /// Feed the AEC what is being played: drain the render reference ring and
    /// hand the processor whole 10 ms frames.
    pub fn feed_render(&mut self, cons: &mut HeapCons<f32>) {
        if self.processor.is_none() {
            return;
        }
        while let Some(s) = cons.try_pop() {
            self.render_accum.push(s);
        }
        while self.render_accum.len() >= AEC_FRAME_SIZE {
            let chunk: Vec<f32> = self.render_accum.drain(..AEC_FRAME_SIZE).collect();
            if let Some(ref mut proc) = self.processor {
                let _ = proc.handle_render_frame(&chunk);
            }
        }
    }

    /// Take everything the capture callback queued (mono i16 at the device's
    /// rate) and bring it to 48 kHz.
    pub fn pull(&mut self, cons: &mut HeapCons<i16>) {
        let avail = cons.occupied_len();
        if avail > 0 {
            if self.resampler.is_none() {
                // Input device is 48kHz — copy directly to the 48k buffer
                for _ in 0..avail {
                    if let Some(s) = cons.try_pop() {
                        self.buf_48k.push(s);
                    }
                }
            } else {
                // Drain raw samples into the resampler accumulator
                for _ in 0..avail {
                    if let Some(s) = cons.try_pop() {
                        self.accum.push(s as f64 / 32768.0);
                    }
                }
            }
        }
        // Resample accumulated raw samples: input_rate → 48kHz
        if let Some(ref mut resampler) = self.resampler {
            let mut needed = resampler.input_frames_next();
            while self.accum.len() >= needed {
                let chunk: Vec<f64> = self.accum.drain(..needed).collect();
                if let Ok(out) = resampler.process(&[&chunk], None) {
                    for &s in &out[0] {
                        self.buf_48k.push((s * 32768.0).clamp(-32768.0, 32767.0) as i16);
                    }
                }
                needed = resampler.input_frames_next();
            }
        }
    }

    /// The next whole Opus frame (960 samples at 48 kHz = 20 ms), if captured.
    pub fn next_frame(&mut self) -> Option<[i16; FRAME_SIZE]> {
        if self.buf_48k.len() < FRAME_SIZE {
            return None;
        }
        let mut frame = [0i16; FRAME_SIZE];
        frame.copy_from_slice(&self.buf_48k[..FRAME_SIZE]);
        self.buf_48k.drain(..FRAME_SIZE);
        Some(frame)
    }

    /// AEC / AGC, then noise suppression, in place. Runs before the level is
    /// measured so the gate sees what AGC made of the signal.
    pub fn process(&mut self, frame: &mut [i16; FRAME_SIZE]) {
        let outgoing_processor = self.outgoing_processor.take();
        let outgoing_rnnoise = self.outgoing_rnnoise.take();
        let previous = std::mem::replace(&mut self.last_raw, *frame);
        if self.processor.is_none() && self.rnnoise.is_none() && outgoing_processor.is_none() && outgoing_rnnoise.is_none() {
            return;
        }
        let mut f32_frame: Vec<f32> = frame.iter().map(|&s| s as f32 / 32768.0).collect();

        // Pass 1: WebRTC AEC + AGC (if enabled)
        match outgoing_processor {
            None => run_processor(self.processor.as_mut(), &mut f32_frame),
            Some(mut old) => {
                let mut old_out = f32_frame.clone();
                run_processor(old.as_mut(), &mut old_out);
                // A new processor's gain controller steps its gain 5 ms into
                // the first audio it is given (a jump of a third of the
                // wave's value, measured). It takes that start on the frame
                // before this one, whose output is not used.
                let mut warm_up: Vec<f32> = previous.iter().map(|&s| s as f32 / 32768.0).collect();
                run_processor(self.processor.as_mut(), &mut warm_up);
                run_processor(self.processor.as_mut(), &mut f32_frame);
                crossfade(&old_out, &mut f32_frame);
            }
        }

        // Pass 2: RNNoise deep-learning noise suppression
        match outgoing_rnnoise {
            None => run_rnnoise(self.rnnoise.as_deref_mut(), &mut f32_frame),
            Some(mut old) => {
                let mut old_out = f32_frame.clone();
                run_rnnoise(old.as_deref_mut(), &mut old_out);
                run_rnnoise(self.rnnoise.as_deref_mut(), &mut f32_frame);
                crossfade(&old_out, &mut f32_frame);
            }
        }

        for (i, s) in f32_frame.iter().enumerate() {
            frame[i] = (s * 32768.0).clamp(-32768.0, 32767.0) as i16;
        }
    }
}

/// `frame` through the WebRTC processor (AEC + AGC), two 10 ms halves.
fn run_processor(processor: Option<&mut aec3::voip::VoipAec3>, f32_frame: &mut [f32]) {
    let Some(proc) = processor else { return };
    let mut output_buf = vec![0.0f32; AEC_FRAME_SIZE];
    for chunk_idx in 0..2 {
        let start = chunk_idx * AEC_FRAME_SIZE;
        let end = start + AEC_FRAME_SIZE;
        if proc.process_capture_frame(&f32_frame[start..end], false, &mut output_buf).is_ok() {
            f32_frame[start..end].copy_from_slice(&output_buf);
        }
    }
}

/// `frame` through RNNoise, two 10 ms halves.
fn run_rnnoise(rnnoise: Option<&mut nnnoiseless::DenoiseState<'static>>, f32_frame: &mut [f32]) {
    let Some(rnn) = rnnoise else { return };
    let mut rnn_in = [0.0f32; AEC_FRAME_SIZE];
    let mut rnn_out = [0.0f32; AEC_FRAME_SIZE];
    for chunk_idx in 0..2 {
        let start = chunk_idx * AEC_FRAME_SIZE;
        let end = start + AEC_FRAME_SIZE;
        // RNNoise expects samples in [-32768, 32767] range
        for (j, s) in f32_frame[start..end].iter().enumerate() {
            rnn_in[j] = s * 32768.0;
        }
        let _vad = rnn.process_frame(&mut rnn_out, &rnn_in);
        // Convert back to [-1, 1] range
        for (j, s) in rnn_out.iter().enumerate() {
            f32_frame[start + j] = s / 32768.0;
        }
    }
}

/// `new` becomes a linear crossfade from `old` to itself across the frame.
fn crossfade(old: &[f32], new: &mut [f32]) {
    let n = new.len() as f32;
    for (i, (s, &o)) in new.iter_mut().zip(old).enumerate() {
        let w = (i + 1) as f32 / n;
        *s = o * (1.0 - w) + *s * w;
    }
}

// ── Transmit gate ────────────────────────────────────────────────────────────

// Hysteresis + hang time stop near-threshold flutter. When the level hovers
// around the threshold, raw per-frame thresholding flip-flops between "send
// voice" and "send silence" every 20ms. Each flip creates a waveform
// discontinuity the listener perceives as a quiet pop. The gate opens at the
// threshold, stays open while rms >= (threshold - 6dB), and keeps
// transmitting for GATE_HANG_FRAMES extra frames after falling below the
// close threshold.
const GATE_HYSTERESIS_DB: f32 = 6.0;
const GATE_HANG_FRAMES: u32 = 10; // 200ms @ 20ms/frame
// When the gate closes we don't stop dead: a few more frames go out at
// normal cadence, so the receiver's decoder renders the voice→silence
// transition itself and its playback ring runs out on true zeros.
pub(crate) const GATE_TAIL_FRAMES: u32 = 3; // 60ms
const GATE_FADE_IN: usize = 480; // 10ms

/// What the gate decided for one frame.
pub(crate) struct TxFrame {
    /// The frame held back before the gate opened, faded in: sent first.
    pub pre_roll: Option<[i16; FRAME_SIZE]>,
    /// The gate is open: this frame is voice.
    pub transmit_voice: bool,
    /// The gate just closed: this is tail frame 0, 1 or 2 (faded out here).
    pub tail_index: Option<u32>,
    pub opened: bool,
}

pub(crate) struct TxGate {
    open: bool,
    hang_remaining: u32,
    tail_remaining: u32,
    /// The most recent processed frame that was NOT transmitted; sent as
    /// pre-roll when the gate opens so a word's attack isn't chopped.
    pre_roll: Option<[i16; FRAME_SIZE]>,
}

impl TxGate {
    pub fn new() -> Self {
        TxGate { open: false, hang_remaining: 0, tail_remaining: 0, pre_roll: None }
    }

    /// Whether this frame goes through the voice processing: always unless
    /// muted, and then still while the frames of the talkspurt's end go out
    /// (the frame the mute lands on and the fade tail). They used to be the
    /// raw microphone: a jump in level and tone against the processed frames
    /// before them.
    pub fn wants_processing(&self, muted: bool) -> bool {
        !muted || self.open || self.tail_remaining > 0
    }

    /// The input device is about to change: whatever was being sent ends
    /// here, with no fade tail and no pre-roll (they would be the old
    /// device's audio against the new one's). True when a talkspurt was
    /// under way: the caller then sends one silence-flagged frame, which
    /// tells the listener to fade out the last voice frame it holds.
    pub fn cut(&mut self) -> bool {
        let sending = self.open || self.tail_remaining > 0;
        self.open = false;
        self.hang_remaining = 0;
        self.tail_remaining = 0;
        self.pre_roll = None;
        sending
    }

    /// Decide for `frame` (processed, its level `rms_db`) and shape the
    /// gate's edges in it.
    pub fn step(
        &mut self,
        frame: &mut [i16; FRAME_SIZE],
        rms_db: f32,
        threshold_db: f32,
        muted: bool,
        hotkey_gate: VoiceGate,
    ) -> TxFrame {
        let open_threshold = threshold_db;
        let close_threshold = threshold_db - GATE_HYSTERESIS_DB;
        // Push-to-talk / push-to-mute (global hotkeys) force the gate
        // either way; a forced close is a closed gate, not a mute, so
        // peers see "not speaking" and the fade tail still runs.
        let was_open = self.open;
        if muted || hotkey_gate == VoiceGate::Closed {
            self.open = false;
            self.hang_remaining = 0;
        } else if hotkey_gate == VoiceGate::Open {
            self.open = true;
            self.hang_remaining = 0;
        } else if rms_db >= open_threshold {
            self.open = true;
            self.hang_remaining = GATE_HANG_FRAMES;
        } else if self.open {
            if rms_db >= close_threshold {
                // Soft zone — keep open and refresh hang.
                self.hang_remaining = GATE_HANG_FRAMES;
            } else if self.hang_remaining > 0 {
                self.hang_remaining -= 1;
            } else {
                self.open = false;
            }
        }
        let transmit_voice = self.open;
        let opened = !was_open && self.open;
        let closed = was_open && !self.open;
        if closed {
            self.tail_remaining = GATE_TAIL_FRAMES;
        }

        // ── Gate edges ───────────────────────────────────────────
        // A 20ms-grid gate chops the waveform at its edges, and the
        // codec only smooths ~2.5ms of that:
        //  - Opening: the frame that crossed the threshold is already
        //    loud (a word's attack rises 30dB inside one frame), so the
        //    listener gets silence → loud with the attack cut off — a
        //    pop at every word start. Fix: pre-roll. Send the previous,
        //    sub-threshold frame first (one frame of lookahead paid
        //    only at talkspurt starts, never in steady state) with a
        //    10ms fade-in on it, so the natural attack is transmitted.
        //  - Closing: after the hang the signal is the room's noise
        //    floor, often lifted by AGC; a zero frame after it is a
        //    step. Fix: the three tail frames are the real signal
        //    faded out over 60ms, not encoded silence.
        let mut pre_roll_frame: Option<[i16; FRAME_SIZE]> = None;
        if opened {
            // No pre-roll on a push-to-talk press: that frame is audio
            // from before the key went down. Fade this one in instead.
            let held_back = self.pre_roll.take();
            match if hotkey_gate == VoiceGate::Open { None } else { held_back } {
                Some(mut pr) => {
                    for (i, s) in pr.iter_mut().take(GATE_FADE_IN).enumerate() {
                        *s = (*s as f32 * (i as f32 / GATE_FADE_IN as f32)) as i16;
                    }
                    pre_roll_frame = Some(pr);
                }
                None => {
                    // Nothing held back (previous frame was sent as a
                    // tail frame, or first frame ever): ramp this one.
                    for (i, s) in frame.iter_mut().take(GATE_FADE_IN).enumerate() {
                        *s = (*s as f32 * (i as f32 / GATE_FADE_IN as f32)) as i16;
                    }
                }
            }
        }
        let tail_index = if !transmit_voice && self.tail_remaining > 0 {
            Some(GATE_TAIL_FRAMES - self.tail_remaining) // 0, 1, 2
        } else {
            None
        };
        if let Some(t) = tail_index {
            let n = frame.len() as f32;
            let g0 = 1.0 - t as f32 / GATE_TAIL_FRAMES as f32;
            let g1 = 1.0 - (t + 1) as f32 / GATE_TAIL_FRAMES as f32;
            for (i, s) in frame.iter_mut().enumerate() {
                let g = g0 + (g1 - g0) * (i as f32 / n);
                *s = (*s as f32 * g) as i16;
            }
            self.tail_remaining -= 1;
        }
        // Hold back an unsent frame as next talkspurt's pre-roll.
        self.pre_roll = if transmit_voice || tail_index.is_some() { None } else { Some(*frame) };

        TxFrame { pre_roll: pre_roll_frame, transmit_voice, tail_index, opened }
    }
}
