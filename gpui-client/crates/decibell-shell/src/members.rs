//! The member list beside a text channel (`features/friends/MembersList.tsx`,
//! `features/members/`): Online / Offline by the community's own view (connected and not
//! invisible, which is how it pages and counts the roster), central's status refining the dot
//! inside Online; windowed rows of one whole-pixel height; the next page asked for as the list
//! nears the end of what is loaded (`Roster::load_more`), with the renderer's "Load more" row.

use std::ops::Range;
use std::rc::Rc;

use decibell_common::{AvatarDot, BoundsCell, StatusMessageText, UserAvatar, Watch, open_profile_beside, open_user_menu, presence_of};
use decibell_model::live::presence::{DisplayStatus, OwnStatus};
use decibell_model::regional::Regional;
use decibell_model::stores::community::{Community, Member, RosterMeta};
use decibell_model::{Model, ServerId, Uid};
use decibell_ui::{ActiveTheme as _, FontRole, Label, RowHeights, ScrollArea, ScrollHandle};
use gpui::{
    AnyElement, App, ClickEvent, Context, IntoElement, MouseButton, MouseDownEvent, Pixels, Render, SharedString,
    Subscription, Window, div, prelude::*, px,
};

/// `AVATAR_PX`.
const AVATAR: f32 = 34.;
/// `--list-row-pad-y: 7px; --list-row-pad-x: 8px; --list-row-gap: 10px`.
const PAD_Y: f32 = 7.;
const PAD_X: f32 = 8.;
const GAP: f32 = 10.;
/// `h-7` / `h-8` headers, `h-11` loading row; the scroller's `pt-1 pb-3`.
const ONLINE_HEADER: f32 = 28.;
const OFFLINE_HEADER: f32 = 32.;
const LOADING_ROW: f32 = 44.;
const PAD_TOP: f32 = 4.;
const PAD_BOTTOM: f32 = 12.;
/// `w-[260px]`.
pub const MEMBERS_WIDTH: f32 = 260.;

/// `--member-row-h: round(calc((AVATAR_PX + 2 × pad-y) × row-scale), 1px)`.
pub fn member_row_height(row_scale: f32) -> f32 {
    ((AVATAR + 2. * PAD_Y) * row_scale).round()
}

#[derive(Debug, Clone, Copy, PartialEq)]
enum Item {
    Pad,
    Header { online: bool },
    Member { online: bool, index: usize },
    Loading,
    Empty,
}

pub struct MembersList {
    model: Model,
    server: Option<ServerId>,
    community: Option<(ServerId, Subscription)>,
    scroll: ScrollHandle,
    _watch: Watch,
}

impl MembersList {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch
            .store_if(&model.nav, cx, |this, nav, _| std::mem::replace(&mut this.server, nav.active_server) != nav.active_server)
            .store(&model.communities, cx)
            // A list that shows many people observes the stores whole.
            .store(&model.users, cx)
            .store(&model.live.presence.statuses, cx)
            .store(&model.live.presence.messages, cx)
            .store(&model.live.presence.avatars, cx)
            .store(&model.live.presence.own, cx);
        let server = model.nav.read(cx).active_server;
        Self { model, server, community: None, scroll: ScrollHandle::new(), _watch: watch }
    }

    fn follow(&mut self, community: Option<&Community>, cx: &mut Context<Self>) {
        let id = community.map(|c| c.id);
        if self.community.as_ref().map(|(id, _)| *id) != id {
            self.community = community.map(|c| (c.id, cx.observe(&c.roster, |_, _, cx| cx.notify())));
            self.scroll.scroll_to_top();
        }
    }
}

impl Render for MembersList {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = self.model.clone();
        let community = self.server.and_then(|id| model.community(id, cx));
        self.follow(community.as_ref(), cx);
        let theme = cx.theme();
        let server = self.server;

        let (online, offline, meta): (Rc<[Member]>, Rc<[Member]>, Option<RosterMeta>) = match &community {
            Some(c) => {
                let roster = c.roster.read(cx);
                let visible = roster.visible();
                let pick = |uids: &[Uid]| -> Rc<[Member]> { uids.iter().filter_map(|u| roster.member(*u).cloned()).collect() };
                (pick(&visible.online), pick(&visible.offline), roster.meta().cloned())
            }
            None => (Rc::from([]), Rc::from([]), None),
        };
        let online_count = meta.as_ref().map_or(online.len() as i64, |m| m.online_count);
        let offline_count = meta.as_ref().map_or(offline.len() as i64, |m| (m.total_members - m.online_count).max(0));
        let online_done = meta.as_ref().is_none_or(|m| m.online_complete);
        let show_offline = online_done && (!offline.is_empty() || offline_count > 0);
        let has_more = meta.as_ref().is_some_and(|m| m.has_more);
        let has_loading_row = has_more && (!online_done || show_offline);
        let loading = meta.as_ref().is_some_and(|m| m.loading_more || m.refetching);
        let empty = online.is_empty() && offline.is_empty() && !has_more;

