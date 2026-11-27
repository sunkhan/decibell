//! DM calls (`features/call/`): the incoming-call card and the call stage.
//!
//! - [`IncomingCall`] (an overlay; `IncomingCallModal.tsx`): top-centred, no backdrop, the
//!   caller's colour washed into the surface, a breathing ring on their avatar while it rings,
//!   Decline / Accept; Escape declines. When another device of ours answers or declines, the
//!   card says so for 3 s. The ring breathes by re-presenting a retained sprite (at most 30 fps,
//!   only while the card is on screen); nothing is drawn per frame.
//! - [`CallStage`] (`Slot::DmCallStage`, hosted by the DM pane above the conversation;
//!   `CallStage.tsx`): the voice view at DM size. A slim call bar (state, duration, the
//!   verification lock, ping and path, theater), you and the peer as 16:9 tiles and a tile per
//!   live screen share, the dock; a focused stream fills the stage with the controls floating
//!   over it; theater fills the DM panel; fullscreen the window. The compact stage's bottom edge
//!   drags to resize (remembered per mode), a double click resets it.

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use decibell_common::{UserAvatar, Watch, user_name};
use decibell_model::live::calls::{CallPath, CallStatus};
use decibell_model::live::voice::session::Participant;
use decibell_model::live::voice::streams::StreamInfo;
use decibell_model::stores::layout::{CALL_STAGE_MIN, CALL_STAGE_STREAM_DEFAULT, CALL_STAGE_VOICE_DEFAULT, CallStageMode};
use decibell_model::{Model, Uid, actions};
use decibell_ui::identity::{avatar_color, person_slot};
use decibell_ui::motion::mix;
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, title_tooltip};
use decibell_video::{Fit, VideoView, Viewer};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, DispatchPhase, Easing, Entity, Hsla, IntoElement,
    KeyDownEvent, MouseButton, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, SharedString, StyleRefinement,
    Subscription, Task, TimedTransformation, Transformation, Window, anchored, canvas, div, prelude::*, px, svg,
};

use crate::dock::{Tone, dock_button, dock_divider, latency_tone};
use crate::focus::{overlay_button, set_stream_volume, stats_overlay};
use crate::layout::{MAX_TILE_W, TILE_ASPECT, TILE_GAP, fit_grid, tile_height};
use crate::scrim::{self, LiveSize, black, on_video_green, white};
use crate::tiles::{ParticipantTile, PersonData, StreamData, StreamTile};
use crate::volume::VolumeControl;

/// How long the card says "Answered on another device".
pub const ENDED_SHOWN: Duration = Duration::from_millis(3000);
/// One breath of the ringing outline (`animate-[breathe_1.6s_ease-in-out_infinite]`).
pub const BREATHE: Duration = Duration::from_millis(1600);
/// The breathing outline re-presents at most this often (opacity 0.4 → 1 in 0.8 s).
pub const BREATHE_FPS: f32 = 30.;

/// A rounded-rect ring as an SVG sprite (alpha only; tinted when drawn): `w` × `h` box, the ring
/// `stroke` wide on its inside edge, corner `radius`.
fn ring_svg(w: f32, h: f32, radius: f32, stroke: f32) -> Vec<u8> {
    let half = stroke / 2.;
    format!(
        r##"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><rect x="{half}" y="{half}" width="{iw}" height="{ih}" rx="{r}" fill="none" stroke="#fff" stroke-width="{stroke}"/></svg>"##,
        iw = w - stroke,
        ih = h - stroke,
        r = (radius - half).max(0.),
    )
    .into_bytes()
}

/// A ring that breathes (opacity 1 ↔ 0.4, ease-in-out) by re-presenting, never re-rendering.
fn breathing_ring(w: f32, h: f32, radius: f32, stroke: f32, color: Hsla, cx: &App) -> AnyElement {
    let data: &'static [u8] = Box::leak(ring_svg(w, h, radius, stroke).into_boxed_slice());
    let mut sprite = svg().data(data).w(px(w)).h(px(h)).text_color(color);
    if !cx.reduce_motion() {
        sprite = sprite.with_timed_transformation(
            TimedTransformation::new(BREATHE)
                .from(Transformation::alpha(0.4))
                .to(Transformation::alpha(1.))
                .with_easing(Easing::EaseInOut)
                .alternate()
                .with_max_fps(BREATHE_FPS),
        );
    }
    sprite.into_any_element()
}

// ── the incoming call ───────────────────────────────────────────────────────────────────

pub struct IncomingCall {
    clear_timer: Option<Task<()>>,
    _watch: Watch,
}

impl IncomingCall {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.live.calls, cx).people(cx, |_, uid| {
            let _ = uid;
            true
        });
        Self { clear_timer: None, _watch: watch }
    }
}

