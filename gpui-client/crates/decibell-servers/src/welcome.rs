//! The welcome banner (`features/chat/WelcomeState.tsx`): what an empty text channel shows in
//! place of its message list, centred in the pane (`ChatPanel`: `messages.length === 0` once the
//! first page is in). Invite friends (CREATE_INVITE or MANAGE_INVITES in the community) opens
//! `modal::INVITE_FRIENDS`; Manage channel (MANAGE_CHANNELS or MANAGE_ROLES in the channel) its
//! settings, where the topic is edited. The renderer has one banner for every text channel
//! (encrypted ones too) and none for DMs.
//!
//! Registered as `decibell_chat`'s welcome slot. It replaces the list rather than sitting at the
//! top of it, so the list's row heights and anchoring never see it: it goes as the first message
//! arrives.
//!
//! The dashed ring around the hash turns forever in the renderer (`welcome-spin-slow`, 20 s a
//! turn, `linear infinite`), which makes Chromium composite the window at the display's rate for
//! as long as an empty channel is open. Here it turns too, while the window is visible (not
//! minimised) and motion isn't reduced: the ring is one SVG sprite drawn once, turned by a timed
//! transformation that re-presents the retained scene at most [`SPIN_FPS`] times a second without
//! re-rendering anything (as the server bar's glow breathes, `decibell-shell/src/glow.rs`). Its
//! edge moves 13 px a second, so a step is under half a pixel. Hidden, it stands still and the
//! window presents nothing. `docs/electron-quirks.md` #37 has the cost.

use std::cell::Cell;
use std::rc::Rc;
use std::time::Duration;

use decibell_common::Watch;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_ui::motion::{self, Transition};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label};
use gpui::{
    AnyView, App, AppContext as _, BoxShadow, ClickEvent, Context, IntoElement, Render, SharedString, Subscription,
    TimedTransformation, Transformation, Window, canvas, div, point, prelude::*, px, svg,
};

use crate::follow::Follow;
use crate::ui;

/// `.welcome-state { max-width: 480px; padding: 40px 20px }`.
const MAX_WIDTH: f32 = 480.;
const PAD_X: f32 = 20.;
/// `animation: welcome-fadeUp 0.5s ease` (`translateY(16px)` → 0, opacity 0 → 1).
const FADE_UP: Transition = Transition::new(500, motion::EASE);
/// `.welcome-icon-wrap::after`: `inset: -6px` around the 72 px wrap, `border: 1.5px dashed`.
const RING_BOX: f32 = 84.;
const RING_WIDTH: f32 = 1.5;
/// One turn (`animation: welcome-spin-slow 20s linear infinite`).
pub const SPIN_PERIOD: Duration = Duration::from_secs(20);
/// How often the turning ring may present while it is the only thing moving. The ring's edge moves
/// 2π × 41.25 px / 20 s ≈ 13 px/s: at 30 fps a step is 0.43 px, under what the eye resolves on a
/// dashed line at 25 % opacity.
pub const SPIN_FPS: f32 = 30.;

/// The ring as an SVG (alpha only, tinted when drawn): a circle on the border's centre line, dashed
/// as GPUI's dashed border was before the ring turned (a dash twice the width, a gap of one width,
/// a whole number of periods around), so the ring at rest looks as it did.
pub fn ring_svg() -> Vec<u8> {
    let r = (RING_BOX - RING_WIDTH) / 2.;
    let circumference = std::f32::consts::TAU * r;
    let periods = (circumference / (3. * RING_WIDTH)).round().max(1.);
    let period = circumference / periods;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{b}" height="{b}" viewBox="0 0 {b} {b}"><circle cx="{c}" cy="{c}" r="{r}" fill="none" stroke="#fff" stroke-width="{w}" stroke-dasharray="{dash:.4} {gap:.4}"/></svg>"##,
        b = RING_BOX,
        c = RING_BOX / 2.,
        w = RING_WIDTH,
        dash = period * 2. / 3.,
        gap = period / 3.,
    )
    .into_bytes()
}

thread_local! {
    static RING: Rc<[u8]> = ring_svg().into();
}

