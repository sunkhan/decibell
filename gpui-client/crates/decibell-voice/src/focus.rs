//! The focused stream (`StreamViewPanel.tsx`): the stream takes the stage, overlays fade in with
//! the pointer and out 2.5 s after it rests, a filmstrip of the other streams and the people runs
//! below; window fullscreen puts it over the whole window with its own dock. A click on the video
//! goes back to the grid (Esc too); in fullscreen a double click or Esc leaves fullscreen.
//!
//! The overlays' hide timer exists only while they show; the stats overlay refreshes twice a
//! second only while it is open (its numbers are on screen). The video itself costs one present
//! per picture: nothing here redraws for a frame.
//!
//! In fullscreen the mouse cursor goes with the overlays (`cursor-none` while they are hidden):
//! the same one-shot timer, re-armed by pointer movement only, hides it
//! (`App::hide_cursor_until_mouse_moves`, GPUI patch `0013`), and the platform shows it again
//! when the pointer next moves, which is also what brings the overlays back.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use decibell_common::{NicknameWatch, UserAvatar, Watch, display_name, user_name};
use decibell_core::api;
use decibell_model::live::voice::codecs::can_watch;
use decibell_model::live::voice::session::Participant;
use decibell_model::live::voice::speaking::SpeakingChanged;
use decibell_model::live::voice::streams::StreamInfo;
use decibell_model::{Model, Uid, actions};
use decibell_ui::motion::{self, Transition};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, title_tooltip};
use decibell_video::{Fit, StatsSampler, StreamRates, VideoHub, VideoView, Viewer};
use gpui::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, KeyDownEvent, MouseButton, Render,
    SharedString, StyleRefinement, Subscription, Task, Window, canvas, div, prelude::*, px,
};

use crate::dock::{Tone, dock_button, dock_divider};
use crate::layout::{self, STRIP_GAP, STRIP_TILE_W};
use crate::people::{People, PeopleHost, TileContext};
use crate::scrim::{self, LiveSize, black, white};
use crate::state::VoiceUi;
use crate::tiles::{StreamData, StreamTile};
use crate::video_events::{StreamStateChanged, StreamStates};
use crate::volume::VolumeControl;

/// Overlays hide after the pointer has been still this long.
pub const OVERLAY_HIDE: Duration = Duration::from_millis(2500);
/// The stats overlay's refresh while it is open (the renderer published stats every 500 ms).
pub const STATS_REFRESH: Duration = Duration::from_millis(500);

pub struct FocusView {
    /// The stream on stage, and the viewer this view holds of it.
    shown: Option<(Uid, Viewer)>,
    /// `watching.fullscreen` at the last look (entering it arms the overlays' timer).
    fullscreen: bool,
    overlay: bool,
    hide_timer: Option<Task<()>>,
    /// The stream-audio gain: the slider, and our own preview's mute (we'd hear ourselves).
    gain: StreamGain,
    prev_volume: u8,
    stats: bool,
    stats_timer: Option<Task<()>>,
    sampler: StatsSampler,
    rates: Option<StreamRates>,
    filmstrip: Entity<Filmstrip>,
    /// The owner's nickname in the voice session's community (the label).
    nickname: NicknameWatch,
    _subs: Vec<Subscription>,
    _watch: Watch,
}

