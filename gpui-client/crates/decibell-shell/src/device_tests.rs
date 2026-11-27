//! Self-tests of staying signed in and of several devices (`docs/FEATURES.md` §1.3, §1.4),
//! against the local test stack. Each registers throwaway accounts with `decibell-headless`
//! (`register`; every process signs up from a loopback address of its own, so central's
//! per-address sign-up bucket doesn't run dry), runs this app in a child (`--check <name>`, its
//! own profile, no `--login`: the check signs in by the model's action, the form is
//! `auth-typed`'s) and drives the account's other devices with more headless clients. Nothing
//! touches the stack's seeded accounts.
//!
//! - `accounts-resume`: the app reaches central through a loopback TCP proxy of the test's; the
//!   test cuts every connection and refuses new ones, the peer DMs us meanwhile, the proxy comes
//!   back: the app signs in again by its resume token (its device's password sign-in time is
//!   unchanged; the proxy saw the reconnect) and the open conversation shows the DM that came while
//!   it was away.
//! - `accounts-signed-out-restart`: the app signs in (saved sign-in), quits; another device signs
//!   it out; the app started again on its profile lands on the sign-in page with Electron's notice
//!   and its saved sign-in is gone.
//! - `accounts-device-limit`: eight headless devices of the account online: the app's sign-in is
//!   refused with central's text under the form; one leaves and the app signs in; then, while the
//!   proxy holds the app off, a ninth device takes the slot: the app's reconnect is refused and the
//!   "Can't reconnect" toast says why.
//! - `accounts-second-device`: a headless second device of the account sets a status, a custom
//!   status and friends-only DMs, DMs a friend, asks for a friendship and joins a public
//!   community; the app follows each (own presence, settings, the DM list, the friend list, the
//!   server bar), then the device leaves the community again.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};
use std::io::BufRead as _;
use std::net::{Shutdown, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx};
use decibell_model::live::friends::FriendStatus;
use decibell_model::live::presence::OwnStatus;
use decibell_model::stores::toasts::Severity;
use decibell_model::{Uid, actions};
use gpui::{App, SharedString};

use crate::probes;
use crate::selftests::{Profile, Stack, model, stack, wait_for};

const MARK: &str = "ACCOUNTS-MARK";
const CHILD_LIMIT: Duration = Duration::from_secs(240);

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "accounts-resume",
            about: "central cut by a proxy and back: the app signs in again by its resume token (no password) and the open conversation catches up",
            run: |cx| Box::pin(resume(cx)),
        },
        SelfTest {
            name: "accounts-signed-out-restart",
            about: "a device signed out by another, started again: the sign-in page with the notice, its saved sign-in gone",
            run: |cx| Box::pin(signed_out_restart(cx)),
        },
        SelfTest {
            name: "accounts-device-limit",
            about: "eight devices online: the ninth sign-in refused under the form; as a reconnect, the \"Can't reconnect\" toast",
            run: |cx| Box::pin(device_limit(cx)),
        },
        SelfTest {
            name: "accounts-second-device",
            about: "another device of the account changes status, custom status, friends-only, DMs, asks a friend, joins a community: the app follows each",
            run: |cx| Box::pin(second_device(cx)),
        },
    ]
}

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "accounts-resume", about: "(in the app) resume after a cut", run: |cx| Box::pin(check_resume(cx)) },
        SelfTest {
            name: "accounts-signed-out-restart",
            about: "(in the app) a signed-out device, restarted",
            run: |cx| Box::pin(check_signed_out_restart(cx)),
        },
        SelfTest { name: "accounts-device-limit", about: "(in the app) the device limit", run: |cx| Box::pin(check_device_limit(cx)) },
        SelfTest { name: "accounts-second-device", about: "(in the app) another device's changes", run: |cx| Box::pin(check_second_device(cx)) },
    ]
}

// ── Shared pieces ────────────────────────────────────────────────────────────────────────────

fn random_hex(n: usize) -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    h.write_u32(std::process::id());
    let a = h.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())[..n].to_string()
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is not set"))
}

fn mark(what: impl std::fmt::Display) {
    println!("{MARK} {what}");
}

fn phase() -> u32 {
    std::env::var("DECIBELL_CHECK_PHASE").ok().and_then(|p| p.parse().ok()).unwrap_or(1)
}

/// `decibell-headless`, built next to this binary.
fn headless_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("decibell-headless");
    candidate.exists().then_some(candidate)
}

