//! The DM rows the shell draws from `model.dm.list` (the chat half's `DmList`): the conversation
//! list of the DM sidebar (`ConversationSidebar.tsx`' rows) and the server bar's unread-DM tiles
//! (`ServerBar.tsx`' `UnreadDmTiles`). The conversation itself is [`crate::Slot::Dm`]'s.

use std::time::{Duration, SystemTime, UNIX_EPOCH};

use decibell_common::{AvatarDot, UserAvatar, Watch, presence_of};
use decibell_model::dm::Conversation;
use decibell_model::regional::Regional;
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{Model, Uid, actions};
use decibell_ui::{ActiveTheme as _, FontRole, Label, ScrollArea, title_tooltip};
use gpui::{AnyElement, ClickEvent, Context, IntoElement, Render, SharedString, Task, Window, div, prelude::*, px};

use crate::ink::Ink;
use crate::server_bar::TILE_HEIGHT;

fn now_ms() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

/// `formatRelativeTime`: now, 5m, 3h, 2d, 4mo.
pub fn relative_time(then_ms: i64, now_ms: i64) -> String {
    let mins = (now_ms - then_ms).div_euclid(60_000);
    if mins < 1 {
        return "now".into();
    }
    if mins < 60 {
        return format!("{mins}m");
    }
    let hours = mins / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 30 { format!("{days}d") } else { format!("{}mo", days / 30) }
}

/// When `relative_time(then, ·)` next reads differently after `now`, in ms from now.
fn next_label_change(then_ms: i64, now_ms: i64) -> i64 {
    let age = (now_ms - then_ms).max(0);
    let unit = match age / 60_000 {
        m if m < 60 => 60_000,
        m if m < 24 * 60 => 3_600_000,
        m if m < 30 * 24 * 60 => 86_400_000,
        _ => 30 * 86_400_000,
    };
    unit - age % unit
}

/// `9` → "9", `120` → "99+".
fn unread_label(unread: i64) -> SharedString {
    if unread > 99 { "99+".into() } else { Regional::get().group_signed(unread).into() }
}

fn name_of(conversation: &Conversation, cx: &gpui::App) -> SharedString {
    decibell_common::user_name(conversation.peer, &conversation.username, cx)
}

/// The red count on a DM avatar (`-top-1 -right-1 h-[18px] rounded-full border-[2px] bg-error
/// text-[10px] font-semibold text-white`), its border the surface behind it.
fn unread_badge(unread: i64, border: gpui::Hsla, ink: &Ink, min_width: bool) -> impl IntoElement + use<> {
    div()
        .absolute()
        .top(px(-4.))
        .right(px(-4.))
        .flex()
        .h(px(18.))
        .when(min_width, |this| this.min_w(px(18.)).px_1())
        .when(!min_width, |this| this.w(px(18.)))
        .items_center()
        .justify_center()
        .rounded_full()
        .border_2()
        .border_color(border)
        .bg(ink.color_error)
        // `text-white` on the error red: a literal of the renderer.
        .child(Label::new(unread_label(unread)).size(10.).line_height(10.).weight(600.).color(gpui::white()))
}

/// The conversation list (the default of [`crate::Slot::DmList`]).
pub struct DmRows {
    /// Re-renders when the soonest relative time on screen changes; only while one is drawn.
    tick: Option<Task<()>>,
    _watch: Watch,
}

impl DmRows {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store(&model.dm.list, cx)
            .store_if(&model.nav, cx, |_, nav, _| nav.active_view == ActiveView::Dm || nav.active_view == ActiveView::Home)
            .people(cx, |_, _| true);
        Self { tick: None, _watch: watch }
    }
}