impl FocusView {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let states = StreamStates::global(cx);
        let subs = vec![
            cx.observe(&voice.watching, |this, _, cx| this.follow(cx)),
            cx.observe(&voice.streams, |this, _, cx| this.follow(cx)),
            cx.observe(&voice.session, |_, _, cx| cx.notify()),
        ];
        let mut watch = Watch::new();
        watch
            .event(&states, cx, |this: &mut Self, e: &StreamStateChanged| this.focused() == Some(e.0))
            .people(cx, |this: &Self, uid| this.focused() == Some(uid));
        let mut this = Self {
            shown: None,
            fullscreen: false,
            overlay: false,
            hide_timer: None,
            gain: StreamGain::new(),
            prev_volume: 100,
            stats: false,
            stats_timer: None,
            sampler: StatsSampler::new(),
            rates: None,
            filmstrip: cx.new(Filmstrip::new),
            nickname: NicknameWatch::default(),
            _subs: subs,
            _watch: watch,
        };
        // Dropped while our own preview has the gain at 0 (the watch list emptied: Stop watching,
        // Stop sharing, leaving): put the slider's value back, as the renderer's unmount cleanup
        // does, or every stream watched later is silent while its slider shows 100 %.
        cx.on_release(|this, cx| {
            if let Some(gain) = this.gain.release() {
                set_stream_volume(gain, cx);
            }
        })
        .detach();
        this.follow(cx);
        this
    }

    fn focused(&self) -> Option<Uid> {
        self.shown.as_ref().map(|(uid, _)| *uid)
    }

    /// Hold a viewer of the focused stream (the new one before the old one goes).
    fn follow(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let voice = &model.live.voice;
        let me = model.auth.read(cx).uid;
        let (connected, in_call) = {
            let s = voice.session.read(cx);
            (s.connected(), s.call_peer.is_some())
        };
        let live: HashSet<Uid> =
            voice.streams.read(cx).session(connected.as_ref(), in_call).iter().map(|s| s.owner_uid).collect();
        let focused = voice.watching.read(cx).focused.filter(|u| live.contains(u));
        self.nickname.follow(connected.as_ref().map(|k| k.0), focused.unwrap_or(0), cx);
        // Entering fullscreen: the overlays show for their 2.5 s and then go with the cursor,
        // also when the pointer never moves again (the button that was under it moved away).
        let fullscreen = voice.watching.read(cx).fullscreen && focused.is_some();
        if std::mem::replace(&mut self.fullscreen, fullscreen) != fullscreen && fullscreen {
            self.poke(cx);
        }
        if focused != self.focused() {
            self.shown = focused.map(|uid| (uid, crate::players::viewer(uid, cx)));
            self.sampler = StatsSampler::new();
            self.rates = None;
            // Our own preview: the (global) stream-audio gain is muted while it shows, restored
            // when it goes (switching to someone else's, or leaving).
            let own = focused == Some(me) && me > 0;
            if let Some(gain) = self.gain.preview(own) {
                set_stream_volume(gain, cx);
            }
            cx.notify();
        }
    }

    /// Pointer moved over the video: show the overlays, hide them after it rests.
    fn poke(&mut self, cx: &mut Context<Self>) {
        if !self.overlay {
            self.overlay = true;
            cx.notify();
        }
        self.hide_timer = Some(cx.spawn(async |this, cx| {
            cx.background_executor().timer(OVERLAY_HIDE).await;
            let _ = this.update(cx, |this, cx| {
                this.hide(cx);
                // The pointer rested on the video: in fullscreen the cursor goes too.
                rest_cursor(cx);
            });
        }));
    }

    /// The pointer rests on a control: keep them.
    fn hold(&mut self, cx: &mut Context<Self>) {
        self.hide_timer = None;
        if !self.overlay {
            self.overlay = true;
            cx.notify();
        }
    }

    fn hide(&mut self, cx: &mut Context<Self>) {
        self.hide_timer = None;
        if self.overlay {
            self.overlay = false;
            cx.notify();
        }
    }

    fn toggle_stats(&mut self, cx: &mut Context<Self>) {
        self.stats = !self.stats;
        self.stats_timer = None;
        if self.stats {
            self.sample(cx);
            self.stats_timer = Some(cx.spawn(async |this, cx| {
                loop {
                    cx.background_executor().timer(STATS_REFRESH).await;
                    if this.update(cx, |this, cx| this.sample(cx)).is_err() {
                        break;
                    }
                }
            }));
        }
        cx.notify();
    }

    fn sample(&mut self, cx: &mut Context<Self>) {
        if let Some((_, viewer)) = &self.shown {
            let stats = viewer.stream().stats();
            self.rates = Some(self.sampler.sample(&stats));
            cx.notify();
        }
    }

    fn set_volume(&mut self, volume: u8, cx: &mut Context<Self>) {
        if let Some(gain) = self.gain.set(volume) {
            set_stream_volume(gain, cx);
            cx.notify();
        }
    }

    fn toggle_volume_mute(&mut self, cx: &mut Context<Self>) {
        if self.gain.volume > 0 {
            self.prev_volume = self.gain.volume;
            self.set_volume(0, cx);
        } else {
            let back = if self.prev_volume > 0 { self.prev_volume } else { 100 };
            self.set_volume(back, cx);
        }
    }
}

