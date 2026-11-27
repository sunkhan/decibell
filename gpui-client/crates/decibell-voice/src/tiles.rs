//! The stage's tiles (`stage/StageTiles.tsx`): a participant, a live stream, the "+N more" tile.
//!
//! Each tile is its own entity drawn as a cached view at the width the stage computed: the stage
//! pushes what it knows (flags, width) and a tile notifies itself only when that changed. A tile
//! subscribes to exactly its own keyed events: speaking for its uid, its person's name and
//! picture, its stream's poster and first picture. A speaking flip redraws that tile and
//! nothing else (the stage replays the others' last paint).
//!
//! Two surface families, as in the renderer: participant tiles sit on the theme (bg-light washed
//! with the person's avatar colour); stream tiles are video, so what is over them is the fixed
//! dark scrim of [`crate::scrim`].

use std::collections::HashSet;

use decibell_common::{BoundsCell, NicknameWatch, UserAvatar, Watch, display_name, open_profile_beside, open_user_menu, user_name, want_member};
use decibell_model::live::voice::speaking::SpeakingChanged;
use decibell_model::live::voice::streams::{StreamInfo, ThumbnailChanged};
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_ui::identity::{avatar_color, person_slot};
use decibell_ui::motion::mix;
use decibell_ui::{ActiveTheme as _, DismissEvent, FontRole, Icon, IconName, Label, Popover, Side, Spinner, title_tooltip};
use decibell_video::{Fit, VideoView, Viewer};
use gpui::{
    AnyElement, App, ClickEvent, Context, Hsla, ImageSource, IntoElement, MouseButton, MouseDownEvent, ObjectFit,
    Render, SharedString, Window, div, img, point, prelude::*, px,
};

use crate::layout::avatar_for;
use crate::scrim::{self, LiveSize, black, white};
use crate::video_events::{StreamStateChanged, StreamStates};

/// `outline-2 -outline-offset-2`: a 2 px ring inside the tile's edge, over its border.
pub fn inset_outline(color: Hsla, radius: gpui::Pixels) -> impl IntoElement {
    // An absolute child sits inside the tile's 1 px border: −1 px puts it on the border box, where
    // `outline-2 -outline-offset-2` draws (the outer 2 px, over the border).
    div().absolute().top(px(-1.)).left(px(-1.)).right(px(-1.)).bottom(px(-1.)).rounded(radius).border_2().border_color(color)
}

/// `ring-N ring-<color>`: a spread shadow of `width` around a box.
pub fn ring(color: Hsla, width: f32) -> Vec<gpui::BoxShadow> {
    vec![gpui::BoxShadow { color, offset: point(px(0.), px(0.)), blur_radius: px(0.), spread_radius: px(width), inset: false }]
}

// ── participant tile ─────────────────────────────────────────────────────────────────────

/// What the stage knows about one participant tile.
#[derive(Debug, Clone, PartialEq)]
pub struct PersonData {
    pub uid: Uid,
    /// The name the roster carried (drawn through the users store).
    pub name: SharedString,
    pub server: Option<ServerId>,
    pub local: bool,
    pub muted: bool,
    pub deafened: bool,
    pub server_muted: bool,
    pub server_deafened: bool,
    /// No SPEAK in this channel (permissions v3).
    pub suppressed: bool,
    pub streaming: bool,
    pub locally_muted: bool,
    pub width: f32,
    /// Filmstrip size: tighter chip, no "you" tag, no LIVE pill.
    pub mini: bool,
}

pub struct ParticipantTile {
    data: PersonData,
    bounds: BoundsCell,
    /// Their nickname in the voice session's community (looked up, followed).
    nickname: NicknameWatch,
    _watch: Watch,
}

impl ParticipantTile {
    pub fn new(data: PersonData, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .event(&model.live.voice.speaking, cx, |this: &mut Self, e: &SpeakingChanged| e.uid == this.data.uid)
            .people(cx, |this: &Self, uid| uid == this.data.uid);
        let mut nickname = NicknameWatch::default();
        nickname.follow(data.server, data.uid, cx);
        Self { data, bounds: BoundsCell::new(), nickname, _watch: watch }
    }

