//! What is drawn over video (`stage/StageTiles.tsx`' scrims, `CodecBadge.tsx`' colours): a fixed
//! dark scrim with white type in every theme, because the picture under it is not themed. These
//! colours are the renderer's own literals (`bg-black/70`, `border-white/10`, `text-white`, the
//! codec accents), the one place they are named; everything else in this crate is a token.

use decibell_model::live::voice::{VideoCodec, streams::StreamInfo};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, Spinner};
use gpui::{App, Div, Hsla, IntoElement, SharedString, div, hsla, prelude::*, px, rgb};

/// `#000` at `alpha` (`bg-black/70` …): the renderer's scrim literal.
pub fn black(alpha: f32) -> Hsla {
    hsla(0., 0., 0., alpha)
}

/// `#fff` at `alpha` (`text-white/70`, `border-white/10` …).
pub fn white(alpha: f32) -> Hsla {
    hsla(0., 0., 1., alpha)
}

/// `CODEC_COLOR` (`CodecBadge.tsx`): theme-independent, the badge floats over video.
pub fn codec_color(codec: VideoCodec) -> Hsla {
    match codec {
        VideoCodec::Av1 => rgb(0x4ade80).into(),
        VideoCodec::H265 => rgb(0x93c5fd).into(),
        VideoCodec::H264Hw => rgb(0x22d3ee).into(),
        _ => white(0.7),
    }
}

/// The lock of an enforced codec (`#e0b050`).
pub fn lock_color() -> Hsla {
    rgb(0xe0b050).into()
}

/// `ON_VIDEO_GREEN` (`CallStage.tsx`): the theme's green can be too dark on black.
pub fn on_video_green() -> Hsla {
    rgb(0x65d097).into()
}

/// The mini-player's LIVE tag (`text-red-400`, `bg-red-500`: Tailwind's palette, as written).
pub fn red_400() -> Hsla {
    rgb(0xf87171).into()
}

pub fn red_500() -> Hsla {
    rgb(0xef4444).into()
}

/// The stall indicator over a stream whose pictures stopped (`StreamVideoPlayer.tsx`: `absolute
/// inset-0 bg-black/40`, a pill `rounded-md border border-white/10 bg-black/70 px-3 py-2
/// text-[12px] font-medium text-white/90` with a 16 px warning spinner and "Reconnecting…").
/// Takes no pointer events, as the renderer's (`pointer-events-none`): it has no handlers.
pub fn reconnecting(cx: &App) -> impl IntoElement {
    let theme = cx.theme();
    div()
        .absolute()
        .inset_0()
        .flex()
        .items_center()
        .justify_center()
        .bg(black(0.4))
        .children(decibell_shell::probes::probe("stream-reconnecting"))
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .rounded(theme.radius_md)
                .border_1()
                .border_color(white(0.1))
                .bg(black(0.7))
                .px_3()
                .py_2()
                .child(Spinner::new().size(px(16.)).color(theme.color_warning))
                .child(Label::new("Reconnecting…").size(12.).weight(500.).color(white(0.9)).nowrap()),
        )
}

/// `shortCodec`: the codec as the quality pill names it.
pub fn short_codec(codec: VideoCodec) -> Option<&'static str> {
    match codec {
        VideoCodec::Av1 => Some("AV1"),
        VideoCodec::H265 => Some("HEVC"),
        VideoCodec::H264Hw | VideoCodec::H264Sw => Some("H.264"),
        VideoCodec::Unknown => None,
    }
}

/// `formatResolution`.
pub fn format_resolution(w: u32, h: u32) -> String {
    match (w, h) {
        (3840, 2160) => "4K".into(),
        (2560, 1440) => "1440p".into(),
        (1920, 1080) => "1080p".into(),
        (1280, 720) => "720p".into(),
        _ => format!("{w}×{h}"),
    }
}

/// `SCRIM_BASE`: `flex items-center whitespace-nowrap rounded-sm border border-white/10
/// bg-black/70 font-meta text-meta font-medium text-white`, without a size.
pub fn scrim_base(cx: &App) -> Div {
    let theme = cx.theme();
    div()
        .flex()
        .flex_none()
        .items_center()
        .rounded(theme.radius_sm)
        .border_1()
        .border_color(white(0.1))
        .bg(black(0.7))
}

/// `SCRIM`: tile chips, `h-6 gap-1.5 px-2`.
pub fn scrim(cx: &App) -> Div {
    scrim_base(cx).h_6().gap_1p5().px_2()
}

/// `SCRIM_LG`: the focused view, `h-8 gap-2 px-2.5`.
pub fn scrim_lg(cx: &App) -> Div {
    scrim_base(cx).h_8().gap_2().px_2p5()
}

/// Text on a scrim: `font-meta text-meta font-medium`, white unless told otherwise.
pub fn scrim_text(text: impl Into<SharedString>, color: Hsla, cx: &App) -> Label {
    Label::new(text).font(FontRole::Meta).size(cx.theme().scaled.text_meta).weight(500.).color(color).nowrap()
}

/// `LivePill` sizes: `sm` h-4 px-1, `md` h-5 px-1.5, `lg` h-8 px-2.5.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum LiveSize {
    Sm,
    Md,
    Lg,
}

/// `LivePill`: `inline-flex items-center gap-1.5 rounded-sm bg-live font-meta text-micro font-bold
/// uppercase leading-none tracking-wider text-white` with a 6 px white dot.
pub fn live_pill(size: LiveSize, cx: &App) -> impl IntoElement + use<> {
    let theme = cx.theme();
    let micro = theme.scaled.text_micro;
    div()
        .flex()
        .flex_none()
        .items_center()
        .gap_1p5()
        .rounded(theme.radius_sm)
        .bg(theme.color_live)
        .map(|this| match size {
            LiveSize::Sm => this.h_4().px_1(),
            LiveSize::Md => this.h_5().px_1p5(),
            LiveSize::Lg => this.h_8().px_2p5(),
        })
        .child(div().size_1p5().flex_none().rounded_full().bg(white(1.)))
        .child(
            Label::new("LIVE")
                .font(FontRole::Meta)
                .size(micro)
                .weight(700.)
                .line_height(micro)
                .tracking(0.05)
                .color(white(1.)),
        )
}

/// `QualityPill`: "1080p60 · AV1" (+ a lock when the streamer pinned the codec), on a scrim; none
/// when there is nothing to say.
pub fn quality_pill(stream: &StreamInfo, large: bool, cx: &App) -> Option<impl IntoElement + use<>> {
    let codec = short_codec(stream.current_codec);
    let has_res = stream.width > 0 && stream.height > 0 && stream.fps > 0;
    if codec.is_none() && !has_res {
        return None;
    }
    let base = if large { scrim_lg(cx) } else { scrim(cx) };
    Some(
        base.when(has_res, |this| {
            this.child(scrim_text(format!("{}{}", format_resolution(stream.width, stream.height), stream.fps), white(0.7), cx))
        })
        .when_some(codec, |this, codec| this.child(scrim_text(codec, codec_color(stream.current_codec), cx)))
        .when(stream.enforced_codec != VideoCodec::Unknown, |this| {
            this.child(Icon::new(IconName::StageLock).size(px(11.)).stroke_width(2.5).color(lock_color()))
        }),
    )
}
