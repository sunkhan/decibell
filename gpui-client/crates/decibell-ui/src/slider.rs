//! The range slider of the settings and menus (`type="range"` with the app's thumb classes).

use std::rc::Rc;

use gpui::{
    App, BoxShadow, Bounds, DispatchPhase, ElementId, Entity, HitboxBehavior, IntoElement,
    MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, RenderOnce, ScrollEvent,
    Window, canvas, div, prelude::*, px, relative,
};

use crate::ActiveTheme as _;
use crate::interaction::{focus_handle, ring};

type ValueHandler = Rc<dyn Fn(&f32, &mut Window, &mut App) + 'static>;

/// The thumb's diameter (`[&::-webkit-slider-thumb]:h-[14px] w-[14px]`).
const THUMB: f32 = 14.;

#[derive(Default)]
struct Drag {
    /// Where on the thumb it was grabbed, from its centre (px); `None` while not dragging.
    grab: Option<f32>,
}

/// A slider: a 4 px `rounded-full bg-bg-lighter` track and a 14 px thumb (`bg-bg-light`,
/// `border-2 border-accent`, `shadow-[0_0_6px_var(--color-accent-mid)]`) that stays inside the
/// track, as Chromium draws a range input. Optionally the run up to the thumb in the accent
/// (`linear-gradient(to right, accent calc(f·(100% − 14px) + 7px), bg-lighter 0)`), and tick
/// marks (8 px `bg-text-muted/40` hairlines at values).
///
/// Pointer: a press on the track moves the thumb there and drags; a press on the thumb drags it
/// from where it was grabbed. Keys while focused: arrows ± step, Page Up / Down ± a tenth,
/// Home / End. The wheel moves it only with [`Slider::wheel_step`] (the media volume bars;
/// Chromium's range inputs ignore the wheel). Layout height is the track's 4 px; the thumb and
/// the pointer area overhang it by 5 px each way.
///
/// ```ignore
/// Slider::new("ptt-delay", self.delay_ms, 0., 1000.).step(10.).fill()
///     .on_change(cx.listener(|this, value: &f32, _, cx| { this.delay_ms = *value; cx.notify() }))
///     .on_commit(cx.listener(|this, _: &f32, _, cx| this.save(cx)))
/// ```
#[derive(IntoElement)]
pub struct Slider {
    id: ElementId,
    value: f32,
    min: f32,
    max: f32,
    step: f32,
    fill: bool,
    ticks: Vec<f32>,
    wheel_step: Option<f32>,
    disabled: bool,
    on_change: Option<ValueHandler>,
    on_commit: Option<ValueHandler>,
}

impl Slider {
    pub fn new(id: impl Into<ElementId>, value: f32, min: f32, max: f32) -> Self {
        Self {
            id: id.into(),
            value,
            min,
            max,
            step: 1.,
            fill: false,
            ticks: Vec::new(),
            wheel_step: None,
            disabled: false,
            on_change: None,
            on_commit: None,
        }
    }

    /// The value snaps to multiples of `step` from `min` (0: continuous).
    pub fn step(mut self, step: f32) -> Self {
        self.step = step;
        self
    }

    /// Paint the run up to the thumb in the accent.
    pub fn fill(mut self) -> Self {
        self.fill = true;
        self
    }

    /// Tick marks at these values.
    pub fn ticks(mut self, ticks: impl IntoIterator<Item = f32>) -> Self {
        self.ticks = ticks.into_iter().collect();
        self
    }

    /// The wheel moves the value by `step` per event (up = more).
    pub fn wheel_step(mut self, step: f32) -> Self {
        self.wheel_step = Some(step);
        self
    }

    /// `opacity-60`, not interactive.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Every change, while dragging too.
    pub fn on_change(mut self, handler: impl Fn(&f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(handler));
        self
    }

    /// When a drag ends or a key is let go (the renderer saves settings there, not per pixel).
    pub fn on_commit(mut self, handler: impl Fn(&f32, &mut Window, &mut App) + 'static) -> Self {
        self.on_commit = Some(Rc::new(handler));
        self
    }
}

/// The value range and snapping, shared by the pointer, key and wheel paths.
#[derive(Clone, Copy)]
struct Range {
    min: f32,
    max: f32,
    step: f32,
}

