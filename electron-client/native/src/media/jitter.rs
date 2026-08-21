use std::collections::HashMap;
use std::time::{Duration, Instant};

// ── Adaptive jitter buffer (NetEQ-light) ─────────────────────────────────────
//
// Holds incoming packets for a short time before decoding so late/out-of-order
// packets can be reordered. The target occupancy adapts to observed network
// jitter: a stable link plays at ~60ms depth, a jittery link grows to ~300ms.
//
// The caller (pipeline.rs) pulls frames with `drain()` whenever its playback
// ring wants data, and reacts to the returned `Frame`:
//   Packet → decode it
//   Lost   → a frame is missing but later ones are buffered: Opus FEC / PLC
//   Expand → the buffer ran dry mid-talkspurt: Opus PLC, bounded, not counted
//            as loss (the sender is probably late, not paused)
//   Idle   → nothing to play (cold, paused sender, or expansion exhausted)
//
// Senders mark non-voice frames (gate-closed tail, keepalives) with a silence
// flag (see FLAG_SILENCE in pipeline.rs). Keepalives arriving while idle are
// consumed without buffering, and flagged frames never feed the jitter
// estimate, so a silence-gated sender's 500ms keepalive cadence can neither
// inflate the target nor delay the next talkspurt's onset.
//
// Latency catch-up is content-aware and lives in the caller: `excess()` says
// how many frames over target we hold, and the caller drops quiet frames
// (after decoding them, so the decoder state stays continuous) rather than
// splicing speech — except past a hard ceiling where a splice beats the lag.

pub const FRAME_DUR_SEC: f64 = 0.020; // 20ms per Opus frame at 48kHz

/// Absolute floor on occupancy before playback starts / resumes.
pub const JITTER_MIN_DEPTH: usize = 2;  // 40ms — a single late packet can still land on time
/// Soft ceiling on adaptive target depth.
pub const JITTER_MAX_DEPTH: usize = 15; // 300ms — above this, audio feels laggy regardless
/// Hard safety cap. If the buffer grows past this we force-drain.
pub const JITTER_MAX: usize = 30;

/// Extra slack above target_depth before the caller starts trimming latency.
const SHRINK_HYSTERESIS: usize = 3;
/// Past this many frames over target the caller may splice speech to catch up.
pub const SHRINK_HARD_EXCESS: usize = 10;

/// Consecutive PLC (Lost) frames that trigger a re-sync reset.
const PLC_RESET_THRESHOLD: u32 = 10;

/// Max consecutive `Expand` frames per underrun before we give up and go
/// idle. Opus PLC fades to silence over roughly this span anyway.
pub const MAX_EXPAND_FRAMES: u32 = 5; // 100ms

/// Inter-arrival gaps at or above this are never network jitter: a paused
/// sender (legacy keepalives are 500ms apart) or a stall too long to buffer
/// against. Excluded from the estimate, and the measurement right after a
/// gap is skipped too (it spans the pause).
const GAP_IGNORE_SEC: f64 = 0.4;

/// A run of this many packets arriving BEHIND the play cursor means the sender
/// restarted its sequence (left + rejoined → fresh VoiceEngine, seq from 0), so
/// we resync. A lone behind-cursor packet is just reordering/duplication and is
/// dropped — resetting on it would needlessly flush good buffered frames.
const REJOIN_RESYNC_STREAK: u32 = 3;

/// A late packet resolving an underrun within this many frames of the play
/// cursor counts as "the buffer was too shallow" → bump the target.
const LATE_RESOLVE_WINDOW: u16 = 8;
/// Underrun-earned depth decays one frame per this interval of calm.
const UNDERRUN_BOOST_DECAY: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq)]
pub enum Frame {
    Packet(Vec<u8>),
    Lost,
    Expand,
    Idle,
}

