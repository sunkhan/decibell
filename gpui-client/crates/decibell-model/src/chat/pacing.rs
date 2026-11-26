//! Outbound message pacing (`features/chat/sendPacing.ts`).
//!
//! The community drops a CHANNEL_MSG once its per-session bucket is empty (10 burst, 3 per
//! second sustained, shared with edits) and answers CHANNEL_MSG_REJECTED. So the client
//! mirrors the bucket a little under the server's rate and *delays* instead: sends queue
//! per community (FIFO, so order holds) and go out no faster than the server admits. The
//! bubble is on screen already; the echo just arrives a beat later. The bucket starts full
//! whenever the community authenticates, as the server's does.
//!
//! **A token is spent when the packet leaves, not when the send is admitted.** A community's
//! message calls run one at a time (the core runs calls concurrently, and a burst admitted at
//! once raced to the socket out of order), so a slow call (a debug build's first send took
//! 180 ms) pushes every later packet back. Had the bucket been stamped at admission, the ten
//! of a burst would reach the server late and together while the eleventh, admitted 370 ms
//! after the burst *began*, followed them within the server's refill: refused. Here a call
//! starts only when the previous one of its community has returned (the packet handed to the
//! connection's writer) and the bucket, with every earlier send stamped at its return, holds a
//! token; the send's own token is stamped when it returns. Every stamp is then no earlier than
//! the server's consumption of that packet, so at each start the mirror holds no more tokens
//! than the server will when the packet arrives (it refills slower, from later stamps). The
//! renderer stamped at admission (its calls weren't chained).
//!
//! A timer exists only while a community has sends queued and its bucket is empty: the wait
//! for the next token.

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use gpui::{App, Context, Entity, Task};

use crate::ids::ServerId;

pub const BURST: f64 = 10.0;
pub const REFILL_PER_SEC: f64 = 2.7;
/// The legacy MOD_ACTION_RES(action "message") that follows a typed rejection is skipped
/// when it lands within this long of it.
pub const LEGACY_SUPPRESS: Duration = Duration::from_millis(1500);

/// A queued message call (a send, an edit): started once admitted, outside the pacer's
/// update. Its task ends when the call has returned, i.e. the packet was handed to the
/// connection (or the call failed): then the community's next call may start.
pub type PacedJob = Box<dyn FnOnce(&mut App) -> Task<()>>;

struct Pacer {
    tokens: f64,
    last: Instant,
    queue: VecDeque<PacedJob>,
    /// A call of this community is running: the next one waits for it to return.
    busy: bool,
    /// The wait for the next token: exists only while sends are queued on an empty bucket.
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
}

impl SendPacing {
    fn pacer(&mut self, server: ServerId, now: Instant) -> &mut Pacer {
        self.pacers.entry(server).or_insert_with(|| Pacer { tokens: BURST, last: now, queue: VecDeque::new(), busy: false, timer: None })
    }

    /// Queue `job` for `server`'s bucket; returns it (or an earlier one) when it may start now,
    /// for the caller to run after this update ([`pace`] does).
    pub fn enqueue(&mut self, server: ServerId, job: PacedJob, cx: &mut Context<Self>) -> Option<PacedJob> {
        let now = cx.background_executor().now();
        self.pacer(server, now).queue.push_back(job);
        self.next(server, cx)
    }

