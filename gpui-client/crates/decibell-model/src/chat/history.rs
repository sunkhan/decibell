//! History requests (channel and DM) and the waits on their replies
//! (`features/chat/historyRequests.ts`).
//!
//! A reply comes back as an event, not as the call's result, and it can be lost: with the
//! connection (a reconnect), or without it (the core skips a frame over 2 MiB). Every
//! request made here is tracked until its reply arrives or [`REPLY_TIMEOUT`] passes, and
//! the future says which: the reply's summary, or `None` (timed out, not sent, or abandoned
//! because the connection went away). Callers retry or reset on `None`.
//!
//! Replies are matched by the mode the server echoes: an around reply by its `around_id`,
//! an after reply by its `after_id`, and a newest / older page (neither echoed) by order,
//! oldest request first. Each wait holds its own timeout timer, dropped with it.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::Duration;

use futures::FutureExt as _;
use futures::channel::oneshot;
use futures::future::Shared;
use gpui::{App, Context, Task};

use crate::ids::{ChannelId, ServerId, Uid};

pub const REPLY_TIMEOUT: Duration = Duration::from_millis(10_000);
/// Between attempts of a newest-page load, and before a paginator asks again after a
/// failure (a failing send can't spin). Views wait this long before re-checking edges.
pub const RETRY_DELAY: Duration = Duration::from_millis(1_000);
pub const LATEST_ATTEMPTS: u32 = 3;
/// Rows per page the panels ask for.
pub const PAGE_LIMIT: i32 = 50;
/// A jump window's size (`aroundId`, 25).
pub const AROUND_LIMIT: i32 = 25;
/// What the echo check pages with.
pub const ECHO_PAGE_LIMIT: i32 = 100;

/// Whose history.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum WaitKey {
    Channel(ServerId, ChannelId),
    Dm(Uid),
}

impl WaitKey {
    fn scope(&self) -> Scope {
        match self {
            WaitKey::Channel(server, _) => Scope::Community(*server),
            WaitKey::Dm(_) => Scope::Central,
        }
    }
}

/// What `abandon` matches: one community's requests, or central's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Scope {
    Community(ServerId),
    Central,
}

/// One request (`HistoryRequest`): `before_id` 0 with no other cursor = the newest page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct HistoryRequest {
    pub before_id: i64,
    pub after_id: i64,
    pub around_id: i64,
    pub limit: i32,
}

impl HistoryRequest {
    pub fn latest(limit: i32) -> Self {
        HistoryRequest { limit, ..Default::default() }
    }
    pub fn before(before_id: i64, limit: i32) -> Self {
        HistoryRequest { before_id, limit, ..Default::default() }
    }
    pub fn after(after_id: i64, limit: i32) -> Self {
        HistoryRequest { after_id, limit, ..Default::default() }
    }
    pub fn around(around_id: i64, limit: i32) -> Self {
        HistoryRequest { around_id, limit, ..Default::default() }
    }
    fn mode(&self) -> (Mode, i64) {
        if self.around_id > 0 {
            (Mode::Around, self.around_id)
        } else if self.after_id > 0 {
            (Mode::After, self.after_id)
        } else {
            // The cursor is kept for the guard below; replies match by order.
            (Mode::Before, self.before_id)
        }
    }
}

/// What a reply said, for the waiter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HistoryReply {
    /// The highest id in the page (0 for an empty one).
    pub newest_id: i64,
    /// The server has newer rows past the page.
    pub has_more_after: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Before,
    After,
    Around,
}

struct Waiter {
    id: u64,
    scope: Scope,
    mode: Mode,
    cursor: i64,
    done: oneshot::Sender<Option<HistoryReply>>,
    _timeout: Task<()>,
}

/// How a request leaves: the core call in the app (`actions::chat::send_history_request`),
/// a recorder in tests. It reports whether the request was sent.
pub type HistorySender = Rc<dyn Fn(WaitKey, HistoryRequest, &mut App, SentCallback)>;

/// Told whether a request was sent.
pub type SentCallback = Box<dyn FnOnce(bool, &mut App)>;