pub struct JitterBuffer {
    /// seq → (payload, silence-flagged)
    packets: HashMap<u16, (Vec<u8>, bool)>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
    consecutive_losses: u32,
    /// Consecutive packets seen behind the play cursor — see REJOIN_RESYNC_STREAK.
    behind_streak: u32,
    /// Whether the last frame handed out (or last keepalive consumed) was
    /// silence-flagged. Decides pause (Idle) vs. late (Expand) on underrun.
    last_dequeued_silence: bool,
    /// Expand frames emitted in the current underrun; 0 = not underrunning.
    expanding: u32,
    /// The current underrun has already been credited as "late packet".
    late_noted: bool,

    /// Arrival time of the most recent packet of any kind (flagged, gapped,
    /// behind — everything). `last_arrival` below is only the jitter
    /// estimator's reference and is cleared across gaps/flags.
    last_any_arrival: Option<Instant>,
    /// Real frames handed out since the last resume-from-underrun; a second
    /// underrun shortly after a resume means one extra frame of target wasn't
    /// enough — rebuild fully (target + prime) instead of thrashing between
    /// PLC and one-frame resumes.
    frames_since_resume: u32,

    // ── Adaptive depth state ──
    last_arrival: Option<Instant>,
    /// RFC-3550-style jitter estimate in seconds.
    jitter_sec: f64,
    /// Depth earned by recent underruns, on top of the jitter-derived target.
    underrun_boost: usize,
    last_boost_decay: Option<Instant>,
    /// Current adaptive occupancy target (jitter-derived + boost, clamped).
    target_depth: usize,
    /// Extra frames required before a cold start so the caller can prime its
    /// playback ring without emptying us. Set by the caller from the output
    /// device's pull size.
    prime_frames: usize,
    /// Frames over target past which `should_drop_excess` splices even loud
    /// audio. Voice keeps the generous default (a pause always comes and
    /// clears the backlog for free); stream audio (music/game sound, rarely
    /// quiet) gets a tight one so it can't drift hundreds of ms behind video.
    hard_excess: usize,

    // ── Stats (read externally for diagnostics) ──
    pub plc_frames: u64,
    pub expand_frames: u64,
    pub dropped_frames: u64,
    /// Frames dequeued from a real packet (not PLC). Combined with
    /// plc_frames over a window this gives a true packet-loss rate
    /// for the user-panel telemetry popover.
    pub decoded_frames: u64,
    pub underruns: u64,
}

impl JitterBuffer {
    pub fn new() -> Self {
        Self {
            packets: HashMap::new(),
            next_seq: 0,
            initialized: false,
            ready: false,
            consecutive_losses: 0,
            behind_streak: 0,
            last_dequeued_silence: false,
            expanding: 0,
            late_noted: false,
            last_any_arrival: None,
            frames_since_resume: u32::MAX,
            last_arrival: None,
            jitter_sec: 0.0,
            underrun_boost: 0,
            last_boost_decay: None,
            target_depth: JITTER_MIN_DEPTH,
            prime_frames: 0,
            hard_excess: SHRINK_HARD_EXCESS,
            plc_frames: 0,
            expand_frames: 0,
            dropped_frames: 0,
            decoded_frames: 0,
            underruns: 0,
        }
    }

    /// Current target occupancy in packets — for diagnostics.
    pub fn target(&self) -> usize { self.target_depth }
    /// Estimated one-way jitter in milliseconds.
    pub fn jitter_ms(&self) -> f64 { self.jitter_sec * 1000.0 }
    /// Buffered packets (diagnostics).
    pub fn len(&self) -> usize { self.packets.len() }
    pub fn is_ready(&self) -> bool { self.ready }
    /// Concealing an underrun (PLC) while waiting for a late packet.
    pub fn is_expanding(&self) -> bool { self.expanding > 0 }
    /// 1-based index of the Expand frame just returned (0 when not expanding).
    /// Callers fade PLC output across 1..=MAX_EXPAND_FRAMES so an unflagged
    /// sender that simply stops (legacy gate close) tails out to silence
    /// instead of ending on a 100ms pitch-repeat.
    pub fn expand_index(&self) -> u32 { self.expanding }

    /// Frames the caller should be able to decode immediately on a cold start
    /// (to prime its playback ring) before we'd dip below target.
    pub fn set_prime_frames(&mut self, frames: usize) {
        self.prime_frames = frames.min(JITTER_MAX_DEPTH);
    }

