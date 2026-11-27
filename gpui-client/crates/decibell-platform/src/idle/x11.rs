//! Auto-away's idle source when GPUI runs on X11: an X11 session, or XWayland when the client was
//! started as an X11 client (GPUI picks Wayland whenever `WAYLAND_DISPLAY` is set and not empty, so
//! it is on X11 only when that variable is unset or empty, and then the core's Wayland reading has no
//! compositor socket either; `XDG_SESSION_TYPE` plays no part in either choice).
//!
//! **What it reads: the X server's own idle counter, nothing else.** The SYNC extension's `IDLETIME`
//! system counter is the milliseconds since the server last saw input from any device (the number it
//! keeps for its screen saver and DPMS). Two alarms on it, on a connection of our own:
//!
//! - *idle*: `PositiveComparison` at the user's threshold. The server sends `AlarmNotify` when the
//!   counter reaches it.
//! - *resume*: armed when the idle alarm fires, `NegativeComparison` one millisecond under the value
//!   the counter had then. The counter only grows while nothing happens, so the server sends
//!   `AlarmNotify` at the next input, when it drops to 0.
//!
//! Each fires once (a comparison alarm without a delta goes inactive) and re-arms the other, so the
//! thread blocked in `wait_for_event` wakes at those two edges and at nothing else: never per key or
//! pointer motion, never while nothing changes. Comparisons rather than transitions: an alarm armed
//! past its point (the threshold lowered under the current idle time, input between the idle alarm
//! and the resume alarm's arming) fires at once instead of never. KDE's KIdleTime watches X11 idle
//! the same way. Nothing here selects for key, pointer or any other input events, uses XInput2 (raw
//! or not), RECORD or a grab (`CLAUDE.md`, "Global hotkeys see bound keys only", in spirit): the
//! server counts, we read its counter and are told when it crosses a value.
//!
//! **Fallback**, for a server without SYNC or without the `IDLETIME` counter: MIT-SCREEN-SAVER's
//! `QueryInfo` (`ms_since_user_input`, the same number), asked once per reading and never on a timer
//! of ours. `AutoAway` then reads again when the threshold can first be reached (one timer, re-armed
//! for the remainder) and, while Away, every second: it has no other way to learn that input resumed
//! (as on Windows and in Electron). Neither extension: "unsupported" (Away on lock and sleep only).
//! The path a server took is logged once per start (`[idle] X11: …`).
//!
//! **The contract with `AutoAway`** (`decibell_model::live::presence::auto_away`): `read` answers
//! `Idle` or `Active { idle_for: Some(d) }` from a fresh read of the counter, on a background thread
//! (on the fallback the model's one timer lands where the threshold can first be reached); every
//! alarm is an [`Edge`] the app turns into `AutoAway::poke` (a fresh read: an edge is a hint, the
//! reading is the answer); `pushes_resume` is true while the alarms are watched (both edges come), so
//! the model keeps no timer, active or Away. Lock and sleep are `crate::session`'s. Across sleep the counter stands still (the server's
//! clock is monotonic) while the session watcher holds us Away; a locked screen counts on until the
//! unlock's typing.
//!
//! **Lifetime**: the connection and the thread exist only while auto-away is on. The first reading
//! connects; [`X11Idle::disable`] (threshold 0, or central without statuses, i.e. before sign-in)
//! destroys the alarms, which ends the thread, and drops the connection. A threshold change re-arms
//! the idle alarm. Under XWayland the counter sees the input Xwayland receives: X11 windows', plus
//! whatever the compositor forwards to it.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::thread::JoinHandle;
use std::time::Duration;

use decibell_model::live::presence::{IdleReading, IdleSource};
use futures::future::LocalBoxFuture;
use gpui::BackgroundExecutor;

/// The alarm thread's name (what `/proc/<pid>/task/*/comm` and `measure.sh` show).
pub(crate) const THREAD_NAME: &str = "x11-idle";

/// What an alarm said: a hint to read again (`AutoAway::poke`), never the reading itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Edge {
    /// The counter reached the threshold.
    Idle,
    /// Input after an idle edge.
    Active,
}

impl fmt::Display for Edge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Edge::Idle => "idle",
            Edge::Active => "active",
        })
    }
}

/// Where the alarm thread sends edges (the app: a channel to the UI thread).
pub(crate) type EdgeSink = Arc<dyn Fn(Edge) + Send + Sync>;

/// The paths a server can take, as the log and the self-test name them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Path {
    /// SYNC's `IDLETIME` alarms, the thread.
    Sync,
    /// MIT-SCREEN-SAVER's `QueryInfo` per reading, no thread.
    ScreenSaver,
    /// Neither: "unsupported".
    Unsupported,
}

impl fmt::Display for Path {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Path::Sync => "sync",
            Path::ScreenSaver => "screensaver",
            Path::Unsupported => "unsupported",
        })
    }
}

/// What woke the alarm thread.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Wake {
    /// The idle alarm fired; the counter's value then.
    Idle(Duration),
    /// The resume alarm fired: input.
    Resumed,
    /// An alarm was destroyed: we are stopping.
    Gone,
}

/// The idle time, read once (both paths).
pub(super) trait Query: Send + Sync {
    fn idle(&self) -> anyhow::Result<Duration>;
}