impl Render for IncomingCall {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("IncomingCall");
        let model = Model::global(cx).clone();
        let calls = model.live.calls.read(cx);
        let ringing = calls.status == CallStatus::Incoming && calls.incoming.is_some();
        let shown = if ringing {
            calls.incoming.as_ref().map(|i| (i.from_uid, i.from.clone(), None, false))
        } else {
            calls.incoming_ended.as_ref().map(|e| (e.from_uid, e.from.clone(), Some(e.reason.clone()), e.answered))
        };
        // The "elsewhere" notice stays 3 s (a timer only while it shows).
        let ended = !ringing && shown.is_some();
        if ended && self.clear_timer.is_none() {
            self.clear_timer = Some(cx.spawn(async |_, cx| {
                cx.background_executor().timer(ENDED_SHOWN).await;
                cx.update(|cx| {
                    let model = Model::global(cx).clone();
                    model.live.calls.update(cx, |c, cx| c.clear_incoming_ended(cx));
                });
            }));
        } else if !ended {
            self.clear_timer = None;
        }
        decibell_shell::probes::flag("incoming-call", shown.is_some());
        let Some((from_uid, from, reason, answered)) = shown else { return div().into_any_element() };
        let caller = user_name(from_uid, &from, cx);
        let theme = cx.theme();
        let tint = mix(theme.color_bg_light, avatar_color(theme, person_slot(from_uid, &caller)), theme.palette.tile_tint);
        let viewport = decibell_ui::visible_window_bounds(window, cx);
        let avatar_radius = f32::from(theme.avatar_radius(px(44.)));
        let keys = canvas(|_, _, _| {}, move |_, _, window, _| {
            window.on_key_event(move |e: &KeyDownEvent, phase, _, cx| {
                if phase == DispatchPhase::Bubble && e.keystroke.key == "escape" {
                    if ringing {
                        actions::calls::decline_call(cx);
                    } else {
                        let model = Model::global(cx).clone();
                        model.live.calls.update(cx, |c, cx| c.clear_incoming_ended(cx));
                    }
                }
            })
        })
        .absolute()
        .size_0();
        let avatar = div()
            .relative()
            .flex_none()
            .when(ringing, |this| {
                // `outline-2 outline-offset-2`: 2 px, 2 px outside the avatar.
                this.child(
                    div()
                        .absolute()
                        .top(px(-4.))
                        .left(px(-4.))
                        .child(breathing_ring(52., 52., avatar_radius + 4., 2., theme.color_success, cx)),
                )
            })
            .child(div().when(!ringing, |this| this.opacity(0.6)).child(UserAvatar::new(from_uid, caller.clone(), 44.)));
        let buttons: AnyElement = if ringing {
            div()
                .flex()
                .flex_none()
                .gap_1p5()
                .child(
                    div()
                        .id("call-decline")
                        .flex()
                        .h_10()
                        .w_12()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_md)
                        .bg(theme.color_error)
                        .hover(|s| s.bg(theme.color_error.opacity(0.85)))
                        .cursor_pointer()
                        .tooltip(title_tooltip("Decline (Esc)"))
                        .on_click(|_: &ClickEvent, _, cx| actions::calls::decline_call(cx))
                        .relative()
                        .children(decibell_shell::probes::probe("call-decline"))
                        .child(Icon::new(IconName::StageLeave).size(px(18.)).color(theme.color_on_error)),
                )
                .child(
                    div()
                        .id("call-accept")
                        .flex()
                        .h_10()
                        .w_12()
                        .items_center()
                        .justify_center()
                        .rounded(theme.radius_md)
                        .bg(theme.color_success)
                        .hover(|s| s.bg(theme.color_success.opacity(0.85)))
                        .cursor_pointer()
                        .tooltip(title_tooltip("Accept"))
                        .on_click(|_: &ClickEvent, _, cx| actions::calls::accept_call(cx))
                        .relative()
                        .children(decibell_shell::probes::probe("call-accept"))
                        .child(Icon::new(IconName::ShellPhone).size(px(18.)).color(theme.color_bg_darkest)),
                )
                .into_any_element()
        } else {
            div()
                .flex()
                .flex_none()
                .h_10()
                .w_12()
                .items_center()
                .justify_center()
                .rounded(theme.radius_md)
                .bg(theme.color_surface_hover)
                .child(Icon::new(if answered { IconName::CallAnswered } else { IconName::CallDeclined }).size(px(16.)).color(theme.color_text_muted))
                .into_any_element()
        };
        let subtitle: SharedString = if ringing { "Incoming voice call".into() } else { reason.unwrap_or_default() };
        let card = div()
            .id("incoming-call")
            .occlude()
            .flex()
            .w(px(384.))
            .items_center()
            .gap_3p5()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(tint)
            .py_3()
            .pl_3p5()
            .pr_3()
            .shadow(theme.shadow_float.clone())
            .child(keys)
            .child(avatar)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .flex_1()
                    .min_w_0()
                    .child(
                        Label::new(caller)
                            .font(FontRole::Display)
                            .size(theme.scaled.text_title)
                            .weight(theme.palette.font_weight_emphasis)
                            .tracking(theme.palette.tracking_title)
                            .color(theme.color_text_bright)
                            .truncate(),
                    )
                    .child(Label::new(subtitle).font(FontRole::Meta).size(theme.scaled.text_meta).color(theme.color_text_muted).truncate()),
            )
            .child(buttons);
        // `fixed inset-x-0 top-14 flex justify-center`: 56 px below the window's top.
        let left = viewport.left() + (viewport.size.width - px(384.)) / 2.;
        decibell_ui::layer(7, anchored().position(gpui::point(left, viewport.top() + px(56.))).child(card)).into_any_element()
    }
}

// ── the call stage ──────────────────────────────────────────────────────────────────────

/// Below these stage widths the focused view drops what would collide.
const CHIPS_MIN_W: f32 = 940.;
const STATUS_MIN_W: f32 = 700.;
const QUALITY_MIN_W: f32 = 560.;
/// One row of tiles wins unless the best-fit grid's tiles are this much bigger.
const ROW_PREFERENCE: f32 = 0.85;
/// The chat keeps at least this much under the compact stage.
const CHAT_MIN: f32 = 180.;

pub struct CallStage {
    /// The stage's own bounds and its parent's height (for the compact cap), as laid out.
    width: Rc<Cell<f32>>,
    parent_height: Rc<Cell<f32>>,
    tiles_size: Rc<Cell<(f32, f32)>>,
    tiles: Vec<(Uid, Entity<ParticipantTile>)>,
    streams: Vec<(Uid, Entity<StreamTile>)>,
    focused: Option<(Uid, Viewer)>,
    overlay: bool,
    hide_timer: Option<Task<()>>,
    duration_timer: Option<Task<()>>,
    stats: bool,
    stats_timer: Option<Task<()>>,
    sampler: decibell_video::StatsSampler,
    rates: Option<decibell_video::StreamRates>,
    volume: u8,
    resizing: Option<(f32, f32, f32)>,
    _subs: Vec<Subscription>,
    _watch: Watch,
}

