//! The floating pop-out player (`MiniStreamPlayer.tsx`): while a stream is loaded (`pip`) and the
//! person is in any view but the voice view (or a DM call's own conversation), the stream floats
//! in a corner of the workspace. Drag it anywhere; on release it springs to the nearest corner
//! (or the corner it was flicked toward) with the renderer's spring; resize it from the grip on
//! the corner facing inward (16:9, 240–640 px wide); click it to go back to the stream; × stops
//! watching. The corner and width are remembered (`ui.json`).
//!
//! It shows the same pictures as every other view of the stream through its own viewer (one
//! decoder). Frames are drawn only while it moves (drag, spring, entrance); at rest the video
//! costs one present per picture.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use decibell_common::{NicknameWatch, Watch};
use decibell_model::stores::layout::{PIP_WIDTH_MAX, PIP_WIDTH_MIN, PipCorner};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{Model, Uid, actions};
use decibell_ui::motion::CubicBezier;
use decibell_ui::{ActiveTheme as _, Icon, IconName, Label, title_tooltip};
use decibell_video::{Fit, VideoView, Viewer};
use gpui::{
    Bounds, ClickEvent, Context, DispatchPhase, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent,
    Pixels, Point, Render, SharedString, Window, anchored, canvas, div, linear_color_stop, linear_gradient,
    point, prelude::*, px, size,
};

use crate::scrim::{black, red_400, red_500, white};
use crate::state::VoiceUi;

const MARGIN: f32 = 16.;
const ASPECT: f32 = 9. / 16.;
/// How far the pointer must move before a press counts as a drag (vs a click).
const DRAG_THRESHOLD: f32 = 4.;
/// The release spring: stiffness pulls toward the corner, retention keeps most of the frame's
/// velocity so a flick carries momentum and overshoots a touch.
const SPRING_STIFFNESS: f32 = 0.02;
const SPRING_RETENTION: f32 = 0.85;
/// A throw: a release faster than this (px per frame) picks the corner by direction.
const FLICK_SPEED: f32 = 7.;
const FLICK_AXIS_SHARE: f32 = 0.4;
const ENTRANCE: Duration = Duration::from_millis(340);
/// `cubic-bezier(0.34, 1.32, 0.64, 1)`: overshoots, so the mini bounces into place.
const ENTRANCE_EASE: CubicBezier = CubicBezier::new(0.34, 1.32, 0.64, 1.);

fn height_for(w: f32) -> f32 {
    (w * ASPECT).round()
}

/// The resting top-left of a corner (`cornerTopLeft`): `MARGIN` inside the workspace panel,
/// clamped so a small window can't push the box off screen.
fn corner_top_left(corner: PipCorner, w: f32, h: f32, panel: Bounds<Pixels>, window: Bounds<Pixels>) -> Point<f32> {
    let (top, left, right, bottom) =
        (f32::from(panel.top()), f32::from(panel.left()), f32::from(panel.right()), f32::from(panel.bottom()));
    let is_top = matches!(corner, PipCorner::TopLeft | PipCorner::TopRight);
    let is_left = matches!(corner, PipCorner::TopLeft | PipCorner::BottomLeft);
    let x = if is_left { left + MARGIN } else { right - w - MARGIN };
    let y = if is_top { top + MARGIN } else { bottom - h - MARGIN };
    let (wl, wt) = (f32::from(window.left()), f32::from(window.top()));
    let (ww, wh) = (f32::from(window.size.width), f32::from(window.size.height));
    point(x.min(wl + ww - w - MARGIN).max(wl + MARGIN), y.min(wt + wh - h - MARGIN).max(wt + MARGIN))
}

struct Drag {
    grab: Point<f32>,
    start: Point<f32>,
    pos: Point<f32>,
    v: Point<f32>,
    fling: Point<f32>,
    moved: bool,
}

struct Spring {
    pos: Point<f32>,
    v: Point<f32>,
    target: PipCorner,
}