    /// The width the stage gave it.
    pub fn width(&self) -> f32 {
        self.data.width
    }

    /// The stage's latest word on this tile: redraws only when it changed. The width alone needs
    /// no notify: the tile is laid out at it, and a cached view re-renders when its bounds move.
    pub fn set(&mut self, data: PersonData, cx: &mut Context<Self>) {
        if data != self.data {
            let only_width = PersonData { width: self.data.width, ..data.clone() } == self.data;
            self.data = data;
            self.nickname.follow(self.data.server, self.data.uid, cx);
            if !only_width {
                crate::trace("ParticipantTile::set notify");
                cx.notify();
            }
        }
    }
}

impl Render for ParticipantTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("ParticipantTile");
        let d = self.data.clone();
        let model = Model::global(cx).clone();
        let speaking = model.live.voice.speaking.read(cx).is_speaking(d.uid);
        let username = user_name(d.uid, &d.name, cx);
        let shown = display_name(d.server, d.uid, &username, cx);
        let theme = cx.theme();
        let height = crate::layout::tile_height(d.width);
        let icon = if d.mini { 11. } else { 14. };
        let radius = if d.mini { theme.radius_md } else { theme.radius_lg };
        // `color-mix(in srgb, <avatar colour> var(--tile-tint), var(--color-bg-light))`.
        let tint = mix(theme.color_bg_light, avatar_color(theme, person_slot(d.uid, &username)), theme.palette.tile_tint);
        let avatar = avatar_for(height);
        let lift = if height < 140. { if d.mini { 14. } else { 20. } } else { 0. };
        let (success, error, accent, on_error) = (theme.color_success, theme.color_error, theme.color_accent, theme.color_on_error);
        let (meta, micro) = (theme.scaled.text_meta, theme.scaled.text_micro);
        let avatar_radius = theme.avatar_radius(px(avatar));

        let badge = (d.server_muted || d.server_deafened || d.suppressed).then(|| {
            let title = if d.server_deafened {
                "Server deafened by a moderator"
            } else if d.server_muted {
                "Server muted by a moderator"
            } else {
                "Can't speak in this channel"
            };
            div()
                .id("server-mod")
                .flex()
                .flex_none()
                .size_4()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .bg(error)
                .tooltip(title_tooltip(title))
                .child(
                    Icon::new(if d.server_deafened { IconName::StageHeadphonesOff } else { IconName::StageMicOff })
                        .size(px(10.))
                        .stroke_width(2.5)
                        .color(on_error),
                )
        });
        let state_icon = if d.deafened {
            Some((IconName::StageHeadphonesOff, "Deafened"))
        } else if d.muted {
            Some((IconName::StageMicOff, "Muted"))
        } else {
            None
        };
        let chip = div()
            .absolute()
            .flex()
            .items_center()
            .rounded(theme.radius_sm)
            .bg(theme.color_bg_mid.opacity(0.8))
            .map(|this| {
                if d.mini {
                    this.bottom_1().left_1().h(px(18.)).max_w(px(d.width - 8.)).gap_1().px_1p5()
                } else {
                    this.bottom_2().left_2().h(px(26.)).max_w(px(d.width - 16.)).gap_1p5().px_2()
                }
            })
            .child(
                div().min_w_0().child(
                    Label::new(shown.clone())
                        .font(FontRole::Channel)
                        .size(if d.mini { micro } else { meta })
                        .weight(500.)
                        .color(theme.color_text_primary)
                        .truncate(),
                ),
            )
            .when(d.local && !d.mini, |this| {
                this.child(
                    Label::new("you").font(FontRole::Channel).size(meta).color(theme.color_text_muted).nowrap(),
                )
            })
            .children(badge)
            .when_some(state_icon, |this, (name, _title)| {
                this.child(Icon::new(name).size(px(icon)).stroke_width(2.5).color(error))
            })
            .when(d.locally_muted, |this| {
                this.child(Icon::new(IconName::StageSpeakerMuted).size(px(icon)).stroke_width(2.5).color(accent))
            });

        let uid = d.uid;
        let server = d.server;
        let bounds = self.bounds.clone();
        div()
            .id(("participant-tile", uid as u64))
            .group("participant-tile")
            .relative()
            .flex()
            .flex_none()
            .items_center()
            .justify_center()
            .w(px(d.width))
            .h(px(height))
            .pb(px(lift))
            .rounded(radius)
            .border_1()
            .border_color(theme.color_border)
            .bg(tint)
            .shadow(theme.shadow_raised.clone())
            .cursor_pointer()
            .tooltip(title_tooltip(shown))
            .on_click(move |_: &ClickEvent, _, cx| open_profile_beside(uid, bounds.get(), server, cx))
            .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| open_user_menu(uid, e.position, server, cx))
            .child(self.bounds.probe())
            .children(decibell_shell::probes::probe(format!("participant-tile:{uid}")))
            // The hover wash (`group-hover:bg-surface-hover` on an inset layer).
            .child(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .group_hover("participant-tile", |s| s.bg(theme.color_surface_hover)),
            )
            .child(
                div()
                    .relative()
                    .rounded(avatar_radius)
                    // `ring-4 ring-success/25` while speaking; no transition (a fading ring
                    // would repaint for its whole duration on every flip).
                    .when(speaking, |this| this.shadow(ring(success.opacity(0.25), 4.)))
                    .child(UserAvatar::new(uid, username, avatar)),
            )
            .when(d.streaming && !d.mini, |this| this.child(div().absolute().left_2().top_2().child(scrim::live_pill(LiveSize::Md, cx))))
            .child(chip)
            .when(speaking, |this| this.child(inset_outline(success, radius)))
    }
}

