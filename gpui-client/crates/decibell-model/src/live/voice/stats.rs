//! The voice link's telemetry (`voiceStatsStore`, `voiceStore.latencyMs`): the last ping
//! and a bounded history of ping + loss samples for the connection popover's graph.
//!
//! **Rhythm**: the engine sends a ping round trip and a stats sample every ~2 s while a
//! session runs: the user panel's signal icon and the open popover observe this, nothing
//! else.

use std::collections::VecDeque;
use std::time::{Duration, Instant};

use gpui::Context;

/// Samples kept (~6–7 min at one per 2 s; the renderer's 200).
pub const MAX_SAMPLES: usize = 200;
/// A gap this long between samples starts a new history, so the graph doesn't draw a
/// line between two unrelated sessions.
pub const STALE_GAP: Duration = Duration::from_secs(30);

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct StatsSample {
    pub at: Instant,
    /// Round trip, ms; none until the first PING reply lands.
    pub ping_ms: Option<u32>,
    /// Audio packet loss over the last window, %.
    pub loss_pct: f32,
}

#[derive(Default)]
pub struct VoiceStats {
    /// The last ping (`voice_ping_updated`).
    pub latency_ms: Option<u32>,
    samples: VecDeque<StatsSample>,
}

impl VoiceStats {
    pub fn samples(&self) -> &VecDeque<StatsSample> {
        &self.samples
    }

    pub fn set_latency(&mut self, ms: u32, cx: &mut Context<Self>) {
        if self.latency_ms != Some(ms) {
            self.latency_ms = Some(ms);
            cx.notify();
        }
    }

    pub fn push(&mut self, sample: StatsSample, cx: &mut Context<Self>) {
        if let Some(last) = self.samples.back()
            && sample.at.saturating_duration_since(last.at) > STALE_GAP
        {
            self.samples.clear();
        }
        if self.samples.len() >= MAX_SAMPLES {
            self.samples.pop_front();
        }
        self.samples.push_back(sample);
        cx.notify();
    }

    /// Leaving voice: the history and the ping go.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if self.latency_ms.is_some() || !self.samples.is_empty() {
            self.latency_ms = None;
            self.samples.clear();
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn history_is_bounded_and_restarts_after_a_gap(cx: &mut TestAppContext) {
        let stats = cx.new(|_| VoiceStats::default());
        let t0 = cx.executor().now();
        let sample = |s: u64| StatsSample { at: t0 + Duration::from_secs(s), ping_ms: Some(20), loss_pct: 0.0 };
        stats.update(cx, |st, cx| {
            for i in 0..(MAX_SAMPLES as u64 + 50) {
                st.push(sample(i * 2), cx);
            }
        });
        stats.read_with(cx, |st, _| {
            assert_eq!(st.samples().len(), MAX_SAMPLES);
            assert_eq!(st.samples().front().unwrap().at, t0 + Duration::from_secs(100));
        });
        // 31 s of silence: a new session's history.
        stats.update(cx, |st, cx| st.push(sample((MAX_SAMPLES as u64 + 49) * 2 + 31), cx));
        stats.read_with(cx, |st, _| assert_eq!(st.samples().len(), 1));
        stats.update(cx, |st, cx| {
            st.set_latency(42, cx);
            st.clear(cx);
        });
        stats.read_with(cx, |st, _| assert!(st.samples().is_empty() && st.latency_ms.is_none()));
    }
}