impl CallStage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let subs = vec![
            cx.observe(&model.live.calls, |this, _, cx| this.sync(cx)),
            cx.observe(&voice.participants, |this, _, cx| this.sync(cx)),
            cx.observe(&voice.streams, |this, _, cx| this.sync(cx)),
            cx.observe(&voice.watching, |this, _, cx| this.sync(cx)),
            cx.observe(&voice.session, |this, _, cx| this.sync(cx)),
            cx.observe(&voice.own_stream, |this, _, cx| this.sync(cx)),
            cx.observe(&model.dm.list, |this, _, cx| this.sync(cx)),
        ];
        let mut watch = Watch::new();
        watch
            .store_if(&voice.stats, cx, |this: &mut Self, _, _| this.tiles.len() == 2)
            // The chips over a focused stream show who talks.
            .event(&voice.speaking, cx, |this: &mut Self, e: &decibell_model::live::voice::speaking::SpeakingChanged| {
                this.focused.is_some() && this.tiles.iter().any(|(u, _)| *u == e.uid)
            })
            .event(&model.layout, cx, |_, _| true)
            // The focused stream's stall ("Reconnecting…").
            .event(&crate::video_events::StreamStates::global(cx), cx, |this: &mut Self, e: &crate::video_events::StreamStateChanged| {
                this.focused.as_ref().is_some_and(|(u, _)| *u == e.0)
            })
            .people(cx, |this: &Self, uid| this.tiles.iter().any(|(u, _)| *u == uid));
        let mut this = Self {
            width: Rc::new(Cell::new(0.)),
            parent_height: Rc::new(Cell::new(0.)),
            tiles_size: Rc::new(Cell::new((0., 0.))),
            tiles: Vec::new(),
            streams: Vec::new(),
            focused: None,
            overlay: true,
            hide_timer: None,
            duration_timer: None,
            stats: false,
            stats_timer: None,
            sampler: decibell_video::StatsSampler::new(),
            rates: None,
            volume: 100,
            resizing: None,
            _subs: subs,
            _watch: watch,
        };
        this.sync(cx);
        this
    }

    /// The call with the open conversation's peer, if the stage shows (`CallStage`: not idle,
    /// not ringing in, and this conversation's peer).
    fn call(cx: &App) -> Option<(Uid, CallStatus)> {
        let model = Model::global(cx);
        let calls = model.live.calls.read(cx);
        let peer = calls.peer?;
        let open = model.dm.list.read(cx).active_peer;
        (matches!(calls.status, CallStatus::Outgoing | CallStatus::Connecting | CallStatus::Active) && open == Some(peer))
            .then_some((peer, calls.status))
    }

    /// Bring the tiles and the focused viewer in line with the stores.
    fn sync(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let me = model.auth.read(cx).uid;
        let Some((peer, status)) = Self::call(cx) else {
            self.tiles.clear();
            self.streams.clear();
            self.focused = None;
            self.duration_timer = None;
            cx.notify();
            return;
        };
        let live = status == CallStatus::Active;
        let voice = model.live.voice.clone();
        let participants: Vec<Participant> = voice.participants.read(cx).list().to_vec();
        let (muted, deafened) = {
            let s = voice.session.read(cx);
            (s.muted, s.deafened)
        };
        let settings = model.settings.read(cx).get().clone();
        let (w, h) = self.tiles_size.get();
        let streams: Vec<StreamInfo> = if live {
            let all = voice.streams.read(cx).session(None, true);
            let own_live = voice.own_stream.read(cx).is_live();
            let peer_stream = all.iter().find(|s| s.owner_uid == peer).cloned();
            let own_stream = own_live.then(|| all.iter().find(|s| s.owner_uid == me).cloned()).flatten();
            [peer_stream, own_stream].into_iter().flatten().collect()
        } else {
            Vec::new()
        };
        let n = 2 + streams.len();
        let best = fit_grid(n, w, h, TILE_GAP, MAX_TILE_W);
        let row_w = ((w - TILE_GAP * (n as f32 - 1.)) / n as f32).min(h * TILE_ASPECT).min(MAX_TILE_W).floor();
        let tile_w = if row_w >= best.tile_w * ROW_PREFERENCE { row_w } else { best.tile_w }.max(0.);
        let mut tiles = Vec::new();
        for (uid, local) in [(me, true), (peer, false)] {
            let p = participants.iter().find(|p| p.uid == uid);
            let state = p.map(|p| p.state).unwrap_or_default();
            let data = PersonData {
                uid,
                name: p.map(|p| p.name.clone()).unwrap_or_else(|| "Unknown user".into()),
                server: None,
                local,
                muted: if local { muted } else { state.muted },
                deafened: if local { deafened } else { state.deafened },
                server_muted: false,
                server_deafened: false,
                suppressed: false,
                streaming: streams.iter().any(|s| s.owner_uid == uid),
                locally_muted: decibell_model::live::voice::gain::locally_muted(&settings, uid),
                width: tile_w,
                mini: false,
            };
            let tile = match self.tiles.iter().find(|(u, _)| *u == uid) {
                Some((_, t)) => {
                    t.update(cx, |t, cx| t.set(data, cx));
                    t.clone()
                }
                None => cx.new(|cx| ParticipantTile::new(data, cx)),
            };
            tiles.push((uid, tile));
        }
        self.tiles = tiles;
        let watching = voice.watching.read(cx).clone();
        let mut stream_tiles = Vec::new();
        for info in &streams {
            let owner = info.owner_uid;
            let data = StreamData {
                info: info.clone(),
                server: None,
                watching: watching.is_watching(owner),
                own: owner == me,
                watching_others: false,
                blocked: None,
                width: tile_w,
                mini: false,
                call: true,
            };
            let tile = match self.streams.iter().find(|(u, _)| *u == owner) {
                Some((_, t)) => {
                    t.update(cx, |t, cx| t.set(data, cx));
                    t.clone()
                }
                None => cx.new(|cx| StreamTile::new(data, cx)),
            };
            stream_tiles.push((owner, tile));
        }
        // The stream on stage: the peer's or our preview, watched and live.
        let focused = watching
            .focused
            .filter(|u| (*u == peer || *u == me) && watching.is_watching(*u) && streams.iter().any(|s| s.owner_uid == *u));
        if focused != self.focused.as_ref().map(|(u, _)| *u) {
            self.focused = focused.map(|u| (u, crate::players::viewer(u, cx)));
            self.overlay = true;
            self.poke(cx);
        }
        if focused.is_some() {
            // The stage shows the stream alone: the stream tiles (and their viewers) go.
            stream_tiles.clear();
        }
        self.streams = stream_tiles;
        // The duration ticks once a second while the call is live and the stage shows it.
        if live && self.duration_timer.is_none() {
            self.duration_timer = Some(cx.spawn(async |this, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    if this.update(cx, |_, cx| cx.notify()).is_err() {
                        break;
                    }
                }
            }));
        } else if !live {
            self.duration_timer = None;
        }
        cx.notify();
    }

    fn poke(&mut self, cx: &mut Context<Self>) {
        if !self.overlay {
            self.overlay = true;
            cx.notify();
        }
        self.hide_timer = Some(cx.spawn(async |this, cx| {
            cx.background_executor().timer(crate::focus::OVERLAY_HIDE).await;
            let _ = this.update(cx, |this, cx| {
                this.overlay = false;
                crate::focus::rest_cursor(cx);
                cx.notify();
            });
        }));
    }

    fn toggle_stats(&mut self, cx: &mut Context<Self>) {
        self.stats = !self.stats;
        self.stats_timer = None;
        if self.stats {
            self.stats_timer = Some(cx.spawn(async |this, cx| {
                loop {
                    let ok = this
                        .update(cx, |this, cx| {
                            if let Some((_, viewer)) = &this.focused {
                                this.rates = Some(this.sampler.sample(&viewer.stream().stats()));
                            }
                            cx.notify();
                        })
                        .is_ok();
                    if !ok {
                        break;
                    }
                    cx.background_executor().timer(crate::focus::STATS_REFRESH).await;
                }
            }));
        }
        cx.notify();
    }
}