/// Lines of a child process, gathered by a thread per stream.
fn gather(child: &mut std::process::Child, label: &str) -> Arc<Mutex<Vec<String>>> {
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let streams: [(Option<Box<dyn std::io::Read + Send>>, &str); 2] = [
        (child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), ""),
        (child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), "stderr: "),
    ];
    for (stream, tag) in streams {
        let Some(stream) = stream else { continue };
        let (lines, tag) = (lines.clone(), tag.to_string());
        std::thread::Builder::new()
            .name(format!("accounts-{label}-pipe"))
            .spawn(move || {
                for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                    lines.lock().unwrap_or_else(|e| e.into_inner()).push(format!("{tag}{line}"));
                }
            })
            .ok();
    }
    lines
}

/// This app in a child process running `check`.
struct App1 {
    name: String,
    child: std::process::Child,
    lines: Arc<Mutex<Vec<String>>>,
    started: Instant,
}

impl App1 {
    fn spawn(check: &str, central: &str, stack: &Stack, profile: &Path, env: &[(&str, String)]) -> Result<App1> {
        let mut command = Command::new(std::env::current_exe()?);
        command
            .arg("--check")
            .arg(check)
            .env("DECIBELL_USER_DATA_DIR", profile)
            .env("DECIBELL_CENTRAL", central)
            .env("DECIBELL_DEV_PASSWORD", &stack.password)
            .env("DECIBELL_TEST_PROBES", "1")
            .env_remove("DECIBELL_PERF")
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().context("starting the app")?;
        let lines = gather(&mut child, "app");
        Ok(App1 { name: check.to_string(), child, lines, started: Instant::now() })
    }

    /// The value after `MARK <key> ` once the child printed it.
    async fn wait_mark(&mut self, cx: &TestCx, key: &str, limit: Duration) -> Result<String> {
        let needle = format!("{MARK} {key}");
        let started = Instant::now();
        loop {
            let found = self.lines.lock().unwrap_or_else(|e| e.into_inner()).iter().find_map(|l| {
                l.strip_prefix(&needle).filter(|rest| rest.is_empty() || rest.starts_with(' ')).map(|rest| rest.trim().to_string())
            });
            if let Some(value) = found {
                return Ok(value);
            }
            if let Some(status) = self.child.try_wait()? {
                self.relay(cx, true);
                bail!("{} ended ({status}) before {key}", self.name);
            }
            if started.elapsed() > limit {
                self.relay(cx, true);
                bail!("{}: no {key} after {} s", self.name, limit.as_secs());
            }
            cx.sleep(Duration::from_millis(100)).await;
        }
    }

    fn relay(&self, cx: &TestCx, errors: bool) {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        for line in lines.iter().filter(|l| !l.starts_with("stderr: ") && !l.starts_with("selftest:") && !l.starts_with(MARK)) {
            cx.log(format_args!("│ {line}"));
        }
        if errors {
            let stderr: Vec<&String> = lines.iter().filter(|l| l.starts_with("stderr: ")).collect();
            for line in stderr.iter().rev().take(16).rev() {
                cx.log(format_args!("│ {line}"));
            }
        }
    }