struct Resize {
    width: f32,
}

pub struct MiniPlayer {
    viewer: Option<(Uid, Viewer)>,
    visible: bool,
    /// The main view's bounds (window coordinates), as last laid out.
    main: Rc<Cell<Bounds<Pixels>>>,
    drag: Option<Drag>,
    spring: Option<Spring>,
    resize: Option<Resize>,
    entrance: Option<(Instant, Bounds<Pixels>)>,
    /// The streamer's nickname in the voice session's community (the label).
    nickname: NicknameWatch,
    _subs: Vec<gpui::Subscription>,
    _watch: Watch,
}

impl MiniPlayer {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let subs = vec![
            cx.observe(&voice.watching, |this, _, cx| this.follow_now(cx)),
            cx.observe(&voice.streams, |this, _, cx| this.follow_now(cx)),
            cx.observe(&voice.session, |this, _, cx| this.follow_now(cx)),
            cx.observe(&model.nav, |this, _, cx| this.follow_now(cx)),
            cx.observe(&model.dm.list, |this, _, cx| this.follow_now(cx)),
        ];
        let mut watch = Watch::new();
        watch
            // The panel's top edge (banners) and the corner / width.
            .store(&model.connections, cx)
            .event(&model.layout, cx, |_, _: &decibell_model::stores::layout::LayoutEvent| true)
            .people(cx, |this: &Self, uid| this.viewer.as_ref().is_some_and(|(u, _)| *u == uid))
            .event(&crate::video_events::StreamStates::global(cx), cx, |this: &mut Self, e: &crate::video_events::StreamStateChanged| {
                this.viewer.as_ref().is_some_and(|(u, _)| *u == e.0)
            });
        let mut this = Self {
            viewer: None,
            visible: false,
            main: Rc::new(Cell::new(Bounds::default())),
            drag: None,
            spring: None,
            resize: None,
            entrance: None,
            nickname: NicknameWatch::default(),
            _subs: subs,
            _watch: watch,
        };
        this.follow_now(cx);
        this
    }

    /// Whether the mini shows, and the viewer it holds: the loaded stream, live, watched, outside
    /// the voice view and outside the call's own conversation.
    fn follow_now(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let voice = &model.live.voice;
        let (connected, call_peer) = {
            let s = voice.session.read(cx);
            (s.connected(), s.call_peer)
        };
        let live: Vec<Uid> =
            voice.streams.read(cx).session(connected.as_ref(), call_peer.is_some()).iter().map(|s| s.owner_uid).collect();
        let (pip, watched) = {
            let w = voice.watching.read(cx);
            (w.pip, w.list().to_vec())
        };
        let view = model.nav.read(cx).active_view;
        let dm_peer = model.dm.list.read(cx).active_peer;
        let stream = pip.filter(|u| watched.contains(u) && live.contains(u));
        let dm_shows_stream = call_peer.is_some() && view == ActiveView::Dm && dm_peer == call_peer;
        let visible = stream.is_some() && view != ActiveView::Voice && !dm_shows_stream;
        let uid = stream.filter(|_| visible);
        self.nickname.follow(voice.session.read(cx).server, uid.unwrap_or(0), cx);
        let changed = uid != self.viewer.as_ref().map(|(u, _)| *u) || visible != self.visible;
        if uid != self.viewer.as_ref().map(|(u, _)| *u) {
            self.viewer = uid.map(|u| (u, crate::players::viewer(u, cx)));
        }
        if visible && !self.visible {
            // Shrink in from where the full view's video was.
            let from = VoiceUi::try_global(cx).and_then(|ui| ui.read(cx).full_rect);
            self.entrance = from.filter(|_| !cx.reduce_motion()).map(|r| (Instant::now(), r));
        }
        if !visible {
            self.drag = None;
            self.spring = None;
            self.resize = None;
            self.entrance = None;
            if self.visible {
                // The full view made next grows out of where this was.
                if let Some(ui) = VoiceUi::try_global(cx) {
                    ui.update(cx, |u, _| u.mini_left());
                }
            }
        }
        if changed {
            cx.notify();
        }
        self.visible = visible;
    }

    fn panel(&self, cx: &gpui::App) -> Bounds<Pixels> {
        // The workspace panel (`[data-pip-content-row]`): below the banners and the server bar,
        // inside the 8 px chrome gutter.
        let model = Model::global(cx);
        let main = self.main.get();
        let reconnecting =
            model.connections.read(cx).central == decibell_model::stores::connections::CentralStatus::Reconnecting;
        let consent = model.settings.read(cx).get().crash_reporting_consent_shown;
        let top = (if reconnecting { 32. } else { 0. }) + (if consent { 0. } else { 36. }) + decibell_shell::BAR_HEIGHT;
        Bounds::new(
            point(main.left() + px(8.), main.top() + px(top)),
            size(main.size.width - px(16.), main.size.height - px(top + 8.)),
        )
    }

    fn expand(cx: &mut gpui::App) {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let (pip, call_peer, connected) = {
            let w = voice.watching.read(cx);
            let s = voice.session.read(cx);
            (w.pip, s.call_peer, s.connected())
        };
        if let Some(pip) = pip {
            voice.watching.update(cx, |w, cx| w.set_focused(Some(pip), cx));
        }
        if let Some(peer) = call_peer {
            model.dm.list.update(cx, |l, cx| l.set_active(Some(peer), cx));
            model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Dm, cx));
        } else {
            if let Some((server, _)) = connected
                && model.nav.read(cx).active_server != Some(server)
            {
                actions::servers::open_server(server, cx);
            }
            model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Voice, cx));
        }
    }

    fn close(uid: Uid, cx: &mut gpui::App) {
        crate::focus::stop_watching(uid, cx);
    }

    fn release(&mut self, w: f32, h: f32, window_bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        let Some(drag) = self.drag.take() else { return };
        if !drag.moved {
            Self::expand(cx);
            return;
        }
        let (wl, wt) = (f32::from(window_bounds.left()), f32::from(window_bounds.top()));
        let (ww, wh) = (f32::from(window_bounds.size.width), f32::from(window_bounds.size.height));
        let centre = point(drag.pos.x + w / 2., drag.pos.y + h / 2.);
        let speed = drag.fling.x.hypot(drag.fling.y);
        let throwing = speed > FLICK_SPEED;
        let right = if throwing && drag.fling.x.abs() > speed * FLICK_AXIS_SHARE { drag.fling.x > 0. } else { centre.x > wl + ww / 2. };
        let bottom = if throwing && drag.fling.y.abs() > speed * FLICK_AXIS_SHARE { drag.fling.y > 0. } else { centre.y > wt + wh / 2. };
        let target = match (bottom, right) {
            (false, false) => PipCorner::TopLeft,
            (false, true) => PipCorner::TopRight,
            (true, false) => PipCorner::BottomLeft,
            (true, true) => PipCorner::BottomRight,
        };
        self.spring = Some(Spring { pos: drag.pos, v: drag.v, target });
        cx.notify();
    }
}

