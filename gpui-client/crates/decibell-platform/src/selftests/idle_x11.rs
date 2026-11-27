//! `idle-x11`: auto-away's X11 source end to end.
//!
//! 1. **SYNC, in a private Xvfb** this test starts (its own display number, `-displayfd`; stopped by its
//!    pid): the source in this process pointed at that display with a 2 s threshold. Input is faked
//!    with XTEST **inside that server only** (nothing reaches the desktop's own display): idle after
//!    2 s, active at once on input, re-armed, the threshold raised and lowered while running, the
//!    thread's context switches at rest (0), disable → the thread gone, enable → back.
//! 2. **The fallback** on the same kind of server (no X server lets SYNC be switched off, so the
//!    source is told to skip it): MIT-SCREEN-SAVER's `QueryInfo`, no thread, nothing pushed; and
//!    a server started without MIT-SCREEN-SAVER: unsupported.
//! 3. **The desktop's own session, read-only**: the app as a child (`--check platform-idle-source`)
//!    on Wayland picks the core's source and starts nothing of this; with `WAYLAND_DISPLAY` empty
//!    (XWayland, as the other "on X11" tests run) it picks this one, starts nothing before statuses
//!    are on, then watches Xwayland's counter with the user's threshold (10 minutes: no input is made
//!    there, none is needed), its thread counted at rest, and stops with the statuses.

use std::io::BufRead as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::TestCx;
use decibell_model::live::presence::{IdleReading, IdleSource as _};
use x11rb::connection::Connection as _;
use x11rb::protocol::xproto::{self, ConnectionExt as _};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

use super::{Child, EmptyBus, TempDir, app_env, dev_markers, exe, model, say};
use crate::idle::x11::{Edge, Path as IdlePath, THREAD_NAME, X11Idle};

const T: Duration = Duration::from_secs(2);

pub(super) async fn run(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    if Command::new("Xvfb").arg("-help").stdout(Stdio::null()).stderr(Stdio::null()).status().is_err() {
        cx.log("SKIP: no Xvfb (the private X server this test fakes input in)");
        return Ok(());
    }
    let dir = TempDir::new("idle-x11")?;
    sync_path(&cx, &dir.0).await?;
    fallback_paths(&cx, &dir.0).await?;
    session(&cx, &dir.0).await?;
    ensure!(threads(None).is_empty(), "an {THREAD_NAME} thread is left in this process");
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────

/// A private X server; stopped (SIGTERM, by its pid: it removes its socket and lock file) when dropped.
struct Xvfb {
    child: std::process::Child,
    display: String,
}

impl Xvfb {
    fn start(dir: &Path, name: &str, extra: &[&str]) -> Result<Xvfb> {
        let log = std::fs::File::create(dir.join(format!("{name}.log")))?;
        let mut child = Command::new("Xvfb")
            // `-displayfd 1`: Xvfb picks a free display and prints its number once it accepts clients.
            .args(["-displayfd", "1", "-nolisten", "tcp", "-noreset", "-screen", "0", "640x480x24"])
            .args(extra)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(log))
            .spawn()
            .context("starting Xvfb")?;
        let stdout = child.stdout.take().context("piped")?;
        let mut xvfb = Xvfb { child, display: String::new() };
        let (tx, rx) = mpsc::channel();
        std::thread::Builder::new().name("platform-test-xvfb".into()).spawn(move || {
            let mut line = String::new();
            let _ = std::io::BufReader::new(stdout).read_line(&mut line);
            let _ = tx.send(line);
        })?;
        let line = rx.recv_timeout(Duration::from_secs(15)).context("Xvfb printed no display within 15 s")?;
        let number: u32 = line.trim().parse().with_context(|| format!("Xvfb printed {line:?} (log: {name}.log)"))?;
        xvfb.display = format!(":{number}");
        Ok(xvfb)
    }
}

