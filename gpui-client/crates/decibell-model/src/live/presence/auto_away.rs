//! Auto-away (`features/presence/useAutoAway.ts`; spec `2026-10-11-user-status-design.md`):
//! Away after the user's threshold (Settings `away_after_minutes`, default 10, 0 = never)
//! without keyboard / mouse input or talking in voice, or at once on screen lock or
//! sleep; back on activity. Central applies it only while the chosen status is Online and
//! shows the account Away only while every device idles.
//!
//! The renderer polled the OS every 15 s while active and every second while away. Here
//! nothing polls. A source that **pushes** both edges (reaching the threshold and input
//! resuming: the compositor's `ext-idle-notify` through the core's watcher on Wayland, the X
//! server's SYNC alarms on X11) keeps no timer at all: an edge is either a reading itself
//! ([`AutoAway::pushed`], the core's `SystemIdleChanged`) or a hint to read again
//! ([`AutoAway::poke`], X11's alarms), so Away shows at the threshold and goes at the next
//! input. A source that can only be asked keeps **one** timer, armed from what it said:
//!
//! - **Active, idle for `d`** (a source that knows: X11's XScreenSaver, Windows'
//!   `GetLastInputInfo`): read again when the threshold can first be reached, `threshold − d`
//!   from now.
//! - **Active, duration unknown** (GNOME's IdleMonitor through the core, which answers only
//!   idle / active): read again one threshold from now; Away can then show up to one
//!   threshold late there.
//! - **Speaking** counts as input (the engine's own speaking flag for ourselves): the
//!   threshold also runs from the last speaking edge. When it holds Away off against an idle
//!   reading, the one timer re-reads when speech's own threshold runs out (on any source).
//! - **Away**: no timer when the source pushes (the app calls [`AutoAway::poke`] on its
//!   resume edge, on an unlock, and on input in our own window); otherwise a 1 s re-read while
//!   away, the renderer's rate, for the one named reason that nothing else can tell us the
//!   user is back.
//!
//! What the app implements: an [`IdleSource`] for the platform (the core's: `CoreIdleSource`,
//! Wayland and GNOME, "unsupported" elsewhere, where Electron used `powerMonitor`), lock /
//! sleep notifications ([`AutoAway::set_locked`]), and the pokes.

use std::rc::Rc;
use std::time::{Duration, Instant};

use futures::future::LocalBoxFuture;
use gpui::{App, Context, Task};

/// The re-read while Away on a source that can't announce resuming input.
pub const AWAY_RECHECK: Duration = Duration::from_secs(1);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IdleReading {
    Idle,
    Active { idle_for: Option<Duration> },
    Locked,
    /// No idle information on this platform: never Away (except on lock / sleep).
    Unsupported,
}

/// The platform's idle state (the app's; `actions::presence::CoreIdleSource` asks the
/// core). Read off the UI thread.
pub trait IdleSource: 'static {
    fn read(&self, threshold: Duration) -> LocalBoxFuture<'static, IdleReading>;
    /// The source announces both edges for the threshold it was last read with: reaching it
    /// and input resuming (as [`AutoAway::pushed`] readings or [`AutoAway::poke`] hints). No
    /// timer is kept then, while Away or active, except speech's.
    fn pushes_resume(&self) -> bool;
}

/// Reports a change of our idle flag (to the own-presence store and central's `set_idle`).
pub type IdleReport = Rc<dyn Fn(bool, &mut App)>;

pub struct AutoAway {
    minutes: u32,
    /// The central understands statuses.
    supported: bool,
    source: Option<Rc<dyn IdleSource>>,
    /// The screen is locked or the machine slept.
    locked: bool,
    /// We are speaking in a voice session.
    speaking: bool,
    /// The last speaking edge (start or stop).
    last_spoke: Option<Instant>,
    /// What we last reported.
    idle: bool,
    report: IdleReport,
    /// The one timer (see the module doc); none while disabled or waiting on a push.
    timer: Option<Task<()>>,
    /// A read in flight.
    reading: Option<Task<()>>,
}

