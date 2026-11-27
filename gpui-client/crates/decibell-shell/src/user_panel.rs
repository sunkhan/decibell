//! The floating user panel at the foot of the sidebar group (`features/channels/UserPanel.tsx`,
//! `features/presence/StatusMenu.tsx`): our avatar, name and status (the status menu), mute /
//! deafen / settings, and while in voice the session's rows (channel, latency, streams, the
//! engine's error, Stream / Stop, Disconnect).
//!
//! The voice feature draws what the voice controls open, through [`crate::PanelAction`]
//! (`register_user_panel_action`): the connection-stats popover (the latency button), the Go Live
//! dialog (Stream), the stream-audio popover, the device menu on a right click of mute / deafen.
//! The update chip dispatches [`RestartToUpdate`]; the app's updater answers it.

use crate::ink::Ink;
use crate::slots::{PanelAction, user_panel_action};
use decibell_common::{BoundsCell, UserAvatar, Watch, dot_presence, status_line};
use decibell_model::live::calls::CallStatus;
use decibell_model::live::presence::{DisplayStatus, OwnStatus};
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::perm;
use decibell_model::stores::update::{UpdateMode, UpdateStatus};
use decibell_model::{Model, actions};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, Placed, Placement, Side, StatusDot, title_tooltip};
use gpui::{
    App, ClickEvent, Context, Entity, IntoElement, MouseButton, Pixels, Point, Render, SharedString, Window,
    canvas, deferred, div, point, prelude::*, px,
};

gpui::actions!(
    decibell_shell,
    [
        /// The user panel's update chip: restart to apply the downloaded update (`quitAndInstall`).
        RestartToUpdate
    ]
);

/// The panel's height as last laid out, for the sidebars' `.sidebar-above-panel` padding (the
/// renderer's `--user-panel-h`, published by a ResizeObserver). 56 px until measured.
pub struct PanelHeight(pub f32);

impl PanelHeight {
    pub const RESTING: f32 = 56.;

    /// `padding-bottom: calc(var(--user-panel-h, 56px) + 16px)`.
    pub fn list_padding(&self) -> Pixels {
        px(self.0 + 16.)
    }
}

pub struct UserPanel {
    height: Entity<PanelHeight>,
    /// The status menu, open at this point (window coordinates).
    status_menu: Option<Point<Pixels>>,
    identity_bounds: BoundsCell,
    /// The voice controls' bounds, for what they open ([`PanelAction`]).
    latency_bounds: BoundsCell,
    stream_bounds: BoundsCell,
    audio_bounds: BoundsCell,
    mute_bounds: BoundsCell,
    deafen_bounds: BoundsCell,
    /// What the panel shows of the voice stats: only the latency.
    latency: Option<u32>,
    _watch: Watch,
}

impl UserPanel {
    pub fn new(height: Entity<PanelHeight>, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let me = model.auth.read(cx).uid;
        let mut watch = Watch::new();
        watch
            .store(&model.auth, cx)
            .store(&voice.session, cx)
            .store(&voice.own_stream, cx)
            .store(&voice.streams, cx)
            .store(&model.live.calls, cx)
            .store(&model.update, cx)
            .store_if(&voice.stats, cx, |this, stats, _| std::mem::replace(&mut this.latency, stats.latency_ms) != stats.latency_ms)
            .event(&voice.speaking, cx, move |_, e: &decibell_model::live::voice::speaking::SpeakingChanged| e.uid == me || e.uid == 0)
            .people(cx, move |_, uid| uid == me)
            // The connected channel's name and our SPEAK there.
            .store(&model.communities, cx);
        let latency = voice.stats.read(cx).latency_ms;
        Self {
            height,
            status_menu: None,
            identity_bounds: BoundsCell::new(),
            latency_bounds: BoundsCell::new(),
            stream_bounds: BoundsCell::new(),
            audio_bounds: BoundsCell::new(),
            mute_bounds: BoundsCell::new(),
            deafen_bounds: BoundsCell::new(),
            latency,
            _watch: watch,
        }
    }

    fn toggle_status_menu(&mut self, at: Point<Pixels>, cx: &mut Context<Self>) {
        self.status_menu = if self.status_menu.is_some() { None } else { Some(at) };
        cx.notify();
    }

