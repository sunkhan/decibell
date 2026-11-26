//! Offline end-to-end model of the voice path: synthetic talker → sender gate
//! (as in pipeline.rs) → Opus → network (delay / stall) → JitterBuffer →
//! Opus decode → per-peer ring → output device pull. Measures waveform
//! discontinuities ("pops") at the receiver output.
//!
//! The sender gate and the receiver drain loop are re-implemented here in
//! miniature, mirroring pipeline.rs. Keep them in sync when that changes.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use super::codec::{OpusDecoder, OpusEncoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE};
use super::jitter::JitterBuffer;

const FLAG_SILENCE: u8 = 0x04;

#[derive(Clone, Copy, Debug, PartialEq)]
pub enum SenderPolicy {
    /// Pre-0.7.1: encoded silence at full 50 pps whenever the gate is closed.
    Continuous,
    /// 0.7.1–0.7.3: stop sending when the gate closes; one unflagged
    /// keepalive every 500 ms.
    Gated,
    /// Proposed: gate closes → N flagged tail frames of encoded silence at
    /// cadence, then flagged keepalives every 500 ms.
    GatedTail,
}

#[derive(Clone, Copy, Debug)]
pub struct Scenario {
    pub name: &'static str,
    /// Packets sent inside [stall_from, stall_to) ms are all delivered at
    /// stall_to + delay (a queueing stall, no loss).
    pub stall: Option<(u32, u32)>,
    /// Receiver-side per-user gain (linear).
    pub gain: f32,
    /// Output device clock error vs. the wall clock (positive = device fast).
    pub device_drift_ppm: i32,
}

/// How the receiver decides when to decode the next frame into its ring.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum Pacing {
    /// 0.7.x: one frame per 20ms of wall clock, regardless of ring level.
    WallClock,
    /// Proposed: decode whenever the ring is below low-water (pull + margin).
    RingLevel,
}

pub struct SimResult {
    pub max_jump_dbfs: f32,
    /// Device callbacks that found fewer samples than they pull → zeros
    /// spliced into the waveform (each one is an audible click mid-speech).
    pub starvations: usize,
    pub zero_fill_samples: usize,
    pub jitter_target: usize,
    pub expand_frames: u64,
    /// PLC frames within 300ms of a talkspurt onset — the signature of a
    /// fill that counted stale keepalives as audio depth.
    pub expands_after_onset: u64,
    pub dropped_frames: u64,
    pub output: Vec<i16>,
}

struct Lcg(u64);
impl Lcg {
    fn next_f32(&mut self) -> f32 {
        self.0 = self.0.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
        ((self.0 >> 40) as f32 / (1u64 << 24) as f32) * 2.0 - 1.0
    }
}

/// Speech-ish source: harmonic stack at 180 Hz with a syllabic AM envelope
/// during talk segments, -60 dBFS noise floor elsewhere.
fn source_sample(t_s: f64, rng: &mut Lcg) -> f32 {
    // Three talkspurts: a 0.8s pause (one legacy keepalive) and a 2.4s pause
    // (four legacy keepalives) between them.
    let talking = (t_s < 1.0) || (t_s >= 1.8 && t_s < 2.6) || (t_s >= 5.0 && t_s < 5.8);
    let noise = rng.next_f32() * 0.001; // ≈ -60 dBFS rms
    if !talking {
        return noise;
    }
    let f0 = 180.0;
    let mut v = 0.0f64;
    for h in 1..=6 {
        v += (2.0 * std::f64::consts::PI * f0 * h as f64 * t_s).sin() / h as f64;
    }
    let env = 0.6 + 0.4 * (2.0 * std::f64::consts::PI * 4.0 * t_s).sin(); // 4 Hz syllables
    (v * 0.08 * env) as f32 + noise // peaks ≈ -14 dBFS
}

fn rms_db(frame: &[i16]) -> f32 {
    let sum_sq: f64 = frame.iter().map(|&s| (s as f64) * (s as f64)).sum();
    let rms = (sum_sq / frame.len() as f64).sqrt() as f32;
    if rms > 0.0 { 20.0 * (rms / 32768.0).log10() } else { -96.0 }
}

