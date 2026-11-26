//! The anchored profile popout (`UserProfilePopup.tsx`): identity (the banner, the avatar that
//! opens the full profile, name / nickname, status, custom status), the live stream the person
//! runs on a community we're in, their roles in the community it was opened from, and the
//! quick-DM field. 300 px, placed at the anchor and kept 16 px inside the window
//! ([`crate::place::Clamped`]). A press outside or Escape closes it.
//!
//! It draws over everything while open: a transparent layer covers the window, so a press
//! outside the card closes it without reaching what is underneath (the renderer's `fixed
//! inset-0` container).

use std::sync::Arc;
use std::time::Duration;

use decibell_common::{StatusMessageText, UserAvatar, Watch, dot_presence, presence_of, status_message_of, want_member};
use decibell_model::live::voice::codecs::can_watch;
use decibell_model::live::voice::streams::StreamsChanged;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::overlays::UserPopup;
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FontRole, Label, StatusDot, title_tooltip};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, AppContext as _, ClickEvent, Context, Entity, FocusHandle,
    ImageSource, IntoElement, KeyDownEvent, MouseDownEvent, ObjectFit, Render, SharedString, Subscription, Task,
    Window, deferred, div, img, point, prelude::*, px,
};

use super::role_chips;
use crate::place::Clamped;
use crate::ui::{BoxLook, caps_label, input_box, person_gradient, status_pill};

pub const POPUP_WIDTH: f32 = 300.;
/// The streamer refreshes its poster every 15 s (`THUMBNAIL_INTERVAL_MS`): polling faster only
/// downloads the same picture again.
const THUMBNAIL_INTERVAL: Duration = Duration::from_secs(15);

pub struct ProfilePopup {
    open: Option<UserPopup>,
    /// Counts openings: keys the entrance and the field's state.
    opened: u64,
    input: Entity<InputState>,
    sending: bool,
    thumbnail: Option<Arc<gpui::Image>>,
    /// Re-fetches the poster while the popup shows a live person.
    thumbnail_task: Option<Task<()>>,
    focus: FocusHandle,
    /// The open person's community entities (roster, roles), re-subscribed per opening.
    community: Vec<Subscription>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl ProfilePopup {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let input = cx.new(|cx| InputState::single_line(window, cx));
        let mut watch = Watch::new();
        watch
            .people(cx, |this: &ProfilePopup, uid| this.open.as_ref().is_some_and(|o| o.uid == uid))
            .event(&model.live.voice.streams, cx, |this: &mut ProfilePopup, _: &StreamsChanged| this.open.is_some())
            .store_if(&model.live.voice.codecs, cx, |this: &mut ProfilePopup, _, _| this.open.is_some());
        let subscriptions = vec![
            cx.observe_in(&model.overlays, window, |this, overlays, window, cx| {
                let next = overlays.read(cx).profile_popup.clone();
                if next != this.open {
                    this.opened_changed(next, window, cx);
                }
            }),
            cx.subscribe_in(&input, window, |this, _, event: &InputEvent, window, cx| match event {
                InputEvent::Submit => this.send(cx),
                InputEvent::Escape => this.close(window, cx),
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }),
            // A stream starting or stopping while open: the poster follows.
            cx.subscribe(&model.live.voice.streams, |this, _, _: &StreamsChanged, cx| this.follow_stream(cx)),
        ];
        let mut this = Self {
            open: None,
            opened: 0,
            input,
            sending: false,
            thumbnail: None,
            thumbnail_task: None,
            focus: cx.focus_handle(),
            community: Vec::new(),
            _watch: watch,
            _subscriptions: subscriptions,
        };
        if let Some(open) = model.overlays.read(cx).profile_popup.clone() {
            this.opened_changed(Some(open), window, cx);
        }
        let entity = cx.entity();
        crate::hooks::remember(&entity, cx);
        this
    }

