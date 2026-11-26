//! Storage (`tabs/StorageTab.tsx`, MANAGE_SERVER): the host disk of the machine running this
//! community, this community's own footprint, and the editable upload headroom. The numbers are
//! the server's `storage_info_received`, which is also the answer to a headroom save (it carries
//! the stored value after the server's clamp), so the save settles on it.

use decibell_common::Watch;
use decibell_core::events::{CoreEvent, StorageInfoReceivedPayload};
use decibell_model::live::transfers::format_size;
use decibell_model::regional::Regional;
use decibell_model::save_echo::EchoSave;
use decibell_model::{Model, ServerId};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, Label, SaveBar};
use gpui::{AnyElement, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Subscription, Window, div, prelude::*, px};

use super::overview::toast_error;
use super::{SettingsTab, TabContext};
use crate::requests::{self, Request};
use crate::ui::InputBox;

const GIB: f64 = 1024. * 1024. * 1024.;

fn kind_label(kind: i32) -> String {
    match kind {
        0 => "Images".into(),
        1 => "Videos".into(),
        2 => "Documents".into(),
        3 => "Audio".into(),
        k => format!("Kind {k}"),
    }
}

/// JavaScript's `parseFloat`: the longest leading number, `None` for none (NaN).
pub fn parse_float(text: &str) -> Option<f64> {
    let t = text.trim_start();
    if let Some(rest) = t.strip_prefix("Infinity").or_else(|| t.strip_prefix("+Infinity")) {
        let _ = rest;
        return Some(f64::INFINITY);
    }
    let bytes = t.as_bytes();
    let mut end = 0;
    let mut best = None;
    while end < bytes.len() {
        end += 1;
        let prefix = &t[..end];
        if prefix.ends_with(['e', 'E', '+', '-', '.']) {
            continue;
        }
        if let Ok(v) = prefix.parse::<f64>() {
            best = Some(v);
        } else if !prefix.chars().all(|c| c.is_ascii_digit() || matches!(c, '.' | 'e' | 'E' | '+' | '-')) {
            break;
        }
    }
    best
}

/// `toFixed(1)` of a byte count in GB.
fn gb_text(bytes: i64) -> String {
    format!("{:.1}", bytes as f64 / GIB)
}

pub struct StorageTab {
    ctx: TabContext,
    info: Option<StorageInfoReceivedPayload>,
    loading: bool,
    error: Option<SharedString>,
    min_free: Entity<InputState>,
    save: EchoSave<()>,
    syncing: bool,
    _watch: Watch,
    _subs: Vec<Subscription>,
}

impl StorageTab {
    #[cfg(feature = "fixtures")]
    /// The server's storage answer, once it came (the self-tests).
    pub(crate) fn info(&self) -> Option<&StorageInfoReceivedPayload> {
        self.info.as_ref()
    }
}

impl SettingsTab for StorageTab {
    fn dirty(&self, cx: &App) -> bool {
        self.dirty_min_free(cx)
    }
}

impl StorageTab {
    pub fn new(ctx: TabContext, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let server = ctx.server;
        let mut watch = Watch::new();
        if let Some(community) = model.community(server, cx) {
            watch.store(&community.channels, cx);
        }
        let min_free = cx.new(|cx| InputState::single_line(window, cx).accept(decibell_text::input::numeric));
        let mut subs = vec![cx.subscribe(&min_free, |this: &mut Self, _, e: &InputEvent, cx| {
            if *e == InputEvent::Change && !this.syncing {
                cx.notify();
            }
        })];
        subs.push(cx.subscribe(&model.answers, move |this: &mut Self, _, event: &CoreEvent, cx| {
            if let CoreEvent::StorageInfoReceived(p) = event
                && ServerId::from_wire(&p.server_id) == server
            {
                this.apply(p.clone(), cx);
            }
        }));
        let weak = cx.weak_entity();
        requests::send(Request::StorageInfo { server }, cx, move |result, cx| {
            if let Err(e) = result {
                let _ = weak.update(cx, |this, cx| {
                    this.loading = false;
                    this.error = Some(e.js_string());
                    cx.notify();
                });
            }
        });
        StorageTab { ctx, info: None, loading: true, error: None, min_free, save: EchoSave::default(), syncing: false, _watch: watch, _subs: subs }
    }

    /// `storage_info_received` only ever arrives as a reply to this tab's own fetch or save
    /// (never a broadcast), so syncing the field to it can't clobber typing, and it shows a
    /// server-side clamp after a save.
    pub fn apply(&mut self, p: StorageInfoReceivedPayload, cx: &mut Context<Self>) {
        self.loading = false;
        self.save.stop();
        if !p.success {
            self.error = Some(if p.message.is_empty() { "Couldn't load storage info.".into() } else { p.message.into() });
            cx.notify();
            return;
        }
        self.error = None;
        let text = gb_text(p.min_free_bytes);
        self.info = Some(p);
        self.set_field(&text, cx);
        cx.notify();
    }