fn format_duration(d: Duration) -> String {
    let total = d.as_secs();
    let (h, m, s) = (total / 3600, (total % 3600) / 60, total % 60);
    if h > 0 { format!("{h}:{m:02}:{s:02}") } else { format!("{m:02}:{s:02}") }
}

fn back_to_tiles(cx: &mut App) {
    let model = Model::global(cx).clone();
    model.live.voice.watching.update(cx, |w, cx| w.set_focused(None, cx));
}

fn hang_up(status: CallStatus, cx: &mut App) {
    let model = Model::global(cx).clone();
    if model.live.voice.watching.read(cx).fullscreen {
        model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(false, cx));
    }
    actions::calls::end_call(Some(if status == CallStatus::Outgoing { "Cancelled" } else { "Call ended" }), true, cx);
}

fn verified_title(verified: bool, peer: &str) -> SharedString {
    if verified {
        format!("End-to-end encrypted. {peer}'s call key matches their verified identity.").into()
    } else {
        format!("End-to-end encrypted, but {peer} hasn't set up encryption, so their identity can't be checked.").into()
    }
}

impl Render for CallStage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("CallStage");
        let model = Model::global(cx).clone();
        let Some((peer, status)) = Self::call(cx) else { return div().into_any_element() };
        let me = model.auth.read(cx).uid;
        let live = status == CallStatus::Active;
        let (theater, ringing_acked, started_at, path, verified) = {
            let c = model.live.calls.read(cx);
            (c.theater, c.ringing_acked, c.started_at, c.path, c.verified)
        };
        let fullscreen = model.live.voice.watching.read(cx).fullscreen && self.focused.is_some();
        let latency = model.live.voice.stats.read(cx).latency_ms;
        let error = model.live.voice.session.read(cx).error.clone();
        let streaming = model.live.voice.own_stream.read(cx).is_live();
        let (muted, deafened) = {
            let s = model.live.voice.session.read(cx);
            (s.muted, s.deafened)
        };
        let peer_name = user_name(peer, &"Unknown user".into(), cx);
        let prefs = model.layout.read(cx).prefs().clone();
        let mode = if self.focused.is_some() { CallStageMode::Stream } else { CallStageMode::Voice };
        let stage_height = match mode {
            CallStageMode::Stream => prefs.call_stage_stream_height,
            CallStageMode::Voice => prefs.call_stage_voice_height,
        } as f32;
        let parent = self.parent_height.get();
        let max_compact = if parent > 0. { (parent - 48. - CHAT_MIN).max(CALL_STAGE_MIN as f32) } else { f32::MAX };
        let compact = !fullscreen && !theater;
        let height = self.resizing.map(|(_, _, h)| h).unwrap_or(stage_height).min(max_compact);
        let duration = started_at.map(|t| format_duration(Instant::now().saturating_duration_since(t))).unwrap_or_default();
        let stage_w = self.width.get();
        let (c_accent, c_border, c_bg_dark) = {
            let t = cx.theme();
            (t.color_accent, t.color_border, t.color_bg_dark)
        };

        let width_cell = self.width.clone();
        let parent_cell = self.parent_height.clone();
        let this = cx.entity().downgrade();
        let measure = canvas(
            move |bounds, window, cx| {
                let w = f32::from(bounds.size.width);
                if (width_cell.get() - w).abs() >= 1. {
                    width_cell.set(w);
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = this.update(cx, |_, cx| cx.notify());
                    });
                }
                let _ = &parent_cell;
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let dock = |over_video: bool, volume: Option<AnyElement>, cx: &mut Context<Self>| -> AnyElement {
            let theme = cx.theme();
            let end_label = if status == CallStatus::Outgoing { "Cancel" } else { "Hang up" };
            let (border, bg) = if over_video { (white(0.1), black(0.7)) } else { (theme.color_border, theme.color_bg_light) };
            let end = div()
                .id("call-hang-up")
                .flex()
                .flex_none()
                .h_10()
                .items_center()
                .justify_center()
                .gap_2()
                .rounded(theme.radius_md)
                .bg(theme.color_error)
                .hover(|s| s.bg(theme.color_error.opacity(0.85)))
                .cursor_pointer()
                .tooltip(title_tooltip(end_label))
                .map(|this| if live { this.w(px(56.)) } else { this.px_4() })
                .on_click(move |_: &ClickEvent, _, cx| hang_up(status, cx))
                .relative()
                .children(decibell_shell::probes::probe("call-hang-up"))
                .child(Icon::new(IconName::StageLeave).size(px(18.)).color(theme.color_on_error))
                .when(!live, |this| this.child(Label::new(end_label).font(FontRole::Channel).size(13.).weight(600.).color(theme.color_on_error)));
            let audio_bounds = decibell_common::BoundsCell::new();
            div()
                .id(if over_video { "call-dock-video" } else { "call-dock" })
                .occlude()
                .flex()
                .items_center()
                .gap_1p5()
                .rounded(theme.radius_lg)
                .border_1()
                .border_color(border)
                .bg(bg)
                .p_1p5()
                .shadow(theme.shadow_float.clone())
                .when(live, |this| {
                    this.child(dock_button(
                        "call-mute",
                        if muted { "Unmute" } else { "Mute" },
                        if muted { IconName::StageMicOff } else { IconName::StageMic },
                        if muted { Tone::Danger } else { Tone::Soft },
                        over_video,
                        cx,
                        |_, _, cx| actions::voice::toggle_mute(cx),
                    ))
                    .child(dock_button(
                        "call-deafen",
                        if deafened { "Undeafen" } else { "Deafen" },
                        if deafened { IconName::StageHeadphonesOff } else { IconName::StageHeadphones },
                        if deafened { Tone::Danger } else { Tone::Soft },
                        over_video,
                        cx,
                        |_, _, cx| actions::voice::toggle_deafen(cx),
                    ))
                    .child(dock_divider(over_video, cx))
                    .child(dock_button(
                        "call-share",
                        if streaming { "Stop sharing" } else { "Share your screen" },
                        if streaming { IconName::StageStopShare } else { IconName::StageScreen },
                        if streaming { Tone::On } else { Tone::Soft },
                        over_video,
                        cx,
                        move |_, _, cx| {
                            if streaming {
                                actions::voice::stop_stream(cx);
                            } else {
                                crate::state::open_go_live(cx);
                            }
                        },
                    ))
                    .children(crate::dock::stream_audio_button("call-stream-audio", over_video, &audio_bounds, cx))
                    .child(dock_divider(over_video, cx))
                    .when_some(volume, |this, v| this.child(v).child(dock_divider(over_video, cx)))
                })
                .child(end)
                .into_any_element()
        };

        let body: AnyElement = if let Some((uid, viewer)) = self.focused.clone() {
            let info = model.live.voice.streams.read(cx).session(None, true).into_iter().find(|s| s.owner_uid == uid);
            let own = uid == me;
            let owner_name = info.as_ref().map(|i| user_name(uid, &i.owner_name, cx)).unwrap_or_default();
            let label: SharedString = if own { "Your screen".into() } else { format!("{owner_name}'s screen").into() };
            let overlay = self.overlay;
            let me_entity = cx.entity().downgrade();
            let volume = (!own).then(|| {
                let a = me_entity.clone();
                let b = me_entity.clone();
                let volume = self.volume;
                VolumeControl::new("call-volume", volume)
                    .on_change(move |v, _, cx| {
                        let _ = a.update(cx, |s, cx| {
                            s.volume = v;
                            set_stream_volume(v as f64 / 100., cx);
                            cx.notify();
                        });
                    })
                    .on_toggle_mute(move |_, cx| {
                        let _ = b.update(cx, |s, cx| {
                            s.volume = if s.volume > 0 { 0 } else { 100 };
                            set_stream_volume(s.volume as f64 / 100., cx);
                            cx.notify();
                        });
                    })
                    .into_any_element()
            });
            let docked = dock(true, volume, cx);
            let stalled = {
                let state = crate::video_events::StreamStates::global(cx).read(cx).get(uid);
                state.picture.is_some() && state.stalled
            };
            let chips = (stage_w >= CHIPS_MIN_W).then(|| {
                div()
                    .absolute()
                    .bottom(px(26.))
                    .right_3()
                    .flex()
                    .gap_1p5()
                    .opacity(if self.overlay { 1. } else { 0. })
                    .child(peer_chip(me, true, cx))
                    .child(peer_chip(peer, false, cx))
            });
            let stats = self.stats.then(|| {
                info.as_ref().map(|i| div().absolute().left_3().top(px(56.)).child(stats_overlay(i, &viewer, self.rates, None, cx)))
            }).flatten();
            let status_on_video = (live && stage_w >= STATUS_MIN_W).then(|| {
                scrim::scrim_lg(cx)
                    .child(div().size_2().rounded_full().bg(on_video_green()))
                    .child(scrim::scrim_text(duration.clone(), white(1.), cx))
                    .when_some(latency, |this, ms| this.child(scrim::scrim_text(format!("{ms} ms"), white(0.6), cx)))
                    .when_some(verified, |this, v| {
                        this.child(
                            div()
                                .id("call-verified-video")
                                .tooltip(title_tooltip(verified_title(v, &peer_name)))
                                .child(Icon::new(IconName::ChatLock).size(px(11.)).color(if v { on_video_green() } else { white(0.45) })),
                        )
                    })
            });
            let opacity = if overlay { 1. } else { 0. };
            div()
                .id("call-video")
                .relative()
                .flex()
                .flex_1()
                .min_h_0()
                .min_w_0()
                .items_center()
                .justify_center()
                .overflow_hidden()
                .bg(black(1.))
                .when(!fullscreen, |this| this.cursor_pointer().on_click(|e: &ClickEvent, _, cx| {
                    if e.click_count() == 1 {
                        back_to_tiles(cx)
                    }
                }))
                .when(fullscreen, |this| this.on_click(|e: &ClickEvent, _, cx| {
                    if e.click_count() == 2 {
                        let model = Model::global(cx).clone();
                        model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(false, cx));
                    }
                }))
                .on_mouse_move(cx.listener(|this, _, _, cx| this.poke(cx)))
                .child(div().size_full().child(VideoView::new(&viewer).fit(Fit::Contain)))
                .when(stalled, |this| this.child(scrim::reconnecting(cx)))
                .children(stats)
                .child(
                    div()
                        .absolute()
                        .left_3()
                        .top_3()
                        .flex()
                        .gap_1p5()
                        .opacity(opacity)
                        .child(scrim::live_pill(LiveSize::Lg, cx))
                        .child(scrim::scrim_lg(cx).child(UserAvatar::new(uid, owner_name.clone(), 18.)).child(scrim::scrim_text(label, white(1.), cx)))
                        .children(info.as_ref().filter(|_| stage_w >= QUALITY_MIN_W).and_then(|i| scrim::quality_pill(i, true, cx))),
                )
                .child(
                    div()
                        .absolute()
                        .right_3()
                        .top_3()
                        .flex()
                        .items_center()
                        .gap_1p5()
                        .opacity(opacity)
                        .when(fullscreen, |this| this.child(div().mr_1p5().child(Label::new("Esc to exit").size(11.).color(white(0.5)))))
                        .children(status_on_video)
                        .child(overlay_button("call-stats", "Stream stats", IconName::StageStats, self.stats, cx, {
                            let me = me_entity.clone();
                            move |_, cx| {
                                let _ = me.update(cx, |s, cx| s.toggle_stats(cx));
                            }
                        }))
                        .when(!fullscreen, |this| {
                            this.child(overlay_button(
                                "call-theater",
                                if theater { "Exit theater" } else { "Theater" },
                                if theater { IconName::StageCollapse } else { IconName::CallTheater },
                                theater,
                                cx,
                                move |_, cx| toggle_theater(peer, cx),
                            ))
                        })
                        .child(overlay_button(
                            "call-fullscreen",
                            if fullscreen { "Exit fullscreen" } else { "Fullscreen" },
                            if fullscreen { IconName::StageCollapse } else { IconName::StageExpand },
                            false,
                            cx,
                            move |_, cx| {
                                let model = Model::global(cx).clone();
                                model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(!fullscreen, cx));
                            },
                        ))
                        .child(overlay_button(
                            "call-close-stream",
                            if own { "Close preview" } else { "Stop watching" },
                            IconName::StageClose,
                            false,
                            cx,
                            move |_, cx| {
                                let model = Model::global(cx).clone();
                                if model.live.voice.watching.read(cx).fullscreen {
                                    model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(false, cx));
                                }
                                actions::calls::unwatch_call_stream(uid, cx);
                            },
                        )),
                )
                .child(div().absolute().left_0().right_0().bottom_4().flex().justify_center().opacity(opacity).child(docked))
                .children(chips)
                .into_any_element()
        } else {
            let tiles_cell = self.tiles_size.clone();
            let this = cx.entity().downgrade();
            let tiles_measure = canvas(
                move |bounds, window, cx| {
                    let size = ((f32::from(bounds.size.width) - 48.).max(0.), (f32::from(bounds.size.height) - 4. - 80.).max(0.));
                    if tiles_cell.get() != size {
                        tiles_cell.set(size);
                        let this = this.clone();
                        window.defer(cx, move |_, cx| {
                            let _ = this.update(cx, |s, cx| s.sync(cx));
                        });
                    }
                },
                |_, _, _, _| {},
            )
            .absolute()
            .size_full();
            let tile_w = self.tiles.first().map(|(_, t)| t.read(cx).width()).unwrap_or(0.);
            let tile_h = tile_height(tile_w);
            let n = self.tiles.len() + self.streams.len();
            let (w, h) = self.tiles_size.get();
            let best = fit_grid(n.max(1), w, h, TILE_GAP, MAX_TILE_W);
            let row_w = ((w - TILE_GAP * (n as f32 - 1.)) / n.max(1) as f32).min(h * TILE_ASPECT).min(MAX_TILE_W).floor();
            let cols = if row_w >= best.tile_w * ROW_PREFERENCE { n } else { best.cols };
            let theme = cx.theme();
            let mut children: Vec<AnyElement> = Vec::new();
            for (uid, tile) in &self.tiles {
                let waiting = *uid != me && !live;
                let radius = theme.radius_lg;
                children.push(
                    div()
                        .relative()
                        .flex_none()
                        .w(px(tile_w))
                        .h(px(tile_h))
                        .child(div().size_full().when(waiting, |this| this.opacity(0.55)).child(tile.clone().cached(StyleRefinement::default().w(px(tile_w)).h(px(tile_h)))))
                        .when(waiting && tile_w > 0., |this| {
                            this.child(div().absolute().inset_0().child(breathing_ring(tile_w, tile_h, f32::from(radius), 2., theme.color_accent, cx)))
                        })
                        .into_any_element(),
                );
            }
            for (_, tile) in &self.streams {
                children.push(tile.clone().cached(StyleRefinement::default().w(px(tile_w)).h(px(tile_h))).into_any_element());
            }
            let docked = dock(false, None, cx);
            let bar = call_bar(peer, live, theater, status, ringing_acked, &duration, latency, path, verified, error, &peer_name, cx);
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_h_0()
                .child(bar)
                .child(
                    div()
                        .relative()
                        .flex_1()
                        .min_h_0()
                        .px_6()
                        .pb_20()
                        .pt_1()
                        .child(tiles_measure)
                        .when(tile_w > 0., |this| {
                            this.child(div().size_full().flex().items_center().justify_center().child(crate::pane::rows(children, cols)))
                        }),
                )
                .child(div().absolute().left_0().right_0().bottom_4().flex().justify_center().child(docked))
                .into_any_element()
        };

        // The compact stage's bottom edge: drag to resize, double click resets.
        let handle = compact.then(|| {
            let resizing = self.resizing.is_some();
            div()
                .id("call-resize")
                .group("call-resize")
                .absolute()
                .left_0()
                .right_0()
                .bottom(px(-4.))
                .h(px(10.))
                .flex()
                .items_center()
                .justify_center()
                .cursor(gpui::CursorStyle::ResizeUpDown)
                .tooltip(title_tooltip("Drag to resize · double-click to reset"))
                .on_mouse_down(
                    MouseButton::Left,
                    cx.listener(move |this, e: &MouseDownEvent, _, cx| {
                        if e.click_count == 2 {
                            let default = match mode {
                                CallStageMode::Stream => CALL_STAGE_STREAM_DEFAULT,
                                CallStageMode::Voice => CALL_STAGE_VOICE_DEFAULT,
                            } as f32;
                            let model = Model::global(cx).clone();
                            model.layout.update(cx, |l, cx| l.set_call_stage_height(mode, default, cx));
                            return;
                        }
                        this.resizing = Some((f32::from(e.position.y), height, height));
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .h_1()
                        .w_12()
                        .rounded_full()
                        .bg(if resizing { c_accent } else { c_border })
                        .group_hover("call-resize", move |s| s.bg(c_accent.opacity(0.7))),
                )
        });
        let track = self.resizing.map(|_| {
            let this = cx.entity().downgrade();
            canvas(|_, _, _| {}, move |_, _, window, _| {
                let a = this.clone();
                window.on_mouse_event(move |e: &MouseMoveEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = a.update(cx, |s, cx| {
                        if let Some((start_y, start_h, _)) = s.resizing {
                            let parent = s.parent_height.get();
                            let max = if parent > 0. { (parent - 48. - CHAT_MIN).max(CALL_STAGE_MIN as f32) } else { f32::MAX };
                            let h = (start_h + f32::from(e.position.y) - start_y).clamp(CALL_STAGE_MIN as f32, max);
                            s.resizing = Some((start_y, start_h, h));
                            cx.notify();
                        }
                    });
                });
                let b = this.clone();
                window.on_mouse_event(move |_: &MouseUpEvent, phase, _, cx| {
                    if phase != DispatchPhase::Bubble {
                        return;
                    }
                    let _ = b.update(cx, |s, cx| {
                        if let Some((_, _, h)) = s.resizing.take() {
                            let model = Model::global(cx).clone();
                            model.layout.update(cx, |l, cx| l.set_call_stage_height(mode, h, cx));
                            cx.notify();
                        }
                    });
                });
            })
            .absolute()
            .size_0()
        });
        let keys = canvas(|_, _, _| {}, move |_, _, window, _| {
            window.on_key_event(move |e: &KeyDownEvent, phase, _, cx| {
                if phase != DispatchPhase::Bubble || e.keystroke.key != "escape" {
                    return;
                }
                let model = Model::global(cx).clone();
                let (fs, focused) = {
                    let w = model.live.voice.watching.read(cx);
                    (w.fullscreen, w.focused)
                };
                if fs {
                    model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(false, cx));
                } else if focused.is_some() {
                    back_to_tiles(cx);
                } else if theater {
                    model.live.calls.update(cx, |c, cx| c.set_theater(false, 0, cx));
                }
            })
        })
        .absolute()
        .size_0();

        let root = div()
            .relative()
            .flex()
            .flex_col()
            .w_full()
            .bg(if fullscreen { black(1.) } else { c_bg_dark })
            .when(!fullscreen, |this| this.border_b_1().border_color(c_border))
            .map(|this| if compact { this.flex_none().h(px(height)) } else { this.flex_1().min_h_0() })
            .child(measure)
            .child(keys)
            .child(body)
            .children(handle)
            .children(track);
        if fullscreen {
            let bounds = decibell_ui::visible_window_bounds(window, cx);
            return decibell_ui::layer(8, anchored().position(bounds.origin).child(div().w(bounds.size.width).h(bounds.size.height).child(root)))
                .into_any_element();
        }
        root.into_any_element()
    }
}

