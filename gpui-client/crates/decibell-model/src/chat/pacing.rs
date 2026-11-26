//! Outbound message pacing (`features/chat/sendPacing.ts`).
//!
//! The community drops a CHANNEL_MSG once its per-session bucket is empty (10 burst, 3 per
//! second sustained, shared with edits) and answers CHANNEL_MSG_REJECTED. So the client
//! mirrors the bucket a little under the server's rate and *delays* instead: sends queue
//! per community (FIFO, so order holds) and go out no faster than the server admits. The
//! bubble is on screen already; the echo just arrives a beat later. The bucket starts full
//! whenever the community authenticates, as the server's does.
//!
//! A timer exists only while a community has sends queued: the wait for the next token.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use futures::future::{LocalBoxFuture, Shared};
use gpui::{App, Context, Task};

use crate::ids::ServerId;

pub const BURST: f64 = 10.0;
pub const REFILL_PER_SEC: f64 = 2.7;
/// The legacy MOD_ACTION_RES(action "message") that follows a typed rejection is skipped
/// when it lands within this long of it.
pub const LEGACY_SUPPRESS: Duration = Duration::from_millis(1500);

/// A queued send: runs once admitted, outside the pacer's update.
pub type PacedJob = Box<dyn FnOnce(&mut App)>;

struct Pacer {
    tokens: f64,
    last: Instant,
    queue: VecDeque<PacedJob>,
    /// The wait for the next token: exists only while sends are queued.
    timer: Option<Task<()>>,
}

impl Pacer {
    fn refill(&mut self, now: Instant) {
        let elapsed = now.saturating_duration_since(self.last).as_secs_f64();
        self.tokens = (self.tokens + elapsed * REFILL_PER_SEC).min(BURST);
        self.last = now;
    }
}

#[derive(Default)]
pub struct SendPacing {
    pacers: HashMap<ServerId, Pacer>,
    suppress_legacy_until: Option<Instant>,
    /// Per community, the last admitted send's call: the next one starts after it, so
    /// sends leave in the order they were made (the core runs calls concurrently).
    pub(crate) chains: HashMap<ServerId, Shared<LocalBoxFuture<'static, ()>>>,
}

impl SendPacing {
    /// Queue `job` for `server`'s bucket; returns the jobs admitted now, in order, for the
    /// caller to run after this update ([`pace`] does). The rest wait for the timer.
    pub fn enqueue(&mut self, server: ServerId, job: PacedJob, cx: &mut Context<Self>) -> Vec<PacedJob> {
        let now = cx.background_executor().now();
        let pacer = self.pacers.entry(server).or_insert_with(|| Pacer {
            tokens: BURST,
            last: now,
            queue: VecDeque::new(),
            timer: None,
        });
        pacer.queue.push_back(job);
        if pacer.timer.is_some() {
            // Draining already: the job waits its turn.
            return Vec::new();
        }
        self.drain(server, cx)
    }

    fn drain(&mut self, server: ServerId, cx: &mut Context<Self>) -> Vec<PacedJob> {
        let now = cx.background_executor().now();
        let Some(pacer) = self.pacers.get_mut(&server) else { return Vec::new() };
        pacer.refill(now);
        let mut ready = Vec::new();
        while pacer.tokens >= 1.0
            && let Some(job) = pacer.queue.pop_front()
        {
            pacer.tokens -= 1.0;
            ready.push(job);
        }
        if pacer.queue.is_empty() {
            pacer.timer = None;
            return ready;
        }
        let wait_ms = ((1.0 - pacer.tokens) / REFILL_PER_SEC * 1000.0).ceil().max(10.0);
        let wait = Duration::from_millis(wait_ms as u64);
        pacer.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let Ok(ready) = this.update(cx, |this, cx| {
                if let Some(p) = this.pacers.get_mut(&server) {
                    p.timer = None;
                }
                this.drain(server, cx)
            }) else {
                return;
            };
            cx.update(|cx| {
                for job in ready {
                    job(cx);
                }
            });
        }));
        ready
    }

    /// The community authenticated: its bucket is full again (`resetSendPacing`). Sends
    /// still queued go first, on the fresh bucket.
    pub fn reset(&mut self, server: ServerId, cx: &mut Context<Self>) -> Vec<PacedJob> {
        let Some(pacer) = self.pacers.get_mut(&server) else { return Vec::new() };
        pacer.tokens = BURST;
        pacer.last = cx.background_executor().now();
        if pacer.queue.is_empty() {
            self.pacers.remove(&server);
            return Vec::new();
        }
        pacer.timer = None;
        self.drain(server, cx)
    }

    /// Sends waiting for a token (tests, dumps).
    pub fn queued(&self, server: ServerId) -> usize {
        self.pacers.get(&server).map_or(0, |p| p.queue.len())
    }

    pub fn has_timer(&self, server: ServerId) -> bool {
        self.pacers.get(&server).is_some_and(|p| p.timer.is_some())
    }

    /// A typed CHANNEL_MSG_REJECTED was shown (`noteTypedRejection`).
    pub fn note_typed_rejection(&mut self, now: Instant) {
        self.suppress_legacy_until = Some(now + LEGACY_SUPPRESS);
    }

    /// `legacyRejectionSuppressed`: the legacy echo of a refusal already shown stays silent.
    pub fn legacy_rejection_suppressed(&self, now: Instant) -> bool {
        self.suppress_legacy_until.is_some_and(|until| now < until)
    }

    pub fn reset_for_logout(&mut self) {
        self.pacers.clear();
        self.suppress_legacy_until = None;
        self.chains.clear();
    }
}