impl Range {
    fn fraction(&self, value: f32) -> f32 {
        if self.max > self.min {
            ((value - self.min) / (self.max - self.min)).clamp(0., 1.)
        } else {
            0.
        }
    }

    fn snap(&self, value: f32) -> f32 {
        let value = value.clamp(self.min, self.max);
        if self.step > 0. {
            let steps = ((value - self.min) / self.step).round();
            (self.min + steps * self.step).clamp(self.min, self.max)
        } else {
            value
        }
    }

    /// The value under the pointer at `x` on a track at `bounds` (the thumb's centre runs from
    /// 7 px to width − 7 px).
    fn at(&self, x: Pixels, bounds: Bounds<Pixels>, grab: f32) -> f32 {
        let span = f32::from(bounds.size.width) - THUMB;
        let f = if span > 0. {
            (f32::from(x - bounds.left()) - grab - THUMB / 2.) / span
        } else {
            0.
        };
        self.snap(self.min + f.clamp(0., 1.) * (self.max - self.min))
    }

    fn key_step(&self) -> f32 {
        if self.step > 0. {
            self.step
        } else {
            (self.max - self.min) / 100.
        }
    }
}

impl RenderOnce for Slider {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let focus = focus_handle(window, cx, (self.id.clone(), "focus"), !self.disabled);
        let drag: Entity<Drag> = window.use_keyed_state((self.id.clone(), "drag"), cx, |_, _| Drag::default());
        let range = Range {
            min: self.min,
            max: self.max,
            step: self.step,
        };
        let value = range.snap(self.value);
        let f = range.fraction(value);
        let theme = cx.theme();
        let focused = focus.is_focused(window) && window.last_input_was_keyboard();
        let mut thumb_shadow = vec![
            BoxShadow::new(px(0.), px(0.), theme.color_accent_mid).blur_radius(px(3.)),
        ];
        if focused {
            thumb_shadow.extend(ring(cx));
        }
        let (track, accent, thumb_bg) =
            (theme.color_bg_lighter, theme.color_accent, theme.color_bg_light);
        let tick_color = theme.color_text_muted.opacity(0.4);
        let disabled = self.disabled;
        let on_change = self.on_change.clone();
        let on_commit = self.on_commit.clone();

        // Everything that follows the thumb is laid out in an inner box inset by its radius, so
        // `relative(f)` there is `f · (100% − 14px) + 7px` of the track.
        let inner = div()
            .absolute()
            .top_0()
            .bottom_0()
            .left(px(THUMB / 2.))
            .right(px(THUMB / 2.))
            .when(self.fill, |this| {
                this.child(
                    div()
                        .absolute()
                        .top_0()
                        .bottom_0()
                        .left(px(-THUMB / 2.))
                        .right(relative(1. - f))
                        .rounded_full()
                        .bg(accent),
                )
            })
            .children(self.ticks.iter().map(|&tick| {
                div()
                    .absolute()
                    .top(px(-2.))
                    .left(relative(range.fraction(tick)))
                    .ml(px(-0.5))
                    .w(px(1.))
                    .h_2()
                    .bg(tick_color)
            }))
            .child(
                div()
                    .absolute()
                    .top(px(-5.))
                    .left(relative(f))
                    .ml(px(-THUMB / 2.))
                    .size(px(THUMB))
                    .rounded_full()
                    .border_2()
                    .border_color(accent)
                    .bg(thumb_bg)
                    .shadow(thumb_shadow),
            );

