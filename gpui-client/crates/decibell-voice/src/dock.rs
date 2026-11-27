//! The floating control dock (`VoicePanel.tsx`' `VoiceDock`): mic and headphones │ screen share
//! (+ the stream-audio app picker while live) │ leave. And the header's right side
//! (`HeaderStats`: participant count, voice ping), its own view so the 2 s ping redraws only it.

use decibell_common::{BoundsCell, Watch};
use decibell_model::live::voice::go_live::StreamSettings;
use decibell_model::{Model, actions};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, title_tooltip};
use gpui::{App, ClickEvent, Context, Hsla, IntoElement, MouseButton, Render, SharedString, Window, div, prelude::*, px};

use crate::state::{PanelPopover, VoiceUi};

/// `DOCK_TONES`.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Tone {
    /// `bg-surface-hover text-text-secondary hover:bg-surface-active hover:text-text-bright`.
    Soft,
    /// `bg-error/15 text-error hover:bg-error/25`.
    Danger,
    /// `bg-accent text-on-accent hover:bg-accent-hover`.
    On,
}

/// The over-video twins (`VIDEO_TONES`, the fullscreen dock's).
pub fn tone_colors(tone: Tone, over_video: bool, cx: &App) -> (Hsla, Hsla, Hsla, Hsla) {
    use crate::scrim::white;
    let t = cx.theme();
    match (tone, over_video) {
        (Tone::Soft, false) => (t.color_surface_hover, t.color_surface_active, t.color_text_secondary, t.color_text_bright),
        (Tone::Soft, true) => (gpui::transparent_black(), white(0.1), white(0.8), white(1.)),
        (Tone::Danger, false) => (t.color_error.opacity(0.15), t.color_error.opacity(0.25), t.color_error, t.color_error),
        (Tone::Danger, true) => (t.color_error.opacity(0.2), t.color_error.opacity(0.3), t.color_error, t.color_error),
        (Tone::On, _) => (t.color_accent, t.color_accent_hover, t.color_on_accent, t.color_on_accent),
    }
}

/// `DockButton`: `h-10 w-10 rounded-md`, an 18 px glyph.
pub fn dock_button(
    id: &'static str,
    title: &'static str,
    icon: IconName,
    tone: Tone,
    over_video: bool,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> gpui::Stateful<gpui::Div> {
    let (bg, bg_hover, fg, fg_hover) = tone_colors(tone, over_video, cx);
    div()
        .id(id)
        .group(id)
        .flex()
        .flex_none()
        .size_10()
        .items_center()
        .justify_center()
        .rounded(cx.theme().radius_md)
        .bg(bg)
        .hover(move |s| s.bg(bg_hover))
        .cursor_pointer()
        .tooltip(title_tooltip(title))
        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
        .on_click(on_click)
        .relative()
        .children(decibell_shell::probes::probe(id))
        .child(Icon::new(icon).size(px(18.)).color(fg).group_hover(id, fg_hover))
}

/// `mx-0.5 h-6 w-px bg-border-divider` (over video `bg-white/10`).
pub fn dock_divider(over_video: bool, cx: &App) -> impl IntoElement + use<> {
    let color = if over_video { crate::scrim::white(0.1) } else { cx.theme().color_border_divider };
    div().mx_0p5().h_6().w(px(1.)).flex_none().bg(color)
}

/// The stream-audio app picker's button (`StreamAudioButton`): only while a stream with sound is
/// live where apps can be picked. Opens the picker in a popover over it.
pub fn stream_audio_button(id: &'static str, over_video: bool, bounds: &BoundsCell, cx: &App) -> Option<impl IntoElement + use<>> {
    let model = Model::global(cx);
    let live = model.live.voice.own_stream.read(cx).is_live();
    let share_audio = StreamSettings::of(model.settings.read(cx).get()).share_audio;
    if !(live && share_audio && actions::voice::can_pick_stream_audio_apps()) {
        return None;
    }
    let bounds = bounds.clone();
    Some(
        dock_button(id, "Stream sound", IconName::StageSpeaker, Tone::Soft, over_video, cx, {
            let bounds = bounds.clone();
            move |_, _, cx| {
                let ui = VoiceUi::global(cx);
                ui.update(cx, |u, cx| u.toggle_popover(PanelPopover::StreamAudio(bounds.get()), cx));
            }
        })
        .relative()
        .child(bounds.probe()),
    )
}

pub struct Dock {
    /// STREAM in the connected channel (permissions v2: resolved per channel by the server).
    can_stream: bool,
    audio_bounds: BoundsCell,
    _watch: Watch,
}

impl Dock {
    pub fn new(can_stream: bool, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store(&model.live.voice.session, cx)
            .store(&model.live.voice.own_stream, cx)
            .store_if(&model.settings, cx, |this: &mut Self, s, _| {
                let _ = this;
                let _ = s;
                true
            });
        Self { can_stream, audio_bounds: BoundsCell::new(), _watch: watch }
    }

    pub fn set_can_stream(&mut self, can: bool, cx: &mut Context<Self>) {
        if self.can_stream != can {
            self.can_stream = can;
            cx.notify();
        }
    }
}

impl Render for Dock {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("Dock");
        let model = Model::global(cx).clone();
        let (muted, deafened) = {
            let s = model.live.voice.session.read(cx);
            (s.muted, s.deafened)
        };
        let streaming = model.live.voice.own_stream.read(cx).is_live();
        let audio = stream_audio_button("dock-stream-audio", false, &self.audio_bounds, cx);
        let theme = cx.theme();
        div()
            .id("voice-dock")
            .occlude()
            .flex()
            .items_center()
            .gap_1p5()
            .rounded(theme.radius_lg)
            .border_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_light)
            .p_1p5()
            .shadow(theme.shadow_float.clone())
            .child(dock_button(
                "dock-mute",
                if muted { "Unmute" } else { "Mute" },
                if muted { IconName::StageMicOff } else { IconName::StageMic },
                if muted { Tone::Danger } else { Tone::Soft },
                false,
                cx,
                |_, _, cx| actions::voice::toggle_mute(cx),
            ))
            .child(dock_button(
                "dock-deafen",
                if deafened { "Undeafen" } else { "Deafen" },
                if deafened { IconName::StageHeadphonesOff } else { IconName::StageHeadphones },
                if deafened { Tone::Danger } else { Tone::Soft },
                false,
                cx,
                |_, _, cx| actions::voice::toggle_deafen(cx),
            ))
            .when(self.can_stream || streaming, |this| {
                this.child(dock_divider(false, cx))
                    .child(dock_button(
                        "dock-share",
                        if streaming { "Stop sharing" } else { "Share your screen" },
                        if streaming { IconName::StageStopShare } else { IconName::StageScreen },
                        if streaming { Tone::On } else { Tone::Soft },
                        false,
                        cx,
                        move |_, _, cx| {
                            if streaming {
                                actions::voice::stop_stream(cx);
                            } else {
                                crate::state::open_go_live(cx);
                            }
                        },
                    ))
                    .children(audio)
            })
            .child(dock_divider(false, cx))
            .child(
                div()
                    .id("dock-leave")
                    .flex()
                    .flex_none()
                    .h_10()
                    .w(px(56.))
                    .items_center()
                    .justify_center()
                    .rounded(theme.radius_md)
                    .bg(theme.color_error)
                    .hover(|s| s.bg(theme.color_error.opacity(0.85)))
                    .cursor_pointer()
                    .tooltip(title_tooltip("Disconnect"))
                    .on_click(|_: &ClickEvent, _, cx| actions::voice::leave_voice_channel(cx))
                    .relative()
                    .children(decibell_shell::probes::probe("dock-leave"))
                    .child(Icon::new(IconName::StageLeave).size(px(18.)).color(theme.color_on_error)),
            )
    }
}