impl AutoAway {
    pub fn new(report: IdleReport) -> Self {
        AutoAway {
            minutes: 0,
            supported: false,
            source: None,
            locked: false,
            speaking: false,
            last_spoke: None,
            idle: false,
            report,
            timer: None,
            reading: None,
        }
    }

    pub fn idle(&self) -> bool {
        self.idle
    }

    pub fn timer_armed(&self) -> bool {
        self.timer.is_some()
    }

    fn enabled(&self) -> bool {
        self.supported && self.minutes > 0
    }

    fn threshold(&self) -> Duration {
        Duration::from_secs(u64::from(self.minutes) * 60)
    }

    pub fn set_source(&mut self, source: Rc<dyn IdleSource>, cx: &mut Context<Self>) {
        self.source = Some(source);
        self.check(cx);
    }

    /// The threshold and whether central has statuses; called whenever either changes.
    pub fn configure(&mut self, minutes: u32, supported: bool, cx: &mut Context<Self>) {
        if (self.minutes, self.supported) == (minutes, supported) {
            return;
        }
        self.minutes = minutes;
        self.supported = supported;
        if !self.enabled() {
            self.timer = None;
            self.reading = None;
            self.locked = false;
            self.set_idle(false, cx);
            return;
        }
        self.check(cx);
    }

    /// Screen lock / sleep (on) or unlock / wake (off).
    pub fn set_locked(&mut self, locked: bool, cx: &mut Context<Self>) {
        self.locked = locked;
        if locked && self.enabled() {
            self.set_idle(true, cx);
        }
        self.check(cx);
    }

    /// Input resumed, the screen unlocked, or the user touched our window: read now.
    pub fn poke(&mut self, cx: &mut Context<Self>) {
        self.check(cx);
    }

    /// An edge the source pushed as a reading (the core's `SystemIdleChanged`: the
    /// compositor's `idled` / `resumed`): `idle` past `threshold` is Away at once (unless
    /// speech holds it off), resumed input is back at once. No read is made, and one in
    /// flight is dropped: it was answered before this edge. An edge for another threshold
    /// comes from a watcher being replaced: read afresh instead (the read sets the watcher to
    /// ours).
    pub fn pushed(&mut self, idle: bool, threshold: Duration, cx: &mut Context<Self>) {
        if !self.enabled() || self.source.is_none() {
            return;
        }
        if threshold != self.threshold() {
            self.check(cx);
            return;
        }
        self.timer = None;
        self.reading = None;
        let reading = if idle { IdleReading::Idle } else { IdleReading::Active { idle_for: Some(Duration::ZERO) } };
        let now = cx.background_executor().now();
        self.apply(reading, now, cx);
    }

    /// Our own speaking flag in a voice session or call changed.
    pub fn set_speaking(&mut self, speaking: bool, now: Instant, cx: &mut Context<Self>) {
        if self.speaking == speaking {
            return;
        }
        self.speaking = speaking;
        self.last_spoke = Some(now);
        // Talking brings an Away user back at once, as input does.
        if speaking && self.idle {
            self.check(cx);
        }
    }

    fn set_idle(&mut self, idle: bool, cx: &mut Context<Self>) {
        if self.idle != idle {
            self.idle = idle;
            cx.notify();
            let report = self.report.clone();
            cx.defer(move |cx| report(idle, cx));
        }
    }

    /// Read the source now and act on the answer.
    fn check(&mut self, cx: &mut Context<Self>) {
        self.timer = None;
        if !self.enabled() {
            return;
        }
        let Some(source) = self.source.clone() else {
            // No source (the headless tool): only lock / sleep make us Away.
            self.set_idle(self.locked, cx);
            return;
        };
        let reading = source.read(self.threshold());
        self.reading = Some(cx.spawn(async move |this, cx| {
            let reading = reading.await;
            let _ = this.update(cx, |this, cx| {
                this.reading = None;
                let now = cx.background_executor().now();
                this.apply(reading, now, cx);
            });
        }));
    }