impl Drop for Xvfb {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            // SAFETY: `kill` on our own child, not yet reaped (so the pid is still its own); no memory
            // is involved.
            unsafe { libc::kill(self.child.id() as libc::pid_t, libc::SIGTERM) };
            let started = Instant::now();
            while matches!(self.child.try_wait(), Ok(None)) && started.elapsed() < Duration::from_secs(5) {
                std::thread::sleep(Duration::from_millis(10));
            }
            let _ = self.child.kill();
        }
        let _ = self.child.wait();
    }
}

/// Fake input inside the private server (XTEST): a pointer motion, then a round trip so the server
/// has processed it.
struct Input {
    conn: RustConnection,
    root: xproto::Window,
    n: i16,
}

impl Input {
    fn open(display: &str) -> Result<Input> {
        let (conn, screen) = x11rb::connect(Some(display)).with_context(|| format!("connecting to {display}"))?;
        conn.xtest_get_version(2, 2)?.reply().context("no XTEST on the private server")?;
        let root = conn.setup().roots.get(screen).context("no screen")?.root;
        Ok(Input { conn, root, n: 0 })
    }

    /// When the motion was sent.
    fn motion(&mut self) -> Result<Instant> {
        self.n = (self.n + 1) % 100;
        let at = Instant::now();
        self.conn.xtest_fake_input(xproto::MOTION_NOTIFY_EVENT, 0, x11rb::CURRENT_TIME, self.root, 20 + self.n, 20 + self.n, 0)?;
        self.conn.get_input_focus()?.reply()?;
        Ok(at)
    }
}

type Edges = mpsc::Receiver<(Edge, Instant)>;

/// The source for `display`, its edges stamped by the alarm thread as it sends them.
fn idle_source(cx: &TestCx, display: &str, sync: bool) -> (X11Idle, Edges) {
    let (tx, rx) = mpsc::channel();
    let tx = Mutex::new(tx);
    let sink = Arc::new(move |edge| {
        let _ = tx.lock().map(|tx| tx.send((edge, Instant::now())));
    });
    let executor = cx.app().background_executor().clone();
    let source = if sync { X11Idle::new(Some(display.to_string()), executor, sink) } else { X11Idle::without_sync(display.to_string(), executor, sink) };
    (source, rx)
}

async fn next_edge(cx: &TestCx, edges: &Edges, limit: Duration) -> Option<(Edge, Instant)> {
    let started = Instant::now();
    loop {
        if let Ok(edge) = edges.try_recv() {
            return Some(edge);
        }
        if started.elapsed() > limit {
            return None;
        }
        cx.sleep(Duration::from_millis(5)).await;
    }
}

async fn expect_edge(cx: &TestCx, edges: &Edges, want: Edge, limit: Duration, what: &str) -> Result<Instant> {
    match next_edge(cx, edges, limit).await {
        Some((edge, at)) if edge == want => Ok(at),
        Some((edge, _)) => bail!("{what}: a {edge} edge, expected {want}"),
        None => bail!("{what}: no {want} edge within {} ms", limit.as_millis()),
    }
}

/// The threads of `pid` (this process: `None`) named like the alarm thread.
fn threads(pid: Option<u32>) -> Vec<u32> {
    let dir = pid.map_or_else(|| "/proc/self/task".to_string(), |pid| format!("/proc/{pid}/task"));
    let Ok(entries) = std::fs::read_dir(&dir) else { return Vec::new() };
    entries
        .filter_map(Result::ok)
        .filter(|e| std::fs::read_to_string(e.path().join("comm")).is_ok_and(|c| c.trim() == THREAD_NAME))
        .filter_map(|e| e.file_name().to_str()?.parse().ok())
        .collect()
}

/// Voluntary + involuntary context switches of a thread (its wakeups).
fn switches(pid: Option<u32>, tid: u32) -> Result<u64> {
    let path = match pid {
        Some(pid) => format!("/proc/{pid}/task/{tid}/status"),
        None => format!("/proc/self/task/{tid}/status"),
    };
    let status = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
    Ok(status
        .lines()
        .filter(|l| l.starts_with("voluntary_ctxt_switches") || l.starts_with("nonvoluntary_ctxt_switches"))
        .filter_map(|l| l.split_whitespace().nth(1)?.parse::<u64>().ok())
        .sum())
}

