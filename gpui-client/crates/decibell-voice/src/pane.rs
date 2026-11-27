//! The community voice view (`VoicePanel.tsx`; design: `2026-10-04-voice-view-redesign.md`):
//!
//! ```text
//!   alone    → our tile, large, with an invite / share prompt
//!   grid     → everyone as a 16:9 tile, best-fit to the stage; past the tile-size floor the
//!              tail collapses into "+N more" and hidden speakers swap into view
//!   streams  → live streams large on top, people in one compact row below
//!   focused  → the stream takes the stage, a filmstrip below (crate::focus)
//! ```
//!
//! The dock floats over the bottom of every mode except window fullscreen. The pane computes its
//! layout when what it shows changes (the stores' observers, a new size), never in `render`; the
//! tiles are cached views it pushes their data to.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::cell::Cell;

use decibell_common::{NicknameWatch, Watch};
use decibell_model::live::voice::codecs::can_watch;
use decibell_model::live::voice::session::{Participant, VoiceE2eePhase};
use decibell_model::live::voice::speaking::SpeakingChanged;
use decibell_model::live::voice::streams::StreamInfo;
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::perm;
use decibell_model::{Model, ServerId, Uid};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, ScrollArea, ScrollHandle, title_tooltip};
use gpui::{
    AnyElement, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString,
    StyleRefinement, Subscription, Window, anchored, canvas, div, prelude::*, px,
};

use crate::dock::{Dock, HeaderStats};
use crate::focus::FocusView;
use crate::layout::{self, Mode, Stage, TILE_GAP};
use crate::people::{People, PeopleHost, TileContext};
use crate::tiles::{StreamData, StreamTile};

/// Padding of the stage's scroll box (`px-6 pb-24 pt-6`): the measured content box is inside it.
const PAD_X: f32 = 24.;
const PAD_TOP: f32 = 24.;
const PAD_BOTTOM: f32 = 96.;

/// What the pane draws, computed by [`VoicePane::relayout`].
#[derive(Clone, PartialEq)]
struct Shown {
    stage: Stage,
    people: Vec<Uid>,
    streams: Vec<Uid>,
    stream_w: f32,
    stream_cols: usize,
    channel_name: SharedString,
    focused: Option<Uid>,
    fullscreen: bool,
    can_stream: bool,
    can_invite: bool,
    server_name: SharedString,
    alone: Option<Uid>,
    e2ee: Option<(VoiceE2eePhase, u64, u32, Vec<Uid>)>,
    connected: bool,
}

pub struct VoicePane {
    /// The stage's content box as last laid out (width, height); shared with the canvas that
    /// measures it.
    measured: Rc<Cell<(f32, f32)>>,
    size: (f32, f32),
    shown: Shown,
    people: People,
    stream_tiles: HashMap<Uid, Entity<StreamTile>>,
    stats: Entity<HeaderStats>,
    dock: Entity<Dock>,
    focus: Option<Entity<FocusView>>,
    scroll: ScrollHandle,
    channels: Option<(ServerId, Subscription)>,
    /// The focused stream owner's nickname in the session's community (the header).
    nickname: NicknameWatch,
    _subs: Vec<Subscription>,
    _watch: Watch,
}

impl PeopleHost for VoicePane {
    fn people(&mut self) -> &mut People {
        &mut self.people
    }

    fn relayout(&mut self, cx: &mut Context<Self>) {
        VoicePane::relayout(self, cx);
    }
}

