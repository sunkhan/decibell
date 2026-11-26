//! One conversation (`DmChatPanel.tsx`): the header, the encryption notice, the call stage, the
//! message list on the peer's slice (or what stands in for it: loading, retry, the beginning of
//! the conversation), the inline send error, the composer, the delete confirmation.
//!
//! It observes exactly what it draws: its slice's flags (the body), the conversation's sidebar
//! facts and the send error (`dm.list`, compared), the re-sync event, the E2EE store and the
//! peer's encryption answer, the call store, the peer's name / status / message, the rail toggle.
//!
//! **Mark read** (the renderer's effect on `[activeDmUser, messages.length, unreadCount]`): while
//! the conversation is mounted, every change of its row count or unread count reads it up to its
//! newest loaded row (`actions::dm::mark_read`: the count clears at once, central hears 250 ms
//! later). As in the renderer this needs neither the window's focus nor the list at its bottom.

use std::time::Duration;

use decibell_chat::{ListEvent, MessageList};
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
use super::{banner, composer::FallbackComposer, dm_composer};
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
    composer: AnyView,
    stage: Option<AnyView>,
    failed: bool,
    load: Option<Task<()>>,
    confirm_delete: Option<i64>,
    body: Body,
    /// What the mark-read effect last saw: (rows, unread).
    seen: (usize, i64),
    send_error: Option<SharedString>,
    _watch: Watch,
    _subscriptions: Vec<Subscription>,
}

impl Conversation {
    pub fn new(peer: Uid, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let model = Model::global(cx).clone();
        let source = std::rc::Rc::new(DmSource { peer });
        let slice = decibell_chat::ListSource::slice(&*source, cx);
        let list = cx.new(|cx| MessageList::new(source, cx));
        // Re-sync handover: `markHistoriesStale` leaves the conversation on screen to us.
        model.dm.list.update(cx, |l, _| l.set_displayed(Some(peer)));
        let composer: AnyView = match dm_composer(cx) {
            Some(build) => build(peer, list.clone(), window, cx),
            None => {
                let name = conversation_name(peer, cx);
                let list = list.clone();
                cx.new(|cx| FallbackComposer::new(peer, name, list, window, cx)).into()
            }
        };
        let mut watch = Watch::new();
        watch
            .store(&model.live.calls, cx)
            .event(&model.layout, cx, |_, event: &LayoutEvent| *event == LayoutEvent::Panels)
            .people(cx, move |_, uid| uid == peer);
        let peer_infos = PeerInfos::entity(cx);
        let subscriptions = vec![
            cx.observe(&slice, |this, _, cx| this.slice_changed(cx)),
            // The conversation's unread count (mark read) and the composer's send error.
            cx.observe(&model.dm.list, move |this, list, cx| {
                let (error, unread) = {
                    let list = list.read(cx);
                    let error = list.send_error.as_ref().filter(|(p, _)| *p == peer).map(|(_, e)| e.clone());
                    (error, list.get(peer).map_or(0, |c| c.unread))
                };
                if unread != this.seen.1 {
                    this.seen.1 = unread;
                    cx.defer(move |cx| actions::dm::mark_read(peer, cx));
                }
                if error != this.send_error {
                    this.send_error = error;
                    cx.notify();
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
            if model.dm.list.read(cx).displayed() == Some(peer) {
                model.dm.list.update(cx, |l, _| l.set_displayed(None));
            }
        })
        .detach();
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
            send_error: None,
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
        cx.defer(move |cx| actions::dm::mark_read(peer, cx));
        this
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

    pub fn composer(&self) -> &AnyView {
        &self.composer
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
            cx.notify();
        }
        let rows = self.slice.read(cx).rows().len();
        if rows != self.seen.0 {
            self.seen.0 = rows;
            let peer = self.peer;
            cx.defer(move |cx| actions::dm::mark_read(peer, cx));
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
                self.stage = decibell_shell::build_pane(Slot::DmCallStage, window, cx);
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
        let (bg_mid, muted, border, bright, secondary, error) = (
            theme.color_bg_mid,
            theme.color_text_muted,
            theme.color_border,
            theme.color_text_bright,
            theme.color_text_secondary,
            theme.color_error,
        );
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
        div()
            .relative()
            .size_full()
            .min_w_0()
            .flex()
            .flex_col()
            .bg(bg_mid)
            .child(head)
            .children(notice)
            .children(self.stage.clone().map(|stage| stage.into_any_element()))
            .when(theater.is_none(), |this| {
                this.child(div().relative().flex().flex_col().flex_1().min_h_0().overflow_hidden().child(body))
                    .when_some(self.send_error.clone(), |this, e| this.child(div().px_4().child(Label::new(e).size(12.).color(error))))
                    .child(div().relative().children(decibell_shell::probes::probe("dm-composer")).child(self.composer.clone().into_any_element()))
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