/// The SYNC path: the two alarms on `IDLETIME` (the seam the unit tests fake).
pub(super) trait Alarms: Send + Sync {
    fn idle(&self) -> anyhow::Result<Duration>;
    /// The idle alarm at `at` (active again).
    fn arm_idle(&self, at: Duration) -> anyhow::Result<()>;
    /// The resume alarm: fires once the counter is at or under `below`.
    fn arm_resume(&self, below: Duration) -> anyhow::Result<()>;
    /// Blocks until an alarm fires.
    fn wait(&self) -> anyhow::Result<Wake>;
    /// Destroys the alarms; `wait` then answers `Gone`.
    fn stop(&self);
}

pub(super) enum Backend {
    Alarms(Arc<dyn Alarms>),
    Query(Arc<dyn Query>),
    Unsupported,
}

/// What a connection found.
pub(super) struct Probe {
    pub backend: Backend,
    /// For the log: the server and why this path.
    pub about: String,
}

impl Probe {
    fn path(&self) -> Path {
        match self.backend {
            Backend::Alarms(_) => Path::Sync,
            Backend::Query(_) => Path::ScreenSaver,
            Backend::Unsupported => Path::Unsupported,
        }
    }
}

/// Connects and arms for the given threshold (blocking: background threads only).
type Connect = Box<dyn Fn(Duration) -> Probe + Send + Sync>;

/// Auto-away's idle source on X11 (the module doc).
pub(crate) struct X11Idle {
    shared: Arc<Shared>,
    executor: BackgroundExecutor,
}

impl X11Idle {
    /// The source for `display` (`None`: `$DISPLAY`, the server GPUI connected to). Connects only
    /// at the first reading.
    pub(crate) fn new(display: Option<String>, executor: BackgroundExecutor, edges: EdgeSink) -> Self {
        Self::with_connect(Box::new(move |threshold| server::connect(display.as_deref(), threshold, true)), executor, edges)
    }

    /// The fallback on a server that has SYNC (no X server lets SYNC be switched off, Xvfb's
    /// `-extension SYNC` included): the self-test's way to run MIT-SCREEN-SAVER's path for real.
    #[cfg(feature = "dev-local")]
    pub(crate) fn without_sync(display: String, executor: BackgroundExecutor, edges: EdgeSink) -> Self {
        Self::with_connect(Box::new(move |threshold| server::connect(Some(&display), threshold, false)), executor, edges)
    }

    fn with_connect(connect: Connect, executor: BackgroundExecutor, edges: EdgeSink) -> Self {
        X11Idle { shared: Arc::new(Shared::new(connect, edges)), executor }
    }

    /// Auto-away is off: the alarms, the thread and the connection go (on a background thread).
    pub(crate) fn disable(&self) {
        if self.shared.want.swap(0, Ordering::SeqCst) != 0 {
            let shared = self.shared.clone();
            self.executor.spawn(async move { shared.reconcile_now() }).detach();
        }
    }

    /// The path the server took (`None` before the first reading or while off).
    #[cfg(feature = "dev-local")]
    pub(crate) fn path(&self) -> Option<Path> {
        self.shared.io().path()
    }
}

impl IdleSource for X11Idle {
    fn read(&self, threshold: Duration) -> LocalBoxFuture<'static, IdleReading> {
        // At least 1 ms: 0 means off. (`AutoAway` asks in whole minutes.)
        let ms = u64::try_from(threshold.as_millis()).unwrap_or(u64::MAX).max(1);
        self.shared.want.store(ms, Ordering::SeqCst);
        let shared = self.shared.clone();
        Box::pin(self.executor.spawn(async move { shared.read(threshold) }))
    }

    fn pushes_resume(&self) -> bool {
        self.shared.pushing.load(Ordering::SeqCst) != 0
    }
}

/// What both threads share. `want` is set on the UI thread; everything that talks to the server runs
/// on background threads under `io`, one at a time, each bringing the server to the latest `want`.
struct Shared {
    connect: Connect,
    /// What `AutoAway` asked for last, in ms; 0 = off.
    want: AtomicU64,
    io: Mutex<Io>,
    /// The running alarm thread's generation (0: none): `pushes_resume`.
    pushing: Arc<AtomicU64>,
    generations: AtomicU64,
    edges: EdgeSink,
}

enum Io {
    Off,
    Watching {
        alarms: Arc<dyn Alarms>,
        /// Where the idle alarm is (the thread re-arms it there after each resume).
        threshold: Arc<Mutex<Duration>>,
        thread: JoinHandle<()>,
        generation: u64,
    },
    Querying(Arc<dyn Query>),
    /// Neither extension: not asked again.
    Unsupported,
}