/// The pointer has rested over a stream for [`OVERLAY_HIDE`] and the overlays went: while the
/// stream fills the window (`watching.fullscreen`) the mouse cursor is hidden until it moves
/// (`StreamViewPanel.tsx`: `cursor-none` while fullscreen and the overlays are hidden).
pub fn rest_cursor(cx: &mut App) {
    let fullscreen = cx.try_global::<Model>().is_some_and(|m| m.live.voice.watching.read(cx).fullscreen);
    if fullscreen {
        cx.hide_cursor_until_mouse_moves();
        CURSOR_RESTS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    }
}

/// Times [`rest_cursor`] hid the cursor (self-tests).
pub static CURSOR_RESTS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

/// The global stream-audio gain as the focused view drives it (`StreamViewPanel.tsx`'s
/// `streamVolume` and its own-preview effect): the slider's value, and whether the preview of our
/// own stream holds it at 0. Each change returns the gain to hand the core when it moves.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct StreamGain {
    /// The slider, 0–100 (a new view starts at 100, as the renderer's `useState(100)`).
    pub volume: u8,
    preview_muted: bool,
}

impl StreamGain {
    pub fn new() -> Self {
        Self { volume: 100, preview_muted: false }
    }

    /// The focused stream changed: 0 while it is our own, the slider's value when it stops being.
    pub fn preview(&mut self, own: bool) -> Option<f64> {
        if own == self.preview_muted {
            return None;
        }
        self.preview_muted = own;
        Some(if own { 0. } else { self.level() })
    }

    /// The slider moved.
    pub fn set(&mut self, volume: u8) -> Option<f64> {
        if self.volume == volume {
            return None;
        }
        self.volume = volume;
        Some(self.level())
    }

    /// The view goes (the renderer's unmount cleanup, which exists only while the preview is
    /// ours): exactly the slider's value back, or nothing when the preview didn't touch the gain.
    pub fn release(&mut self) -> Option<f64> {
        self.preview(false)
    }

    fn level(&self) -> f64 {
        self.volume as f64 / 100.
    }
}

/// `set_stream_volume` (the global stream-audio gain).
pub fn set_stream_volume(volume: f64, cx: &mut App) {
    if !decibell_model::core::is_running() {
        return;
    }
    cx.background_executor()
        .spawn(async move {
            let _ = api::voice::set_stream_volume(api::voice::SetStreamVolumeArgs { volume }).await;
        })
        .detach();
}

fn back_to_grid(cx: &mut App) {
    let model = Model::global(cx).clone();
    let fullscreen = model.live.voice.watching.read(cx).fullscreen;
    model.live.voice.watching.update(cx, |w, cx| {
        if fullscreen {
            w.set_fullscreen(false, cx);
        }
        w.set_focused(None, cx);
    });
}

fn set_fullscreen(on: bool, cx: &mut App) {
    let model = Model::global(cx).clone();
    model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(on, cx));
}

/// The square button over video (`OverlayButton`): `h-8 w-8 rounded-sm border border-white/10`,
/// `bg-black/70 text-white hover:bg-black/85`, or the accent while active.
pub fn overlay_button(
    id: &'static str,
    title: &'static str,
    icon: IconName,
    active: bool,
    cx: &App,
    on_click: impl Fn(&mut Window, &mut App) + 'static,
) -> impl IntoElement {
    let theme = cx.theme();
    let (bg, hover, fg) = if active {
        (theme.color_accent, theme.color_accent, theme.color_on_accent)
    } else {
        (black(0.7), black(0.85), white(1.))
    };
    div()
        .id(id)
        .flex()
        .flex_none()
        .size_8()
        .items_center()
        .justify_center()
        .rounded(theme.radius_sm)
        .border_1()
        .border_color(white(0.1))
        .bg(bg)
        .hover(move |s| s.bg(hover))
        .cursor_pointer()
        .tooltip(title_tooltip(title))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(move |_: &ClickEvent, window, cx| {
            cx.stop_propagation();
            on_click(window, cx);
        })
        .relative()
        .children(decibell_shell::probes::probe(id))
        .child(Icon::new(icon).size(px(16.)).color(fg))
}