    fn close_status_menu(&mut self, cx: &mut Context<Self>) {
        if self.status_menu.take().is_some() {
            cx.notify();
        }
    }
}

/// `PanelButton`: 32 × 32 `rounded-sm`, danger `bg-error/15 text-error hover:bg-error/25`, active
/// `bg-accent-soft text-accent-bright`, else `text-text-secondary hover:bg-surface-hover
/// hover:text-text-primary`.
#[allow(clippy::too_many_arguments)] // the renderer's PanelButton props, one per argument
fn panel_button<F: Fn(&ClickEvent, &mut Window, &mut App) + 'static>(
    id: &'static str,
    icon: IconName,
    title: &'static str,
    danger: bool,
    active: bool,
    t: &Ink,
    right_click: Option<(PanelAction, BoundsCell)>,
    on_click: F,
) -> impl IntoElement + use<F> {
    let (bg, bg_hover, fg, fg_hover) = if danger {
        (t.color_error.opacity(0.15), t.color_error.opacity(0.25), t.color_error, t.color_error)
    } else if active {
        (t.color_accent_soft, t.color_accent_soft, t.color_accent_bright, t.color_accent_bright)
    } else {
        (gpui::transparent_black(), t.color_surface_hover, t.color_text_secondary, t.color_text_primary)
    };
    div()
        .id(id)
        .group(id)
        .flex()
        .flex_none()
        .size_8()
        .items_center()
        .justify_center()
        .rounded(t.radius_sm)
        .bg(bg)
        .hover(move |s| s.bg(bg_hover))
        .cursor_pointer()
        .tooltip(title_tooltip(title))
        .on_click(on_click)
        // `user-panel-settings` (the gear), `user-panel-mute`, …: where the self-tests click.
        .relative()
        .children(crate::probes::probe(id))
        .when_some(right_click, |this, (action, bounds)| {
            this.child(bounds.probe()).on_mouse_down(MouseButton::Right, move |_, window, cx| {
                cx.stop_propagation();
                user_panel_action(action, bounds.get(), window, cx);
            })
        })
        .child(Icon::new(icon).size(px(16.)).color(fg).group_hover(id, fg_hover))
}

