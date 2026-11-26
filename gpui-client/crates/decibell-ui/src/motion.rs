//! Motion: CSS timing functions, transitions and keyframe tracks, and the rule that keeps them
//! lean: **an animation only asks for frames while its value is changing**, and a reduced-motion
//! app (`App::reduce_motion`) jumps straight to the end.
//!
//! - [`CubicBezier`]: CSS `cubic-bezier()` solved as Chromium solves it (`gfx::CubicBezier`), plus
//!   the named curves the renderer uses ([`EASE`], [`EASE_OUT`], [`TAILWIND`], …).
//! - [`transition`]: a CSS transition of one number, kept per element: when `target` changes, the
//!   value runs from where it is to the new target over the duration, with Chromium's
//!   reversal shortening. It requests the next frame while it runs and none after.
//! - [`Keyframes`]: a CSS `@keyframes` track (`barNudge`, `fadeUp`), evaluated per segment with the
//!   animation's timing function, as CSS does; drive it with GPUI's `with_animation`.
//!
//! ```ignore
//! let t = motion::transition(window, cx, ("switch-knob", id), if on { 1. } else { 0. },
//!     Transition::TAILWIND);
//! div().ml(px(18. * t)).bg(motion::mix(off_bg, on_bg, t))
//! ```

use std::time::{Duration, Instant};

use gpui::{App, ElementId, Hsla, Rgba, Window};

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    ax: f64,
    bx: f64,
    cx: f64,
    ay: f64,
    by: f64,
    cy: f64,
    /// The control points, kept for the end slopes.
    x1: f64,
    y1: f64,
    x2: f64,
    y2: f64,
}

/// CSS `ease`: `cubic-bezier(0.25, 0.1, 0.25, 1)`.
pub const EASE: CubicBezier = CubicBezier::new(0.25, 0.1, 0.25, 1.);
/// CSS `ease-in-out` (and cc's scroll curve): `cubic-bezier(0.42, 0, 0.58, 1)`.
pub const EASE_IN_OUT: CubicBezier = CubicBezier::new(0.42, 0., 0.58, 1.);
/// CSS `ease-out`: `cubic-bezier(0, 0, 0.58, 1)`.
pub const CSS_EASE_OUT: CubicBezier = CubicBezier::new(0., 0., 0.58, 1.);
/// Tailwind's `ease-out` (`--ease-out`): `cubic-bezier(0, 0, 0.2, 1)`.
pub const EASE_OUT: CubicBezier = CubicBezier::new(0., 0., 0.2, 1.);
/// Tailwind's default transition curve (`transition-*` without `ease-*`):
/// `cubic-bezier(0.4, 0, 0.2, 1)`.
pub const TAILWIND: CubicBezier = CubicBezier::new(0.4, 0., 0.2, 1.);
/// The renderer's "pop" curve (`dselIn`, `bellRise`, the image viewer): `cubic-bezier(0.2, 0.8, 0.2, 1)`.
pub const POP: CubicBezier = CubicBezier::new(0.2, 0.8, 0.2, 1.);
/// `linear`.
pub const LINEAR: CubicBezier = CubicBezier::new(0., 0., 1., 1.);

impl CubicBezier {
    pub const fn new(x1: f64, y1: f64, x2: f64, y2: f64) -> Self {
        let cx = 3. * x1;
        let bx = 3. * (x2 - x1) - cx;
        let ax = 1. - cx - bx;
        let cy = 3. * y1;
        let by = 3. * (y2 - y1) - cy;
        let ay = 1. - cy - by;
        Self {
            ax,
            bx,
            cx,
            ay,
            by,
            cy,
            x1,
            y1,
            x2,
            y2,
        }
    }

    fn sample_x(&self, t: f64) -> f64 {
        ((self.ax * t + self.bx) * t + self.cx) * t
    }

    fn sample_y(&self, t: f64) -> f64 {
        ((self.ay * t + self.by) * t + self.cy) * t
    }

    fn sample_dx(&self, t: f64) -> f64 {
        (3. * self.ax * t + 2. * self.bx) * t + self.cx
    }

    fn sample_dy(&self, t: f64) -> f64 {
        (3. * self.ay * t + 2. * self.by) * t + self.cy
    }

