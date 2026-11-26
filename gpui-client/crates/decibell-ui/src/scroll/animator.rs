//! Chromium's wheel and key scrolling as a pure state machine: `gpui-client/docs/wheel-scroll.md`
//! §2 (cc's `ScrollOffsetAnimationCurve`, every input handled at its own timestamp). No GPUI here:
//! the [`ScrollArea`](super::ScrollArea) element and the message list each drive one with their own
//! clock and frames.
//!
//! ```ignore
//! let mut animator = ScrollAnimator::new();
//! animator.set_max_offset(content_height - viewport_height);
//! animator.input(now, ScrollAnimator::notches(-lines.y), ScrollKind::Wheel); // a wheel notch
//! animator.precise(now, -pixels.y);                                          // a touchpad delta
//! let top = animator.position(frame_time);       // scrollTop for this frame
//! if animator.is_animating() { /* ask for the next frame */ }
//! ```

use std::time::Instant;

use crate::motion::CubicBezier;

/// Where a scroll step came from: it picks the animation's duration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScrollKind {
    /// A mouse-wheel notch: 200 ms up to 120 px of travel, down to 100 ms from 480 px.
    Wheel,
    /// A scroll key (arrows, Page Up / Down, Home / End, Space): 150 ms whatever the distance.
    Key,
}

/// CSS px per wheel notch on Linux (`kWheelScrollAmount`).
pub const WHEEL_NOTCH_PX: f64 = 120.;
/// CSS px per arrow key (`cc::kPixelsPerLineStep`).
pub const LINE_STEP_PX: f64 = 40.;
/// Page Up / Page Down / Space move this share of the visible height, rounded down.
pub const PAGE_STEP_FRACTION: f64 = 0.875;

/// px of wheel travel per GPUI line: GPUI reports a Linux detent as 3 lines (`value120 / 120 × 3`)
/// and a Windows one as `WHEEL_DELTA / 120 × SPI_GETWHEELSCROLLLINES` (3 by default); Chromium
/// scrolls 120 px per detent on Linux and 100 px (3 lines × 33.3) on Windows.
pub const PX_PER_GPUI_LINE: f64 = if cfg!(target_os = "windows") {
    100. / 3.
} else {
    WHEEL_NOTCH_PX / 3.
};

/// One segment of the running animation.
#[derive(Debug, Clone, Copy)]
struct Animation {
    from: f64,
    to: f64,
    /// When the first input of this run arrived; every time below is ms after it.
    t0: Instant,
    /// When the current segment starts (ms after `t0`); the position holds at `from` until then.
    t_seg: f64,
    /// When the current segment ends (ms after `t0`).
    end: f64,
    /// The bezier's y1: `0.42 × the initial slope` (0 for a segment from rest).
    y1: f64,
}

impl Animation {
    fn curve(&self) -> CubicBezier {
        CubicBezier::new(0.42, self.y1, 0.58, 1.)
    }

    fn elapsed(&self, now: Instant) -> f64 {
        now.saturating_duration_since(self.t0).as_secs_f64() * 1e3
    }

    fn x(&self, u: f64) -> f64 {
        ((u - self.t_seg) / (self.end - self.t_seg)).clamp(0., 1.)
    }

    fn position_at(&self, u: f64) -> f64 {
        if u <= self.t_seg {
            return self.from;
        }
        self.from + self.curve().y(self.x(u)) * (self.to - self.from)
    }

    /// px per ms in the direction of travel.
    fn velocity_at(&self, u: f64) -> f64 {
        self.curve().slope(self.x(u)) * (self.to - self.from) / (self.end - self.t_seg)
    }
}

/// The scroll position of one scroller and its running animation, if any. Positions are px from
/// the top of the content (CSS `scrollTop`), in `[0, max_offset]`.
#[derive(Debug, Clone, Default)]
pub struct ScrollAnimator {
    /// The resting position (where the last animation ended, or the last precise / programmatic one).
    rest: f64,
    max: f64,
    animation: Option<Animation>,
}

impl ScrollAnimator {
    pub fn new() -> Self {
        Self::default()
    }

