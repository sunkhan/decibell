//! Sums several independent stereo capture sources into one stream.
//!
//! The Windows share-audio path runs one WASAPI process-loopback client per
//! allowed application (the API covers exactly one process tree per
//! client). Each client delivers interleaved stereo f32 chunks at the same
//! sample rate on its own thread; this mixer owns one ring per source and
//! emits one summed stream paced by the wall clock.
//!
//! Pacing: every tick the caller asks for the frames the clock says are due
//! (`frames_due`) and takes them with `mix`. A source contributes once it has
//! `prime_frames` buffered (absorbs delivery jitter); a source that runs dry
//! contributes zeros and un-primes so it re-buffers before rejoining; a
//! source that runs ahead is trimmed at `max_ring_frames` (drops oldest).
//! Wall-clock vs audio-clock drift therefore surfaces as a rare trim, not a
//! growing delay. Pure logic — cfg-free, unit-tested on every platform.

use std::collections::{BTreeMap, VecDeque};
use std::time::Duration;

#[derive(Clone, Copy, Debug)]
pub struct MixerConfig {
    pub sample_rate: u32,
    /// Per-source backlog cap in frames (a frame = one L+R pair).
    pub max_ring_frames: usize,
    /// Frames a source must hold before it starts contributing.
    pub prime_frames: usize,
}

impl MixerConfig {
    /// 200 ms cap, 20 ms prime — sized for WASAPI's 10 ms delivery.
    pub fn for_rate(sample_rate: u32) -> Self {
        let per_ms = sample_rate as usize / 1000;
        Self {
            sample_rate,
            max_ring_frames: per_ms * 200,
            prime_frames: per_ms * 20,
        }
    }
}

struct SourceRing {
    /// Interleaved stereo samples.
    buf: VecDeque<f32>,
    primed: bool,
}

pub struct Mixer {
    cfg: MixerConfig,
    sources: BTreeMap<u32, SourceRing>,
    frames_emitted: u64,
    trims: u64,
}

impl Mixer {
    pub fn new(cfg: MixerConfig) -> Self {
        Self { cfg, sources: BTreeMap::new(), frames_emitted: 0, trims: 0 }
    }

    pub fn sample_rate(&self) -> u32 {
        self.cfg.sample_rate
    }

    /// Idempotent.
    pub fn add_source(&mut self, key: u32) {
        self.sources.entry(key).or_insert_with(|| SourceRing {
            buf: VecDeque::with_capacity(self.cfg.max_ring_frames * 2),
            primed: false,
        });
    }

    pub fn remove_source(&mut self, key: u32) {
        self.sources.remove(&key);
    }

    pub fn has_source(&self, key: u32) -> bool {
        self.sources.contains_key(&key)
    }

