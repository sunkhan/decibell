//! The Friends page (`FriendsPage.tsx`): the header with its filter segments, the search row with
//! Add Friend (and the add form under it), the action error, and the friend list in sections
//! (Pending, Online, Offline, Blocked; a section's header only under "All"). Windowed: only the
//! rows near the viewport are built, at the height the renderer's `list-row` gives them.

use std::collections::HashSet;
use std::rc::Rc;
use std::time::Duration;

use decibell_common::{AvatarDot, BoundsCell, StatusMessageText, UserAvatar, Watch, open_profile_at, open_user_menu, presence_of};
use decibell_model::live::friends::{Friend, FriendAction, FriendStatus};
use decibell_model::{Model, Uid, actions};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FontRole, Icon, IconName, Label, RowHeights, ScrollArea, title_tooltip};
use gpui::{
    AnyElement, App, ClickEvent, Context, Entity, IntoElement, MouseButton, MouseDownEvent, Render, SharedString,
    Subscription, Task, Window, div, point, prelude::*, px,
};

use super::{open_conversation, status_label};
use crate::ui::{BoxLook, input_box};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Filter {
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

/// How long the add form's error shows (`setTimeout(…, 5000)`).
const ADD_ERROR_LIFETIME: Duration = Duration::from_secs(5);

/// One windowed item of the list.
#[derive(Clone)]
enum Item {
    /// `pt-1` above everything / `pb-5` below.
    Pad(f32),
    /// A section's `mt-2.5` and, under "All", its header.
    Header(&'static str, usize),
    Gap,
    Row(Friend),
}

pub struct FriendsPage {
    filter: Filter,
    search: Entity<InputState>,
    add_open: bool,
    add: Entity<InputState>,
    add_error: Option<SharedString>,
    add_error_timer: Option<Task<()>>,
    /// The uids of the friend list (names, statuses and messages of anyone else don't redraw).
    shown: HashSet<Uid>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl FriendsPage {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let search = cx.new(|cx| InputState::single_line(window, cx).placeholder("Search friends"));
        let add = cx.new(|cx| InputState::single_line(window, cx).placeholder("Username"));
        let mut watch = Watch::new();
        watch
            .store_if(&model.live.friends, cx, |this: &mut FriendsPage, friends, _| {
                this.shown = friends.list().iter().map(|f| f.uid).collect();
                true
            })
            .people(cx, |this, uid| this.shown.contains(&uid));
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if *event == InputEvent::Change {
                    cx.notify();
                }
            }),
            cx.subscribe(&add, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => this.send_request(cx),
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }),
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Focus | InputEvent::Blur) {
                    cx.notify();
                }
            }),
        ];
        let shown = model.live.friends.read(cx).list().iter().map(|f| f.uid).collect();
        let (filter, add_open) = cx.default_global::<super::Initial>().0.take().unwrap_or((Filter::All, false));
        let entity = cx.entity();
        crate::hooks::remember(&entity, cx);
        if add_open {
            add.update(cx, |state, cx| state.focus(window, cx));
        }
        Self {
            filter,
            search,
            add_open,
            add,
            add_error: None,
            add_error_timer: None,
            shown,
            _watch: watch,
            _subscriptions: subscriptions,
        }
    }

    pub fn filter(&self) -> Filter {
        self.filter
    }

    /// What a self-test reports when a typed request went nowhere: the add form open, its field's
    /// text and focus, the search field's text, the form's error.
    pub(crate) fn add_state(&self, window: &Window, cx: &App) -> String {
        format!(
            "the add form open {}, its field {:?} (focused {}), the search field {:?}, the form's error {:?}",
            self.add_open,
            self.add.read(cx).value(),
            gpui::Focusable::focus_handle(self.add.read(cx), cx).is_focused(window),
            self.search.read(cx).value(),
            self.add_error
        )
    }

    pub fn set_filter(&mut self, filter: Filter, cx: &mut Context<Self>) {
        if self.filter != filter {
            self.filter = filter;
            cx.notify();
        }
    }

    /// Opens the add form (the screens and the self-tests; a click on Add Friend toggles it).
    pub fn open_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if !self.add_open {
            self.toggle_add(window, cx);
        }
    }

    fn toggle_add(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.add_open = !self.add_open;
        if self.add_open {
            // `autoFocus`.
            self.add.update(cx, |state, cx| state.focus(window, cx));
        }
        cx.notify();
    }

    fn set_add_error(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        self.add_error_timer = error.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ADD_ERROR_LIFETIME).await;
                let _ = this.update(cx, |this, cx| {
                    this.add_error = None;
                    this.add_error_timer = None;
                    cx.notify();
                });
            })
        });
        self.add_error = error;
        cx.notify();
    }

    /// `handleAdd`: the typed name to central; the form closes once the request left.
    fn send_request(&mut self, cx: &mut Context<Self>) {
        let target = self.add.read(cx).value().trim().to_string();
        if target.is_empty() {
            return;
        }
        self.set_add_error(None, cx);
        let this = cx.weak_entity();
        actions::friends::add_friend(target, cx, move |result, cx| {
            let _ = this.update(cx, |this, cx| match result {
                Ok(()) => {
                    this.add.update(cx, |state, cx| state.clear(cx));
                    this.add_open = false;
                    cx.notify();
                }
                Err(error) => this.set_add_error(Some(error), cx),
            });
        });
    }

    /// The list's items (sections, rows) and their heights, for the current filter and search.
    fn items(&self, friends: &[Friend], row_height: f32, section: f32, cx: &App) -> (Rc<[Item]>, Rc<[gpui::Pixels]>, usize) {
        let needle = self.search.read(cx).value().trim().to_lowercase();
        let mut items = vec![Item::Pad(4.)];
        let mut shown = 0;
        for (label, statuses) in SECTIONS {
            let rows: Vec<&Friend> = friends
                .iter()
                .filter(|f| statuses.contains(&f.status) && self.filter.allows(f.status))
                .filter(|f| decibell_common::user_name(f.uid, &f.username, cx).to_lowercase().contains(&needle))
                .collect();
            if rows.is_empty() {
                continue;
            }
            shown += rows.len();
            // With one status selected the segment above says what this is.
            items.push(if self.filter == Filter::All { Item::Header(label, rows.len()) } else { Item::Gap });
            items.extend(rows.into_iter().map(|f| Item::Row(f.clone())));
        }
        items.push(Item::Pad(20.));
        let mut offsets = Vec::with_capacity(items.len() + 1);
        let mut y = 0.;
        offsets.push(px(0.));
        for item in &items {
            y += match item {
                Item::Pad(h) => *h,
                // `mt-2.5`, the header (`leading-none`) and its `mb-1`.
                Item::Header(..) => 10. + section + 4.,
                Item::Gap => 10.,
                Item::Row(_) => row_height,
            };
            offsets.push(px(y));
        }
        (items.into(), offsets.into(), shown)
    }
}

