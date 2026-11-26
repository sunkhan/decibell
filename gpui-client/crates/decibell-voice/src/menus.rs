//! The voice feature's menus and popovers:
//!
//! - The user menu's voice sections (`UserContextMenu.tsx`): local mute and the volume slider
//!   ([`user_menu_mute`], [`user_menu_volume`]) and voice moderation ([`user_menu_moderation`]:
//!   server mute / deafen, move to a voice channel, disconnect) gated exactly as the renderer gates
//!   it (permissions v3: the bits in the target's channel, the hierarchy's "manage", never
//!   ourselves or the owner). The menu itself is the people crate's (`decibell_home::menu`, one
//!   menu per right click); the app joins these sections into it at `order::VOICE` /
//!   `VOICE_MODERATION` (`registry::join_features`: neither crate depends on the other), with
//!   [`watch_user_menu`] for what they read and [`MenuKit`] for the host's row look.
//! - [`PanelPopovers`]: what the user panel's voice controls open — the connection-stats popover
//!   (`ConnectionStatsPopover.tsx`, the ping / loss graph drawn as paths), the device menu on a
//!   right click of mute / deafen (`DeviceContextMenu.tsx`), the stream-audio app picker
//!   (`StreamAudioPopover.tsx`).

use std::time::{Duration, Instant};

use decibell_common::Watch;
use decibell_model::live::voice::gain::{self, DEFAULT_DB, MAX_DB, MIN_DB};
use decibell_model::live::voice::stats::StatsSample;
use decibell_model::stores::community::ChannelKind;
use decibell_model::stores::permissions::{Hierarchy, has_bits, perm};
use decibell_model::{ChannelId, Model, ServerId, Uid, actions};
use decibell_ui::{ActiveTheme as _, DismissEvent, Icon, IconName, Label, Placed, Placement, Popover, Side, Slider};
use gpui::{
    AnyElement, App, Bounds, ClickEvent, Context, DispatchPhase, Div, Entity, IntoElement, KeyDownEvent, PathBuilder, Pixels,
    Render, SharedString, Stateful, Subscription, Task, Window, canvas, deferred, div, linear_color_stop, linear_gradient, point,
    prelude::*, px,
};

use crate::state::{PanelPopover, VoiceUi};

// ── the user context menu's voice sections ─────────────────────────────────────────────

/// The row look of the menu that hosts the voice sections (the people crate's user menu: the app
/// hands its builders in, `registry::join_features`, so neither crate depends on the other).
#[derive(Clone, Copy)]
pub struct MenuKit {
    /// `flex w-full items-center rounded-md px-2.5 py-[7px] text-[13px] text-text-primary
    /// hover:bg-surface-hover`.
    pub row: fn(SharedString, SharedString, &App) -> Stateful<Div>,
    /// The same in `text-error hover:bg-error/10`.
    pub danger: fn(SharedString, SharedString, &App) -> Stateful<Div>,
    /// A section heading ("MODERATION").
    pub label: fn(&str, &App) -> AnyElement,
    /// Close the menu (a row's action ends with it).
    pub close: fn(&mut App),
}

/// What the voice sections keep while the menu is open: "Move to…" unfolded (a dropdown nested
/// in the menu would be a second floating layer).
#[derive(Default)]
pub struct VoiceMenuState {
    move_open: bool,
}

struct VoiceMenuGlobal(Entity<VoiceMenuState>);

impl gpui::Global for VoiceMenuGlobal {}

fn menu_state(cx: &mut App) -> Entity<VoiceMenuState> {
    if let Some(state) = cx.try_global::<VoiceMenuGlobal>() {
        return state.0.clone();
    }
    let state = cx.new(|_| VoiceMenuState::default());
    cx.set_global(VoiceMenuGlobal(state.clone()));
    state
}

