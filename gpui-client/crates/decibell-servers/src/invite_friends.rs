//! "Invite friends" (`features/servers/InviteFriendsModal.tsx` + `personalInvite.ts`,
//! `modal::INVITE_FRIENDS`): your accepted friends who aren't members yet, each a click away from
//! a **personal invite** (single use, never expires, only they can redeem it) sent to them as a
//! DM.
//!
//! "Not a member" comes from central, which holds both our friend list and every community's
//! members (`friends_in_community`), plus the roster we already have. **The friend list never goes
//! to the community** (owner rule): it isn't asked to filter it, nor are friends looked up there
//! one by one. When central can't say, the loaded roster alone decides, and inviting someone it
//! missed comes back `already_member`, which the row shows.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::time::Duration;

use decibell_common::{AvatarDot, UserAvatar, Watch, presence_of};
use decibell_core::api;
use decibell_core::events::CoreEvent;
use decibell_model::error::UiError;
use decibell_model::invite_link::build_invite_link;
use decibell_model::stores::overlays::modal;
use decibell_model::{Model, ServerId, Uid};
use decibell_text::input::{InputEvent, InputState};
use decibell_ui::{ActiveTheme as _, CloseRequest, FontRole, Icon, IconName, Label, Modal, RowHeights, ScrollArea, ScrollHandle};
use gpui::{App, AsyncApp, ClickEvent, Context, Entity, IntoElement, Render, SharedString, Task, WeakEntity, Window, div, prelude::*, px};

use crate::follow::Follow;
use crate::ui::{self, HEADING_TRACKING, InputLook};

/// A row's outcome (`RowState`). `Member`: the server refused because they're already in.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowState {
    Sending,
    Sent,
    Member,
}

/// Central's answer for the community: friends who are members. `None` while asking.
#[derive(Clone, Debug, PartialEq)]
pub enum CentralMembers {
    Known(HashSet<Uid>),
    /// It can't say: the roster alone decides.
    Unknown,
}

pub struct InviteFriendsOverlay {
    model: Model,
    open: bool,
    server: Option<ServerId>,
    search: Entity<InputState>,
    rows: HashMap<Uid, RowState>,
    central: Option<CentralMembers>,
    scroll: ScrollHandle,
    follow: Follow,
    focus_search: bool,
    _ask: Option<Task<()>>,
    _subscriptions: Vec<gpui::Subscription>,
    _watch: Watch,
}