    /// The curve parameter whose x is `x` (Newton's method, then bisection), to within 1e-9.
    fn solve_t(&self, x: f64) -> f64 {
        const EPSILON: f64 = 1e-9;
        let mut t = x;
        for _ in 0..8 {
            let error = self.sample_x(t) - x;
            if error.abs() < EPSILON {
                return t;
            }
            let slope = self.sample_dx(t);
            if slope.abs() < 1e-7 {
                break;
            }
            t -= error / slope;
        }
        let (mut lo, mut hi) = (0., 1.);
        t = x;
        for _ in 0..64 {
            let value = self.sample_x(t);
            if (value - x).abs() < EPSILON {
                break;
            }
            if value < x {
                lo = t;
            } else {
                hi = t;
            }
            t = (lo + hi) / 2.;
        }
        t
    }

    /// The eased value at progress `x` (clamped to `[0, 1]`).
    pub fn y(&self, x: f64) -> f64 {
        let x = x.clamp(0., 1.);
        if x == 0. || x == 1. {
            return x;
        }
        self.sample_y(self.solve_t(x))
    }

    /// dy/dx at progress `x` (clamped to `[0, 1]`).
    pub fn slope(&self, x: f64) -> f64 {
        let x = x.clamp(0., 1.);
        if x == 0. {
            return if self.x1 > 0. {
                self.y1 / self.x1
            } else if self.y1 == 0. && self.x2 > 0. {
                self.y2 / self.x2
            } else {
                0.
            };
        }
        if x == 1. {
            return if self.x2 < 1. {
                (self.y2 - 1.) / (self.x2 - 1.)
            } else if self.y2 == 1. && self.x1 < 1. {
                (self.y1 - 1.) / (self.x1 - 1.)
            } else {
                0.
            };
        }
        let t = self.solve_t(x);
        let dx = self.sample_dx(t);
        if dx.abs() < 1e-12 {
            0.
        } else {
            self.sample_dy(t) / dx
        }
    }

    /// As an `Animation::with_easing` closure.
    pub fn easing(self) -> impl Fn(f32) -> f32 + 'static {
        move |x| self.y(x as f64) as f32
    }
}

impl From<decibell_theme::Easing> for CubicBezier {
    fn from(easing: decibell_theme::Easing) -> Self {
        let decibell_theme::Easing::CubicBezier(x1, y1, x2, y2) = easing;
        CubicBezier::new(x1 as f64, y1 as f64, x2 as f64, y2 as f64)
    }
}

/// How a value transitions: a CSS `transition-duration` and `transition-timing-function`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Transition {
    pub duration: Duration,
    pub easing: CubicBezier,
}

impl Transition {
    /// Tailwind's `transition-*` default: 150 ms, `cubic-bezier(0.4, 0, 0.2, 1)`.
    pub const TAILWIND: Transition = Transition::new(150, TAILWIND);
    /// `duration-300`, Tailwind's curve: the modal backdrop and card.
    pub const MODAL: Transition = Transition::new(300, TAILWIND);

    pub const fn new(millis: u64, easing: CubicBezier) -> Self {
        Self {
            duration: Duration::from_millis(millis),
            easing,
        }
    }
}

/// One running (or finished) transition of a number.
#[derive(Debug, Clone, Copy)]
struct TransitionState {
    from: f32,
    to: f32,
    /// The value the running transition started from, for the reversal rule.
    start_value: f32,
    started: Option<Instant>,
    duration: Duration,
}

impl TransitionState {
    fn settled(value: f32) -> Self {
        Self {
            from: value,
            to: value,
            start_value: value,
            started: None,
            duration: Duration::ZERO,
        }
    }

    /// The value at `now` and whether the transition is still running.
    fn value(&self, now: Instant, easing: &CubicBezier) -> (f32, bool) {
        let Some(started) = self.started else {
            return (self.to, false);
        };
        if self.duration.is_zero() || self.from == self.to {
            return (self.to, false);
        }
        let elapsed = now.saturating_duration_since(started);
        if elapsed >= self.duration {
            return (self.to, false);
        }
        let x = elapsed.as_secs_f64() / self.duration.as_secs_f64();
        let eased = easing.y(x) as f32;
        (self.from + (self.to - self.from) * eased, true)
    }