/// `list-row` with the page's `--list-row-pad-y: 8px`: the 36 px avatar or the two text lines,
/// whichever is taller, between the scaled paddings.
pub fn row_height(cx: &App) -> f32 {
    let s = &cx.theme().scaled;
    let scale = s.list_row_avatar_scale;
    (36. * scale).max(1.5 * (s.text_body + s.text_micro)) + 2. * 8. * scale
}

/// `Segment`: a filter tab with an optional count (red for incoming requests).
fn segment(id: &'static str, label: &'static str, count: usize, alert: bool, active: bool, cx: &App) -> gpui::Stateful<gpui::Div> {
    let probe = decibell_shell::probes::probe(format!("friends-{id}"));
    let t = cx.theme();
    let (text_member, text_micro) = (t.scaled.text_member, t.scaled.text_micro);
    let fg = if active { t.color_accent_bright } else { t.color_text_muted };
    div()
        .id(id)
        .relative()
        .children(probe)
        .flex()
        .items_center()
        .gap_1p5()
        .rounded(t.radius_sm)
        .px_2p5()
        .py(px(5.))
        .cursor_pointer()
        .when(active, |this| this.bg(t.color_accent_soft))
        .when(!active, |this| {
            let (bg, fg) = (t.color_surface_hover, t.color_text_primary);
            this.hover(move |s| s.bg(bg).text_color(fg))
        })
        .text_color(fg)
        .child(Label::new(label).font(FontRole::Channel).size(text_member).weight(500.))
        .when(count > 0, |this| {
            let (bg, fg) = if alert {
                // `text-white` on the error red: a literal of the renderer.
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
                    .child(Label::new(crate::fmt::count(count)).font(FontRole::Meta).size(text_micro).color(fg)),
            )
        })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum RowButtonLook {
    Filled,
    Outline,
    Danger,
}

/// `RowButton`: filled for the affirmative action, outlined otherwise (red on hover where
/// destructive).
fn row_button(id: &'static str, label: &'static str, icon: Option<IconName>, look: RowButtonLook, cx: &App) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme();
    let filled = look == RowButtonLook::Filled;
    let (bg, fg) = if filled { (t.color_accent, t.color_on_accent) } else { (t.color_bg_light, t.color_text_secondary) };
    let (hover_bg, hover_fg, error) = (t.color_accent_hover, t.color_text_primary, t.color_error);
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
        .tooltip(title_tooltip(label))
        .when(!filled, |this| this.border_1().border_color(t.color_border))
        .map(|this| match look {
            RowButtonLook::Filled => this.hover(move |s| s.bg(hover_bg)),
            RowButtonLook::Danger => this.hover(move |s| s.border_color(error).text_color(error)),
            RowButtonLook::Outline => this.hover(move |s| s.text_color(hover_fg)),
        })
        .children(icon.map(|icon| Icon::new(icon).size(px(15.)).stroke_width(1.8)))
        .child(Label::new(label).font(FontRole::Channel).size(t.scaled.text_member).weight(600.))
}