impl InviteFriendsOverlay {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let search = cx.new(|cx| InputState::single_line(window, cx).placeholder("Search friends"));
        cx.subscribe(&search, |this, _, event: &InputEvent, cx| match event {
            InputEvent::Change => cx.notify(),
            // `useEscapeToClose` listens on the document: the field's Escape closes too.
            InputEvent::Escape => this.close(cx),
            _ => {}
        })
        .detach();
        let mut watch = Watch::new();
        let mut subscriptions: Vec<gpui::Subscription> = Vec::new();
        subscriptions.push(cx.observe(&model.overlays, |this: &mut Self, _, cx| {
            if this.sync(cx) {
                cx.notify();
            }
        }));
        subscriptions.push(cx.observe(&model.nav, |this: &mut Self, _, cx| {
            if this.sync(cx) {
                cx.notify();
            }
        }));
        watch
            .store_if(&model.live.friends, cx, |this: &mut Self, _, _| this.open)
            .store_if(&model.servers, cx, |this: &mut Self, _, _| this.open)
            .people(cx, |this: &Self, _| this.open);
        let mut this = Self {
            model,
            open: false,
            server: None,
            search,
            rows: HashMap::new(),
            central: None,
            scroll: ScrollHandle::new(),
            follow: Follow::default(),
            focus_search: false,
            _ask: None,
            _subscriptions: subscriptions,
            _watch: watch,
        };
        this.sync(cx);
        cx.set_global(crate::Current::<InviteFriendsOverlay>(cx.weak_entity()));
        this
    }

    /// Opening (or a server switch while open) starts over: the search, the outcomes, central's
    /// answer (`useEffect([open, serverId])`).
    fn sync(&mut self, cx: &mut Context<Self>) -> bool {
        let open = self.model.overlays.read(cx).is_open(modal::INVITE_FRIENDS);
        let server = self.model.nav.read(cx).active_server;
        if (open, server) == (self.open, self.server) {
            return false;
        }
        self.open = open;
        self.server = server;
        self.follow.set(open.then_some(server).flatten(), cx);
        if !open {
            self._ask = None;
            return true;
        }
        self.search.update(cx, |s, cx| s.clear(cx));
        self.rows.clear();
        self.scroll = ScrollHandle::new();
        self.central = None;
        if let Some((central, rows)) = crate::screen_state(cx).and_then(|s| s.friends.clone()) {
            self.central = Some(central);
            self.rows = rows.into_iter().collect();
            return true;
        }
        if let Some(server) = server {
            let args = api::friends::FriendsInCommunityArgs { server_id: server.wire() };
            self._ask = Some(cx.spawn(async move |this, cx| {
                let result = if decibell_model::core::is_running() {
                    api::friends::friends_in_community(args).await.ok()
                } else {
                    None
                };
                let central = match result {
                    Some(r) if r.known => CentralMembers::Known(r.member_uids.into_iter().collect()),
                    _ => CentralMembers::Unknown,
                };
                let _ = this.update(cx, |this, cx| {
                    if this.server == Some(server) {
                        this.central = Some(central);
                        cx.notify();
                    }
                });
            }));
            // `autoFocus` (on the next render, which has the window).
            self.focus_search = true;
        }
        true
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        self.model.overlays.update(cx, |o, cx| o.close_modal(cx));
    }

    /// Accepted friends (not pending, not blocked), by name.
    fn candidates(&self, cx: &App) -> Vec<(Uid, SharedString)> {
        let mut list: Vec<(Uid, SharedString)> = self
            .model
            .live
            .friends
            .read(cx)
            .list()
            .iter()
            .filter(|f| f.status.is_accepted())
            .map(|f| (f.uid, self.model.name_of(f.uid, &f.username, cx)))
            .collect();
        list.sort_by(|a, b| ui::locale_compare(&a.1, &b.1));
        list
    }

    /// Central's answer plus the loaded roster, never a lookup on the community; a row with an
    /// outcome stays (a friend who joins after their invite keeps showing "Sent").
    fn not_members(&self, candidates: &[(Uid, SharedString)], cx: &App) -> Vec<(Uid, SharedString)> {
        let mut members: HashSet<Uid> = self
            .server
            .and_then(|s| self.model.community(s, cx))
            .map(|c| c.roster.read(cx).members().keys().copied().collect())
            .unwrap_or_default();
        if let Some(CentralMembers::Known(known)) = &self.central {
            members.extend(known.iter().copied());
        }
        candidates.iter().filter(|(uid, _)| self.rows.contains_key(uid) || !members.contains(uid)).cloned().collect()
    }

    /// `handleInvite`.
    fn invite(&mut self, uid: Uid, username: SharedString, cx: &mut Context<Self>) {
        let Some(server) = self.server else { return };
        self.rows.insert(uid, RowState::Sending);
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = invite_friend(server, uid, username.clone(), cx).await;
            let _ = this.update(cx, |this, cx| {
                match result {
                    Ok(()) => {
                        this.rows.insert(uid, RowState::Sent);
                    }
                    Err(InviteError::AlreadyMember) => {
                        this.rows.insert(uid, RowState::Member);
                    }
                    Err(InviteError::Failed(message)) => {
                        this.rows.remove(&uid);
                        let model = this.model.clone();
                        model.toasts.update(cx, |t, cx| t.error(format!("Couldn't invite {username}"), Some(message), cx));
                    }
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// For the screens: central's answer and a row's outcome as they would be.
    pub fn set_fixture(&mut self, central: CentralMembers, rows: &[(Uid, RowState)], cx: &mut Context<Self>) {
        self._ask = None;
        self.central = Some(central);
        self.rows = rows.iter().copied().collect();
        cx.notify();
    }

    /// What the list shows now (the self-tests read it): `(uid, name, outcome)`.
    pub fn shown(&self, cx: &App) -> Vec<(Uid, SharedString, Option<RowState>)> {
        let candidates = self.candidates(cx);
        let query = self.search.read(cx).value().trim().to_lowercase();
        self.not_members(&candidates, cx)
            .into_iter()
            .filter(|(_, name)| query.is_empty() || name.to_lowercase().contains(&query))
            .map(|(uid, name)| (uid, name, self.rows.get(&uid).copied()))
            .collect()
    }

    pub fn central_answered(&self) -> bool {
        self.central.is_some()
    }
}

impl Render for InviteFriendsOverlay {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let open = self.open && self.server.is_some();
        if std::mem::take(&mut self.focus_search) && open {
            let search = self.search.clone();
            window.defer(cx, move |window, cx| search.update(cx, |s, cx| s.focus(window, cx)));
        }
        let card = open.then(|| self.card(window, cx));
        Modal::new("invite-friends", open)
            .on_close(cx.listener(|this, _: &CloseRequest, _, cx| this.close(cx)))
            .children(card)
    }
}

impl InviteFriendsOverlay {
    fn card(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement + use<> {
        let server = self.server.expect("open with a server");
        let server_name = self.model.servers.read(cx).get(server).map(|s| s.name.clone());
        let candidates = self.candidates(cx);
        let not_members = self.not_members(&candidates, cx);
        let query = self.search.read(cx).value().trim().to_lowercase();
        let shown: Vec<(Uid, SharedString)> =
            not_members.iter().filter(|(_, name)| query.is_empty() || name.to_lowercase().contains(&query)).cloned().collect();
        let empty: Option<&'static str> = if candidates.is_empty() {
            Some("Add some friends first — they'll show up here.")
        } else if self.central.is_none() {
            Some("Loading…")
        } else if not_members.is_empty() {
            Some("All your friends are already here.")
        } else if shown.is_empty() {
            Some("No friends match that search.")
        } else {
            None
        };
        let t = cx.theme();
        let search = ui::input_box("invite-friends-search", &self.search, InputLook::new(t.color_bg_light, t.radius_md).ring(), window, cx);
        let t = cx.theme();
        let title: SharedString = format!("Invite friends to {}", server_name.as_deref().unwrap_or("this server")).into();
        let list = match empty {
            Some(text) => div()
                .h(px(288.))
                .flex()
                .items_center()
                .justify_center()
                .px_4()
                .py_3()
                .child(Label::new(text).size(13.).line_height(19.5).color(t.color_text_muted))
                .into_any_element(),
            None => {
                let rows: Rc<Vec<(Uid, SharedString, Option<RowState>)>> =
                    Rc::new(shown.iter().map(|(uid, name)| (*uid, name.clone(), self.rows.get(uid).copied())).collect());
                let view = cx.entity().downgrade();
                let count = rows.len();
                // `py-3` around rows of `py-1.5` and a 32 px avatar: 44 px each.
                let mut offsets: Vec<gpui::Pixels> = (0..=count).map(|i| px(12. + 44. * i as f32)).collect();
                offsets[0] = px(0.);
                if let Some(last) = offsets.last_mut() {
                    *last = px(12. + 44. * count as f32 + 12.);
                }
                ScrollArea::new("invite-friends-list")
                    .track_scroll(&self.scroll)
                    .h(px(288.))
                    .flex_none()
                    .windowed(count, RowHeights::Offsets(offsets.into()), move |range, _, cx| {
                        range.map(|i| friend_row(&rows[i], i == 0, view.clone(), cx)).collect()
                    })
                    .into_any_element()
            }
        };
        div()
            .id("invite-friends-card")
            .occlude()
            .flex()
            .flex_col()
            .w_full()
            .max_w(px(440.))
            .overflow_hidden()
            .rounded(t.radius_xl)
            .border_1()
            .border_color(t.color_border)
            .bg(t.color_bg_dark)
            .shadow(t.shadow_modal.clone())
            .child(
                div()
                    .flex()
                    .flex_none()
                    .items_start()
                    .justify_between()
                    .gap_3()
                    .border_b_1()
                    .border_color(t.color_border_divider)
                    .px_6()
                    .py_5()
                    .child(
                        div()
                            .min_w_0()
                            .child(Label::new(title).font(FontRole::Display).size(18.).line_height(27.).weight(600.).tracking(HEADING_TRACKING).color(t.color_text_primary).truncate())
                            .child(
                                div().mt_0p5().child(
                                    Label::new("Each friend gets their own invite in a DM. It works once, only for them, and never expires.")
                                        .size(12.)
                                        .line_height(18.)
                                        .color(t.color_text_muted),
                                ),
                            ),
                    )
                    .child(ui::close_button("invite-friends-close", cx.listener(|this, _: &ClickEvent, _, cx| this.close(cx)), cx)),
            )
            .child(div().flex_none().px_6().pt_4().child(search))
            .child(list)
    }
}

/// `FriendRow`.
fn friend_row(row: &(Uid, SharedString, Option<RowState>), first: bool, view: WeakEntity<InviteFriendsOverlay>, cx: &mut App) -> gpui::AnyElement {
    let (uid, name, state) = row.clone();
    let t = cx.theme();
    let (accent, accent_bright, on_accent, muted) = (t.color_accent, t.color_accent_bright, t.color_on_accent, t.color_text_muted);
    let status = presence_of(uid, false, cx);
    let t = cx.theme();
    let action = match state {
        Some(RowState::Member) => div().flex_none().px_3().py_1p5().child(Label::new("Already here").size(12.).line_height(18.).weight(500.).color(muted)).into_any_element(),
        Some(RowState::Sent) => div()
            .flex()
            .flex_none()
            .items_center()
            .gap_1p5()
            .px_3()
            .py_1p5()
            .child(Icon::new(IconName::ServersCheck).size(px(12.)).color(muted))
            .child(Label::new("Sent").size(12.).line_height(18.).weight(500.).color(muted))
            .into_any_element(),
        _ => {
            let sending = state == Some(RowState::Sending);
            let group: SharedString = format!("invite-friend-{uid}").into();
            div()
                .id("invite-friend")
                .group(group.clone())
                .relative()
                .children(decibell_shell::probes::probe(format!("invite-friend:{uid}")))
                .flex_none()
                .rounded(t.radius_sm)
                .border_1()
                .border_color(accent)
                .px_3()
                .py_1p5()
                .when(!sending, |this| this.cursor_pointer().hover(move |s| s.bg(accent)))
                .when(sending, |this| this.opacity(0.5))
                .on_click(move |_: &ClickEvent, _, cx| {
                    if !sending {
                        let name = name.clone();
                        let _ = view.update(cx, |this, cx| this.invite(uid, name, cx));
                    }
                })
                .child(
                    div()
                        .text_color(accent_bright)
                        .when(!sending, |this| this.group_hover(group, move |s| s.text_color(on_accent)))
                        .child(Label::new(if sending { "Inviting…" } else { "Invite" }).size(12.).line_height(18.).weight(600.)),
                )
                .into_any_element()
        }
    };
    let (uid2, name2) = (row.0, row.1.clone());
    div()
        .id(("friend-row", uid as u64))
        .when(first, |this| this.mt_3())
        .mx_4()
        .flex()
        .h(px(44.))
        .items_center()
        .gap_3()
        .rounded(t.radius_md)
        .px_2()
        .py_1p5()
        .child(UserAvatar::new(uid2, name2.clone(), 32.).dot(AvatarDot::new(status, t.color_bg_dark, 12.)))
        .child(div().flex_1().min_w_0().child(Label::new(name2).font(FontRole::Channel).size(14.).line_height(21.).color(t.color_text_primary).truncate()))
        .child(action)
        .into_any_element()
}

/// Why a personal invite wasn't sent.
#[derive(Debug, Clone)]
pub enum InviteError {
    /// The community refused it: they're already a member.
    AlreadyMember,
    Failed(SharedString),
}

// The community answers `INVITE_CREATE_REQ` asynchronously; a request id (echoed on the answer)
// ties each answer to its caller, so several invites can be out at once and a request that
// never answers can't take the next one's reply.
thread_local! {
    static NEXT_REQUEST_ID: Cell<u32> = const { Cell::new(1) };
}

const NO_ANSWER: Duration = Duration::from_millis(8000);

/// `createPersonalInvite`: single use, never expires, only `uid` can redeem it. The server hands
/// back their live one when it exists, so inviting the same friend twice doesn't pile invites up.
pub async fn create_personal_invite(server: ServerId, uid: Uid, username: SharedString, cx: &mut AsyncApp) -> Result<SharedString, InviteError> {
    let request_id = NEXT_REQUEST_ID.with(|n| {
        let id = n.get();
        n.set(id.wrapping_add(1).max(1));
        id
    });
    let (tx, rx) = futures::channel::oneshot::channel::<Result<SharedString, InviteError>>();
    let tx = Rc::new(std::cell::RefCell::new(Some(tx)));
    let wire = server.wire();
    // The answer, from the model's answers bus.
    let subscription = cx.update(|cx| {
        let model = Model::global(cx).clone();
        let tx = tx.clone();
        cx.subscribe(&model.answers, move |_, event: &CoreEvent, _| {
            let CoreEvent::InviteCreateResponded(p) = event else { return };
            if p.server_id != wire || p.request_id != request_id {
                return;
            }
            let result = match (&p.invite, p.success) {
                (Some(invite), true) => Ok(SharedString::from(invite.code.clone())),
                _ if p.already_member => Err(InviteError::AlreadyMember),
                _ => Err(InviteError::Failed(if p.message.is_empty() { "Couldn't create the invite.".into() } else { p.message.clone().into() })),
            };
            if let Some(tx) = tx.borrow_mut().take() {
                let _ = tx.send(result);
            }
        })
    });
    let args = api::community::CreateInviteArgs {
        server_id: server.wire(),
        expires_at: 0,
        max_uses: 1,
        for_user: Some(username.to_string()),
        for_uid: Some(uid),
        request_id: Some(request_id),
    };
    let sent = if decibell_model::core::is_running() { api::community::create_invite(args).await.map_err(UiError::from) } else { Err(UiError::not_running()) };
    if let Err(e) = sent {
        drop(subscription);
        return Err(InviteError::Failed(e.plain()));
    }
    let timer = cx.background_executor().timer(NO_ANSWER);
    let answer = futures::future::select(rx, Box::pin(timer)).await;
    drop(subscription);
    match answer {
        futures::future::Either::Left((Ok(result), _)) => result,
        _ => Err(InviteError::Failed("The server didn't answer.".into())),
    }
}

/// `inviteFriend`: their personal invite, sent to them as a DM (the link renders as an invite card
/// on their side). The DM goes the way the renderer sends it (`send_private_message`): no bubble
/// in our conversation until central's echo, and a draft typed to them stays.
pub async fn invite_friend(server: ServerId, uid: Uid, username: SharedString, cx: &mut AsyncApp) -> Result<(), InviteError> {
    let code = create_personal_invite(server, uid, username.clone(), cx).await?;
    let args = api::messaging::SendPrivateMessageArgs {
        recipient: username.to_string(),
        recipient_uid: Some(uid),
        message: build_invite_link(&code),
        reply_to: None,
        nonce: None,
    };
    let sent = if decibell_model::core::is_running() { api::messaging::send_private_message(args).await.map_err(UiError::from) } else { Err(UiError::not_running()) };
    sent.map_err(|e| InviteError::Failed(e.plain()))
}