// ── stream tile ──────────────────────────────────────────────────────────────────────────

/// What the stage knows about one stream tile.
#[derive(Clone, PartialEq)]
pub struct StreamData {
    pub info: StreamInfo,
    pub server: Option<ServerId>,
    pub watching: bool,
    pub own: bool,
    /// Another stream is watched: the hover offers Switch / Watch too.
    pub watching_others: bool,
    /// Why it can't be played here (codec), if it can't.
    pub blocked: Option<SharedString>,
    pub width: f32,
    /// Filmstrip size: no hover actions (a click focuses), small pills.
    pub mini: bool,
    /// A DM call's stream tile (`CallStreamTile`): no thumbnails, call watch actions.
    pub call: bool,
}

pub struct StreamTile {
    data: StreamData,
    /// Held while watched: the hub decodes the stream while any viewer of it exists.
    viewer: Option<Viewer>,
    /// The owner's nickname in the voice session's community (looked up, followed).
    nickname: NicknameWatch,
    _watch: Watch,
}

impl StreamTile {
    pub fn new(data: StreamData, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let states = StreamStates::global(cx);
        let mut watch = Watch::new();
        watch
            .event(&model.live.voice.thumbnails, cx, |this: &mut Self, e: &ThumbnailChanged| e.0 == this.data.info.owner_uid)
            .event(&states, cx, |this: &mut Self, e: &StreamStateChanged| e.0 == this.data.info.owner_uid)
            .people(cx, |this: &Self, uid| uid == this.data.info.owner_uid);
        let viewer = data.watching.then(|| crate::players::viewer(data.info.owner_uid, cx));
        let mut nickname = NicknameWatch::default();
        nickname.follow(data.server, data.info.owner_uid, cx);
        Self { data, viewer, nickname, _watch: watch }
    }

    pub fn set(&mut self, data: StreamData, cx: &mut Context<Self>) {
        if data == self.data {
            return;
        }
        if data.watching != self.viewer.is_some() {
            self.viewer = data.watching.then(|| crate::players::viewer(data.info.owner_uid, cx));
        }
        let only_width = StreamData { width: self.data.width, ..data.clone() } == self.data;
        self.data = data;
        self.nickname.follow(self.data.server, self.data.info.owner_uid, cx);
        if !only_width {
            cx.notify();
        }
    }
}

