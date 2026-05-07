//! Codec selection — the LCD (lowest common denominator) picker plus the
//! CodecSelector that owns timer state and emits swap events.
//!
//! Spec §5.1: given the streamer's encode caps + every current watcher's
//! decode caps + the user's enabled-codec settings, pick the highest-priority
//! codec that everyone supports, with resolution/fps clamped to the smallest
//! of (streamer original, streamer encode ceiling, every watcher's decode
//! ceiling for that codec).
//!
//! Spec §5.3 / §5.4: downgrades fire after a 200ms debounce (coalesces
//! simultaneous joins). Upgrades fire after a 30s cooldown (prevents
//! thrashing when watchers churn). Cooldown is monotonic — does not
//! reset if target rises further during the wait.

use crate::media::caps::{CodecCap, CodecKind};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::mpsc;
use tokio::task::JoinHandle;

const DOWNGRADE_DEBOUNCE: Duration = Duration::from_millis(200);
const UPGRADE_COOLDOWN: Duration = Duration::from_secs(30);

/// Encoder priority order — earlier = higher quality / preferred.
const PRIORITY: [CodecKind; 4] = [
    CodecKind::Av1,
    CodecKind::H265,
    CodecKind::H264Hw,
    CodecKind::H264Sw,
];

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct StreamSettings {
    pub codec: CodecKind,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
}

#[derive(Clone, Debug)]
pub struct Toggles {
    pub use_av1: bool,
    pub use_h265: bool,
}