async fn until(cx: &TestCx, limit: Duration, mut ready: impl FnMut() -> bool) -> bool {
    let started = Instant::now();
    while !ready() {
        if started.elapsed() > limit {
            return false;
        }
        cx.sleep(Duration::from_millis(10)).await;
    }
    true
}

fn active_under(reading: IdleReading, limit: Duration) -> bool {
    matches!(reading, IdleReading::Active { idle_for: Some(d) } if d < limit)
}

fn millis(d: Duration) -> String {
    format!("{:.2} ms", d.as_secs_f64() * 1000.0)
}

fn secs(d: Duration) -> String {
    format!("{:.3} s", d.as_secs_f64())
}

// ── 1. SYNC ────────────────────────────────────────────────────────────────────────────────────

async fn sync_path(cx: &TestCx, dir: &Path) -> Result<()> {
    let xvfb = Xvfb::start(dir, "xvfb", &[])?;
    let mut input = Input::open(&xvfb.display)?;
    let (source, edges) = idle_source(cx, &xvfb.display, true);
    ensure!(threads(None).is_empty(), "an {THREAD_NAME} thread before the first reading");

    let t0 = input.motion()?;
    let first = source.read(T).await;
    ensure!(source.path() == Some(IdlePath::Sync), "Xvfb {}: path {:?}, expected SYNC IDLETIME", xvfb.display, source.path());
    ensure!(active_under(first, Duration::from_millis(500)), "first reading {first:?}");
    ensure!(source.pushes_resume(), "the SYNC path doesn't push");
    let tids = threads(None);
    ensure!(tids.len() == 1, "{} {THREAD_NAME} threads while on", tids.len());
    let tid = tids[0];
    cx.log(format_args!("Xvfb {}: SYNC IDLETIME alarms; first reading {first:?}; one {THREAD_NAME} thread", xvfb.display));

    let idle_at = expect_edge(cx, &edges, Edge::Idle, Duration::from_secs(4), "the first threshold").await?;
    let after = idle_at - t0;
    ensure!(after >= Duration::from_millis(1_950) && after < Duration::from_millis(2_500), "idle {} after the input (threshold 2 s)", secs(after));
    ensure!(source.read(T).await == IdleReading::Idle, "reading after the idle edge is not Idle");

    // Away and at rest: the thread sleeps.
    cx.sleep(Duration::from_millis(200)).await;
    let before = switches(None, tid)?;
    cx.sleep(Duration::from_secs(3)).await;
    let woke_away = switches(None, tid)? - before;
    ensure!(edges.try_recv().is_err(), "an edge at rest");
    ensure!(woke_away == 0, "the {THREAD_NAME} thread woke {woke_away} times in 3 s at rest (Away)");

    // Input: active at once, and the idle alarm re-armed for a threshold later.
    let t1 = input.motion()?;
    let active_at = expect_edge(cx, &edges, Edge::Active, Duration::from_secs(1), "input while idle").await?;
    let latency = active_at - t1;
    ensure!(latency < Duration::from_millis(250), "active {} after the input", secs(latency));
    let reading = source.read(T).await;
    ensure!(active_under(reading, Duration::from_millis(500)), "reading after input {reading:?}");
    let idle_again = expect_edge(cx, &edges, Edge::Idle, Duration::from_secs(4), "re-armed").await? - t1;
    ensure!(idle_again >= Duration::from_millis(1_950) && idle_again < Duration::from_millis(2_500), "idle again {} after the input", secs(idle_again));
    cx.log(format_args!(
        "idle {} after the input; Away 3 s: the thread woke {woke_away} times; input → active in {}; idle again {} after it",
        secs(after),
        millis(latency),
        secs(idle_again)
    ));

    // Active and at rest (input, then nothing until the threshold): no wakeups either.
    let t2 = input.motion()?;
    expect_edge(cx, &edges, Edge::Active, Duration::from_secs(1), "input").await?;
    cx.sleep(Duration::from_millis(300)).await;
    let before = switches(None, tid)?;
    cx.sleep(Duration::from_millis(1_000)).await;
    let woke_active = switches(None, tid)? - before;
    ensure!(woke_active == 0, "the {THREAD_NAME} thread woke {woke_active} times in 1 s while active at rest");

    // Raised to 4 s (~1.3 s in): nothing at 2 s.
    let raised = source.read(Duration::from_secs(4)).await;
    ensure!(matches!(raised, IdleReading::Active { .. }), "raised to 4 s: {raised:?}");
    let quiet = next_edge(cx, &edges, Duration::from_secs(3).saturating_sub(t2.elapsed())).await;
    ensure!(quiet.is_none(), "an edge {quiet:?} before the raised threshold");
    // Lowered to 2 s at ~3 s in: idle at once.
    let lowered_at = Instant::now();
    let lowered = source.read(T).await;
    ensure!(lowered == IdleReading::Idle, "lowered under the idle time: {lowered:?}");
    let at_once = expect_edge(cx, &edges, Edge::Idle, Duration::from_secs(1), "the threshold lowered under the idle time").await? - lowered_at;
    ensure!(at_once < Duration::from_millis(250), "idle {} after lowering", secs(at_once));
    // Raised to 5 s while idle: active by the reading, idle again at 5 s.
    let reading = source.read(Duration::from_secs(5)).await;
    ensure!(matches!(reading, IdleReading::Active { idle_for: Some(d) } if d >= Duration::from_secs(3)), "raised while idle: {reading:?}");
    let at_five = expect_edge(cx, &edges, Edge::Idle, Duration::from_secs(4), "the raised threshold").await? - t2;
    ensure!(at_five >= Duration::from_millis(4_950) && at_five < Duration::from_millis(5_500), "idle {} after the input (threshold 5 s)", secs(at_five));
    let t3 = input.motion()?;
    let back = expect_edge(cx, &edges, Edge::Active, Duration::from_secs(1), "input after the raised threshold").await? - t3;
    cx.log(format_args!(
        "active at rest 1 s: woke {woke_active} times; raised to 4 s: no edge at 2 s; lowered to 2 s at {} idle: idle edge {} later; raised to 5 s: idle at {}, active {} after input",
        secs(lowered_at - t2),
        millis(at_once),
        secs(at_five),
        millis(back)
    ));

    // Off: the thread goes; on again: one thread; off.
    source.disable();
    ensure!(until(cx, Duration::from_secs(2), || threads(None).is_empty()).await, "the {THREAD_NAME} thread is still there 2 s after disable");
    ensure!(!source.pushes_resume() && source.path().is_none(), "still on after disable");
    let gone = Instant::now();
    let reading = source.read(T).await;
    ensure!(matches!(reading, IdleReading::Active { .. }), "re-enabled: {reading:?}");
    let tids = threads(None);
    ensure!(tids.len() == 1 && tids[0] != tid, "re-enabled: threads {tids:?} (was {tid})");
    source.disable();
    ensure!(until(cx, Duration::from_secs(2), || threads(None).is_empty()).await, "the thread is still there after the second disable");
    cx.log(format_args!("disabled: the thread gone; enabled again: a new one ({} after); disabled: gone", secs(gone.elapsed())));
    drop(source);
    Ok(())
}