/// The stores the voice sections read, observed while the menu is open (the menu calls this when it
/// opens: a new menu starts with "Move to…" folded). Each change redraws the menu.
pub fn watch_user_menu<T: 'static>(cx: &mut Context<T>) -> Vec<Subscription> {
    let model = Model::global(cx).clone();
    let state = menu_state(cx);
    state.update(cx, |s, _| s.move_open = false);
    let mut subscriptions = vec![
        // The volume, local mute (settings); who is in which voice channel and their server
        // mute / deafen (rosters); the moderation gate (channels, roles, roster, ownership).
        cx.observe(&model.settings, |_, _, cx| cx.notify()),
        cx.observe(&model.live.voice.rosters, |_, _, cx| cx.notify()),
        cx.observe(&state, |_, _, cx| cx.notify()),
    ];
    if let Some(open) = model.overlays.read(cx).context_menu.clone()
        && let Some(community) = open.server.and_then(|server| model.community(server, cx))
    {
        subscriptions.push(cx.observe(&community.channels, |_, _, cx| cx.notify()));
    }
    subscriptions
}

/// Local mute (`px-[5px] py-1` around one row): none for ourselves.
pub fn user_menu_mute(uid: Uid, own: bool, cx: &App) -> Option<AnyElement> {
    if own {
        return None;
    }
    let settings = Model::global(cx).settings.read(cx).get();
    let locally_muted = gain::locally_muted(settings, uid);
    let theme = cx.theme();
    let (fg, bg) = if locally_muted { (theme.color_error, theme.color_error.opacity(0.1)) } else { (theme.color_text_secondary, gpui::transparent_black()) };
    let (hover_bg, hover_fg) = (theme.color_surface_hover, theme.color_text_primary);
    let row = div()
        .id("user-menu-mute")
        .relative()
        .children(decibell_shell::probes::probe("user-menu:user-menu-mute"))
        .flex()
        .w_full()
        .items_center()
        .gap_2()
        .rounded(theme.radius_md)
        .bg(bg)
        .px_2p5()
        .py_2()
        .when(!locally_muted, |this| this.hover(move |s| s.bg(hover_bg).text_color(hover_fg)))
        .text_color(fg)
        .cursor_pointer()
        .on_click(move |_: &ClickEvent, _, cx| actions::voice::toggle_local_mute(uid, cx))
        .child(
            Icon::new(if locally_muted { IconName::StageSpeaker } else { IconName::StageSpeakerMuted })
                .size(px(16.))
                .color(if locally_muted { theme.color_error } else { theme.color_text_muted }),
        )
        .child(Label::new(if locally_muted { "Unmute" } else { "Mute" }).size(13.).weight(500.));
    Some(div().px(px(5.)).py_1().child(row).into_any_element())
}

/// "USER VOLUME" with Reset, the slider (0–100 over MIN_DB..MAX_DB, a tick at 0 dB, within
/// 0.8 dB of 0 snaps to it; every move applies at once) and the dB / percent readout
/// (`px-3.5 py-3`): none for ourselves.
pub fn user_menu_volume(uid: Uid, own: bool, cx: &App) -> Option<AnyElement> {
    if own {
        return None;
    }
    let db = gain::user_db(Model::global(cx).settings.read(cx).get(), uid).unwrap_or(DEFAULT_DB);
    let theme = cx.theme();
    let value = ((db - MIN_DB) / (MAX_DB - MIN_DB) * 100.) as f32;
    let zero = ((0. - MIN_DB) / (MAX_DB - MIN_DB) * 100.) as f32;
    let (accent, accent_bright) = (theme.color_accent, theme.color_accent_bright);
    Some(
        div()
            .px_3p5()
            .py_3()
            .child(
                div()
                    .mb_2p5()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Label::new("USER VOLUME").size(11.).weight(600.).tracking(0.07).color(theme.color_text_muted))
                    .child(
                        div()
                            .id("user-menu-volume-reset")
                            .cursor_pointer()
                            .text_color(accent_bright)
                            .hover(move |s| s.text_color(accent))
                            .tooltip(decibell_ui::title_tooltip("Reset to 0 dB"))
                            .on_click(move |_: &ClickEvent, _, cx| actions::voice::set_user_volume(uid, DEFAULT_DB, cx))
                            .child(Label::new("Reset").size(11.).weight(500.)),
                    ),
            )
            .child(
                div()
                    .relative()
                    .children(decibell_shell::probes::probe("user-menu:user-menu-volume"))
                    .mb_2()
                    .py(px(5.))
                    .child(Slider::new("user-menu-volume", value, 0., 100.).ticks([zero]).on_change(move |v, _, cx| {
                        let db = MIN_DB + (*v as f64 / 100.) * (MAX_DB - MIN_DB);
                        let db = if db.abs() < 0.8 { 0. } else { (db * 10.).round() / 10. };
                        actions::voice::set_user_volume(uid, db.clamp(MIN_DB, MAX_DB), cx);
                    })),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .child(Label::new(SharedString::from(gain::format_db(db))).size(11.).weight(500.).color(theme.color_text_primary))
                    .child(Label::new(SharedString::from(gain::db_to_percent(db))).size(11.).color(theme.color_text_muted)),
            )
            .into_any_element(),
    )
}

