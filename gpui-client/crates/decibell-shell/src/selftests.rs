//! The shell's self-tests, against the local test stack (`tools/dev/README.md`) as `perf1`.
//!
//! Each `shell-*` self-test starts the app again in a child process — `decibell --check
//! shell-… --login perf1` with its own temporary profile (`DECIBELL_USER_DATA_DIR`), the stack's
//! `DECIBELL_CENTRAL` and `DECIBELL_TEST_PROBES=1` — and passes when the check inside it does: the
//! core runs once per process and owns its profile, so a sign-in test can't share the test
//! window's process. They need a binary built with `dev-local` (`--login`); they refuse to run
//! unless `DECIBELL_CENTRAL` is loopback and skip, saying so, when the stack is down.
//!
//! The checks ([`checks`]) click and scroll the real window at the places the shell's probes
//! recorded ([`crate::probes`]) and read the model.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx, perf};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{Model, ServerId, actions};
use gpui::{
    App, Bounds, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput, Point, ScrollDelta, ScrollEvent,
    TouchPhase, point, px,
};

use crate::probes;

const ACCOUNT: &str = "perf1";
const DAILY_GAMING: &str = "Daily Gaming";

// ── The parents: one child app each ───────────────────────────────────────────────────────

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "shell-signin",
            about: "--login perf1 (local stack): the main view, Daily Gaming's tile connects, its channels render",
            run: |cx| Box::pin(parent(cx, "shell-signin")),
        },
        SelfTest {
            name: "shell-navigate",
            about: "click the tile, a channel, the server menu (open, close), a member's menu, Home: frames, then 0",
            run: |cx| Box::pin(parent(cx, "shell-navigate")),
        },
        SelfTest {
            name: "shell-members",
            about: "the member list: scrolling pages every member of Daily Gaming in (as many as the server counts), counts as the server's",
            run: |cx| Box::pin(parent(cx, "shell-members")),
        },
        SelfTest {
            name: "shell-idle",
            about: "#general open: 0 frames over 10 s unfocused; focused only the glow's capped presents",
            run: |cx| Box::pin(parent(cx, "shell-idle")),
        },
        SelfTest {
            name: "shell-signout",
            about: "Log out returns to the sign-in page and forgets the saved sign-in",
            run: |cx| Box::pin(parent(cx, "shell-signout")),
        },
        SelfTest {
            name: "shell-no-keyring",
            about: "no Secret Service on the session bus: sign in works, and the saved sign-in opens on the next start",
            run: |cx| Box::pin(no_keyring(cx)),
        },
    ]
    .into_iter()
    .chain(crate::account_tests::selftests())
    .collect()
}

/// The stack as `accounts.json` describes it.
pub(crate) struct Stack {
    pub(crate) central: String,
    pub(crate) password: String,
}

pub(crate) fn stack() -> Result<Option<Stack>> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let path = dev.join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let json: serde_like::Value = serde_like::parse(&text).with_context(|| format!("parsing {}", path.display()))?;
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json.str("central")).context("no central address")?;
    let password = json.str("password").context("accounts.json has no password")?;
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the shell's tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
}

/// A temporary profile, removed when dropped.
pub(crate) struct Profile(pub(crate) PathBuf);