fn toggle_theater(peer: Uid, cx: &mut App) {
    let model = Model::global(cx).clone();
    let on = !model.live.calls.read(cx).theater;
    let baseline = model.dm.list.read(cx).slice(peer).map(|s| s.read(cx).rows().len()).unwrap_or(0);
    model.live.calls.update(cx, |c, cx| c.set_theater(on, baseline, cx));
}

/// The call bar (`CallBar`): state and duration, the verification lock, the engine's error, the
/// ping and path, the theater toggle.
#[allow(clippy::too_many_arguments)]
fn call_bar(
    peer: Uid,
    live: bool,
    theater: bool,
    status: CallStatus,
    ringing_acked: bool,
    duration: &str,
    latency: Option<u32>,
    path: Option<CallPath>,
    verified: Option<bool>,
    error: Option<SharedString>,
    peer_name: &str,
    cx: &mut Context<CallStage>,
) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let meta = theme.scaled.text_meta;
    let pending = if status == CallStatus::Outgoing { if ringing_acked { "Ringing…" } else { "Calling…" } } else { "Connecting…" };
    let state_dot: AnyElement = if live {
        div().size_2().rounded_full().bg(theme.color_success).into_any_element()
    } else {
        breathing_ring(8., 8., 4., 4., theme.color_accent, cx)
    };
    let status_el = div()
        .flex()
        .flex_none()
        .items_center()
        .gap_2()
        .child(state_dot)
        .child(Label::new(if live { "Voice call" } else { pending }).font(FontRole::Meta).size(meta).weight(500.).color(theme.color_text_secondary))
        .when(live, |this| {
            this.child(div().h(px(14.)).w(px(1.)).bg(theme.color_border_divider))
                .child(Label::new(SharedString::from(duration.to_string())).font(FontRole::Meta).size(meta).weight(500.).color(theme.color_text_muted))
        });
    let lock = verified.filter(|_| live).map(|v| {
        let (bg, fg) = if v { (theme.color_success.opacity(0.15), theme.color_success) } else { (theme.color_text_muted.opacity(0.15), theme.color_text_muted) };
        div()
            .id("call-verified")
            .flex()
            .flex_none()
            .h(px(22.))
            .items_center()
            .gap_1p5()
            .rounded(theme.radius_sm)
            .bg(bg)
            .px_2()
            .tooltip(title_tooltip(verified_title(v, peer_name)))
            .child(Icon::new(IconName::ChatLock).size(px(11.)).color(fg))
            .child(Label::new(if v { "Verified" } else { "Not verified" }).font(FontRole::Meta).size(theme.scaled.text_micro).weight(500.).color(fg))
    });
    let error_el = error.filter(|_| live).map(|e| {
        div()
            .min_w_0()
            .rounded(theme.radius_sm)
            .bg(theme.color_error.opacity(0.15))
            .px_2()
            .py_0p5()
            .child(Label::new(e).font(FontRole::Meta).size(theme.scaled.text_micro).color(theme.color_error).truncate())
    });
    let path_label = match path {
        Some(CallPath::Host) => Some(("LAN", "Connected over your local network")),
        Some(CallPath::Srflx) => Some(("Direct", "Connected directly, peer to peer")),
        _ => None,
    };
    let stats = (live && (latency.is_some() || path_label.is_some())).then(|| {
        div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1()
            .when_some(latency, |this, ms| {
                let tone = latency_tone(ms, cx);
                this.child(
                    div()
                        .id("call-latency")
                        .flex()
                        .h_7()
                        .items_center()
                        .gap_1p5()
                        .px_1p5()
                        .tooltip(title_tooltip("Voice latency"))
                        .child(Icon::new(IconName::StageSignal).size(px(13.)).color(tone))
                        .child(Label::new(SharedString::from(format!("{ms} ms"))).font(FontRole::Meta).size(meta).color(tone)),
                )
            })
            .when(latency.is_some() && path_label.is_some(), |this| this.child(div().h_4().w(px(1.)).bg(theme.color_border_divider)))
            .when_some(path_label, |this, (label, title)| {
                this.child(
                    div()
                        .id("call-path")
                        .flex()
                        .h_7()
                        .items_center()
                        .px_1p5()
                        .tooltip(title_tooltip(title))
                        .child(Label::new(label).font(FontRole::Meta).size(meta).color(theme.color_text_muted)),
                )
            })
    });
    let theater_button = live.then(|| {
        let (bg, fg) = if theater { (theme.color_accent_soft, theme.color_accent) } else { (gpui::transparent_black(), theme.color_text_muted) };
        div()
            .id("call-bar-theater")
            .group("call-bar-theater")
            .flex()
            .flex_none()
            .size_7()
            .items_center()
            .justify_center()
            .rounded(theme.radius_sm)
            .bg(bg)
            .when(!theater, |this| this.hover(|s| s.bg(theme.color_surface_hover)))
            .cursor_pointer()
            .tooltip(title_tooltip(if theater { "Exit theater (Esc)" } else { "Theater" }))
            .on_click(move |_: &ClickEvent, _, cx| toggle_theater(peer, cx))
            .child(
                Icon::new(if theater { IconName::StageCollapse } else { IconName::CallTheater })
                    .size(px(15.))
                    .color(fg)
                    .group_hover("call-bar-theater", if theater { fg } else { theme.color_text_secondary }),
            )
    });
    div()
        .flex()
        .flex_none()
        .h_11()
        .items_center()
        .gap_2p5()
        .px_4()
        .child(status_el)
        .children(lock)
        .children(error_el)
        .child(div().flex_1())
        .children(stats)
        .children(theater_button)
}