    /// Frames over target at which even loud frames get spliced to catch up.
    pub fn set_hard_excess(&mut self, frames: usize) {
        self.hard_excess = frames.max(1);
    }

    /// How many frames over (target + hysteresis) we currently hold — the
    /// caller trims that many by dropping quiet frames.
    pub fn excess(&self) -> usize {
        self.packets.len().saturating_sub(self.target_depth + SHRINK_HYSTERESIS)
    }

    fn recompute_target(&mut self) {
        // Target: roughly 2× observed jitter, floored at MIN, ceilinged at MAX.
        let from_jitter = (self.jitter_sec * 2.0 / FRAME_DUR_SEC).ceil() as usize + JITTER_MIN_DEPTH;
        self.target_depth = (from_jitter + self.underrun_boost).clamp(JITTER_MIN_DEPTH, JITTER_MAX_DEPTH);
    }

    /// Update RFC-3550 jitter estimate and recompute target depth.
    /// J = J + (|D(i-1,i)| - J) / 16
    /// `gap_before`: this packet itself followed a ≥GAP_IGNORE gap. It can be
    /// measured against nothing and must not become the reference either —
    /// otherwise a lone legacy keepalive anchors the estimate and the next
    /// talkspurt's first packet reads as hundreds of ms of jitter.
    fn on_arrival(&mut self, now: Instant, silence: bool, gap_before: bool) {
        // Decay underrun-earned depth slowly while the link is calm.
        match self.last_boost_decay {
            None => self.last_boost_decay = Some(now),
            Some(t) if self.underrun_boost > 0 && now.duration_since(t) >= UNDERRUN_BOOST_DECAY => {
                self.underrun_boost -= 1;
                self.last_boost_decay = Some(now);
                self.recompute_target();
            }
            _ => {}
        }
        let mut measurable = !silence && !gap_before;
        if let Some(prev) = self.last_arrival {
            let iat = now.duration_since(prev).as_secs_f64();
            if iat >= GAP_IGNORE_SEC {
                // Spans a pause/stall — not jitter. Skip this and the next.
                measurable = false;
            } else if !silence {
                let d = (iat - FRAME_DUR_SEC).abs();
                self.jitter_sec += (d - self.jitter_sec) / 16.0;
                self.recompute_target();
            }
        }
        // A flagged frame (tail / keepalive) or a gap ends the measurable run;
        // the next voice frame has no valid predecessor.
        self.last_arrival = if measurable { Some(now) } else { None };
    }

    /// An underrun turned out to be a late packet, not a pause: hold one more
    /// frame from now on.
    fn note_late_resolve(&mut self) {
        if self.underrun_boost < JITTER_MAX_DEPTH {
            self.underrun_boost += 1;
            self.recompute_target();
        }
        self.last_boost_decay = self.last_arrival;
    }

    /// Insert a packet. `silence` = the sender flagged it as non-voice (gate
    /// tail / keepalive). Ignores packets behind the play cursor.
    pub fn push(&mut self, seq: u16, data: Vec<u8>, silence: bool) {
        self.push_at(seq, data, Instant::now(), silence);
    }