impl Profile {
    pub(crate) fn new(name: &str) -> Result<Profile> {
        let dir = std::env::temp_dir().join(format!("decibell-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(Profile(dir))
    }
}

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// How a child ended.
pub(crate) enum Child {
    Passed,
    /// The binary has no `--login` (not built with `dev-local`).
    NoDevLogin,
}

/// Run `decibell --check <check>` (signing in as perf1 when `login`) and relay its log.
pub(crate) async fn run_child(cx: &TestCx, check: &str, login: bool, stack: &Stack, profile: &Path, env: &[(&str, String)]) -> Result<Child> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.arg("--check").arg(check);
    if login {
        command.arg("--login").arg(ACCOUNT);
    }
    command
        .env("DECIBELL_USER_DATA_DIR", profile)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    let started = Instant::now();
    let mut child = command.spawn().context("starting the app")?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let output = cx
        .app()
        .background_executor()
        .spawn(async move {
            let err = std::thread::Builder::new()
                .name("decibell-test-stderr".into())
                .spawn(move || std::io::BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<_>>())
                .ok();
            let out: Vec<String> = std::io::BufReader::new(stdout).lines().map_while(Result::ok).collect();
            let status = child.wait();
            let err = err.and_then(|h| h.join().ok()).unwrap_or_default();
            (status, out, err)
        })
        .await;
    let (status, out, err) = output;
    let status = status?;
    for line in out.iter().filter(|l| !l.starts_with("selftest:")) {
        cx.log(format_args!("│ {line}"));
    }
    if err.iter().any(|l| l.contains("--login refused")) && err.iter().any(|l| l.contains("dev-local")) {
        return Ok(Child::NoDevLogin);
    }
    if !status.success() {
        for line in err.iter().rev().take(12).rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{check} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(Child::Passed)
}

async fn parent(cx: TestCx, check: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile::new(check)?;
    match run_child(&cx, check, true, &stack, &profile.0, &[]).await? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

/// A private session bus with nothing on it (no Secret Service can be activated: no service
/// directories), for as long as it is held.
struct EmptyBus {
    pid: u32,
    address: String,
    _dir: Profile,
}

impl EmptyBus {
    fn start() -> Result<EmptyBus> {
        let dir = Profile::new("empty-bus")?;
        let socket = dir.0.join("bus");
        let config = dir.0.join("bus.conf");
        std::fs::write(
            &config,
            format!(
                r#"<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path={}</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>"#,
                socket.display()
            ),
        )?;
        let out = Command::new("dbus-daemon")
            .arg(format!("--config-file={}", config.display()))
            .args(["--fork", "--print-pid=1", "--print-address=1"])
            .output()
            .context("starting dbus-daemon")?;
        ensure!(out.status.success(), "dbus-daemon: {}", String::from_utf8_lossy(&out.stderr));
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        let address = lines.next().context("dbus-daemon printed no address")?.to_string();
        let pid = lines.next().and_then(|l| l.trim().parse().ok()).context("dbus-daemon printed no pid")?;
        Ok(EmptyBus { pid, address, _dir: dir })
    }
}

impl Drop for EmptyBus {
    fn drop(&mut self) {
        let _ = Command::new("kill").arg(self.pid.to_string()).status();
    }
}

async fn no_keyring(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let bus = EmptyBus::start()?;
    cx.log(format_args!("private session bus without a Secret Service: {}", bus.address));
    let profile = Profile::new("shell-no-keyring")?;
    // A development build keeps its key in a file; this test is the one that asks for the real
    // keychain path, on a bus where there is none (so nothing is ever written to a keyring).
    let env = |phase: &str| {
        vec![
            ("DBUS_SESSION_BUS_ADDRESS", bus.address.clone()),
            ("DECIBELL_DEV_KEYCHAIN", "real".to_string()),
            ("DECIBELL_CHECK_PHASE", phase.to_string()),
        ]
    };
    match run_child(&cx, "shell-no-keyring", true, &stack, &profile.0, &env("1")).await? {
        Child::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            return Ok(());
        }
        Child::Passed => {}
    }
    cx.log("restarting on the same profile, without --login: the saved sign-in must sign in");
    run_child(&cx, "shell-no-keyring", false, &stack, &profile.0, &env("2")).await?;
    Ok(())
}

// ── The checks, inside the signed-in app ────────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "shell-signin", about: "(in the app) signed in, Daily Gaming connected, channels drawn", run: |cx| Box::pin(check_signin(cx)) },
        SelfTest { name: "shell-navigate", about: "(in the app) clicks and their frames", run: |cx| Box::pin(check_navigate(cx)) },
        SelfTest { name: "shell-members", about: "(in the app) the roster pages in", run: |cx| Box::pin(check_members(cx)) },
        SelfTest { name: "shell-idle", about: "(in the app) idle frames, unfocused and focused", run: |cx| Box::pin(check_idle(cx)) },
        SelfTest { name: "shell-signout", about: "(in the app) Log out", run: |cx| Box::pin(check_signout(cx)) },
        SelfTest { name: "shell-no-keyring", about: "(in the app) sign-in and saved sign-in without a keychain", run: |cx| Box::pin(check_no_keyring(cx)) },
    ]
    .into_iter()
    .chain(crate::account_tests::checks())
    .collect()
}