pub fn run(policy: SenderPolicy, pacing: Pacing, sc: Scenario, total_ms: u32) -> SimResult {
    const TICK_MS: u32 = 5;
    const NET_DELAY_MS: u32 = 30;
    /// Frames per output callback — measured on a PipeWire/ALSA desktop
    /// (1176 @ 44.1k ≈ 1280 @ 48k, i.e. 26.7ms per pull).
    const PULL_FRAMES: usize = 1280;
    const GATE_THRESHOLD_DB: f32 = -47.0;
    const GATE_HYSTERESIS_DB: f32 = 6.0;
    const GATE_HANG_FRAMES: u32 = 10;
    const KEEPALIVE_MS: u32 = 500;
    const TAIL_FRAMES: u32 = 3;

    let t0 = Instant::now();
    let at = |ms: u32| t0 + Duration::from_millis(ms as u64);

    let mut enc = OpusEncoder::new(64000).unwrap();
    let mut rng = Lcg(0x1234_5678);

    // ── Sender: produce (send_time_ms, seq, payload) ──────────────────────
    struct Sent { send_ms: u32, seq: u16, payload: Vec<u8> }
    let mut sent: Vec<Sent> = Vec::new();
    let mut seq: u16 = 0;
    let mut gate_open = false;
    let mut hang = 0u32;
    let mut tail_left = 0u32;
    let mut last_send_ms: i64 = -(KEEPALIVE_MS as i64);
    let mut frame_idx = 0u32;
    let mut send_ms = 0u32;
    while send_ms < total_ms {
        let mut frame = [0i16; FRAME_SIZE];
        for (i, s) in frame.iter_mut().enumerate() {
            let t_s = (frame_idx as f64 * FRAME_SIZE as f64 + i as f64) / 48000.0;
            let v = source_sample(t_s, &mut rng);
            *s = (v * 32768.0).clamp(-32768.0, 32767.0) as i16;
        }
        let db = rms_db(&frame);
        let was_open = gate_open;
        if db >= GATE_THRESHOLD_DB {
            gate_open = true; hang = GATE_HANG_FRAMES;
        } else if gate_open {
            if db >= GATE_THRESHOLD_DB - GATE_HYSTERESIS_DB { hang = GATE_HANG_FRAMES; }
            else if hang > 0 { hang -= 1; }
            else { gate_open = false; }
        }
        if was_open && !gate_open { tail_left = TAIL_FRAMES; }

        let mut out = [0u8; MAX_OPUS_FRAME_SIZE];
        let mut emit = |enc: &mut OpusEncoder, voice: bool, flags: u8, sent: &mut Vec<Sent>, seq: &mut u16| {
            let len = if voice { enc.encode(&frame, &mut out).unwrap() } else { enc.encode_silence(&mut out).unwrap() };
            let mut p = Vec::with_capacity(len + 1);
            p.push(flags);
            p.extend_from_slice(&out[..len]);
            sent.push(Sent { send_ms, seq: *seq, payload: p });
            *seq = seq.wrapping_add(1);
        };
        let keepalive_due = (send_ms as i64 - last_send_ms) >= KEEPALIVE_MS as i64;
        match policy {
            SenderPolicy::Continuous => {
                emit(&mut enc, gate_open, 0, &mut sent, &mut seq);
                last_send_ms = send_ms as i64;
            }
            SenderPolicy::Gated => {
                if gate_open { emit(&mut enc, true, 0, &mut sent, &mut seq); last_send_ms = send_ms as i64; }
                else if keepalive_due { emit(&mut enc, false, 0, &mut sent, &mut seq); last_send_ms = send_ms as i64; }
            }
            SenderPolicy::GatedTail => {
                if gate_open { emit(&mut enc, true, 0, &mut sent, &mut seq); last_send_ms = send_ms as i64; }
                else if tail_left > 0 { tail_left -= 1; emit(&mut enc, false, FLAG_SILENCE, &mut sent, &mut seq); last_send_ms = send_ms as i64; }
                else if keepalive_due { emit(&mut enc, false, FLAG_SILENCE, &mut sent, &mut seq); last_send_ms = send_ms as i64; }
            }
        }
        frame_idx += 1;
        send_ms += 20;
    }

    // ── Network: arrival times ─────────────────────────────────────────────
    let mut arrivals: Vec<(u32, u16, Vec<u8>)> = sent.into_iter().map(|s| {
        let mut arr = s.send_ms + NET_DELAY_MS;
        if let Some((from, to)) = sc.stall {
            if s.send_ms >= from && s.send_ms < to { arr = to + NET_DELAY_MS; }
        }
        (arr, s.seq, s.payload)
    }).collect();
    arrivals.sort_by_key(|a| (a.0, a.1));

    // ── Receiver (mirrors pipeline.rs §4b + peer ring + output callback) ──
    let mut jb = JitterBuffer::new();
    let mut dec = OpusDecoder::new().unwrap();
    let mut ring: VecDeque<i16> = VecDeque::new();
    let mut output: Vec<i16> = Vec::new();
    let mut drain_time = at(0);
    let frame_dur = Duration::from_millis(20);
    let max_behind = Duration::from_millis(100);
    let mut next_arrival = 0usize;
    // Output device: pulls PULL frames per callback on its own clock.
    let pull_period_ms = PULL_FRAMES as f64 / 48.0 * (1.0 + sc.device_drift_ppm as f64 * 1e-6);
    let mut next_pull_ms = 13.0f64; // arbitrary initial phase
    let low_water = PULL_FRAMES + 480; // pull + 10ms of loop jitter
    if pacing == Pacing::RingLevel {
        // Mirrors pipeline.rs prime_frames(): ring's top of range + 1.
        jb.set_prime_frames((low_water + FRAME_SIZE - 1) / FRAME_SIZE + 1);
    }
    let mut starvations = 0usize;
    let mut zero_fill = 0usize;
    let mut onset_ms: Option<u32> = None;
    let mut expands_after_onset = 0u64;
    let mut was_idle = true;
    let mut now_ms = 0u32;
    while now_ms < total_ms + 300 {
        let now = at(now_ms);
        if now_ms % TICK_MS == 0 {
            while next_arrival < arrivals.len() && arrivals[next_arrival].0 <= now_ms {
                let (_, s, ref p) = arrivals[next_arrival];
                let flagged = p.first().map(|f| f & FLAG_SILENCE != 0).unwrap_or(false);
                jb.push_at(s, p.clone(), now, flagged);
                next_arrival += 1;
            }
            if now.duration_since(drain_time) > max_behind { drain_time = now - max_behind; }
            let mut produced = 0;
            loop {
                let due = match pacing {
                    Pacing::WallClock => now.duration_since(drain_time) >= frame_dur,
                    Pacing::RingLevel => ring.len() < low_water,
                };
                if !due || produced >= 8 { break; }
                if pacing == Pacing::WallClock { drain_time += frame_dur; }
                let mut pcm = [0i16; FRAME_SIZE];
                let fr = jb.drain();
                if std::env::var("SIM_TRACE").is_ok() && (350..700).contains(&now_ms) {
                    let tag = match &fr { super::jitter::Frame::Packet(_) => "pkt", super::jitter::Frame::Lost => "lost", super::jitter::Frame::Expand => "EXPAND", super::jitter::Frame::Idle => "idle" };
                    println!("  t={} ring={} jb={} tgt={} ready={} -> {}", now_ms, ring.len(), jb.len(), jb.target(), jb.is_ready(), tag);
                }
                match &fr {
                    super::jitter::Frame::Packet(_) if was_idle => { onset_ms = Some(now_ms); was_idle = false; }
                    super::jitter::Frame::Idle => { was_idle = true; }
                    super::jitter::Frame::Expand => {
                        if let Some(o) = onset_ms { if now_ms - o < 300 { expands_after_onset += 1; } }
                    }
                    _ => {}
                }
                let decoded = match fr {
                    super::jitter::Frame::Packet(p) => {
                        let opus = if p.len() > 1 { &p[1..] } else { &p[..] };
                        dec.decode(opus, &mut pcm).is_ok()
                    }
                    super::jitter::Frame::Lost => {
                        if let Some(next) = jb.peek_next() {
                            let opus = if next.len() > 1 { &next[1..] } else { &next[..] };
                            dec.decode_fec(opus, &mut pcm).is_ok()
                        } else {
                            dec.decode(&[], &mut pcm).is_ok()
                        }
                    }
                    super::jitter::Frame::Expand => dec.decode(&[], &mut pcm).is_ok(),
                    super::jitter::Frame::Idle => {
                        if pacing == Pacing::WallClock { drain_time = now; }
                        break;
                    }
                };
                produced += 1;
                if decoded {
                    if jb.should_drop_excess(rms_db(&pcm) < -45.0) {
                        if pacing == Pacing::WallClock { drain_time -= frame_dur; }
                        continue;
                    }
                    for &s in pcm.iter() {
                        let v = (s as f32 / 32768.0) * sc.gain;
                        ring.push_back((v * 32768.0).clamp(-32768.0, 32767.0) as i16);
                    }
                }
            }
        }
        // Device callback(s) due this millisecond.
        while (now_ms as f64) >= next_pull_ms {
            next_pull_ms += pull_period_ms;
            let avail = ring.len();
            // A short pull splices zeros into audio: a partial pull always,
            // an empty pull only if audio was expected (the jitter buffer is
            // playing or concealing). A silent pull during a real sender
            // pause is just silence.
            let audio_expected = jb.is_ready() || jb.is_expanding();
            // The last 300ms are the end-of-run drain-out (sender simply
            // stops) — not a scenario we measure. Zeros appended to a ring
            // that already ended on digital silence (an encoded-silence tail
            // frame) are not a splice either.
            let in_window = now_ms + 300 < total_ms;
            let cut_audio = ring.back().map(|&x| (x as i32).abs() > 64).unwrap_or(false);
            if in_window && avail < PULL_FRAMES && ((avail > 0 && cut_audio) || (avail == 0 && audio_expected)) {
                starvations += 1;
                zero_fill += PULL_FRAMES - avail;
                if std::env::var("SIM_TRACE").is_ok() {
                    println!("  STARVE t={} avail={} last={:?} ready={} expanding={}", now_ms, avail, ring.back(), jb.is_ready(), jb.is_expanding());
                }
            }
            for _ in 0..PULL_FRAMES {
                output.push(ring.pop_front().unwrap_or(0));
            }
        }
        now_ms += 1;
    }

    // ── Metric: clicks = zero-fill gaps spliced into the stream ───────────
    let mut max_jump = 0.0f32;
    for w in output.windows(2) {
        let j = ((w[1] as i32 - w[0] as i32).abs() as f32) / 32768.0;
        if j > max_jump { max_jump = j; }
    }
    SimResult {
        max_jump_dbfs: if max_jump > 0.0 { 20.0 * max_jump.log10() } else { -96.0 },
        starvations,
        zero_fill_samples: zero_fill,
        jitter_target: jb.target(),
        expand_frames: jb.expand_frames,
        expands_after_onset,
        dropped_frames: jb.dropped_frames,
        output,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SCENARIOS: [Scenario; 4] = [
        Scenario { name: "clean link, device +40ppm", stall: None, gain: 1.0, device_drift_ppm: 40 },
        Scenario { name: "clean link, device -40ppm", stall: None, gain: 1.0, device_drift_ppm: -40 },
        Scenario { name: "clean link, +15dB gain, +40ppm", stall: None, gain: 5.6, device_drift_ppm: 40 },
        Scenario { name: "100ms stall mid-speech, +40ppm", stall: Some((400, 500)), gain: 1.0, device_drift_ppm: 40 },
    ];

    #[test]
    fn trace_stall() {
        if std::env::var("SIM_TRACE").is_err() { return; }
        let _ = run(SenderPolicy::GatedTail, Pacing::RingLevel, SCENARIOS[3], 6000);
    }

    /// SIM_DUMP=/dir writes the sim's device output for both pacing modes as
    /// WAVs, to exercise scripts/analyze_audio_dump.py on known artifacts.
    #[test]
    fn dump_sim_output() {
        let Some(dir) = std::env::var_os("SIM_DUMP") else { return };
        let dir = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&dir).unwrap();
        for (pacing, tag) in [(Pacing::WallClock, "wallclock"), (Pacing::RingLevel, "ring")] {
            let r = run(SenderPolicy::Gated, pacing, SCENARIOS[2], 6000);
            let mut w = super::super::debug_dump::WavWriter::create(dir.join(format!("output-{}.wav", tag)), 48000).unwrap();
            w.write(&r.output);
        }
    }

    #[test]
    fn print_table() {
        println!("\n{:<10} {:<12} {:<34} {:>6} {:>8} {:>5} {:>6} {:>7} {:>5} {:>9}",
            "pacing", "sender", "scenario", "starve", "zeros", "tgt", "expand", "onsetXP", "drop", "maxjump");
        for sc in SCENARIOS {
            for pacing in [Pacing::WallClock, Pacing::RingLevel] {
                for pol in [SenderPolicy::Continuous, SenderPolicy::Gated, SenderPolicy::GatedTail] {
                    let r = run(pol, pacing, sc, 6500);
                    println!("{:<10} {:<12} {:<34} {:>6} {:>8} {:>5} {:>6} {:>7} {:>5} {:>9.1}",
                        format!("{:?}", pacing), format!("{:?}", pol), sc.name,
                        r.starvations, r.zero_fill_samples, r.jitter_target, r.expand_frames, r.expands_after_onset, r.dropped_frames, r.max_jump_dbfs);
                }
            }
        }
    }

    /// With ring-level pacing the output device must never find the ring
    /// short while a talkspurt is playing, for any sender policy.
    #[test]
    fn ring_paced_receiver_never_starves() {
        for sc in SCENARIOS {
            for pol in [SenderPolicy::Continuous, SenderPolicy::Gated, SenderPolicy::GatedTail] {
                let r = run(pol, Pacing::RingLevel, sc, 6500);
                assert_eq!(r.starvations, 0, "{:?} / {}: spliced zeros (starve={}, zeros={})",
                    pol, sc.name, r.starvations, r.zero_fill_samples);
            }
        }
    }

    /// A talkspurt must never start with PLC on a clean link — stale legacy
    /// keepalives must not be counted as buffer depth.
    #[test]
    fn no_plc_right_after_onset_on_clean_link() {
        for pol in [SenderPolicy::Continuous, SenderPolicy::Gated, SenderPolicy::GatedTail] {
            let r = run(pol, Pacing::RingLevel, SCENARIOS[0], 6500);
            assert_eq!(r.expands_after_onset, 0, "{:?}: PLC within 300ms of onset", pol);
        }
    }

    /// Characterisation: the wall-clock-paced receiver does splice zeros into
    /// speech with a gated sender (this is the 0.7.1+ regression).
    #[test]
    fn wall_clock_pacing_with_gated_sender_splices_zeros() {
        let sc = SCENARIOS[0];
        let r = run(SenderPolicy::Gated, Pacing::WallClock, sc, 6500);
        assert!(r.starvations > 0, "expected starvation with wall-clock pacing");
    }
}