/// `ACTION_PRIMARY`: solid white over video. `ACTION_SECONDARY`: outlined white.
pub fn action_button(
    id: &'static str,
    primary: bool,
    icon: Option<(IconName, f32, bool)>,
    label: &'static str,
    title: Option<&'static str>,
    cx: &App,
    on_click: impl Fn(&mut App) + 'static,
) -> impl IntoElement {
    let theme = cx.theme();
    let (bg, hover, fg) = if primary { (white(1.), white(0.9), black(1.)) } else { (white(0.15), white(0.25), white(1.)) };
    div()
        .id(id)
        .flex()
        .flex_none()
        .h_9()
        .items_center()
        .gap_2()
        .rounded(theme.radius_md)
        .px_3p5()
        .bg(bg)
        .hover(move |s| s.bg(hover))
        .when(!primary, |this| this.border_1().border_color(white(0.3)))
        .cursor_pointer()
        .when_some(title, |this, title| this.tooltip(title_tooltip(title)))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_: &ClickEvent, _, cx| {
            cx.stop_propagation();
            on_click(cx);
        })
        .when_some(icon, |this, (name, size, bold)| {
            this.child(Icon::new(name).size(px(size)).stroke_width(if bold { 2.5 } else { 2. }).color(fg))
        })
        .child(Label::new(label).size(13.).weight(600.).color(fg).nowrap())
}