/// Poll `ready` every 50 ms for at most `limit`.
pub(crate) async fn wait_for(cx: &TestCx, limit: Duration, what: &str, ready: impl Fn(&mut App) -> bool) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if cx.update(|_, cx| ready(cx))? {
            return Ok(started.elapsed());
        }
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {what}", limit.as_secs());
        cx.sleep(Duration::from_millis(50)).await;
    }
}

pub(crate) fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

fn daily_gaming() -> ServerId {
    ServerId::from_central(1)
}

fn centre(bounds: Bounds<gpui::Pixels>) -> Point<gpui::Pixels> {
    bounds.center()
}

/// Click the middle of a probed element, then rest the pointer where nothing reacts to it (a
/// pointer left on a tile would open its tooltip, a frame that isn't the click's).
pub(crate) async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    let bounds = wait_probe(cx, key).await?;
    cx.click(centre(bounds))?;
    park_pointer(cx)
}

/// The pointer over the pane's empty middle (the chat placeholder; Home's list ends above it).
pub(crate) fn park_pointer(cx: &TestCx) -> Result<()> {
    let at = cx.update(|window, _| {
        let size = window.viewport_size();
        point(size.width * 0.55, size.height * 0.85)
    })?;
    cx.mouse_move(at)
}

pub(crate) async fn wait_probe(cx: &TestCx, key: &str) -> Result<Bounds<gpui::Pixels>> {
    let key_owned = key.to_string();
    wait_for(cx, Duration::from_secs(10), &format!("{key} on screen"), move |_| probes::bounds(&key_owned).is_some()).await?;
    probes::bounds(key).context("probe vanished")
}

fn right_click(cx: &TestCx, position: Point<gpui::Pixels>) -> Result<()> {
    cx.mouse_move(position)?;
    cx.update(|window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent {
                button: MouseButton::Right,
                position,
                modifiers: Modifiers::default(),
                click_count: 1,
                first_mouse: false,
            }),
            cx,
        );
        window.dispatch_event(
            PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Right, position, modifiers: Modifiers::default(), click_count: 1 }),
            cx,
        );
    })?;
    park_pointer(cx)
}

/// Signed in and Daily Gaming connected, with how long each took.
async fn signed_in(cx: &TestCx) -> Result<()> {
    let t = wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.log(format_args!("signed in as {ACCOUNT} after {} ms", t.as_millis()));
    let t = wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    cx.log(format_args!("{DAILY_GAMING} connected {} ms later", t.as_millis()));
    Ok(())
}

/// Daily Gaming open on `#general` by clicks, its channel list drawn.
async fn open_general(cx: &TestCx) -> Result<()> {
    click_probe(cx, "server-tile:1").await?;
    let opened = wait_for(cx, Duration::from_secs(10), "Daily Gaming's channels", |cx| {
        let m = model(cx);
        m.nav.read(cx).active_server == Some(daily_gaming())
            && m.community(daily_gaming(), cx).is_some_and(|c| !c.channels.read(cx).list().is_empty())
    })
    .await;
    if let Err(e) = opened {
        // Where the click went: what the server bar's press opened, what lay over it.
        let state = cx.update(|window, cx| {
            let m = model(cx);
            format!(
                "active server {:?}, its channels {:?}, a modal {:?}, the tile at {:?}, window active {}",
                m.nav.read(cx).active_server,
                m.community(daily_gaming(), cx).map(|c| c.channels.read(cx).list().len()),
                m.overlays.read(cx).active_modal,
                probes::bounds("server-tile:1"),
                window.is_window_active()
            )
        })?;
        bail!("{e:#} ({state})");
    }
    click_probe(cx, "channel:general").await?;
    wait_for(cx, Duration::from_secs(5), "#general", |cx| model(cx).nav.read(cx).active_channel.as_deref() == Some("general")).await?;
    Ok(())
}

/// Nothing drawn or presented for `seconds` after drawing stopped (a pointer crossing the window
/// draws: up to three tries, as the app's idle test).
async fn assert_idle(cx: &TestCx, seconds: u64, what: &str) -> Result<()> {
    let mut last = Frames { drawn: 0, presented: 0 };
    for _ in 0..3 {
        let before = cx.settle().await?;
        cx.sleep(Duration::from_secs(seconds)).await;
        last = cx.frames()?.since(before);
        if last.drawn == 0 && last.presented == 0 {
            return Ok(());
        }
    }
    bail!("{what}: not idle ({} drawn, {} presented in {seconds} s)", last.drawn, last.presented)
}

