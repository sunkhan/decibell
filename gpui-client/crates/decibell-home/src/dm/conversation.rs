//! One conversation (`DmChatPanel.tsx`): the header, the encryption notice, the call stage, the
//! message list on the peer's slice (or what stands in for it: loading, retry, the beginning of
//! the conversation), the composer (with its inline send error), the delete confirmation.
//!
//! It observes exactly what it draws: its slice's flags (the body), the conversation's sidebar
//! facts and the send error (`dm.list`, compared), the re-sync event, the E2EE store and the
//! peer's encryption answer, the call store, the peer's name / status / message, the rail toggle.
//!
//! **Mark read** (owner, 2026-11-26; the renderer differs: CONVENTIONS "Home",
//! `docs/electron-quirks.md` #16). The conversation is **being read** while all three hold: it is
//! the open view (this view is mounted, its list drawn: not loading, not hidden by the call's
//! theater), the window is focused (`decibell_harness::focus`: active and not minimised) and the
//! list rests at its live bottom (`ListEvent::AtBottom`). Only then is it read up to its newest
//! loaded row (`actions::dm::mark_read`: the count clears at once, central hears 250 ms later and
//! tells the account's other devices). A DM arriving otherwise stays unread (the row's badge, the
//! server bar's tile, the other devices) until the three hold again — the window regains the
//! focus, the list is scrolled (or jumped) to the bottom, the conversation is opened — and is
//! marked then. The model knows which conversation is being read (`DmList::set_reading`), so a DM
//! arriving for it is never counted. Events only: the window's activation and visibility, the
//! list's at-bottom changes, the slice, the call store.

use std::time::Duration;

use decibell_chat::{ComposerParts, ListEvent, MessageList};
use decibell_common::{UserAvatar, Watch, person_name_color, presence_of, status_message_of};
use decibell_model::chat::MessageSlice;
use decibell_model::dm::DmEvent;
use decibell_model::live::calls::CallStatus;
use decibell_model::stores::layout::LayoutEvent;
use decibell_model::{Model, Uid, actions};
use decibell_shell::Slot;
use decibell_ui::{ActiveTheme as _, ConfirmModal, ConfirmTone, FontRole, Label, motion};
use gpui::{
    Animation, AnimationExt as _, AnyView, App, AppContext as _, ClickEvent, Context, Entity, IntoElement, Render,
    SharedString, StyleRefinement, Subscription, Task, Window, div, prelude::*, px,
};

use super::header::{HeaderFacts, header};
use super::source::DmSource;
use super::{banner, dm_composer};
use crate::e2ee::{PeerInfoChanged, PeerInfos, current_key};
use crate::ui::{Look, button};

/// What stands in the list's place.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Body {
    /// The newest page isn't in yet (shown only for a slow load).
    Loading,
    /// It got no answer: Retry.
    Failed,
    /// Nobody wrote yet.
    Beginning,
    List,
}