        // The pointer: a 14 px band over the track, with the pointer captured while dragging.
        let pointer = {
            let drag = drag.clone();
            let wheel_step = self.wheel_step;
            canvas(
                |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
                move |bounds, hitbox, window, _| {
                    if disabled {
                        return;
                    }
                    window.set_cursor_style(gpui::CursorStyle::PointingHand, &hitbox);
                    let thumb_x = bounds.left() + px(THUMB / 2. + f * (f32::from(bounds.size.width) - THUMB));
                    {
                        let (drag, hitbox, on_change) = (drag.clone(), hitbox.clone(), on_change.clone());
                        window.on_mouse_event(move |event: &MouseDownEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble
                                || event.button != MouseButton::Left
                                || !hitbox.is_hovered(window)
                            {
                                return;
                            }
                            let offset = f32::from(event.position.x - thumb_x);
                            let grab = if offset.abs() <= THUMB / 2. { offset } else { 0. };
                            drag.update(cx, |drag, _| drag.grab = Some(grab));
                            window.capture_pointer(hitbox.id);
                            let next = range.at(event.position.x, bounds, grab);
                            if let Some(handler) = &on_change
                                && next != value
                            {
                                handler(&next, window, cx);
                            }
                            cx.stop_propagation();
                        });
                    }
                    {
                        let (drag, on_change) = (drag.clone(), on_change.clone());
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble {
                                return;
                            }
                            let Some(grab) = drag.read(cx).grab else {
                                return;
                            };
                            let next = range.at(event.position.x, bounds, grab);
                            if let Some(handler) = &on_change
                                && next != value
                            {
                                handler(&next, window, cx);
                            }
                        });
                    }
                    {
                        let (drag, on_commit) = (drag.clone(), on_commit.clone());
                        window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble || drag.read(cx).grab.is_none() {
                                return;
                            }
                            drag.update(cx, |drag, _| drag.grab = None);
                            window.release_pointer();
                            if let Some(handler) = &on_commit {
                                handler(&value, window, cx);
                            }
                        });
                    }
                    if let Some(step) = wheel_step {
                        let (hitbox, on_change) = (hitbox.clone(), on_change.clone());
                        window.on_mouse_event(move |event: &ScrollEvent, phase, window, cx| {
                            if phase != DispatchPhase::Bubble || !hitbox.should_handle_scroll(window) {
                                return;
                            }
                            let delta = event.delta.pixel_delta(px(20.)).y;
                            if delta == px(0.) {
                                return;
                            }
                            cx.stop_propagation();
                            let next = range.snap(value + if delta > px(0.) { step } else { -step });
                            if let Some(handler) = &on_change
                                && next != value
                            {
                                handler(&next, window, cx);
                            }
                        });
                    }
                },
            )
            .absolute()
            .top(px(-5.))
            .left_0()
            .right_0()
            .h(px(THUMB))
        };

        div()
            .id(self.id)
            .track_focus(&focus)
            .relative()
            .w_full()
            .h_1()
            .rounded_full()
            .bg(track)
            .when(disabled, |this| this.opacity(0.6).cursor_not_allowed())
            .when(!disabled, |this| {
                let on_change = self.on_change.clone();
                let on_commit = self.on_commit.clone();
                this.on_key_down(move |event, window, cx| {
                    let keystroke = &event.keystroke;
                    if keystroke.modifiers.modified() {
                        return;
                    }
                    let page = range.key_step().max((range.max - range.min) / 10.);
                    let next = match keystroke.key.as_str() {
                        "right" | "up" => value + range.key_step(),
                        "left" | "down" => value - range.key_step(),
                        "pageup" => value + page,
                        "pagedown" => value - page,
                        "home" => range.min,
                        "end" => range.max,
                        _ => return,
                    };
                    cx.stop_propagation();
                    let next = range.snap(next);
                    if let Some(handler) = &on_change
                        && next != value
                    {
                        handler(&next, window, cx);
                    }
                })
                .on_key_up(move |event, window, cx| {
                    if matches!(
                        event.keystroke.key.as_str(),
                        "right" | "up" | "left" | "down" | "pageup" | "pagedown" | "home" | "end"
                    ) && let Some(handler) = &on_commit
                    {
                        handler(&value, window, cx);
                    }
                })
            })
            .child(inner)
            .child(pointer)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{point, size};

    #[test]
    fn values_follow_the_thumb_centre_and_snap() {
        let range = Range {
            min: 0.,
            max: 100.,
            step: 10.,
        };
        let track = Bounds {
            origin: point(px(100.), px(0.)),
            size: size(px(114.), px(4.)),
        };
        // The thumb's centre runs from 107 to 207.
        assert_eq!(range.at(px(107.), track, 0.), 0.);
        assert_eq!(range.at(px(157.), track, 0.), 50.);
        assert_eq!(range.at(px(154.), track, 0.), 50.);
        assert_eq!(range.at(px(300.), track, 0.), 100.);
        // Grabbed 3 px right of its centre: the centre is 3 px left of the pointer.
        assert_eq!(range.at(px(160.), track, 3.), 50.);
        assert_eq!(range.snap(-5.), 0.);
        assert_eq!(range.fraction(25.), 0.25);
    }
}