impl Render for StreamTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("StreamTile");
        let d = self.data.clone();
        let owner = d.info.owner_uid;
        let model = Model::global(cx).clone();
        let owner_name = user_name(owner, &d.info.owner_name, cx);
        let shown = display_name(d.server, owner, &owner_name, cx);
        let label: SharedString = if d.own { "Your screen".into() } else { format!("{shown}'s screen").into() };
        let thumbnail = (!d.call).then(|| model.live.voice.thumbnails.read(cx).get(owner).map(|t| t.image.clone())).flatten();
        let state = StreamStates::global(cx).read(cx).get(owner);
        let (has_picture, stalled) = (state.picture.is_some(), state.stalled);
        let theme = cx.theme();
        let height = crate::layout::tile_height(d.width);
        let radius = if d.mini { theme.radius_md } else { theme.radius_lg };
        let can_watch = d.blocked.is_none();

        let content: AnyElement = match &self.viewer {
            Some(viewer) => div()
                .size_full()
                .relative()
                .child(VideoView::new(viewer).fit(Fit::Cover).rounded(radius))
                .when(!has_picture, |this| {
                    this.child(
                        div()
                            .absolute()
                            .inset_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(Spinner::new().size(px(32.)).color(theme.color_accent)),
                    )
                })
                .when(has_picture && stalled, |this| this.child(div().absolute().inset_0().rounded(radius).overflow_hidden().child(crate::scrim::reconnecting(cx))))
                .into_any_element(),
            None => match thumbnail {
                Some(image) => img(ImageSource::Image(image))
                    .size_full()
                    .object_fit(ObjectFit::Cover)
                    .rounded(radius)
                    .into_any_element(),
                None => div()
                    .size_full()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(UserAvatar::new(owner, owner_name.clone(), if d.mini { 28. } else { 56. }))
                    .into_any_element(),
            },
        };

        let actions: Option<AnyElement> = if !can_watch {
            let codec = scrim::short_codec(if d.info.enforced_codec != decibell_model::live::voice::VideoCodec::Unknown {
                d.info.enforced_codec
            } else {
                d.info.current_codec
            });
            let text: SharedString =
                if d.mini { "Unsupported".into() } else { format!("Can't play {}", codec.unwrap_or("this codec")).into() };
            Some(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .flex()
                    .items_center()
                    .justify_center()
                    .bg(black(0.55))
                    .child(
                        scrim::scrim(cx)
                            .child(Icon::new(IconName::StageLock).size(px(12.)).stroke_width(2.5).color(white(1.)))
                            .child(scrim::scrim_text(text, white(1.), cx)),
                    )
                    .into_any_element(),
            )
        } else if d.mini {
            None
        } else {
            let buttons: Vec<AnyElement> = if d.call {
                if d.watching {
                    vec![
                        action_button("focus", true, Some((IconName::StageExpand, 14., false)), "Focus", None, cx, move |cx| {
                            focus_call_stream(owner, cx)
                        })
                        .into_any_element(),
                        action_button("stop", false, Some((IconName::StageClose, 14., true)), "Stop", None, cx, move |cx| {
                            actions::calls::unwatch_call_stream(owner, cx)
                        })
                        .into_any_element(),
                    ]
                } else {
                    vec![
                        action_button(
                            "watch",
                            true,
                            Some((IconName::StagePlay, 12., false)),
                            if d.own { "Preview" } else { "Watch stream" },
                            None,
                            cx,
                            move |cx| actions::calls::watch_call_stream(owner, cx),
                        )
                        .into_any_element(),
                    ]
                }
            } else if d.watching {
                vec![
                    action_button("focus", true, Some((IconName::StageExpand, 14., false)), "Focus", None, cx, move |cx| {
                        actions::voice::focus_stream(owner, cx)
                    })
                    .into_any_element(),
                    action_button("stop", false, Some((IconName::StageClose, 14., true)), "Stop", None, cx, move |cx| {
                        actions::voice::stop_watching_stream(owner, cx)
                    })
                    .into_any_element(),
                ]
            } else if d.own {
                vec![
                    action_button("preview", true, Some((IconName::StagePlay, 12., false)), "Preview", None, cx, move |cx| {
                        actions::voice::focus_stream(owner, cx)
                    })
                    .into_any_element(),
                ]
            } else if d.watching_others {
                vec![
                    action_button(
                        "switch",
                        true,
                        Some((IconName::StagePlay, 12., false)),
                        "Switch",
                        Some("Stop the streams you're watching and watch this one"),
                        cx,
                        move |cx| actions::voice::switch_to_stream(owner, cx),
                    )
                    .into_any_element(),
                    action_button(
                        "watch-too",
                        false,
                        Some((IconName::StagePlus, 14., true)),
                        "Watch too",
                        Some("Keep what you're watching and add this stream"),
                        cx,
                        move |cx| actions::voice::watch_stream_too(owner, cx),
                    )
                    .into_any_element(),
                ]
            } else {
                vec![
                    action_button("watch", true, Some((IconName::StagePlay, 12., false)), "Watch stream", None, cx, move |cx| {
                        actions::voice::focus_stream(owner, cx)
                    })
                    .into_any_element(),
                ]
            };
            Some(
                div()
                    .absolute()
                    .inset_0()
                    .rounded(radius)
                    .flex()
                    .items_center()
                    .justify_center()
                    .gap_2()
                    .bg(black(0.45))
                    .opacity(0.)
                    .group_hover("stream-tile", |s| s.opacity(1.))
                    .children(buttons)
                    .into_any_element(),
            )
        };

        let micro = theme.scaled.text_micro;
        let pills = div()
            .absolute()
            .flex()
            .gap_1p5()
            .map(|this| if d.mini { this.left_1().top_1() } else { this.left_2().top_2() })
            .child(scrim::live_pill(if d.mini { LiveSize::Sm } else { LiveSize::Md }, cx))
            .when(d.watching && !d.mini, |this| {
                this.child(
                    div()
                        .flex()
                        .h_5()
                        .items_center()
                        .gap_1()
                        .rounded(theme.radius_sm)
                        .bg(theme.color_accent)
                        .px_1p5()
                        .child(Icon::new(IconName::StageEye).size(px(12.)).stroke_width(2.5).color(theme.color_on_accent))
                        .child(
                            Label::new("Watching")
                                .font(FontRole::Meta)
                                .size(micro)
                                .weight(600.)
                                .line_height(micro)
                                .color(theme.color_on_accent)
                                .nowrap(),
                        ),
                )
            });

        let footer: AnyElement = if d.mini {
            div()
                .absolute()
                .bottom_1()
                .left_1()
                .flex()
                .h(px(18.))
                .max_w(px(d.width - 8.))
                .items_center()
                .rounded(theme.radius_sm)
                .bg(black(0.7))
                .px_1p5()
                .child(Label::new(label.clone()).font(FontRole::Channel).size(micro).weight(500.).color(white(1.)).truncate())
                .into_any_element()
        } else {
            div()
                .absolute()
                .inset_0()
                .child(div().absolute().right_2().top_2().children(scrim::quality_pill(&d.info, false, cx)))
                .child(
                    div().absolute().bottom_2().left_2().flex().max_w(px(d.width - if d.call { 16. } else { 72. })).child(
                        scrim::scrim(cx)
                            .min_w_0()
                            .child(UserAvatar::new(owner, owner_name.clone(), 16.))
                            .child(div().min_w_0().child(scrim::scrim_text(label.clone(), white(1.), cx).truncate()))
                            .when(d.info.has_audio && !d.call, |this| {
                                this.child(Icon::new(IconName::StageSpeaker).size(px(13.)).color(white(0.7)))
                            }),
                    ),
                )
                .when(d.info.watcher_count > 0 && !d.call, |this| {
                    this.child(
                        div().absolute().bottom_2().right_2().child(
                            scrim::scrim(cx)
                                .child(Icon::new(IconName::StageEye).size(px(13.)).color(white(1.)))
                                .child(scrim::scrim_text(decibell_model::regional::Regional::get().group(d.info.watcher_count as u64), white(1.), cx)),
                        ),
                    )
                })
                .into_any_element()
        };

        let call = d.call;
        let (mini, watching, own, watching_others) = (d.mini, d.watching, d.own, d.watching_others);
        div()
            .id(("stream-tile", owner as u64))
            .group("stream-tile")
            .relative()
            .flex_none()
            .w(px(d.width))
            .h(px(height))
            .rounded(radius)
            .border_1()
            .border_color(theme.color_border)
            .bg(black(1.))
            .shadow(theme.shadow_raised.clone())
            .when(can_watch, |this| this.cursor_pointer())
            .when(!can_watch, |this| this.cursor_not_allowed())
            .tooltip(title_tooltip(d.blocked.clone().unwrap_or_else(|| label.clone())))
            .on_click(move |_: &ClickEvent, _, cx| {
                if !can_watch {
                    return;
                }
                // A click on the tile runs its first hover action; in the filmstrip a focus.
                if call {
                    if watching { focus_call_stream(owner, cx) } else { actions::calls::watch_call_stream(owner, cx) }
                } else if mini || watching || own || !watching_others {
                    actions::voice::focus_stream(owner, cx)
                } else {
                    actions::voice::switch_to_stream(owner, cx)
                }
            })
            .children(decibell_shell::probes::probe(format!("stream-tile:{owner}{}", if mini { ":mini" } else { "" })))
            .child(div().absolute().inset_0().flex().items_center().justify_center().child(content))
            .children(actions)
            .child(pills)
            .child(footer)
            .when(watching, |this| this.child(inset_outline(theme.color_accent, radius)))
    }
}