pub struct Conversation {
    peer: Uid,
    slice: Entity<MessageSlice>,
    list: Entity<MessageList>,
    /// The composer crate's, when the app registered it (`set_dm_composer`).
    composer: Option<ComposerParts>,
    stage: Option<AnyView>,
    failed: bool,
    load: Option<Task<()>>,
    confirm_delete: Option<i64>,
    body: Body,
    /// What the mark-read effect last saw: (rows, unread).
    seen: (usize, i64),
    /// The window is focused (`decibell_harness::focus::window_focused`).
    focused: bool,
    /// The list said it rests at its live bottom (`ListEvent::AtBottom`), since it was last shown.
    list_bottom: bool,
    /// The list is drawn (the body is the list, the theater doesn't hide it).
    list_shown: bool,
    /// Being read: the three above (and `DmList::reading` says so).
    reading: bool,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl Conversation {
    pub fn new(peer: Uid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let source = std::rc::Rc::new(DmSource { peer });
        let slice = decibell_chat::ListSource::slice(&*source, cx);
        // `className="pr-4"`: the rows keep 16 px from the scrollbar.
        let list = cx.new(|cx| MessageList::new(source, cx).with_right_padding(16.));
        // Re-sync handover: `markHistoriesStale` leaves the conversation on screen to us.
        model.dm.list.update(cx, |l, _| l.set_displayed(Some(peer)));
        let composer = dm_composer(cx).map(|build| build(peer, list.clone(), window, cx));
        let mut watch = Watch::new();
        watch
            .store(&model.live.calls, cx)
            .event(&model.layout, cx, |_, event: &LayoutEvent| *event == LayoutEvent::Panels)
            .people(cx, move |_, uid| uid == peer);
        let peer_infos = PeerInfos::entity(cx);
        let subscriptions = vec![
            cx.observe(&slice, |this, _, cx| this.slice_changed(cx)),
            // The conversation's unread count: central's count for it (a sign-in's previews) is
            // read again while it is being read (the renderer's effect on `unreadCount`).
            cx.observe(&model.dm.list, move |this, list, cx| {
                let unread = list.read(cx).get(peer).map_or(0, |c| c.unread);
                if unread != this.seen.1 {
                    this.seen.1 = unread;
                    if this.reading {
                        cx.defer(move |cx| actions::dm::mark_read(peer, cx));
                    }
                }
            }),
            cx.observe(&model.live.e2ee, move |_, _, cx| {
                PeerInfos::want(peer, cx);
                cx.notify();
            }),
            cx.subscribe(&peer_infos, move |_, _, event: &PeerInfoChanged, cx| {
                if event.0 == peer {
                    cx.notify();
                }
            }),
            cx.subscribe(&list, |this, list, event: &ListEvent, cx| this.list_event(list, event, cx)),
            // The theater hides the list (and shows it again).
            cx.observe(&model.live.calls, |this, _, cx| this.update_reading(cx)),
            // The re-sync after central signed us in again (`DmEvent::Resync`).
            cx.subscribe(&model.dm.list, move |this, _, event: &DmEvent, cx| {
                if let DmEvent::Resync = event {
                    let at_bottom = this.list.read(cx).at_bottom();
                    actions::dm::resync_displayed(peer, at_bottom, cx);
                }
            }),
        ];
        cx.on_release(move |_, cx| {
            let model = Model::global(cx).clone();
            model.dm.list.update(cx, |l, _| {
                if l.displayed() == Some(peer) {
                    l.set_displayed(None);
                }
                l.set_reading(peer, false);
            });
        })
        .detach();
        let mut subscriptions = subscriptions;
        subscriptions.extend(decibell_harness::focus::observe_window_focus(window, cx, |this: &mut Self, window, cx| {
            let focused = decibell_harness::focus::window_focused(window, cx);
            if focused != this.focused {
                this.focused = focused;
                this.update_reading(cx);
            }
        }));
        let focused = decibell_harness::focus::window_focused(window, cx);
        let entity = cx.entity();
        crate::hooks::remember(&entity, cx);
        let mut this = Self {
            peer,
            slice,
            list,
            composer,
            stage: None,
            failed: false,
            load: None,
            confirm_delete: None,
            body: Body::Loading,
            seen: (0, model.dm.list.read(cx).get(peer).map_or(0, |c| c.unread)),
            focused,
            list_bottom: false,
            list_shown: false,
            reading: false,
            _watch: watch,
            _subscriptions: subscriptions,
        };
        // On switching to a peer, the newest page unless the server's history is in.
        if let Some(load) = actions::dm::open_history(peer, cx) {
            this.load = Some(this.await_load(load, cx));
        }
        PeerInfos::want(peer, cx);
        this.body = this.body_now(cx);
        this.seen.0 = this.slice.read(cx).rows().len();
        // Read once the list says it landed at the bottom of a focused window (its first frame).
        this.update_reading(cx);
        this
    }

    /// Whether the conversation is being read now (tests).
    pub fn reading(&self) -> bool {
        self.reading
    }

    /// The three conditions of "being read" may have changed: tell the model, and read the
    /// conversation when they start to hold.
    fn update_reading(&mut self, cx: &mut Context<Self>) {
        let shown = self.body == Body::List && self.theater(cx).is_none();
        if shown != self.list_shown {
            self.list_shown = shown;
            if !shown {
                // Hidden: its last word on the bottom goes stale; drawn again, it says anew.
                self.list_bottom = false;
                self.list.update(cx, |list, _| list.retell_bottom());
            }
        }
        let reading = self.focused && self.list_shown && self.list_bottom;
        if reading == self.reading {
            return;
        }
        self.reading = reading;
        let peer = self.peer;
        let list = Model::global(cx).dm.list.clone();
        list.update(cx, |l, _| l.set_reading(peer, reading));
        if reading {
            cx.defer(move |cx| actions::dm::mark_read(peer, cx));
        }
    }

    pub fn peer(&self) -> Uid {
        self.peer
    }

    pub fn list(&self) -> &Entity<MessageList> {
        &self.list
    }

    pub fn body(&self) -> Body {
        self.body
    }

    /// The composer under the list (none until the app registers one).
    pub fn composer(&self) -> Option<&AnyView> {
        self.composer.as_ref().map(|c| &c.view)
    }

    fn await_load(&mut self, load: decibell_model::chat::history::LatestLoad, cx: &mut Context<Self>) -> Task<()> {
        cx.spawn(async move |this, cx| {
            let ok = load.await;
            let _ = this.update(cx, |this, cx| {
                this.failed = !ok;
                this.slice_changed(cx);
                cx.notify();
            });
        })
    }

    fn retry(&mut self, cx: &mut Context<Self>) {
        self.failed = false;
        let load = actions::dm::load_latest(self.peer, cx);
        self.load = Some(self.await_load(load, cx));
        self.slice_changed(cx);
    }

    fn body_now(&self, cx: &App) -> Body {
        let slice = self.slice.read(cx);
        let rows = slice.rows().len();
        let awaiting = !slice.fetched && !self.failed;
        if awaiting {
            Body::Loading
        } else if (rows == 0 || !slice.fetched) && self.failed {
            Body::Failed
        } else if rows == 0 {
            Body::Beginning
        } else {
            Body::List
        }
    }

    fn slice_changed(&mut self, cx: &mut Context<Self>) {
        let body = self.body_now(cx);
        if body != self.body {
            self.body = body;
            self.update_reading(cx);
            cx.notify();
        }
        let rows = self.slice.read(cx).rows().len();
        if rows != self.seen.0 {
            self.seen.0 = rows;
            let peer = self.peer;
            // New rows while being read are read (a DM arriving for it wasn't counted).
            if self.reading {
                cx.defer(move |cx| actions::dm::mark_read(peer, cx));
            }
            // The theater badge counts messages.
            if self.theater(cx).is_some() {
                cx.notify();
            }
        }
    }

    fn list_event(&mut self, list: Entity<MessageList>, event: &ListEvent, cx: &mut Context<Self>) {
        match event {
            ListEvent::Reply(id) => list.update(cx, |list, cx| list.set_reply_target(Some(*id), cx)),
            ListEvent::Delete { id, skip_confirm: true } => actions::dm::delete_dm(self.peer, *id, cx),
            ListEvent::Delete { id, .. } => {
                self.confirm_delete = Some(*id);
                cx.notify();
            }
            ListEvent::OpenLink(url) => cx.open_url(url),
            ListEvent::AtBottom(at) => {
                self.list_bottom = *at;
                self.update_reading(cx);
            }
            ListEvent::OpenMedia(_) => {}
        }
    }

    /// The call stage fills the panel: messages since it did (`theaterUnread`), else none.
    fn theater(&self, cx: &App) -> Option<usize> {
        let calls = Model::global(cx).live.calls.read(cx);
        let here = calls.status == CallStatus::Active && calls.peer == Some(self.peer);
        (calls.theater && here).then(|| self.slice.read(cx).rows().len().saturating_sub(calls.theater_baseline))
    }

    /// The call stage shows while a call with this peer rings out, connects or runs.
    fn stage_shown(&self, cx: &App) -> bool {
        let calls = Model::global(cx).live.calls.read(cx);
        !matches!(calls.status, CallStatus::Idle | CallStatus::Incoming) && calls.peer == Some(self.peer)
    }
}

fn conversation_name(peer: Uid, cx: &App) -> SharedString {
    let fallback = Model::global(cx).dm.list.read(cx).get(peer).map(|c| c.username.clone()).unwrap_or_default();
    decibell_common::user_name(peer, &fallback, cx)
}

/// `fadeUp`: from 6 px down and transparent, `duration` long after `delay`.
fn fade_up<E: IntoElement + Styled + 'static>(id: impl Into<gpui::ElementId>, delay: f32, duration: f32, element: E) -> impl IntoElement {
    let total = delay + duration;
    element.with_animation(
        id,
        Animation::new(Duration::from_secs_f32(total)),
        move |element, t| {
            let p = ((t * total - delay) / duration).clamp(0., 1.);
            let p = motion::EASE.y(p as f64) as f32;
            element.opacity(p).top(px(6. * (1. - p)))
        },
    )
}