/// The header's right side (`HeaderStats`): `font-meta text-meta tabular-nums text-text-muted`,
/// the participant count, then (once a ping landed) a divider and the latency in its tone.
pub struct HeaderStats {
    latency: Option<u32>,
    count: usize,
    _watch: Watch,
}

impl HeaderStats {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.live.voice.stats, cx, |this: &mut Self, s, _| {
                let changed = std::mem::replace(&mut this.latency, s.latency_ms) != s.latency_ms;
                if changed {
                    crate::trace("HeaderStats latency notify");
                }
                changed
            })
            .store_if(&model.live.voice.participants, cx, |this: &mut Self, p, _| {
                std::mem::replace(&mut this.count, p.list().len()) != p.list().len()
            });
        let latency = model.live.voice.stats.read(cx).latency_ms;
        let count = model.live.voice.participants.read(cx).list().len();
        Self { latency, count, _watch: watch }
    }
}

/// `latencyMs <= 70 ? success : < 175 ? warning : error`.
pub fn latency_tone(ms: u32, cx: &App) -> Hsla {
    let t = cx.theme();
    if ms <= 70 {
        t.color_success
    } else if ms < 175 {
        t.color_warning
    } else {
        t.color_error
    }
}

impl Render for HeaderStats {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        crate::trace("HeaderStats");
        let theme = cx.theme();
        let meta = theme.scaled.text_meta;
        let muted = theme.color_text_muted;
        let count = self.count;
        div()
            .size_full()
            .flex()
            .items_center()
            .justify_end()
            .gap_1()
            .child(
                div()
                    .id("header-count")
                    .flex()
                    .h_7()
                    .items_center()
                    .gap_1p5()
                    .px_2()
                    .tooltip(title_tooltip(SharedString::from(format!("{} in voice", decibell_model::regional::Regional::get().group(count as u64)))))
                    .child(Icon::new(IconName::StageUsers).size(px(14.)).color(muted))
                    .child(Label::new(SharedString::from(decibell_model::regional::Regional::get().group(count as u64))).font(FontRole::Meta).size(meta).color(muted)),
            )
            .when_some(self.latency, |this, ms| {
                let tone = latency_tone(ms, cx);
                this.child(div().h_4().w(px(1.)).bg(theme.color_border_divider)).child(
                    div()
                        .id("header-latency")
                        .flex()
                        .h_7()
                        .items_center()
                        .gap_1p5()
                        .px_2()
                        .tooltip(title_tooltip("Voice latency"))
                        .child(Icon::new(IconName::StageSignal).size(px(13.)).color(tone))
                        .child(Label::new(SharedString::from(format!("{ms} ms"))).font(FontRole::Meta).size(meta).color(tone)),
                )
            })
    }
}