/// "Moderation" (`border-t … px-[5px] py-1`): server mute / deafen, Move to… (unfolding the voice
/// channels in place), Disconnect from voice, gated as the renderer gates them
/// ([`voice_moderation`]). Each action closes the menu (`runVoiceMod`).
pub fn user_menu_moderation(uid: Uid, server: Option<ServerId>, kit: MenuKit, cx: &App) -> Option<AnyElement> {
    let server = server?;
    let m = voice_moderation(server, uid, cx)?;
    let move_open = cx.try_global::<VoiceMenuGlobal>().is_some_and(|s| s.0.read(cx).move_open);
    let close = kit.close;
    let run = move |action: &'static str, channel: Option<ChannelId>| {
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            actions::voice::voice_mod(server, uid, action, channel.clone(), cx);
            close(cx);
        }
    };
    let theme = cx.theme();
    let (muted, accent, divider, secondary, primary, hover) =
        (theme.color_text_muted, theme.color_accent, theme.color_border_divider, theme.color_text_secondary, theme.color_text_primary, theme.color_surface_hover);
    let radius = theme.radius_md;
    let mut rows: Vec<AnyElement> = vec![(kit.label)("Moderation", cx)];
    if m.can_mute {
        let (label, action) = if m.server_muted { ("Server unmute", "server_unmute") } else { ("Server mute", "server_mute") };
        rows.push((kit.row)("menu-voice-mute".into(), label.into(), cx).on_click(run(action, None)).into_any_element());
    }
    if m.can_deafen {
        let (label, action) = if m.server_deafened { ("Server undeafen", "server_undeafen") } else { ("Server deafen", "server_deafen") };
        rows.push((kit.row)("menu-voice-deafen".into(), label.into(), cx).on_click(run(action, None)).into_any_element());
    }
    if m.can_move && !m.move_targets.is_empty() {
        rows.push(
            (kit.row)("menu-voice-move".into(), "Move to…".into(), cx)
                .child(Icon::new(IconName::ChevronRight).size(px(12.)).stroke_width(2.5).color(if move_open { accent } else { muted }))
                .on_click(|_: &ClickEvent, _, cx| {
                    let state = menu_state(cx);
                    state.update(cx, |s, cx| {
                        s.move_open = !s.move_open;
                        cx.notify();
                    });
                })
                .into_any_element(),
        );
        if move_open {
            let mut list = div().id("menu-voice-move-list").mb_1().ml_2p5().max_h(px(192.)).overflow_y_scroll().border_l_1().border_color(divider).pl_1();
            for (channel, name) in m.move_targets {
                let id = SharedString::from(format!("menu-voice-move:{channel}"));
                list = list.child(
                    div()
                        .id(gpui::ElementId::Name(id.clone()))
                        .relative()
                        .children(decibell_shell::probes::probe(format!("user-menu:{id}")))
                        .flex()
                        .w_full()
                        .items_center()
                        .gap_2()
                        .rounded(radius)
                        .px_2()
                        .py_1p5()
                        .cursor_pointer()
                        .text_color(secondary)
                        .hover(move |s| s.bg(hover).text_color(primary))
                        .on_click(run("move", Some(channel.clone())))
                        .child(Icon::new(IconName::SettingsSpeaker).size(px(12.)).color(muted))
                        .child(div().flex_1().min_w_0().child(Label::new(name).size(13.).truncate())),
                );
            }
            rows.push(list.into_any_element());
        }
    }
    if m.can_move {
        rows.push(div().mt_1().child((kit.danger)("menu-voice-disconnect".into(), "Disconnect from voice".into(), cx).on_click(run("disconnect", None))).into_any_element());
    }
    Some(div().px(px(5.)).py_1().children(rows).into_any_element())
}