    /// CSS Transitions §3: a change of target restarts from the current value; going back to
    /// where the running transition started takes only as long as the way there took
    /// (the reversing shortening factor).
    fn retarget(&mut self, target: f32, now: Instant, spec: &Transition) {
        let (current, running) = self.value(now, &spec.easing);
        let mut duration = spec.duration;
        let mut start_value = current;
        if running && target == self.start_value && self.to != self.start_value {
            // How much of the way from the start value the value has come.
            let covered = ((current - self.start_value) / (self.to - self.start_value))
                .abs()
                .clamp(0., 1.);
            duration = spec.duration.mul_f32(covered);
            start_value = self.to;
        }
        *self = TransitionState {
            from: current,
            to: target,
            start_value,
            started: Some(now),
            duration,
        };
    }
}

/// The time base of every animation here: GPUI's executor clock (the test platform fakes it).
pub fn now(cx: &App) -> Instant {
    cx.background_executor().now()
}

/// A CSS transition of one number, kept in the element state of `id` (unique among its siblings'
/// ids, stable across frames): returns the value to draw now. When `target` differs from last
/// frame's, the value runs from where it is to `target`; while it runs, the next frame is
/// requested (re-rendering the calling view), and once it arrives nothing more is. On the first
/// render, and with reduced motion, the value is `target` at once.
///
/// Call it from `render` (or a `RenderOnce` component's render): it needs the current view.
pub fn transition(
    window: &mut Window,
    cx: &mut App,
    id: impl Into<ElementId>,
    target: f32,
    spec: Transition,
) -> f32 {
    let now = now(cx);
    let reduce_motion = cx.reduce_motion();
    let (value, running) = window.with_global_id(id.into(), |global_id, window| {
        window.with_element_state(global_id, |state: Option<TransitionState>, _| {
            let mut state = state.unwrap_or_else(|| TransitionState::settled(target));
            if reduce_motion {
                state = TransitionState::settled(target);
            } else if state.to != target {
                state.retarget(target, now, &spec);
            }
            (state.value(now, &spec.easing), state)
        })
    });
    if running {
        window.request_animation_frame();
    }
    value
}

/// [`transition`] that starts from `initial` the first time `id` is drawn (an entrance: a modal
/// fading in from 0 as it mounts), instead of at `target`.
pub fn transition_from(
    window: &mut Window,
    cx: &mut App,
    id: impl Into<ElementId>,
    initial: f32,
    target: f32,
    spec: Transition,
) -> f32 {
    let now = now(cx);
    let reduce_motion = cx.reduce_motion();
    let (value, running) = window.with_global_id(id.into(), |global_id, window| {
        window.with_element_state(global_id, |state: Option<TransitionState>, _| {
            let mut state = state.unwrap_or_else(|| TransitionState::settled(initial));
            if reduce_motion {
                state = TransitionState::settled(target);
            } else if state.to != target {
                state.retarget(target, now, &spec);
            }
            (state.value(now, &spec.easing), state)
        })
    });
    if running {
        window.request_animation_frame();
    }
    value
}

/// [`transition`] of a state that is on or off: 0 when off, 1 when on, in between while it moves.
pub fn transition_flag(
    window: &mut Window,
    cx: &mut App,
    id: impl Into<ElementId>,
    on: bool,
    spec: Transition,
) -> f32 {
    transition(window, cx, id, if on { 1. } else { 0. }, spec)
}

/// `a` → `b` at `t` (0..1), interpolated as CSS interpolates colours in a transition:
/// premultiplied sRGB.
pub fn mix(a: Hsla, b: Hsla, t: f32) -> Hsla {
    if t <= 0. {
        return a;
    }
    if t >= 1. {
        return b;
    }
    let (a, b): (Rgba, Rgba) = (a.into(), b.into());
    let alpha = a.a + (b.a - a.a) * t;
    if alpha <= 0. {
        return Hsla::transparent_black();
    }
    let channel = |x: f32, y: f32| (x * a.a + (y * b.a - x * a.a) * t) / alpha;
    Rgba {
        r: channel(a.r, b.r),
        g: channel(a.g, b.g),
        b: channel(a.b, b.b),
        a: alpha,
    }
    .into()
}

/// A CSS `@keyframes` track of one number: `(offset 0..1, value)` stops, eased per segment by
/// the animation's timing function (as CSS applies `animation-timing-function` between keyframes).
#[derive(Debug, Clone, Copy)]
pub struct Keyframes {
    pub stops: &'static [(f32, f32)],
    pub easing: CubicBezier,
}