async fn check_signin(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    wait_probe(&cx, "home").await?;
    wait_probe(&cx, "server-tile:1").await?;
    open_general(&cx).await?;
    // Every channel perf1 may see has its row (categories are headers; voice rows by their id).
    let listed: Vec<(String, String, bool)> = cx.update(|_, cx| {
        model(cx).community(daily_gaming(), cx).map_or(Vec::new(), |c| {
            c.channels
                .read(cx)
                .list()
                .iter()
                .filter(|c| c.kind != decibell_model::stores::community::ChannelKind::Category)
                .map(|c| (c.id.to_string(), c.name.to_string(), c.kind == decibell_model::stores::community::ChannelKind::Voice))
                .collect()
        })
    })?;
    let missing: Vec<&str> = listed
        .iter()
        .filter(|(id, _, voice)| probes::bounds(&format!("{}:{id}", if *voice { "voice" } else { "channel" })).is_none())
        .map(|(_, name, _)| name.as_str())
        .collect();
    cx.log(format_args!(
        "{} channels drawn ({})",
        listed.len(),
        listed.iter().map(|(_, n, _)| n.as_str()).collect::<Vec<_>>().join(", ")
    ));
    ensure!(missing.is_empty(), "rows missing from the channel list: {missing:?}");
    Ok(())
}

/// Waits until nothing has been *drawn* for 1 s (the glow may keep presenting: it re-presents the
/// retained scene without drawing) and returns the counters. A second, not GPUI's 250 ms: what a
/// navigation asks for arrives over the network (the presence subscription goes out 600 ms after
/// the rows that want it are drawn, and central's answer redraws the dots).
async fn settle_drawn(cx: &TestCx) -> Result<Frames> {
    let started = Instant::now();
    let mut last = cx.frames()?;
    let mut quiet_since = Instant::now();
    loop {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames()?;
        if now.drawn != last.drawn {
            quiet_since = Instant::now();
        } else if quiet_since.elapsed() >= Duration::from_secs(1) {
            return Ok(now);
        }
        last = now;
        ensure!(started.elapsed() < Duration::from_secs(15), "the window was still drawing after 15 s");
    }
}

/// One interaction: what it drew, then no frame drawn for a second.
async fn step(cx: &TestCx, what: &str, act: impl AsyncFnOnce(&TestCx) -> Result<()>, done: impl Fn(&mut App) -> bool) -> Result<()> {
    let before = settle_drawn(cx).await?;
    act(cx).await?;
    wait_for(cx, Duration::from_secs(5), what, done).await?;
    let mut after = settle_drawn(cx).await?;
    let frames = after.since(before);
    ensure!(frames.drawn >= 1, "{what} drew nothing");
    // Then a second without a drawn frame. Another window taking or giving back the focus redraws
    // the whole window (GPUI refreshes on activation): a real leak shows in every try.
    let mut idle = Frames { drawn: 0, presented: 0 };
    for attempt in 1..=3 {
        cx.sleep(Duration::from_secs(1)).await;
        idle = cx.frames()?.since(after);
        if idle.drawn == 0 {
            break;
        }
        cx.log(format_args!("{what}: try {attempt}: {} drawn in the idle second; again", idle.drawn));
        after = settle_drawn(cx).await?;
    }
    cx.log(format_args!(
        "{what}: {} drawn / {} presented; then {} drawn / {} presented in 1 s",
        frames.drawn, frames.presented, idle.drawn, idle.presented
    ));
    ensure!(idle.drawn == 0, "{what}: frames drawn after it settled");
    ensure!(idle.presented as f32 <= crate::glow::BREATHE_FPS + 2., "{what}: more presents than the glow's cap");
    Ok(())
}

fn nav(cx: &App) -> (ActiveView, Option<ServerId>, Option<gpui::SharedString>) {
    let nav = model(cx).nav.read(cx);
    (nav.active_view, nav.active_server, nav.active_channel.clone())
}