impl Render for MiniPlayer {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("MiniPlayer");
        let main = self.main.clone();
        let measure = canvas(
            move |bounds, _, _| {
                main.set(bounds);
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let root = div().absolute().top_0().left_0().size_full().child(measure);
        decibell_shell::probes::flag("mini-player", self.visible && self.viewer.is_some());
        let Some((uid, viewer)) = self.viewer.clone() else { return root };
        if !self.visible {
            return root;
        }
        let model = Model::global(cx).clone();
        let prefs = model.layout.read(cx).prefs().clone();
        let corner = prefs.pip_corner;
        let width = self.resize.as_ref().map(|r| r.width).unwrap_or(prefs.pip_width as f32);
        let height = height_for(width);
        let window_bounds = decibell_ui::visible_window_bounds(window, cx);
        let panel = self.panel(cx);
        if self.main.get().size.width <= px(0.) {
            // Not laid out yet: place it next frame.
            window.request_animation_frame();
            return root;
        }
        let rest = corner_top_left(corner, width, height, panel, window_bounds);

        // Where it is this frame.
        let mut pos = rest;
        let mut box_size = (width, height);
        if let Some(drag) = &self.drag {
            pos = drag.pos;
        } else if let Some(spring) = &mut self.spring {
            let target = corner_top_left(spring.target, width, height, panel, window_bounds);
            spring.v.x = (spring.v.x + (target.x - spring.pos.x) * SPRING_STIFFNESS) * SPRING_RETENTION;
            spring.pos.x += spring.v.x;
            spring.v.y = (spring.v.y + (target.y - spring.pos.y) * SPRING_STIFFNESS) * SPRING_RETENTION;
            spring.pos.y += spring.v.y;
            let settled = (target.x - spring.pos.x).abs() < 0.4
                && spring.v.x.abs() < 0.4
                && (target.y - spring.pos.y).abs() < 0.4
                && spring.v.y.abs() < 0.4;
            if settled {
                let target_corner = spring.target;
                pos = target;
                self.spring = None;
                model.layout.update(cx, |l, cx| l.set_pip_corner(target_corner, cx));
            } else {
                pos = spring.pos;
                window.request_animation_frame();
            }
        } else if let Some((started, from)) = self.entrance {
            let t = (started.elapsed().as_secs_f32() / ENTRANCE.as_secs_f32()).min(1.);
            if t >= 1. {
                self.entrance = None;
            } else {
                let e = ENTRANCE_EASE.y(t as f64) as f32;
                let lerp = |a: f32, b: f32| a + (b - a) * e;
                pos = point(lerp(f32::from(from.left()), rest.x), lerp(f32::from(from.top()), rest.y));
                box_size = (lerp(f32::from(from.size.width), width), lerp(f32::from(from.size.height), height));
                window.request_animation_frame();
            }
        }
        if let Some(ui) = VoiceUi::try_global(cx) {
            let rect = Bounds::new(point(px(pos.x), px(pos.y)), size(px(box_size.0), px(box_size.1)));
            ui.update(cx, |u, _| u.record_mini_rect(rect));
        }

        let name = decibell_common::display_name(
            model.live.voice.session.read(cx).server,
            uid,
            &SharedString::default(),
            cx,
        );
        let state = crate::video_events::StreamStates::global(cx).read(cx).get(uid);
        let (has_picture, stalled) = (state.picture.is_some(), state.stalled);
        let theme = cx.theme();
        // The grip sits on the corner facing the screen interior (opposite the dock).
        let (grip_icon, grip_right, grip_bottom) = match corner {
            PipCorner::TopLeft => (IconName::VoiceGripBr, true, true),
            PipCorner::TopRight => (IconName::VoiceGripBl, false, true),
            PipCorner::BottomLeft => (IconName::VoiceGripTr, true, false),
            PipCorner::BottomRight => (IconName::VoiceGripTl, false, false),
        };

        // While dragging or resizing, the window's pointer events steer it.
        let this = cx.entity().downgrade();
        let tracking = self.drag.is_some() || self.resize.is_some();
        let track = tracking.then(|| {
            let this = this.clone();
            canvas(
                |_, _, _| {},
                move |_, _, window, _| {
                    let this_move = this.clone();
                    window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let _ = this_move.update(cx, |mini, cx| {
                            let p = point(f32::from(e.position.x), f32::from(e.position.y));
                            if let Some(drag) = &mut mini.drag {
                                let next = point(p.x - drag.grab.x, p.y - drag.grab.y);
                                drag.v = point(next.x - drag.pos.x, next.y - drag.pos.y);
                                drag.fling = point(drag.fling.x * 0.6 + drag.v.x * 0.4, drag.fling.y * 0.6 + drag.v.y * 0.4);
                                drag.pos = next;
                                if (p.x - drag.start.x).abs() > DRAG_THRESHOLD || (p.y - drag.start.y).abs() > DRAG_THRESHOLD {
                                    drag.moved = true;
                                }
                                cx.notify();
                            } else if mini.resize.is_some() {
                                let model = Model::global(cx).clone();
                                let corner = model.layout.read(cx).prefs().pip_corner;
                                let panel = mini.panel(cx);
                                let is_right = matches!(corner, PipCorner::TopRight | PipCorner::BottomRight);
                                let is_bottom = matches!(corner, PipCorner::BottomLeft | PipCorner::BottomRight);
                                let anchor_x = if is_right { f32::from(panel.right()) - MARGIN } else { f32::from(panel.left()) + MARGIN };
                                let anchor_y = if is_bottom { f32::from(panel.bottom()) - MARGIN } else { f32::from(panel.top()) + MARGIN };
                                let avail_w = if is_right {
                                    anchor_x - (f32::from(panel.left()) + MARGIN)
                                } else {
                                    f32::from(panel.right()) - MARGIN - anchor_x
                                };
                                let avail_h = if is_bottom {
                                    anchor_y - (f32::from(panel.top()) + MARGIN)
                                } else {
                                    f32::from(panel.bottom()) - MARGIN - anchor_y
                                };
                                let hi = (PIP_WIDTH_MIN as f32).max((PIP_WIDTH_MAX as f32).min(avail_w).min(avail_h / ASPECT));
                                let from_x = if is_right { anchor_x - p.x } else { p.x - anchor_x };
                                let from_y = (if is_bottom { anchor_y - p.y } else { p.y - anchor_y }) / ASPECT;
                                let w = from_x.max(from_y).max(PIP_WIDTH_MIN as f32).min(hi).round();
                                if let Some(r) = &mut mini.resize
                                    && r.width != w
                                {
                                    r.width = w;
                                    cx.notify();
                                }
                            }
                        });
                    });
                    let this_up = this.clone();
                    window.on_mouse_event(move |_: &MouseUpEvent, phase, window, cx| {
                        if phase != DispatchPhase::Bubble {
                            return;
                        }
                        let window_bounds = decibell_ui::visible_window_bounds(window, cx);
                        let _ = this_up.update(cx, |mini, cx| {
                            if let Some(resize) = mini.resize.take() {
                                let model = Model::global(cx).clone();
                                model.layout.update(cx, |l, cx| l.set_pip_width(resize.width, cx));
                                cx.notify();
                            } else if mini.drag.is_some() {
                                let width = Model::global(cx).layout.read(cx).prefs().pip_width as f32;
                                mini.release(width, height_for(width), window_bounds, cx);
                            }
                        });
                    });
                },
            )
            .absolute()
            .size_0()
        });