/// `StreamStatsOverlay`: `min-w-[150px] rounded-md border border-white/10 bg-black/70 px-3 py-2
/// font-mono text-[11px] leading-relaxed text-white/90 shadow-float`. One row more than the
/// renderer's: how the stream is decoded (hardware / software), and for our own stream the
/// encoder, which the native client reports (the owner's "report which path is in use").
pub fn stats_overlay(info: &StreamInfo, viewer: &Viewer, rates: Option<StreamRates>, encoder: Option<SharedString>, cx: &App) -> impl IntoElement {
    let stats = viewer.stream().stats();
    let theme = cx.theme();
    let codec = stats.codec.map(|c| c.label()).unwrap_or_else(|| scrim::short_codec(info.current_codec).unwrap_or("—"));
    let resolution = if stats.width > 0 { format!("{}×{}", stats.width, stats.height) } else { "—".into() };
    let rates = rates.unwrap_or_default();
    let mut rows: Vec<(&str, String, bool)> = vec![
        ("Codec", codec.to_string(), false),
        ("Resolution", resolution, false),
        ("Input", format!("{} fps", rates.input_fps.round()), false),
        ("Decode", format!("{} fps", rates.decoded_fps.round()), false),
        ("Queue", stats.queue.to_string(), stats.queue > 3),
        ("Dropped", decibell_model::regional::Regional::get().group(stats.dropped), stats.dropped > 0),
        ("Path", stats.path.map(|p| p.label().to_string()).unwrap_or_else(|| "—".into()), false),
    ];
    if let Some(encoder) = encoder.filter(|e| !e.is_empty()) {
        rows.push(("Encoder", encoder.to_string(), false));
    }
    div()
        .min_w(px(150.))
        .rounded(theme.radius_md)
        .border_1()
        .border_color(white(0.1))
        .bg(black(0.7))
        .px_3()
        .py_2()
        .shadow(theme.shadow_float.clone())
        .children(rows.into_iter().map(|(k, v, warn)| {
            div()
                .flex()
                .justify_between()
                .gap_4()
                .child(Label::new(k).font(FontRole::Mono).size(11.).line_height(17.875).color(white(0.5)))
                .child(
                    Label::new(SharedString::from(v))
                        .font(FontRole::Mono)
                        .size(11.)
                        .line_height(17.875)
                        .color(if warn { theme.color_warning } else { white(0.9) }),
                )
        }))
}

