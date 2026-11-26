//! Channel settings → Overview (`channelSettings/OverviewTab.tsx`): the name, a text channel's
//! topic, slowmode and encryption (chosen at creation, fixed), a voice channel's bitrate.

use decibell_model::stores::community::{Channel, ChannelKind};
use decibell_ui::{ActiveTheme as _, Field, FontRole, Icon, IconName, Label, Select, SelectOption, SelectSize, Slider, TextField};
use gpui::{AnyElement, ClickEvent, Context, IntoElement, ParentElement as _, Styled as _, div, prelude::*, px};

use super::{ChannelSettingsWindow, TOPIC_MAX_BYTES, channel_icon};
use crate::ui::{InputBox, Tone, description, group, notice, pill, row};

pub const SLOWMODE: [(i32, &str); 9] = [
    (0, "Off"),
    (5, "5 seconds"),
    (10, "10 seconds"),
    (30, "30 seconds"),
    (60, "1 minute"),
    (300, "5 minutes"),
    (900, "15 minutes"),
    (3600, "1 hour"),
    (21600, "6 hours"),
];

/// Reference ticks on the bitrate slider (kbps). Purely visual: the slider is continuous.
pub const BITRATE_MARKS: [i32; 5] = [32, 64, 128, 192, 256];
pub const BITRATE_MIN: i32 = 16;
pub const BITRATE_MAX: i32 = 320;
pub const BITRATE_DEFAULT: i32 = 64;

fn frac(k: i32) -> f32 {
    (k - BITRATE_MIN) as f32 / (BITRATE_MAX - BITRATE_MIN) as f32
}

