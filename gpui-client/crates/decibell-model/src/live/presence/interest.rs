//! Presence by subscription: whom this client watches (`features/presence/
//! presenceInterest.ts`; spec `2026-10-15-central-scaling-design.md`).
//!
//! Every view that shows a user's status, message or picture retains that uid while it
//! is on screen and releases it when it goes (ref-counted: the same person in the member
//! list and a DM row is one watch). The union goes to central, which then sends statuses
//! and profile changes for those users only. The client never watches itself (our own
//! state comes from the sign-in reply and `own_presence`). The core keeps the last set
//! and re-sends it after every central sign-in (`net/central.rs`), so a reconnect loses
//! nothing and nothing here re-sends.
//!
//! **Batching**: at most one request per [`FLUSH_INTERVAL`] (central allows 10 burst, 2
//! per second): a list mounting or a server switch changes many rows at once. The timer
//! exists only while a change is waiting; an unchanged set is never re-sent.

use std::collections::HashMap;
use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::future::LocalBoxFuture;
use gpui::{Context, Task};

use crate::error::UiError;
use crate::ids::Uid;

/// Central's cap per connection (`SessionManager::MAX_WATCH`).
pub const MAX_WATCH: usize = 5000;
/// Least time between two subscription requests.
pub const FLUSH_INTERVAL: Duration = Duration::from_millis(600);

pub type Subscriber = Rc<dyn Fn(Vec<Uid>) -> LocalBoxFuture<'static, Result<(), UiError>>>;

pub struct PresenceInterest {
    counts: HashMap<Uid, u32>,
    /// Never watched (ourselves).
    me: Uid,
    last_flush: Option<Instant>,
    /// What central has; starts as the empty set, so nothing is sent until someone is
    /// watched.
    last_sent: Option<Vec<Uid>>,
    /// The pending flush (only while a change waits).
    flush: Option<Task<()>>,
    subscriber: Subscriber,
}

impl PresenceInterest {
    pub fn new(subscriber: Subscriber) -> Self {
        PresenceInterest {
            counts: HashMap::new(),
            me: 0,
            last_flush: None,
            last_sent: Some(Vec::new()),
            flush: None,
            subscriber,
        }
    }

    /// Whether a view shows `uid` right now (the avatar cache fetches and keeps these
    /// first).
    pub fn is_on_screen(&self, uid: Uid) -> bool {
        self.counts.contains_key(&uid)
    }

    pub fn watched(&self) -> usize {
        self.counts.len()
    }

    pub fn pending(&self) -> bool {
        self.flush.is_some()
    }

    /// Who is signed in (never watched).
    pub fn set_me(&mut self, me: Uid, cx: &mut Context<Self>) {
        if self.me != me {
            self.me = me;
            self.schedule(cx);
        }
    }

    pub fn retain(&mut self, uids: impl IntoIterator<Item = Uid>, cx: &mut Context<Self>) {
        let mut changed = false;
        for uid in uids.into_iter().filter(|u| *u > 0) {
            let n = self.counts.entry(uid).or_insert(0);
            *n += 1;
            changed |= *n == 1;
        }
        if changed {
            self.schedule(cx);
        }
    }

    pub fn release(&mut self, uids: impl IntoIterator<Item = Uid>, cx: &mut Context<Self>) {
        let mut changed = false;
        for uid in uids {
            match self.counts.get_mut(&uid) {
                Some(n) if *n > 1 => *n -= 1,
                Some(_) => {
                    self.counts.remove(&uid);
                    changed = true;
                }
                None => {}
            }
        }
        if changed {
            self.schedule(cx);
        }
    }