/// One side of the call over the video (`PeerChip`): avatar with the speaking ring, name, mute /
/// deafen; right click opens the user menu.
fn peer_chip(uid: Uid, local: bool, cx: &mut Context<CallStage>) -> impl IntoElement + use<> {
    let model = Model::global(cx).clone();
    let speaking = model.live.voice.speaking.read(cx).is_speaking(uid);
    let p = model.live.voice.participants.read(cx).list().iter().find(|p| p.uid == uid).cloned();
    let (self_muted, self_deafened) = {
        let s = model.live.voice.session.read(cx);
        (s.muted, s.deafened)
    };
    let name = user_name(uid, &p.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Unknown user".into()), cx);
    let muted = if local { self_muted } else { p.as_ref().is_some_and(|p| p.state.muted) };
    let deafened = if local { self_deafened } else { p.as_ref().is_some_and(|p| p.state.deafened) };
    let theme = cx.theme();
    let error = theme.color_error;
    scrim::scrim_lg(cx)
        .id(("peer-chip", uid as u64))
        .pl_1()
        .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| decibell_common::open_user_menu(uid, e.position, None, cx))
        .child(
            div()
                .rounded(theme.avatar_radius(px(22.)))
                .when(speaking, |this| this.shadow(crate::tiles::ring(on_video_green(), 2.)))
                .child(UserAvatar::new(uid, name.clone(), 22.)),
        )
        .child(scrim::scrim_text(name, white(1.), cx))
        .when(deafened, |this| this.child(Icon::new(IconName::StageHeadphonesOff).size(px(13.)).stroke_width(2.5).color(error)))
        .when(!deafened && muted, |this| this.child(Icon::new(IconName::StageMicOff).size(px(13.)).stroke_width(2.5).color(error)))
}