impl ChannelSettingsWindow {
    pub(crate) fn overview_tab(&mut self, channel: &Channel, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.fields(cx) else { return div().into_any_element() };
        let theme = cx.theme();
        let is_text = channel.kind == ChannelKind::Text;
        let is_voice = channel.kind == ChannelKind::Voice;
        let left = TOPIC_MAX_BYTES as i64 - draft.topic.len() as i64;
        let mut out = div().flex().flex_col().gap_7();
        if !editable {
            out = out.child(notice(IconName::SettingsLock, "You need Manage Channels in this channel to change these.", cx));
        }
        out = out.child(
            Field::new(if channel.kind == ChannelKind::Category { "Category name" } else { "Channel name" })
                .child(TextField::new("channel-name").editor(&self.name).leading_icon(channel_icon(channel.kind)).disabled(!editable)),
        );
        if is_text {
            let theme = cx.theme();
            let hint: AnyElement = if channel.encrypted {
                div()
                    .flex()
                    .items_center()
                    .gap_1p5()
                    .child(Icon::new(IconName::SettingsLockOpen).size(px(13.)).color(theme.color_text_muted))
                    .child(Label::new("Topics aren't encrypted. The server host can read them.").size(12.).line_height(12. * 1.55).color(theme.color_text_muted))
                    .into_any_element()
            } else {
                Label::new("Shown next to the channel name at the top of the chat.").size(12.).line_height(12. * 1.55).color(theme.color_text_muted).into_any_element()
            };
            let mut field = Field::new("Topic");
            if left < 100 {
                field = field.aside(
                    Label::new(decibell_model::regional::Regional::get().group_signed(left))
                        .font(FontRole::Meta)
                        .size(theme.scaled.text_meta)
                        .color(if left < 20 { theme.color_warning } else { theme.color_text_muted }),
                );
            }
            out = out.child(
                field
                    .child(InputBox::new("channel-topic", &self.topic).text(13., 400).leading(20. / 13.).pad(12., 10.).rows(3).hover())
                    .child(div().mt_2().child(hint)),
            );
            let slowmode = Select::options(
                "channel-slowmode",
                Some(draft.slowmode),
                SLOWMODE.iter().map(|(v, l)| SelectOption::new(*v, *l)).collect(),
            )
            .size(SelectSize::Sm)
            .disabled(!editable)
            .searchable(false)
            .on_change(cx.listener(|this, v: &i32, _, cx| {
                let v = *v;
                this.change(cx, |d| d.slowmode = v);
            }));
            let enc_text = if channel.encrypted {
                "Messages and attachments are sealed on members' devices with keys the server never holds. Set when the channel was created; it can't be changed."
            } else {
                "Messages are stored on the server as plain text. Set when the channel was created; it can't be changed."
            };
            out = out.child(group(
                "Messages",
                None,
                vec![
                    row(
                        Some(IconName::SettingsHourglass),
                        Tone::Neutral,
                        "Slowmode",
                        Some(description("Members wait this long between messages. Anyone who can Manage Messages here is exempt.", cx)),
                        Some(div().w(px(160.)).flex_none().child(slowmode).into_any_element()),
                        None,
                        cx,
                    )
                    .into_any_element(),
                    row(
                        Some(if channel.encrypted { IconName::SettingsLock } else { IconName::SettingsLockOpen }),
                        if channel.encrypted { Tone::Good } else { Tone::Neutral },
                        "End-to-end encryption",
                        Some(description(enc_text, cx)),
                        Some(
                            if channel.encrypted { pill(true, Some(IconName::SettingsLock), "On", cx) } else { pill(false, None, "Off", cx) }.into_any_element(),
                        ),
                        None,
                        cx,
                    )
                    .into_any_element(),
                ],
                cx,
            ));
        }
        if is_voice {
            let shown = if draft.bitrate == 0 { BITRATE_DEFAULT } else { draft.bitrate };
            let theme = cx.theme();
            let control = div()
                .flex()
                .flex_none()
                .items_center()
                .gap_2()
                .when(draft.bitrate != 0 && editable, |this| {
                    let (muted, primary, hover) = (theme.color_text_muted, theme.color_text_primary, theme.color_surface_hover);
                    this.child(
                        div()
                            .id("bitrate-reset")
                            .rounded(theme.radius_sm)
                            .px_2()
                            .py_1()
                            .text_color(muted)
                            .cursor_pointer()
                            .hover(move |s| s.bg(hover).text_color(primary))
                            .child(Label::new("Reset").size(12.).weight(600.))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.change(cx, |d| d.bitrate = 0))),
                    )
                })
                .child(
                    div()
                        .flex()
                        .h_8()
                        .min_w(px(96.))
                        .items_baseline()
                        .justify_center()
                        .gap_1()
                        .rounded(theme.radius_md)
                        .border_1()
                        .border_color(theme.color_border_divider)
                        .bg(theme.color_bg_lighter)
                        .px_2p5()
                        .pt_1p5()
                        .child(Label::new(shown.to_string()).font(FontRole::Meta).size(14.).weight(600.).color(theme.color_text_primary))
                        .child(Label::new("kbps").font(FontRole::Meta).size(12.).color(theme.color_text_muted)),
                );
            // The labels under the slider, each centred on its tick (the thumb's centre travels
            // 7 px inside each end).
            let label_at = |k: i32, f: f32, bold: bool| {
                div().absolute().top_0().left(gpui::relative(f)).w(px(0.)).flex().justify_center().child(
                    Label::new(k.to_string())
                        .font(FontRole::Meta)
                        .size(11.)
                        .line_height(11.)
                        .weight(if bold { 600. } else { 400. })
                        .color(if bold { theme.color_text_secondary } else { theme.color_text_muted })
                        .nowrap(),
                )
            };
            let mut labels = div().absolute().top_0().bottom_0().left(px(7.)).right(px(7.));
            for k in BITRATE_MARKS {
                labels = labels.child(label_at(k, frac(k), k == BITRATE_DEFAULT));
            }
            let below = div()
                .mt_4()
                .pl(px(50.))
                .when(!editable, |this| this.opacity(0.6))
                .child(
                    Slider::new("bitrate", shown as f32, BITRATE_MIN as f32, BITRATE_MAX as f32)
                        .step(1.)
                        .fill()
                        .ticks(BITRATE_MARKS.map(|k| k as f32))
                        .disabled(!editable)
                        .on_change(cx.listener(|this, v: &f32, _, cx| {
                            let v = v.round() as i32;
                            this.change(cx, |d| d.bitrate = v);
                        })),
                )
                .child(
                    div()
                        .relative()
                        .mt_2()
                        .h(px(14.))
                        .child(div().absolute().left_0().child(Label::new(BITRATE_MIN.to_string()).font(FontRole::Meta).size(11.).line_height(11.).color(theme.color_text_muted)))
                        .child(labels)
                        .child(div().absolute().right_0().child(Label::new(BITRATE_MAX.to_string()).font(FontRole::Meta).size(11.).line_height(11.).color(theme.color_text_muted))),
                )
                .when(draft.bitrate == 0, |this| {
                    this.child(
                        div().mt_3().child(
                            Label::new(format!("Using the default, {BITRATE_DEFAULT} kbps. Drag to set this channel's own.")).size(12.).color(theme.color_text_muted),
                        ),
                    )
                });
            out = out.child(group(
                "Voice",
                None,
                vec![
                    row(
                        Some(IconName::SettingsWaves),
                        Tone::Neutral,
                        "Bitrate",
                        Some(description("Higher sounds better and uses more bandwidth for everyone listening. Connected members retune right away.", cx)),
                        Some(control.into_any_element()),
                        Some(below.into_any_element()),
                        cx,
                    )
                    .into_any_element(),
                    row(
                        Some(IconName::SettingsLock),
                        Tone::Good,
                        "End-to-end encryption",
                        Some(description("Voice and screen shares are always sealed on members' devices. Stream preview thumbnails aren't.", cx)),
                        Some(pill(true, Some(IconName::SettingsLock), "Always on", cx).into_any_element()),
                        None,
                        cx,
                    )
                    .into_any_element(),
                ],
                cx,
            ));
        }
        let _ = theme;
        out.into_any_element()
    }
}