    /// A wheel delta in GPUI lines (`ScrollDelta::Lines`, + = towards the top, as GPUI reports it)
    /// as a scroll distance in px (+ = down, towards the end).
    pub fn lines_to_px(lines_y: f32) -> f64 {
        -(lines_y as f64) * PX_PER_GPUI_LINE
    }

    /// `D_wheel` / `D_key` in ms for a travel of `delta` px.
    pub fn duration(kind: ScrollKind, delta: f64) -> f64 {
        match kind {
            ScrollKind::Wheel => (14. - delta.abs() / 60.).clamp(6., 12.) / 60. * 1000.,
            ScrollKind::Key => 150.,
        }
    }

    /// The page step for a viewport `visible` px tall (Page Up / Down, Space).
    pub fn page_step(visible: f64) -> f64 {
        (PAGE_STEP_FRACTION * visible).floor()
    }

    pub fn max_offset(&self) -> f64 {
        self.max
    }

    /// The content or the viewport changed size: the range is now `[0, max]`. A position past the
    /// end moves back to it (the browser clamps scrollTop the same way).
    pub fn set_max_offset(&mut self, max: f64) {
        self.max = max.max(0.);
        self.rest = self.rest.clamp(0., self.max);
        if let Some(animation) = &mut self.animation {
            animation.to = animation.to.clamp(0., self.max);
        }
    }

    pub fn is_animating(&self) -> bool {
        self.animation.is_some()
    }

    /// Where the running animation is heading (the resting position if none).
    pub fn target(&self) -> f64 {
        self.animation.map_or(self.rest, |animation| animation.to)
    }

    /// The position for a frame at `now`; ends the animation once it is over.
    pub fn position(&mut self, now: Instant) -> f64 {
        let Some(animation) = self.animation else {
            return self.rest;
        };
        let u = animation.elapsed(now);
        if u >= animation.end {
            self.rest = animation.to.clamp(0., self.max);
            self.animation = None;
            return self.rest;
        }
        animation.position_at(u).clamp(0., self.max)
    }

    /// A notch or a key step of `delta` px (+ = down) at `now`: starts an animation, or retargets
    /// the running one keeping its speed (cc's `UpdateTarget`).
    pub fn input(&mut self, now: Instant, delta: f64, kind: ScrollKind) {
        if let Some(animation) = self.animation
            && animation.elapsed(now) >= animation.end
        {
            self.rest = animation.to;
            self.animation = None;
        }
        let Some(animation) = self.animation else {
            let to = (self.rest + delta).clamp(0., self.max);
            if (to - self.rest).abs() <= 0.1 {
                // At the end already: nothing moves.
                self.rest = to;
                return;
            }
            self.animation = Some(Animation {
                from: self.rest,
                to,
                t0: now,
                t_seg: 0.,
                end: Self::duration(kind, to - self.rest),
                y1: 0.,
            });
            return;
        };

        let new_to = (animation.to + delta).clamp(0., self.max);
        if (new_to - animation.to).abs() < 0.01 {
            // Clamped onto the same target: unchanged.
            return;
        }
        let u = animation.elapsed(now);
        let current = animation.position_at(u);
        let velocity = animation.velocity_at(u);
        let remaining = new_to - current;
        let mut next = animation;
        if remaining.abs() < 0.01 {
            next.to = new_to;
            next.end = u;
            next.t_seg = u;
            self.animation = Some(next);
            return;
        }
        let mut duration = Self::duration(kind, remaining);
        if velocity.abs() >= 0.00001 && remaining / velocity > 0. {
            // Moving towards it already: don't crawl the last pixels.
            duration = duration.min(2.5 * remaining / velocity);
        }
        if duration < 10. {
            // cc: a duration under kEpsilon (0.01 s) jumps to the target.
            next.to = new_to;
            next.end = u;
            self.animation = Some(next);
            return;
        }
        let slope = (velocity * duration / remaining).clamp(-1000., 1000.);
        self.animation = Some(Animation {
            from: current,
            to: new_to,
            t0: animation.t0,
            t_seg: u,
            end: u + duration,
            y1: 0.42 * slope,
        });
    }

    /// A precise delta (a touchpad, `ScrollDelta::Pixels`): no animation. A running one stops
    /// where it is, then the delta is applied.
    pub fn precise(&mut self, now: Instant, delta: f64) {
        let current = self.position(now);
        self.animation = None;
        self.rest = (current + delta).clamp(0., self.max);
    }