impl VoicePane {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        // Every one of these changes the layout: recompute (which notifies when it moved).
        let subs = vec![
            cx.observe(&voice.session, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.participants, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.streams, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.watching, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.codecs, |this, _, cx| this.relayout(cx)),
            cx.observe(&model.settings, |this, _, cx| this.relayout(cx)),
            cx.observe(&model.nav, |this, _, cx| this.relayout(cx)),
            cx.subscribe(&voice.speaking, |this, _, e: &SpeakingChanged, cx| this.people.speaking(e.uid, e.speaking, cx)),
        ];
        // The header names the focused stream's owner.
        let mut watch = Watch::new();
        watch.people(cx, |this: &VoicePane, uid| this.shown.focused == Some(uid));
        let stats = cx.new(HeaderStats::new);
        let dock = cx.new(|cx| Dock::new(false, cx));
        // Posters are kept while the voice view shows.
        cx.defer(decibell_model::actions::voice::retain_thumbnails);
        cx.on_release(|_, cx| decibell_model::actions::voice::release_thumbnails(cx)).detach();
        crate::codecs::ensure_caps(cx);
        let mut pane = Self {
            _subs: subs,
            measured: Rc::new(Cell::new((0., 0.))),
            size: (0., 0.),
            shown: Shown {
                stage: layout::stage(0, 0, 0., 0.),
                people: Vec::new(),
                streams: Vec::new(),
                stream_w: 0.,
                stream_cols: 1,
                channel_name: "Voice".into(),
                focused: None,
                fullscreen: false,
                can_stream: false,
                can_invite: false,
                server_name: "the server".into(),
                alone: None,
                e2ee: None,
                connected: false,
            },
            people: People::new(false),
            stream_tiles: HashMap::new(),
            stats,
            dock,
            focus: None,
            scroll: ScrollHandle::new(),
            channels: None,
            nickname: NicknameWatch::default(),
            _watch: watch,
        };
        pane.relayout(cx);
        pane
    }

    /// Recompute what the stage shows and push it to the tiles (each redraws only if its data
    /// changed); notify when the pane's own layout moved.
    pub fn relayout(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let me = model.auth.read(cx).uid;
        let (connected, self_muted, self_deafened, e2ee) = {
            let s = voice.session.read(cx);
            (s.connected(), s.muted, s.deafened, s.e2ee.clone())
        };
        self.follow_channels(connected.as_ref().map(|k| k.0), cx);
        let participants: Vec<Participant> = voice.participants.read(cx).list().to_vec();
        let streams: Vec<StreamInfo> = voice.streams.read(cx).session(connected.as_ref(), false);
        let watching = voice.watching.read(cx).clone();
        let decode = voice.codecs.read(cx).decode.clone();
        let streamers: HashSet<Uid> = streams.iter().map(|s| s.owner_uid).collect();
        let mut pinned = streamers.clone();
        if me > 0 {
            pinned.insert(me);
        }
        let (channel_name, server_name, can_stream, can_invite) = match &connected {
            Some((server, channel)) => {
                let community = model.community(*server, cx);
                let name = community
                    .as_ref()
                    .and_then(|c| c.channels.read(cx).get(channel).map(|c| c.name.clone()))
                    .unwrap_or_else(|| "Voice".into());
                let server_name = model.servers.read(cx).get(*server).map(|s| s.name.clone()).unwrap_or_else(|| "the server".into());
                let can_stream = model.channel_permission(*server, channel, perm::STREAM, cx);
                let can_invite = (model.has_permission(*server, perm::CREATE_INVITE, cx)
                    || model.has_permission(*server, perm::MANAGE_INVITES, cx))
                    && model.nav.read(cx).active_server == Some(*server);
                (name, server_name, can_stream, can_invite)
            }
            None => ("Voice".into(), "the server".into(), false, false),
        };
        let (w, h) = self.size;
        let n = participants.len();
        let stage = layout::stage(n, streams.len(), w, h);
        let tile_w = match stage.mode {
            Mode::Alone { tile_w } => tile_w,
            _ => stage.people_w,
        };
        let tcx = TileContext {
            me,
            server: connected.as_ref().map(|k| k.0),
            channel_name: channel_name.clone(),
            participants: &participants,
            self_muted,
            self_deafened,
            streamers: &streamers,
            width: tile_w,
        };
        let placed = self.people.place(stage.slots, pinned, &tcx, cx);
        let alone = matches!(stage.mode, Mode::Alone { .. }).then(|| participants.first().map(|p| p.uid).unwrap_or(me));
        // Alone with nobody listed yet: our own tile stands in (the renderer's `roster[0] ?? ownUid`).
        if let Some(uid) = alone
            && !self.people.tiles.contains_key(&uid)
            && uid > 0
        {
            let name = model.auth.read(cx).username.clone().unwrap_or_default();
            let data = crate::tiles::PersonData {
                uid,
                name,
                server: tcx.server,
                local: uid == me,
                muted: self_muted,
                deafened: self_deafened,
                server_muted: false,
                server_deafened: false,
                suppressed: false,
                streaming: false,
                locally_muted: false,
                width: tile_w,
                mini: false,
            };
            let tile = cx.new(|cx| crate::tiles::ParticipantTile::new(data, cx));
            self.people.tiles.insert(uid, tile);
        }

        // Stream tiles.
        let (stream_cols, stream_w) = match stage.mode {
            Mode::Streams { cols, tile_w } => (cols, tile_w),
            _ => (1, 0.),
        };
        for info in &streams {
            let owner = info.owner_uid;
            let own = owner == me;
            let data = StreamData {
                info: info.clone(),
                server: tcx.server,
                watching: watching.is_watching(owner),
                own,
                watching_others: watching.list().iter().any(|u| *u != owner),
                blocked: if own { None } else { can_watch(info, &decode).err() },
                width: stream_w,
                mini: false,
                call: false,
            };
            match self.stream_tiles.get(&owner) {
                Some(tile) => tile.update(cx, |t, cx| t.set(data, cx)),
                None => {
                    let tile = cx.new(|cx| StreamTile::new(data, cx));
                    self.stream_tiles.insert(owner, tile);
                }
            }
        }
        // While a stream is focused the stage is not drawn: its tiles go (a watched tile would
        // otherwise keep a viewer, i.e. a decoder, for a picture nobody sees).
        let focused = watching.focused.filter(|u| streamers.contains(u));
        if focused.is_some() {
            self.stream_tiles.clear();
        } else {
            self.stream_tiles.retain(|uid, _| streamers.contains(uid));
        }
        // The focused view lives while any stream is watched (its volume and stats toggle are
        // kept across focus changes, as the renderer kept the hidden panel mounted).
        if watching.list().is_empty() {
            self.focus = None;
        } else if self.focus.is_none() {
            self.focus = Some(cx.new(FocusView::new));
        }
        let dock_can_stream = can_stream;
        self.dock.update(cx, |d, cx| d.set_can_stream(dock_can_stream, cx));

        let shown = Shown {
            stage,
            people: placed.shown,
            streams: streams.iter().map(|s| s.owner_uid).collect(),
            stream_w,
            stream_cols,
            channel_name,
            focused,
            fullscreen: watching.fullscreen && focused.is_some(),
            can_stream,
            can_invite,
            server_name,
            alone,
            e2ee: e2ee.map(|e| (e.phase, e.epoch, e.members, e.unverified)),
            connected: connected.is_some(),
        };
        let server = Model::global(cx).live.voice.session.read(cx).server;
        self.nickname.follow(server, shown.focused.unwrap_or(0), cx);
        if shown != self.shown {
            self.shown = shown;
            crate::trace("VoicePane::relayout notify");
            cx.notify();
        }
    }

    /// Keep a subscription to the connected community's channels (the header's name, STREAM).
    fn follow_channels(&mut self, server: Option<ServerId>, cx: &mut Context<Self>) {
        if self.channels.as_ref().map(|(s, _)| *s) == server {
            return;
        }
        self.channels = server.and_then(|server| {
            let community = Model::global(cx).community(server, cx)?;
            Some((server, cx.observe(&community.channels, |this, _, cx| this.relayout(cx))))
        });
    }

    fn set_size(&mut self, size: (f32, f32), cx: &mut Context<Self>) {
        if self.size != size {
            self.size = size;
            self.relayout(cx);
        }
    }
}

