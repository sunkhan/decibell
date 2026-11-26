//! Home's Friends page (`features/friends/FriendsPage.tsx`): the header with its filter
//! segments, the search row with Add Friend, and the friend list in sections (Pending, Online,
//! Offline, Blocked) from the friends store. The shell's default for [`crate::Slot::Home`].
//!
//! Left to the features that own them: the search field's typing and the add form's (the text
//! editor), the user menu's body (More).

use decibell_common::{AvatarDot, BoundsCell, StatusMessageText, UserAvatar, Watch, open_profile_at, open_user_menu, presence_of};
use decibell_model::live::friends::{Friend, FriendAction, FriendStatus};
use decibell_model::{Model, Uid, actions};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, ScrollArea, TextField, title_tooltip};
use gpui::{
    AnyElement, App, ClickEvent, Context, IntoElement, MouseButton, MouseDownEvent, Render, SharedString, Window, div,
    point, prelude::*, px,
};

use crate::ink::Ink;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Filter {
    All,
    Online,
    Pending,
    Blocked,
}

impl Filter {
    fn allows(self, status: FriendStatus) -> bool {
        match self {
            Filter::All => true,
            Filter::Online => status == FriendStatus::Online,
            Filter::Pending => status.is_pending(),
            Filter::Blocked => status == FriendStatus::Blocked,
        }
    }
}

/// Pending leads: an unanswered request is the only thing here that needs a decision.
const SECTIONS: [(&str, &[FriendStatus]); 4] = [
    ("Pending", &[FriendStatus::PendingIncoming, FriendStatus::PendingOutgoing]),
    ("Online", &[FriendStatus::Online]),
    ("Offline", &[FriendStatus::Offline]),
    ("Blocked", &[FriendStatus::Blocked]),
];

fn status_label(status: FriendStatus) -> &'static str {
    match status {
        FriendStatus::Online => "Online",
        FriendStatus::Offline => "Offline",
        FriendStatus::PendingIncoming => "Wants to be friends",
        FriendStatus::PendingOutgoing => "Request sent",
        FriendStatus::Blocked => "Blocked",
    }
}

pub struct FriendsPage {
    filter: Filter,
    search: SharedString,
    add_open: bool,
    _watch: Watch,
}

impl FriendsPage {
    pub fn new(cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let mut watch = Watch::new();
        watch.store(&model.live.friends, cx).people(cx, |_, _| true);
        Self { filter: Filter::All, search: SharedString::default(), add_open: false, _watch: watch }
    }

    fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            cx.notify();
        }
    }
}

/// The DM view with `uid`'s conversation (created when new).
fn open_conversation(uid: Uid, cx: &mut App) {
    let fallback = Model::global(cx).live.friends.read(cx).get(uid).map(|f| f.username.clone()).unwrap_or_default();
    let name = decibell_common::user_name(uid, &fallback, cx);
    actions::dm::open_dm(uid, name, cx);
}

/// `Segment`: a filter tab with an optional count (red for incoming requests).
#[allow(clippy::too_many_arguments)] // the TSX component's props, flat
fn segment(
    id: &'static str,
    label: &'static str,
    count: usize,
    alert: bool,
    active: bool,
    t: &Ink,
    text_member: f32,
    text_micro: f32,
) -> gpui::Stateful<gpui::Div> {
    let (bg, fg) = if active { (Some(t.color_accent_soft), t.color_accent_bright) } else { (None, t.color_text_muted) };
    div()
        .id(id)
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(t.radius_sm)
        .px_2p5()
        .py(px(5.))
        .cursor_pointer()
        .when_some(bg, |this, bg| this.bg(bg))
        .when(!active, |this| {
            let (bg, fg) = (t.color_surface_hover, t.color_text_primary);
            this.hover(move |s| s.bg(bg).text_color(fg))
        })
        .text_color(fg)
        .child(Label::new(label).font(FontRole::Channel).size(text_member).weight(500.))
        .when(count > 0, |this| {
            let (bg, fg) = if alert {
                (t.color_error, gpui::white())
            } else if active {
                (t.color_accent_mid, t.color_accent_bright)
            } else {
                (t.color_surface_active, t.color_text_muted)
            };
            this.child(
                div()
                    .rounded(t.radius_sm)
                    .bg(bg)
                    .px_1p5()
                    .child(Label::new(SharedString::from(count.to_string())).font(FontRole::Meta).size(text_micro).color(fg)),
            )
        })
}

