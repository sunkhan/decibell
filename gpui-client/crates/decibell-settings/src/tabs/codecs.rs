//! Settings → Codecs (`CodecsTab.tsx`): the AV1 / H.265 switches (greyed out without a hardware
//! encoder for them), "Take stream with me", "Refresh codec capabilities", what this machine
//! encodes and decodes.
//!
//! The capabilities are the model's (`live.voice.codecs`: the video crate's decode probe, the
//! core's encode probe), loaded by its one action (`actions::voice::init_codec_caps`): opening
//! the tab asks for them if nothing has yet (the renderer's `load()`), "Refresh" probes both
//! again. More than the renderer's "(HW)" / "(SW)", in its layout: each encoder says which
//! backend encodes it ("NVENC (NVIDIA)", "Software (SVT-AV1)"), each decoder which decoder its
//! streams really get ("Vulkan Video", "NVDEC", "VA-API", "Software (dav1d)"), and under the
//! decoders every decoder that was tried and is not used, with the reason (the owner's rule:
//! probe, fall through in order, report which path is in use).

use std::collections::HashMap;

use decibell_common::Watch;
use decibell_model::Model;
use decibell_model::actions;
use decibell_model::live::voice::codecs::{CodecCap, SkippedDecoder, VideoCodec};
use decibell_ui::{ActiveTheme as _, Label, ScrollHandle};
use gpui::{AnyElement, App, Context, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*};

use super::TabId;
use crate::rows::{Btn, Look, ToggleRow, card};
use crate::window::CloseLink;

pub struct CodecsTab {
    scroll: ScrollHandle,
    close: CloseLink,
    /// A fixture's encoder backends (screens have no probe); the model's win.
    labels: HashMap<VideoCodec, SharedString>,
    take_stream: bool,
    _refresh: Subscription,
    _watch: Watch,
}

impl CodecsTab {
    pub fn new(close: CloseLink, _: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store_if(&model.settings, cx, |this: &mut CodecsTab, s, _| {
            let on = s.get().take_stream_on_channel_switch;
            std::mem::replace(&mut this.take_stream, on) != on
        });
        let refresh = cx.observe(&model.live.voice.codecs, |_, _, cx| cx.notify());
        let (loaded, refreshing) = {
            let codecs = model.live.voice.codecs.read(cx);
            (codecs.loaded, codecs.refreshing)
        };
        if !loaded && !refreshing && decibell_model::core::is_running() {
            actions::voice::init_codec_caps(false, cx);
        }
        let staged = crate::hooks::SettingsHooks::get(cx).staged.encoder_labels.clone();
        Self {
            scroll: ScrollHandle::new(),
            close,
            labels: staged.unwrap_or_default(),
            take_stream: model.settings.read(cx).get().take_stream_on_channel_switch,
            _refresh: refresh,
            _watch: watch,
        }
    }

    /// The fixture's backends (screens have no core to ask).
    pub fn set_labels(&mut self, labels: HashMap<VideoCodec, SharedString>, cx: &mut Context<Self>) {
        self.labels = labels;
        cx.notify();
    }
}

/// `codecLabel`.
fn codec_label(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::Av1 => "AV1",
        VideoCodec::H265 => "H.265 / HEVC",
        VideoCodec::H264Hw => "H.264 (hardware)",
        VideoCodec::H264Sw => "H.264 (software)",
        VideoCodec::Unknown => "Unknown",
    }
}

/// A decoder that was tried and is not used: "H.265 / HEVC: VA-API is not used (no VA driver)".
pub fn format_skipped(s: &SkippedDecoder) -> String {
    if s.reason.is_empty() {
        format!("{}: {} is not used", codec_label(s.codec), s.label)
    } else {
        format!("{}: {} is not used ({})", codec_label(s.codec), s.label, s.reason)
    }
}

/// `formatCap`: "AV1 — up to 3840×2160 @ 120fps (HW)", and after it the backend that encodes
/// it or the decoder that decodes it.
pub fn format_cap(c: &CodecCap, backend: Option<&SharedString>) -> String {
    let base = format!(
        "{} — up to {}×{} @ {}fps{}",
        codec_label(c.codec),
        c.max_width,
        c.max_height,
        c.max_fps,
        if c.hardware { " (HW)" } else { " (SW)" }
    );
    match backend {
        Some(label) if !label.is_empty() => format!("{base} · {label}"),
        _ => base,
    }
}

