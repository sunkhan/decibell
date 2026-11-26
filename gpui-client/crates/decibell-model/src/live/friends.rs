//! The friend list (`stores/friendsStore.ts`, `features/friends/useFriendsEvents.ts`,
//! `friendList.ts`).
//!
//! Central sends the whole list (`friend_list_received`) on request and after every
//! change; presence flips accepted friends between online and offline in between
//! (`reconcile`). The request is asked again when no list comes back (review B4: the
//! reply can be lost with central's connection, or skipped as an oversized frame): up to
//! [`ATTEMPTS`] times, [`REPLY_TIMEOUT`] apart, a newer request taking over from an older
//! one's retries. That timer exists only while a request waits for its list.
//!
//! An action's refusal (`friend_action_responded`) or a send error shows inline on the
//! Friends page for [`ERROR_LIFETIME`] (the renderer clears it with a timer in each view
//! that shows it; here the store holds the one timer, only while an error shows).
//!
//! **Rhythm**: a list per change, a flip per presence change of a friend. The Friends
//! page and the DM sidebar observe it.

use std::rc::Rc;
use std::time::Duration;

use decibell_core::events::FriendInfoPayload;
use futures::future::LocalBoxFuture;
use gpui::{Context, SharedString, Task};

use crate::error::UiError;
use crate::ids::Uid;

pub const REPLY_TIMEOUT: Duration = Duration::from_secs(10);
pub const ATTEMPTS: u32 = 3;
pub const ERROR_LIFETIME: Duration = Duration::from_secs(5);

/// The protobuf `FriendActionType`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FriendAction {
    Add = 0,
    /// Also cancels an outgoing request and unblocks.
    Remove = 1,
    Block = 2,
    Accept = 3,
    /// Declines an incoming request.
    Reject = 4,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FriendStatus {
    Online,
    Offline,
    PendingIncoming,
    PendingOutgoing,
    Blocked,
}