impl Keyframes {
    /// `barNudge` (the save bar's shake): translateX 0, −6, 5, −3, 2, 0 px at 0/20/40/60/80/100 %.
    /// The renderer runs it for 0.36 s with `ease`.
    pub const BAR_NUDGE: Keyframes = Keyframes {
        stops: &[
            (0., 0.),
            (0.2, -6.),
            (0.4, 5.),
            (0.6, -3.),
            (0.8, 2.),
            (1., 0.),
        ],
        easing: EASE,
    };

    /// The value at linear progress `t` (0..1).
    pub fn value(&self, t: f32) -> f32 {
        let stops = self.stops;
        let t = t.clamp(0., 1.);
        let index = stops
            .windows(2)
            .position(|pair| t <= pair[1].0)
            .unwrap_or(stops.len().saturating_sub(2));
        let ((t0, v0), (t1, v1)) = (stops[index], stops[index + 1]);
        let local = if t1 > t0 { (t - t0) / (t1 - t0) } else { 1. };
        v0 + (v1 - v0) * self.easing.y(local as f64) as f32
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ease_in_out_matches_the_spec_spot_values() {
        // docs/wheel-scroll.md §9.
        let expect = [
            (0.05, 0.0048),
            (0.10, 0.0197),
            (0.20, 0.0817),
            (0.25, 0.1292),
            (0.30, 0.1874),
            (0.40, 0.3319),
            (0.50, 0.5000),
            (0.60, 0.6681),
            (0.70, 0.8126),
            (0.80, 0.9183),
            (0.90, 0.9803),
            (0.95, 0.9952),
        ];
        for (x, y) in expect {
            assert!((EASE_IN_OUT.y(x) - y).abs() < 5e-5, "B({x}) = {}", EASE_IN_OUT.y(x));
        }
    }

    #[test]
    fn slopes_are_the_derivative() {
        for curve in [EASE, EASE_IN_OUT, TAILWIND, CubicBezier::new(0.42, 0.9, 0.58, 1.)] {
            for x in [0.1, 0.3, 0.5, 0.7, 0.9] {
                let h = 1e-6;
                let numeric = (curve.y(x + h) - curve.y(x - h)) / (2. * h);
                assert!((curve.slope(x) - numeric).abs() < 1e-4, "{curve:?} at {x}");
            }
        }
        assert!((CubicBezier::new(0.42, 0.84, 0.58, 1.).slope(0.) - 2.).abs() < 1e-12);
        assert_eq!(EASE_IN_OUT.slope(0.), 0.);
    }

    #[test]
    fn transitions_run_and_reverse_shortened() {
        let spec = Transition::new(100, LINEAR);
        let mut state = TransitionState::settled(0.);
        let t0 = Instant::now();
        state.retarget(1., t0, &spec);
        let ms = |n| t0 + Duration::from_millis(n);
        assert_eq!(state.value(ms(50), &LINEAR), (0.5, true));
        assert_eq!(state.value(ms(100), &LINEAR), (1., false));
        // Reversed half way: back in half the time.
        state.retarget(0., ms(50), &spec);
        assert_eq!(state.duration, Duration::from_millis(50));
        assert!((state.value(ms(75), &LINEAR).0 - 0.25).abs() < 1e-6);
        assert_eq!(state.value(ms(100), &LINEAR), (0., false));
    }

    #[test]
    fn colours_mix_premultiplied() {
        let red = Hsla::from(Rgba { r: 1., g: 0., b: 0., a: 1. });
        let clear = Hsla::from(Rgba { r: 0., g: 0., b: 1., a: 0. });
        let half: Rgba = mix(red, clear, 0.5).into();
        // Premultiplied: a transparent colour contributes no hue.
        assert!((half.r - 1.).abs() < 1e-4 && half.b.abs() < 1e-4 && (half.a - 0.5).abs() < 1e-4);
    }

    #[test]
    fn keyframes_ease_per_segment() {
        let nudge = Keyframes::BAR_NUDGE;
        assert_eq!(nudge.value(0.), 0.);
        assert_eq!(nudge.value(0.2), -6.);
        assert_eq!(nudge.value(0.4), 5.);
        assert_eq!(nudge.value(1.), 0.);
        // Half way through the first segment, eased by `ease` (0.5 → 0.8024).
        assert!((nudge.value(0.1) - -6. * EASE.y(0.5) as f32).abs() < 1e-5);
    }
}