    /// The next job of `server` if it may start now: nothing running, a token in the bucket.
    /// With sends waiting on an empty bucket, the timer for the next token.
    fn next(&mut self, server: ServerId, cx: &mut Context<Self>) -> Option<PacedJob> {
        let now = cx.background_executor().now();
        let pacer = self.pacers.get_mut(&server)?;
        if pacer.busy || pacer.timer.is_some() {
            return None;
        }
        if pacer.queue.is_empty() {
            return None;
        }
        pacer.refill(now);
        if pacer.tokens >= 1.0 {
            pacer.busy = true;
            return pacer.queue.pop_front();
        }
        let wait_ms = ((1.0 - pacer.tokens) / REFILL_PER_SEC * 1000.0).ceil().max(10.0);
        let wait = Duration::from_millis(wait_ms as u64);
        pacer.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let Some(pacing) = this.upgrade() else { return };
            cx.update(|cx| {
                let job = pacing.update(cx, |this, cx| {
                    if let Some(p) = this.pacers.get_mut(&server) {
                        p.timer = None;
                    }
                    this.next(server, cx)
                });
                if let Some(job) = job {
                    start(&pacing, server, job, cx);
                }
            });
        }));
        None
    }

    /// The running call of `server` returned: its token is spent now (the packet has left),
    /// and the next job is returned when it may start.
    fn finished(&mut self, server: ServerId, cx: &mut Context<Self>) -> Option<PacedJob> {
        let now = cx.background_executor().now();
        let pacer = self.pacers.get_mut(&server)?;
        pacer.refill(now);
        pacer.tokens = (pacer.tokens - 1.0).max(0.0);
        pacer.busy = false;
        self.next(server, cx)
    }

    /// The community authenticated: its bucket is full again (`resetSendPacing`). Sends
    /// still queued go first, on the fresh bucket (after a call still running returns).
    pub fn reset(&mut self, server: ServerId, cx: &mut Context<Self>) -> Option<PacedJob> {
        let now = cx.background_executor().now();
        let pacer = self.pacers.get_mut(&server)?;
        pacer.tokens = BURST;
        pacer.last = now;
        if pacer.queue.is_empty() && !pacer.busy {
            self.pacers.remove(&server);
            return None;
        }
        pacer.timer = None;
        self.next(server, cx)
    }

    /// Sends waiting for a token or for the running call (tests, dumps).
    pub fn queued(&self, server: ServerId) -> usize {
        self.pacers.get(&server).map_or(0, |p| p.queue.len())
    }

    pub fn has_timer(&self, server: ServerId) -> bool {
        self.pacers.get(&server).is_some_and(|p| p.timer.is_some())
    }

    /// A call of `server`'s is running (tests).
    pub fn busy(&self, server: ServerId) -> bool {
        self.pacers.get(&server).is_some_and(|p| p.busy)
    }

    /// The tokens `server`'s bucket holds now (tests).
    pub fn tokens(&self, server: ServerId, now: Instant) -> f64 {
        self.pacers.get(&server).map_or(BURST, |p| {
            let elapsed = now.saturating_duration_since(p.last).as_secs_f64();
            (p.tokens + elapsed * REFILL_PER_SEC).min(BURST)
        })
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
    }
}

/// Start `job` and, when its call returned, spend its token and start the next one.
fn start(pacing: &Entity<SendPacing>, server: ServerId, job: PacedJob, cx: &mut App) {
    let call = job(cx);
    let pacing = pacing.downgrade();
    cx.spawn(async move |cx| {
        call.await;
        let Some(pacing) = pacing.upgrade() else { return };
        cx.update(|cx| {
            if let Some(job) = pacing.update(cx, |p, cx| p.finished(server, cx)) {
                start(&pacing, server, job, cx);
            }
        });
    })
    .detach();
}

/// Run `job` once `server`'s message bucket admits another send and its previous call has
/// returned (`paceSend`, plus the in-order chain). Jobs of one community run in order.
pub fn pace(pacing: &Entity<SendPacing>, server: ServerId, job: PacedJob, cx: &mut App) {
    if let Some(job) = pacing.update(cx, |p, cx| p.enqueue(server, job, cx)) {
        start(pacing, server, job, cx);
    }
}