impl Render for CodecsTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let codecs = model.live.voice.codecs.read(cx);
        let (use_av1, use_h265) = (codecs.use_av1, codecs.use_h265);
        let encode = codecs.encode.clone();
        let decode = codecs.decode.clone();
        let (loaded, refreshing) = (codecs.loaded, codecs.refreshing);
        // Which backend encodes and which decoder decodes each codec, and what was given up.
        let mut encoder_labels = self.labels.clone();
        encoder_labels.extend(codecs.encoders.iter().filter(|e| !e.label.is_empty()).map(|e| (e.codec, e.label.clone())));
        let decoder_labels: HashMap<VideoCodec, SharedString> = codecs.decoders.iter().map(|d| (d.codec, d.label.clone())).collect();
        let skipped = codecs.skipped_decoders.clone();
        let has_av1 = encode.iter().any(|c| c.codec == VideoCodec::Av1);
        let has_h265 = encode.iter().any(|c| c.codec == VideoCodec::H265);
        let take = self.take_stream;
        let theme = cx.theme();

        let refresh = Btn::new(
            "codecs-refresh",
            if refreshing { "Refreshing…" } else { "Refresh codec capabilities" },
            Look {
                bg: theme.color_bg_light,
                bg_hover: theme.color_bg_lighter,
                text: theme.color_text_primary,
                text_hover: theme.color_text_primary,
                border: Some((theme.color_border_divider, theme.color_border_divider)),
            },
            theme.radius_md,
        )
        .disabled(refreshing)
        .on_click(|_, _, cx| actions::voice::init_codec_caps(true, cx));

        let list = |title: &'static str, caps: &[CodecCap], labels: &HashMap<VideoCodec, SharedString>, skipped: &[SkippedDecoder], cx: &App| {
            let theme = cx.theme();
            card(cx)
                .p_4()
                .min_w_0()
                .child(
                    div().mb_2().child(
                        Label::new(SharedString::from(title.to_uppercase()))
                            .size(12.)
                            .weight(600.)
                            .tracking(0.07)
                            .color(theme.color_text_muted),
                    ),
                )
                .map(|this| {
                    if caps.is_empty() {
                        this.child(Label::new("None detected.").size(12.).color(theme.color_text_muted))
                    } else {
                        this.child(div().flex().flex_col().gap_1p5().children(caps.iter().map(|c| {
                            let backend = labels.get(&c.codec);
                            Label::new(SharedString::from(format_cap(c, backend))).size(13.).color(theme.color_text_primary)
                        })))
                    }
                })
                .when(!skipped.is_empty(), |this| {
                    this.child(div().mt_2().flex().flex_col().gap_1().children(skipped.iter().map(|s| {
                        Label::new(SharedString::from(format_skipped(s))).size(12.).color(theme.color_text_muted)
                    })))
                })
                .into_any_element()
        };
        let lists: Option<AnyElement> = loaded.then(|| {
            div()
                .mt_4()
                .grid()
                .grid_cols(2)
                .gap_3()
                .child(list("Detected encoders", &encode, &encoder_labels, &[], cx))
                .child(list("Detected decoders", &decode, &decoder_labels, &skipped, cx))
                .into_any_element()
        });
        let theme = cx.theme();

        let body = div()
            .flex()
            .flex_col()
            .gap_3()
            .child(
                ToggleRow::new(
                    "codecs-av1",
                    "Use AV1 codec when available",
                    if has_av1 {
                        "Auto-pick AV1 for screen sharing when your hardware supports it. Best quality at lower bitrates."
                    } else {
                        "Your hardware does not support AV1 encoding."
                    },
                    use_av1 && has_av1,
                )
                .disabled(!has_av1)
                .on_toggle(move |_, _, cx| actions::voice::set_codec_toggles(!use_av1, use_h265, cx)),
            )
            .child(
                ToggleRow::new(
                    "codecs-h265",
                    "Use H.265 / HEVC codec when available",
                    if has_h265 {
                        "Auto-pick H.265 for screen sharing when your hardware supports it. Better than H.264, broader hardware support than AV1."
                    } else {
                        "Your hardware does not support H.265 encoding."
                    },
                    use_h265 && has_h265,
                )
                .disabled(!has_h265)
                .on_toggle(move |_, _, cx| actions::voice::set_codec_toggles(use_av1, !use_h265, cx)),
            )
            .child(
                ToggleRow::new(
                    "codecs-take-stream",
                    "Take stream with me when switching voice channels",
                    "When you move to another voice channel while screen sharing, carry the stream into the new channel instead of ending it.",
                    take,
                )
                .on_toggle(move |_, _, cx| {
                    let settings = Model::global(cx).settings.clone();
                    settings.update(cx, |s, cx| s.change(|v| v.take_stream_on_channel_switch = !take, cx));
                }),
            )
            .child(
                div()
                    .mt_2()
                    .flex()
                    .items_center()
                    .gap_3()
                    .child(refresh)
                    .child(Label::new("Re-probe after a GPU/driver change.").size(12.).color(theme.color_text_muted)),
            )
            .children(lists)
            .child(
                div().mt_2().child(
                    Label::new(
                        "Disabling a codec removes it from the list advertised to peers, so streams you start will skip it during auto-selection. You can still receive streams that use it (decoding is independent).",
                    )
                    .size(12.)
                    .color(theme.color_text_muted),
                ),
            );

        super::pane(TabId::Codecs, &self.scroll, &self.close).child(body)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn capabilities_read_as_the_renderer_writes_them() {
        let cap = CodecCap { codec: VideoCodec::Av1, max_width: 3840, max_height: 2160, max_fps: 120, hardware: true };
        assert_eq!(format_cap(&cap, None), "AV1 — up to 3840×2160 @ 120fps (HW)");
        let soft = CodecCap { hardware: false, ..cap };
        assert_eq!(
            format_cap(&soft, Some(&"Software (SVT-AV1)".into())),
            "AV1 — up to 3840×2160 @ 120fps (SW) · Software (SVT-AV1)"
        );
        assert_eq!(format_cap(&cap, Some(&"Vulkan Video".into())), "AV1 — up to 3840×2160 @ 120fps (HW) · Vulkan Video");
        let skipped = SkippedDecoder { codec: VideoCodec::H265, label: "VA-API".into(), reason: "no VA driver for this GPU".into() };
        assert_eq!(format_skipped(&skipped), "H.265 / HEVC: VA-API is not used (no VA driver for this GPU)");
    }
}