impl Render for FocusView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("FocusView");
        let model = Model::global(cx).clone();
        let me = model.auth.read(cx).uid;
        let fullscreen = model.live.voice.watching.read(cx).fullscreen;
        let (connected, in_call, muted, deafened) = {
            let s = model.live.voice.session.read(cx);
            (s.connected(), s.call_peer.is_some(), s.muted, s.deafened)
        };
        let Some((uid, viewer)) = self.shown.clone() else { return div().size_full().into_any_element() };
        let Some(info) = model.live.voice.streams.read(cx).session(connected.as_ref(), in_call).into_iter().find(|s| s.owner_uid == uid)
        else {
            return div().size_full().into_any_element();
        };
        let own = uid == me;
        let server = connected.as_ref().map(|k| k.0);
        let owner_name = user_name(uid, &info.owner_name, cx);
        let shown_name = display_name(server, uid, &owner_name, cx);
        let label: SharedString = if own { "Your screen".into() } else { format!("{shown_name}'s screen").into() };
        let state = StreamStates::global(cx).read(cx).get(uid);
        let (has_picture, stalled) = (state.picture.is_some(), state.stalled);
        let encoder = own.then(|| model.live.voice.own_stream.read(cx).live().map(|l| l.encoder.clone())).flatten();
        let show_volume = !own && info.has_audio;
        // The overlays fade in 150 ms (`transition-opacity duration-150`): frames only while
        // they move.
        let t = motion::transition_flag(window, cx, "focus-overlay", self.overlay, Transition::new(150, motion::EASE));
        let theme = cx.theme();
        let radius = theme.radius_lg;
        // Escape steps back: fullscreen → the panel → the grid.
        let keys = canvas(|_, _, _| {}, move |_, _, window, _| {
            window.on_key_event(move |e: &KeyDownEvent, phase, _, cx| {
                if phase == gpui::DispatchPhase::Bubble && e.keystroke.key == "escape" {
                    if fullscreen { set_fullscreen(false, cx) } else { back_to_grid(cx) }
                }
            });
        })
        .absolute()
        .size_0();
        // Where the video sits, for the mini-player's shrink.
        let ui = VoiceUi::try_global(cx);
        let record = canvas(
            move |bounds, _, cx| {
                if let Some(ui) = &ui {
                    ui.update(cx, |u, _| u.record_full_rect(bounds));
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();

        let video_box = div()
            .id("focus-video")
            .relative()
            .flex()
            .flex_1()
            .min_h_0()
            .min_w_0()
            .items_center()
            .justify_center()
            .overflow_hidden()
            .bg(black(1.))
            .when(!fullscreen, |this| {
                this.cursor_pointer().rounded(radius).border_1().border_color(theme.color_border).on_click(
                    |e: &ClickEvent, _, cx| {
                        if e.click_count() == 1 {
                            back_to_grid(cx)
                        }
                    },
                )
            })
            .when(fullscreen, |this| {
                // `cursor-none` while the overlays are hidden: `rest_cursor`, from the hide timer.
                this.on_click(|e: &ClickEvent, _, cx| {
                    if e.click_count() == 2 {
                        set_fullscreen(false, cx)
                    }
                })
            })
            .on_mouse_move(cx.listener(|this, _, _, cx| this.poke(cx)))
            .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                if !*hovered {
                    this.hide(cx)
                }
            }))
            .children(decibell_shell::probes::probe("focus-video"))
            .child(record)
            .child(keys)
            .child(
                div().size_full().child(
                    VideoView::new(&viewer).fit(Fit::Contain).when(!fullscreen, |v| v.rounded(radius - px(1.))),
                ),
            )
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
            .when(has_picture && stalled, |this| this.child(scrim::reconnecting(cx)))
            .when(self.stats, |this| {
                this.child(div().absolute().left_3().top(px(56.)).child(stats_overlay(&info, &viewer, self.rates, encoder.clone(), cx)))
            })
            // Top left: what this is.
            .child(
                div()
                    .absolute()
                    .left_3()
                    .top_3()
                    .flex()
                    .gap_1p5()
                    .opacity(t)
                    .child(scrim::live_pill(LiveSize::Lg, cx))
                    .child(
                        scrim::scrim_lg(cx)
                            .child(UserAvatar::new(uid, owner_name.clone(), 18.))
                            .child(scrim::scrim_text(label, white(1.), cx)),
                    )
                    .children(scrim::quality_pill(&info, true, cx)),
            )
            // Top right: the view controls.
            .child(
                div()
                    .id("focus-controls")
                    .absolute()
                    .right_3()
                    .top_3()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .opacity(t)
                    .when(t == 0., |this| this.invisible())
                    .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                        if *hovered {
                            this.hold(cx)
                        }
                    }))
                    .when(fullscreen, |this| {
                        this.child(div().mr_1p5().child(Label::new("Esc to exit").size(11.).color(white(0.5)).nowrap()))
                    })
                    .child(overlay_button("focus-stats", "Stream stats", IconName::StageStats, self.stats, cx, {
                        let this = cx.entity().downgrade();
                        move |_, cx| {
                            let _ = this.update(cx, |this, cx| this.toggle_stats(cx));
                        }
                    }))
                    .child(overlay_button(
                        "focus-fullscreen",
                        if fullscreen { "Exit fullscreen" } else { "Fullscreen" },
                        if fullscreen { IconName::StageCollapse } else { IconName::StageExpand },
                        false,
                        cx,
                        move |_, cx| set_fullscreen(!fullscreen, cx),
                    ))
                    .child(overlay_button("focus-stop", "Stop watching", IconName::StageClose, false, cx, move |_, cx| {
                        stop_watching(uid, cx)
                    })),
            )
            .when(!fullscreen && show_volume, |this| {
                let me = cx.entity().downgrade();
                let me2 = me.clone();
                this.child(
                    div().absolute().left_3().bottom_3().opacity(t).when(t == 0., |this| this.invisible()).child(
                        VolumeControl::new("focus-volume", self.gain.volume)
                            .on_change(move |v, _, cx| {
                                let _ = me.update(cx, |this, cx| this.set_volume(v, cx));
                            })
                            .on_toggle_mute(move |_, cx| {
                                let _ = me2.update(cx, |this, cx| this.toggle_volume_mute(cx));
                            }),
                    ),
                )
            })
            .when(fullscreen, |this| {
                let me = cx.entity().downgrade();
                let me2 = me.clone();
                let volume = self.gain.volume;
                this.child(
                    div()
                        .absolute()
                        .left_0()
                        .right_0()
                        .bottom_6()
                        .flex()
                        .justify_center()
                        .opacity(t)
                        .when(t == 0., |this| this.invisible())
                        .child(
                            div()
                                .id("focus-fullscreen-dock")
                                .flex()
                                .items_center()
                                .gap_1p5()
                                .rounded(radius)
                                .border_1()
                                .border_color(white(0.1))
                                .bg(black(0.7))
                                .p_1p5()
                                .shadow(theme.shadow_float.clone())
                                .on_hover(cx.listener(|this, hovered: &bool, _, cx| {
                                    if *hovered {
                                        this.hold(cx)
                                    }
                                }))
                                .child(dock_button(
                                    "fs-mute",
                                    if muted { "Unmute" } else { "Mute" },
                                    if muted { IconName::StageMicOff } else { IconName::StageMic },
                                    if muted { Tone::Danger } else { Tone::Soft },
                                    true,
                                    cx,
                                    |_, _, cx| actions::voice::toggle_mute(cx),
                                ))
                                .child(dock_button(
                                    "fs-deafen",
                                    if deafened { "Undeafen" } else { "Deafen" },
                                    if deafened { IconName::StageHeadphonesOff } else { IconName::StageHeadphones },
                                    if deafened { Tone::Danger } else { Tone::Soft },
                                    true,
                                    cx,
                                    |_, _, cx| actions::voice::toggle_deafen(cx),
                                ))
                                .when(show_volume, |this| {
                                    this.child(dock_divider(true, cx)).child(
                                        VolumeControl::new("fs-volume", volume)
                                            .on_change(move |v, _, cx| {
                                                let _ = me.update(cx, |this, cx| this.set_volume(v, cx));
                                            })
                                            .on_toggle_mute(move |_, cx| {
                                                let _ = me2.update(cx, |this, cx| this.toggle_volume_mute(cx));
                                            }),
                                    )
                                })
                                .child(dock_divider(true, cx))
                                .child(dock_button(
                                    "fs-exit",
                                    "Exit fullscreen",
                                    IconName::StageCollapse,
                                    Tone::Soft,
                                    true,
                                    cx,
                                    |_, _, cx| set_fullscreen(false, cx),
                                )),
                        ),
                )
            });

        if fullscreen {
            return div().size_full().flex().flex_col().bg(black(1.)).child(video_box).into_any_element();
        }
        div()
            .size_full()
            .flex()
            .flex_col()
            .gap_3()
            .px_4()
            .pt_4()
            .pb(px(84.))
            .child(video_box)
            .child(div().h(px(72.)).flex_none().child(self.filmstrip.clone().cached(StyleRefinement::default().w_full().h(px(72.)))))
            .into_any_element()
    }
}