/// What voice moderation the menu offers for `uid` in `server` (`showVoiceMod` and its parts).
#[derive(Debug, Clone, PartialEq)]
pub struct VoiceModeration {
    pub channel: ChannelId,
    pub server_muted: bool,
    pub server_deafened: bool,
    pub can_mute: bool,
    pub can_deafen: bool,
    pub can_move: bool,
    /// Every other voice channel we can see (the target needs nothing there: a voice pass).
    pub move_targets: Vec<(ChannelId, SharedString)>,
}

/// The renderer's gating, exactly: the target is in voice on this community, isn't us or the
/// owner, we may manage them, and we hold at least one of MUTE / DEAFEN / MOVE in their channel.
pub fn voice_moderation(server: ServerId, uid: Uid, cx: &App) -> Option<VoiceModeration> {
    let model = Model::global(cx);
    let me = model.auth.read(cx).uid;
    let community = model.community(server, cx)?;
    let owner = community.meta.read(cx).owner;
    if uid == me || uid == owner {
        return None;
    }
    let channels = community.channels.read(cx);
    let voice_channels: Vec<_> = channels.list().iter().filter(|c| c.kind == ChannelKind::Voice).collect();
    let rosters = model.live.voice.rosters.read(cx);
    let target = voice_channels.iter().find(|c| rosters.roster(&(server, c.id.clone())).contains(&uid))?;
    let state = rosters.channel(&(server, target.id.clone())).map(|c| c.state(uid)).unwrap_or_default();
    let roles = community.roles.read(cx);
    let roster = community.roster.read(cx);
    let hierarchy = Hierarchy::new(me, owner, roles.list(), Some(roster.members()));
    let holds = |bit: u64| hierarchy.is_owner || has_bits(target.my_permissions, bit);
    let (can_mute, can_deafen, can_move) = (holds(perm::MUTE_MEMBERS), holds(perm::DEAFEN_MEMBERS), holds(perm::MOVE_MEMBERS));
    if !hierarchy.can_manage(uid) || !(can_mute || can_deafen || can_move) {
        return None;
    }
    Some(VoiceModeration {
        channel: target.id.clone(),
        server_muted: state.server_muted,
        server_deafened: state.server_deafened,
        can_mute,
        can_deafen,
        can_move,
        move_targets: voice_channels.iter().filter(|c| c.id != target.id).map(|c| (c.id.clone(), c.name.clone())).collect(),
    })
}

// ── the user panel's popovers ───────────────────────────────────────────────────────────

pub struct PanelPopovers {
    ui: Entity<VoiceUi>,
    apps_timer: Option<Task<()>>,
    /// The device menu is open: the model's device lists are watched (read again as it opens,
    /// then on every device change the core reports; nothing while it is closed).
    devices_watched: bool,
    _watch: Watch,
    _subscriptions: Vec<gpui::Subscription>,
}

impl PanelPopovers {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let ui = VoiceUi::global(cx);
        let mut watch = Watch::new();
        watch
            .store(&ui, cx)
            .store_if(&model.live.voice.stats, cx, |this: &mut Self, _, cx| matches!(this.ui.read(cx).popover, Some(PanelPopover::Stats(_))))
            .store_if(&model.live.voice.devices, cx, |this: &mut Self, _, cx| matches!(this.ui.read(cx).popover, Some(PanelPopover::Devices { .. })))
            .store_if(&model.settings, cx, |this: &mut Self, _, cx| this.ui.read(cx).popover.is_some())
            .store_if(&model.live.voice.stream_audio_apps, cx, |this: &mut Self, _, cx| {
                matches!(this.ui.read(cx).popover, Some(PanelPopover::StreamAudio(_)))
            })
            .store_if(&model.live.voice.own_stream, cx, |this: &mut Self, own, cx| {
                // The stream ended: its sound picker goes.
                if !own.is_live() && matches!(this.ui.read(cx).popover, Some(PanelPopover::StreamAudio(_))) {
                    let ui = this.ui.clone();
                    let _ = ui;
                }
                true
            });
        let subscriptions = vec![
            cx.observe(&ui, |this: &mut Self, ui, cx| {
                let open = matches!(ui.read(cx).popover, Some(PanelPopover::Devices { .. }));
                this.watch_devices(open, cx);
            }),
            cx.on_release(|this: &mut Self, cx| this.watch_devices(false, cx)),
        ];
        let mut this = Self { ui, apps_timer: None, devices_watched: false, _watch: watch, _subscriptions: subscriptions };
        let open = matches!(this.ui.read(cx).popover, Some(PanelPopover::Devices { .. }));
        this.watch_devices(open, cx);
        this
    }

    fn watch_devices(&mut self, on: bool, cx: &mut App) {
        if on == self.devices_watched {
            return;
        }
        self.devices_watched = on;
        let devices = Model::global(cx).live.voice.devices.clone();
        devices.update(cx, |d, cx| if on { d.watch(cx) } else { d.unwatch() });
    }
}

