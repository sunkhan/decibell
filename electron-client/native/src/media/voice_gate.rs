//! Push-to-talk / push-to-mute state, read by the capture loop once per
//! 20 ms frame (`pipeline.rs`). Process-wide on purpose: the hotkey
//! dispatcher writes it from whatever thread saw the key, it outlives
//! voice sessions and DM calls, and a press never round-trips through
//! the renderer. See docs/superpowers/specs/2026-10-10-global-hotkeys-design.md.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::OnceLock;
use std::time::Instant;

/// What the hotkeys say about the mic gate for the current frame.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Gate {
    /// Voice-activity mode, nothing held: the VAD threshold decides.
    Vad,
    /// Push-to-talk held (or inside its release delay): transmit,
    /// threshold ignored.
    Open,
    /// Push-to-talk mode with the key up, or push-to-mute held.
    Closed,
}

pub const DEFAULT_RELEASE_DELAY_MS: u32 = 100;
pub const MAX_RELEASE_DELAY_MS: u32 = 1000;

pub struct GateState {
    ptt_mode: AtomicBool,
    ptt_held: AtomicBool,
    release_delay_ms: AtomicU32,
    /// Clock reading (ms) at the last push-to-talk release, +1 so that
    /// 0 can mean "never released".
    released_at: AtomicU64,
    ptm_held: AtomicBool,
}

impl GateState {
    pub const fn new() -> Self {
        Self {
            ptt_mode: AtomicBool::new(false),
            ptt_held: AtomicBool::new(false),
            release_delay_ms: AtomicU32::new(DEFAULT_RELEASE_DELAY_MS),
            released_at: AtomicU64::new(0),
            ptm_held: AtomicBool::new(false),
        }
    }

    pub fn set_mode(&self, push_to_talk: bool, release_delay_ms: u32) {
        self.release_delay_ms
            .store(release_delay_ms.min(MAX_RELEASE_DELAY_MS), Ordering::Relaxed);
        self.ptt_mode.store(push_to_talk, Ordering::Relaxed);
    }

    pub fn set_ptt_held(&self, held: bool, now_ms: u64) {
        let was = self.ptt_held.swap(held, Ordering::AcqRel);
        if was && !held {
            self.released_at.store(now_ms + 1, Ordering::Release);
        }
    }

    pub fn set_ptm_held(&self, held: bool) {
        self.ptm_held.store(held, Ordering::Relaxed);
    }

    pub fn gate_at(&self, now_ms: u64) -> Gate {
        if self.ptm_held.load(Ordering::Relaxed) {
            return Gate::Closed;
        }
        if !self.ptt_mode.load(Ordering::Relaxed) {
            return Gate::Vad;
        }
        if self.ptt_held.load(Ordering::Acquire) {
            return Gate::Open;
        }
        let released = self.released_at.load(Ordering::Acquire);
        let delay = self.release_delay_ms.load(Ordering::Relaxed) as u64;
        if released != 0 && now_ms + 1 < released + delay {
            Gate::Open
        } else {
            Gate::Closed
        }
    }
}

static STATE: GateState = GateState::new();

fn now_ms() -> u64 {
    static EPOCH: OnceLock<Instant> = OnceLock::new();
    EPOCH.get_or_init(Instant::now).elapsed().as_millis() as u64
}

pub fn set_mode(push_to_talk: bool, release_delay_ms: u32) {
    STATE.set_mode(push_to_talk, release_delay_ms);
}

pub fn set_ptt_held(held: bool) {
    STATE.set_ptt_held(held, now_ms());
}

pub fn set_ptm_held(held: bool) {
    STATE.set_ptm_held(held);
}

pub fn gate() -> Gate {
    STATE.gate_at(now_ms())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn voice_activity_mode_defers_to_vad() {
        let s = GateState::new();
        assert_eq!(s.gate_at(0), Gate::Vad);
        // A push-to-talk press means nothing outside PTT mode.
        s.set_ptt_held(true, 5);
        assert_eq!(s.gate_at(10), Gate::Vad);
    }

    #[test]
    fn push_to_talk_opens_only_while_held_plus_delay() {
        let s = GateState::new();
        s.set_mode(true, 100);
        assert_eq!(s.gate_at(0), Gate::Closed, "never pressed");
        s.set_ptt_held(true, 1000);
        assert_eq!(s.gate_at(1500), Gate::Open);
        s.set_ptt_held(false, 2000);
        assert_eq!(s.gate_at(2000), Gate::Open);
        assert_eq!(s.gate_at(2099), Gate::Open);
        assert_eq!(s.gate_at(2100), Gate::Closed);
    }

    #[test]
    fn zero_delay_closes_on_release() {
        let s = GateState::new();
        s.set_mode(true, 0);
        s.set_ptt_held(true, 0);
        s.set_ptt_held(false, 0);
        assert_eq!(s.gate_at(0), Gate::Closed);
    }

    #[test]
    fn repeated_release_does_not_extend_the_delay() {
        let s = GateState::new();
        s.set_mode(true, 100);
        s.set_ptt_held(true, 0);
        s.set_ptt_held(false, 50);
        s.set_ptt_held(false, 140);
        assert_eq!(s.gate_at(150), Gate::Closed);
    }

    #[test]
    fn push_to_mute_beats_everything() {
        let s = GateState::new();
        s.set_ptm_held(true);
        assert_eq!(s.gate_at(0), Gate::Closed);
        s.set_mode(true, 100);
        s.set_ptt_held(true, 0);
        assert_eq!(s.gate_at(0), Gate::Closed);
        s.set_ptm_held(false);
        assert_eq!(s.gate_at(0), Gate::Open);
    }

    #[test]
    fn delay_is_clamped() {
        let s = GateState::new();
        s.set_mode(true, 60_000);
        s.set_ptt_held(true, 0);
        s.set_ptt_held(false, 0);
        assert_eq!(s.gate_at(MAX_RELEASE_DELAY_MS as u64), Gate::Closed);
    }
}