impl Render for VoicePane {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("VoicePane");
        let s = self.shown.clone();
        decibell_shell::probes::flag("voice-pane", true);
        let theme = cx.theme();
        let bg = theme.color_bg_mid;

        // The stage's content box, measured as laid out; a change recomputes the layout (one
        // more frame, only when the size moved).
        let measured = self.measured.clone();
        let this = cx.entity().downgrade();
        let probe = canvas(
            move |bounds, window, cx| {
                let w = (f32::from(bounds.size.width) - 2. * PAD_X).floor().max(0.);
                let h = (f32::from(bounds.size.height) - PAD_TOP - PAD_BOTTOM).floor().max(0.);
                if measured.get() != (w, h) {
                    measured.set((w, h));
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = this.update(cx, |p, cx| p.set_size((w, h), cx));
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let header = (!s.fullscreen).then(|| self.header(&s, cx));
        let body: AnyElement = match (s.focused, &self.focus) {
            (Some(_), Some(focus)) if !s.fullscreen => {
                div().flex().flex_1().min_h_0().child(focus.clone().cached(StyleRefinement::default().size_full())).into_any_element()
            }
            (Some(_), Some(focus)) => {
                // Window fullscreen: the focused view over the whole window, title bar included
                // (the renderer's `fixed inset-0 z-50`).
                let bounds = decibell_ui::visible_window_bounds(window, cx);
                decibell_ui::layer(
                    8,
                    anchored()
                        .position(bounds.origin)
                        .child(div().w(bounds.size.width).h(bounds.size.height).child(focus.clone())),
                )
                .into_any_element()
            }
            _ => div()
                .relative()
                .flex_1()
                .min_h_0()
                .child(probe)
                .child(
                    ScrollArea::new("voice-stage")
                        .track_scroll(&self.scroll)
                        .size_full()
                        .child(div().pt(px(PAD_TOP)).pb(px(PAD_BOTTOM)).px(px(PAD_X)).child(self.stage(&s, cx))),
                )
                .into_any_element(),
        };
        let dock = (!s.fullscreen).then(|| {
            div()
                .absolute()
                .left_0()
                .right_0()
                .bottom_4()
                .flex()
                .justify_center()
                .child(self.dock.clone())
        });
        div().relative().size_full().flex().flex_col().bg(bg).children(header).child(body).children(dock)
    }
}

impl VoicePane {
    /// `VoiceHeader`: `h-12 border-b border-border-divider px-4 gap-2.5`, the speaker glyph, the
    /// channel name (or the breadcrumb back to the grid while a stream is focused), the
    /// encryption badge, the stats.
    fn header(&self, s: &Shown, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let model = Model::global(cx).clone();
        let me = model.auth.read(cx).uid;
        let server = model.live.voice.session.read(cx).server;
        let focused_name = s.focused.map(|uid| {
            if uid == me {
                SharedString::new_static("Your screen")
            } else {
                let name = decibell_common::display_name(server, uid, &SharedString::default(), cx);
                SharedString::from(format!("{name}'s screen"))
            }
        });
        let badge = s.connected.then(|| encryption_badge(&s.e2ee, cx));
        let theme = cx.theme();
        let title = |text: SharedString| {
            Label::new(text)
                .font(FontRole::Display)
                .size(theme.scaled.text_title)
                .weight(theme.palette.font_weight_emphasis)
                .tracking(theme.palette.tracking_title)
                .color(theme.color_text_bright)
                .truncate()
        };
        let lead: AnyElement = match focused_name {
            Some(name) => div()
                .flex()
                .min_w_0()
                .items_center()
                .gap_2p5()
                .child(
                    div()
                        .id("voice-breadcrumb")
                        .flex_none()
                        .mx(px(-4.))
                        .px_1()
                        .rounded(theme.radius_sm)
                        .hover(|st| st.bg(theme.color_surface_hover))
                        .cursor_pointer()
                        .tooltip(title_tooltip("Back to the grid (Esc)"))
                        .on_click(|_: &ClickEvent, _, cx| {
                            let model = Model::global(cx).clone();
                            model.live.voice.watching.update(cx, |w, cx| w.set_focused(None, cx));
                        })
                        .child(
                            Label::new(s.channel_name.clone())
                                .font(FontRole::Channel)
                                .size(theme.scaled.text_title)
                                .weight(500.)
                                .tracking(theme.palette.tracking_title)
                                .color(theme.color_text_secondary)
                                .nowrap(),
                        ),
                )
                .child(div().mx(px(-4.)).child(Icon::new(IconName::StageChevronRight).size(px(14.)).color(theme.color_text_muted)))
                .child(div().min_w_0().child(title(name)))
                .into_any_element(),
            None => div().min_w_0().child(title(s.channel_name.clone())).into_any_element(),
        };
        div()
            .flex()
            .flex_none()
            .h_12()
            .items_center()
            .gap_2p5()
            .border_b_1()
            .border_color(theme.color_border_divider)
            .px_4()
            .relative()
            .children(decibell_shell::probes::probe("voice-header"))
            .child(Icon::new(IconName::StageSpeaker).size(px(18.)).color(theme.color_text_muted))
            .child(lead)
            .children(badge)
            .child(div().ml_auto().flex_none().h_7().child(self.stats.clone()))
    }

    /// The stage body for the current mode.
    fn stage(&self, s: &Shown, cx: &mut Context<Self>) -> AnyElement {
        let (w, h) = self.size;
        match s.stage.mode {
            Mode::Unmeasured => div().into_any_element(),
            Mode::Alone { tile_w } => self.alone(s, tile_w, h, cx).into_any_element(),
            Mode::Grid => div()
                .flex()
                .min_h(px(h))
                .items_center()
                .justify_center()
                .child(self.people_rows(s, cx))
                .into_any_element(),
            Mode::Streams { .. } => {
                let tiles: Vec<AnyElement> = s
                    .streams
                    .iter()
                    .filter_map(|uid| self.stream_tiles.get(uid))
                    .map(|t| {
                        let height = layout::tile_height(s.stream_w);
                        t.clone().cached(StyleRefinement::default().w(px(s.stream_w)).h(px(height))).into_any_element()
                    })
                    .collect();
                let _ = w;
                div()
                    .flex()
                    .flex_col()
                    .min_h(px(h))
                    .items_center()
                    .justify_center()
                    .gap_4()
                    .child(rows(tiles, s.stream_cols))
                    .child(self.people_rows(s, cx))
                    .into_any_element()
            }
        }
    }

    /// The people tiles (and "+N more") in rows of the layout's column count, each row centred
    /// (`flex-wrap justify-center gap-3` under a `maxWidth` of that many tiles).
    fn people_rows(&self, s: &Shown, _cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let w = s.stage.people_w;
        let height = layout::tile_height(w);
        let mut tiles: Vec<AnyElement> = s
            .people
            .iter()
            .filter_map(|uid| self.people.tiles.get(uid))
            .map(|t| t.clone().cached(StyleRefinement::default().w(px(w)).h(px(height))).into_any_element())
            .collect();
        if let Some(overflow) = &self.people.overflow {
            tiles.push(overflow.clone().cached(StyleRefinement::default().w(px(w)).h(px(height))).into_any_element());
        }
        rows(tiles, s.stage.people_cols)
    }

    /// `AloneStage`: our tile, large, then "It's just you in …" and the invite / share buttons.
    fn alone(&self, s: &Shown, tile_w: f32, height: f32, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let theme = cx.theme();
        let copy = if s.can_invite {
            format!("Invite people to {}{}", s.server_name, if s.can_stream { ", or share your screen while you wait." } else { "." })
        } else if s.can_stream {
            "Share your screen while you wait for others.".into()
        } else {
            "People show up here as they join.".into()
        };
        let tile = s.alone.and_then(|uid| self.people.tiles.get(&uid)).map(|t| {
            t.clone().cached(StyleRefinement::default().w(px(tile_w)).h(px(layout::tile_height(tile_w))))
        });
        let invite = s.can_invite.then(|| {
            div()
                .id("alone-invite")
                .flex()
                .items_center()
                .gap_2()
                .rounded(theme.radius_sm)
                .bg(theme.color_accent)
                .hover(|st| st.bg(theme.color_accent_hover))
                .px_4()
                .py_2()
                .cursor_pointer()
                .on_click(|_: &ClickEvent, _, cx| {
                    let overlays = Model::global(cx).overlays.clone();
                    overlays.update(cx, |o, cx| o.open_modal(modal::INVITE_MANAGE, cx));
                })
                .child(Icon::new(IconName::StageUserPlus).size(px(16.)).color(theme.color_on_accent))
                .child(Label::new("Invite people").size(13.).weight(600.).color(theme.color_on_accent).nowrap())
        });
        let share = s.can_stream.then(|| {
            div()
                .id("alone-share")
                .flex()
                .items_center()
                .gap_2()
                .rounded(theme.radius_sm)
                .bg(theme.color_surface_hover)
                .hover(|st| st.bg(theme.color_surface_active))
                .px_4()
                .py_2()
                .cursor_pointer()
                .on_click(|_: &ClickEvent, _, cx| crate::state::open_go_live(cx))
                .child(Icon::new(IconName::StageScreen).size(px(16.)).color(theme.color_text_primary))
                .child(Label::new("Share screen").size(13.).weight(600.).color(theme.color_text_primary).nowrap())
        });
        div()
            .flex()
            .flex_col()
            .min_h(px(height))
            .items_center()
            .justify_center()
            .gap(px(22.))
            .children(tile)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1p5()
                    .child(
                        Label::new(SharedString::from(format!("It's just you in {}", s.channel_name)))
                            .font(FontRole::Display)
                            .size(theme.scaled.text_title)
                            .weight(theme.palette.font_weight_emphasis)
                            .tracking(theme.palette.tracking_title)
                            .color(theme.color_text_bright),
                    )
                    .child(Label::new(SharedString::from(copy)).size(13.).color(theme.color_text_muted)),
            )
            .when(s.can_invite || s.can_stream, |this| this.child(div().flex().gap_2().children(invite).children(share)))
    }
}

/// Rows of at most `cols` tiles, each centred, `gap-3` both ways.
pub fn rows(tiles: Vec<AnyElement>, cols: usize) -> impl IntoElement {
    let cols = cols.max(1);
    let mut rows: Vec<AnyElement> = Vec::new();
    let mut tiles = tiles.into_iter().peekable();
    while tiles.peek().is_some() {
        let row: Vec<AnyElement> = tiles.by_ref().take(cols).collect();
        rows.push(div().flex().justify_center().gap(px(TILE_GAP)).children(row).into_any_element());
    }
    div().flex().flex_col().items_center().gap(px(TILE_GAP)).children(rows)
}

/// `VoiceEncryptionBadge`: the connected channel's MLS state.
fn encryption_badge(e2ee: &Option<(VoiceE2eePhase, u64, u32, Vec<Uid>)>, cx: &mut Context<VoicePane>) -> impl IntoElement + use<> {
    let model = Model::global(cx).clone();
    let theme = cx.theme();
    let phase = e2ee.as_ref().map(|e| e.0).unwrap_or(VoiceE2eePhase::Joining);
    let (label, bg, fg, title): (&str, _, _, SharedString) = match phase {
        VoiceE2eePhase::Ready => {
            let (epoch, members) = e2ee.as_ref().map(|e| (e.1, e.2)).unwrap_or((0, 0));
            (
                "Encrypted",
                theme.color_success.opacity(0.15),
                theme.color_success,
                format!("End-to-end encrypted (MLS epoch {epoch}, {} members)", decibell_model::regional::Regional::get().group(members as u64)).into(),
            )
        }
        VoiceE2eePhase::Quarantine => {
            let names: Vec<String> = e2ee
                .as_ref()
                .map(|e| e.3.iter().map(|uid| model.name_of(*uid, &"Unknown user".into(), cx).to_string()).collect())
                .unwrap_or_default();
            (
                "Unverified member",
                theme.color_warning.opacity(0.15),
                theme.color_warning,
                format!("Not sending: {} could not be verified", names.join(", ")).into(),
            )
        }
        VoiceE2eePhase::Failed => {
            ("Encryption failed", theme.color_error.opacity(0.15), theme.color_error, "Leave and rejoin the channel".into())
        }
        _ => (
            "Securing…",
            theme.color_text_muted.opacity(0.15),
            theme.color_text_muted,
            "Joining the channel's encryption group".into(),
        ),
    };
    div()
        .id("voice-e2ee")
        .flex()
        .flex_none()
        .h(px(22.))
        .items_center()
        .gap_1p5()
        .rounded(theme.radius_sm)
        .bg(bg)
        .px_2()
        .tooltip(title_tooltip(title))
        .child(Icon::new(IconName::ChatLock).size(px(11.)).color(fg))
        .child(Label::new(label).font(FontRole::Meta).size(theme.scaled.text_micro).weight(500.).color(fg).nowrap())
}
