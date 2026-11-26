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
//! Not mirrored: the dashed ring around the hash turns forever in the renderer
//! (`welcome-spin-slow`, 20 s a turn); here it stands still, so an empty channel at rest draws
//! no frames.

use std::cell::Cell;
use std::rc::Rc;

use decibell_common::Watch;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Model, ServerId};
use decibell_ui::motion::{self, Transition};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label};
use gpui::{
    AnyView, App, AppContext as _, BoxShadow, ClickEvent, Context, IntoElement, Render, SharedString, Window, canvas, div,
    point, prelude::*, px,
};

use crate::follow::Follow;
use crate::ui;

/// `.welcome-state { max-width: 480px; padding: 40px 20px }`.
const MAX_WIDTH: f32 = 480.;
const PAD_X: f32 = 20.;
/// `animation: welcome-fadeUp 0.5s ease` (`translateY(16px)` → 0, opacity 0 → 1).
const FADE_UP: Transition = Transition::new(500, motion::EASE);

pub struct WelcomeBanner {
    server: ServerId,
    channel: ChannelId,
    /// The banner's width as last laid out: the paragraphs are centred line by line at it.
    width: Rc<Cell<f32>>,
    follow: Follow,
    _watch: Watch,
}

/// The chat pane's welcome slot.
pub fn build(server: ServerId, channel: ChannelId, _: &mut Window, cx: &mut App) -> AnyView {
    cx.new(|cx| WelcomeBanner::new(server, channel, cx)).into()
}

impl WelcomeBanner {
    pub fn new(server: ServerId, channel: ChannelId, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.communities, cx);
        let mut follow = Follow::default();
        follow.set(Some(server), cx);
        Self { server, channel, width: Rc::new(Cell::new(0.)), follow, _watch: watch }
    }
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

        let shown = motion::transition_from(window, cx, "welcome-fade-up", 0., 1., FADE_UP);
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
                        .child(
                            div()
                                .absolute()
                                .top(px(-6.))
                                .left(px(-6.))
                                .size(px(84.))
                                .rounded_full()
                                .border(px(1.5))
                                .border_dashed()
                                .border_color(accent.opacity(0.25)),
                        )
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