impl Render for UserPanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let t = crate::ink::chrome(cx);
        let auth = model.auth.read(cx);
        let (me, username) = (auth.uid, auth.username.clone().unwrap_or_default());
        let voice = &model.live.voice;
        let session = voice.session.read(cx);
        let (connected, call_peer, muted, deafened, error) =
            (session.connected(), session.call_peer, session.muted, session.deafened, session.error.clone());
        let in_session = connected.is_some() || call_peer.is_some();
        // No SPEAK in the connected channel (permissions v3): the mic reads as muted.
        let speak_blocked =
            connected.as_ref().is_some_and(|(server, channel)| !model.channel_permission(*server, channel, perm::SPEAK, cx));
        let mic_off = muted || speak_blocked;
        let streaming = voice.own_stream.read(cx).is_live();
        // The stream-audio button shows while streaming with audio where apps can be picked.
        let audio_button = streaming
            && actions::voice::can_pick_stream_audio_apps()
            && decibell_model::live::voice::go_live::StreamSettings::of(model.settings.read(cx).get()).share_audio;
        let stream_count = voice.streams.read(cx).session(connected.as_ref(), call_peer.is_some()).len();
        let channel_name: Option<SharedString> = if let Some(peer) = call_peer {
            Some(format!("Call · {peer}").into())
        } else {
            connected.as_ref().map(|(server, channel)| {
                model
                    .community(*server, cx)
                    .and_then(|c| c.channels.read(cx).get(channel).map(|c| c.name.clone()))
                    .unwrap_or_else(|| "Voice".into())
            })
        };
        let update = model.update.read(cx);
        let chip = match (&update.status, update.mode) {
            (UpdateStatus::Downloaded { version }, UpdateMode::SelfUpdate) => Some(version.clone()),
            _ => None,
        };
        let own = model.live.presence.own.read(cx);
        let supported = own.supported;
        let status = own.display();
        let own_status = own.status;
        let message = own.message.clone();
        let speaking = voice.speaking.read(cx).is_speaking(me);

        // The update chip ("Update ready — <version>  RESTART"): restarting is the updater's
        // (`RestartToUpdate`, answered by the app). No pulse on its dot: a glow that animates for
        // days would draw frames at rest.
        let chip = chip.map(|version| {
            div()
                .id("update-chip")
                .cursor_pointer()
                .hover(|s| s.bg(t.color_accent_mid))
                .tooltip(title_tooltip(format!("Restart to update to {version}")))
                .on_click(|_, window, cx| window.dispatch_action(Box::new(RestartToUpdate), cx))
                .mb_2()
                .flex()
                .w_full()
                .items_center()
                .gap_2()
                .rounded(t.radius_sm)
                .bg(t.color_accent_soft)
                .px_2()
                .py_1p5()
                .child(div().size_1p5().rounded_full().bg(t.color_accent_bright))
                .child(
                    div().flex_1().min_w_0().child(
                        Label::new(SharedString::from(format!("Update ready — {version}")))
                            .size(12.)
                            .weight(500.)
                            .color(t.color_accent_bright)
                            .truncate(),
                    ),
                )
                .child(Label::new("RESTART").font(FontRole::Meta).size(cx.theme().scaled.text_micro).weight(600.).tracking(0.1).color(t.color_accent_bright))
        });

        let session_row = channel_name.map(|name| {
            let latency_color = match self.latency {
                Some(ms) if ms <= 70 => t.color_success,
                Some(ms) if ms < 175 => t.color_warning,
                Some(_) => t.color_error,
                None => t.color_success,
            };
            let latency: SharedString = match self.latency {
                Some(ms) => format!("{ms}ms").into(),
                None => "Connected".into(),
            };
            div()
                .mb_2()
                .flex()
                .items_center()
                .gap_1p5()
                .px_0p5()
                .child(
                    Icon::new(if call_peer.is_some() { IconName::ShellPhone } else { IconName::ShellVoiceMic })
                        .size(px(14.))
                        .color(t.color_text_muted),
                )
                .child(
                    Label::new(name)
                        .font(FontRole::Display)
                        .size(cx.theme().scaled.text_member)
                        .weight(cx.theme().palette.font_weight_emphasis)
                        .color(t.color_text_primary),
                )
                .when(stream_count > 0, |this| {
                    this.child(
                        div().rounded(t.radius_sm).bg(t.color_accent.opacity(0.12)).px_1p5().py_0p5().child(
                            Label::new(SharedString::from(format!(
                                "{stream_count} stream{}",
                                if stream_count > 1 { "s" } else { "" }
                            )))
                            .size(10.)
                            .weight(600.)
                            .color(t.color_accent),
                        ),
                    )
                })
                .child(
                    // The connection-stats popover opens here (voice feature).
                    div()
                        .id("user-panel-latency")
                        .relative()
                        .ml_auto()
                        .cursor_pointer()
                        .tooltip(title_tooltip("Click for connection stats"))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click({
                            let bounds = self.latency_bounds.clone();
                            move |_: &ClickEvent, window, cx| user_panel_action(PanelAction::Latency, bounds.get(), window, cx)
                        })
                        .child(self.latency_bounds.probe())
                        .child(Label::new(latency).size(11.).weight(500.).color(latency_color)),
                )
        });

        let error_row = error.filter(|_| in_session).map(|error| {
            div()
                .mb_2()
                .rounded(t.radius_sm)
                .bg(t.color_error.opacity(0.1))
                .px_2()
                .py_1()
                .child(Label::new(error).size(11.).color(t.color_error))
        });

        let (stream_bounds, audio_bounds) = (self.stream_bounds.clone(), self.audio_bounds.clone());
        let stream_row = (in_session && call_peer.is_none()).then(|| {
            let accent = t.color_accent;
            let (bg, bg_hover, border) = if streaming {
                (accent.opacity(0.12), accent.opacity(0.18), accent.opacity(0.25))
            } else {
                (accent.opacity(0.08), accent.opacity(0.15), accent.opacity(0.2))
            };
            div()
                .mb_2()
                .flex()
                .items_center()
                .gap_1p5()
                .child(
                    div()
                        .id("user-panel-stream")
                        .group("user-panel-stream")
                        .relative()
                        .flex()
                        .flex_1()
                        .h_8()
                        .items_center()
                        .justify_center()
                        .gap(px(6.))
                        .rounded(t.radius_md)
                        .border_1()
                        .border_color(border)
                        .bg(bg)
                        .hover(move |s| s.bg(bg_hover))
                        .cursor_pointer()
                        .tooltip(title_tooltip(if streaming { "Stop sharing" } else { "Share screen" }))
                        .child(stream_bounds.probe())
                        .on_click({
                            let bounds = stream_bounds.clone();
                            move |_: &ClickEvent, window, cx| {
                                if streaming {
                                    actions::voice::stop_stream(cx);
                                } else {
                                    user_panel_action(PanelAction::GoLive, bounds.get(), window, cx);
                                }
                            }
                        })
                        .child(
                            Icon::new(if streaming { IconName::StageStopShare } else { IconName::StageScreen })
                                .size(px(13.))
                                .color(accent),
                        )
                        .child(Label::new(if streaming { "Stop" } else { "Stream" }).size(12.).weight(500.).color(accent)),
                )
                .when(audio_button, |this| this.child(
                    // The stream-audio popover's button (voice feature).
                    div()
                        .id("user-panel-stream-audio")
                        .relative()
                        .flex()
                        .flex_none()
                        .cursor_pointer()
                        .tooltip(title_tooltip("Stream sound"))
                        .hover(move |s| s.bg(accent.opacity(0.15)))
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .on_click({
                            let bounds = audio_bounds.clone();
                            move |_: &ClickEvent, window, cx| user_panel_action(PanelAction::StreamAudio, bounds.get(), window, cx)
                        })
                        .child(audio_bounds.probe())
                        .size_8()
                        .items_center()
                        .justify_center()
                        .rounded(t.radius_md)
                        .border_1()
                        .border_color(accent.opacity(0.2))
                        .bg(accent.opacity(0.08))
                        .child(Icon::new(IconName::SettingsSpeaker).size(px(13.)).color(accent)),
                ))
        });

        // Our avatar, name and status line; a button opening the status menu where central
        // understands statuses.
        let avatar = div()
            .relative()
            .flex_none()
            .rounded(t.radius_md)
            // The speaking ring flips instantly (no transition), as in the renderer.
            .when(speaking, |this| this.shadow(vec![gpui::BoxShadow {
                color: t.color_success,
                offset: point(px(0.), px(0.)),
                blur_radius: px(0.),
                spread_radius: px(2.),
                inset: false,
            }]))
            .child(UserAvatar::new(me, username.clone(), 36.).chrome())
            .when(supported, |this| {
                this.child(
                    div().absolute().bottom(px(-1.)).right(px(-1.)).child(
                        StatusDot::new(dot_presence(status), t.color_bg_light).size(13.),
                    ),
                )
            });
        let status_text: SharedString = match &message {
            Some(message) => status_line(message),
            None => SharedString::new_static(status.label()),
        };
        let identity_body = div()
            .flex()
            .flex_1()
            .min_w_0()
            .items_center()
            .gap_2()
            .child(avatar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Label::new(username)
                            .font(FontRole::Channel)
                            .size(cx.theme().scaled.text_member)
                            .weight(cx.theme().palette.font_weight_emphasis)
                            .color(t.color_text_bright)
                            .truncate(),
                    )
                    .when(supported, |this| {
                        this.child(
                            Label::new(status_text)
                                .font(FontRole::Meta)
                                .size(cx.theme().scaled.text_micro)
                                .color(t.color_text_muted)
                                .truncate(),
                        )
                    }),
            );
        let identity = if supported {
            div()
                .id("user-panel-identity")
                .relative()
                .flex()
                .flex_1()
                .min_w_0()
                .my(px(-4.))
                .ml(px(-4.))
                .py_1()
                .pl_1()
                .rounded(t.radius_md)
                .hover(|s| s.bg(t.color_surface_hover))
                .cursor_pointer()
                .tooltip(title_tooltip("Set status"))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                    // `{ x: rect.left, y: rect.top - 6 }`, opening above.
                    let row = this.identity_bounds.get();
                    this.toggle_status_menu(point(row.left(), row.top() - px(6.)), cx);
                }))
                .child(self.identity_bounds.probe())
                .child(identity_body)
                .into_any_element()
        } else {
            identity_body.into_any_element()
        };

        let controls = div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .when(in_session, |this| {
                this.child(panel_button(
                    "user-panel-disconnect",
                    IconName::SettingsLogout,
                    if call_peer.is_some() { "Hang up" } else { "Disconnect" },
                    true,
                    false,
                    &t,
                    None,
                    move |_, _, cx| {
                        let in_call = Model::global(cx).live.calls.read(cx).status != CallStatus::Idle;
                        if in_call {
                            actions::calls::end_call(Some("Call ended"), true, cx);
                        } else {
                            actions::voice::leave_voice_channel(cx);
                        }
                    },
                ))
            })
            .child(panel_button(
                "user-panel-mute",
                if mic_off { IconName::VoiceMute } else { IconName::StageMic },
                if speak_blocked {
                    "You can't speak in this channel"
                } else if muted {
                    "Unmute"
                } else {
                    "Mute"
                },
                mic_off,
                mic_off,
                &t,
                Some((PanelAction::InputDevices, self.mute_bounds.clone())),
                |_, _, cx| actions::voice::toggle_mute(cx),
            ))
            .child(panel_button(
                "user-panel-deafen",
                if deafened { IconName::StageHeadphonesOff } else { IconName::StageHeadphones },
                if deafened { "Undeafen" } else { "Deafen" },
                deafened,
                deafened,
                &t,
                Some((PanelAction::OutputDevices, self.deafen_bounds.clone())),
                |_, _, cx| actions::voice::toggle_deafen(cx),
            ))
            .child(panel_button("user-panel-settings", IconName::SettingsGear, "Settings", false, false, &t, None, |_, _, cx| {
                let overlays = Model::global(cx).overlays.clone();
                overlays.update(cx, |o, cx| o.open_modal(modal::SETTINGS, cx));
            }));

        let height = self.height.clone();
        let menu = self.status_menu.map(|at| status_menu(at, own_status, message.clone(), cx));

        div()
            .relative()
            .w_full()
            .rounded(t.radius_lg)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_light)
            .px_3()
            .py_2p5()
            .shadow(cx.theme().chrome.shadow_float.clone())
            // Publish the panel's height for the lists above it (one extra frame when it changes).
            .child(
                canvas(
                    move |bounds, window, cx| {
                        let h = f32::from(bounds.size.height);
                        if (height.read(cx).0 - h).abs() >= 0.5 {
                            let height = height.clone();
                            window.defer(cx, move |_, cx| height.update(cx, |p, cx| {
                                p.0 = h;
                                cx.notify();
                            }));
                        }
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .children(chip)
            .children(session_row)
            .children(error_row)
            .children(stream_row)
            .child(div().flex().items_center().gap_2().child(identity).child(controls))
            .children(menu)
    }
}

const STATUS_OPTIONS: [(OwnStatus, &str, Option<&str>); 4] = [
    (OwnStatus::Online, "Online", None),
    (OwnStatus::Away, "Away", None),
    (OwnStatus::Dnd, "Do Not Disturb", Some("Mutes notifications")),
    (OwnStatus::Invisible, "Invisible", Some("Appear offline")),
];

/// The status menu (`StatusMenu`): the custom status row, then the four statuses with their
/// dot, label, hint and a tick on ours. `w-60 rounded-lg border bg-bg-secondary p-1
/// shadow-modal`, opening above the user panel.
fn status_menu(
    at: Point<Pixels>,
    current: OwnStatus,
    message: Option<decibell_model::live::presence::StatusMessage>,
    cx: &Context<UserPanel>,
) -> impl IntoElement + use<> {
    let t = crate::ink::chrome(cx);
    let close = cx.listener(|this, _: &gpui::MouseDownEvent, _, cx| this.close_status_menu(cx));
    let row = |id: &'static str| {
        div()
            .id(id)
            .flex()
            .w_full()
            .items_start()
            .gap_2p5()
            .rounded(t.radius_sm)
            .px_2p5()
            .py_2()
            .hover(|s| s.bg(t.color_surface_hover))
            .cursor_pointer()
    };
    let custom = match message {
        Some(message) => div()
            .id("status-menu-custom")
            .flex()
            .items_center()
            .gap_1()
            .rounded(t.radius_sm)
            .hover(|s| s.bg(t.color_surface_hover))
            .child(
                div()
                    .id("status-menu-edit")
                    .flex_1()
                    .min_w_0()
                    .py_2()
                    .pl_2p5()
                    .cursor_pointer()
                    .tooltip(title_tooltip("Edit custom status"))
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                        this.close_status_menu(cx);
                        let overlays = Model::global(cx).overlays.clone();
                        overlays.update(cx, |o, cx| o.open_modal(modal::CUSTOM_STATUS, cx));
                    }))
                    .child(Label::new(status_line(&message)).size(13.).color(t.color_text_primary).truncate()),
            )
            .child(
                div()
                    .id("status-menu-clear")
                    .group("status-menu-clear")
                    .mr_1()
                    .flex()
                    .flex_none()
                    .size_6()
                    .items_center()
                    .justify_center()
                    .rounded(t.radius_sm)
                    .hover(|s| s.bg(t.color_surface_active))
                    .cursor_pointer()
                    .tooltip(title_tooltip("Clear custom status"))
                    .on_click(|_: &ClickEvent, _, cx| actions::presence::save_status_message(None, 0, cx))
                    .child(
                        Icon::new(IconName::SettingsX)
                            .size(px(14.))
                            .stroke_width(2.5)
                            .color(t.color_text_muted)
                            .group_hover("status-menu-clear", t.color_text_primary),
                    ),
            )
            .into_any_element(),
        None => row("status-menu-set-custom")
            .items_center()
            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.close_status_menu(cx);
                let overlays = Model::global(cx).overlays.clone();
                overlays.update(cx, |o, cx| o.open_modal(modal::CUSTOM_STATUS, cx));
            }))
            .child(Icon::new(IconName::ShellSmile).size(px(14.)).color(t.color_text_muted))
            .child(Label::new("Set a custom status").size(13.).weight(500.).color(t.color_text_primary))
            .into_any_element(),
    };
    let options = STATUS_OPTIONS.iter().map(|(status, label, hint)| {
        let (status, selected) = (*status, *status == current);
        let shown = match status {
            OwnStatus::Online => DisplayStatus::Online,
            OwnStatus::Away => DisplayStatus::Away,
            OwnStatus::Dnd => DisplayStatus::Dnd,
            OwnStatus::Invisible => DisplayStatus::Invisible,
        };
        row(label)
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                if !selected {
                    actions::presence::choose_status(status, cx);
                }
                this.close_status_menu(cx);
            }))
            .child(div().mt_1().child(StatusDot::new(dot_presence(shown), t.color_bg_secondary).size(12.).ring_width(0.)))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(Label::new(*label).size(13.).weight(500.).color(t.color_text_primary))
                    .when_some(*hint, |this, hint| {
                        this.child(div().mt_0p5().child(Label::new(hint).size(12.).line_height(16.5).color(t.color_text_muted)))
                    }),
            )
            .when(selected, |this| {
                this.child(div().mt_0p5().child(Icon::new(IconName::Checkmark).size(px(14.)).stroke_width(2.5).color(t.color_accent)))
            })
    });
    let panel = div()
        .id("status-menu")
        .occlude()
        .w(px(240.))
        .rounded(t.radius_lg)
        .border_1()
        .border_color(t.color_border)
        .bg(t.color_bg_secondary)
        .p_1()
        .shadow(cx.theme().chrome.shadow_modal.clone())
        .on_mouse_down_out(close)
        .on_key_down(cx.listener(|this, event: &gpui::KeyDownEvent, _, cx| {
            if event.keystroke.key == "escape" {
                this.close_status_menu(cx);
            }
        }))
        .child(custom)
        .child(div().mx_1p5().my_1().h(px(1.)).bg(t.color_border_divider))
        .children(options);
    deferred(Placed::new(Placement::Menu { anchor: at, prefer: Side::Above }).child(panel)).with_priority(3)
}