    fn set_field(&mut self, text: &str, cx: &mut Context<Self>) {
        self.syncing = true;
        self.min_free.update(cx, |s, cx| s.set_value(text, cx));
        self.syncing = false;
    }

    /// Anything but the stored value is a change, a half-typed number too: Save says what's wrong
    /// with it rather than the edit vanishing.
    fn dirty_min_free(&self, cx: &App) -> bool {
        let Some(info) = &self.info else { return false };
        match parse_float(self.min_free.read(cx).value()) {
            Some(gb) if gb.is_finite() && gb >= 0. => (gb * GIB).round() as i64 != info.min_free_bytes,
            _ => true,
        }
    }

    fn save_min_free(&mut self, cx: &mut Context<Self>) {
        let gb = parse_float(self.min_free.read(cx).value());
        let Some(gb) = gb.filter(|g| g.is_finite() && *g >= 0.) else {
            toast_error("Invalid value", "Enter a number of gigabytes (0 or more).", cx);
            return;
        };
        self.save.start((), cx, |this| &mut this.save);
        let weak = cx.weak_entity();
        requests::send(
            Request::SetStorageMinFree { server: self.ctx.server, min_free_bytes: (gb * GIB).round() as i64 },
            cx,
            move |result, cx| {
                if let Err(e) = result {
                    let _ = weak.update(cx, |this, cx| {
                        this.save.stop();
                        cx.notify();
                    });
                    toast_error("Couldn't update headroom", e.js_string(), cx);
                }
            },
        );
    }
}

fn section(title: impl Into<SharedString>, body: impl IntoElement, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div().child(div().mb_2().child(Label::new(title.into()).size(13.).weight(600.).color(theme.color_text_primary))).child(body)
}

fn list(rows: Vec<AnyElement>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    let mut out = div().flex().flex_col().rounded(theme.radius_md).border_1().border_color(theme.color_border);
    let n = rows.len();
    for (i, row) in rows.into_iter().enumerate() {
        out = out.child(row);
        if i + 1 < n {
            out = out.child(div().h(px(1.)).bg(theme.color_border_divider));
        }
    }
    out
}

fn row(left: String, right: String, sub: Option<String>, truncate_left: bool, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let left_label = Label::new(left).size(13.).color(theme.color_text_primary);
    div()
        .flex()
        .items_center()
        .justify_between()
        .gap_3()
        .px_3()
        .py_2()
        .child(
            div()
                .min_w_0()
                .when(truncate_left, |this| this.flex_1())
                .child(if truncate_left { left_label.truncate() } else { left_label })
                .when_some(sub, |this, sub| this.child(Label::new(sub).size(11.).color(theme.color_text_muted))),
        )
        .child(div().flex_none().child(Label::new(right).size(13.).color(theme.color_text_secondary)))
        .into_any_element()
}

fn stat(label: &'static str, value: String, sub: Option<String>, cx: &App) -> gpui::Div {
    let theme = cx.theme();
    div()
        .flex_1()
        .min_w_0()
        .rounded(theme.radius_md)
        .border_1()
        .border_color(theme.color_border)
        .p_2p5()
        .child(Label::new(label).size(11.).color(theme.color_text_muted))
        .child(Label::new(value).size(14.).weight(600.).color(theme.color_text_primary))
        .when_some(sub, |this, sub| this.child(Label::new(sub).size(11.).color(theme.color_text_muted)))
}

impl Render for StorageTab {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let Some(info) = self.info.clone() else {
            let (text, color) = match (&self.error, self.loading) {
                (Some(e), _) => (e.clone(), theme.color_error),
                (None, true) => ("Loading storage…".into(), theme.color_text_muted),
                (None, false) => return div(),
            };
            return div().py_8().flex().justify_center().child(Label::new(text).size(13.).color(color));
        };
        let model = Model::global(cx).clone();
        let channels = model.community(self.ctx.server, cx).map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default();
        let channel_name = |id: &str| channels.iter().find(|c| &*c.id == id).map_or_else(|| id.to_string(), |c| c.name.to_string());
        let regional = Regional::get();
        let size = |b: i64| format_size(b.max(0) as u64);
        let used = (info.volume_total_bytes - info.volume_available_bytes).max(0);
        let used_pct = if info.volume_total_bytes > 0 { (used as f64 / info.volume_total_bytes as f64 * 100.).min(100.) } else { 0. };
        let low = info.volume_available_bytes <= info.min_free_bytes;
        let footprint = info.attachments_bytes + info.thumbnails_bytes + info.database_bytes;
        let dirty = self.dirty_min_free(cx);
        let theme = cx.theme();