    /// `push` with an explicit arrival time (tests / simulation).
    pub fn push_at(&mut self, seq: u16, data: Vec<u8>, now: Instant, silence: bool) {
        let gap_before = self.last_any_arrival
            .map(|t| now.duration_since(t).as_secs_f64() >= GAP_IGNORE_SEC)
            .unwrap_or(false);
        self.last_any_arrival = Some(now);
        self.on_arrival(now, silence, gap_before);

        if !self.initialized {
            self.next_seq = seq;
            self.initialized = true;
        }
        // A packet behind the play cursor (wrapping) is normally just late,
        // reordered, or a duplicate — drop it. Resetting the whole buffer on one
        // such packet (the old behaviour) flushed good buffered frames and
        // forced a re-buffer on any network that reorders/duplicates, an audible
        // dropout. Only a *run* of behind packets means the sender genuinely
        // restarted its sequence (rejoin); then we resync to the new stream.
        let behind = seq.wrapping_sub(self.next_seq) >= 32768;
        if behind {
            self.behind_streak = self.behind_streak.saturating_add(1);
            if self.behind_streak >= REJOIN_RESYNC_STREAK {
                self.packets.clear();
                self.next_seq = seq;
                self.ready = false;
                self.expanding = 0;
                self.late_noted = false;
                self.behind_streak = 0;
                self.packets.insert(seq, (data, silence));
            }
            // else: a lone late/duplicate packet — discard it.
            return;
        }
        self.behind_streak = 0;

        // Unflagged sender (pre-0.7.4) and this packet follows a gap while we
        // weren't playing: whatever we were holding is stale — keepalives
        // (encoded silence every 500ms, same size as quiet speech, so timing
        // is the only tell) or the remnant of an utterance too short to reach
        // the fill threshold. Counting stale keepalives toward the fill made
        // the next talkspurt start with 60–80ms of silence and only one or
        // two frames of real audio in hand → underrun and PLC 80ms into every
        // utterance. Drop them; this packet starts a fresh fill.
        if !silence && gap_before && !self.ready && self.expanding == 0 && !self.packets.is_empty() {
            self.dropped_frames += self.packets.len() as u64;
            self.packets.clear();
            self.next_seq = seq;
        }

        // A flagged frame arriving while we're idle and empty is a keepalive:
        // the sender is paused. Consume it in place — buffering it would only
        // add a silent frame of latency in front of the next talkspurt.
        if silence && !self.ready && self.packets.is_empty() && self.expanding == 0 {
            self.next_seq = seq.wrapping_add(1);
            self.last_dequeued_silence = true;
            return;
        }

        // A packet landing at/near the cursor while we're expanding means the
        // underrun was lateness, not a pause → deepen the buffer (once per
        // underrun, not once per packet of the burst that resolves it).
        if self.expanding > 0 && !self.late_noted && seq.wrapping_sub(self.next_seq) < LATE_RESOLVE_WINDOW {
            self.late_noted = true;
            self.note_late_resolve();
        }

        self.packets.insert(seq, (data, silence));

        // Cold start needs target + prime (the caller will immediately pull
        // `prime` frames into its ring); resuming after an expansion only
        // needs target — the ring is still primed with PLC audio — unless
        // the previous resume underran again almost immediately (bursty
        // link): then rebuild fully rather than thrash.
        const THRASH_FRAMES: u32 = 10;
        let quick_re_underrun = self.frames_since_resume < THRASH_FRAMES;
        let need = if self.expanding > 0 && !quick_re_underrun {
            self.target_depth
        } else {
            self.target_depth + self.prime_frames
        };
        if !self.ready && self.packets.len() >= need {
            self.ready = true;
        }
        // Force-drain excess so the buffer can't grow unbounded.
        while self.packets.len() > JITTER_MAX {
            if !self.packets.contains_key(&self.next_seq) {
                if let Some(&earliest) = self.packets.keys()
                    .min_by_key(|&&s| s.wrapping_sub(self.next_seq))
                {
                    self.next_seq = earliest;
                } else {
                    break;
                }
            }
            self.packets.remove(&self.next_seq);
            self.next_seq = self.next_seq.wrapping_add(1);
            self.dropped_frames += 1;
        }
    }