async fn check_navigate(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    wait_probe(&cx, "server-tile:1").await?;
    step(&cx, "the Daily Gaming tile", async |cx| click_probe(cx, "server-tile:1").await, |cx| {
        let (view, server, _) = nav(cx);
        view == ActiveView::Server && server == Some(daily_gaming())
    })
    .await?;
    wait_probe(&cx, "channel:general").await?;
    step(&cx, "#general", async |cx| click_probe(cx, "channel:general").await, |cx| nav(cx).2.as_deref() == Some("general")).await?;
    step(&cx, "the server menu opens", async |cx| click_probe(cx, "server-menu-button").await, |_| probes::flag_set("server-menu-open"))
        .await?;
    step(&cx, "the server menu closes (a click outside it)", async |cx| click_probe(cx, "channel:general").await, |_| {
        !probes::flag_set("server-menu-open")
    })
    .await?;
    // A member's user menu: the request is the shell's; its body is the profile feature's.
    // The first row of the member list (who is online on the stack varies).
    let first = cx.update(|_, cx| {
        let community = model(cx).community(daily_gaming(), cx).context("not connected")?;
        let visible = community.roster.read(cx).visible();
        visible.online.first().or(visible.offline.first()).copied().context("an empty roster")
    })??;
    let member = wait_probe(&cx, &format!("member:{first}")).await?;
    step(&cx, "a member's user menu (right click)", async |cx| right_click(cx, member.center()), move |cx| {
        model(cx).overlays.read(cx).context_menu.as_ref().is_some_and(|m| m.uid == first)
    })
    .await?;
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_context_menu(cx)))?;
    step(&cx, "Home", async |cx| click_probe(cx, "home").await, |cx| nav(cx).0 == ActiveView::Home).await?;
    assert_idle(&cx, 2, "Home").await?;
    Ok(())
}

async fn check_members(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    open_general(&cx).await?;
    wait_for(&cx, Duration::from_secs(10), "the roster's first page", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.roster.read(cx).meta().is_some())
    })
    .await?;
    let counts = |cx: &mut App| {
        let community = model(cx).community(daily_gaming(), cx).expect("connected");
        let roster = community.roster.read(cx);
        let meta = roster.meta().cloned().expect("a page");
        let visible = roster.visible();
        (meta.total_members, meta.online_count, visible.online.len(), visible.offline.len(), meta.has_more)
    };
    let (total, online, shown_on, shown_off, more) = cx.update(|_, cx| counts(cx))?;
    cx.log(format_args!("first page: {total} members, {online} online; drawn {shown_on} online + {shown_off} offline, more: {more}"));
    // The stack seeded 298; throwaway accounts of other tests join it. Whatever the server
    // answers is what every member must be paged in to.
    ensure!(total >= 298, "Daily Gaming has {total} members, the stack seeded 298");
    let list = wait_probe(&cx, "member-list").await?;
    let started = Instant::now();
    let mut scrolls = 0;
    loop {
        let (_, _, on, off, more) = cx.update(|_, cx| counts(cx))?;
        if !more && on + off == total as usize {
            break;
        }
        ensure!(started.elapsed() < Duration::from_secs(60), "the roster stopped paging at {} of {total}", on + off);
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: list.center(),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-1200.))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                    source: Default::default(),
                }),
                cx,
            )
        })?;
        scrolls += 1;
        cx.sleep(Duration::from_millis(120)).await;
    }
    let (total, online, on, off, _) = cx.update(|_, cx| counts(cx))?;
    cx.log(format_args!(
        "paged in after {scrolls} scrolls ({:.1} s): {on} online + {off} offline = {}, {online} online by the server",
        started.elapsed().as_secs_f64(),
        on + off
    ));
    ensure!(on as i64 == online && (on + off) as i64 == total, "the counts don't match the server's");
    // Scroll to the end: the last member's row is drawn (only rows near the viewport are built).
    let last = cx.update(|_, cx| {
        let community = model(cx).community(daily_gaming(), cx).expect("connected");
        let visible = community.roster.read(cx).visible();
        visible.offline.last().or(visible.online.last()).copied()
    })?;
    let last = last.context("an empty roster")?;
    let key = format!("member:{last}");
    let started = Instant::now();
    while !probes::bounds(&key).is_some_and(|b| b.top() < list.bottom() && b.bottom() > list.top()) {
        ensure!(started.elapsed() < Duration::from_secs(20), "the list never reached its last row");
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: list.center(),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-1200.))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                    source: Default::default(),
                }),
                cx,
            )
        })?;
        cx.sleep(Duration::from_millis(60)).await;
    }
    cx.log(format_args!("scrolled to the end in {:.1} s: the last member's row is drawn", started.elapsed().as_secs_f64()));
    Ok(())
}