fn close_popover(cx: &mut App) {
    let ui = VoiceUi::global(cx);
    ui.update(cx, |u, cx| u.close_popover(cx));
}

/// Escape closes what is open (a key listener only while it is).
fn escape_closes() -> impl IntoElement {
    canvas(|_, _, _| {}, |_, _, window, _| {
        window.on_key_event(|e: &KeyDownEvent, phase, _, cx| {
            if phase == DispatchPhase::Bubble && e.keystroke.key == "escape" {
                close_popover(cx);
            }
        })
    })
    .absolute()
    .size_0()
}

impl Render for PanelPopovers {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("PanelPopovers");
        let popover = self.ui.read(cx).popover;
        let live = Model::global(cx).live.voice.own_stream.read(cx).is_live();
        let audio_open = matches!(popover, Some(PanelPopover::StreamAudio(_))) && live;
        if audio_open != self.apps_timer.is_some() {
            self.apps_timer = audio_open.then(|| {
                cx.spawn(async |this, cx| {
                    loop {
                        if this.update(cx, |_, cx| crate::go_live::refresh_apps(cx)).is_err() {
                            break;
                        }
                        cx.background_executor().timer(crate::go_live::APPS_POLL).await;
                    }
                })
            });
        }
        let body: Option<AnyElement> = match popover {
            None => None,
            Some(PanelPopover::Stats(anchor)) => Some(stats_popover(anchor, cx).into_any_element()),
            Some(PanelPopover::Devices { input, anchor }) => Some(device_menu(input, anchor, cx).into_any_element()),
            Some(PanelPopover::StreamAudio(anchor)) if live => Some(stream_audio_popover(anchor, window, cx).into_any_element()),
            Some(PanelPopover::StreamAudio(_)) => None,
        };
        div().children(body).when(popover.is_some(), |this| this.child(escape_closes()))
    }
}