    fn opened_changed(&mut self, next: Option<UserPopup>, window: &mut Window, cx: &mut Context<Self>) {
        self.open = next;
        self.opened += 1;
        self.sending = false;
        self.thumbnail = None;
        self.thumbnail_task = None;
        self.community.clear();
        self.input.update(cx, |state, cx| state.clear(cx));
        if let Some(open) = self.open.clone() {
            let model = Model::global(cx).clone();
            let name = popup_name(&open, cx);
            self.input.update(cx, |state, cx| state.set_placeholder(format!("Message @{name}"), cx));
            if let Some(server) = open.server
                && let Some(community) = model.community(server, cx)
            {
                if community.roster.read(cx).member(open.uid).is_none() {
                    want_member(server, open.uid, cx);
                }
                self.community.push(cx.observe(&community.roster, |_, _, cx| cx.notify()));
                self.community.push(cx.observe(&community.roles, |_, _, cx| cx.notify()));
            }
            // The quick-DM field takes focus as the popup opens; without one the card does
            // (Escape then reaches it).
            let own = model.auth.read(cx).uid;
            if open.uid > 0 && open.uid != own {
                self.input.update(cx, |state, cx| state.focus(window, cx));
            } else {
                window.focus(&self.focus, cx);
            }
            self.follow_stream(cx);
        }
        cx.notify();
    }

    fn close(&mut self, _: &mut Window, cx: &mut Context<Self>) {
        let overlays = Model::global(cx).overlays.clone();
        overlays.update(cx, |o, cx| o.close_profile_popup(cx));
    }