impl Io {
    #[cfg_attr(not(feature = "dev-local"), allow(dead_code))]
    fn path(&self) -> Option<Path> {
        match self {
            Io::Off => None,
            Io::Watching { .. } => Some(Path::Sync),
            Io::Querying(_) => Some(Path::ScreenSaver),
            Io::Unsupported => Some(Path::Unsupported),
        }
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

impl Shared {
    fn new(connect: Connect, edges: EdgeSink) -> Self {
        Shared {
            connect,
            want: AtomicU64::new(0),
            io: Mutex::new(Io::Off),
            pushing: Arc::new(AtomicU64::new(0)),
            generations: AtomicU64::new(0),
            edges,
        }
    }

    fn io(&self) -> MutexGuard<'_, Io> {
        lock(&self.io)
    }

    fn want(&self) -> Option<Duration> {
        match self.want.load(Ordering::SeqCst) {
            0 => None,
            ms => Some(Duration::from_millis(ms)),
        }
    }

    fn reconcile_now(&self) {
        let mut io = self.io();
        self.reconcile(&mut io);
    }

    /// Brings the server to `want`: starts, re-arms or stops. Blocking (connects, round trips).
    fn reconcile(&self, io: &mut Io) {
        if let Io::Watching { thread, .. } = io
            && thread.is_finished()
        {
            // The connection broke (the thread logged why): start again below if still wanted.
            *io = Io::Off;
        }
        match (self.want(), &mut *io) {
            (None, Io::Watching { .. }) => {
                if let Io::Watching { alarms, generation, .. } = std::mem::replace(io, Io::Off) {
                    let _ = self.pushing.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
                    // The thread wakes on the alarms' destruction and ends; the connection goes with
                    // the last handle (the thread's).
                    alarms.stop();
                    crate::marker(format_args!("idle x11 stopped"));
                }
            }
            (None, Io::Querying(_)) => *io = Io::Off,
            (None, Io::Off | Io::Unsupported) | (Some(_), Io::Querying(_) | Io::Unsupported) => {}
            (Some(threshold), Io::Off) => *io = self.start(threshold),
            (Some(want), Io::Watching { alarms, threshold, .. }) => {
                // Under the lock the thread re-arms with, so the server gets the newest value last.
                let mut armed = lock(threshold);
                if *armed != want {
                    *armed = want;
                    match alarms.arm_idle(want) {
                        Ok(()) => crate::marker(format_args!("idle x11 threshold {} ms", want.as_millis())),
                        Err(e) => log::warn!("[idle] re-arming the X11 idle alarm: {e:#}"),
                    }
                }
            }
        }
    }

    fn start(&self, threshold: Duration) -> Io {
        let probe = (self.connect)(threshold);
        log::info!("[idle] X11: {}", probe.about);
        crate::marker(format_args!("idle x11 path {}: {}", probe.path(), probe.about));
        match probe.backend {
            Backend::Alarms(alarms) => {
                let generation = self.generations.fetch_add(1, Ordering::SeqCst) + 1;
                let armed = Arc::new(Mutex::new(threshold));
                // Set before the thread exists, so its own exit is what clears it.
                self.pushing.store(generation, Ordering::SeqCst);
                let spawned = {
                    let (alarms, armed, edges, pushing) = (alarms.clone(), armed.clone(), self.edges.clone(), self.pushing.clone());
                    std::thread::Builder::new().name(THREAD_NAME.into()).spawn(move || {
                        watch(&*alarms, &armed, &*edges);
                        let _ = pushing.compare_exchange(generation, 0, Ordering::SeqCst, Ordering::SeqCst);
                    })
                };
                match spawned {
                    Ok(thread) => {
                        crate::marker(format_args!("idle x11 watching"));
                        Io::Watching { alarms, threshold: armed, thread, generation }
                    }
                    Err(e) => {
                        log::warn!("[idle] no X11 idle thread: {e}");
                        self.pushing.store(0, Ordering::SeqCst);
                        alarms.stop();
                        Io::Unsupported
                    }
                }
            }
            Backend::Query(query) => Io::Querying(query),
            Backend::Unsupported => Io::Unsupported,
        }
    }

    /// One reading for `AutoAway`, against its threshold.
    fn read(&self, threshold: Duration) -> IdleReading {
        let mut io = self.io();
        self.reconcile(&mut io);
        let idle = match &*io {
            Io::Watching { alarms, .. } => alarms.idle(),
            Io::Querying(query) => query.idle(),
            Io::Off | Io::Unsupported => return IdleReading::Unsupported,
        };
        match idle {
            Ok(idle) if idle >= threshold => IdleReading::Idle,
            Ok(idle) => IdleReading::Active { idle_for: Some(idle) },
            Err(e) => {
                log::warn!("[idle] reading the X server's idle time: {e:#}");
                IdleReading::Unsupported
            }
        }
    }
}

impl Drop for Shared {
    fn drop(&mut self) {
        if let Io::Watching { alarms, .. } = std::mem::replace(&mut *self.io(), Io::Off) {
            alarms.stop();
        }
    }
}

/// The alarm thread: blocked in `wait` from one edge to the next.
fn watch(alarms: &dyn Alarms, threshold: &Mutex<Duration>, edges: &(dyn Fn(Edge) + Send + Sync)) {
    loop {
        let step = alarms.wait().and_then(|wake| step(alarms, threshold, wake));
        match step {
            Ok(Some(edge)) => edges(edge),
            Ok(None) => return,
            Err(e) => {
                log::warn!("[idle] X11 idle alarms: {e:#}");
                return;
            }
        }
    }
}

/// What one wake does: arm the other alarm, then tell the app.
fn step(alarms: &dyn Alarms, threshold: &Mutex<Duration>, wake: Wake) -> anyhow::Result<Option<Edge>> {
    match wake {
        Wake::Idle(counter) => {
            // Just under where the counter stood: only input brings it there.
            alarms.arm_resume(counter.saturating_sub(Duration::from_millis(1)))?;
            Ok(Some(Edge::Idle))
        }
        Wake::Resumed => {
            let at = lock(threshold);
            alarms.arm_idle(*at)?;
            Ok(Some(Edge::Active))
        }
        Wake::Gone => Ok(None),
    }
}

/// The X server side, through x11rb on a connection of our own.
mod server {
    use std::sync::Arc;
    use std::time::Duration;