impl Toggles {
    fn allows(&self, codec: CodecKind) -> bool {
        match codec {
            CodecKind::Av1 => self.use_av1,
            CodecKind::H265 => self.use_h265,
            _ => true,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SwapReason {
    WatcherJoinedLowCaps,
    LimitingWatcherLeft,
    StreamerInitiated,
}

#[derive(Clone, Debug)]
pub struct SwapEvent {
    pub target: StreamSettings,
    pub reason: SwapReason,
}

fn find_cap(caps: &[CodecCap], codec: CodecKind) -> Option<&CodecCap> {
    caps.iter().find(|c| c.codec == codec)
}

fn priority_index(codec: CodecKind) -> usize {
    PRIORITY.iter().position(|c| *c == codec).unwrap_or(usize::MAX)
}

/// Pure LCD picker. `original` is the streamer's preferred ceiling — the
/// codec field is ignored (we pick the codec); the dimensions/fps cap
/// the result from above.
pub fn pick(
    streamer_encode: &[CodecCap],
    watchers_decode: &[Vec<CodecCap>],
    toggles: &Toggles,
    original: StreamSettings,
) -> StreamSettings {
    for &candidate in &PRIORITY {
        if !toggles.allows(candidate) { continue; }

        let Some(streamer_cap) = find_cap(streamer_encode, candidate) else { continue };

        // Every watcher must have this codec in their decode list.
        let mut watcher_caps_for_candidate: Vec<&CodecCap> = Vec::with_capacity(watchers_decode.len());
        let mut all_have = true;
        for w in watchers_decode {
            match find_cap(w, candidate) {
                Some(c) => watcher_caps_for_candidate.push(c),
                None => { all_have = false; break; }
            }
        }
        if !all_have { continue; }

        let mut width = original.width.min(streamer_cap.max_width);
        let mut height = original.height.min(streamer_cap.max_height);
        let mut fps = original.fps.min(streamer_cap.max_fps);
        for c in &watcher_caps_for_candidate {
            width = width.min(c.max_width);
            height = height.min(c.max_height);
            fps = fps.min(c.max_fps);
        }

        return StreamSettings { codec: candidate, width, height, fps };
    }

    // Spec §5.1 step 3: defensive fallback when no codec satisfies every
    // watcher. Essentially unreachable since H.264 decode is universal in
    // WebCodecs and we always fallback-add it (decoderProbe.ts §3.3
    // fallback). If we land here, pick the streamer's top codec; affected
    // watchers will get incompatible packets and their decoder will fail.
    eprintln!("[codec_selection] LCD failed to converge — falling back");
    let fallback_codec = streamer_encode.first().map(|c| c.codec).unwrap_or(CodecKind::H264Hw);
    let fallback_cap = find_cap(streamer_encode, fallback_codec);
    StreamSettings {
        codec: fallback_codec,
        width: original.width.min(fallback_cap.map(|c| c.max_width).unwrap_or(original.width)),
        height: original.height.min(fallback_cap.map(|c| c.max_height).unwrap_or(original.height)),
        fps: original.fps.min(fallback_cap.map(|c| c.max_fps).unwrap_or(original.fps)),
    }
}

// ──────────────────────────────────────────────────────────────────────
// CodecSelector — stateful coordinator with timers.
// ──────────────────────────────────────────────────────────────────────

pub struct CodecSelector {
    inner: Arc<Mutex<SelectorState>>,
    swap_tx: mpsc::UnboundedSender<SwapEvent>,
}

struct SelectorState {
    original: StreamSettings,
    current: StreamSettings,
    enforced: Option<CodecKind>,
    streamer_encode: Vec<CodecCap>,
    toggles: Toggles,
    /// Username → decode caps (only watchers, not all voice members).
    watchers: HashMap<String, Vec<CodecCap>>,
    timer: Option<TimerInfo>,
    timer_generation: u64,
}

struct TimerInfo {
    handle: JoinHandle<()>,
    kind: TimerKind,
    #[allow(dead_code)]
    generation: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TimerKind { Downgrade, Upgrade }

impl CodecSelector {
    pub fn new(
        initial: StreamSettings,
        enforced: Option<CodecKind>,
        streamer_encode: Vec<CodecCap>,
        toggles: Toggles,
    ) -> (Self, mpsc::UnboundedReceiver<SwapEvent>) {
        let (swap_tx, swap_rx) = mpsc::unbounded_channel();
        let state = SelectorState {
            original: initial,
            current: initial,
            enforced,
            streamer_encode,
            toggles,
            watchers: HashMap::new(),
            timer: None,
            timer_generation: 0,
        };
        (Self { inner: Arc::new(Mutex::new(state)), swap_tx }, swap_rx)
    }

    pub fn current_settings(&self) -> StreamSettings {
        self.inner.lock().unwrap().current
    }

    pub fn enforced(&self) -> Option<CodecKind> {
        self.inner.lock().unwrap().enforced
    }

    /// Reflect a swap that the pipeline actually performed.
    pub fn record_swap(&self, new: StreamSettings) {
        let mut s = self.inner.lock().unwrap();
        s.current = new;
        s.timer = None;
    }

    pub fn on_watcher_joined(self: &Arc<Self>, username: String, decode: Vec<CodecCap>) {
        let mut s = self.inner.lock().unwrap();
        if s.enforced.is_some() { return; }
        s.watchers.insert(username, decode);
        self.recompute_locked(&mut s);
    }

    pub fn on_watcher_left(self: &Arc<Self>, username: &str) {
        let mut s = self.inner.lock().unwrap();
        if s.enforced.is_some() { return; }
        s.watchers.remove(username);
        self.recompute_locked(&mut s);
    }

    fn recompute_locked(self: &Arc<Self>, s: &mut SelectorState) {
        let watchers_decode: Vec<Vec<CodecCap>> = s.watchers.values().cloned().collect();
        let target = pick(&s.streamer_encode, &watchers_decode, &s.toggles, s.original);

        if target == s.current {
            // Cancel any in-flight upgrade — current is now adequate.
            if let Some(t) = s.timer.take() {
                t.handle.abort();
            }
            return;
        }

        let new_kind = if priority_index(target.codec) < priority_index(s.current.codec) {
            TimerKind::Upgrade
        } else if priority_index(target.codec) > priority_index(s.current.codec) {
            TimerKind::Downgrade
        } else {
            // Same codec — dim/fps shrunk = downgrade, grew = upgrade.
            if target.width < s.current.width
                || target.height < s.current.height
                || target.fps < s.current.fps {
                TimerKind::Downgrade
            } else {
                TimerKind::Upgrade
            }
        };

        // Spec §5.4: cooldown is monotonic. If an upgrade is already
        // scheduled and the new event still wants an upgrade, leave the
        // existing timer alone — it'll fire and pick up the latest target.
        if let Some(existing) = &s.timer {
            if existing.kind == TimerKind::Upgrade && new_kind == TimerKind::Upgrade {
                return;
            }
            existing.handle.abort();
        }

        s.timer_generation = s.timer_generation.wrapping_add(1);
        let gen = s.timer_generation;
        let delay = match new_kind {
            TimerKind::Downgrade => DOWNGRADE_DEBOUNCE,
            TimerKind::Upgrade => UPGRADE_COOLDOWN,
        };

        let inner = self.inner.clone();
        let swap_tx = self.swap_tx.clone();
        let handle = tokio::spawn(async move {
            tokio::time::sleep(delay).await;
            let mut state = inner.lock().unwrap();
            if state.timer_generation != gen { return; }
            // Recompute target one more time at fire time — watchers may have changed.
            let watchers_decode: Vec<Vec<CodecCap>> = state.watchers.values().cloned().collect();
            let final_target = pick(&state.streamer_encode, &watchers_decode, &state.toggles, state.original);
            if final_target == state.current { return; }
            let reason = match new_kind {
                TimerKind::Downgrade => SwapReason::WatcherJoinedLowCaps,
                TimerKind::Upgrade => SwapReason::LimitingWatcherLeft,
            };
            state.timer = None;
            drop(state);
            let _ = swap_tx.send(SwapEvent { target: final_target, reason });
        });
        s.timer = Some(TimerInfo { handle, kind: new_kind, generation: gen });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cap(codec: CodecKind, w: u32, h: u32, fps: u32) -> CodecCap {
        CodecCap { codec, max_width: w, max_height: h, max_fps: fps }
    }
    fn caps(decoders: &[(CodecKind, u32, u32, u32)]) -> Vec<CodecCap> {
        decoders.iter().map(|(c, w, h, f)| cap(*c, *w, *h, *f)).collect()
    }
    fn toggles_all() -> Toggles { Toggles { use_av1: true, use_h265: true } }
    fn original_4k60() -> StreamSettings {
        StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 }
    }

    // ── pure LCD picker tests (spec §5.1) ──

    #[test]
    fn no_watchers_returns_streamers_top_codec() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let r = pick(&enc, &[], &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::Av1);
        assert_eq!(r.width, 3840);
    }

    #[test]
    fn watcher_with_av1_decode_keeps_av1() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::Av1);
        assert_eq!(r.width, 3840);
    }

    #[test]
    fn h264_only_watcher_forces_h264_and_drops_to_1440p() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let watchers = vec![vec![cap(CodecKind::H264Hw, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::H264Hw);
        assert_eq!(r.width, 2560);
        assert_eq!(r.height, 1440);
        assert_eq!(r.fps, 60);
    }

    #[test]
    fn user_disabled_av1_skips_av1_even_if_supported() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)]];
        let toggles = Toggles { use_av1: false, use_h265: true };
        let r = pick(&enc, &watchers, &toggles, original_4k60());
        assert_eq!(r.codec, CodecKind::H265);
    }

    #[test]
    fn watcher_with_only_h265_decode_picks_h265() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)];
        let watchers = vec![vec![cap(CodecKind::H265, 3840, 2160, 60), cap(CodecKind::H264Hw, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::H265);
    }

    #[test]
    fn streamer_no_av1_encode_picks_h265_even_if_watchers_decode_av1() {
        let enc = vec![cap(CodecKind::H265, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H265, 3840, 2160, 60)]];
        let r = pick(&enc, &watchers, &toggles_all(), original_4k60());
        assert_eq!(r.codec, CodecKind::H265);
    }

    #[test]
    fn original_resolution_is_upper_bound() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60)];
        let watchers = vec![vec![cap(CodecKind::Av1, 3840, 2160, 60)]];
        let original = StreamSettings { codec: CodecKind::Av1, width: 1920, height: 1080, fps: 60 };
        let r = pick(&enc, &watchers, &toggles_all(), original);
        assert_eq!(r.width, 1920);
        assert_eq!(r.height, 1080);
    }

    // ── CodecSelector timer tests (spec §5.3 / §5.4) ──

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn downgrade_on_watcher_join_fires_after_debounce() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (sel, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());
        let sel = Arc::new(sel);

        sel.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        assert!(rx.try_recv().is_err()); // not yet
        tokio::time::advance(Duration::from_millis(250)).await;
        let event = rx.recv().await.expect("swap event");
        assert_eq!(event.target.codec, CodecKind::H264Hw);
        assert_eq!(event.target.width, 2560);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn rapid_joins_coalesce_into_one_swap() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (sel, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());
        let sel = Arc::new(sel);

        sel.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(100)).await;
        sel.on_watcher_joined("bob".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(250)).await;

        let event = rx.recv().await.expect("swap event");
        assert_eq!(event.target.codec, CodecKind::H264Hw);
        // Should not get a second event for bob.
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn upgrade_after_limiting_watcher_leaves_waits_30s() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (sel, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());
        let sel = Arc::new(sel);

        sel.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(250)).await;
        let downgrade = rx.recv().await.unwrap();
        sel.record_swap(downgrade.target);

        sel.on_watcher_left("alice");
        tokio::time::advance(Duration::from_secs(10)).await;
        assert!(rx.try_recv().is_err(), "shouldn't fire before 30s");

        tokio::time::advance(Duration::from_secs(25)).await;
        let upgrade = rx.recv().await.expect("upgrade event");
        assert_eq!(upgrade.target.codec, CodecKind::Av1);
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn upgrade_cancelled_if_low_cap_watcher_rejoins_during_cooldown() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (sel, mut rx) = CodecSelector::new(initial, None, enc, toggles_all());
        let sel = Arc::new(sel);

        sel.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_millis(250)).await;
        let downgrade = rx.recv().await.unwrap();
        sel.record_swap(downgrade.target);

        sel.on_watcher_left("alice");
        tokio::time::advance(Duration::from_secs(10)).await;
        sel.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(rx.try_recv().is_err(), "no upgrade should fire — and no further downgrade either");
    }

    #[tokio::test(flavor = "current_thread", start_paused = true)]
    async fn enforcement_blocks_all_recompute() {
        let enc = vec![cap(CodecKind::Av1, 3840, 2160, 60), cap(CodecKind::H264Hw, 2560, 1440, 60)];
        let initial = StreamSettings { codec: CodecKind::Av1, width: 3840, height: 2160, fps: 60 };
        let (sel, mut rx) = CodecSelector::new(initial, Some(CodecKind::Av1), enc, toggles_all());
        let sel = Arc::new(sel);

        sel.on_watcher_joined("alice".into(), caps(&[(CodecKind::H264Hw, 3840, 2160, 60)]));
        tokio::time::advance(Duration::from_secs(60)).await;
        assert!(rx.try_recv().is_err(), "enforcement should block all recompute");
    }
}