    /// Jumps to `offset` (programmatic scrolling, a scrollbar drag): no animation.
    pub fn jump_to(&mut self, offset: f64) {
        self.animation = None;
        self.rest = offset.clamp(0., self.max);
    }

    /// Content was inserted (+) or removed (−) above the viewport: the position and a running
    /// animation move with it and keep their timing (cc's `ApplyAdjustment`).
    pub fn shift(&mut self, by: f64) {
        self.rest = (self.rest + by).clamp(0., self.max);
        if let Some(animation) = &mut self.animation {
            animation.from += by;
            animation.to = (animation.to + by).clamp(0., self.max);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    /// One vector of docs/wheel-scroll.md §9: inputs (px, ms, kind), the start and range, and the
    /// expected positions (ms, px). Inputs at a sample's time are applied before the sample.
    fn check(
        name: &str,
        p0: f64,
        max: f64,
        inputs: &[(f64, u64, ScrollKind)],
        expect: &[(u64, f64)],
    ) {
        let origin = Instant::now();
        let at = |ms: u64| origin + Duration::from_millis(ms);
        let mut animator = ScrollAnimator::new();
        animator.set_max_offset(max);
        animator.jump_to(p0);
        let mut inputs = inputs.iter().peekable();
        let mut worst: f64 = 0.;
        for &(t, want) in expect {
            while let Some(&&(delta, when, kind)) = inputs.peek() {
                if when > t {
                    break;
                }
                animator.input(at(when), delta, kind);
                inputs.next();
            }
            let got = animator.position(at(t));
            assert!(
                (got - want).abs() <= 0.05,
                "{name} at {t} ms: {got:.3}, expected {want:.2}"
            );
            worst = worst.max((got - want).abs());
        }
        // `cargo test -p decibell-ui vectors -- --nocapture` lists them.
        println!("{name}: {} samples, worst {worst:.4} px", expect.len());
    }

    use ScrollKind::{Key, Wheel};

    #[test]
    fn w1_one_notch_down() {
        check(
            "W1",
            1000.,
            10000.,
            &[(120., 0, Wheel)],
            &[
                (0, 1000.00),
                (10, 1000.58),
                (25, 1003.73),
                (50, 1015.50),
                (75, 1035.13),
                (100, 1060.00),
                (125, 1084.87),
                (150, 1104.50),
                (175, 1116.27),
                (199, 1119.99),
                (200, 1120.00),
                (250, 1120.00),
            ],
        );
    }

    #[test]
    fn w2_one_notch_up() {
        check(
            "W2",
            1000.,
            10000.,
            &[(-120., 0, Wheel)],
            &[(25, 996.27), (50, 984.50), (100, 940.00), (150, 895.50), (200, 880.00)],
        );
    }

    #[test]
    fn w3_two_notches_60_ms_apart() {
        check(
            "W3",
            1000.,
            10000.,
            &[(120., 0, Wheel), (120., 60, Wheel)],
            &[
                (30, 1005.43),
                (60, 1022.49),
                (90, 1053.72),
                (120, 1101.65),
                (150, 1158.61),
                (200, 1227.78),
                (250, 1240.00),
            ],
        );
    }

    #[test]
    fn w4_three_notches_30_ms_apart() {
        check(
            "W4",
            1000.,
            10000.,
            &[(120., 0, Wheel), (120., 30, Wheel), (120., 60, Wheel)],
            &[
                (30, 1005.43),
                (60, 1029.27),
                (90, 1087.03),
                (120, 1183.40),
                (150, 1286.03),
                (180, 1347.94),
                (203, 1360.00),
            ],
        );
    }

    #[test]
    fn w5_five_notches_30_ms_apart() {
        let inputs: Vec<_> = (0..5).map(|i| (120., i * 30, Wheel)).collect();
        check(
            "W5",
            1000.,
            10000.,
            &inputs,
            &[
                (60, 1029.27),
                (120, 1189.08),
                (150, 1327.42),
                (180, 1474.26),
                (210, 1572.64),
                (239, 1600.00),
            ],
        );
    }

    #[test]
    fn w6_reversal() {
        check(
            "W6",
            1000.,
            10000.,
            &[(120., 0, Wheel), (-120., 60, Wheel)],
            &[
                (30, 1005.43),
                (60, 1022.49),
                (90, 1040.16),
                (115, 1045.92),
                (150, 1039.52),
                (200, 1014.83),
                (260, 1000.00),
            ],
        );
    }

    #[test]
    fn w7_thirty_px_from_the_end() {
        check(
            "W7",
            1000.,
            1030.,
            &[(120., 0, Wheel)],
            &[(50, 1003.87), (100, 1015.00), (150, 1026.13), (200, 1030.00)],
        );
    }

    #[test]
    fn w8_twenty_notches_40_ms_apart() {
        let inputs: Vec<_> = (0..20).map(|i| (120., i * 40, Wheel)).collect();
        check(
            "W8",
            1000.,
            100000.,
            &inputs,
            &[
                (100, 1087.62),
                (200, 1413.68),
                (400, 2029.53),
                (600, 2629.23),
                (760, 3108.85),
                (800, 3228.74),
                (900, 3398.61),
                (1000, 3400.00),
            ],
        );
    }

    #[test]
    fn k1_arrow_down() {
        check(
            "K1",
            1000.,
            10000.,
            &[(40., 0, Key)],
            &[(25, 1002.25), (50, 1009.27), (75, 1020.00), (100, 1030.73), (150, 1040.00)],
        );
    }

    #[test]
    fn k2_page_down() {
        assert_eq!(ScrollAnimator::page_step(574.), 502.);
        check(
            "K2",
            1000.,
            10000.,
            &[(502., 0, Key)],
            &[
                (25, 1028.18),
                (50, 1116.35),
                (75, 1251.00),
                (100, 1385.65),
                (125, 1473.82),
                (150, 1502.00),
            ],
        );
    }

    #[test]
    fn k3_arrow_down_held() {
        let mut inputs = vec![(40., 0, Key)];
        inputs.extend([600, 640, 680, 720, 760].map(|t| (40., t, Key)));
        check(
            "K3",
            1000.,
            10000.,
            &inputs,
            &[
                (100, 1030.73),
                (150, 1040.00),
                (620, 1041.42),
                (700, 1078.88),
                (800, 1176.89),
                (900, 1239.63),
            ],
        );
    }

    #[test]
    fn durations_follow_the_inverse_delta_rule() {
        for (delta, ms) in [
            (30., 200.),
            (120., 200.),
            (180., 183.33),
            (240., 166.67),
            (300., 150.),
            (360., 133.33),
            (480., 100.),
            (600., 100.),
        ] {
            assert!((ScrollAnimator::duration(Wheel, delta) - ms).abs() < 0.01, "{delta}");
        }
    }

    #[test]
    fn precise_deltas_and_the_ends() {
        let now = Instant::now();
        let mut animator = ScrollAnimator::new();
        animator.set_max_offset(500.);
        // At the top, a notch up moves nothing and starts nothing.
        animator.input(now, -120., Wheel);
        assert!(!animator.is_animating());
        assert_eq!(animator.position(now), 0.);
        // A precise delta cancels a running animation where it is.
        animator.input(now, 120., Wheel);
        let mid = now + Duration::from_millis(100);
        let there = animator.position(mid);
        animator.precise(mid, 10.);
        assert!(!animator.is_animating());
        assert!((animator.position(mid) - (there + 10.)).abs() < 1e-9);
        // Content inserted above moves everything along.
        animator.input(mid, 120., Wheel);
        let target = animator.target();
        animator.set_max_offset(5000.);
        animator.shift(1000.);
        assert!((animator.target() - (target + 1000.)).abs() < 1e-9);
        // Shrinking the content clamps the position.
        animator.jump_to(400.);
        animator.set_max_offset(100.);
        assert_eq!(animator.position(mid), 100.);
    }

    #[test]
    fn lines_convert_to_notches() {
        // GPUI: one Linux detent = 3 lines, + = towards the top.
        if !cfg!(target_os = "windows") {
            assert_eq!(ScrollAnimator::lines_to_px(-3.), 120.);
            assert_eq!(ScrollAnimator::lines_to_px(1.5), -60.);
        }
    }
}
