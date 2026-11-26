//! The stream volume on a scrim (`VolumeControl` in `stage/StageTiles.tsx`): a mute toggle, an
//! 84 px range (`custom-slider`: a 14 px accent thumb in a 2 px black ring, the run up to it in the
//! accent, the rest `white/22`), the percentage.

use std::rc::Rc;

use decibell_ui::{ActiveTheme as _, Icon, IconName, title_tooltip};
use gpui::{
    App, Bounds, ClickEvent, DispatchPhase, ElementId, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, RenderOnce, SharedString, Window, canvas, div, prelude::*, px,
};

use crate::scrim::{black, scrim_base, scrim_text, white};

type ChangeHandler = Rc<dyn Fn(u8, &mut Window, &mut App)>;
type ToggleHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// The track's width (`w-21`) and the thumb's diameter.
const TRACK: f32 = 84.;
const THUMB: f32 = 14.;

#[derive(IntoElement)]
pub struct VolumeControl {
    id: ElementId,
    volume: u8,
    on_change: Option<ChangeHandler>,
    on_toggle_mute: Option<ToggleHandler>,
}

impl VolumeControl {
    pub fn new(id: impl Into<ElementId>, volume: u8) -> Self {
        Self { id: id.into(), volume, on_change: None, on_toggle_mute: None }
    }

    pub fn on_change(mut self, f: impl Fn(u8, &mut Window, &mut App) + 'static) -> Self {
        self.on_change = Some(Rc::new(f));
        self
    }

    pub fn on_toggle_mute(mut self, f: impl Fn(&mut Window, &mut App) + 'static) -> Self {
        self.on_toggle_mute = Some(Rc::new(f));
        self
    }
}

/// The value under a pointer at `x` on a track laid out at `bounds` (Chromium keeps the thumb
/// inside the track: the run is the track minus a thumb).
fn value_at(x: Pixels, bounds: Bounds<Pixels>) -> u8 {
    let run = f32::from(bounds.size.width) - THUMB;
    let at = f32::from(x - bounds.left()) - THUMB / 2.;
    ((at / run).clamp(0., 1.) * 100.).round() as u8
}

impl RenderOnce for VolumeControl {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let dragging = window.use_keyed_state((self.id.clone(), "drag"), cx, |_, _| false);
        let theme = cx.theme();
        let volume = self.volume;
        let accent = theme.color_accent;
        let error = theme.color_error;
        let muted = volume == 0;
        let fill = volume as f32 / 100.;
        let on_change = self.on_change.clone();
        let track = {
            let on_change = on_change.clone();
            let dragging = dragging.clone();
            let thumb_x = fill * (TRACK - THUMB);
            div()
                .id((self.id.clone(), "track"))
                .relative()
                .flex_none()
                .w(px(TRACK))
                .h_1()
                .rounded_full()
                .bg(white(0.22))
                .cursor_pointer()
                .child(div().absolute().left_0().top_0().h_1().w(px(thumb_x + THUMB / 2.)).rounded_full().bg(accent))
                .child(
                    div()
                        .absolute()
                        .top(px(-5.))
                        .left(px(thumb_x))
                        .size(px(THUMB))
                        .rounded_full()
                        .border_2()
                        .border_color(black(1.))
                        .bg(accent),
                )
                .tooltip(title_tooltip(SharedString::from(format!("Stream volume: {volume}%"))))
                .child(
                    canvas(
                        |_, _, _| {},
                        move |bounds, _, window, _| {
                            let on_down = on_change.clone();
                            let dragging_down = dragging.clone();
                            window.on_mouse_event(move |e: &MouseDownEvent, phase, window, cx| {
                                if phase == DispatchPhase::Bubble
                                    && e.button == MouseButton::Left
                                    && bounds.dilate(px(5.)).contains(&e.position)
                                {
                                    cx.stop_propagation();
                                    dragging_down.update(cx, |d, cx| {
                                        *d = true;
                                        cx.notify();
                                    });
                                    if let Some(f) = &on_down {
                                        f(value_at(e.position.x, bounds), window, cx);
                                    }
                                }
                            });
                            let on_move = on_change.clone();
                            let dragging_move = dragging.clone();
                            window.on_mouse_event(move |e: &MouseMoveEvent, phase, window, cx| {
                                if phase == DispatchPhase::Bubble
                                    && *dragging_move.read(cx)
                                    && e.dragging()
                                    && let Some(f) = &on_move
                                {
                                    f(value_at(e.position.x, bounds), window, cx);
                                }
                            });
                            let dragging_up = dragging.clone();
                            window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                                if phase == DispatchPhase::Bubble && *dragging_up.read(cx) {
                                    dragging_up.update(cx, |d, cx| {
                                        *d = false;
                                        cx.notify();
                                    });
                                }
                            });
                        },
                    )
                    .absolute()
                    .size_full(),
                )
        };
        let toggle = self.on_toggle_mute.clone();
        scrim_base(cx)
            .id(self.id.clone())
            .h_8()
            .gap_2p5()
            .px_2p5()
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(|_: &ClickEvent, _, cx| cx.stop_propagation())
            .child(
                div()
                    .id((self.id.clone(), "mute"))
                    .cursor_pointer()
                    .tooltip(title_tooltip(if muted { "Unmute stream" } else { "Mute stream" }))
                    .on_click(move |_: &ClickEvent, window, cx| {
                        cx.stop_propagation();
                        if let Some(f) = &toggle {
                            f(window, cx);
                        }
                    })
                    .child(
                        Icon::new(if muted { IconName::StageSpeakerMuted } else { IconName::StageSpeaker })
                            .size(px(15.))
                            .color(if muted { error } else { white(1.) }),
                    ),
            )
            .child(track)
            .child(div().w_8().child(scrim_text(format!("{volume}%"), white(0.7), cx)))
    }
}
