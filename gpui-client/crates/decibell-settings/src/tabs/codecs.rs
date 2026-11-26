//! Settings → Codecs (`CodecsTab.tsx`): the AV1 / H.265 switches (greyed out without a hardware
//! encoder for them), "Take stream with me", "Refresh codec capabilities", what this machine
//! encodes and decodes.
//!
//! The capabilities are the model's (`live.voice.codecs`: the video crate's decode probe, the
//! core's encode probe). Opening the tab loads them if nothing has yet (the renderer's `load()`;
//! here the app doesn't probe at boot yet). Each encoder also says which backend encodes it
//! ("NVENC (NVIDIA)", "Software (SVT-AV1)"), from the core's last probe
//! (`cached_native_encoders`): the renderer's list has only "(HW)" / "(SW)".

use std::collections::HashMap;

use decibell_common::Watch;
use decibell_model::Model;
use decibell_model::actions;
use decibell_model::live::voice::codecs::{CodecCap, VideoCodec};
use decibell_ui::{ActiveTheme as _, Label, ScrollHandle};
use gpui::{AnyElement, App, Context, IntoElement, Render, SharedString, Subscription, Task, Window, div, prelude::*};

use super::TabId;
use crate::rows::{Btn, Look, ToggleRow, card};
use crate::window::CloseLink;

pub struct CodecsTab {
    scroll: ScrollHandle,
    close: CloseLink,
    /// The backend that encodes each codec, as the core last probed it.
    labels: HashMap<VideoCodec, SharedString>,
    /// A refresh was running (the backends are asked for again when it ends).
    was_refreshing: bool,
    take_stream: bool,
    _labels: Option<Task<()>>,
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
        // Every change redraws; a refresh that just ended means the core probed again: ask
        // for the backends again.
        let refresh = cx.observe(&model.live.voice.codecs, |this, codecs, cx| {
            let refreshing = codecs.read(cx).refreshing;
            if this.was_refreshing && !refreshing {
                this.fetch_labels(cx);
            }
            this.was_refreshing = refreshing;
            cx.notify();
        });
        let loaded = model.live.voice.codecs.read(cx).loaded;
        if !loaded && decibell_model::core::is_running() {
            actions::voice::init_codec_caps(false, cx);
        }
        let staged = crate::hooks::SettingsHooks::get(cx).staged.encoder_labels.clone();
        let mut tab = Self {
            scroll: ScrollHandle::new(),
            close,
            labels: staged.unwrap_or_default(),
            was_refreshing: model.live.voice.codecs.read(cx).refreshing,
            take_stream: model.settings.read(cx).get().take_stream_on_channel_switch,
            _labels: None,
            _refresh: refresh,
            _watch: watch,
        };
        tab.fetch_labels(cx);
        tab
    }

    /// The fixture's backends (screens have no core to ask).
    pub fn set_labels(&mut self, labels: HashMap<VideoCodec, SharedString>, cx: &mut Context<Self>) {
        self.labels = labels;
        cx.notify();
    }

    fn fetch_labels(&mut self, cx: &mut Context<Self>) {
        if !decibell_model::core::is_running() {
            return;
        }
        #[cfg(any(target_os = "windows", target_os = "linux"))]
        {
            self._labels = Some(cx.spawn(async move |this, cx| {
                let Ok(caps) = decibell_core::api::streaming::cached_native_encoders().await else { return };
                let labels: HashMap<VideoCodec, SharedString> =
                    caps.into_iter().map(|c| (VideoCodec::from_wire(c.codec), SharedString::from(c.label))).collect();
                let _ = this.update(cx, |this, cx| {
                    this._labels = None;
                    this.set_labels(labels, cx);
                });
            }));
        }
        #[cfg(not(any(target_os = "windows", target_os = "linux")))]
        let _ = cx;
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

/// `formatCap`: "AV1 — up to 3840×2160 @ 120fps (HW)", and for an encoder the backend after it.
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

        let list = |title: &'static str, caps: &[CodecCap], labels: Option<&HashMap<VideoCodec, SharedString>>, cx: &App| {
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
                            let backend = labels.and_then(|l| l.get(&c.codec));
                            Label::new(SharedString::from(format_cap(c, backend))).size(13.).color(theme.color_text_primary)
                        })))
                    }
                })
                .into_any_element()
        };
        let lists: Option<AnyElement> = loaded.then(|| {
            div()
                .mt_4()
                .grid()
                .grid_cols(2)
                .gap_3()
                .child(list("Detected encoders", &encode, Some(&self.labels), cx))
                .child(list("Detected decoders", &decode, None, cx))
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
    }
}