pub struct WelcomeBanner {
    server: ServerId,
    channel: ChannelId,
    /// The banner's width as last laid out: the paragraphs are centred line by line at it.
    width: Rc<Cell<f32>>,
    follow: Follow,
    _watch: Watch,
    /// Whether the ring turned in the last render.
    turning: Rc<Cell<bool>>,
    /// The ring turns only while the window is visible: re-render when that changes it.
    _visibility: Subscription,
}

/// The chat pane's welcome slot.
pub fn build(server: ServerId, channel: ChannelId, window: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| WelcomeBanner::new(server, channel, window, cx)).into()
}

impl WelcomeBanner {
    pub fn new(server: ServerId, channel: ChannelId, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.communities, cx);
        let mut follow = Follow::default();
        follow.set(Some(server), cx);
        // A visibility event that doesn't start or stop the ring draws nothing.
        let visibility = cx.observe_window_visibility(window, |this: &mut Self, _, window, cx| {
            if ring_turns(window, cx) != this.turning.get() {
                log::debug!("[welcome] window visibility changed: the ring turns {}", !this.turning.get());
                cx.notify();
            }
        });
        let turning = Rc::new(Cell::new(ring_turns(window, cx)));
        Self { server, channel, width: Rc::new(Cell::new(0.)), follow, _watch: watch, turning, _visibility: visibility }
    }
}

/// Whether the ring turns: the window is visible and motion isn't reduced (captures reduce it).
pub fn ring_turns(window: &Window, cx: &App) -> bool {
    window.is_visible() && !cx.reduce_motion()
}

impl Render for WelcomeBanner {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        self.follow.set(Some(self.server), cx);
        let (server, channel) = (self.server, self.channel.clone());
        let name: SharedString = model
            .community(server, cx)
            .and_then(|c| c.channels.read(cx).get(&channel).map(|c| c.name.clone()))
            .unwrap_or_else(|| "channel".into());
        // Each action shows only with its permission.
        let invite = model.has_permission(server, perm::CREATE_INVITE, cx) || model.has_permission(server, perm::MANAGE_INVITES, cx);
        let manage = model.channel_permission(server, &channel, perm::MANAGE_CHANNELS, cx)
            || model.channel_permission(server, &channel, perm::MANAGE_ROLES, cx);
        decibell_harness::probes::flag("welcome-shown", true);
        decibell_harness::probes::flag("welcome-invite-shown", invite);
        decibell_harness::probes::flag("welcome-manage-shown", manage);