        let control = |id: &'static str, icon: IconName, title: &'static str, danger: bool| {
            let hover = if danger { theme.color_error } else { white(0.2) };
            div()
                .id(id)
                .rounded(theme.radius_sm)
                .bg(black(0.4))
                .hover(move |s| s.bg(hover))
                .p_1p5()
                .cursor_pointer()
                .tooltip(title_tooltip(title))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .child(Icon::new(icon).size(px(15.)).color(white(0.9)))
        };
        let mini = div()
            .id("mini-player")
            .group("mini-player")
            .occlude()
            .relative()
            .w(px(box_size.0))
            .h(px(box_size.1))
            .overflow_hidden()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(white(0.1))
            .bg(black(1.))
            .shadow(theme.shadow_modal.clone())
            .cursor(if self.drag.is_some() { gpui::CursorStyle::ClosedHand } else { gpui::CursorStyle::OpenHand })
            .tooltip(title_tooltip("Drag to move · click to return to the stream"))
            .on_mouse_down(
                MouseButton::Left,
                cx.listener(move |mini, e: &MouseDownEvent, _, cx| {
                    let p = point(f32::from(e.position.x), f32::from(e.position.y));
                    mini.spring = None;
                    mini.entrance = None;
                    mini.drag = Some(Drag {
                        grab: point(p.x - pos.x, p.y - pos.y),
                        start: p,
                        pos,
                        v: point(0., 0.),
                        fling: point(0., 0.),
                        moved: false,
                    });
                    cx.notify();
                }),
            )
            .children(decibell_shell::probes::probe("mini-player"))
            .child(div().absolute().inset_0().child(VideoView::new(&viewer).fit(Fit::Contain).rounded(theme.radius_lg - px(1.))))
            .when(!has_picture, |this| {
                this.child(
                    div()
                        .absolute()
                        .inset_0()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(decibell_ui::Spinner::new().size(px(32.)).color(theme.color_accent)),
                )
            })
            .when(has_picture && stalled, |this| {
                this.child(div().absolute().inset_0().rounded(theme.radius_lg - px(1.)).overflow_hidden().child(crate::scrim::reconnecting(cx)))
            })
            .child(
                div()
                    .absolute()
                    .top_0()
                    .left_0()
                    .right_0()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .bg(linear_gradient(180., linear_color_stop(black(0.7), 0.), linear_color_stop(black(0.), 1.)))
                    .child(div().min_w_0().child(Label::new(name).size(11.).weight(500.).color(white(0.9)).truncate()))
                    .child(
                        div()
                            .flex()
                            .flex_none()
                            .items_center()
                            .gap_1()
                            .child(control("mini-expand", IconName::CallTheater, "Return to stream", false).on_click(
                                |_: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    MiniPlayer::expand(cx);
                                },
                            ))
                            .child(control("mini-close", IconName::StageClose, "Stop watching", true).on_click(
                                move |_: &ClickEvent, _, cx| {
                                    cx.stop_propagation();
                                    MiniPlayer::close(uid, cx);
                                },
                            )),
                    ),
            )
            .child(
                div()
                    .absolute()
                    .bottom_1p5()
                    .left_1p5()
                    .flex()
                    .items_center()
                    .gap_1()
                    .rounded(theme.radius_sm)
                    .bg(black(0.55))
                    .px_1p5()
                    .py_0p5()
                    .child(div().size_1p5().rounded_full().bg(red_500()))
                    .child(Label::new("LIVE").size(9.).weight(600.).tracking(0.025).color(red_400())),
            )
            .child(
                div()
                    .id("mini-grip")
                    .absolute()
                    .when(grip_right, |this| this.right_0())
                    .when(!grip_right, |this| this.left_0())
                    .when(grip_bottom, |this| this.bottom_0())
                    .when(!grip_bottom, |this| this.top_0())
                    .flex()
                    .size_5()
                    .items_center()
                    .justify_center()
                    .opacity(0.6)
                    .group_hover("mini-player", |s| s.opacity(1.))
                    .cursor(if grip_right == grip_bottom { gpui::CursorStyle::ResizeUpLeftDownRight } else { gpui::CursorStyle::ResizeUpRightDownLeft })
                    .tooltip(title_tooltip("Drag to resize"))
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |mini, _: &MouseDownEvent, _, cx| {
                            cx.stop_propagation();
                            mini.spring = None;
                            mini.entrance = None;
                            mini.resize = Some(Resize { width });
                            cx.notify();
                        }),
                    )
                    .child(Icon::new(grip_icon).size(px(13.)).color(white(0.4)).group_hover("mini-player", white(1.))),
            )
            .children(track);
        root.child(decibell_ui::layer(5, anchored().position(point(px(pos.x), px(pos.y))).child(mini)))
    }
}