/// `CallStage`'s focus: the stream takes the stage (it is already watched).
pub fn focus_call_stream(owner: Uid, cx: &mut App) {
    if let Some(model) = cx.try_global::<Model>().cloned() {
        model.live.voice.watching.update(cx, |w, cx| w.set_focused(Some(owner), cx));
    }
}

// ── "+N more" ────────────────────────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq)]
pub struct OverflowData {
    pub hidden: Vec<Uid>,
    /// The roster's names of the first three (the stack's letters).
    pub names: Vec<SharedString>,
    pub width: f32,
    pub mini: bool,
    pub server: Option<ServerId>,
    pub channel_name: SharedString,
}

/// Stands in for the people who don't fit: a few stacked avatars and the count; a click lists
/// them. Lights up while someone in it talks (they swap into view a moment later).
pub struct OverflowTile {
    data: OverflowData,
    hidden: HashSet<Uid>,
    open: bool,
    bounds: BoundsCell,
    /// While the list is open: the community's roster (the rows' nicknames).
    roster: Option<gpui::Subscription>,
    _watch: Watch,
}

impl OverflowTile {
    pub fn new(data: OverflowData, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .event(&model.live.voice.speaking, cx, |this: &mut Self, e: &SpeakingChanged| this.hidden.contains(&e.uid))
            .people(cx, |this: &Self, uid| this.hidden.contains(&uid))
            // The open list shows each one's flags.
            .store_if(&model.live.voice.participants, cx, |this: &mut Self, _, _| this.open);
        let hidden = data.hidden.iter().copied().collect();
        Self { data, hidden, open: false, bounds: BoundsCell::new(), roster: None, _watch: watch }
    }

    /// The open list names everyone hidden (`HiddenRow`'s `useDisplayName`): look up those the
    /// roster lacks.
    fn want_hidden(&self, cx: &mut Context<Self>) {
        if let Some(server) = self.data.server {
            for uid in &self.data.hidden {
                want_member(server, *uid, cx);
            }
        }
    }