        let shown = motion::transition_from(window, cx, "welcome-fade-up", 0., 1., FADE_UP);
        let turns = ring_turns(window, cx);
        self.turning.set(turns);
        decibell_harness::probes::flag("welcome-ring-turns", turns);
        let width = match self.width.get() {
            w if w > 0. => w,
            _ => MAX_WIDTH,
        };
        let inner = (width - 2. * PAD_X).max(1.);
        let scale = window.scale_factor();
        let title_style = ui::label_style(FontRole::Display, 26., 39., 700, Some(-0.01), cx);
        let title_lines = ui::lines(&format!("Welcome to #{name}"), &title_style, inner, scale, cx);
        let sub_style = ui::text_style(FontRole::Sans, 14., 14. * 1.6, cx);
        let sub_lines = ui::lines(
            &format!("This is the start of the #{name} channel. Invite your squad and get the conversation going."),
            &sub_style,
            inner,
            scale,
            cx,
        );
        let t = cx.theme();
        let (accent, accent_hover, accent_soft, accent_bright, on_accent, bg_light, bg_lighter, muted, primary) = (
            t.color_accent,
            t.color_accent_hover,
            t.color_accent_soft,
            t.color_accent_bright,
            t.color_on_accent,
            t.color_bg_light,
            t.color_bg_lighter,
            t.color_text_muted,
            t.color_text_primary,
        );
        let radius = t.radius_md;
        let glow = |y: f32, blur: f32| BoxShadow {
            color: accent.opacity(0.22),
            offset: point(px(0.), px(y)),
            // CSS blur is twice GPUI's radius.
            blur_radius: px(blur / 2.),
            spread_radius: px(0.),
            inset: false,
        };
        let button = |id: &'static str, icon: IconName, label: &'static str, primary_button: bool| {
            let group = SharedString::new_static(id);
            div()
                .id(id)
                .group(group.clone())
                .relative()
                .children(decibell_shell::probes::probe(id))
                .flex()
                .items_center()
                .gap(px(7.))
                .px_4()
                .py(px(9.))
                .rounded(radius)
                .cursor_pointer()
                .map(|this| {
                    if primary_button {
                        this.bg(accent).shadow(vec![glow(2., 12.)]).hover(move |s| s.bg(accent_hover).shadow(vec![glow(4., 20.)]))
                    } else {
                        this.bg(bg_light).hover(move |s| s.bg(bg_lighter))
                    }
                })
                .child(if primary_button {
                    Icon::new(icon).size(px(15.)).color(on_accent)
                } else {
                    Icon::new(icon).size(px(15.)).color(muted).group_hover(group.clone(), primary)
                })
                .child(
                    div()
                        .text_color(if primary_button { on_accent } else { muted })
                        .when(!primary_button, |this| this.group_hover(group, move |s| s.text_color(primary)))
                        .child(Label::new(label).size(13.).line_height(19.5).weight(500.).nowrap()),
                )
        };
        let centred = |lines: Vec<String>, style: &dyn Fn(SharedString) -> Label| {
            div().flex().flex_col().items_center().children(lines.into_iter().map(|line| style(line.into())))
        };
        let measured = self.width.clone();
        let view = cx.entity_id();
        div()
            .relative()
            .w(px(MAX_WIDTH))
            .max_w_full()
            .px(px(PAD_X))
            .py(px(40.))
            .top(px(16. * (1. - shown)))
            .opacity(shown)
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = f32::from(bounds.size.width);
                        if (width - measured.get()).abs() > 0.5 {
                            measured.set(width);
                            cx.notify(view);
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .child(
                div().flex().justify_center().mb_5().child(
                    div()
                        .relative()
                        .flex()
                        .size(px(72.))
                        .items_center()
                        .justify_center()
                        .rounded_full()
                        .bg(accent_soft)
                        .child(div().absolute().top(px(-6.)).left(px(-6.)).size(px(RING_BOX)).child({
                            let ring = svg().data(&RING.with(Rc::clone)).size_full().text_color(accent.opacity(0.25));
                            if turns {
                                ring.with_timed_transformation(
                                    TimedTransformation::rotation(SPIN_PERIOD, Transformation::default()).with_max_fps(SPIN_FPS),
                                )
                            } else {
                                ring
                            }
                        }))
                        .child(Label::new("#").font(FontRole::Display).size(30.).line_height(45.).weight(700.).color(accent_bright)),
                ),
            )
            .child(div().mb_2().child(centred(title_lines, &|line| {
                Label::new(line).font(FontRole::Display).size(26.).line_height(39.).weight(700.).tracking(-0.01).color(primary).nowrap()
            })))
            .child(div().mb(px(28.)).child(centred(sub_lines, &|line| Label::new(line).size(14.).line_height(14. * 1.6).color(muted).nowrap())))
            .when(invite || manage, |this| {
                this.child(
                    div()
                        .flex()
                        .flex_wrap()
                        .justify_center()
                        .gap(px(10.))
                        .when(invite, |this| {
                            this.child(button("welcome-invite", IconName::LucideUserPlus, "Invite friends", true).on_click(|_: &ClickEvent, _, cx| {
                                let model = Model::global(cx).clone();
                                model.overlays.update(cx, |o, cx| o.open_modal(modal::INVITE_FRIENDS, cx));
                            }))
                        })
                        .when(manage, |this| {
                            let channel = channel.clone();
                            this.child(button("welcome-manage", IconName::LucideSettings, "Manage channel", false).on_click(move |_: &ClickEvent, _, cx| {
                                let model = Model::global(cx).clone();
                                model.overlays.update(cx, |o, cx| o.open_channel_settings(channel.clone(), cx));
                            }))
                        }),
                )
            })
    }
}