/// `ConnectionStatsPopover`: the latest ping and loss, a 244 × 120 graph of the last five
/// minutes (ping as a line over a gradient, loss dashed), a note on sustained loss.
fn stats_popover(anchor: Bounds<Pixels>, cx: &mut Context<PanelPopovers>) -> impl IntoElement + use<> {
    const WIDTH: f32 = 244.;
    const HEIGHT: f32 = 120.;
    const PAD_X: f32 = 8.;
    const PAD_TOP: f32 = 12.;
    const PAD_BOTTOM: f32 = 16.;
    const WINDOW: Duration = Duration::from_secs(300);
    let model = Model::global(cx).clone();
    let samples: Vec<StatsSample> = model.live.voice.stats.read(cx).samples().iter().copied().collect();
    let theme = cx.theme();
    let now = Instant::now();
    let ping_max = {
        let max = samples.iter().filter_map(|s| s.ping_ms).max().unwrap_or(0).max(100);
        max.div_ceil(50) * 50
    } as f32;
    let latest = samples.last().copied();
    let latest_ping = latest.and_then(|s| s.ping_ms);
    let latest_loss = latest.map(|s| s.loss_pct).unwrap_or(0.);
    let pings: Vec<u32> = samples.iter().filter_map(|s| s.ping_ms).collect();
    let avg_ping = (!pings.is_empty()).then(|| (pings.iter().sum::<u32>() as f32 / pings.len() as f32).round() as u32);
    let avg_loss = if samples.is_empty() { 0. } else { samples.iter().map(|s| s.loss_pct).sum::<f32>() / samples.len() as f32 };
    let format_loss = |p: f32| if p < 0.1 { "0%".to_string() } else if p < 10. { format!("{p:.1}%") } else { format!("{}%", p.round()) };
    let (accent, error, border, muted) = (theme.color_accent, theme.color_error, theme.color_border, theme.color_text_muted);
    let h = HEIGHT - PAD_TOP - PAD_BOTTOM;
    let w = WIDTH - PAD_X * 2.;
    let point_of = move |s: &StatsSample, v: f32, max: f32| -> Option<(f32, f32)> {
        let age = now.saturating_duration_since(s.at).as_secs_f32();
        let t = 1. - age / WINDOW.as_secs_f32();
        if !(0. ..=1.).contains(&t) {
            return None;
        }
        Some((PAD_X + t * w, PAD_TOP + (1. - (v / max).clamp(0., 1.)) * h))
    };
    let graph_samples = samples.clone();
    let graph = canvas(
        |_, _, _| {},
        move |bounds, _, window, _| {
            let origin = bounds.origin;
            let at = |(x, y): (f32, f32)| point(origin.x + px(x), origin.y + px(y));
            // The grid: three hairlines.
            for frac in [0., 0.5, 1.] {
                let y = PAD_TOP + h * frac;
                let mut line = PathBuilder::stroke(px(0.5));
                line.move_to(at((PAD_X, y)));
                line.line_to(at((WIDTH - PAD_X, y)));
                if let Ok(path) = line.build() {
                    window.paint_path(path, border);
                }
            }
            // Ping: the area under it, then the line (a gap where a sample had no ping).
            let ping_points: Vec<(f32, f32)> =
                graph_samples.iter().filter_map(|s| s.ping_ms.and_then(|p| point_of(s, p as f32, ping_max))).collect();
            if ping_points.len() >= 2 {
                let bottom = PAD_TOP + h;
                let mut area = PathBuilder::fill();
                area.move_to(at((ping_points[0].0, bottom)));
                for p in &ping_points {
                    area.line_to(at(*p));
                }
                area.line_to(at((ping_points[ping_points.len() - 1].0, bottom)));
                area.close();
                if let Ok(path) = area.build() {
                    window.paint_path(
                        path,
                        linear_gradient(180., linear_color_stop(accent.opacity(0.15), 0.), linear_color_stop(accent.opacity(0.), 1.)),
                    );
                }
            }
            let mut line = PathBuilder::stroke(px(1.5));
            let mut pen_up = true;
            let mut drew = false;
            for s in &graph_samples {
                let Some(p) = s.ping_ms.and_then(|p| point_of(s, p as f32, ping_max)) else {
                    pen_up = true;
                    continue;
                };
                if pen_up { line.move_to(at(p)) } else { line.line_to(at(p)) }
                pen_up = false;
                drew = true;
            }
            if drew && let Ok(path) = line.build() {
                window.paint_path(path, accent);
            }
            // Loss: dashed.
            let mut loss = PathBuilder::stroke(px(1.5)).dash_array(&[px(3.), px(3.)]);
            let mut first = true;
            let mut drew = false;
            for s in &graph_samples {
                let Some(p) = point_of(s, s.loss_pct, 100.) else { continue };
                if first { loss.move_to(at(p)) } else { loss.line_to(at(p)) }
                first = false;
                drew = true;
            }
            if drew && let Ok(path) = loss.build() {
                window.paint_path(path, error);
            }
            // The current ping: a dot with a halo at the right edge.
            if let Some(ping) = latest_ping {
                let y = PAD_TOP + (1. - (ping as f32 / ping_max).clamp(0., 1.)) * h;
                for (r, color) in [(4., accent.opacity(0.2)), (2.5, accent)] {
                    let c = at((WIDTH - PAD_X, y));
                    let mut dot = PathBuilder::fill();
                    dot.move_to(point(c.x + px(r), c.y));
                    dot.arc_to(point(px(r), px(r)), px(0.), false, true, point(c.x - px(r), c.y));
                    dot.arc_to(point(px(r), px(r)), px(0.), false, true, point(c.x + px(r), c.y));
                    dot.close();
                    if let Ok(path) = dot.build() {
                        window.paint_path(path, color);
                    }
                }
            }
        },
    )
    .absolute()
    .size_full();
    let axis = |text: String, x: f32, y: f32, right: bool| {
        div()
            .absolute()
            .top(px(y - 10.))
            .when(!right, |this| this.left(px(x)))
            .when(right, |this| this.right(px(WIDTH - x)))
            .child(Label::new(SharedString::from(text)).size(10.).line_height(12.).color(muted).nowrap())
    };
    let dot = |color| div().size(px(7.)).flex_none().rounded_full().bg(color);
    let content = div()
        .w(px(260.))
        .child(
            div()
                .border_b_1()
                .border_color(theme.color_border_divider)
                .px_4()
                .py_3()
                .child(div().mb_2p5().child(Label::new("CONNECTION").size(11.).weight(600.).tracking(0.07).color(muted)))
                .child(
                    div()
                        .flex()
                        .items_center()
                        .gap_4()
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(dot(accent))
                                .child(Label::new("Ping").size(12.).color(theme.color_text_secondary))
                                .child(
                                    Label::new(SharedString::from(latest_ping.map_or("—".into(), |p| format!("{p} ms"))))
                                        .size(13.)
                                        .weight(600.)
                                        .color(theme.color_text_primary)
                                        .nowrap(),
                                )
                                .when_some(avg_ping.filter(|a| Some(*a) != latest_ping), |this, avg| {
                                    this.child(Label::new(SharedString::from(format!("(avg {avg} ms)"))).size(11.).color(muted).nowrap())
                                }),
                        )
                        .child(
                            div()
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .child(dot(error))
                                .child(Label::new("Loss").size(12.).color(theme.color_text_secondary))
                                .child(
                                    Label::new(SharedString::from(format_loss(latest_loss)))
                                        .size(13.)
                                        .weight(600.)
                                        .color(theme.color_text_primary)
                                        .nowrap(),
                                ),
                        ),
                ),
        )
        .child(
            div().px_4().pb_3().pt_2p5().child(
                div()
                    .relative()
                    .overflow_hidden()
                    .rounded(theme.radius_md)
                    .border_1()
                    .border_color(theme.color_border_divider)
                    .bg(theme.color_bg_mid)
                    .w(px(WIDTH + 2.))
                    .h(px(HEIGHT + 2.))
                    .child(
                        div()
                            .relative()
                            .w(px(WIDTH))
                            .h(px(HEIGHT))
                            .child(graph)
                            .child(axis(format!("{} ms", ping_max as u32), PAD_X + 2., PAD_TOP + 8., false))
                            .child(axis(format!("{} ms", (ping_max / 2.).round() as u32), PAD_X + 2., PAD_TOP + h * 0.5 + 3., false))
                            .child(axis("0 ms".into(), PAD_X + 2., HEIGHT - PAD_BOTTOM - 2., false))
                            .child(axis("last 5 min".into(), WIDTH - PAD_X - 2., HEIGHT - 3., true)),
                    ),
            ),
        )
        .when(samples.is_empty(), |this| {
            this.child(div().px_4().pb_3().flex().justify_center().child(Label::new("Collecting samples…").size(11.).color(muted)))
        })
        .when(!samples.is_empty() && avg_loss > 0.5, |this| {
            this.child(
                div().px_4().pb_3().child(
                    Label::new(SharedString::from(format!(
                        "Avg loss {} over window — sustained loss above 1% can cause stuttering.",
                        format_loss(avg_loss)
                    )))
                    .size(11.)
                    .line_height(17.)
                    .color(muted),
                ),
            )
        });
    Popover::new("connection-stats", point(anchor.left(), anchor.top() - px(6.)))
        .prefer(Side::Above)
        .on_dismiss(|_: &DismissEvent, _, cx| close_popover(cx))
        .child(content)
}