    use anyhow::Context as _;
    use x11rb::connection::{Connection as _, RequestConnection as _};
    use x11rb::protocol::Event;
    use x11rb::protocol::screensaver::{self, ConnectionExt as _};
    use x11rb::protocol::sync::{self, ConnectionExt as _};
    use x11rb::rust_connection::RustConnection;

    use super::{Alarms, Backend, Probe, Query, Wake};

    /// Connects to `display` and picks the path: SYNC's `IDLETIME` alarms (unless `sync` is false),
    /// else MIT-SCREEN-SAVER's `QueryInfo`, else none.
    pub(super) fn connect(display: Option<&str>, threshold: Duration, sync: bool) -> Probe {
        let (conn, screen) = match x11rb::connect(display) {
            Ok(connected) => connected,
            Err(e) => return Probe { backend: Backend::Unsupported, about: format!("no connection to the X server ({e}): no idle time") },
        };
        let conn = Arc::new(conn);
        let server = {
            let setup = conn.setup();
            format!("{} {}", String::from_utf8_lossy(&setup.vendor), setup.release_number)
        };
        let why_not_sync = if sync {
            match SyncAlarms::new(conn.clone(), threshold) {
                Ok(alarms) => {
                    return Probe {
                        backend: Backend::Alarms(Arc::new(alarms)),
                        about: format!("SYNC IDLETIME alarms ({server}): a thread woken only when the threshold is reached and at the next input"),
                    };
                }
                Err(e) => format!("{e:#}"),
            }
        } else {
            "SYNC not used (test)".to_string()
        };
        match ScreenSaver::new(conn, screen) {
            Ok(query) => Probe {
                backend: Backend::Query(Arc::new(query)),
                about: format!("{why_not_sync}; MIT-SCREEN-SAVER QueryInfo instead, read when the threshold can first be reached and every second while Away ({server})"),
            },
            Err(e) => Probe { backend: Backend::Unsupported, about: format!("{why_not_sync}; {e:#}: no idle time ({server})") },
        }
    }

    fn int64(value: i64) -> sync::Int64 {
        sync::Int64 { hi: (value >> 32) as i32, lo: value as u32 }
    }

    fn millis(value: sync::Int64) -> Duration {
        let value = (i64::from(value.hi) << 32) | i64::from(value.lo);
        Duration::from_millis(value.max(0) as u64)
    }

    fn ms(duration: Duration) -> i64 {
        i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
    }

    struct SyncAlarms {
        conn: Arc<RustConnection>,
        counter: sync::Counter,
        idle: sync::Alarm,
        resume: sync::Alarm,
    }

    impl SyncAlarms {
        fn new(conn: Arc<RustConnection>, threshold: Duration) -> anyhow::Result<SyncAlarms> {
            conn.extension_information(sync::X11_EXTENSION_NAME)?.context("no SYNC extension")?;
            let version = conn.sync_initialize(3, 1)?.reply()?;
            let counters = conn.sync_list_system_counters()?.reply()?.counters;
            let counter = counters
                .iter()
                .find(|c| c.name == b"IDLETIME")
                .map(|c| c.counter)
                .with_context(|| format!("SYNC {}.{} has no IDLETIME counter", version.major_version, version.minor_version))?;
            let alarm = |test: sync::TESTTYPE, value: i64| -> anyhow::Result<sync::Alarm> {
                let id = conn.generate_id()?;
                let aux = sync::CreateAlarmAux::new()
                    .counter(counter)
                    .value_type(sync::VALUETYPE::ABSOLUTE)
                    .value(int64(value))
                    .test_type(test)
                    .delta(int64(0))
                    .events(1);
                conn.sync_create_alarm(id, &aux)?.check()?;
                Ok(id)
            };
            let idle = alarm(sync::TESTTYPE::POSITIVE_COMPARISON, ms(threshold))?;
            // Parked under anything the counter can read until the idle alarm arms it.
            let resume = alarm(sync::TESTTYPE::NEGATIVE_COMPARISON, -1)?;
            Ok(SyncAlarms { conn, counter, idle, resume })
        }

        fn change(&self, alarm: sync::Alarm, value: i64) -> anyhow::Result<()> {
            // A change makes the alarm active again (an X error would come to `wait`).
            self.conn.sync_change_alarm(alarm, &sync::ChangeAlarmAux::new().value(int64(value)))?;
            self.conn.flush()?;
            Ok(())
        }
    }

    impl Alarms for SyncAlarms {
        fn idle(&self) -> anyhow::Result<Duration> {
            Ok(millis(self.conn.sync_query_counter(self.counter)?.reply()?.counter_value))
        }