/// `RowButton`: filled for the affirmative action, outlined otherwise (red on hover where
/// destructive).
fn row_button(id: &'static str, label: &'static str, icon: Option<IconName>, filled: bool, danger: bool, t: &Ink, text_member: f32) -> gpui::Stateful<gpui::Div> {
    let (bg, fg) = if filled { (t.color_accent, t.color_on_accent) } else { (t.color_bg_light, t.color_text_secondary) };
    div()
        .id(id)
        .flex()
        .flex_none()
        .h(px(30.))
        .items_center()
        .gap_1p5()
        .rounded(t.radius_md)
        .px_2p5()
        .bg(bg)
        .text_color(fg)
        .cursor_pointer()
        .when(!filled, |this| this.border_1().border_color(t.color_border))
        .map(|this| {
            let (hover_bg, hover_fg, hover_border) = (t.color_accent_hover, t.color_text_primary, t.color_error);
            let error = t.color_error;
            if filled {
                this.hover(move |s| s.bg(hover_bg))
            } else if danger {
                this.hover(move |s| s.border_color(hover_border).text_color(error))
            } else {
                this.hover(move |s| s.text_color(hover_fg))
            }
        })
        .children(icon.map(|icon| Icon::new(icon).size(px(15.)).stroke_width(1.8)))
        .child(Label::new(label).font(FontRole::Channel).size(text_member).weight(600.))
}

fn icon_button(id: &'static str, icon: IconName, title: &'static str, t: &Ink) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .group(id)
        .flex()
        .flex_none()
        .size_8()
        .items_center()
        .justify_center()
        .rounded(t.radius_md)
        .border_1()
        .border_color(t.color_border_divider)
        .bg(t.color_bg_light)
        .cursor_pointer()
        .hover(|s| s.bg(t.color_bg_lighter))
        .tooltip(title_tooltip(title))
        .child(Icon::new(icon).size(px(16.)).stroke_width(1.8).color(t.color_text_muted).group_hover(id, t.color_text_primary))
}

fn friend_row(friend: &Friend, cx: &mut App) -> AnyElement {
    let theme = cx.theme();
    let t = crate::ink::base(cx);
    let scale = theme.scaled.list_row_avatar_scale;
    let (text_body, text_member, text_micro) = (theme.scaled.text_body, theme.scaled.text_member, theme.scaled.text_micro);
    let uid = friend.uid;
    let status = friend.status;
    let name = decibell_common::user_name(uid, &friend.username, cx);
    let presence = presence_of(uid, status == FriendStatus::Online, cx);
    let message = status.is_accepted().then(|| decibell_common::status_message_of(uid, cx)).flatten();
    let can_message = status.is_accepted();
    let pinned = status.is_pending();
    let act = move |action: FriendAction, name: SharedString| move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
        cx.stop_propagation();
        actions::friends::act(action, uid, name.clone(), cx);
    };
    let actions_row: Vec<AnyElement> = match status {
        FriendStatus::PendingIncoming => vec![
            row_button("accept", "Accept", Some(IconName::Checkmark), true, false, &t, text_member)
                .on_click(act(FriendAction::Accept, name.clone()))
                .into_any_element(),
            row_button("decline", "Decline", Some(IconName::SettingsX), false, true, &t, text_member)
                .on_click(act(FriendAction::Reject, name.clone()))
                .into_any_element(),
        ],
        FriendStatus::PendingOutgoing => vec![row_button("cancel", "Cancel", Some(IconName::SettingsX), false, true, &t, text_member)
            .on_click(act(FriendAction::Remove, name.clone()))
            .into_any_element()],
        FriendStatus::Blocked => vec![row_button("unblock", "Unblock", None, false, false, &t, text_member)
            .on_click(act(FriendAction::Remove, name.clone()))
            .into_any_element()],
        _ => {
            let more = BoundsCell::new();
            vec![
                icon_button("message", IconName::SettingsMessage, "Message", &t)
                    .on_click(move |_: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        open_conversation(uid, cx);
                    })
                    .into_any_element(),
                icon_button("more", IconName::SettingsMore, "More", &t)
                    .relative()
                    .child(more.probe())
                    .on_click({
                        let more = more.clone();
                        move |_: &ClickEvent, _, cx| {
                            cx.stop_propagation();
                            // The user menu is 220 px wide: right-aligned under the button.
                            let r = more.get();
                            let at = point((r.right() - px(220.)).max(px(8.)), r.bottom() + px(6.));
                            open_user_menu(uid, at, None, cx);
                        }
                    })
                    .into_any_element(),
            ]
        }
    };
    let subline: AnyElement = match message {
        Some(message) => StatusMessageText::new(message).size(text_micro).color(t.color_text_muted).into_any_element(),
        None => Label::new(if status.is_accepted() { presence.label() } else { status_label(status) })
            .font(FontRole::Meta)
            .size(text_micro)
            .color(t.color_text_muted)
            .into_any_element(),
    };
    let group: SharedString = format!("friend-{uid}").into();
    let bounds = BoundsCell::new();
    let mut avatar = UserAvatar::new(uid, name.clone(), 36. * scale);
    if status.is_accepted() {
        avatar = avatar.dot(AvatarDot::new(presence, t.color_bg_mid, 13. * scale));
    }
    div()
        .id(("friend", uid as u64))
        .group(group.clone())
        .relative()
        .flex()
        .items_center()
        .py(px(8. * scale))
        .px(px(8. * scale))
        .gap(px(12. * scale))
        .rounded(t.radius_md)
        .cursor_pointer()
        .hover(|s| s.bg(t.color_surface_hover))
        .child(bounds.probe())
        .on_click(move |_: &ClickEvent, _, cx| {
            // A friend row opens the conversation; one you can't message opens the profile.
            if can_message {
                open_conversation(uid, cx);
            } else {
                let r = bounds.get();
                open_profile_at(uid, point(r.left() + px(40.), r.top()), None, cx);
            }
        })
        .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| open_user_menu(uid, e.position, None, cx))
        .child(avatar)
        .child(
            div()
                .flex()
                .flex_col()
                .flex_1()
                .min_w_0()
                .child(Label::new(name).font(FontRole::Channel).size(text_body).weight(500.).color(t.color_text_primary).truncate())
                .child(subline),
        )
        .child(
            div()
                .flex()
                .flex_none()
                .items_center()
                .gap_1p5()
                .when(!pinned, |this| this.invisible().group_hover(group, |s| s.visible()))
                .children(actions_row),
        )
        .into_any_element()
}