/// A floating card that a press outside closes (the device menu, the stream-audio picker):
/// `rounded-lg border border-border bg-bg-secondary shadow-modal`.
fn floating(id: &'static str, anchor: gpui::Point<Pixels>, prefer: Side, content: AnyElement, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let card = div()
        .id(id)
        .occlude()
        .rounded(theme.radius_lg)
        .border_1()
        .border_color(theme.color_border)
        .bg(theme.color_bg_secondary)
        .shadow(theme.shadow_modal.clone())
        .on_mouse_down_out(|_, _, cx| close_popover(cx))
        .child(content);
    deferred(Placed::new(Placement::Menu { anchor, prefer }).child(card)).with_priority(4)
}

/// `DeviceContextMenu`: "INPUT DEVICE" / "OUTPUT DEVICE", Default then each device, a tick on
/// the chosen one. Opens upward from the button.
fn device_menu(input: bool, anchor: Bounds<Pixels>, cx: &mut Context<PanelPopovers>) -> impl IntoElement + use<> {
    let model = Model::global(cx).clone();
    let devices = model.live.voice.devices.read(cx);
    let list = if input { devices.inputs.clone() } else { devices.outputs.clone() };
    let settings = model.settings.read(cx).get();
    let selected = if input { settings.input_device.clone() } else { settings.output_device.clone() };
    let theme = cx.theme();
    let row = |id: SharedString, name: Option<String>, label: SharedString, on: bool| {
        div()
            .id(id)
            .flex()
            .w_full()
            .items_center()
            .gap_2()
            .px_3()
            .py_2()
            .hover(|s| s.bg(theme.color_surface_hover))
            .cursor_pointer()
            .on_click(move |_: &ClickEvent, _, cx| {
                if input {
                    actions::voice::set_input_device(name.clone(), cx);
                } else {
                    actions::voice::set_output_device(name.clone(), cx);
                }
                close_popover(cx);
            })
            .when(on, |this| this.child(Icon::new(IconName::SettingsCheck).size(px(12.)).stroke_width(3.).color(theme.color_accent)))
            .child(
                div().when(!on, |this| this.pl_5()).child(
                    Label::new(label)
                        .size(12.)
                        .weight(if on { 600. } else { 400. })
                        .color(if on { theme.color_accent_bright } else { theme.color_text_secondary }),
                ),
            )
            .into_any_element()
    };
    let mut rows = vec![row("device-default".into(), None, "Default".into(), selected.is_none())];
    for d in &list {
        let on = selected.as_deref() == Some(&*d.name);
        let label = if d.label.is_empty() { d.name.clone() } else { d.label.clone() };
        rows.push(row(SharedString::from(format!("device-{}", d.name)), Some(d.name.to_string()), label, on));
    }
    let content = div()
        .w(px(240.))
        .child(
            div().border_b_1().border_color(theme.color_border).px_3().py_2p5().child(
                Label::new(if input { "INPUT DEVICE" } else { "OUTPUT DEVICE" }).size(11.).weight(600.).tracking(0.07).color(theme.color_text_muted),
            ),
        )
        .child(div().id("device-list").max_h(px(240.)).overflow_y_scroll().py_1().children(rows))
        .into_any_element();
    floating("device-menu", point(anchor.left(), anchor.top()), Side::Above, content, cx)
}

/// `StreamAudioPopover`: "STREAM SOUND" over the app picker, 300 px wide, above its button when
/// the button sits low on the screen.
fn stream_audio_popover(anchor: Bounds<Pixels>, window: &mut Window, cx: &mut Context<PanelPopovers>) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let viewport = decibell_ui::visible_window_bounds(window, cx);
    let est = px(280.);
    let above = anchor.bottom() + est + px(8.) > viewport.bottom() && anchor.top() - viewport.top() > est;
    let content = div()
        .w(px(300.))
        .p_3()
        .flex()
        .flex_col()
        .gap_2()
        .child(Label::new("STREAM SOUND").size(11.).weight(600.).tracking(0.07).color(theme.color_text_muted))
        .child(crate::go_live::audio_app_picker(true, cx))
        .into_any_element();
    let at = if above { point(anchor.left(), anchor.top() - px(8.)) } else { point(anchor.left(), anchor.bottom() + px(8.)) };
    floating("stream-audio", at, if above { Side::Above } else { Side::Below }, content, cx)
}
