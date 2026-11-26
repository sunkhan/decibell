//! The Friends rail beside an open conversation (`FriendsList.tsx`): 260 px, its own Add Friend
//! and search, the sections ONLINE / OFFLINE / PENDING / BLOCKED with counts, compact rows that
//! open the profile popup (a right click: the user menu), Accept / Reject on incoming requests.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use decibell_common::{AvatarDot, BoundsCell, StatusMessageText, UserAvatar, Watch, open_profile_beside, open_user_menu, presence_of};
use decibell_model::live::friends::{Friend, FriendAction, FriendStatus};
use decibell_model::{Model, Uid, actions};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, FontRole, Label, ScrollArea};
use gpui::{
    AnyElement, ClickEvent, Context, Entity, IntoElement, MouseButton, MouseDownEvent, Render, SharedString,
    Subscription, Task, Window, div, prelude::*,
};

use crate::ui::{BoxLook, input_box};

pub const RAIL_WIDTH: f32 = 260.;

const ADD_ERROR_LIFETIME: Duration = Duration::from_secs(5);

pub struct FriendsRail {
    search: Entity<InputState>,
    add_open: bool,
    add: Entity<InputState>,
    add_error: Option<SharedString>,
    add_error_timer: Option<Task<()>>,
    /// `FriendActionButton`'s own state per row: a request on its way, the error it got.
    busy: HashSet<Uid>,
    row_errors: HashMap<Uid, SharedString>,
    shown: HashSet<Uid>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl FriendsRail {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let search = cx.new(|cx| InputState::single_line(window, cx).placeholder("Search friends..."));
        let add = cx.new(|cx| InputState::single_line(window, cx).placeholder("Username"));
        let mut watch = Watch::new();
        watch
            .store_if(&model.live.friends, cx, |this: &mut FriendsRail, friends, _| {
                this.shown = friends.list().iter().map(|f| f.uid).collect();
                true
            })
            .people(cx, |this, uid| this.shown.contains(&uid));
        let subscriptions = vec![
            cx.subscribe(&search, |_, _, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change | InputEvent::Focus | InputEvent::Blur) {
                    cx.notify();
                }
            }),
            cx.subscribe(&add, |this, _, event: &InputEvent, cx| match event {
                InputEvent::Submit => this.send_request(cx),
                InputEvent::Focus | InputEvent::Blur => cx.notify(),
                _ => {}
            }),
        ];
        let shown = model.live.friends.read(cx).list().iter().map(|f| f.uid).collect();
        Self {
            search,
            add_open: false,
            add,
            add_error: None,
            add_error_timer: None,
            busy: HashSet::new(),
            row_errors: HashMap::new(),
            shown,
            _watch: watch,
            _subscriptions: subscriptions,
        }
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

    /// `FriendActionButton`'s click: "..." while the request leaves, its error under it.
    fn act(&mut self, action: FriendAction, uid: Uid, name: SharedString, cx: &mut Context<Self>) {
        self.busy.insert(uid);
        self.row_errors.remove(&uid);
        cx.notify();
        let this = cx.weak_entity();
        let request = decibell_core::api::friends::send_friend_action(decibell_core::api::friends::SendFriendActionArgs {
            action: action as i32,
            target_username: name.to_string(),
            target_uid: Some(uid),
        });
        decibell_model::call::call(cx, request, move |result, cx| {
            let _ = this.update(cx, |this, cx| {
                this.busy.remove(&uid);
                if let Err(e) = result {
                    this.row_errors.insert(uid, e.js_string());
                }
                cx.notify();
            });
        });
    }

    fn row(&self, friend: &Friend, cx: &mut Context<Self>) -> AnyElement {
        let uid = friend.uid;
        let name = decibell_common::user_name(uid, &friend.username, cx);
        let status = presence_of(uid, false, cx);
        let accepted = friend.status.is_accepted();
        let message = accepted.then(|| decibell_common::status_message_of(uid, cx)).flatten();
        let busy = self.busy.contains(&uid);
        let error = self.row_errors.get(&uid).cloned();
        let theme = cx.theme();
        let (bg_secondary, hover, secondary, primary, muted) =
            (theme.color_bg_secondary, theme.color_surface_hover, theme.color_text_secondary, theme.color_text_primary, theme.color_text_muted);
        let (success, err, radius_sm, micro) = (theme.color_success, theme.color_error, theme.radius_sm, theme.scaled.text_micro);
        let group: SharedString = format!("rail-friend-{uid}").into();
        let bounds = BoundsCell::new();
        // `FriendActionButton` (`rounded-sm px-2.5 py-1 text-xs font-semibold text-white`).
        let action_button = |id: &'static str, label: &'static str, bg: gpui::Hsla, action: FriendAction, cx: &mut Context<Self>| {
            let name = name.clone();
            div()
                .id(id)
                .flex()
                .rounded(radius_sm)
                .px_2p5()
                .py_1()
                .bg(bg)
                .when(busy, |this| this.opacity(0.5))
                .when(!busy, |this| this.cursor_pointer().hover(move |s| s.bg(bg.opacity(0.8))))
                .on_click(cx.listener(move |this, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    if !this.busy.contains(&uid) {
                        this.act(action, uid, name.clone(), cx);
                    }
                }))
                // `text-white`: a literal of the renderer.
                .child(Label::new(if busy { "..." } else { label }).size(12.).weight(600.).color(gpui::white()))
        };
        let trailing: Option<AnyElement> = match friend.status {
            FriendStatus::PendingIncoming => Some(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .gap_1()
                            .child(action_button("accept", "Accept", success, FriendAction::Accept, cx))
                            .child(action_button("reject", "Reject", err, FriendAction::Reject, cx)),
                    )
                    .when_some(error, |this, error| this.child(div().mt_0p5().child(Label::new(error).size(10.).color(err))))
                    .into_any_element(),
            ),
            FriendStatus::PendingOutgoing => Some(Label::new("Pending").size(11.).color(muted).into_any_element()),
            _ => None,
        };
        let mut avatar = UserAvatar::new(uid, name.clone(), 32.);
        if accepted {
            avatar = avatar.dot(AvatarDot::new(status, bg_secondary, 12.));
        }
        div()
            .id(("rail-friend", uid as u64))
            .group(group.clone())
            .relative()
            .flex()
            .items_center()
            .gap_2p5()
            .rounded(radius_sm)
            .px_2()
            .py_1p5()
            .cursor_pointer()
            .hover(move |s| s.bg(hover))
            .child(bounds.probe())
            .on_click(move |_: &ClickEvent, _, cx| open_profile_beside(uid, bounds.get(), None, cx))
            .on_mouse_down(MouseButton::Right, move |e: &MouseDownEvent, _, cx| open_user_menu(uid, e.position, None, cx))
            .child(avatar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_color(secondary)
                    .group_hover(group, move |s| s.text_color(primary))
                    .child(Label::new(name).font(FontRole::Channel).size(14.).line_height(18.).truncate())
                    .when_some(message, |this, message| {
                        this.child(StatusMessageText::new(message).size(micro).line_height(14.).color(muted))
                    }),
            )
            .children(trailing)
            .into_any_element()
    }
}