    pub fn set(&mut self, data: OverflowData, cx: &mut Context<Self>) {
        if data != self.data {
            let only_width = OverflowData { width: self.data.width, ..data.clone() } == self.data;
            self.hidden = data.hidden.iter().copied().collect();
            self.data = data;
            if self.open {
                self.want_hidden(cx);
            }
            if !only_width {
                cx.notify();
            }
        }
    }

    fn set_open(&mut self, open: bool, cx: &mut Context<Self>) {
        if self.open != open {
            self.open = open;
            self.roster = None;
            if open {
                self.want_hidden(cx);
                let community = self.data.server.and_then(|server| Model::global(cx).community(server, cx));
                self.roster = community.map(|community| cx.observe(&community.roster, |_, _, cx| cx.notify()));
            }
            cx.notify();
        }
    }
}

impl Render for OverflowTile {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("OverflowTile");
        let d = self.data.clone();
        let model = Model::global(cx).clone();
        let speaking = model.live.voice.speaking.read(cx);
        let any_speaking = d.hidden.iter().any(|u| speaking.is_speaking(*u));
        let names: Vec<SharedString> =
            d.hidden.iter().zip(d.names.iter()).take(3).map(|(u, name)| user_name(*u, name, cx)).collect();
        let bounds = self.bounds.clone();
        let popover = self.open.then(|| {
            let b = self.bounds.get();
            // Anchored at the tile's right edge, 8 px above it; flips to end flush with the tile
            // where the list doesn't fit to the right.
            let anchor = point(b.right(), b.top() - px(8.));
            Popover::new("overflow-list", anchor)
                .prefer(Side::Above)
                .on_dismiss(cx.listener(|this, _: &DismissEvent, _, cx| this.set_open(false, cx)))
                .child(overflow_list(&d, cx))
        });
        let theme = cx.theme();
        let height = crate::layout::tile_height(d.width);
        let stack = if d.mini { 20. } else if height < 110. { 24. } else { 28. };
        let radius = if d.mini { theme.radius_md } else { theme.radius_lg };
        let outline = if self.open {
            Some(theme.color_accent)
        } else if any_speaking {
            Some(theme.color_success)
        } else {
            None
        };
        let count = d.hidden.len();
        let avatars = d.hidden.iter().take(3).zip(names).enumerate().map(|(i, (uid, name))| {
            div()
                .rounded(theme.avatar_radius(px(stack)))
                .shadow(ring(theme.color_bg_light, 2.))
                .when(i > 0, |this| this.ml(px(-8.)))
                .child(UserAvatar::new(*uid, name, stack))
        });
        div()
            .id("overflow-tile")
            .group("overflow-tile")
            .relative()
            .flex()
            .flex_col()
            .flex_none()
            .items_center()
            .justify_center()
            .gap_2()
            .w(px(d.width))
            .h(px(height))
            .rounded(radius)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_light)
            .shadow(theme.shadow_raised.clone())
            .cursor_pointer()
            .tooltip(title_tooltip(SharedString::from(format!("{} more in {}", decibell_model::regional::Regional::get().group(count as u64), d.channel_name))))
            .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                let _ = &bounds;
                let open = !this.open;
                this.set_open(open, cx);
            }))
            .child(self.bounds.probe())
            .child(div().absolute().inset_0().rounded(radius).group_hover("overflow-tile", |s| s.bg(theme.color_surface_hover)))
            .child(div().flex().children(avatars))
            .child(
                div()
                    .flex()
                    .items_baseline()
                    .gap_1()
                    .child(
                        Label::new(SharedString::from(format!("+{}", decibell_model::regional::Regional::get().group(count as u64))))
                            .font(FontRole::Channel)
                            .size(if d.mini { theme.scaled.text_micro } else { theme.scaled.text_member })
                            .weight(theme.palette.font_weight_emphasis)
                            .color(theme.color_text_bright),
                    )
                    .when(!d.mini, |this| {
                        this.child(Label::new("more").font(FontRole::Meta).size(theme.scaled.text_micro).color(theme.color_text_muted))
                    }),
            )
            .when_some(outline, |this, color| this.child(inset_outline(color, radius)))
            .children(popover)
    }
}