    fn schedule(&mut self, cx: &mut Context<Self>) {
        if self.flush.is_some() {
            return;
        }
        let now = cx.background_executor().now();
        let wait = self.last_flush.map_or(Duration::ZERO, |at| (at + FLUSH_INTERVAL).saturating_duration_since(now));
        self.flush = Some(cx.spawn(async move |this, cx| {
            // Even with no wait this runs after the current update: every retain made in
            // the same turn goes out in one request.
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| this.flush_now(cx));
        }));
    }

    fn flush_now(&mut self, cx: &mut Context<Self>) {
        self.flush = None;
        self.last_flush = Some(cx.background_executor().now());
        let me = self.me;
        let mut uids: Vec<Uid> = self.counts.keys().copied().filter(|u| *u != me).collect();
        uids.sort_unstable();
        uids.truncate(MAX_WATCH);
        if self.last_sent.as_ref() == Some(&uids) {
            return;
        }
        self.last_sent = Some(uids.clone());
        let sent = (self.subscriber)(uids);
        cx.spawn(async move |this, cx| {
            if let Err(e) = sent.await {
                log::error!("presence_subscribe: {e}");
                // Retried with the next change.
                let _ = this.update(cx, |this, _| this.last_sent = None);
            }
        })
        .detach();
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    type Sent = Rc<RefCell<Vec<Vec<Uid>>>>;

    fn interest(cx: &mut TestAppContext) -> (gpui::Entity<PresenceInterest>, Sent) {
        let sent: Sent = Rc::default();
        let entity = cx.new(|_| {
            let sent = sent.clone();
            PresenceInterest::new(Rc::new(move |uids| {
                sent.borrow_mut().push(uids);
                Box::pin(async { Ok(()) })
            }))
        });
        (entity, sent)
    }

    /// Ref-counted: the same person in two views is one watch; a burst of retains in one
    /// turn is one request; requests are 600 ms apart; ourselves are never watched; an
    /// unchanged set is not re-sent.
    #[gpui::test]
    fn ref_counts_and_batching(cx: &mut TestAppContext) {
        let (i, sent) = interest(cx);
        i.update(cx, |i, cx| {
            i.set_me(7, cx);
            i.retain([3, 2, 7, 0, -4], cx);
            i.retain([2], cx);
        });
        cx.run_until_parked();
        assert_eq!(*sent.borrow(), vec![vec![2, 3]], "one request; no 0, no placeholder, not us");
        // A change right after waits out the interval.
        i.update(cx, |i, cx| i.retain([5], cx));
        cx.run_until_parked();
        assert_eq!(sent.borrow().len(), 1);
        cx.executor().advance_clock(FLUSH_INTERVAL);
        cx.run_until_parked();
        assert_eq!(sent.borrow().last().unwrap(), &vec![2, 3, 5]);
        // One of two views of 2 goes: still watched, nothing sent.
        cx.executor().advance_clock(FLUSH_INTERVAL);
        i.update(cx, |i, cx| i.release([2], cx));
        cx.run_until_parked();
        assert!(i.read_with(cx, |i, _| i.is_on_screen(2) && !i.pending()));
        // The last view of 2 goes; 5 goes and comes back within the interval.
        i.update(cx, |i, cx| {
            i.release([2, 5], cx);
            i.retain([5], cx);
        });
        cx.run_until_parked();
        assert_eq!(sent.borrow().last().unwrap(), &vec![3, 5]);
        // Released and re-retained to the same set: no request.
        cx.executor().advance_clock(FLUSH_INTERVAL);
        i.update(cx, |i, cx| {
            i.release([3], cx);
            i.retain([3], cx);
        });
        cx.executor().advance_clock(FLUSH_INTERVAL);
        cx.run_until_parked();
        assert_eq!(sent.borrow().len(), 3);
        assert!(!i.read_with(cx, |i, _| i.pending()), "no timer at rest");
    }

    #[gpui::test]
    fn a_failed_request_is_retried_with_the_next_change(cx: &mut TestAppContext) {
        let attempts = Rc::new(RefCell::new(0));
        let i = cx.new(|_| {
            let attempts = attempts.clone();
            PresenceInterest::new(Rc::new(move |_| {
                *attempts.borrow_mut() += 1;
                let fail = *attempts.borrow() == 1;
                Box::pin(async move { if fail { Err(UiError::not_running()) } else { Ok(()) } })
            }))
        });
        i.update(cx, |i, cx| i.retain([2], cx));
        cx.run_until_parked();
        cx.executor().advance_clock(FLUSH_INTERVAL);
        // 3 retained then released: the set is {2} again, but the last send failed.
        i.update(cx, |i, cx| {
            i.retain([3], cx);
            i.release([3], cx);
        });
        cx.executor().advance_clock(FLUSH_INTERVAL);
        cx.run_until_parked();
        assert_eq!(*attempts.borrow(), 2);
    }
}