impl Render for FriendsRail {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let friends: Vec<Friend> = model.live.friends.read(cx).list().to_vec();
        let error = model.live.friends.read(cx).last_action_error.clone();
        let needle = self.search.read(cx).value().to_lowercase();
        let filtered: Vec<&Friend> =
            friends.iter().filter(|f| decibell_common::user_name(f.uid, &f.username, cx).to_lowercase().contains(&needle)).collect();
        let sections: [(&str, Vec<&Friend>); 4] = [
            ("ONLINE", filtered.iter().copied().filter(|f| f.status == FriendStatus::Online).collect()),
            ("OFFLINE", filtered.iter().copied().filter(|f| f.status == FriendStatus::Offline).collect()),
            ("PENDING", filtered.iter().copied().filter(|f| f.status.is_pending()).collect()),
            ("BLOCKED", filtered.iter().copied().filter(|f| f.status == FriendStatus::Blocked).collect()),
        ];
        let add_field = self.add_open.then(|| {
            let t = cx.theme();
            input_box(
                "rail-add-username",
                &self.add,
                BoxLook { py: 6., pl: 10., pr: 10., size: 12., font: FontRole::Sans, ring: false, ..BoxLook::new(t.color_bg_primary, cx) },
                window,
                cx,
            )
        });
        let search = {
            let t = cx.theme();
            input_box("rail-search", &self.search, BoxLook { py: 8., size: 12., font: FontRole::Sans, ..BoxLook::new(t.color_bg_light, cx) }, window, cx)
        };
        let mut body: Vec<AnyElement> = Vec::new();
        for (label, items) in &sections {
            if items.is_empty() {
                continue;
            }
            let rows: Vec<AnyElement> = items.iter().map(|f| self.row(f, cx)).collect();
            let muted = cx.theme().color_text_muted;
            body.push(
                div()
                    .mb_3()
                    .child(
                        div().mb_1().px_2().child(
                            Label::new(SharedString::from(format!("{label} — {}", crate::fmt::count(items.len()))))
                                .font(FontRole::Section)
                                .size(11.)
                                .weight(600.)
                                .tracking(0.07)
                                .color(muted),
                        ),
                    )
                    .children(rows)
                    .into_any_element(),
            );
        }
        let theme = cx.theme();
        let muted = theme.color_text_muted;
        if friends.is_empty() {
            body.push(div().mt_4().flex().justify_center().child(Label::new("No friends yet. Add someone!").size(12.).color(muted)).into_any_element());
        } else if filtered.is_empty() {
            body.push(div().mt_4().flex().justify_center().child(Label::new("No results found").size(12.).color(muted)).into_any_element());
        }
        let (border, bg_dark, bright, accent_bright, accent_hover) =
            (theme.color_border, theme.color_bg_dark, theme.color_text_bright, theme.color_accent_bright, theme.color_accent_hover);
        let (accent, on_accent, err, radius_sm, radius_md) =
            (theme.color_accent, theme.color_on_accent, theme.color_error, theme.radius_sm, theme.radius_md);
        let (text_title, weight, tracking) = (theme.scaled.text_title, theme.palette.font_weight_emphasis, theme.palette.tracking_title);
        div()
            .size_full()
            .flex()
            .flex_col()
            .border_l_1()
            .border_color(border)
            .bg(bg_dark)
            .child(
                div()
                    .flex()
                    .flex_none()
                    .h_12()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(border)
                    .px_4()
                    .child(Label::new("Friends").font(FontRole::Display).size(text_title).weight(weight).tracking(tracking).color(bright))
                    .child(
                        div()
                            .id("rail-add-friend")
                            .rounded(radius_sm)
                            .px_2()
                            .py_1()
                            .cursor_pointer()
                            .hover(move |s| s.bg(accent_hover.opacity(0.15)))
                            .on_click(cx.listener(|this, _: &ClickEvent, window, cx| {
                                this.add_open = !this.add_open;
                                if this.add_open {
                                    this.add.update(cx, |s, cx| s.focus(window, cx));
                                }
                                cx.notify();
                            }))
                            .child(Label::new("Add Friend").font(FontRole::Channel).size(12.).weight(500.).color(accent_bright)),
                    ),
            )
            .when_some(add_field, |this, field| {
                this.child(
                    div()
                        .flex()
                        .flex_none()
                        .gap_1p5()
                        .border_b_1()
                        .border_color(border)
                        .px_3()
                        .py_2p5()
                        .child(div().flex_1().min_w_0().child(field))
                        .child(
                            div()
                                .id("rail-add-send")
                                .flex_none()
                                .rounded(radius_md)
                                .bg(accent)
                                .px_2p5()
                                .py_1p5()
                                .cursor_pointer()
                                .hover(move |s| s.bg(accent_hover))
                                .on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.send_request(cx)))
                                .child(Label::new("Send").size(12.).weight(600.).color(on_accent)),
                        ),
                )
                .when_some(self.add_error.clone(), |this, error| this.child(div().px_3().pb_1().child(Label::new(error).size(12.).color(err))))
            })
            .child(div().flex_none().px_3().py_2().child(search))
            .when_some(error, |this, error| {
                this.child(div().mx_3().mb_1().flex_none().rounded(radius_md).bg(err.opacity(0.1)).px_2p5().py_1p5().child(Label::new(error).size(12.).color(err)))
            })
            .child(ScrollArea::new("rail-friends").flex_1().child(div().px_2().pb_2().children(body)))
    }
}