    async fn finish(mut self, cx: &TestCx) -> Result<()> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.relay(cx, !status.success());
                ensure!(status.success(), "{} failed in the app ({status}, after {:.1} s)", self.name, self.started.elapsed().as_secs_f64());
                return Ok(());
            }
            if self.started.elapsed() > CHILD_LIMIT {
                let _ = self.child.kill();
                self.relay(cx, true);
                bail!("{} still running after {} s: killed", self.name, CHILD_LIMIT.as_secs());
            }
            cx.sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for App1 {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// A `decibell-headless` script: another device, or a helper. Its profile lives as long as it.
struct Headless {
    label: String,
    child: std::process::Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
    _profile: Profile,
}

impl Headless {
    fn start(label: &str, central: &str, steps: &[&str]) -> Result<Headless> {
        let exe = headless_exe().context("decibell-headless is not built next to this binary")?;
        let profile = Profile::new(&format!("accounts-{label}-{}", random_hex(6)))?;
        let mut child = Command::new(exe)
            .arg("--user-data-dir")
            .arg(&profile.0)
            .arg("--timeout")
            .arg("120")
            .args(steps)
            .env("DECIBELL_CENTRAL", central)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting decibell-headless")?;
        let stdout = child.stdout.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new().name(format!("accounts-{label}")).spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
        Ok(Headless { label: label.to_string(), child, lines, seen: Vec::new(), _profile: profile })
    }

    /// Until a line starting with `prefix` (FAILED fails); returns it.
    async fn line(&mut self, cx: &TestCx, prefix: &str, limit: Duration) -> Result<String> {
        let started = Instant::now();
        loop {
            while let Ok(line) = self.lines.try_recv() {
                self.seen.push(line.clone());
                if line.starts_with("FAILED") {
                    bail!("{}: {line}", self.label);
                }
                if line.starts_with(prefix) {
                    return Ok(line);
                }
            }
            ensure!(started.elapsed() < limit, "{} never printed {prefix:?} (it printed {:?})", self.label, self.seen);
            cx.sleep(Duration::from_millis(30)).await;
        }
    }

    async fn done(mut self, cx: &TestCx) -> Result<Vec<String>> {
        self.line(cx, "DONE", Duration::from_secs(150)).await?;
        let _ = self.child.wait();
        Ok(std::mem::take(&mut self.seen))
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

/// Sign `name` up (a headless client); central's sign-up bucket of a busy address is waited for.
async fn register(cx: &TestCx, central: &str, name: &str, password: &str) -> Result<()> {
    let started = Instant::now();
    loop {
        let client = Headless::start("register", central, &["register", name, password])?;
        match client.done(cx).await {
            Ok(_) => return Ok(()),
            Err(e) if e.to_string().contains("Too many sign-ups") && started.elapsed() < Duration::from_secs(300) => {
                cx.log("central's sign-up bucket is empty: waiting for a token");
                cx.sleep(Duration::from_secs(30)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// A loopback TCP proxy in front of central that the test can cut: every live connection is
/// shut down and new ones are closed at once until it is restored.
struct Proxy {
    addr: String,
    state: Arc<ProxyState>,
}

#[derive(Default)]
struct ProxyState {
    cut: AtomicBool,
    stop: AtomicBool,
    accepted: AtomicUsize,
    live: Mutex<Vec<TcpStream>>,
}

impl Proxy {
    fn start(upstream: &str) -> Result<Proxy> {
        let listener = TcpListener::bind("127.0.0.1:0")?;
        let addr = listener.local_addr()?.to_string();
        let state = Arc::new(ProxyState::default());
        let upstream = upstream.to_string();
        let s = state.clone();
        std::thread::Builder::new().name("accounts-proxy".into()).spawn(move || {
            for stream in listener.incoming() {
                if s.stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(client) = stream else { continue };
                if s.cut.load(Ordering::SeqCst) {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                }
                let Ok(server) = TcpStream::connect(&upstream) else {
                    let _ = client.shutdown(Shutdown::Both);
                    continue;
                };
                s.accepted.fetch_add(1, Ordering::SeqCst);
                let pairs = [(client.try_clone(), server.try_clone()), (server.try_clone(), client.try_clone())];
                {
                    let mut live = s.live.lock().unwrap_or_else(|e| e.into_inner());
                    live.extend(client.try_clone());
                    live.extend(server.try_clone());
                }
                for (from, to) in pairs {
                    let (Ok(mut from), Ok(mut to)) = (from, to) else { continue };
                    let _ = std::thread::Builder::new().name("accounts-proxy-pump".into()).spawn(move || {
                        let _ = std::io::copy(&mut from, &mut to);
                        let _ = to.shutdown(Shutdown::Both);
                        let _ = from.shutdown(Shutdown::Both);
                    });
                }
            }
        })?;
        Ok(Proxy { addr, state })
    }

    fn cut(&self) {
        self.state.cut.store(true, Ordering::SeqCst);
        for stream in self.state.live.lock().unwrap_or_else(|e| e.into_inner()).drain(..) {
            let _ = stream.shutdown(Shutdown::Both);
        }
    }

    fn restore(&self) {
        self.state.cut.store(false, Ordering::SeqCst);
    }

    fn accepted(&self) -> usize {
        self.state.accepted.load(Ordering::SeqCst)
    }
}

impl Drop for Proxy {
    fn drop(&mut self) {
        self.cut();
        self.state.stop.store(true, Ordering::SeqCst);
        // Wake the accept.
        let _ = TcpStream::connect(&self.addr);
    }
}

macro_rules! skip_unless_ready {
    ($cx:expr) => {
        match stack()? {
            None => {
                $cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
                return Ok(());
            }
            Some(_) if headless_exe().is_none() => {
                $cx.log("SKIP: decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)");
                return Ok(());
            }
            Some(stack) => stack,
        }
    };
}

/// The core honours `DECIBELL_CENTRAL` in this build and it is loopback.
fn local_only() -> Result<()> {
    ensure!(
        decibell_model::core::DEVELOPMENT_BUILD,
        "this build ignores DECIBELL_CENTRAL (a release build without dev-local): refusing to talk to any server"
    );
    let central = env("DECIBELL_CENTRAL")?;
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: refusing");
    Ok(())
}

/// Sign in by the model's action (the form itself is `auth-typed`'s) and wait for it.
async fn sign_in(cx: &TestCx, name: &str, password: &str) -> Result<()> {
    wait_for(cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    let (n, p) = (name.to_string(), password.to_string());
    cx.update(move |_, cx| actions::auth::sign_in(n, p, cx))?;
    wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}

async fn log_out(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::auth::log_out(cx))?;
    wait_for(cx, Duration::from_secs(20), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}

/// This device's row of the account's list: (id, password sign-in time), loading the list.
async fn this_device(cx: &TestCx) -> Result<(i64, i64)> {
    // A fresh answer: forget what was loaded before.
    cx.update(|_, cx| {
        model(cx).live.devices.update(cx, |d, _| d.list = None);
        actions::devices::load(cx);
    })?;
    wait_for(cx, Duration::from_secs(15), "the device list", |cx| model(cx).live.devices.read(cx).list.is_some()).await?;
    cx.update(|_, cx| {
        let d = model(cx).live.devices.read(cx);
        d.list.as_ref().and_then(|l| l.iter().find(|d| d.current).map(|d| (d.id, d.signed_in_at)))
    })?
    .context("this device isn't in the account's list")
}

fn rows_of(cx: &App, peer: Uid) -> Vec<String> {
    model(cx).dm.list.read(cx).slice(peer).map_or(Vec::new(), |s| s.read(cx).rows().iter().filter(|m| m.id > 0).map(|m| m.content.to_string()).collect())
}

/// Throwaway names for a run: `<prefix><hex>`.
fn throwaway(prefix: &str) -> (String, String) {
    let tag = random_hex(8);
    (format!("{prefix}{tag}"), format!("pw-{}", random_hex(12)))
}

// ── accounts-resume (T2) ─────────────────────────────────────────────────────────────────────

async fn resume(cx: TestCx) -> Result<()> {
    let stack = skip_unless_ready!(cx);
    let (a, password) = throwaway("ra");
    let b = format!("rb{}", &a[2..]);
    cx.log(format_args!("throwaway accounts {a} (the app) and {b} (the peer)"));
    register(&cx, &stack.central, &a, &password).await?;
    register(&cx, &stack.central, &b, &password).await?;
    let proxy = Proxy::start(&stack.central)?;
    cx.log(format_args!("the app reaches central through {}", proxy.addr));
    let profile = Profile::new("accounts-resume")?;
    let env = [("DECIBELL_TEST_ACCOUNT", a.clone()), ("DECIBELL_TEST_PEER", b.clone()), ("DECIBELL_TEST_PASSWORD", password.clone())];
    let mut app = App1::spawn("accounts-resume", &proxy.addr, &stack, &profile.0, &env)?;
    let uid = app.wait_mark(&cx, "uid", Duration::from_secs(60)).await?;
    let before = format!("before-{}", random_hex(6));
    let peer = Headless::start("peer", &stack.central, &["login-as", &b, &password, "wait", "logged-in", "dm-send", &format!("uid:{uid}"), &before, "idle", "1", "logout", "wait", "logged-out"])?;
    peer.done(&cx).await?;
    app.wait_mark(&cx, "open", Duration::from_secs(60)).await?;
    let accepted = proxy.accepted();
    proxy.cut();
    cx.log(format_args!("cut: {accepted} connection(s) through the proxy so far; new ones refused"));
    cx.sleep(Duration::from_secs(2)).await;
    let away = format!("away-{}", random_hex(6));
    let peer = Headless::start("peer", &stack.central, &["login-as", &b, &password, "wait", "logged-in", "dm-send", &format!("uid:{uid}"), &away, "idle", "1", "logout", "wait", "logged-out"])?;
    peer.done(&cx).await?;
    cx.log(format_args!("the peer sent {away:?} while the app was cut off; restoring the proxy"));
    proxy.restore();
    app.wait_mark(&cx, "caught-up", Duration::from_secs(120)).await?;
    ensure!(proxy.accepted() > accepted, "the app never connected again through the proxy");
    cx.log(format_args!("{} connection(s) through the proxy in all", proxy.accepted()));
    app.finish(&cx).await
}

async fn check_resume(cx: TestCx) -> Result<()> {
    local_only()?;
    let (a, b, password) = (env("DECIBELL_TEST_ACCOUNT")?, env("DECIBELL_TEST_PEER")?, env("DECIBELL_TEST_PASSWORD")?);
    sign_in(&cx, &a, &password).await?;
    let (device, signed_in_at) = this_device(&cx).await?;
    let uid = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    cx.log(format_args!("signed in as {a} (uid {uid}), device {device}, password sign-in at {signed_in_at}"));
    mark(format_args!("uid {uid}"));
    // The peer's first DM makes the conversation; open it (the DM view, as a click on its row).
    let b_name: SharedString = b.clone().into();
    wait_for(&cx, Duration::from_secs(60), "the peer's DM", move |cx| model(cx).dm.list.read(cx).conversations().any(|c| c.username == b_name))
        .await?;
    let peer = cx.update(|_, cx| model(cx).dm.list.read(cx).conversations().find(|c| *c.username == *b).map(|c| c.peer))?.context("gone")?;
    let name: SharedString = env("DECIBELL_TEST_PEER")?.into();
    cx.update(move |_, cx| {
        actions::dm::open_dm(peer, name, cx);
        actions::dm::open_history(peer, cx);
    })?;
    wait_for(&cx, Duration::from_secs(20), "the open conversation's history", move |cx| rows_of(cx, peer).iter().any(|t| t.starts_with("before-")))
        .await?;
    let epoch = cx.update(|_, cx| model(cx).dm.list.read(cx).resync_epoch())?;
    // A password sign-in would move the device's sign-in time: let a second pass first.
    cx.sleep(Duration::from_millis(1500)).await;
    mark("open");
    let t = wait_for(&cx, Duration::from_secs(120), "the DM sent while away, in the open conversation", move |cx| {
        rows_of(cx, peer).iter().any(|t| t.starts_with("away-"))
    })
    .await?;
    let resyncs = cx.update(|_, cx| model(cx).dm.list.read(cx).resync_epoch())? - epoch;
    ensure!(resyncs >= 1, "the DM arrived without a new sign-in (the cut didn't cut?)");
    let (device_after, signed_in_after) = this_device(&cx).await?;
    cx.log(format_args!(
        "signed in again ({resyncs} resync); the open conversation caught up {} ms after the cut began; device {device_after}, password sign-in at {signed_in_after}",
        t.as_millis()
    ));
    ensure!(device_after == device, "another device row: {device} → {device_after}");
    ensure!(signed_in_after == signed_in_at, "the device signed in by password again ({signed_in_at} → {signed_in_after}): not a resume");
    mark("caught-up");
    log_out(&cx).await
}

// ── accounts-signed-out-restart (T3) ─────────────────────────────────────────────────────────

async fn signed_out_restart(cx: TestCx) -> Result<()> {
    let stack = skip_unless_ready!(cx);
    let (a, password) = throwaway("sa");
    cx.log(format_args!("throwaway account {a}"));
    register(&cx, &stack.central, &a, &password).await?;
    let profile = Profile::new("accounts-signed-out-restart")?;
    let env = |phase: u32| {
        [("DECIBELL_TEST_ACCOUNT", a.clone()), ("DECIBELL_TEST_PASSWORD", password.clone()), ("DECIBELL_CHECK_PHASE", phase.to_string())]
    };
    let mut first = App1::spawn("accounts-signed-out-restart", &stack.central, &stack, &profile.0, &env(1))?;
    let device = first.wait_mark(&cx, "device", Duration::from_secs(60)).await?;
    first.finish(&cx).await?;
    cx.log(format_args!("the app quit signed in (device {device}); another device signs it out"));
    let other = Headless::start(
        "other",
        &stack.central,
        &["login-as", &a, &password, "wait", "logged-in", "devices", "wait", "devices", "device-sign-out", &device, "idle", "2", "logout", "wait", "logged-out"],
    )?;
    other.done(&cx).await?;
    cx.log("signed out from the other device: starting the app again on its profile");
    App1::spawn("accounts-signed-out-restart", &stack.central, &stack, &profile.0, &env(2))?.finish(&cx).await
}

async fn check_signed_out_restart(cx: TestCx) -> Result<()> {
    local_only()?;
    let (a, password) = (env("DECIBELL_TEST_ACCOUNT")?, env("DECIBELL_TEST_PASSWORD")?);
    if phase() == 1 {
        sign_in(&cx, &a, &password).await?;
        let saved = crate::selftests::saved_username().await?;
        ensure!(saved.as_deref() == Some(a.as_str()), "the sign-in wasn't saved ({saved:?})");
        let (device, _) = this_device(&cx).await?;
        cx.log(format_args!("signed in as {a}, saved; device {device}; quitting without signing out"));
        mark(format_args!("device {device}"));
        return Ok(());
    }
    // The saved sign-in is tried at start; central refuses it (`signed_out`).
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    let notice = "You were signed out on this device. Sign in again to keep using it here.";
    wait_for(&cx, Duration::from_secs(30), "the sign-in page's notice", |cx| {
        let auth = model(cx).auth.read(cx);
        !auth.is_authenticated && !auth.is_logging_in && auth.login_notice.as_deref() == Some(notice)
    })
    .await?;
    wait_for(&cx, Duration::from_secs(5), "the notice drawn", |_| {
        probes::flag_set("page-sign-in") && probes::text_of("auth-notice").as_deref() == Some(notice)
    })
    .await?;
    let saved = crate::selftests::saved_username().await?;
    ensure!(saved.is_none(), "the refused saved sign-in was kept ({saved:?})");
    ensure!(!cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?, "signed in");
    cx.log(format_args!("restarted: the sign-in page says {notice:?}; the saved sign-in is gone"));
    Ok(())
}

// ── accounts-device-limit (T4) ───────────────────────────────────────────────────────────────

/// Central's refusal over the device limit (`SessionManager::MAX_DEVICES`).
const DEVICE_LIMIT: &str = "This account is signed in on too many devices (8). Sign out on one of them first.";

async fn device_limit(cx: TestCx) -> Result<()> {
    let stack = skip_unless_ready!(cx);
    let (a, password) = throwaway("da");
    cx.log(format_args!("throwaway account {a}"));
    register(&cx, &stack.central, &a, &password).await?;
    let stops = Profile::new("accounts-device-limit-stops")?;
    let device = |i: usize| -> Result<Headless> {
        let stop = stops.0.join(format!("stop-{i}"));
        let stop = stop.display().to_string();
        Headless::start(&format!("device-{i}"), &stack.central, &["login-as", &a, &password, "wait", "logged-in", "print", "up", "wait-file", &stop, "logout", "wait", "logged-out"])
    };
    let stop = |i: usize| std::fs::write(stops.0.join(format!("stop-{i}")), b"");
    let mut devices = Vec::new();
    for i in 0..8 {
        devices.push(device(i)?);
    }
    for d in &mut devices {
        d.line(&cx, "MARK up", Duration::from_secs(60)).await?;
    }
    cx.log("eight headless devices of the account are online");
    let proxy = Proxy::start(&stack.central)?;
    let profile = Profile::new("accounts-device-limit")?;
    let env = [("DECIBELL_TEST_ACCOUNT", a.clone()), ("DECIBELL_TEST_PASSWORD", password.clone())];
    let mut app = App1::spawn("accounts-device-limit", &proxy.addr, &stack, &profile.0, &env)?;
    app.wait_mark(&cx, "refused", Duration::from_secs(60)).await?;
    stop(0)?;
    devices.remove(0).done(&cx).await?;
    cx.log("one device signed out: seven online");
    std::fs::write(profile.0.join("go-sign-in"), b"")?;
    app.wait_mark(&cx, "signed-in", Duration::from_secs(60)).await?;
    proxy.cut();
    cx.log("the app is held off by the proxy; a ninth device takes the free slot");
    let mut ninth = device(8)?;
    ninth.line(&cx, "MARK up", Duration::from_secs(60)).await?;
    proxy.restore();
    app.wait_mark(&cx, "toast", Duration::from_secs(120)).await?;
    for i in 1..=8 {
        stop(i)?;
    }
    for d in devices {
        d.done(&cx).await?;
    }
    ninth.done(&cx).await?;
    cx.log("every headless device signed out");
    app.finish(&cx).await
}

async fn check_device_limit(cx: TestCx) -> Result<()> {
    local_only()?;
    let (a, password) = (env("DECIBELL_TEST_ACCOUNT")?, env("DECIBELL_TEST_PASSWORD")?);
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    let (n, p) = (a.clone(), password.clone());
    cx.update(move |_, cx| actions::auth::sign_in(n, p, cx))?;
    wait_for(&cx, Duration::from_secs(30), "central's refusal", |cx| model(cx).auth.read(cx).login_error.is_some()).await?;
    let error = cx.update(|_, cx| model(cx).auth.read(cx).login_error.clone())?.unwrap_or_default();
    ensure!(&*error == DEVICE_LIMIT, "the refusal reads {error:?}");
    wait_for(&cx, Duration::from_secs(5), "the refusal under the form", |_| probes::text_of("auth-error").as_deref() == Some(DEVICE_LIMIT))
        .await?;
    ensure!(!cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?, "signed in as a ninth device");
    cx.log(format_args!("the ninth device's sign-in: {error:?} under the form"));
    mark("refused");
    let go = PathBuf::from(env("DECIBELL_USER_DATA_DIR")?).join("go-sign-in");
    let started = Instant::now();
    while !go.exists() {
        ensure!(started.elapsed() < Duration::from_secs(90), "no go from the test");
        cx.sleep(Duration::from_millis(100)).await;
    }
    sign_in(&cx, &a, &password).await?;
    cx.log("a slot freed: signed in");
    let epoch = cx.update(|_, cx| model(cx).dm.list.read(cx).resync_epoch())?;
    mark("signed-in");
    let reconnect = |cx: &mut App| {
        model(cx).toasts.read(cx).toasts().iter().any(|t| {
            t.severity == Severity::Warning && &*t.title == "Can't reconnect" && t.body.as_deref() == Some(DEVICE_LIMIT)
        })
    };
    wait_for(&cx, Duration::from_secs(120), "the \"Can't reconnect\" toast", reconnect).await?;
    cx.log(format_args!("the reconnect was refused: toast \"Can't reconnect\" — {DEVICE_LIMIT:?}"));
    mark("toast");
    // The headless devices go; the app's own reconnect loop gets back in (a sign-in again), then
    // forgets this device.
    wait_for(&cx, Duration::from_secs(150), "the app back in once a slot is free", move |cx| {
        model(cx).dm.list.read(cx).resync_epoch() > epoch
    })
    .await?;
    cx.log("the headless devices left: the app's reconnect got back in");
    log_out(&cx).await
}

// ── accounts-second-device (T5) ──────────────────────────────────────────────────────────────

const COMMUNITY: &str = "Design Lab";

async fn second_device(cx: TestCx) -> Result<()> {
    let stack = skip_unless_ready!(cx);
    let (a, password) = throwaway("ma");
    let b = format!("mb{}", &a[2..]);
    cx.log(format_args!("throwaway accounts {a} (the app and its second device) and {b} (a friend to be)"));
    register(&cx, &stack.central, &a, &password).await?;
    register(&cx, &stack.central, &b, &password).await?;
    let profile = Profile::new("accounts-second-device")?;
    let env = [("DECIBELL_TEST_ACCOUNT", a.clone()), ("DECIBELL_TEST_PEER", b.clone()), ("DECIBELL_TEST_PASSWORD", password.clone())];
    let mut app = App1::spawn("accounts-second-device", &stack.central, &stack, &profile.0, &env)?;
    let peer_uid = {
        let peer = Headless::start("peer-uid", &stack.central, &["login-as", &b, &password, "wait", "logged-in", "dump", "auth", "logout", "wait", "logged-out"])?;
        let seen = peer.done(&cx).await?;
        let auth = seen.iter().find(|l| l.starts_with("DUMP auth ")).context("no auth dump")?;
        let at = auth.find("\"uid\":").context("no uid")? + 6;
        auth[at..].chars().take_while(|c| c.is_ascii_digit()).collect::<String>()
    };
    app.wait_mark(&cx, "ready", Duration::from_secs(60)).await?;
    let dm = format!("from-the-other-device-{}", random_hex(6));
    let to_peer = format!("uid:{peer_uid}");
    let leave = profile.0.join("leave").display().to_string();
    let steps: [&str; 49] = [
        "login-as", &a, &password, "wait", "logged-in",
        "status", "dnd", "print", "status",
        "status-message", "on another device", "0", "print", "message",
        "friends-only", "on", "print", "friends-only",
        "dm-send", &to_peer, &dm, "print", "dm",
        "friend-add", &b, "print", "friend",
        "join", COMMUNITY, "print", "joined",
        "wait-file", &leave,
        "leave", COMMUNITY, "idle", "2",
        "status", "online", "status-message", "", "0", "friends-only", "off", "idle", "1",
        "logout", "wait", "logged-out",
    ];
    let mut other = Headless::start("other", &stack.central, &steps)?;
    other.line(&cx, "MARK joined", Duration::from_secs(120)).await?;
    app.wait_mark(&cx, "followed", Duration::from_secs(120)).await?;
    std::fs::write(profile.0.join("leave"), b"")?;
    other.done(&cx).await?;
    app.finish(&cx).await
}

async fn check_second_device(cx: TestCx) -> Result<()> {
    local_only()?;
    let (a, b, password) = (env("DECIBELL_TEST_ACCOUNT")?, env("DECIBELL_TEST_PEER")?, env("DECIBELL_TEST_PASSWORD")?);
    sign_in(&cx, &a, &password).await?;
    cx.log(format_args!("signed in as {a}; the other device acts"));
    mark("ready");
    let own = |cx: &mut App| {
        let m = model(cx);
        let o = m.live.presence.own.read(cx);
        (o.status, o.message.as_ref().map(|m| m.text.to_string()), m.settings.read(cx).get().friends_only_dms)
    };
    wait_for(&cx, Duration::from_secs(60), "Do Not Disturb set on the other device", move |cx| own(cx).0 == OwnStatus::Dnd).await?;
    cx.log("status: Do Not Disturb, as the other device set it");
    wait_for(&cx, Duration::from_secs(60), "the custom status set on the other device", move |cx| {
        own(cx).1.as_deref() == Some("on another device")
    })
    .await?;
    cx.log("custom status: \"on another device\"");
    wait_for(&cx, Duration::from_secs(60), "friends-only DMs set on the other device", move |cx| own(cx).2).await?;
    cx.log("friends-only DMs: on");
    let peer_name: SharedString = b.clone().into();
    let dm_shown = {
        let peer_name = peer_name.clone();
        move |cx: &mut App| {
            let m = model(cx);
            let list = m.dm.list.read(cx);
            list.conversations()
                .find(|c| c.username == peer_name)
                .and_then(|c| c.last_message.as_ref().map(|m| m.content.starts_with("from-the-other-device-")))
                .unwrap_or(false)
        }
    };
    wait_for(&cx, Duration::from_secs(60), "the other device's DM in our conversation list", dm_shown).await?;
    cx.log(format_args!("the DM the other device sent to {b} is the conversation's last message here"));
    let friend = {
        let peer_name = peer_name.clone();
        move |cx: &mut App| model(cx).live.friends.read(cx).list().iter().any(|f| f.username == peer_name && f.status == FriendStatus::PendingOutgoing)
    };
    wait_for(&cx, Duration::from_secs(60), "the friend request in our list", friend).await?;
    cx.log(format_args!("the friend request to {b}: pending, outgoing"));
    let joined = |cx: &mut App| {
        let m = model(cx);
        m.servers.read(cx).servers().iter().find(|s| &*s.name == COMMUNITY).is_some_and(|s| m.connections.read(cx).is_connected(s.id))
    };
    wait_for(&cx, Duration::from_secs(60), "the community the other device joined", joined).await?;
    let tile = cx.update(|_, cx| model(cx).servers.read(cx).servers().iter().find(|s| &*s.name == COMMUNITY).map(|s| s.id))?.context("gone")?;
    wait_for(&cx, Duration::from_secs(10), "its server bar tile", move |_| probes::bounds(&format!("server-tile:{}", tile.wire())).is_some())
        .await?;
    cx.log(format_args!("{COMMUNITY}: joined here too, connected, its tile drawn"));
    mark("followed");
    wait_for(&cx, Duration::from_secs(90), "the other device to leave the community", move |cx| !joined(cx)).await?;
    cx.log(format_args!("the other device left {COMMUNITY}: gone here too"));
    log_out(&cx).await
}