#[cfg(test)]
mod wire_sizes {
    use super::super::codec::{OpusEncoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE};
    /// Sizes of encoded frames: speech-like, then encode_silence() right after
    /// speech (legacy keepalive), then repeated silence (DTX). Informs the
    /// unflagged-keepalive heuristic in jitter.rs.
    #[test]
    fn print_legacy_silence_frame_sizes() {
        let mut enc = OpusEncoder::new(64000).unwrap();
        let mut out = [0u8; MAX_OPUS_FRAME_SIZE];
        let mut speech = Vec::new();
        for f in 0..50 {
            let mut frame = [0i16; FRAME_SIZE];
            for (i, s) in frame.iter_mut().enumerate() {
                let t = (f * FRAME_SIZE + i) as f64 / 48000.0;
                let v: f64 = (1..=6).map(|h| (2.0 * std::f64::consts::PI * 180.0 * h as f64 * t).sin() / h as f64).sum();
                *s = (v * 0.08 * 32768.0) as i16;
            }
            speech.push(enc.encode(&frame, &mut out).unwrap());
        }
        let mut quiet = Vec::new();
        for f in 0..10 {
            let mut frame = [0i16; FRAME_SIZE];
            for (i, s) in frame.iter_mut().enumerate() {
                let t = (f * FRAME_SIZE + i) as f64 / 48000.0;
                *s = ((2.0 * std::f64::consts::PI * 180.0 * t).sin() * 0.002 * 32768.0) as i16; // -54 dBFS
            }
            quiet.push(enc.encode(&frame, &mut out).unwrap());
        }
        let silence: Vec<usize> = (0..20).map(|_| enc.encode_silence(&mut out).unwrap()).collect();
        println!("speech frames: {:?}", &speech[40..]);
        println!("quiet (-54 dBFS) frames: {:?}", quiet);
        println!("encode_silence after speech (x20): {:?}", silence);
    }
}