        fn arm_idle(&self, at: Duration) -> anyhow::Result<()> {
            self.change(self.idle, ms(at))
        }

        fn arm_resume(&self, below: Duration) -> anyhow::Result<()> {
            self.change(self.resume, ms(below))
        }

        fn wait(&self) -> anyhow::Result<Wake> {
            loop {
                match self.conn.wait_for_event()? {
                    Event::SyncAlarmNotify(e) if e.state == sync::ALARMSTATE::DESTROYED => return Ok(Wake::Gone),
                    Event::SyncAlarmNotify(e) if e.alarm == self.idle => return Ok(Wake::Idle(millis(e.counter_value))),
                    Event::SyncAlarmNotify(e) if e.alarm == self.resume => return Ok(Wake::Resumed),
                    Event::Error(e) => log::debug!("[idle] X11 error on the idle connection: {e:?}"),
                    _ => {}
                }
            }
        }

        fn stop(&self) {
            // Each destroyed alarm sends AlarmNotify(Destroyed): the thread's `wait` returns.
            let _ = self.conn.sync_destroy_alarm(self.idle);
            let _ = self.conn.sync_destroy_alarm(self.resume);
            let _ = self.conn.flush();
        }
    }

    struct ScreenSaver {
        conn: Arc<RustConnection>,
        root: u32,
    }

    impl ScreenSaver {
        fn new(conn: Arc<RustConnection>, screen: usize) -> anyhow::Result<ScreenSaver> {
            conn.extension_information(screensaver::X11_EXTENSION_NAME)?.context("no MIT-SCREEN-SAVER extension")?;
            conn.screensaver_query_version(1, 1)?.reply()?;
            let root = conn.setup().roots.get(screen).context("no such screen")?.root;
            let query = ScreenSaver { conn, root };
            query.idle()?;
            Ok(query)
        }
    }