/// A newest-page load in flight, shared by every caller (`latestChannelLoads`).
pub type LatestLoad = Shared<Task<bool>>;

pub struct HistoryRequests {
    waiters: HashMap<WaitKey, Vec<Waiter>>,
    next_id: u64,
    sender: HistorySender,
    pub(crate) latest: HashMap<WaitKey, LatestLoad>,
}

impl HistoryRequests {
    pub fn new(sender: HistorySender) -> HistoryRequests {
        HistoryRequests { waiters: HashMap::new(), next_id: 0, sender, latest: HashMap::new() }
    }

    /// Send `request` and wait for its reply (or [`REPLY_TIMEOUT`]).
    pub fn request(
        &mut self,
        key: WaitKey,
        request: HistoryRequest,
        cx: &mut Context<Self>,
    ) -> impl std::future::Future<Output = Option<HistoryReply>> + 'static + use<> {
        let (mode, cursor) = request.mode();
        let (tx, rx) = oneshot::channel();
        self.next_id += 1;
        let id = self.next_id;
        let timeout = cx.spawn({
            let key = key.clone();
            async move |this, cx| {
                cx.background_executor().timer(REPLY_TIMEOUT).await;
                let _ = this.update(cx, |this, _| this.finish(&key, id, None));
            }
        });
        self.waiters.entry(key.clone()).or_default().push(Waiter {
            id,
            scope: key.scope(),
            mode,
            cursor,
            done: tx,
            _timeout: timeout,
        });
        let sender = self.sender.clone();
        let this = cx.weak_entity();
        // After this update: the sender may answer at once (a recorder, "not running").
        cx.defer(move |cx| {
            sender(
                key.clone(),
                request,
                cx,
                Box::new(move |sent, cx| {
                    if !sent {
                        let _ = this.update(cx, |this, _| this.finish(&key, id, None));
                    }
                }),
            )
        });
        rx.map(|r| r.ok().flatten())
    }

    fn finish(&mut self, key: &WaitKey, id: u64, reply: Option<HistoryReply>) {
        let Some(list) = self.waiters.get_mut(key) else { return };
        let Some(i) = list.iter().position(|w| w.id == id) else { return };
        let waiter = list.remove(i);
        if list.is_empty() {
            self.waiters.remove(key);
        }
        let _ = waiter.done.send(reply);
    }

    #[cfg(test)]
    pub(crate) fn set_sender(&mut self, sender: HistorySender) {
        self.sender = sender;
    }

    /// A reply arrived (after the page was applied to the store).
    pub fn settle(&mut self, key: &WaitKey, around_id: i64, after_id: i64, reply: HistoryReply) {
        let Some(list) = self.waiters.get(key) else { return };
        let found = if around_id > 0 {
            list.iter().find(|w| w.mode == Mode::Around && w.cursor == around_id)
        } else if after_id > 0 {
            list.iter().find(|w| w.mode == Mode::After && w.cursor == after_id)
        } else {
            list.iter().find(|w| w.mode == Mode::Before)
        };
        if let Some(id) = found.map(|w| w.id) {
            self.finish(key, id, Some(reply));
        }
    }

    /// The `before_id` of the request a cursorless reply answers (the oldest waiting
    /// newest-or-older request), if one waits. A reply with no cursor echoed is the newest
    /// page, except when it answers an older-page request: a server that predates the
    /// echo reads `before_id` as 0 (proto3), and taking that for the newest page wiped the
    /// loaded rows on 2026-10-28. Never act destructively on a field an older server omits.
    pub fn waiting_before_cursor(&self, key: &WaitKey) -> Option<i64> {
        self.waiters.get(key)?.iter().find(|w| w.mode == Mode::Before).map(|w| w.cursor)
    }

    /// The connection went away (or was replaced): nothing sent on it will be answered.
    pub fn abandon(&mut self, scope: Scope) {
        let mut done = Vec::new();
        for (key, list) in &self.waiters {
            done.extend(list.iter().filter(|w| w.scope == scope).map(|w| (key.clone(), w.id)));
        }
        for (key, id) in done {
            self.finish(&key, id, None);
        }
    }

    /// Requests waiting for a reply (tests, dumps).
    pub fn waiting(&self) -> usize {
        self.waiters.values().map(Vec::len).sum()
    }

    pub fn reset_for_logout(&mut self) {
        self.waiters.clear();
        self.latest.clear();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    type Log = Rc<RefCell<Vec<(WaitKey, HistoryRequest)>>>;

    fn requests(cx: &mut TestAppContext, sent: bool) -> (gpui::Entity<HistoryRequests>, Log) {
        let log: Log = Rc::default();
        let l = log.clone();
        let h = cx.new(|_| {
            HistoryRequests::new(Rc::new(move |key, req, cx, done| {
                l.borrow_mut().push((key, req));
                done(sent, cx);
            }))
        });
        (h, log)
    }

    #[gpui::test]
    fn replies_match_by_mode_and_time_out(cx: &mut TestAppContext) {
        let (h, log) = requests(cx, true);
        let key = WaitKey::Channel(ServerId::from_central(1), "general".into());
        type Results = Rc<RefCell<Vec<(&'static str, Option<HistoryReply>)>>>;
        let results: Results = Rc::default();
        for (name, req) in [
            ("older", HistoryRequest::before(100, 50)),
            ("after", HistoryRequest::after(200, 50)),
            ("around", HistoryRequest::around(300, 25)),
            ("lost", HistoryRequest::latest(50)),
        ] {
            let fut = h.update(cx, |h, cx| h.request(key.clone(), req, cx));
            let results = results.clone();
            cx.spawn(async move |_| {
                let reply = fut.await;
                results.borrow_mut().push((name, reply));
            })
            .detach();
        }
        cx.run_until_parked();
        assert_eq!(log.borrow().len(), 4);
        let reply = HistoryReply { newest_id: 7, has_more_after: false };
        h.update(cx, |h, _| {
            h.settle(&key, 300, 0, reply);
            h.settle(&key, 0, 200, reply);
            // Neither cursor: the oldest before-mode request.
            h.settle(&key, 0, 0, reply);
            // An after reply nobody waits for.
            h.settle(&key, 0, 999, reply);
        });
        cx.run_until_parked();
        assert_eq!(*results.borrow(), [("around", Some(reply)), ("after", Some(reply)), ("older", Some(reply))]);
        // The last one gets no answer: None after 10 s, and nothing is left waiting.
        cx.executor().advance_clock(REPLY_TIMEOUT);
        cx.run_until_parked();
        assert_eq!(results.borrow().last(), Some(&("lost", None)));
        h.read_with(cx, |h, _| assert_eq!(h.waiting(), 0));
    }

    #[gpui::test]
    fn unsent_and_abandoned_requests_end_at_once(cx: &mut TestAppContext) {
        let (h, _) = requests(cx, false);
        let fut = h.update(cx, |h, cx| h.request(WaitKey::Dm(3), HistoryRequest::latest(50), cx));
        let got = Rc::new(RefCell::new(None));
        let g = got.clone();
        cx.spawn(async move |_| {
            let reply = fut.await;
            *g.borrow_mut() = Some(reply);
        })
        .detach();
        cx.run_until_parked();
        assert_eq!(*got.borrow(), Some(None));

        let (h, _) = requests(cx, true);
        let server = ServerId::from_central(2);
        let a = h.update(cx, |h, cx| h.request(WaitKey::Channel(server, "a".into()), HistoryRequest::latest(50), cx));
        let b = h.update(cx, |h, cx| h.request(WaitKey::Dm(3), HistoryRequest::latest(50), cx));
        let got: Rc<RefCell<Vec<Option<HistoryReply>>>> = Rc::default();
        for fut in [a.boxed_local(), b.boxed_local()] {
            let got = got.clone();
            cx.spawn(async move |_| {
                let reply = fut.await;
                got.borrow_mut().push(reply);
            })
            .detach();
        }
        cx.run_until_parked();
        h.update(cx, |h, _| h.abandon(Scope::Community(server)));
        cx.run_until_parked();
        assert_eq!(*got.borrow(), [None]);
        h.read_with(cx, |h, _| assert_eq!(h.waiting(), 1, "central's request still waits"));
    }
}