impl FriendStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "online" => Some(Self::Online),
            "offline" => Some(Self::Offline),
            "pending_incoming" => Some(Self::PendingIncoming),
            "pending_outgoing" => Some(Self::PendingOutgoing),
            "blocked" => Some(Self::Blocked),
            _ => None,
        }
    }

    /// An accepted friend (a conversation exists).
    pub fn is_accepted(self) -> bool {
        matches!(self, Self::Online | Self::Offline)
    }

    pub fn is_pending(self) -> bool {
        matches!(self, Self::PendingIncoming | Self::PendingOutgoing)
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Friend {
    pub uid: Uid,
    /// The list's name (fallback; views draw the users store's).
    pub username: SharedString,
    pub status: FriendStatus,
    pub avatar_version: SharedString,
}

impl Friend {
    pub fn from_payload(f: &FriendInfoPayload) -> Option<Friend> {
        Some(Friend {
            uid: f.uid,
            username: f.username.clone().into(),
            status: FriendStatus::parse(&f.status)?,
            avatar_version: f.avatar_version.clone().into(),
        })
    }
}

/// Sends `FRIEND_LIST_REQ`.
pub type ListRequester = Rc<dyn Fn() -> LocalBoxFuture<'static, Result<(), UiError>>>;

pub struct Friends {
    list: Vec<Friend>,
    pub last_action_error: Option<SharedString>,
    /// Lists received (asked for or pushed).
    received: u64,
    generation: u64,
    requester: ListRequester,
    /// The reply timeout of a waiting request.
    retry: Option<Task<()>>,
    /// The inline error's lifetime.
    error_timer: Option<Task<()>>,
}

impl Friends {
    pub fn new(requester: ListRequester) -> Self {
        Friends {
            list: Vec::new(),
            last_action_error: None,
            received: 0,
            generation: 0,
            requester,
            retry: None,
            error_timer: None,
        }
    }

    pub fn list(&self) -> &[Friend] {
        &self.list
    }

    pub fn get(&self, uid: Uid) -> Option<&Friend> {
        self.list.iter().find(|f| f.uid == uid)
    }

    pub fn accepted_count(&self) -> usize {
        self.list.iter().filter(|f| f.status.is_accepted()).count()
    }

    pub fn waiting_for_list(&self) -> bool {
        self.retry.is_some()
    }

    /// A whole list from central.
    pub fn set_list(&mut self, list: Vec<Friend>, cx: &mut Context<Self>) {
        self.received += 1;
        self.retry = None;
        if self.list != list {
            self.list = list;
            cx.notify();
        }
    }

    /// Flip accepted friends between online and offline to match presence; pending and
    /// blocked rows, and friends `online` knows nothing about (none), stay.
    pub fn reconcile(&mut self, online: impl Fn(Uid) -> Option<bool>, cx: &mut Context<Self>) {
        let mut changed = false;
        for f in self.list.iter_mut().filter(|f| f.status.is_accepted()) {
            if let Some(on) = online(f.uid) {
                let next = if on { FriendStatus::Online } else { FriendStatus::Offline };
                if f.status != next {
                    f.status = next;
                    changed = true;
                }
            }
        }
        if changed {
            cx.notify();
        }
    }

    /// Ask central for the list, again while none arrives (`requestFriendList`).
    pub fn request(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        let (generation, before) = (self.generation, self.received);
        self.attempt(0, generation, before, cx);
    }

    fn attempt(&mut self, n: u32, generation: u64, before: u64, cx: &mut Context<Self>) {
        let sent = (self.requester)();
        self.retry = Some(cx.spawn(async move |this, cx| {
            if let Err(e) = sent.await {
                log::error!("request_friend_list: {e}");
            }
            cx.background_executor().timer(REPLY_TIMEOUT).await;
            let _ = this.update(cx, |this, cx| {
                if this.generation != generation || this.received > before {
                    return;
                }
                if n + 1 >= ATTEMPTS {
                    this.retry = None;
                    return;
                }
                this.attempt(n + 1, generation, before, cx);
            });
        }));
    }

    /// Show an action's error inline (or clear it).
    pub fn set_error(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        self.error_timer = error.as_ref().map(|_| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(ERROR_LIFETIME).await;
                let _ = this.update(cx, |this, cx| {
                    this.error_timer = None;
                    this.last_action_error = None;
                    cx.notify();
                });
            })
        });
        if self.last_action_error != error {
            self.last_action_error = error;
            cx.notify();
        }
    }

    /// Sign-out.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.list.clear();
        self.last_action_error = None;
        self.generation += 1;
        self.retry = None;
        self.error_timer = None;
        cx.notify();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn friend(uid: Uid, status: FriendStatus) -> Friend {
        Friend { uid, username: format!("u{uid}").into(), status, avatar_version: "".into() }
    }

    fn store(cx: &mut TestAppContext) -> (gpui::Entity<Friends>, Rc<Cell<u32>>) {
        let asks = Rc::new(Cell::new(0));
        let f = cx.new(|_| {
            let asks = asks.clone();
            Friends::new(Rc::new(move || {
                asks.set(asks.get() + 1);
                Box::pin(async { Ok(()) })
            }))
        });
        (f, asks)
    }

    #[gpui::test]
    fn a_lost_reply_is_asked_again_three_times_at_most(cx: &mut TestAppContext) {
        let (f, asks) = store(cx);
        f.update(cx, |f, cx| f.request(cx));
        cx.run_until_parked();
        assert_eq!(asks.get(), 1);
        cx.executor().advance_clock(REPLY_TIMEOUT);
        cx.run_until_parked();
        assert_eq!(asks.get(), 2);
        cx.executor().advance_clock(REPLY_TIMEOUT * 5);
        cx.run_until_parked();
        assert_eq!(asks.get(), 3);
        assert!(!f.read_with(cx, |f, _| f.waiting_for_list()), "gave up: no timer");
        // A list that arrives ends the wait at once.
        f.update(cx, |f, cx| f.request(cx));
        cx.run_until_parked();
        f.update(cx, |f, cx| f.set_list(vec![friend(2, FriendStatus::Online)], cx));
        cx.executor().advance_clock(REPLY_TIMEOUT * 3);
        cx.run_until_parked();
        assert_eq!(asks.get(), 4);
    }

    /// Request → accept → remove, as the lists central sends after each action, and
    /// presence flipping an accepted friend.
    #[gpui::test]
    fn list_transitions(cx: &mut TestAppContext) {
        let (f, _) = store(cx);
        f.update(cx, |f, cx| f.set_list(vec![friend(5, FriendStatus::PendingOutgoing)], cx));
        f.update(cx, |f, cx| f.reconcile(|_| Some(true), cx));
        f.read_with(cx, |f, _| assert_eq!(f.get(5).unwrap().status, FriendStatus::PendingOutgoing, "pending rows don't flip"));
        f.update(cx, |f, cx| f.set_list(vec![friend(5, FriendStatus::Offline)], cx));
        f.update(cx, |f, cx| f.reconcile(|uid| (uid == 5).then_some(true), cx));
        f.read_with(cx, |f, _| {
            assert_eq!(f.get(5).unwrap().status, FriendStatus::Online);
            assert_eq!(f.accepted_count(), 1);
        });
        f.update(cx, |f, cx| f.set_list(vec![], cx));
        f.read_with(cx, |f, _| assert!(f.list().is_empty()));
        // The inline error goes after 5 s.
        f.update(cx, |f, cx| f.set_error(Some("No such user".into()), cx));
        cx.executor().advance_clock(ERROR_LIFETIME);
        cx.run_until_parked();
        f.read_with(cx, |f, _| assert!(f.last_action_error.is_none()));
    }
}