// ── 2. The fallback, and neither ───────────────────────────────────────────────────────────────

async fn fallback_paths(cx: &TestCx, dir: &Path) -> Result<()> {
    let xvfb = Xvfb::start(dir, "xvfb-screensaver", &[])?;
    let mut input = Input::open(&xvfb.display)?;
    let (source, edges) = idle_source(cx, &xvfb.display, false);
    let t0 = input.motion()?;
    let first = source.read(T).await;
    ensure!(source.path() == Some(IdlePath::ScreenSaver), "path {:?}, expected MIT-SCREEN-SAVER", source.path());
    ensure!(active_under(first, Duration::from_millis(500)), "first reading {first:?}");
    ensure!(!source.pushes_resume(), "the fallback claims to push");
    ensure!(threads(None).is_empty(), "the fallback started a thread");
    cx.sleep((T + Duration::from_millis(100)).saturating_sub(t0.elapsed())).await;
    let idle = source.read(T).await;
    ensure!(idle == IdleReading::Idle, "{} after input: {idle:?}", secs(t0.elapsed()));
    input.motion()?;
    let back = source.read(T).await;
    ensure!(active_under(back, Duration::from_millis(500)), "after input: {back:?}");
    ensure!(edges.try_recv().is_err(), "the fallback sent an edge");
    source.disable();
    ensure!(until(cx, Duration::from_secs(2), || source.path().is_none()).await, "still connected after disable");
    cx.log(format_args!("MIT-SCREEN-SAVER (SYNC skipped): {first:?}, then {idle:?} at 2.1 s, {back:?} after input; no thread, no edges"));
    drop((source, xvfb));

    let bare = Xvfb::start(dir, "xvfb-bare", &["-extension", "MIT-SCREEN-SAVER"])?;
    let (source, _edges) = idle_source(cx, &bare.display, false);
    let reading = source.read(T).await;
    ensure!(reading == IdleReading::Unsupported && source.path() == Some(IdlePath::Unsupported), "without either: {reading:?}, {:?}", source.path());
    cx.log("neither SYNC nor MIT-SCREEN-SAVER: Unsupported");
    Ok(())
}