/// "Stop watching" over the focused video (`handleStopWatching`): the community watch or the
/// call's, the watch gone, fullscreen left.
pub fn stop_watching(uid: Uid, cx: &mut App) {
    let model = Model::global(cx).clone();
    let me = model.auth.read(cx).uid;
    let in_call = model.live.voice.session.read(cx).call_peer.is_some();
    let fullscreen = model.live.voice.watching.read(cx).fullscreen;
    if in_call {
        if uid != me {
            actions::calls::unwatch_call_stream(uid, cx);
        } else {
            model.live.voice.watching.update(cx, |w, cx| w.remove(uid, cx));
        }
    } else {
        actions::voice::stop_watching_stream(uid, cx);
    }
    if fullscreen {
        set_fullscreen(false, cx);
    }
}

// ── the filmstrip ───────────────────────────────────────────────────────────────────────

/// The row under a focused stream (`Filmstrip`): the other live streams first (watched ones keep
/// playing; a click focuses), then people as mini tiles in one row with the same "+N more" and
/// hidden-speaker swap as the grid.
pub struct Filmstrip {
    width: f32,
    measured: Rc<Cell<f32>>,
    people: People,
    streams: HashMap<Uid, Entity<StreamTile>>,
    order: Vec<Uid>,
    shown: Vec<Uid>,
    _subs: Vec<Subscription>,
}