        // The list as items of fixed heights: the window builds only those near the viewport.
        let row = member_row_height(theme.scaled.list_row_avatar_scale);
        let mut items: Vec<Item> = Vec::with_capacity(online.len() + offline.len() + 5);
        let mut offsets: Vec<Pixels> = vec![px(0.)];
        let mut push = |item: Item, height: f32| {
            items.push(item);
            let last = *offsets.last().expect("starts at 0");
            offsets.push(last + px(height));
        };
        push(Item::Pad, PAD_TOP);
        push(Item::Header { online: true }, ONLINE_HEADER);
        for index in 0..online.len() {
            push(Item::Member { online: true, index }, row);
        }
        if show_offline {
            push(Item::Header { online: false }, OFFLINE_HEADER);
            for index in 0..offline.len() {
                push(Item::Member { online: false, index }, row);
            }
        }
        if has_loading_row {
            push(Item::Loading, LOADING_ROW);
        }
        if empty {
            push(Item::Empty, 32.);
        }
        push(Item::Pad, PAD_BOTTOM);
        let loading_index = items.iter().position(|i| *i == Item::Loading);
        let count = items.len();
        let items: Rc<[Item]> = items.into();

        let render_rows = {
            let model = model.clone();
            move |range: Range<usize>, _: &mut Window, cx: &mut App| -> Vec<AnyElement> {
                // The paging trigger: the loading row (the end of what is loaded) is within the
                // drawn stretch (the viewport and half of it either way).
                if let (Some(index), Some(community)) = (loading_index, community.clone())
                    && range.contains(&index)
                    && !loading
                {
                    cx.defer(move |cx| community.roster.update(cx, |r, cx| r.load_more(cx)));
                }
                range
                    .map(|i| match items[i] {
                        Item::Pad => div().into_any_element(),
                        Item::Header { online: is_online } => header(is_online, if is_online { online_count } else { offline_count }, cx),
                        Item::Member { online: is_online, index } => {
                            let member = if is_online { &online[index] } else { &offline[index] };
                            member_row(&model, server, member, is_online, cx)
                        }
                        Item::Loading => loading_row(loading, community.clone(), cx),
                        Item::Empty => div()
                            .mt_4()
                            .flex()
                            .justify_center()
                            .child(Label::new("No members yet").size(12.).color(cx.theme().color_text_muted))
                            .into_any_element(),
                    })
                    .collect()
            }
        };

        div()
            .relative()
            .flex()
            .flex_none()
            .flex_col()
            .w(px(MEMBERS_WIDTH))
            .h_full()
            .border_l_1()
            .border_color(theme.color_border)
            .bg(theme.color_bg_dark)
            .children(crate::probes::probe("member-list"))
            .child(
                ScrollArea::new("member-list")
                    .track_scroll(&self.scroll)
                    .flex_1()
                    .windowed(count, RowHeights::Offsets(offsets.into()), render_rows),
            )
    }
}

/// `ONLINE — 1,234` (`toLocaleString()`): the count grouped as the system's regional settings say.
fn header(online: bool, count: i64, cx: &App) -> AnyElement {
    let theme = cx.theme();
    let text: SharedString = format!("{} — {}", if online { "ONLINE" } else { "OFFLINE" }, Regional::get().group_signed(count)).into();
    crate::probes::text(if online { "members-header:online" } else { "members-header:offline" }, &text);
    div()
        .size_full()
        .flex()
        .items_end()
        .px_4()
        .pb_1()
        .child(
            Label::new(text)
                .font(FontRole::Section)
                .size(theme.scaled.text_section)
                .line_height(theme.scaled.text_section)
                .weight(500.)
                .tracking(theme.palette.tracking_section)
                .color(theme.color_text_muted),
        )
        .into_any_element()
}