    /// Poll the poster while the person streams (the renderer's interval), stop otherwise.
    fn follow_stream(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.clone() else { return };
        let model = Model::global(cx).clone();
        let Some(server) = model.live.voice.streams.read(cx).of_user(open.uid).map(|s| s.server) else {
            self.thumbnail_task = None;
            if self.thumbnail.take().is_some() {
                cx.notify();
            }
            return;
        };
        if self.thumbnail_task.is_some() {
            return;
        }
        let uid = open.uid;
        self.thumbnail_task = Some(cx.spawn(async move |this, cx| {
            loop {
                let (tx, rx) = futures::channel::oneshot::channel();
                cx.update(|cx| actions::voice::fetch_stream_thumbnail(server, uid, cx, move |jpeg, _| drop(tx.send(jpeg))));
                // Empty: the placeholder stays; the next tick tries again.
                if let Ok(Some(jpeg)) = rx.await {
                    let image = Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Jpeg, jpeg));
                    if this.update(cx, |this, cx| {
                        this.thumbnail = Some(image);
                        cx.notify();
                    })
                    .is_err()
                    {
                        return;
                    }
                }
                cx.background_executor().timer(THUMBNAIL_INTERVAL).await;
            }
        }));
    }

    /// The quick DM: sent through the DM actions (the bubble at once, the echo confirms it), then
    /// the conversation opens.
    fn send(&mut self, cx: &mut Context<Self>) {
        let Some(open) = self.open.clone() else { return };
        let text = self.input.read(cx).value().trim().to_string();
        if text.is_empty() || self.sending {
            return;
        }
        self.sending = true;
        let name = popup_name(&open, cx);
        // The conversation exists before the bubble lands in it.
        actions::dm::open_dm(open.uid, name, cx);
        actions::dm::send_dm(open.uid, &text, 0, cx);
        self.input.update(cx, |state, cx| state.clear(cx));
        let overlays = Model::global(cx).overlays.clone();
        overlays.update(cx, |o, cx| o.close_profile_popup(cx));
    }

    /// The live preview's click: open the stream's community, join its channel, watch it large.
    fn join_and_watch(uid: Uid, server: ServerId, channel: decibell_model::ChannelId, cx: &mut App) {
        let model = Model::global(cx).clone();
        model.overlays.update(cx, |o, cx| o.close_profile_popup(cx));
        if model.nav.read(cx).active_server != Some(server) {
            actions::servers::open_server(server, cx);
        }
        model.nav.update(cx, |nav, cx| nav.set_active_view(ActiveView::Voice, cx));
        let connected = model.live.voice.session.read(cx).is_connected_to(server, &channel);
        if connected {
            actions::voice::focus_stream(uid, cx);
            return;
        }
        let join = actions::voice::join_voice_channel(server, channel, cx);
        cx.spawn(async move |cx| match join.await {
            Ok(()) => cx.update(|cx| actions::voice::focus_stream(uid, cx)),
            Err(e) => log::error!("Join + watch failed: {e}"),
        })
        .detach();
    }

    fn stream_section(&self, uid: Uid, name: &SharedString, cx: &mut Context<Self>) -> Option<AnyElement> {
        let model = Model::global(cx).clone();
        let location = model.live.voice.streams.read(cx).of_user(uid).cloned()?;
        let channel_name = model
            .community(location.server, cx)
            .and_then(|c| c.channels.read(cx).get(&location.channel).map(|ch| ch.name.clone()))
            .unwrap_or_else(|| "voice".into());
        let watchable = can_watch(&location.info, &model.live.voice.codecs.read(cx).decode);
        let can = watchable.is_ok();
        let title: SharedString = match &watchable {
            Ok(()) => "Watch stream".into(),
            Err(reason) => reason.clone(),
        };
        let gradient = person_gradient(uid, name, cx);
        let t = cx.theme();
        let (divider, secondary, accent_bright, radius_sm) =
            (t.color_border_divider, t.color_text_secondary, t.color_accent_bright, t.radius_sm);
        let (server, channel) = (location.server, location.channel.clone());
        Some(
            div()
                .child(div().mx_4().my_3().h(px(1.)).bg(divider))
                .child(
                    div()
                        .mx_4()
                        .mb_3()
                        .child(
                            div()
                                .mb_2()
                                .flex()
                                .child(Label::new("Live in\u{a0}").size(11.).weight(500.).color(secondary))
                                .child(Label::new(SharedString::from(format!("#{channel_name}"))).size(11.).weight(500.).color(accent_bright)),
                        )
                        .child(
                            div()
                                .id("popup-stream")
                                .group("popup-stream")
                                .relative()
                                .w_full()
                                // `aspect-video` on the 268 px column.
                                .h(px((POPUP_WIDTH - 32.) * 9. / 16.))
                                .overflow_hidden()
                                .rounded(radius_sm)
                                .tooltip(title_tooltip(title))
                                .when(!can, |this| this.opacity(0.5).cursor_not_allowed())
                                .when(can, |this| {
                                    this.cursor_pointer().on_click(move |_: &ClickEvent, _, cx| {
                                        ProfilePopup::join_and_watch(uid, server, channel.clone(), cx)
                                    })
                                })
                                // `animate-pulse` under the poster: only while there is none (the
                                // renderer's pulses beneath a loaded one too, unseen).
                                .child({
                                    let placeholder = div().absolute().inset_0().bg(gradient);
                                    if self.thumbnail.is_none() && !cx.reduce_motion() {
                                        placeholder
                                            .with_animation(
                                                ("popup-stream-pulse", self.opened),
                                                Animation::new(Duration::from_secs(2)).repeat(),
                                                |el, t| {
                                                    // opacity 1 → 0.5 → 1, cubic-bezier(0.4, 0, 0.6, 1).
                                                    let e = decibell_ui::motion::CubicBezier::new(0.4, 0., 0.6, 1.).y(f64::from(if t < 0.5 { t * 2. } else { 2. - t * 2. })) as f32;
                                                    el.opacity(1. - 0.5 * e)
                                                },
                                            )
                                            .into_any_element()
                                    } else {
                                        placeholder.into_any_element()
                                    }
                                })
                                // The renderer's `CodecBadge` sits right of a zero-size box at the
                                // preview's top-left and is clipped away: it never shows (mirrored).
                                .children(self.thumbnail.clone().map(|image| {
                                    img(ImageSource::Image(image)).absolute().inset_0().size_full().object_fit(ObjectFit::Cover)
                                }))
                                .when(can, |this| {
                                    // Over video: literal colours in every theme (the CodecBadge / scrim rule).
                                    this.child(
                                        div()
                                            .absolute()
                                            .inset_0()
                                            .flex()
                                            .items_center()
                                            .justify_center()
                                            .invisible()
                                            .group_hover("popup-stream", |s| s.visible().bg(gpui::black().opacity(0.4)))
                                            .child(Label::new("WATCH STREAM").size(12.).weight(600.).color(gpui::white())),
                                    )
                                }),
                        ),
                )
                .into_any_element(),
        )
    }
}