// ── 3. The desktop's session, read-only ────────────────────────────────────────────────────────

async fn session(cx: &TestCx, dir: &Path) -> Result<()> {
    let set = |key: &str| std::env::var_os(key).is_some_and(|v| !v.is_empty());
    let sessions: Vec<(&str, Vec<(&str, String)>)> = match (set("WAYLAND_DISPLAY"), set("DISPLAY")) {
        (true, true) => vec![("wayland", vec![]), ("xwayland", vec![("WAYLAND_DISPLAY", String::new())])],
        (true, false) => vec![("wayland", vec![])],
        (false, true) => vec![("x11", vec![])],
        (false, false) => {
            cx.log("no desktop session: part 3 skipped");
            return Ok(());
        }
    };
    let bus = EmptyBus::start(dir)?;
    for (session, extra) in sessions {
        let x11 = session != "wayland";
        let mut env = app_env(&dir.join(format!("profile-{session}")), &bus, None);
        env.extend(extra);
        let mut child = Child::spawn(&format!("idle-{session}"), &exe()?, &["--check", "platform-idle-source"], &env, &[], dir)?;
        let pid = child.pid();
        let chosen = child.wait_line(cx, "decibell: platform idle source ", Duration::from_secs(40)).await?;
        let chosen = chosen.trim_start_matches("decibell: platform idle source ").to_string();
        ensure!(chosen == if x11 { "x11" } else { "core" }, "{session}: the {chosen} source");
        let ready = child.wait_line(cx, "platform-check: ready ", Duration::from_secs(40)).await?;
        let minutes = ready.trim_start_matches("platform-check: ready ").to_string();
        ensure!(threads(Some(pid)).is_empty(), "{session}: an {THREAD_NAME} thread before statuses");
        child.wait_line(cx, "platform-check: statuses on", Duration::from_secs(10)).await?;
        if x11 {
            let path = child.wait_line(cx, "decibell: platform idle x11 path ", Duration::from_secs(10)).await?;
            let path = path.trim_start_matches("decibell: platform idle x11 path ").to_string();
            if path.starts_with("sync") {
                ensure!(until(cx, Duration::from_secs(2), || threads(Some(pid)).len() == 1).await, "{session}: no {THREAD_NAME} thread with statuses on");
                // Let the first reading and its edges (the user may have been idle for longer than
                // the threshold already) pass, then count.
                cx.sleep(Duration::from_millis(1_500)).await;
                let tid = threads(Some(pid)).first().copied().context("the thread went")?;
                child.drain();
                let edges_before = child.seen.iter().filter(|l| l.starts_with("decibell: platform idle x11 idle") || l.starts_with("decibell: platform idle x11 active")).count();
                // An idle edge at once: the server had been idle past the threshold already.
                let idle_at_start = child.seen.iter().any(|l| l.starts_with("decibell: platform idle x11 idle"));
                let before = switches(Some(pid), tid)?;
                cx.sleep(Duration::from_secs(5)).await;
                let woke = switches(Some(pid), tid)? - before;
                child.drain();
                let edges = child.seen.iter().filter(|l| l.starts_with("decibell: platform idle x11 idle") || l.starts_with("decibell: platform idle x11 active")).count() - edges_before;
                // Each edge is one wake, and the reading it causes one more (its reply).
                ensure!(woke <= 2 * edges as u64, "{session}: the {THREAD_NAME} thread woke {woke} times in 5 s with {edges} edges");
                cx.log(format_args!(
                    "{session} {minutes}: {path}; {}; the thread woke {woke} times in 5 s at rest ({edges} edges meanwhile)",
                    if idle_at_start { "an idle edge at once (the server had been idle past the threshold)" } else { "no edge at the start" }
                ));
            } else {
                cx.log(format_args!("{session}: {path} (no thread)"));
            }
            child.wait_line(cx, "platform-check: statuses off", Duration::from_secs(20)).await?;
            ensure!(until(cx, Duration::from_secs(2), || threads(Some(pid)).is_empty()).await, "{session}: the thread outlived the statuses");
        } else {
            cx.sleep(Duration::from_secs(1)).await;
            ensure!(threads(Some(pid)).is_empty(), "{session}: an {THREAD_NAME} thread");
            ensure!(!child.printed("decibell: platform idle x11"), "{session}: the X11 source ran");
            cx.log(format_args!("{session} {minutes}: the core's source; no {THREAD_NAME} thread, no X11 connection"));
        }
        child.wait_line(cx, "platform-check: done", Duration::from_secs(20)).await?;
        let status = child.wait_exit(cx, Duration::from_secs(20)).await?;
        ensure!(status.success(), "{session}: the child failed ({status}):\n{}", child.tail());
    }
    Ok(())
}