    impl Query for ScreenSaver {
        fn idle(&self) -> anyhow::Result<Duration> {
            let info = self.conn.screensaver_query_info(self.root)?.reply()?;
            Ok(Duration::from_millis(info.ms_since_user_input.into()))
        }
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn int64_round_trips() {
            for v in [0i64, 1, 999, 2_000, 600_000, (1 << 32) + 5, i64::MAX / 2] {
                assert_eq!(millis(int64(v)), Duration::from_millis(v as u64));
            }
            let parked = int64(-1);
            assert_eq!((parked.hi, parked.lo), (-1, u32::MAX));
            assert_eq!(millis(int64(-1)), Duration::ZERO);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::collections::VecDeque;
    use std::sync::Condvar;
    use std::sync::mpsc;
    use std::time::Instant;

    use super::*;

    /// An X server's `IDLETIME` and the two comparison alarms, on a clock the test moves.
    #[derive(Default)]
    struct Server {
        state: Mutex<State>,
        changed: Condvar,
    }

    #[derive(Default)]
    struct State {
        now: u64,
        last_input: u64,
        /// The idle alarm: `PositiveComparison` (fires when counter ≥ value).
        idle_at: Option<u64>,
        /// The resume alarm: `NegativeComparison` (fires when counter ≤ value).
        resume_at: Option<i64>,
        queue: VecDeque<Wake>,
        arms: Vec<String>,
        stops: u32,
        reads: u32,
    }

    impl State {
        fn counter(&self) -> u64 {
            self.now - self.last_input
        }

        /// The server's check after anything changes; a comparison alarm fires once, then goes
        /// inactive until changed.
        fn evaluate(&mut self) {
            let counter = self.counter();
            if self.idle_at.is_some_and(|at| counter >= at) {
                self.idle_at = None;
                self.queue.push_back(Wake::Idle(Duration::from_millis(counter)));
            }
            if self.resume_at.is_some_and(|at| counter as i64 <= at) {
                self.resume_at = None;
                self.queue.push_back(Wake::Resumed);
            }
        }
    }

    impl Server {
        fn with<R>(&self, f: impl FnOnce(&mut State) -> R) -> R {
            let mut state = lock(&self.state);
            let r = f(&mut state);
            state.evaluate();
            self.changed.notify_all();
            r
        }

        fn advance(&self, ms: u64) {
            // A millisecond at a time, as the server's own wakeups land on the alarm's value.
            for _ in 0..ms {
                self.with(|s| s.now += 1);
            }
        }

        fn input(&self) {
            self.with(|s| s.last_input = s.now);
        }

        fn arms(&self) -> Vec<String> {
            std::mem::take(&mut lock(&self.state).arms)
        }
    }

    impl Alarms for Server {
        fn idle(&self) -> anyhow::Result<Duration> {
            let mut s = lock(&self.state);
            s.reads += 1;
            Ok(Duration::from_millis(s.counter()))
        }
        fn arm_idle(&self, at: Duration) -> anyhow::Result<()> {
            self.with(|s| {
                s.arms.push(format!("idle {}", at.as_millis()));
                s.idle_at = Some(at.as_millis() as u64);
            });
            Ok(())
        }
        fn arm_resume(&self, below: Duration) -> anyhow::Result<()> {
            self.with(|s| {
                s.arms.push(format!("resume {}", below.as_millis()));
                s.resume_at = Some(below.as_millis() as i64);
            });
            Ok(())
        }
        fn wait(&self) -> anyhow::Result<Wake> {
            let mut s = lock(&self.state);
            loop {
                if let Some(wake) = s.queue.pop_front() {
                    return Ok(wake);
                }
                s = self.changed.wait(s).unwrap_or_else(PoisonError::into_inner);
            }
        }
        fn stop(&self) {
            self.with(|s| {
                s.stops += 1;
                s.idle_at = None;
                s.resume_at = None;
                s.queue.push_back(Wake::Gone);
            });
        }
    }

    /// Created as the real one is: the idle alarm at the threshold, the resume alarm parked.
    fn created(server: &Arc<Server>, threshold: Duration) -> Arc<Server> {
        server.with(|s| {
            s.idle_at = Some(threshold.as_millis() as u64);
            s.resume_at = Some(-1);
            s.queue.clear();
        });
        server.clone()
    }

    struct Rig {
        server: Arc<Server>,
        shared: Arc<Shared>,
        edges: mpsc::Receiver<Edge>,
        connects: Arc<AtomicU64>,
    }

    fn rig(backend: &'static str) -> Rig {
        let server = Arc::new(Server::default());
        let connects = Arc::new(AtomicU64::new(0));
        let connect: Connect = {
            let (server, connects) = (server.clone(), connects.clone());
            Box::new(move |threshold| {
                connects.fetch_add(1, Ordering::SeqCst);
                let backend = match backend {
                    "sync" => Backend::Alarms(created(&server, threshold)),
                    "query" => Backend::Query(Arc::new(Fallback(server.clone()))),
                    _ => Backend::Unsupported,
                };
                Probe { backend, about: "fake".into() }
            })
        };
        let (tx, edges) = mpsc::channel();
        let tx = Mutex::new(tx);
        let sink: EdgeSink = Arc::new(move |edge| {
            let _ = lock(&tx).send(edge);
        });
        Rig { server, shared: Arc::new(Shared::new(connect, sink)), edges, connects }
    }

    /// MIT-SCREEN-SAVER's answer from the same clock.
    struct Fallback(Arc<Server>);

    impl Query for Fallback {
        fn idle(&self) -> anyhow::Result<Duration> {
            Alarms::idle(&*self.0)
        }
    }

    const T: Duration = Duration::from_millis(2_000);

    impl Rig {
        fn read(&self, threshold: Duration) -> IdleReading {
            self.shared.want.store(threshold.as_millis() as u64, Ordering::SeqCst);
            self.shared.read(threshold)
        }

        fn disable(&self) {
            self.shared.want.store(0, Ordering::SeqCst);
            self.shared.reconcile_now();
        }

        fn edge(&self) -> Option<Edge> {
            self.edges.recv_timeout(Duration::from_secs(2)).ok()
        }

        fn no_edge(&self) {
            assert_eq!(self.edges.recv_timeout(Duration::from_millis(50)).ok(), None);
        }

        /// The alarm thread ran to its end (it clears `pushing` last; a kept handle says finished).
        fn thread_ended(&self) -> bool {
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(2) {
                let finished = match &*self.shared.io() {
                    Io::Watching { thread, .. } => thread.is_finished(),
                    _ => true,
                };
                if finished && self.shared.pushing.load(Ordering::SeqCst) == 0 {
                    return true;
                }
                std::thread::sleep(Duration::from_millis(1));
            }
            false
        }
    }

    /// One wake: the other alarm armed, one edge.
    #[test]
    fn a_wake_arms_the_other_alarm() {
        let server = Arc::new(Server::default());
        let threshold = Mutex::new(T);
        assert_eq!(step(&*server, &threshold, Wake::Idle(Duration::from_millis(2_003))).unwrap(), Some(Edge::Idle));
        assert_eq!(server.arms(), ["resume 2002"]);
        assert_eq!(step(&*server, &threshold, Wake::Resumed).unwrap(), Some(Edge::Active));
        assert_eq!(server.arms(), ["idle 2000"]);
        assert_eq!(step(&*server, &threshold, Wake::Gone).unwrap(), None);
        assert!(server.arms().is_empty());
    }

    /// Idle exactly at the threshold, active at the next input, idle again a threshold later; the
    /// thread wakes at those edges only, and readings are fresh reads of the counter.
    #[test]
    fn edges_at_the_threshold_and_at_input() {
        let r = rig("sync");
        assert_eq!(r.read(T), IdleReading::Active { idle_for: Some(Duration::ZERO) });
        assert_ne!(r.shared.pushing.load(Ordering::SeqCst), 0, "the SYNC path pushes resumes");
        r.server.advance(1_999);
        r.no_edge();
        r.server.advance(1);
        assert_eq!(r.edge(), Some(Edge::Idle));
        assert_eq!(r.read(T), IdleReading::Idle);
        // Nothing more while idle, however long.
        r.server.advance(5_000);
        r.no_edge();
        r.server.input();
        assert_eq!(r.edge(), Some(Edge::Active));
        assert_eq!(r.read(T), IdleReading::Active { idle_for: Some(Duration::ZERO) });
        // Re-armed: idle again one threshold after the input, not before.
        r.server.advance(1_500);
        r.no_edge();
        r.server.input();
        r.no_edge();
        r.server.advance(2_000);
        assert_eq!(r.edge(), Some(Edge::Idle));
        assert_eq!(r.connects.load(Ordering::SeqCst), 1);
        r.disable();
    }

    /// Input between the idle alarm and the resume alarm's arming still brings an active edge
    /// (comparison alarms fire at once when already past).
    #[test]
    fn input_racing_the_resume_alarm_is_not_lost() {
        let server = Arc::new(Server::default());
        let threshold = Mutex::new(T);
        created(&server, T);
        server.advance(2_000);
        assert_eq!(server.wait().unwrap(), Wake::Idle(T));
        server.input();
        assert_eq!(step(&*server, &threshold, Wake::Idle(T)).unwrap(), Some(Edge::Idle));
        assert_eq!(server.wait().unwrap(), Wake::Resumed);
    }

    /// Lowered under the current idle time: idle at once. Raised while idle: active by the
    /// reading, idle again at the new threshold.
    #[test]
    fn threshold_changes_rearm() {
        let r = rig("sync");
        r.read(Duration::from_millis(4_000));
        r.server.advance(2_500);
        r.no_edge();
        assert_eq!(r.read(T), IdleReading::Idle);
        assert_eq!(r.edge(), Some(Edge::Idle));
        assert_eq!(r.server.arms(), ["idle 2000", "resume 2499"]);
        assert_eq!(r.read(Duration::from_millis(5_000)), IdleReading::Active { idle_for: Some(Duration::from_millis(2_500)) });
        r.server.advance(2_499);
        r.no_edge();
        r.server.advance(1);
        assert_eq!(r.edge(), Some(Edge::Idle));
        // Input: active, and the idle alarm back at the new threshold.
        r.server.input();
        assert_eq!(r.edge(), Some(Edge::Active));
        assert_eq!(r.server.arms().last().map(String::as_str), Some("idle 5000"));
        // The same threshold again re-arms nothing.
        r.read(Duration::from_millis(5_000));
        assert!(r.server.arms().is_empty());
        r.disable();
    }

    /// Off: the alarms are destroyed, the thread ends, nothing pushes; on again: a new connection
    /// and thread.
    #[test]
    fn disable_ends_the_thread_and_enable_starts_one() {
        let r = rig("sync");
        r.read(T);
        assert!(matches!(*r.shared.io(), Io::Watching { .. }));
        r.disable();
        assert!(r.thread_ended(), "the thread ended");
        assert!(matches!(*r.shared.io(), Io::Off));
        assert_eq!(lock(&r.server.state).stops, 1);
        // Disabling again is nothing.
        r.disable();
        assert_eq!(lock(&r.server.state).stops, 1);
        r.read(T);
        assert_eq!(r.connects.load(Ordering::SeqCst), 2);
        assert_ne!(r.shared.pushing.load(Ordering::SeqCst), 0);
        r.server.advance(2_000);
        assert_eq!(r.edge(), Some(Edge::Idle));
        r.disable();
        assert!(r.thread_ended());
    }

    /// A thread that died (connection lost) is started again at the next reading.
    #[test]
    fn a_dead_thread_is_restarted() {
        let r = rig("sync");
        r.read(T);
        // As if the server destroyed our alarms.
        lock(&r.server.state).queue.push_back(Wake::Gone);
        r.server.changed.notify_all();
        assert!(r.thread_ended());
        assert_eq!(r.shared.pushing.load(Ordering::SeqCst), 0, "a dead thread pushes nothing: AutoAway re-reads while Away");
        r.read(T);
        assert_eq!(r.connects.load(Ordering::SeqCst), 2);
        assert_ne!(r.shared.pushing.load(Ordering::SeqCst), 0);
        r.disable();
        assert!(r.thread_ended());
    }

    /// The fallback: a fresh QueryInfo per reading with the idle time, no thread, no pushes.
    #[test]
    fn the_fallback_reads_and_never_pushes() {
        let r = rig("query");
        r.server.advance(700);
        assert_eq!(r.read(T), IdleReading::Active { idle_for: Some(Duration::from_millis(700)) });
        assert_eq!(r.shared.pushing.load(Ordering::SeqCst), 0);
        assert!(matches!(*r.shared.io(), Io::Querying(_)));
        r.server.advance(1_300);
        assert_eq!(r.read(T), IdleReading::Idle);
        r.server.input();
        assert_eq!(r.read(T), IdleReading::Active { idle_for: Some(Duration::ZERO) });
        r.no_edge();
        r.disable();
        assert!(matches!(*r.shared.io(), Io::Off));
    }

    /// Neither extension: unsupported, and the server is not asked again.
    #[test]
    fn unsupported_is_asked_once() {
        let r = rig("none");
        assert_eq!(r.read(T), IdleReading::Unsupported);
        assert_eq!(r.read(T), IdleReading::Unsupported);
        assert_eq!(r.connects.load(Ordering::SeqCst), 1);
    }

    // ── Under the model's `AutoAway` ────────────────────────────────────────────────────────────

    use std::cell::RefCell;
    use std::rc::Rc;

    use decibell_model::live::presence::AutoAway;
    use gpui::{AppContext as _, Entity, TestAppContext};

    fn auto_away(source: Rc<X11Idle>, minutes: u32, cx: &mut TestAppContext) -> (Entity<AutoAway>, Rc<RefCell<Vec<bool>>>) {
        let reports: Rc<RefCell<Vec<bool>>> = Rc::default();
        let away = cx.new(|_| {
            let reports = reports.clone();
            AutoAway::new(Rc::new(move |idle, _| reports.borrow_mut().push(idle)))
        });
        away.update(cx, |a, cx| {
            a.set_source(source, cx);
            a.configure(minutes, true, cx);
        });
        cx.run_until_parked();
        (away, reports)
    }

    /// MIT-SCREEN-SAVER's answer on the test executor's clock.
    struct ExecutorClock {
        executor: BackgroundExecutor,
        last_input: Mutex<Instant>,
        reads: AtomicU64,
    }

    struct ClockQuery(Arc<ExecutorClock>);

    impl Query for ClockQuery {
        fn idle(&self) -> anyhow::Result<Duration> {
            self.0.reads.fetch_add(1, Ordering::SeqCst);
            Ok(self.0.executor.now().saturating_duration_since(*lock(&self.0.last_input)))
        }
    }

    /// The fallback: one QueryInfo when the threshold can first be reached, the next one armed for
    /// the remainder (no timer of ours, no periodic read while active); every second while Away,
    /// the model's own rate for a source that can't announce input.
    #[gpui::test]
    fn the_fallback_reads_once_per_threshold(cx: &mut TestAppContext) {
        const MIN: Duration = Duration::from_secs(60);
        let clock = Arc::new(ExecutorClock { executor: cx.executor(), last_input: Mutex::new(cx.executor().now()), reads: AtomicU64::new(0) });
        let connect: Connect = {
            let clock = clock.clone();
            Box::new(move |_| Probe { backend: Backend::Query(Arc::new(ClockQuery(clock.clone()))), about: "fake".into() })
        };
        let source = Rc::new(X11Idle::with_connect(connect, cx.executor(), Arc::new(|_| {})));
        let reads = || clock.reads.load(Ordering::SeqCst);
        let (away, reports) = auto_away(source.clone(), 10, cx);
        assert_eq!(reads(), 1);
        // Input at 4 minutes: the read at 10 says 6 minutes idle; the next is 4 minutes later.
        cx.executor().advance_clock(4 * MIN);
        *lock(&clock.last_input) = cx.executor().now();
        cx.executor().advance_clock(6 * MIN);
        cx.run_until_parked();
        assert_eq!(reads(), 2);
        cx.executor().advance_clock(4 * MIN - Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(reads(), 2, "nothing before the threshold can be reached");
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(reads(), 3);
        assert_eq!(*reports.borrow(), [true]);
        assert!(!source.pushes_resume());
        // Away: the model re-reads every second until input.
        cx.executor().advance_clock(Duration::from_secs(3));
        cx.run_until_parked();
        assert_eq!(reads(), 6);
        *lock(&clock.last_input) = cx.executor().now();
        cx.executor().advance_clock(Duration::from_secs(1));
        cx.run_until_parked();
        assert_eq!(*reports.borrow(), [true, false]);
        assert!(away.read_with(cx, |a, _| a.timer_armed()));
    }

    /// The SYNC path: the model goes Away on the idle edge's poke and keeps no timer while Away
    /// (the source pushes); the input edge's poke brings it back. Off: the thread ends.
    #[gpui::test]
    fn sync_edges_drive_auto_away(cx: &mut TestAppContext) {
        let server = Arc::new(Server::default());
        let connect: Connect = {
            let server = server.clone();
            Box::new(move |threshold| Probe { backend: Backend::Alarms(created(&server, threshold)), about: "fake".into() })
        };
        let (tx, edges) = mpsc::channel();
        let tx = Mutex::new(tx);
        let source = Rc::new(X11Idle::with_connect(connect, cx.executor(), Arc::new(move |edge| {
            let _ = lock(&tx).send(edge);
        })));
        let (away, reports) = auto_away(source.clone(), 1, cx);
        assert!(source.pushes_resume());
        assert_eq!(*lock(&server.state).idle_at.as_ref().unwrap(), 60_000);
        // What the app does with an edge (`idle::source`): poke.
        let edge = |cx: &mut TestAppContext| {
            let edge = edges.recv_timeout(Duration::from_secs(2)).ok();
            away.update(cx, |a, cx| a.poke(cx));
            cx.run_until_parked();
            edge
        };
        server.advance(60_000);
        assert_eq!(edge(cx), Some(Edge::Idle));
        assert_eq!(*reports.borrow(), [true]);
        assert!(!away.read_with(cx, |a, _| a.timer_armed()), "no timer while Away: the source pushes");
        server.input();
        assert_eq!(edge(cx), Some(Edge::Active));
        assert_eq!(*reports.borrow(), [true, false]);
        // Off (the app's observer): the alarms go, the thread ends.
        away.update(cx, |a, cx| a.configure(0, true, cx));
        source.disable();
        cx.run_until_parked();
        assert_eq!(lock(&server.state).stops, 1);
        let started = Instant::now();
        while source.pushes_resume() && started.elapsed() < Duration::from_secs(2) {
            std::thread::sleep(Duration::from_millis(1));
        }
        assert!(!source.pushes_resume());
    }
}