impl PeopleHost for Filmstrip {
    fn people(&mut self) -> &mut People {
        &mut self.people
    }

    fn relayout(&mut self, cx: &mut Context<Self>) {
        Filmstrip::relayout(self, cx)
    }
}

impl Filmstrip {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let subs = vec![
            cx.observe(&voice.session, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.participants, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.streams, |this, _, cx| this.relayout(cx)),
            cx.observe(&voice.watching, |this, _, cx| this.relayout(cx)),
            cx.observe(&model.settings, |this, _, cx| this.relayout(cx)),
            cx.subscribe(&voice.speaking, |this, _, e: &SpeakingChanged, cx| this.people.speaking(e.uid, e.speaking, cx)),
        ];
        let mut this = Self {
            width: 0.,
            measured: Rc::new(Cell::new(0.)),
            people: People::new(true),
            streams: HashMap::new(),
            order: Vec::new(),
            shown: Vec::new(),
            _subs: subs,
        };
        this.relayout(cx);
        this
    }

    pub fn relayout(&mut self, cx: &mut Context<Self>) {
        let model = Model::global(cx).clone();
        let voice = model.live.voice.clone();
        let me = model.auth.read(cx).uid;
        let (connected, muted, deafened) = {
            let s = voice.session.read(cx);
            (s.connected(), s.muted, s.deafened)
        };
        let watching = voice.watching.read(cx).clone();
        let streams: Vec<StreamInfo> = voice.streams.read(cx).session(connected.as_ref(), false);
        let decode = voice.codecs.read(cx).decode.clone();
        let participants: Vec<Participant> = voice.participants.read(cx).list().to_vec();
        let focused = watching.focused;
        let others: Vec<StreamInfo> = streams.iter().filter(|s| Some(s.owner_uid) != focused).cloned().collect();
        let streamers: HashSet<Uid> = streams.iter().map(|s| s.owner_uid).collect();
        let mut pinned = streamers.clone();
        if me > 0 {
            pinned.insert(me);
        }
        let strip = layout::strip(self.width, others.len(), participants.len());
        let channel_name = connected
            .as_ref()
            .and_then(|(server, channel)| model.community(*server, cx).and_then(|c| c.channels.read(cx).get(channel).map(|c| c.name.clone())))
            .unwrap_or_else(|| "Voice".into());
        let tcx = TileContext {
            me,
            server: connected.as_ref().map(|k| k.0),
            channel_name,
            participants: &participants,
            self_muted: muted,
            self_deafened: deafened,
            streamers: &streamers,
            width: STRIP_TILE_W,
        };
        let placed = self.people.place(strip.slots, pinned, &tcx, cx);
        if !strip.overflow {
            self.people.overflow = None;
        }
        let order: Vec<Uid> = others.iter().take(strip.stream_slots).map(|s| s.owner_uid).collect();
        for info in others.iter().take(strip.stream_slots) {
            let owner = info.owner_uid;
            let data = StreamData {
                info: info.clone(),
                server: tcx.server,
                watching: watching.is_watching(owner),
                own: owner == me,
                watching_others: true,
                blocked: if owner == me { None } else { can_watch(info, &decode).err() },
                width: STRIP_TILE_W,
                mini: true,
                call: false,
            };
            match self.streams.get(&owner) {
                Some(tile) => tile.update(cx, |t, cx| t.set(data, cx)),
                None => {
                    let tile = cx.new(|cx| StreamTile::new(data, cx));
                    self.streams.insert(owner, tile);
                }
            }
        }
        self.streams.retain(|uid, _| order.contains(uid));
        if (order.as_slice(), placed.shown.as_slice()) != (self.order.as_slice(), self.shown.as_slice()) {
            self.order = order;
            self.shown = placed.shown;
            cx.notify();
        }
    }
}