/// The list of hidden people (`OverflowPopover`): `w-70 p-1.5`, a section heading, one row each.
fn overflow_list(d: &OverflowData, cx: &mut Context<OverflowTile>) -> impl IntoElement + use<> {
    let model = Model::global(cx).clone();
    let theme = cx.theme();
    let heading = div().px_2().pb_1p5().pt_2().child(
        Label::new(SharedString::from(format!("{} MORE IN {}", d.hidden.len(), d.channel_name.to_uppercase())))
            .font(FontRole::Section)
            .size(theme.scaled.text_section)
            .weight(500.)
            .line_height(theme.scaled.text_section)
            .tracking(theme.palette.tracking_section)
            .color(theme.color_text_muted),
    );
    let participants = model.live.voice.participants.read(cx);
    let speaking = model.live.voice.speaking.read(cx);
    let settings = model.settings.read(cx).get();
    let rows: Vec<AnyElement> = d
        .hidden
        .iter()
        .map(|uid| {
            let uid = *uid;
            let p = participants.list().iter().find(|p| p.uid == uid).cloned();
            let fallback: SharedString = p.as_ref().map(|p| p.name.clone()).unwrap_or_else(|| "Unknown user".into());
            let username = user_name(uid, &fallback, cx);
            let shown = display_name(d.server, uid, &username, cx);
            let talking = speaking.is_speaking(uid);
            let local_mute = decibell_model::live::voice::gain::locally_muted(settings, uid);
            let state = p.map(|p| p.state).unwrap_or_default();
            let server = d.server;
            let row_bounds = BoundsCell::new();
            let probe = row_bounds.probe();
            div()
                .id(("hidden-row", uid as u64))
                .group("hidden-row")
                .relative()
                .flex()
                .h_9()
                .items_center()
                .gap_2p5()
                .rounded(theme.radius_sm)
                .px_2()
                .hover(|s| s.bg(theme.color_surface_hover))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    open_profile_beside(uid, row_bounds.get(), server, cx);
                    this.set_open(false, cx);
                }))
                .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| open_user_menu(uid, e.position, server, cx))
                .child(probe)
                .child(
                    div()
                        .flex_none()
                        .rounded(theme.avatar_radius(px(24.)))
                        .when(talking, |this| this.shadow(ring(theme.color_success, 2.)))
                        .child(UserAvatar::new(uid, username, 24.)),
                )
                .child(
                    div().min_w_0().child(
                        Label::new(shown)
                            .font(FontRole::Channel)
                            .size(theme.scaled.text_member)
                            .color(if talking { theme.color_success } else { theme.color_text_secondary })
                            .truncate(),
                    ),
                )
                .child(
                    div()
                        .ml_auto()
                        .flex()
                        .flex_none()
                        .items_center()
                        .gap_1p5()
                        .when(state.server_muted || state.server_deafened || state.suppressed, |this| {
                            this.child(
                                div()
                                    .flex()
                                    .size_4()
                                    .items_center()
                                    .justify_center()
                                    .rounded(theme.radius_sm)
                                    .bg(theme.color_error)
                                    .child(
                                        Icon::new(if state.server_deafened { IconName::StageHeadphonesOff } else { IconName::StageMicOff })
                                            .size(px(10.))
                                            .stroke_width(2.5)
                                            .color(theme.color_on_error),
                                    ),
                            )
                        })
                        .when(state.deafened, |this| {
                            this.child(Icon::new(IconName::StageHeadphonesOff).size(px(14.)).stroke_width(2.5).color(theme.color_error))
                        })
                        .when(!state.deafened && state.muted, |this| {
                            this.child(Icon::new(IconName::StageMicOff).size(px(14.)).stroke_width(2.5).color(theme.color_error))
                        })
                        .when(local_mute, |this| {
                            this.child(Icon::new(IconName::StageSpeakerMuted).size(px(14.)).stroke_width(2.5).color(theme.color_accent))
                        }),
                )
                .into_any_element()
        })
        .collect();
    div().w(px(280.)).p_1p5().child(heading).children(rows)
}