    /// Decide from one reading and arm the next check.
    fn apply(&mut self, reading: IdleReading, now: Instant, cx: &mut Context<Self>) {
        if !self.enabled() {
            return;
        }
        let threshold = self.threshold();
        let spoke_until = self.last_spoke.map(|at| at + threshold);
        let spoke = self.speaking || spoke_until.is_some_and(|until| until > now);
        let idle = self.locked || reading == IdleReading::Locked || (reading == IdleReading::Idle && !spoke);
        self.set_idle(idle, cx);
        let pushes = self.source.as_ref().is_some_and(|s| s.pushes_resume());
        // Speech holds Away off until its own threshold runs out.
        let speech = spoke_until.map_or(Duration::ZERO, |u| u.saturating_duration_since(now));
        let wait = if idle {
            (!pushes).then_some(AWAY_RECHECK)
        } else if pushes {
            // The source announces the threshold itself: a timer only when speech held Away
            // off against an idle reading (nothing would announce its end). While still
            // speaking, its threshold runs from the stop edge: look again a threshold on.
            (reading == IdleReading::Idle)
                .then(|| if self.speaking { threshold } else { speech }.max(Duration::from_secs(1)))
        } else {
            match reading {
                IdleReading::Unsupported => None,
                IdleReading::Active { idle_for: Some(d) } => Some(threshold.saturating_sub(d)),
                _ => Some(threshold),
            }
            .map(|w| w.max(speech).max(Duration::from_secs(1)))
        };
        self.timer = wait.map(|wait| {
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| this.check(cx));
            })
        });
    }

    /// Sign-out.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        self.timer = None;
        self.reading = None;
        self.supported = false;
        self.locked = false;
        self.speaking = false;
        self.last_spoke = None;
        self.set_idle(false, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    /// A source whose answer the test sets, counting reads.
    struct Fake {
        answer: Rc<Cell<IdleReading>>,
        reads: Rc<Cell<u32>>,
        pushes: bool,
    }

    impl IdleSource for Fake {
        fn read(&self, _: Duration) -> LocalBoxFuture<'static, IdleReading> {
            self.reads.set(self.reads.get() + 1);
            let answer = self.answer.get();
            Box::pin(async move { answer })
        }
        fn pushes_resume(&self) -> bool {
            self.pushes
        }
    }

    struct Rig {
        away: gpui::Entity<AutoAway>,
        answer: Rc<Cell<IdleReading>>,
        reads: Rc<Cell<u32>>,
        reports: Rc<RefCell<Vec<bool>>>,
    }

    fn rig(cx: &mut TestAppContext, pushes: bool) -> Rig {
        let answer = Rc::new(Cell::new(IdleReading::Active { idle_for: Some(Duration::ZERO) }));
        let reads = Rc::new(Cell::new(0));
        let reports: Rc<RefCell<Vec<bool>>> = Rc::default();
        let away = cx.new(|_| {
            let reports = reports.clone();
            AutoAway::new(Rc::new(move |idle, _| reports.borrow_mut().push(idle)))
        });
        let source = Rc::new(Fake { answer: answer.clone(), reads: reads.clone(), pushes });
        away.update(cx, |a, cx| {
            a.set_source(source, cx);
            a.configure(10, true, cx);
        });
        cx.run_until_parked();
        Rig { away, answer, reads, reports }
    }

    const TEN_MIN: Duration = Duration::from_secs(600);

    /// An asked source, active with a known idle time: the next read lands exactly when the
    /// threshold can first be reached; one timer, no polling.
    #[gpui::test]
    fn the_timer_is_armed_at_the_threshold(cx: &mut TestAppContext) {
        let r = rig(cx, false);
        assert_eq!(r.reads.get(), 1);
        // 4 minutes idle at the next read: re-armed for 6 more.
        r.answer.set(IdleReading::Active { idle_for: Some(Duration::from_secs(240)) });
        cx.executor().advance_clock(TEN_MIN);
        cx.run_until_parked();
        assert_eq!(r.reads.get(), 2);
        cx.executor().advance_clock(Duration::from_secs(359));
        cx.run_until_parked();
        assert_eq!(r.reads.get(), 2, "nothing before the threshold can be reached");
        r.answer.set(IdleReading::Idle);
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(r.reads.get(), 3);
        assert_eq!(*r.reports.borrow(), vec![true]);
    }

    /// A source that pushes both edges (the core's Wayland watcher): no timer while active or
    /// Away, Away on the idle edge at once and back on the resume edge at once, with no read;
    /// an edge for another threshold (a replaced watcher's) is only a reason to read.
    #[gpui::test]
    fn a_pushing_source_keeps_no_timer(cx: &mut TestAppContext) {
        let r = rig(cx, true);
        assert_eq!(r.reads.get(), 1);
        assert!(!r.away.read_with(cx, |a, _| a.timer_armed()), "active on a pushing source: no timer");
        cx.executor().advance_clock(TEN_MIN * 6);
        cx.run_until_parked();
        assert_eq!(r.reads.get(), 1, "nothing read while active");
        r.away.update(cx, |a, cx| a.pushed(true, TEN_MIN, cx));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true], "Away at the idle edge");
        assert!(!r.away.read_with(cx, |a, _| a.timer_armed()), "Away on a pushing source: no timer");
        cx.executor().advance_clock(TEN_MIN * 6);
        cx.run_until_parked();
        assert_eq!(r.reads.get(), 1, "nothing read while Away");
        r.away.update(cx, |a, cx| a.pushed(false, TEN_MIN, cx));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true, false], "back at the resume edge");
        assert!(!r.away.read_with(cx, |a, _| a.timer_armed()));
        assert_eq!(r.reads.get(), 1);
        // A replaced watcher's edge: read instead of believing it.
        r.away.update(cx, |a, cx| a.pushed(true, TEN_MIN / 2, cx));
        cx.run_until_parked();
        assert_eq!(r.reads.get(), 2);
        assert_eq!(*r.reports.borrow(), vec![true, false], "the read says active");
        // Off: edges are ignored.
        r.away.update(cx, |a, cx| a.configure(0, true, cx));
        r.away.update(cx, |a, cx| a.pushed(true, TEN_MIN, cx));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true, false]);
    }

    /// A source whose reads wait until the test answers them.
    struct Pending {
        answers: Rc<RefCell<Vec<futures::channel::oneshot::Sender<IdleReading>>>>,
    }

    impl IdleSource for Pending {
        fn read(&self, _: Duration) -> LocalBoxFuture<'static, IdleReading> {
            let (tx, rx) = futures::channel::oneshot::channel();
            self.answers.borrow_mut().push(tx);
            Box::pin(async move { rx.await.unwrap_or(IdleReading::Unsupported) })
        }
        fn pushes_resume(&self) -> bool {
            true
        }
    }

    /// A read answered before an edge arrives after it: the edge wins.
    #[gpui::test]
    fn a_push_drops_a_read_in_flight(cx: &mut TestAppContext) {
        let answers: Rc<RefCell<Vec<futures::channel::oneshot::Sender<IdleReading>>>> = Rc::default();
        let reports: Rc<RefCell<Vec<bool>>> = Rc::default();
        let away = cx.new(|_| {
            let reports = reports.clone();
            AutoAway::new(Rc::new(move |idle, _| reports.borrow_mut().push(idle)))
        });
        away.update(cx, |a, cx| {
            a.set_source(Rc::new(Pending { answers: answers.clone() }), cx);
            a.configure(10, true, cx);
        });
        cx.run_until_parked();
        assert_eq!(answers.borrow().len(), 1);
        away.update(cx, |a, cx| a.pushed(true, TEN_MIN, cx));
        cx.run_until_parked();
        assert_eq!(*reports.borrow(), vec![true]);
        // The read made before the edge says "active": nobody is listening any more.
        let stale = answers.borrow_mut().remove(0);
        let _ = stale.send(IdleReading::Active { idle_for: None });
        cx.run_until_parked();
        assert_eq!(*reports.borrow(), vec![true]);
        assert!(away.read_with(cx, |a, _| a.idle()));
    }

    /// Speech holds a pushed idle edge off; the one timer then finds its end, as nothing else
    /// would announce it.
    #[gpui::test]
    fn speech_holds_a_pushed_idle_until_its_threshold(cx: &mut TestAppContext) {
        let r = rig(cx, true);
        let now = cx.executor().now();
        r.away.update(cx, |a, cx| a.set_speaking(true, now, cx));
        r.answer.set(IdleReading::Idle);
        r.away.update(cx, |a, cx| a.pushed(true, TEN_MIN, cx));
        cx.run_until_parked();
        assert!(r.reports.borrow().is_empty(), "talking: not Away");
        assert!(r.away.read_with(cx, |a, _| a.timer_armed()));
        // Speech stops a minute later; its threshold runs from there (11 minutes from the edge).
        cx.executor().advance_clock(Duration::from_secs(60));
        let now = cx.executor().now();
        r.away.update(cx, |a, cx| a.set_speaking(false, now, cx));
        cx.executor().advance_clock(TEN_MIN - Duration::from_secs(60));
        cx.run_until_parked();
        assert!(r.reports.borrow().is_empty(), "speech's threshold hasn't run out");
        cx.executor().advance_clock(Duration::from_secs(59));
        cx.run_until_parked();
        assert!(r.reports.borrow().is_empty());
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true], "Away when speech's threshold ran out");
        assert!(!r.away.read_with(cx, |a, _| a.timer_armed()));
    }

    /// A source that can't push: a 1 s re-read while Away, none while active beyond the
    /// threshold re-read.
    #[gpui::test]
    fn away_on_a_polling_source_rechecks_every_second(cx: &mut TestAppContext) {
        let r = rig(cx, false);
        r.answer.set(IdleReading::Idle);
        r.away.update(cx, |a, cx| a.poke(cx));
        cx.run_until_parked();
        let reads = r.reads.get();
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(r.reads.get(), reads + 3);
        r.answer.set(IdleReading::Active { idle_for: None });
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(r.reports.borrow().last(), Some(&false));
        let reads = r.reads.get();
        cx.executor().advance_clock(TEN_MIN - Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(r.reads.get(), reads, "unknown idle time: next read one threshold later");
    }

    /// Lock is Away at once; speaking keeps an idle user online and brings them back.
    #[gpui::test]
    fn lock_and_speech(cx: &mut TestAppContext) {
        let r = rig(cx, true);
        r.away.update(cx, |a, cx| a.set_locked(true, cx));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true]);
        r.away.update(cx, |a, cx| a.set_locked(false, cx));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true, false]);
        // Talking: the OS says idle, but speech within the threshold keeps us online.
        let now = cx.executor().now();
        r.away.update(cx, |a, cx| a.set_speaking(true, now, cx));
        r.answer.set(IdleReading::Idle);
        r.away.update(cx, |a, cx| a.poke(cx));
        cx.run_until_parked();
        assert_eq!(*r.reports.borrow(), vec![true, false]);
        // Disabled (threshold 0): never away, no timer.
        r.away.update(cx, |a, cx| a.configure(0, true, cx));
        cx.run_until_parked();
        assert!(!r.away.read_with(cx, |a, _| a.timer_armed() || a.idle()));
    }
}