/// `resetSendPacing`, with whatever the fresh bucket admits at once started after the update.
pub fn reset(pacing: &Entity<SendPacing>, server: ServerId, cx: &mut App) {
    if let Some(job) = pacing.update(cx, |p, cx| p.reset(server, cx)) {
        start(pacing, server, job, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::RefCell;
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    type Log = Rc<RefCell<Vec<(u32, Duration, Duration)>>>;

    /// A job whose call takes `delay` (on the test clock) and records (n, started, returned).
    fn job(n: u32, delay: Duration, log: &Log, start: Instant) -> PacedJob {
        let log = log.clone();
        Box::new(move |cx: &mut App| {
            let started = cx.background_executor().now() - start;
            cx.spawn(async move |cx| {
                if !delay.is_zero() {
                    cx.background_executor().timer(delay).await;
                }
                let returned = cx.background_executor().now() - start;
                log.borrow_mut().push((n, started, returned));
            })
        })
    }

    /// The community's bucket (`rate_bucket`: 10 burst, 3 per second), fed the packets at the
    /// times they reached it: the first refused one, if any.
    fn server_refuses(arrivals: &[Duration]) -> Option<usize> {
        let (mut tokens, mut last) = (10.0f64, Duration::ZERO);
        for (i, at) in arrivals.iter().enumerate() {
            tokens = (tokens + (*at - last).as_secs_f64() * 3.0).min(10.0);
            last = *at;
            if tokens < 1.0 {
                return Some(i);
            }
            tokens -= 1.0;
        }
        None
    }

    #[gpui::test]
    fn a_burst_is_paced_in_order_never_dropped(cx: &mut TestAppContext) {
        let pacing = cx.new(|_| SendPacing::default());
        let server = ServerId::from_central(1);
        let log: Log = Rc::default();
        let start = cx.executor().now();
        cx.update(|cx| {
            for n in 0..15u32 {
                pace(&pacing, server, job(n, Duration::ZERO, &log, start), cx);
            }
        });
        cx.run_until_parked();
        // Ten at once (one after another, each when the previous returned), five waiting on a timer.
        assert_eq!(log.borrow().len(), 10);
        pacing.read_with(cx, |p, _| {
            assert_eq!(p.queued(server), 5);
            assert!(p.has_timer(server));
        });
        // 2.7 tokens a second: the 11th after ~370 ms, the 15th after ~1.85 s.
        cx.executor().advance_clock(Duration::from_millis(371));
        cx.run_until_parked();
        assert_eq!(log.borrow().len(), 11);
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        let log = log.borrow();
        assert_eq!(log.iter().map(|(n, ..)| *n).collect::<Vec<_>>(), (0..15).collect::<Vec<_>>(), "FIFO");
        assert!(log[14].1 >= Duration::from_millis(1850) && log[14].1 < Duration::from_millis(1900), "{:?}", log[14].1);
        // Idle: no timer left.
        pacing.read_with(cx, |p, _| {
            assert_eq!(p.queued(server), 0);
            assert!(!p.has_timer(server));
            assert!(!p.busy(server));
        });
    }

    /// `composer-burst` in a debug build: the first send's call took 180 ms. Stamped at
    /// admission, the eleventh left at 370 ms, inside the server's refill after the ten that
    /// reached it together at ~180 ms (refused). Stamped when each call returns, it waits for
    /// a token counted from there.
    #[gpui::test]
    fn a_slow_first_send_pushes_the_bucket_back(cx: &mut TestAppContext) {
        let pacing = cx.new(|_| SendPacing::default());
        let server = ServerId::from_central(1);
        let log: Log = Rc::default();
        let start = cx.executor().now();
        cx.update(|cx| {
            for n in 0..25u32 {
                let delay = if n == 0 { Duration::from_millis(180) } else { Duration::from_millis(1) };
                pace(&pacing, server, job(n, delay, &log, start), cx);
            }
        });
        // Nothing but the first call before it returns: the rest wait for it (in order).
        cx.run_until_parked();
        assert!(log.borrow().is_empty());
        pacing.read_with(cx, |p, _| assert!(p.busy(server) && p.queued(server) == 24));
        cx.executor().advance_clock(Duration::from_secs(10));
        cx.run_until_parked();
        let log = log.borrow();
        assert_eq!(log.iter().map(|(n, ..)| *n).collect::<Vec<_>>(), (0..25).collect::<Vec<_>>(), "FIFO, none dropped");
        // A call starts only after the previous one returned.
        assert!(log.windows(2).all(|w| w[1].1 >= w[0].2), "{log:?}");
        // The burst left at ~180–190 ms; the eleventh waits a token counted from the tenth's
        // return, not from when the burst was admitted.
        assert!(log[9].2 >= Duration::from_millis(189), "{:?}", log[9]);
        assert!(log[10].1 >= log[9].2 + Duration::from_millis(350), "the 11th started at {:?}, the 10th returned at {:?}", log[10].1, log[9].2);
        // Each packet reaches the server when its call returns: never refused.
        let arrivals: Vec<Duration> = log.iter().map(|(_, _, returned)| *returned).collect();
        assert_eq!(server_refuses(&arrivals), None, "{arrivals:?}");
        // Even if a packet reached the server the moment its call started.
        let earliest: Vec<Duration> = log.iter().map(|(_, started, _)| *started).collect();
        assert_eq!(server_refuses(&earliest), None, "{earliest:?}");
        // Stamped at admission (the old rule), the same calls would have been refused.
        let admitted_old: Vec<Duration> = (0..25u64)
            .map(|n| if n < 10 { Duration::from_millis(180 + n) } else { Duration::from_millis(((n - 9) as f64 / REFILL_PER_SEC * 1000.).ceil() as u64) })
            .collect();
        let mut old = admitted_old.clone();
        old.sort();
        assert!(server_refuses(&old).is_some(), "the scenario reproduces the refusal: {old:?}");
        pacing.read_with(cx, |p, _| assert!(!p.busy(server) && !p.has_timer(server) && p.queued(server) == 0));
    }

    #[gpui::test]
    fn a_reset_refills_and_sends_what_was_queued(cx: &mut TestAppContext) {
        let pacing = cx.new(|_| SendPacing::default());
        let server = ServerId::from_central(1);
        let log: Log = Rc::default();
        let start = cx.executor().now();
        cx.update(|cx| {
            for n in 0..13 {
                pace(&pacing, server, job(n, Duration::ZERO, &log, start), cx);
            }
        });
        cx.run_until_parked();
        assert_eq!(log.borrow().len(), 10);
        cx.update(|cx| reset(&pacing, server, cx));
        cx.run_until_parked();
        assert_eq!(log.borrow().len(), 13);
        pacing.read_with(cx, |p, _| assert!(!p.has_timer(server)));
        // Another community's bucket is its own.
        let other = ServerId::from_central(2);
        cx.update(|cx| pace(&pacing, other, job(99, Duration::ZERO, &log, start), cx));
        cx.run_until_parked();
        assert_eq!(log.borrow().len(), 14);
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