/// The name the popup shows and messages: the users store's, the member row's as the fallback.
fn popup_name(open: &UserPopup, cx: &App) -> SharedString {
    let model = Model::global(cx);
    let fallback = open
        .server
        .and_then(|s| model.community(s, cx))
        .and_then(|c| c.roster.read(cx).member(open.uid).map(|m| m.username.clone()))
        .unwrap_or_else(|| SharedString::new_static(decibell_common::UNKNOWN_USER));
    decibell_common::user_name(open.uid, &fallback, cx)
}

impl Render for ProfilePopup {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let Some(open) = self.open.clone() else { return div().into_any_element() };
        let model = Model::global(cx).clone();
        let uid = open.uid;
        let own = model.auth.read(cx).uid;
        let (member, roles) = match open.server.and_then(|s| model.community(s, cx)) {
            Some(c) => (c.roster.read(cx).member(uid).cloned(), c.roles.read(cx).list().to_vec()),
            None => (None, Vec::new()),
        };
        let name = popup_name(&open, cx);
        let nickname = member.as_ref().map(|m| m.nickname.clone()).filter(|n| !n.is_empty());
        let presence = presence_of(uid, member.as_ref().is_some_and(|m| m.is_online), cx);
        let message = status_message_of(uid, cx);
        let stream = self.stream_section(uid, &name, cx);
        let quick_dm = (uid > 0 && uid != own).then(|| {
            let mut look = BoxLook::new(cx.theme().color_bg_mid, cx);
            look.py = 9.;
            look.size = 12.;
            look.font = FontRole::Sans;
            input_box(("popup-dm", self.opened), &self.input, look, window, cx)
        });
        let gradient = person_gradient(uid, &name, cx);
        let pill = status_pill(presence, FontRole::Sans, cx);
        crate::hooks::record("profile-popup-roles", || {
            let held = member.as_ref().map_or(Vec::new(), |m| m.role_ids.to_vec());
            let mut labels: Vec<String> = held.iter().filter_map(|id| roles.iter().find(|r| r.id == *id).map(|r| r.name.to_string())).collect();
            if member.as_ref().is_some_and(|m| m.is_owner) {
                labels.insert(0, "Owner".into());
            }
            labels
        });
        let chips = open.server.map(|_| role_chips(&roles, member.as_ref().map_or(&[][..], |m| &m.role_ids[..]), member.as_ref().is_some_and(|m| m.is_owner), cx));
        let roles_label = caps_label("Roles", 11., cx);
        let t = cx.theme();
        let (bg_light, border, primary, muted, divider, secondary) =
            (t.color_bg_light, t.color_border, t.color_text_primary, t.color_text_muted, t.color_border_divider, t.color_text_secondary);
        let avatar_radius = t.avatar_radius(px(64.));
        let shadow = t.shadow_modal.clone();
        let server = open.server;
        let sending = self.sending;
        let card = div()
            .id(("profile-popup", self.opened))
            .occlude()
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, e: &KeyDownEvent, window, cx| {
                if e.keystroke.key == "escape" {
                    this.close(window, cx);
                }
            }))
            .on_mouse_down(gpui::MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .relative()
            .w(px(POPUP_WIDTH))
            .max_h(window.viewport_size().height - px(32.))
            .overflow_hidden()
            .rounded(t.radius_lg)
            .border_1()
            .border_color(border)
            .bg(bg_light)
            .shadow(shadow)
            .child(div().h(px(60.)).bg(gradient))
            .child(
                div().relative().h_7().mx_4().child(
                    // The avatar is the door to the full profile; the 4 px ring cuts into the banner.
                    div()
                        .id("popup-avatar")
                        .group("popup-avatar")
                        .absolute()
                        .top(px(-36.))
                        .left_0()
                        .p_1()
                        .rounded(avatar_radius + px(4.))
                        .bg(bg_light)
                        .cursor_pointer()
                        .tooltip(title_tooltip("View full profile"))
                        .on_click(move |_: &ClickEvent, _, cx| {
                            let overlays = Model::global(cx).overlays.clone();
                            overlays.update(cx, |o, cx| o.open_user_profile(uid, server, cx));
                        })
                        .child(
                            div()
                                .relative()
                                .size(px(64.))
                                .child(UserAvatar::new(uid, name.clone(), 64.))
                                .child(
                                    // `bg-black/55` scrim with white type: the renderer's literals.
                                    div()
                                        .absolute()
                                        .inset_0()
                                        .flex()
                                        .flex_col()
                                        .items_center()
                                        .justify_center()
                                        .rounded(avatar_radius)
                                        .bg(gpui::black().opacity(0.55))
                                        .invisible()
                                        .group_hover("popup-avatar", |s| s.visible())
                                        .child(Label::new("VIEW").size(9.).line_height(11.).weight(600.).tracking(0.06).color(gpui::white()))
                                        .child(Label::new("PROFILE").size(9.).line_height(11.).weight(600.).tracking(0.06).color(gpui::white())),
                                )
                                .child(div().absolute().bottom(px(-1.)).right(px(-1.)).child(StatusDot::new(dot_presence(presence), bg_light).size(16.).ring_width(3.))),
                        ),
                ),
            )
            .child(
                div()
                    .px_4()
                    .pb_1()
                    .pt_3()
                    .child(Label::new(nickname.clone().unwrap_or_else(|| name.clone())).font(FontRole::Display).size(16.).weight(600.).color(primary))
                    .when(nickname.as_ref().is_some_and(|n| *n != name), |this| this.child(Label::new(name.clone()).size(12.).color(muted)))
                    .child(div().mt_1p5().flex().child(pill))
                    .when_some(message, |this, message| {
                        this.child(div().mt_2().child(StatusMessageText::new(message).font(FontRole::Sans).size(13.).line_height(13. * 1.375).color(secondary)))
                    }),
            )
            .children(stream)
            .when_some(chips, |this, chips| {
                this.child(div().mx_4().my_3().h(px(1.)).bg(divider)).child(
                    div()
                        .px_4()
                        .when(uid == own || uid < 0, |this| this.pb_4())
                        .child(div().mb_2().child(roles_label))
                        .child(chips),
                )
            })
            .when_some(quick_dm, |this, field| {
                this.child(div().mx_4().my_3().h(px(1.)).bg(divider))
                    .child(div().px_4().pb_4().when(sending, |this| this.opacity(0.5)).child(field))
            });
        let entrance = card.with_animation(
            ("profile-popup-in", self.opened),
            Animation::new(Duration::from_millis(250)).with_easing(decibell_ui::motion::EASE.easing()),
            |card, t| card.opacity(t).top(px(6. * (1. - t))),
        );
        let anchor = point(px(open.anchor.x), px(open.anchor.y));
        deferred(
            div()
                .id("profile-popup-layer")
                .absolute()
                .inset_0()
                .occlude()
                .on_mouse_down(gpui::MouseButton::Left, cx.listener(|this, _: &MouseDownEvent, window, cx| this.close(window, cx)))
                .on_mouse_down(gpui::MouseButton::Right, cx.listener(|this, _: &MouseDownEvent, window, cx| this.close(window, cx)))
                .child(Clamped::new(anchor).child(entrance)),
        )
        .with_priority(5)
        .into_any_element()
    }
}