    /// Pop the next frame. See the module docs for the `Frame` contract.
    pub fn drain(&mut self) -> Frame {
        if !self.ready {
            if self.expanding > 0 {
                if self.expanding < MAX_EXPAND_FRAMES {
                    self.expanding += 1;
                    self.expand_frames += 1;
                    return Frame::Expand;
                }
                // Expansion exhausted: treat as a pause from here on.
                self.expanding = 0;
                self.late_noted = false;
                self.last_dequeued_silence = true;
                self.frames_since_resume = u32::MAX;
            }
            return Frame::Idle;
        }

        // Auto-recovery: if we've produced N consecutive PLC frames the audio
        // is already unintelligible. Reset and re-buffer from scratch.
        if self.consecutive_losses >= PLC_RESET_THRESHOLD {
            self.reset();
            return Frame::Idle;
        }

        let seq = self.next_seq;
        match self.packets.remove(&seq) {
            Some((data, silence)) => {
                self.next_seq = self.next_seq.wrapping_add(1);
                self.consecutive_losses = 0;
                if self.expanding > 0 {
                    self.frames_since_resume = 0; // resumed from an underrun
                } else {
                    self.frames_since_resume = self.frames_since_resume.saturating_add(1);
                }
                self.expanding = 0;
                self.late_noted = false;
                self.decoded_frames += 1;
                self.last_dequeued_silence = silence;
                Frame::Packet(data)
            }
            None => {
                if self.packets.is_empty() {
                    // Nothing buffered ahead either. next_seq stays put so the
                    // sender's next contiguous packet lands cleanly.
                    self.ready = false;
                    if self.last_dequeued_silence {
                        // The sender told us it went quiet (tail/keepalive):
                        // a genuine pause, nothing to conceal.
                        self.frames_since_resume = u32::MAX;
                        Frame::Idle
                    } else {
                        // Mid-talkspurt underrun: bridge with PLC while we
                        // wait — the packet is probably late, not absent.
                        self.underruns += 1;
                        self.expanding = 1;
                        self.expand_frames += 1;
                        Frame::Expand
                    }
                } else {
                    // Real mid-stream loss — later frames ARE buffered, so PLC
                    // to bridge the gap up to them.
                    self.next_seq = self.next_seq.wrapping_add(1);
                    self.consecutive_losses += 1;
                    self.plc_frames += 1;
                    Frame::Lost
                }
            }
        }
    }

    /// Caller-side latency trim: after decoding a frame, ask whether to
    /// discard it instead of playing it. Quiet frames go first; past
    /// SHRINK_HARD_EXCESS even speech is spliced. Counts the drop.
    pub fn should_drop_excess(&mut self, frame_is_quiet: bool) -> bool {
        let excess = self.excess();
        if excess == 0 { return false; }
        if frame_is_quiet || excess > self.hard_excess {
            self.dropped_frames += 1;
            true
        } else {
            false
        }
    }

    /// Reset the buffer to its initial state, forcing a re-buffering period.
    pub fn reset(&mut self) {
        self.packets.clear();
        self.initialized = false;
        self.ready = false;
        self.consecutive_losses = 0;
        self.expanding = 0;
        self.late_noted = false;
        self.last_dequeued_silence = false;
        // Keep jitter estimate — network conditions don't change on reset.
    }