impl Render for DmRows {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let ink = crate::ink::chrome(cx);
        let (scale, text_member) = (cx.theme().scaled.list_row_avatar_scale, cx.theme().scaled.text_member);
        let dmbar = cx.theme().chrome.color_bg_dmbar;
        let in_dm = model.nav.read(cx).active_view == ActiveView::Dm;
        let list = model.dm.list.read(cx);
        let active = list.active_peer.filter(|_| in_dm);
        let now = now_ms();
        let mut soonest: Option<i64> = None;
        let mut rows: Vec<AnyElement> = Vec::new();
        for peer in list.sorted().iter() {
            let Some(c) = list.get(*peer) else { continue };
            let name = name_of(c, cx);
            let preview: Option<SharedString> = c.last_message.as_ref().map(|m| {
                let doc = decibell_richtext::plain::preview(&m.content);
                doc.own_text().unwrap_or("").to_string().into()
            });
            let time = (c.last_message_time_ms > 0).then(|| {
                let next = next_label_change(c.last_message_time_ms, now);
                soonest = Some(soonest.map_or(next, |s: i64| s.min(next)));
                SharedString::from(relative_time(c.last_message_time_ms, now))
            });
            let is_active = active == Some(*peer);
            let status = presence_of(*peer, false, cx);
            let (peer, unread) = (*peer, c.unread);
            let open_name = name.clone();
            rows.push(
                div()
                    .id(("dm-row", peer as u64))
                    .flex()
                    .w_full()
                    .items_center()
                    .py(px(8. * scale))
                    .px(px(10. * scale))
                    .gap(px(10. * scale))
                    .rounded(ink.radius_md)
                    .cursor_pointer()
                    .text_color(if is_active { ink.color_text_bright } else { ink.color_text_secondary })
                    .map(|this| {
                        if is_active {
                            this.bg(ink.color_accent_soft)
                        } else {
                            let (bg, fg) = (ink.color_surface_hover, ink.color_text_primary);
                            this.hover(move |s| s.bg(bg).text_color(fg))
                        }
                    })
                    .relative()
                    .children(crate::probes::probe(format!("dm-row:{peer}")))
                    .on_click(move |_: &ClickEvent, _, cx| actions::dm::open_dm(peer, open_name.clone(), cx))
                    .child(
                        div()
                            .relative()
                            .flex_none()
                            .child(
                                // The dot's ring is `--color-bg-dmbar` (the chrome ground), as the renderer's.
                                UserAvatar::new(peer, name.clone(), 34. * scale)
                                    .chrome()
                                    .dot(AvatarDot::new(status, dmbar, 12. * scale).ring_width(2.)),
                            )
                            .when(unread > 0, |this| {
                                this.child(
                                    div()
                                        .id(("dm-row-unread", peer as u64))
                                        .tooltip(title_tooltip(SharedString::from(format!("{unread} unread"))))
                                        .child(unread_badge(unread, ink.color_bg_dark, &ink, false)),
                                )
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .flex_1()
                            .min_w_0()
                            .child(Label::new(name).font(FontRole::Channel).size(text_member).weight(500.).truncate())
                            .when_some(preview, |this, preview| {
                                this.child(Label::new(preview).font(FontRole::Channel).size(11.).color(ink.color_text_muted).truncate())
                            }),
                    )
                    .when_some(time, |this, time| {
                        this.child(div().flex_none().child(Label::new(time).font(FontRole::Channel).size(10.).color(ink.color_text_muted)))
                    })
                    .into_any_element(),
            );
        }
        let empty = rows.is_empty();
        // The relative times are memoised in the renderer with a 60 s clock; here the list
        // re-renders exactly when the soonest label on screen changes, and has no timer otherwise.
        self.tick = soonest.map(|ms| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(Duration::from_millis(ms.max(1000) as u64)).await;
                let _ = this.update(cx, |_, cx| cx.notify());
            })
        });
        ScrollArea::new("dm-list").size_full().child(
            div()
                .px_2()
                .py_2p5()
                .flex()
                .flex_col()
                .children(rows)
                .when(empty, |this| {
                    this.child(
                        div()
                            .flex()
                            .justify_center()
                            .pt_8()
                            .child(Label::new("No conversations yet").size(12.).color(ink.color_text_muted)),
                    )
                }),
        )
    }
}

/// The server bar's unread-DM tiles: one avatar tile per conversation with unread messages, most
/// recent first, then a divider; nothing while everything is read. A click opens the
/// conversation (which marks it read, and the tile goes).
pub struct UnreadDmTiles {
    _watch: Watch,
}

impl UnreadDmTiles {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.dm.list, cx).people(cx, |_, _| true);
        Self { _watch: watch }
    }
}

impl Render for UnreadDmTiles {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let ink = crate::ink::chrome(cx);
        let list = model.dm.list.read(cx);
        let unread: Vec<(Uid, SharedString, i64)> = list
            .sorted()
            .iter()
            .filter_map(|peer| list.get(*peer))
            .filter(|c| c.unread > 0)
            .map(|c| (c.peer, name_of(c, cx), c.unread))
            .collect();
        if unread.is_empty() {
            return div().into_any_element();
        }
        div()
            .flex()
            .items_center()
            .gap_2()
            .children(unread.into_iter().map(|(peer, name, count)| {
                let title: SharedString = format!("{name} — {} unread", unread_label(count)).into();
                let open_name = name.clone();
                div()
                    .id(("unread-dm", peer as u64))
                    .relative()
                    .children(crate::probes::probe(format!("unread-dm:{peer}")))
                    .flex_none()
                    .cursor_pointer()
                    .tooltip(title_tooltip(title))
                    .on_click(move |_: &ClickEvent, _, cx| actions::dm::open_dm(peer, open_name.clone(), cx))
                    .child(UserAvatar::new(peer, name, TILE_HEIGHT).chrome())
                    .child(unread_badge(count, ink.color_bg_darkest, &ink, true))
            }))
            .child(div().mx_1().flex_none().h_6().w(px(1.)).bg(ink.color_border_divider))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn relative_times_read_like_the_renderer_and_change_when_expected() {
        let now = 1_000_000_000_000;
        assert_eq!(relative_time(now - 30_000, now), "now");
        assert_eq!(relative_time(now - 5 * 60_000, now), "5m");
        assert_eq!(relative_time(now - 3 * 3_600_000, now), "3h");
        assert_eq!(relative_time(now - 2 * 86_400_000, now), "2d");
        assert_eq!(relative_time(now - 65 * 86_400_000, now), "2mo");
        // 5 m 20 s old: "5m" turns "6m" in 40 s.
        assert_eq!(next_label_change(now - 320_000, now), 40_000);
        assert_eq!(unread_label(120), "99+");
    }
}