/// Run `job` once `server`'s message bucket admits another send (`paceSend`). Jobs of one
/// community run in order.
pub fn pace(pacing: &gpui::Entity<SendPacing>, server: ServerId, job: PacedJob, cx: &mut App) {
    let ready = pacing.update(cx, |p, cx| p.enqueue(server, job, cx));
    for job in ready {
        job(cx);
    }
}

/// `resetSendPacing`, with whatever the fresh bucket admits at once run after the update.
pub fn reset(pacing: &gpui::Entity<SendPacing>, server: ServerId, cx: &mut App) {
    let ready = pacing.update(cx, |p, cx| p.reset(server, cx));
    for job in ready {
        job(cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    #[gpui::test]
    fn a_burst_is_paced_in_order_never_dropped(cx: &mut TestAppContext) {
        let pacing = cx.new(|_| SendPacing::default());
        let server = ServerId::from_central(1);
        let sent: Rc<RefCell<Vec<(u32, Duration)>>> = Rc::default();
        let start = cx.executor().now();
        cx.update(|cx| {
            for n in 0..15u32 {
                let (sent, executor) = (sent.clone(), cx.background_executor().clone());
                pace(&pacing, server, Box::new(move |_| sent.borrow_mut().push((n, executor.now() - start))), cx);
            }
        });
        // Ten at once, five waiting on a timer.
        assert_eq!(sent.borrow().len(), 10);
        pacing.read_with(cx, |p, _| {
            assert_eq!(p.queued(server), 5);
            assert!(p.has_timer(server));
        });
        // 2.7 tokens a second: the 11th after ~370 ms, the 15th after ~1.85 s.
        cx.executor().advance_clock(Duration::from_millis(371));
        cx.run_until_parked();
        assert_eq!(sent.borrow().len(), 11);
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        let sent = sent.borrow();
        assert_eq!(sent.iter().map(|(n, _)| *n).collect::<Vec<_>>(), (0..15).collect::<Vec<_>>(), "FIFO");
        assert!(sent[14].1 >= Duration::from_millis(1850) && sent[14].1 < Duration::from_millis(1900), "{:?}", sent[14].1);
        // Idle: no timer left.
        pacing.read_with(cx, |p, _| {
            assert_eq!(p.queued(server), 0);
            assert!(!p.has_timer(server));
        });
    }

    #[gpui::test]
    fn a_reset_refills_and_sends_what_was_queued(cx: &mut TestAppContext) {
        let pacing = cx.new(|_| SendPacing::default());
        let server = ServerId::from_central(1);
        let count = Rc::new(RefCell::new(0));
        cx.update(|cx| {
            for _ in 0..13 {
                let count = count.clone();
                pace(&pacing, server, Box::new(move |_| *count.borrow_mut() += 1), cx);
            }
        });
        assert_eq!(*count.borrow(), 10);
        cx.update(|cx| reset(&pacing, server, cx));
        assert_eq!(*count.borrow(), 13);
        pacing.read_with(cx, |p, _| assert!(!p.has_timer(server)));
        // Another community's bucket is its own.
        let other = ServerId::from_central(2);
        cx.update(|cx| {
            let count = count.clone();
            pace(&pacing, other, Box::new(move |_| *count.borrow_mut() += 1), cx);
        });
        assert_eq!(*count.borrow(), 14);
    }

    #[gpui::test]
    fn the_legacy_echo_is_suppressed_for_a_beat(cx: &mut TestAppContext) {
        let pacing = cx.new(|_| SendPacing::default());
        let now = cx.executor().now();
        pacing.update(cx, |p, _| {
            assert!(!p.legacy_rejection_suppressed(now));
            p.note_typed_rejection(now);
            assert!(p.legacy_rejection_suppressed(now + Duration::from_millis(1499)));
            assert!(!p.legacy_rejection_suppressed(now + LEGACY_SUPPRESS));
        });
    }
}