    /// Peek at the next packet (next_seq) without consuming it.
    /// Used for FEC: when current packet is missing, check if the next
    /// packet is available to decode with fec=true.
    pub fn peek_next(&self) -> Option<&Vec<u8>> {
        self.packets.get(&self.next_seq).map(|(d, _)| d)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn t(ms: u64) -> Instant {
        // A fixed origin far enough in the past that subtraction never underflows.
        thread_local! { static T0: Instant = Instant::now() - Duration::from_secs(3600); }
        T0.with(|t0| *t0 + Duration::from_millis(ms))
    }

    fn fill(jb: &mut JitterBuffer, from: u16, n: u16, start_ms: u64) {
        for i in 0..n {
            jb.push_at(from + i, vec![i as u8], t(start_ms + i as u64 * 20), false);
        }
    }

    #[test]
    fn cold_start_waits_for_target_plus_prime() {
        let mut jb = JitterBuffer::new();
        jb.set_prime_frames(2);
        fill(&mut jb, 0, 3, 0); // target is 2 → need 4
        assert_eq!(jb.drain(), Frame::Idle);
        jb.push_at(3, vec![3], t(60), false);
        assert!(matches!(jb.drain(), Frame::Packet(_)));
    }

    #[test]
    fn underrun_mid_talkspurt_expands_then_idles() {
        let mut jb = JitterBuffer::new();
        fill(&mut jb, 0, 2, 0);
        assert!(matches!(jb.drain(), Frame::Packet(_)));
        assert!(matches!(jb.drain(), Frame::Packet(_)));
        // Empty, last frame was voice → Expand up to the cap, then Idle.
        for _ in 0..MAX_EXPAND_FRAMES {
            assert_eq!(jb.drain(), Frame::Expand);
        }
        assert_eq!(jb.drain(), Frame::Idle);
        assert_eq!(jb.drain(), Frame::Idle);
        assert_eq!(jb.underruns, 1);
        assert_eq!(jb.plc_frames, 0, "expansion is not packet loss");
    }

    #[test]
    fn underrun_after_silence_flag_is_a_pause() {
        let mut jb = JitterBuffer::new();
        jb.push_at(0, vec![0], t(0), false);
        jb.push_at(1, vec![1], t(20), true); // gate tail
        assert!(matches!(jb.drain(), Frame::Packet(_)));
        assert!(matches!(jb.drain(), Frame::Packet(_)));
        assert_eq!(jb.drain(), Frame::Idle, "sender said it paused — no PLC");
        assert_eq!(jb.underruns, 0);
    }

    #[test]
    fn keepalives_while_idle_are_consumed_not_buffered() {
        let mut jb = JitterBuffer::new();
        jb.push_at(0, vec![0], t(0), false);
        jb.push_at(1, vec![1], t(20), true);
        jb.drain(); jb.drain(); jb.drain(); // play 2, go idle
        jb.push_at(2, vec![2], t(520), true);  // keepalive
        jb.push_at(3, vec![3], t(1020), true); // keepalive
        assert_eq!(jb.len(), 0);
        assert_eq!(jb.drain(), Frame::Idle);
        // Next talkspurt starts at seq 4 and lands contiguous — no behind, no
        // stale silence ahead of it.
        jb.push_at(4, vec![4], t(1200), false);
        jb.push_at(5, vec![5], t(1220), false);
        assert_eq!(jb.drain(), Frame::Packet(vec![4]));
    }

    #[test]
    fn keepalive_gaps_do_not_inflate_target() {
        let mut jb = JitterBuffer::new();
        fill(&mut jb, 0, 10, 0);
        let base = jb.target();
        for i in 0..10 { jb.drain(); let _ = i; }
        for k in 0..20u64 {
            jb.push_at(10 + k as u16, vec![], t(200 + 500 * (k + 1)), true);
        }
        assert_eq!(jb.target(), base);
        // Legacy (unflagged) 500ms keepalives are excluded by the gap rule too.
        let mut legacy = JitterBuffer::new();
        fill(&mut legacy, 0, 10, 0);
        let base = legacy.target();
        for _ in 0..10 { legacy.drain(); }
        for k in 0..20u64 {
            legacy.push_at(10 + k as u16, vec![], t(200 + 500 * (k + 1)), false);
        }
        assert_eq!(legacy.target(), base);
    }

    #[test]
    fn late_packet_resolving_underrun_deepens_target() {
        let mut jb = JitterBuffer::new();
        fill(&mut jb, 0, 2, 0);
        jb.drain(); jb.drain();
        let before = jb.target();
        assert_eq!(jb.drain(), Frame::Expand);
        jb.push_at(2, vec![2], t(70), false); // late by ~30ms
        let bumped = jb.target();
        assert!(bumped > before, "late resolve should bump target");
        jb.push_at(3, vec![3], t(80), false);
        assert_eq!(jb.target(), bumped, "one bump per underrun, not per packet");
        // Resume needs only `target` (ring is primed), not target + prime.
        jb.set_prime_frames(5);
        let target = jb.target();
        let mut seq = 4u16;
        while jb.len() < target {
            jb.push_at(seq, vec![], t(80 + (seq as u64 - 3) * 20), false);
            seq += 1;
        }
        assert!(jb.is_ready(), "should resume at target ({}) without the prime", target);
    }

    /// Legacy (unflagged) keepalives every 500ms must not count toward the
    /// fill: the next talkspurt should start with real audio depth.
    #[test]
    fn legacy_keepalives_are_dropped_as_stale() {
        let mut jb = JitterBuffer::new();
        jb.set_prime_frames(3);
        fill(&mut jb, 0, 10, 0);
        for _ in 0..10 { jb.drain(); }
        while jb.drain() != Frame::Idle {}
        // Four unflagged keepalives, 500ms apart.
        for k in 0..4u64 {
            jb.push_at(10 + k as u16, vec![], t(700 + 500 * k), false);
            assert!(jb.len() <= 1, "at most the latest stale packet is held");
        }
        // Talkspurt resumes 300ms after the last keepalive: that one lone
        // keepalive may remain, but ready needs target+prime *fresh* frames.
        let need = jb.target() + 3;
        for i in 0..need as u16 {
            jb.push_at(14 + i, vec![i as u8], t(2500 + 20 * i as u64), false);
        }
        assert!(jb.is_ready(), "target {} len {} need {}", jb.target(), jb.len(), need);
        assert!(jb.len() >= need, "fill is real audio, not stale keepalives (len {})", jb.len());
    }

    #[test]
    fn quick_re_underrun_requires_full_rebuild() {
        let mut jb = JitterBuffer::new();
        jb.set_prime_frames(3);
        fill(&mut jb, 0, 5, 0);          // target 2 + prime 3 → ready
        for _ in 0..5 { jb.drain(); }
        assert_eq!(jb.drain(), Frame::Expand);
        // Late packets land; the first bumps the target. Resume needs `target`.
        let mut seq = 5u16;
        let mut ms = 120u64;
        while !jb.is_ready() {
            jb.push_at(seq, vec![], t(ms), false);
            assert!(jb.len() <= jb.target(), "first resume must not need prime");
            seq += 1; ms += 20;
        }
        let got = jb.len();
        for _ in 0..got { assert!(matches!(jb.drain(), Frame::Packet(_))); }
        assert_eq!(jb.drain(), Frame::Expand, "underran again right after resume");
        let tgt2 = jb.target();
        for i in 0..tgt2 as u16 { jb.push_at(seq + i, vec![], t(ms + 20 * i as u64), false); }
        assert!(!jb.is_ready(), "thrash: second resume must rebuild target+prime (len {} tgt {})", jb.len(), jb.target());
    }

    #[test]
    fn lost_with_frames_ahead_is_plc_and_counted() {
        let mut jb = JitterBuffer::new();
        jb.push_at(0, vec![0], t(0), false);
        jb.push_at(2, vec![2], t(40), false); // seq 1 never arrives
        jb.push_at(3, vec![3], t(60), false);
        assert!(jb.is_ready(), "target {} len {}", jb.target(), jb.len());
        assert_eq!(jb.drain(), Frame::Packet(vec![0]));
        assert_eq!(jb.drain(), Frame::Lost);
        assert_eq!(jb.plc_frames, 1);
        assert_eq!(jb.drain(), Frame::Packet(vec![2]));
    }

    #[test]
    fn excess_trim_prefers_quiet_frames() {
        let mut jb = JitterBuffer::new();
        fill(&mut jb, 0, 12, 0); // target 2 + hysteresis 3 → excess 7
        assert!(jb.excess() > 0);
        assert!(!jb.should_drop_excess(false), "speech is kept at moderate excess");
        assert!(jb.should_drop_excess(true));
        fill(&mut jb, 12, 10, 240); // push well past the hard ceiling
        assert!(jb.excess() > SHRINK_HARD_EXCESS);
        assert!(jb.should_drop_excess(false), "past the ceiling even speech is spliced");
    }

    #[test]
    fn lone_behind_packet_is_dropped_but_a_run_resyncs() {
        let mut jb = JitterBuffer::new();
        fill(&mut jb, 100, 3, 0);
        jb.push_at(99, vec![], t(60), false); // one stray late packet
        assert_eq!(jb.len(), 3);
        for s in 0..3u16 { jb.push_at(s, vec![], t(80 + s as u64 * 20), false); }
        // Third behind packet (seq 1) triggered the resync; seq 2 followed it.
        assert_eq!(jb.len(), 2, "rejoin streak flushed the old stream");
        assert_eq!(jb.peek_next().map(|v| v.len()), Some(0), "cursor now at the resync seq");
    }
}