        let disk = div()
            .rounded(theme.radius_md)
            .border_1()
            .border_color(theme.color_border)
            .p_3()
            .child(
                div()
                    .mb_2()
                    .flex()
                    .items_baseline()
                    .justify_between()
                    .child(Label::new(format!("{} / {} used", size(used), size(info.volume_total_bytes))).size(12.).color(theme.color_text_secondary))
                    .child(
                        Label::new(format!("{} available", size(info.volume_available_bytes)))
                            .size(12.)
                            .color(if low { theme.color_error } else { theme.color_text_muted }),
                    ),
            )
            .child(
                div().h_2p5().w_full().overflow_hidden().rounded_full().bg(theme.color_bg_light).child(
                    div()
                        .h_full()
                        .rounded_full()
                        .bg(if low { theme.color_error } else { theme.color_accent })
                        .w(gpui::relative((used_pct / 100.) as f32)),
                ),
            )
            .when(low, |this| {
                this.child(
                    div().mt_2().child(
                        Label::new(
                            "Available space is at or below the upload headroom — new uploads are being refused until space is freed or the headroom is lowered.",
                        )
                        .size(11.)
                        .color(theme.color_error),
                    ),
                )
            });

        let headroom = div()
            .child(div().mb_2().child(Label::new("Refuse uploads that would leave the disk with less than this much free space.").size(12.).color(theme.color_text_muted)))
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap_2()
                    .child(div().w(px(112.)).child(InputBox::new("storage-min-free", &self.min_free).text(13., 400).pad(10., 6.).bg(theme.color_bg_light).no_ring()))
                    .child(Label::new("GB").size(13.).color(theme.color_text_secondary)),
            );

        let stats = div()
            .flex()
            .gap_2()
            .child(stat("Attachments", size(info.attachments_bytes), Some(format!("{} files", regional.group_signed(info.attachment_count))), cx))
            .child(stat("Thumbnails", size(info.thumbnails_bytes), None, cx))
            .child(stat("Database", size(info.database_bytes), None, cx));

        let mut by_kind = info.by_kind.clone();
        by_kind.sort_by_key(|k| std::cmp::Reverse(k.bytes));
        let kinds: Vec<AnyElement> =
            by_kind.iter().map(|k| row(kind_label(k.kind), size(k.bytes), Some(format!("{} files", regional.group_signed(k.count))), false, cx)).collect();
        let by_channel: Vec<AnyElement> = info
            .by_channel
            .iter()
            .map(|c| row(format!("#{}", channel_name(&c.channel_id)), size(c.bytes), Some(format!("{} files", regional.group_signed(c.count))), false, cx))
            .collect();
        let largest: Vec<AnyElement> = info
            .largest
            .iter()
            .map(|l| {
                let name = if l.filename.is_empty() { format!("#{}", l.attachment_id) } else { l.filename.clone() };
                row(name, size(l.size_bytes), Some(format!("#{}", channel_name(&l.channel_id))), true, cx)
            })
            .collect();

        let mut body = div().flex().flex_col().gap_5();
        if let Some(error) = &self.error {
            body = body.child(Label::new(error.clone()).size(12.).color(theme.color_error));
        }
        body = body
            .child(section("Host disk", disk, cx))
            .child(section("Upload headroom", headroom, cx))
            .child(section(format!("This server is using {}", size(footprint)), stats, cx));
        if !kinds.is_empty() {
            body = body.child(section("By type", list(kinds, cx), cx));
        }
        if !by_channel.is_empty() {
            body = body.child(section("By channel", list(by_channel, cx), cx));
        }
        if !largest.is_empty() {
            body = body.child(section("Largest files", list(largest, cx), cx));
        }
        div().child(body).child(
            SaveBar::new("storage-save", &self.ctx.scroll, dirty, self.ctx.nudge.get())
                .saving(self.save.saving())
                .on_reset(cx.listener(|this, _: &ClickEvent, _, cx| {
                    if let Some(info) = &this.info {
                        let text = gb_text(info.min_free_bytes);
                        this.set_field(&text, cx);
                        cx.notify();
                    }
                }))
                .on_save(cx.listener(|this, _: &ClickEvent, _, cx| this.save_min_free(cx))),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_float_is_javascripts() {
        assert_eq!(parse_float("1.5"), Some(1.5));
        assert_eq!(parse_float(" 2"), Some(2.));
        assert_eq!(parse_float("1.5GB"), Some(1.5));
        assert_eq!(parse_float("1e3"), Some(1000.));
        assert_eq!(parse_float(".5"), Some(0.5));
        assert_eq!(parse_float("-1"), Some(-1.));
        assert_eq!(parse_float(""), None);
        assert_eq!(parse_float("abc"), None);
        assert_eq!(parse_float("1."), Some(1.));
    }

    #[test]
    fn gigabytes_show_one_decimal() {
        assert_eq!(gb_text(5 * 1024 * 1024 * 1024), "5.0");
        assert_eq!(gb_text(1610612736), "1.5");
    }
}