impl Render for FriendsPage {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let theme = cx.theme();
        let t = crate::ink::base(cx);
        let (text_title, text_member, text_micro, text_section, text_body) = (
            theme.scaled.text_title,
            theme.scaled.text_member,
            theme.scaled.text_micro,
            theme.scaled.text_section,
            theme.scaled.text_body,
        );
        let (weight, tracking_title, tracking_section) =
            (theme.palette.font_weight_emphasis, theme.palette.tracking_title, theme.palette.tracking_section);
        let friends: Vec<Friend> = model.live.friends.read(cx).list().to_vec();
        let error = model.live.friends.read(cx).last_action_error.clone();
        let count = |f: &dyn Fn(FriendStatus) -> bool| friends.iter().filter(|x| f(x.status)).count();
        let accepted = count(&|s| s.is_accepted());
        let online = count(&|s| s == FriendStatus::Online);
        let pending_in = count(&|s| s == FriendStatus::PendingIncoming);
        let pending = count(&|s| s.is_pending());
        let blocked = count(&|s| s == FriendStatus::Blocked);
        let needle = self.search.trim().to_lowercase();
        let filter = self.filter;

        let mut body: Vec<AnyElement> = Vec::new();
        let mut shown = 0;
        for (label, statuses) in SECTIONS {
            let items: Vec<&Friend> = friends
                .iter()
                .filter(|f| statuses.contains(&f.status) && filter.allows(f.status))
                .filter(|f| decibell_common::user_name(f.uid, &f.username, cx).to_lowercase().contains(&needle))
                .collect();
            if items.is_empty() {
                continue;
            }
            shown += items.len();
            let mut section = div().mt_2p5();
            // With one status selected the segment above says what this is.
            if filter == Filter::All {
                section = section.child(
                    div()
                        .mb_1()
                        .flex()
                        .items_center()
                        .gap_2()
                        .px_2()
                        .child(
                            Label::new(SharedString::from(label.to_uppercase()))
                                .font(FontRole::Section)
                                .size(text_section)
                                .line_height(text_section)
                                .weight(500.)
                                .tracking(tracking_section)
                                .color(t.color_text_muted),
                        )
                        .child(
                            div().opacity(0.75).child(
                                Label::new(SharedString::from(items.len().to_string()))
                                    .font(FontRole::Section)
                                    .size(text_section)
                                    .line_height(text_section)
                                    .weight(500.)
                                    .color(t.color_text_muted),
                            ),
                        ),
                );
            }
            for friend in items {
                section = section.child(friend_row(friend, cx));
            }
            body.push(section.into_any_element());
        }
        if shown == 0 {
            let (title, hint) = if friends.is_empty() {
                ("No friends yet", "Add someone with the button above")
            } else {
                ("Nothing here", "No friends match this filter")
            };
            body.push(
                div()
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap_1p5()
                    .pt(px(56.))
                    .child(Label::new(title).font(FontRole::Channel).size(text_body).weight(500.).color(t.color_text_secondary))
                    .child(Label::new(hint).font(FontRole::Meta).size(text_micro).color(t.color_text_muted))
                    .into_any_element(),
            );
        }