impl Render for Filmstrip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("Filmstrip");
        let measured = self.measured.clone();
        let this = cx.entity().downgrade();
        let probe = canvas(
            move |bounds, window, cx| {
                let w = f32::from(bounds.size.width).floor();
                if measured.get() != w {
                    measured.set(w);
                    let this = this.clone();
                    window.defer(cx, move |_, cx| {
                        let _ = this.update(cx, |f, cx| {
                            f.width = w;
                            f.relayout(cx);
                        });
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let h = layout::tile_height(STRIP_TILE_W);
        let tile = |e: AnyElement| e;
        let mut children: Vec<AnyElement> = self
            .order
            .iter()
            .filter_map(|uid| self.streams.get(uid))
            .map(|t| tile(t.clone().cached(StyleRefinement::default().w(px(STRIP_TILE_W)).h(px(h))).into_any_element()))
            .collect();
        children.extend(
            self.shown
                .iter()
                .filter_map(|uid| self.people.tiles.get(uid))
                .map(|t| t.clone().cached(StyleRefinement::default().w(px(STRIP_TILE_W)).h(px(h))).into_any_element()),
        );
        if let Some(overflow) = &self.people.overflow {
            children.push(overflow.clone().cached(StyleRefinement::default().w(px(STRIP_TILE_W)).h(px(h))).into_any_element());
        }
        div().relative().size_full().flex().justify_center().gap(px(STRIP_GAP)).child(probe).children(children)
    }
}

/// The renderer's `VideoHub` handle for tests.
pub fn hub(cx: &mut App) -> VideoHub {
    VideoHub::global(cx)
}

#[cfg(test)]
mod tests {
    use super::StreamGain;

    #[test]
    fn our_own_preview_mutes_the_stream_gain_and_releasing_the_view_restores_it_exactly() {
        // The slider at 60 %, then our own stream focused: 0.
        let mut gain = StreamGain::new();
        assert_eq!(gain.set(60), Some(0.6));
        assert_eq!(gain.preview(true), Some(0.0));
        assert_eq!(gain.preview(true), None, "still ours: nothing sent");
        // The view is dropped while previewing (Stop watching / Stop sharing / leaving): exactly
        // the slider's value comes back, once.
        assert_eq!(gain.release(), Some(0.6));
        assert_eq!(gain.release(), None);

        // Switching from our preview to someone else's stream restores it too.
        let mut gain = StreamGain::new();
        assert_eq!(gain.preview(true), Some(0.0));
        assert_eq!(gain.preview(false), Some(1.0));
        assert_eq!(gain.release(), None, "a view that isn't previewing leaves the gain alone");

        // A view that never showed our stream never touches the gain when it goes.
        let mut gain = StreamGain::new();
        assert_eq!(gain.set(35), Some(0.35));
        assert_eq!(gain.set(35), None);
        assert_eq!(gain.preview(false), None);
        assert_eq!(gain.release(), None);

        // Muted by the slider (0), then our preview, then released: back to 0, not 100.
        let mut gain = StreamGain::new();
        assert_eq!(gain.set(0), Some(0.0));
        assert_eq!(gain.preview(true), Some(0.0));
        assert_eq!(gain.release(), Some(0.0));
    }
}
