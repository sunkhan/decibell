//! Channel settings → Retention (`channelSettings/RetentionTab.tsx`): how long a text channel
//! keeps each kind of thing posted, and wiping its history.

use decibell_model::stores::community::Channel;
use decibell_ui::{ActiveTheme as _, Button, ButtonVariant, IconName, Label, Select, SelectOption, SelectSize};
use gpui::{AnyElement, ClickEvent, Context, IntoElement, ParentElement as _, Styled as _, div, px};

use super::{ChannelSettingsWindow, Confirm};
use crate::ui::{Tone, description, group, notice, row};

pub const PRESETS: [(i32, &str); 6] = [(0, "Forever"), (1, "1 day"), (7, "7 days"), (30, "30 days"), (90, "90 days"), (365, "1 year")];

pub const KINDS: [(IconName, &str, &str); 5] = [
    (IconName::SettingsMessage, "Messages", "The message itself, with any attachments still on it"),
    (IconName::SettingsImage, "Images", "Photos and other still images"),
    (IconName::SettingsFilm, "Videos", "Recorded clips and screen captures"),
    (IconName::SettingsFile, "Files", "PDFs, archives and other documents"),
    (IconName::SettingsMusic, "Audio", "Voice notes and other audio files"),
];

impl ChannelSettingsWindow {
    pub(crate) fn retention_tab(&mut self, channel: &Channel, editable: bool, cx: &mut Context<Self>) -> AnyElement {
        let Some(draft) = self.fields(cx) else { return div().into_any_element() };
        let theme = cx.theme();
        let mut out = div().flex().flex_col().gap_7();
        if !editable {
            out = out.child(notice(IconName::SettingsLock, "You need Manage Channels in this channel to change these.", cx));
        }
        out = out.child(
            div().mb(px(-8.)).child(
                Label::new(format!(
                    "How long #{} keeps what's posted. An expired message goes with its attachments. An expired attachment leaves a placeholder, so readers can still see something was there.",
                    channel.name
                ))
                .size(13.)
                .line_height(13. * 1.6)
                .color(theme.color_text_secondary),
            ),
        );
        let mut rows = Vec::new();
        for (i, (icon, label, hint)) in KINDS.into_iter().enumerate() {
            let value = draft.retention[i];
            let preset = PRESETS.iter().any(|(d, _)| *d == value);
            let mut options: Vec<SelectOption<i32>> = PRESETS.iter().map(|(d, l)| SelectOption::new(*d, *l)).collect();
            // A value set outside the presets (server config) shows as its own entry.
            if !preset {
                options.push(SelectOption::new(value, format!("{value} days")).hint("Custom"));
            }
            let select = Select::options(("retention", i), Some(value), options)
                .size(SelectSize::Sm)
                .disabled(!editable)
                .searchable(false)
                .on_change(cx.listener(move |this, v: &i32, _, cx| {
                    let v = *v;
                    this.change(cx, |d| d.retention[i] = v);
                }));
            rows.push(
                row(Some(icon), Tone::Neutral, label, Some(description(hint, cx)), Some(div().w(px(144.)).flex_none().child(select).into_any_element()), None, cx)
                    .into_any_element(),
            );
        }
        out = out.child(group("Keep for", None, rows, cx));
        if editable {
            let wipe = Button::new("channel-wipe", "Wipe history…").variant(ButtonVariant::DangerOutline).on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                this.confirm = Some(Confirm::Wipe);
                cx.notify();
            }));
            out = out.child(group(
                "Clean up",
                None,
                vec![
                    row(
                        Some(IconName::SettingsEraser),
                        Tone::Danger,
                        "Wipe channel history",
                        Some(description(
                            format!("Deletes every message and attachment in #{} now. The channel, its settings and its permissions stay.", channel.name),
                            cx,
                        )),
                        Some(div().flex_none().child(wipe).into_any_element()),
                        None,
                        cx,
                    )
                    .into_any_element(),
                ],
                cx,
            ));
        }
        out.into_any_element()
    }
}