// ── a stand-in host (screens and self-tests only) ────────────────────────────────────────

/// The DM pane's stand-in where no DM pane is registered (this crate's screens and self-tests):
/// the call stage over the conversation's place. The real host is the DM pane, which shows the
/// view registered for `Slot::DmCallStage` above its message list.
struct DmStandIn {
    stage: Option<gpui::AnyView>,
}

impl Render for DmStandIn {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let theater = Model::global(cx).live.calls.read(cx).theater;
        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme.color_bg_primary)
            .children(self.stage.clone())
            .when(!theater, |this| {
                this.child(
                    div().flex_1().flex().items_center().justify_center().child(
                        Label::new("Conversation").font(FontRole::Display).size(13.).weight(500.).color(theme.color_text_muted),
                    ),
                )
            })
    }
}

/// Host the call stage in a stand-in DM pane when no DM pane is registered.
pub fn host_stage_in_dm_stand_in(cx: &mut App) {
    if decibell_shell::is_registered(cx, decibell_shell::Slot::Dm) {
        return;
    }
    decibell_shell::register_pane(cx, decibell_shell::Slot::Dm, |window, cx| {
        let stage = decibell_shell::build_slot(decibell_shell::Slot::DmCallStage, window, cx);
        cx.new(|_| DmStandIn { stage }).into()
    });
}