fn loading_row(busy: bool, community: Option<Community>, cx: &App) -> AnyElement {
    let theme = cx.theme();
    div()
        .size_full()
        .flex()
        .items_center()
        .px_4()
        .child(
            div()
                .id("members-load-more")
                .h_8()
                .w_full()
                .flex()
                .items_center()
                .justify_center()
                .rounded(theme.radius_sm)
                .text_color(theme.color_text_muted)
                .when(!busy, |this| {
                    let (bg, fg) = (theme.color_surface_hover, theme.color_text_secondary);
                    this.cursor_pointer().hover(move |s| s.bg(bg).text_color(fg)).on_click(move |_: &ClickEvent, _, cx| {
                        if let Some(community) = &community {
                            community.roster.update(cx, |r, cx| r.load_more(cx));
                        }
                    })
                })
                .child(Label::new(if busy { "Loading…" } else { "Load more" }).size(12.)),
        )
        .into_any_element()
}

/// One member (`MemberRow`): avatar with its dot (dimmed offline), the name (nickname first),
/// the status message. A click opens the profile beside the row, a right click the user menu.
fn member_row(model: &Model, server: Option<ServerId>, member: &Member, online: bool, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let scale = theme.scaled.list_row_avatar_scale;
    let uid = member.uid;
    let me = model.auth.read(cx).uid;
    // The community decides the part; central's status refines the dot inside Online.
    let status = if online {
        let s = presence_of(uid, true, cx);
        if s.is_online() { s } else { DisplayStatus::Online }
    } else if uid == me && model.live.presence.own.read(cx).status == OwnStatus::Invisible {
        DisplayStatus::Invisible
    } else {
        DisplayStatus::Offline
    };
    if crate::probes::enabled() {
        // The dot as drawn, for the self-tests (`people-interest`).
        let dot = match status {
            DisplayStatus::Online => "online",
            DisplayStatus::Away => "away",
            DisplayStatus::Dnd => "dnd",
            _ => "offline",
        };
        crate::probes::text(format!("member-status:{uid}"), &SharedString::new_static(dot));
    }
    let name = decibell_common::user_name(uid, &member.username, cx);
    let shown = if member.nickname.is_empty() { name.clone() } else { member.nickname.clone() };
    let message = decibell_common::status_message_of(uid, cx);
    let (text, weight) = if online { (theme.color_text_secondary, 500.) } else { (theme.color_text_muted, 400.) };
    let group: SharedString = format!("member-{uid}").into();
    let bounds = BoundsCell::new();
    div()
        .size_full()
        .px_3()
        .child(
            div()
                .id(("member", uid as u64))
                .group(group.clone())
                .relative()
                .children(crate::probes::probe(format!("member:{uid}")))
                .size_full()
                .flex()
                .items_center()
                .px(px(PAD_X * scale))
                .gap(px(GAP * scale))
                .rounded(theme.radius_sm)
                .cursor_pointer()
                .hover(|s| s.bg(theme.color_surface_hover))
                .child(bounds.probe())
                .on_click({
                    let bounds = bounds.clone();
                    move |_: &ClickEvent, _, cx| open_profile_beside(uid, bounds.get(), server, cx)
                })
                .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| open_user_menu(uid, e.position, server, cx))
                .child(
                    UserAvatar::new(uid, name, AVATAR * scale)
                        .dot(AvatarDot::new(status, theme.color_bg_tertiary, 13. * scale))
                        .dimmed(!online),
                )
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_color(text)
                                .group_hover(group.clone(), {
                                    let c = theme.color_text_primary;
                                    move |s| s.text_color(c)
                                })
                                .child(
                                    Label::new(shown)
                                        .font(FontRole::Channel)
                                        .size(theme.scaled.text_member)
                                        .line_height(18.)
                                        .weight(weight)
                                        .truncate(),
                                ),
                        )
                        .when_some(message, |this, message| {
                            this.child(
                                StatusMessageText::new(message)
                                    .size(theme.scaled.text_micro)
                                    .line_height(16.)
                                    .emoji_size(13.)
                                    .color(theme.color_text_muted),
                            )
                        }),
                ),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rows_are_whole_pixels_and_counts_group_like_the_renderer() {
        assert_eq!(member_row_height(1.), 48.);
        assert_eq!(member_row_height(0.85), 41.);
        assert_eq!(member_row_height(1.15), 55.);
        // The header's count: `decibell_model::regional` (its tests cover the groupings).
        let en_us = decibell_model::regional::Regional::en_us();
        assert_eq!((en_us.group_signed(298), en_us.group_signed(1_234_567)), ("298".to_string(), "1,234,567".to_string()));
    }
}