        let segments = div()
            .ml_auto()
            .flex()
            .items_center()
            .gap_0p5()
            .child(segment("filter-all", "All", 0, false, filter == Filter::All, &t, text_member, text_micro)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::All, cx))))
            .child(segment("filter-online", "Online", online, false, filter == Filter::Online, &t, text_member, text_micro)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::Online, cx))))
            .child(
                segment(
                    "filter-pending",
                    "Pending",
                    if pending_in > 0 { pending_in } else { pending },
                    pending_in > 0,
                    filter == Filter::Pending,
                    &t,
                    text_member,
                    text_micro,
                )
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::Pending, cx))),
            )
            .child(segment("filter-blocked", "Blocked", blocked, false, filter == Filter::Blocked, &t, text_member, text_micro)
                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::Blocked, cx))));

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(t.color_bg_mid)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h_12()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(t.color_border_divider)
                    .px_5()
                    .child(
                        Label::new("Friends")
                            .font(FontRole::Display)
                            .size(text_title)
                            .weight(weight)
                            .tracking(tracking_title)
                            .color(t.color_text_bright),
                    )
                    .child(Label::new(SharedString::from(accepted.to_string())).font(FontRole::Meta).size(text_micro).color(t.color_text_muted))
                    .child(segments),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2p5()
                    .border_b_1()
                    .border_color(t.color_border_divider)
                    .px_5()
                    .py_3()
                    .child(
                        div().flex_1().min_w_0().child(
                            TextField::new("friends-search")
                                .leading_icon(IconName::Search)
                                .placeholder("Search friends")
                                .value(self.search.clone()),
                        ),
                    )
                    .child(
                        div()
                            .id("add-friend")
                            .flex()
                            .flex_none()
                            .h_9()
                            .items_center()
                            .gap_1p5()
                            .rounded(t.radius_md)
                            .bg(t.color_accent)
                            .px_4()
                            .cursor_pointer()
                            .hover(|s| s.bg(t.color_accent_hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, _, cx| {
                                this.add_open = !this.add_open;
                                cx.notify();
                            }))
                            .child(Icon::new(IconName::SettingsPlus).size(px(16.)).stroke_width(1.8).color(t.color_on_accent))
                            .child(Label::new("Add Friend").font(FontRole::Channel).size(text_member).weight(600.).color(t.color_on_accent)),
                    ),
            )
            .when(self.add_open, |this| {
                this.child(
                    div()
                        .flex_none()
                        .border_b_1()
                        .border_color(t.color_border_divider)
                        .bg(t.color_bg_light)
                        .px_5()
                        .py_3p5()
                        .child(
                            div().mb_1p5().child(
                                Label::new("SEND A FRIEND REQUEST").font(FontRole::Meta).size(text_micro).tracking(0.1).color(t.color_text_muted),
                            ),
                        )
                        .child(TextField::new("add-friend-username").placeholder("Username"))
                        .child(
                            div().mt_1p5().child(
                                Label::new("Usernames are case-sensitive.").font(FontRole::Meta).size(text_micro).color(t.color_text_muted),
                            ),
                        ),
                )
            })
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .mx_5()
                        .mt_2()
                        .flex_none()
                        .rounded(t.radius_md)
                        .bg(t.color_error.opacity(0.1))
                        .px_3()
                        .py_2()
                        .child(Label::new(error).size(text_member).color(t.color_error)),
                )
            })
            .child(ScrollArea::new("friends-list").flex_1().child(div().px_3().pb_5().pt_1().children(body)))
    }
}