/// `tools/perf/winctl.sh` (KWin scripting: focus another window, or ours), found above the binary.
fn winctl(verb: &str) -> Result<()> {
    let exe = std::env::current_exe()?;
    let script = exe
        .ancestors()
        .map(|dir| dir.join("tools/perf/winctl.sh"))
        .find(|path| path.exists())
        .context("tools/perf/winctl.sh not found above the binary")?;
    let status = Command::new(script)
        .arg(verb)
        .arg(std::process::id().to_string())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()?;
    ensure!(status.success(), "winctl.sh {verb} failed ({status})");
    Ok(())
}

/// Sleeps `duration` (polling every 100 ms: the main-thread wakeups logged with it include these;
/// `tools/perf` measures the real figure), watching what would draw a frame that isn't the app's own: the focus
/// changing hands, the pointer moving over the window. Returns what happened ("" when nothing).
async fn watched_sleep(cx: &TestCx, duration: Duration) -> Result<String> {
    let state = |cx: &TestCx| cx.update(|window, _| (window.is_window_active(), window.mouse_position()));
    let start = state(cx)?;
    let mut flips = 0;
    let mut moved = false;
    let mut last = start;
    let started = Instant::now();
    while started.elapsed() < duration {
        cx.sleep(Duration::from_millis(100)).await;
        let now = state(cx)?;
        flips += usize::from(now.0 != last.0);
        moved |= now.1 != last.1;
        last = now;
    }
    Ok(match (flips, moved) {
        (0, false) => String::new(),
        (flips, moved) => format!(" (disturbed: {flips} focus changes{})", if moved { ", the pointer moved" } else { "" }),
    })
}

/// Core events the pump has delivered so far.
fn core_events(cx: &TestCx) -> Result<u64> {
    cx.update(|_, cx| model(cx).pump_stats().events)
}

async fn wait_active(cx: &TestCx, active: bool) -> bool {
    wait_for(cx, Duration::from_secs(3), "the window's focus", |_| true).await.ok();
    for _ in 0..60 {
        if cx.update(|window, _| window.is_window_active()).unwrap_or(!active) == active {
            return true;
        }
        cx.sleep(Duration::from_millis(50)).await;
    }
    false
}