    pub fn source_keys(&self) -> impl Iterator<Item = u32> + '_ {
        self.sources.keys().copied()
    }

    pub fn source_count(&self) -> usize {
        self.sources.len()
    }

    /// Times a source's backlog was trimmed (telemetry).
    pub fn trims(&self) -> u64 {
        self.trims
    }

    /// Append interleaved stereo samples for `key`. Unknown keys are ignored
    /// (a client that was removed while its thread still had a chunk).
    pub fn push(&mut self, key: u32, stereo: &[f32]) {
        let Some(ring) = self.sources.get_mut(&key) else { return };
        ring.buf.extend(stereo.iter().copied().take(stereo.len() & !1));
        let cap = self.cfg.max_ring_frames * 2;
        if ring.buf.len() > cap {
            let excess = ring.buf.len() - cap;
            ring.buf.drain(..excess);
            self.trims += 1;
        }
        if !ring.primed && ring.buf.len() >= self.cfg.prime_frames * 2 {
            ring.primed = true;
        }
    }

    /// Frames the wall clock says should have been emitted by now but have
    /// not been. A backlog larger than the ring cap (the thread was
    /// descheduled) is forgiven rather than burst out: the clock is moved
    /// forward and only the cap is returned.
    pub fn frames_due(&mut self, elapsed: Duration) -> usize {
        // Integer math: f64 would truncate 2.020 s × 1000 Hz to 2019 frames.
        let target = (elapsed.as_nanos() * self.cfg.sample_rate as u128 / 1_000_000_000) as u64;
        let due = target.saturating_sub(self.frames_emitted);
        let cap = self.cfg.max_ring_frames as u64;
        if due > cap {
            self.frames_emitted += due - cap;
            cap as usize
        } else {
            due as usize
        }
    }

    /// Take `frames` frames from every primed source, sum them (clamped to
    /// ±1.0) and advance the clock. `None` when no primed source had any
    /// data — the caller sends nothing, so an idle desktop costs no
    /// bandwidth. A primed source that is short contributes what it has plus
    /// zeros; one that was completely empty un-primes.
    pub fn mix(&mut self, frames: usize) -> Option<Vec<f32>> {
        self.frames_emitted += frames as u64;
        if frames == 0 {
            return None;
        }
        let want = frames * 2;
        let mut out = vec![0.0f32; want];
        let mut contributed = false;
        for ring in self.sources.values_mut() {
            if !ring.primed {
                continue;
            }
            if ring.buf.is_empty() {
                ring.primed = false;
                continue;
            }
            contributed = true;
            let n = want.min(ring.buf.len());
            for (o, s) in out.iter_mut().zip(ring.buf.drain(..n)) {
                *o += s;
            }
        }
        if !contributed {
            return None;
        }
        for s in &mut out {
            *s = s.clamp(-1.0, 1.0);
        }
        Some(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixer() -> Mixer {
        // 1 kHz makes the numbers small: prime 20 frames, cap 200 frames.
        Mixer::new(MixerConfig::for_rate(1000))
    }

    fn tone(frames: usize, v: f32) -> Vec<f32> {
        vec![v; frames * 2]
    }

    #[test]
    fn two_sources_sum() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(2);
        m.push(1, &tone(30, 0.25));
        m.push(2, &tone(30, 0.25));
        let out = m.mix(10).unwrap();
        assert_eq!(out.len(), 20);
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn stalled_source_leaves_the_other_intact() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(2);
        m.push(1, &tone(30, 0.3));
        // source 2 never delivers
        let out = m.mix(10).unwrap();
        assert!(out.iter().all(|&s| (s - 0.3).abs() < 1e-6));
    }

    #[test]
    fn ring_cap_drops_oldest_and_counts_trims() {
        let mut m = mixer();
        m.add_source(1);
        m.push(1, &tone(150, 0.1));
        m.push(1, &tone(150, 0.9)); // 300 frames > 200 cap → oldest 100 dropped
        assert_eq!(m.trims(), 1);
        let out = m.mix(200).unwrap();
        assert_eq!(out.len(), 400);
        // First 50 frames left of the 0.1 tone, then 150 frames of 0.9.
        assert!((out[0] - 0.1).abs() < 1e-6);
        assert!((out[99] - 0.1).abs() < 1e-6);
        assert!((out[100] - 0.9).abs() < 1e-6);
        assert!((out[399] - 0.9).abs() < 1e-6);
    }

    #[test]
    fn sum_is_clamped() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(2);
        m.push(1, &tone(30, 0.8));
        m.push(2, &tone(30, -0.9).iter().map(|s| -s).collect::<Vec<_>>()); // +0.9
        let out = m.mix(10).unwrap();
        assert!(out.iter().all(|&s| s == 1.0));
        let mut m = mixer();
        m.add_source(1);
        m.push(1, &tone(30, -1.5));
        assert!(m.mix(10).unwrap().iter().all(|&s| s == -1.0));
    }

    #[test]
    fn frames_due_tracks_elapsed_and_forgives_bursts() {
        let mut m = mixer();
        assert_eq!(m.frames_due(Duration::from_millis(10)), 10);
        m.mix(10);
        assert_eq!(m.frames_due(Duration::from_millis(20)), 10);
        m.mix(10);
        // 2 s asleep → only the 200-frame cap is due, the rest is forgiven.
        assert_eq!(m.frames_due(Duration::from_millis(2020)), 200);
        m.mix(200);
        assert_eq!(m.frames_due(Duration::from_millis(2030)), 10);
    }

    #[test]
    fn unprimed_source_is_silent_until_primed() {
        let mut m = mixer();
        m.add_source(1);
        m.push(1, &tone(5, 0.5)); // < 20-frame prime
        assert!(m.mix(5).is_none());
        m.push(1, &tone(20, 0.5)); // 25 buffered ≥ prime
        let out = m.mix(10).unwrap();
        assert!(out.iter().all(|&s| (s - 0.5).abs() < 1e-6));
    }

    #[test]
    fn short_source_zero_pads_but_empty_source_unprimes() {
        let mut m = mixer();
        m.add_source(1);
        m.push(1, &tone(25, 0.5));
        let out = m.mix(30).unwrap(); // 25 real + 5 zero frames
        assert!((out[49] - 0.5).abs() < 1e-6);
        assert_eq!(out[50], 0.0);
        assert_eq!(out[59], 0.0);
        // Ring is now empty but still primed: the next mix un-primes it.
        assert!(m.mix(10).is_none());
        m.push(1, &tone(5, 0.5));
        assert!(m.mix(5).is_none(), "must re-prime after a stall");
        m.push(1, &tone(20, 0.5));
        assert!(m.mix(5).is_some());
    }

    #[test]
    fn remove_and_unknown_keys() {
        let mut m = mixer();
        m.add_source(1);
        m.add_source(1);
        assert_eq!(m.source_count(), 1);
        m.push(7, &tone(30, 0.5)); // unknown key ignored
        assert!(m.mix(10).is_none());
        m.push(1, &tone(30, 0.5));
        m.remove_source(1);
        assert!(!m.has_source(1));
        assert!(m.mix(10).is_none());
        assert_eq!(m.source_keys().count(), 0);
    }

    #[test]
    fn odd_sample_count_is_truncated_to_pairs() {
        let mut m = mixer();
        m.add_source(1);
        m.push(1, &[0.5; 41]);
        let out = m.mix(20).unwrap();
        assert_eq!(out.len(), 40);
    }
}