/// `IconButton`: the row's Message and More.
fn icon_button(id: &'static str, icon: IconName, title: &'static str, cx: &App) -> gpui::Stateful<gpui::Div> {
    let t = cx.theme();
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
        .child(Icon::new(icon).size(px(16.)).color(t.color_text_muted).group_hover(id, t.color_text_primary))
}

/// `FriendRow`.
fn friend_row(friend: &Friend, cx: &mut App) -> AnyElement {
    let uid = friend.uid;
    let status = friend.status;
    let name = decibell_common::user_name(uid, &friend.username, cx);
    let presence = presence_of(uid, false, cx);
    let accepted = status.is_accepted();
    let message = accepted.then(|| decibell_common::status_message_of(uid, cx)).flatten();
    let theme = cx.theme();
    let scale = theme.scaled.list_row_avatar_scale;
    let (text_body, text_micro) = (theme.scaled.text_body, theme.scaled.text_micro);
    let (surface_hover, bg_mid, primary, muted) =
        (theme.color_surface_hover, theme.color_bg_mid, theme.color_text_primary, theme.color_text_muted);
    let act = move |action: FriendAction, name: SharedString| {
        move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            cx.stop_propagation();
            actions::friends::act(action, uid, name.clone(), cx);
        }
    };
    // Requests keep their buttons visible: something awaiting a decision shouldn't hide.
    let pinned = status.is_pending();
    let actions_row: Vec<AnyElement> = match status {
        FriendStatus::PendingIncoming => vec![
            row_button("accept", "Accept", Some(IconName::SettingsCheck), RowButtonLook::Filled, cx)
                .relative()
                .children(decibell_shell::probes::probe(format!("friend-accept:{uid}")))
                .on_click(act(FriendAction::Accept, name.clone()))
                .into_any_element(),
            row_button("decline", "Decline", Some(IconName::SettingsX), RowButtonLook::Danger, cx)
                .on_click(act(FriendAction::Reject, name.clone()))
                .into_any_element(),
        ],
        FriendStatus::PendingOutgoing => vec![
            row_button("cancel", "Cancel", Some(IconName::SettingsX), RowButtonLook::Danger, cx)
                .on_click(act(FriendAction::Remove, name.clone()))
                .into_any_element(),
        ],
        FriendStatus::Blocked => vec![
            row_button("unblock", "Unblock", None, RowButtonLook::Outline, cx)
                .on_click(act(FriendAction::Remove, name.clone()))
                .into_any_element(),
        ],
        _ => {
            let more = BoundsCell::new();
            vec![
                icon_button("message", IconName::HomeFriendMessage, "Message", cx)
                    .on_click(move |_: &ClickEvent, _, cx| {
                        cx.stop_propagation();
                        open_conversation(uid, cx);
                    })
                    .into_any_element(),
                icon_button("more", IconName::HomeFriendMore, "More", cx)
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
        // A friend's custom status stands in for "Online" / "Away", as Discord's page reads.
        Some(message) => StatusMessageText::new(message).size(text_micro).color(muted).into_any_element(),
        None => Label::new(if accepted { presence.label() } else { status_label(status) })
            .font(FontRole::Meta)
            .size(text_micro)
            .color(muted)
            .into_any_element(),
    };
    let group: SharedString = format!("friend-{uid}").into();
    let bounds = BoundsCell::new();
    let mut avatar = UserAvatar::new(uid, name.clone(), 36. * scale);
    if accepted {
        avatar = avatar.dot(AvatarDot::new(presence, bg_mid, 13. * scale));
    }
    div()
        .id(("friend", uid as u64))
        .group(group.clone())
        .relative()
        .size_full()
        .flex()
        .items_center()
        .py(px(8. * scale))
        .px(px(8. * scale))
        .gap(px(12. * scale))
        .rounded(cx.theme().radius_md)
        .cursor_pointer()
        .hover(move |s| s.bg(surface_hover))
        .child(bounds.probe())
        .children(decibell_shell::probes::probe(format!("friend:{uid}")))
        .on_click(move |_: &ClickEvent, _, cx| {
            // A friend row opens the conversation; one you can't message opens the profile.
            if accepted {
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
                .child(Label::new(name).font(FontRole::Channel).size(text_body).weight(500.).color(primary).truncate())
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
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let friends: Vec<Friend> = model.live.friends.read(cx).list().to_vec();
        let error = model.live.friends.read(cx).last_action_error.clone();
        let count = |f: &dyn Fn(FriendStatus) -> bool| friends.iter().filter(|x| f(x.status)).count();
        let accepted = count(&|s| s.is_accepted());
        let online = count(&|s| s == FriendStatus::Online);
        // Only incoming requests are a call to action: the badge counts those first.
        let pending_in = count(&|s| s == FriendStatus::PendingIncoming);
        let pending = count(&|s| s.is_pending());
        let blocked = count(&|s| s == FriendStatus::Blocked);
        let filter = self.filter;
        let row_h = row_height(cx);
        let section = cx.theme().scaled.text_section;
        let (items, offsets, shown) = self.items(&friends, row_h, section, cx);
        crate::hooks::record("friends-rows", || {
            items.iter().filter_map(|i| if let Item::Row(f) = i { Some(f.uid.to_string()) } else { None }).collect()
        });

        let search = input_box(
            "friends-search",
            &self.search,
            BoxLook { height: Some(36.), pl: 34., icon: Some(IconName::HomeFriendSearch), ..BoxLook::new(cx.theme().color_bg_input, cx) },
            window,
            cx,
        );
        let add_field = self.add_open.then(|| {
            input_box(
                "add-friend-username",
                &self.add,
                BoxLook { height: Some(34.), ..BoxLook::new(cx.theme().color_bg_input, cx) },
                window,
                cx,
            )
        });

        let theme = cx.theme();
        let (text_title, text_member, text_micro, text_body) =
            (theme.scaled.text_title, theme.scaled.text_member, theme.scaled.text_micro, theme.scaled.text_body);
        let (weight, tracking_title, tracking_section) =
            (theme.palette.font_weight_emphasis, theme.palette.tracking_title, theme.palette.tracking_section);
        let (bg_mid, divider, bright, muted, secondary) = (
            theme.color_bg_mid,
            theme.color_border_divider,
            theme.color_text_bright,
            theme.color_text_muted,
            theme.color_text_secondary,
        );
        let (accent, accent_hover, on_accent, bg_light, err) =
            (theme.color_accent, theme.color_accent_hover, theme.color_on_accent, theme.color_bg_light, theme.color_error);
        let radius_md = theme.radius_md;

        let segments = div()
            .ml_auto()
            .flex()
            .items_center()
            .gap_0p5()
            .child(segment("filter-all", "All", 0, false, filter == Filter::All, cx).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::All, cx))))
            .child(
                segment("filter-online", "Online", online, false, filter == Filter::Online, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::Online, cx))),
            )
            .child(
                segment("filter-pending", "Pending", if pending_in > 0 { pending_in } else { pending }, pending_in > 0, filter == Filter::Pending, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::Pending, cx))),
            )
            .child(
                segment("filter-blocked", "Blocked", blocked, false, filter == Filter::Blocked, cx)
                    .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.set_filter(Filter::Blocked, cx))),
            );

        let list: AnyElement = if shown == 0 {
            let (title, hint) = if friends.is_empty() {
                ("No friends yet", "Add someone with the button above")
            } else {
                ("Nothing here", "No friends match this filter")
            };
            ScrollArea::new("friends-list")
                .flex_1()
                .child(
                    div()
                        .px_3()
                        .pb_5()
                        .pt_1()
                        .child(
                            div()
                                .flex()
                                .flex_col()
                                .items_center()
                                .gap_1p5()
                                .pt(px(56.))
                                .child(Label::new(title).font(FontRole::Channel).size(text_body).weight(500.).color(secondary))
                                .child(Label::new(hint).font(FontRole::Meta).size(text_micro).color(muted)),
                        ),
                )
                .into_any_element()
        } else {
            let count = items.len();
            ScrollArea::new("friends-list")
                .flex_1()
                .windowed(count, RowHeights::Offsets(offsets), move |range, _, cx| {
                    range
                        .map(|index| match &items[index] {
                            Item::Pad(_) | Item::Gap => div().into_any_element(),
                            Item::Header(label, n) => {
                                let t = cx.theme();
                                let size = t.scaled.text_section;
                                div()
                                    .size_full()
                                    .px_3()
                                    .pt_2p5()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .gap_2()
                                            .px_2()
                                            .child(
                                                Label::new(SharedString::from(label.to_uppercase()))
                                                    .font(FontRole::Section)
                                                    .size(size)
                                                    .line_height(size)
                                                    .weight(500.)
                                                    .tracking(tracking_section)
                                                    .color(t.color_text_muted),
                                            )
                                            .child(
                                                div().opacity(0.75).child(
                                                    Label::new(crate::fmt::count(*n))
                                                        .font(FontRole::Section)
                                                        .size(size)
                                                        .line_height(size)
                                                        .weight(500.)
                                                        .color(t.color_text_muted),
                                                ),
                                            ),
                                    )
                                    .into_any_element()
                            }
                            Item::Row(friend) => div().size_full().px_3().child(friend_row(friend, cx)).into_any_element(),
                        })
                        .collect()
                })
                .into_any_element()
        };

        div()
            .flex()
            .flex_col()
            .size_full()
            .bg(bg_mid)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h_12()
                    .items_center()
                    .gap_3()
                    .border_b_1()
                    .border_color(divider)
                    .px_5()
                    .child(Label::new("Friends").font(FontRole::Display).size(text_title).weight(weight).tracking(tracking_title).color(bright))
                    .child(Label::new(crate::fmt::count(accepted)).font(FontRole::Meta).size(text_micro).color(muted))
                    .child(segments),
            )
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_center()
                    .gap_2p5()
                    .border_b_1()
                    .border_color(divider)
                    .px_5()
                    .py_3()
                    .child(div().flex_1().min_w_0().child(search))
                    .child(
                        div()
                            .id("add-friend")
                            .relative()
                            .children(decibell_shell::probes::probe("friends-add"))
                            .flex()
                            .flex_none()
                            .h_9()
                            .items_center()
                            .gap_1p5()
                            .rounded(radius_md)
                            .bg(accent)
                            .px_4()
                            .cursor_pointer()
                            .hover(move |s| s.bg(accent_hover))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| this.toggle_add(window, cx)))
                            .child(Icon::new(IconName::SettingsPlus).size(px(16.)).stroke_width(1.8).color(on_accent))
                            .child(Label::new("Add Friend").font(FontRole::Channel).size(text_member).weight(600.).color(on_accent)),
                    ),
            )
            .when_some(add_field, |this, field| {
                let send = cx.listener(|this, _: &ClickEvent, _, cx| this.send_request(cx));
                this.child(
                    div()
                        .flex_none()
                        .border_b_1()
                        .border_color(divider)
                        .bg(bg_light)
                        .px_5()
                        .py_3p5()
                        .child(
                            div().mb_1p5().child(
                                Label::new("SEND A FRIEND REQUEST").font(FontRole::Meta).size(text_micro).tracking(0.1).color(muted),
                            ),
                        )
                        .child(
                            div().flex().gap_2().child(div().flex_1().min_w_0().child(field)).child(
                                div()
                                    .id("add-friend-send")
                                    .flex()
                                    .flex_none()
                                    .h(px(34.))
                                    .items_center()
                                    .rounded(radius_md)
                                    .bg(accent)
                                    .px_3p5()
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(accent_hover))
                                    .on_click(send)
                                    .child(Label::new("Send request").font(FontRole::Channel).size(text_member).weight(600.).color(on_accent)),
                            ),
                        )
                        .child(div().mt_1p5().child(Label::new("Usernames are case-sensitive.").font(FontRole::Meta).size(text_micro).color(muted)))
                        .when_some(self.add_error.clone(), |this, error| {
                            this.child(div().mt_1p5().child(Label::new(error).size(text_micro).color(err)))
                        }),
                )
            })
            .when_some(error, |this, error| {
                this.child(
                    div()
                        .mx_5()
                        .mt_2()
                        .flex_none()
                        .rounded(radius_md)
                        .bg(err.opacity(0.1))
                        .px_3()
                        .py_2()
                        .child(Label::new(error).size(text_member).color(err)),
                )
            })
            .child(list)
    }
}