async fn check_idle(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    open_general(&cx).await?;

    // Unfocused (KWin focuses the desktop): nothing drawn or presented, the main thread asleep.
    // The stack is shared (other clients sign in, join voice: roster and presence events redraw
    // what they change), and other windows can take or give the focus: a try counts only when
    // no core event arrived in it; up to five tries.
    let mut last = String::new();
    let mut measured = false;
    for attempt in 1..=5 {
        winctl("unfocus")?;
        if !wait_active(&cx, false).await {
            last = "the window kept the focus (winctl.sh unfocus)".into();
            continue;
        }
        let Ok(before) = cx.settle().await else {
            last = "still drawing after 10 s".into();
            continue;
        };
        let wakeups_before = perf::thread_wakeups();
        let events_before = core_events(&cx)?;
        let disturbed = watched_sleep(&cx, Duration::from_secs(10)).await?;
        let frames = cx.frames()?.since(before);
        let wakeups = perf::thread_wakeups().zip(wakeups_before).map(|(a, b)| a - b);
        let events = core_events(&cx)? - events_before;
        let still_unfocused = !cx.update(|window, _| window.is_window_active())?;
        last = format!(
            "try {attempt}: {} drawn, {} presented, {} main-thread wakeups in 10 s unfocused; {events} core events{disturbed}",
            frames.drawn,
            frames.presented,
            wakeups.map_or("?".into(), |w| w.to_string())
        );
        cx.log(&last);
        if events > 0 || !still_unfocused || !disturbed.is_empty() {
            continue;
        }
        ensure!(frames.drawn == 0 && frames.presented == 0, "unfocused and quiet, yet it drew: {last}");
        measured = true;
        break;
    }
    // Every try disturbed (the desktop is shared): nothing measured, which is not a pass.
    if !measured {
        return Err(decibell_harness::inconclusive(format_args!("unfocused, #general open: no undisturbed try in five ({last})")));
    }

    // Focused: the glow breathes by re-presenting, at most BREATHE_FPS a second; nothing drawn.
    let mut last = String::new();
    let mut measured = false;
    for attempt in 1..=5 {
        winctl("activate")?;
        if !wait_active(&cx, true).await {
            last = "the window didn't get the focus (winctl.sh activate)".into();
            continue;
        }
        settle_drawn(&cx).await?;
        let before = cx.frames()?;
        let wakeups_before = perf::thread_wakeups();
        let events_before = core_events(&cx)?;
        let disturbed = watched_sleep(&cx, Duration::from_secs(5)).await?;
        let frames = cx.frames()?.since(before);
        let wakeups = perf::thread_wakeups().zip(wakeups_before).map(|(a, b)| a - b);
        let events = core_events(&cx)? - events_before;
        let still_focused = cx.update(|window, _| window.is_window_active())?;
        last = format!(
            "try {attempt}: {} drawn, {} presented in 5 s focused ({:.1} presents/s, cap {}); main-thread wakeups {}; {events} core events{disturbed}",
            frames.drawn,
            frames.presented,
            frames.presented as f64 / 5.,
            crate::glow::BREATHE_FPS,
            wakeups.map_or("?".into(), |w| w.to_string())
        );
        cx.log(&last);
        if events > 0 || !still_focused || !disturbed.is_empty() {
            continue;
        }
        // Presents can be 0 while focused: a compositor sends no frame callbacks to a covered
        // window, and the scene's animation waits for them.
        ensure!(frames.drawn == 0, "focused and quiet, yet it drew: {last}");
        ensure!(frames.presented as f32 <= 5. * (crate::glow::BREATHE_FPS + 2.), "the glow presented above its cap: {last}");
        measured = true;
        break;
    }
    if !measured {
        return Err(decibell_harness::inconclusive(format_args!("focused, #general open: no undisturbed try in five ({last})")));
    }
    Ok(())
}

pub(crate) async fn saved_username() -> Result<Option<String>> {
    let config = decibell_core::api::settings::load_config().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    Ok(config["saved_username"].as_str().map(str::to_string))
}

async fn check_signout(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let saved = saved_username().await?;
    cx.log(format_args!("saved sign-in after signing in: {saved:?}"));
    ensure!(saved.as_deref() == Some(ACCOUNT), "the sign-in wasn't saved");
    cx.update(|_, cx| actions::auth::sign_out(cx))?;
    let t = wait_for(&cx, Duration::from_secs(10), "the sign-in page", |cx| {
        let auth = model(cx).auth.read(cx);
        !auth.is_authenticated && probes::flag_set("page-sign-in")
    })
    .await?;
    let saved = saved_username().await?;
    cx.log(format_args!("signed out in {} ms; saved sign-in now {saved:?}", t.as_millis()));
    ensure!(saved.is_none(), "Log out kept the saved sign-in");
    Ok(())
}

async fn check_no_keyring(cx: TestCx) -> Result<()> {
    let phase = std::env::var("DECIBELL_CHECK_PHASE").unwrap_or_else(|_| "1".into());
    signed_in(&cx).await?;
    let saved = saved_username().await?;
    cx.log(format_args!("phase {phase}: signed in; saved sign-in {saved:?}"));
    ensure!(saved.as_deref() == Some(ACCOUNT), "no saved sign-in without a keychain");
    Ok(())
}

/// The few `accounts.json` reads the parents need, without a JSON dependency in the shipped
/// crate: top-level string fields.
mod serde_like {
    pub struct Value(String);

    pub fn parse(text: &str) -> anyhow::Result<Value> {
        anyhow::ensure!(text.trim_start().starts_with('{'), "not a JSON object");
        Ok(Value(text.to_string()))
    }

    impl Value {
        /// `"key": "value"` at any depth, first match (the fields read here are unique).
        pub fn str(&self, key: &str) -> Option<String> {
            let needle = format!("\"{key}\"");
            let at = self.0.find(&needle)? + needle.len();
            let rest = self.0[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
            Some(rest[..rest.find('"')?].to_string())
        }
    }
}