impl Render for Conversation {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let model = Model::global(cx).clone();
        let peer = self.peer;
        let name = conversation_name(peer, cx);
        let theater = self.theater(cx);
        // The stage: built while it shows, dropped when it goes.
        if self.stage_shown(cx) {
            if self.stage.is_none() {
                self.stage = decibell_shell::build_slot(Slot::DmCallStage, window, cx);
            }
        } else {
            self.stage = None;
        }
        let key = current_key(peer, cx);
        let info = PeerInfos::entity(cx).read(cx).get(peer, key).cloned();
        let facts = {
            let e2ee = model.live.e2ee.read(cx);
            let calls = model.live.calls.read(cx);
            HeaderFacts {
                peer,
                name: name.clone(),
                presence: presence_of(peer, false, cx),
                message: status_message_of(peer, cx),
                status: e2ee.status,
                info: info.clone(),
                call_status: calls.status,
                call_signaling: calls.call_signaling,
                theater,
                rail_on: model.layout.read(cx).prefs().dm_friends_panel,
            }
        };
        let notice = banner::banner(peer, &name, info.as_ref(), cx);
        let head = header(&facts, cx);

        let theme = cx.theme();
        let (bg_mid, muted, border, bright, secondary) =
            (theme.color_bg_mid, theme.color_text_muted, theme.color_border, theme.color_text_bright, theme.color_text_secondary);
        let centered = || div().flex_1().flex().flex_col().items_center().justify_center();
        let body = match self.body {
            // Keyed per conversation: each conversation's wait starts blank.
            Body::Loading => centered()
                .child(fade_up(("dm-loading", peer as u64), 0.3, 0.3, div().relative().child(Label::new("Loading…").size(14.).color(muted))))
                .into_any_element(),
            Body::Failed => centered()
                .gap_3()
                .child(Label::new("Couldn't load messages.").size(14.).color(muted))
                .child(button("dm-retry", "Retry", Look::primary(cx), false).on_click(cx.listener(|this, _: &ClickEvent, _, cx| this.retry(cx))))
                .into_any_element(),
            Body::Beginning => {
                let name_color = person_name_color(peer, &name, cx);
                div()
                    .flex_1()
                    .pr_4()
                    .py_4()
                    .child(fade_up(
                        ("dm-beginning", peer as u64),
                        0.,
                        0.4,
                        div().relative().pl_4().child(
                            div()
                                .border_b_1()
                                .border_color(border)
                                .pb_5()
                                .mb_5()
                                .child(div().mb_3().child(UserAvatar::new(peer, name.clone(), 60.)))
                                .child(
                                    div()
                                        .mb_1p5()
                                        .child(Label::new(name.clone()).font(FontRole::Sans).size(26.).weight(600.).tracking(-0.025).color(bright)),
                                )
                                .child(
                                    div()
                                        .flex()
                                        .flex_wrap()
                                        .child(
                                            Label::new("This is the beginning of your conversation with ")
                                                .size(14.)
                                                .line_height(14. * 1.55)
                                                .color(secondary),
                                        )
                                        .child(Label::new(name.clone()).size(14.).line_height(14. * 1.55).weight(600.).color(name_color))
                                        .child(Label::new(".").size(14.).line_height(14. * 1.55).color(secondary)),
                                ),
                        ),
                    ))
                    .into_any_element()
            }
            Body::List => div().flex_1().min_h_0().child(self.list.clone().cached(StyleRefinement::default().size_full())).into_any_element(),
        };
        let confirm = self.confirm_delete;
        // "Type anywhere to compose": a key that reaches the conversation from the list (or with
        // nothing focused) goes to the composer (`useTypeToFocusComposer`), as in the chat pane.
        let type_to_focus = self.composer.as_ref().map(|c| c.type_to_focus.clone());
        let list_focus = self.list.read(cx).focus_handle().clone();
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(bg_mid)
            .when_some(type_to_focus, |root, hook| {
                root.on_key_down(move |event, window, cx| {
                    let focused = window.focused(cx);
                    if (focused.is_none() || focused.as_ref() == Some(&list_focus)) && hook(event, window, cx) {
                        cx.stop_propagation();
                    }
                })
            })
            .child(head)
            .children(notice)
            .children(self.stage.clone().map(|stage| stage.into_any_element()))
            .when(theater.is_none(), |this| {
                // The composer draws the inline send error above its card (`sendError`).
                this.child(div().relative().flex().flex_col().flex_1().min_h_0().overflow_hidden().child(body)).children(
                    self.composer.as_ref().map(|c| div().relative().children(decibell_shell::probes::probe("dm-composer")).child(c.view.clone())),
                )
            })
            .child(
                ConfirmModal::new("delete-dm", confirm.is_some(), "Delete message", "Delete")
                    .message("Delete this message? This cannot be undone.\n\nTip: hold Shift when clicking delete to skip this confirmation.")
                    .tone(ConfirmTone::Danger)
                    .on_confirm(cx.listener(|this, _, _, cx| {
                        if let Some(id) = this.confirm_delete.take() {
                            actions::dm::delete_dm(this.peer, id, cx);
                        }
                        cx.notify();
                    }))
                    .on_cancel(cx.listener(|this, _, _, cx| {
                        this.confirm_delete = None;
                        cx.notify();
                    })),
            )
    }
}