/// `--check platform-idle-source` (in the app): statuses on as a sign-in to a central with statuses
/// turns them on (`OwnPresence::apply_account`: auto-away at the settings' threshold), then off as
/// a sign-out does. No central is reached (`set_idle` reports fail quietly).
pub(super) async fn check(cx: TestCx) -> Result<()> {
    let model = cx.app().update(|cx| model(cx));
    cx.sleep(Duration::from_secs(1)).await;
    let minutes = cx.app().update(|cx| model.settings.read(cx).get().away_after_minutes);
    say(format_args!("ready ({} min threshold, {})", minutes, cx.app().update(|cx| cx.compositor_name())));
    cx.sleep(Duration::from_millis(500)).await;
    cx.app().update(|cx| {
        let now = cx.background_executor().now();
        model.live.presence.own.update(cx, |o, cx| o.apply_account(true, "online", None, Duration::ZERO, false, now, cx));
    });
    say(format_args!("statuses on"));
    // `DECIBELL_CHECK_HOLD_SECS`: longer, to measure the child from outside (`tools/perf/measure.sh`).
    let hold = std::env::var("DECIBELL_CHECK_HOLD_SECS").ok().and_then(|s| s.parse().ok()).unwrap_or(9);
    cx.sleep(Duration::from_secs(hold)).await;
    cx.app().update(|cx| model.live.presence.own.update(cx, |o, cx| o.reset(cx)));
    say(format_args!("statuses off"));
    cx.sleep(Duration::from_secs(3)).await;
    say(format_args!("done"));
    Ok(())
}
