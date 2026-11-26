//! The self-tests, against the local test stack (`tools/dev/README.md`).
//!
//! Each starts the app again in a child process (`decibell --check <name>`, its own temporary
//! profile, the stack's `DECIBELL_CENTRAL`, `DECIBELL_TEST_PROBES=1`), as the shell's and
//! Settings' do, and passes when the check inside passes. They refuse unless `DECIBELL_CENTRAL` is
//! loopback, and skip, saying so, when the stack is down or the binary has no `--login`
//! (`dev-local`).
//!
//! **The stack is shared with other agents' tests.** Joining and leaving use a **throwaway
//! account** registered for the run (`t<random>`), never a seeded one. As `carol` (Design Lab's
//! owner) the checks change only what they create — channels and categories named `sv-<pid>…`,
//! invites they made — and delete it again; reorders happen among those only, in Design Lab, and
//! the seeded channels' order is asserted unchanged at the end.

use std::cell::Cell;
use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_core::api;
use decibell_core::events::CoreEvent;
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_model::live::friends::{FriendAction, FriendStatus};
use decibell_model::stores::community::{Channel, ChannelKind};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::overlays::modal;
use decibell_model::{ChannelId, Model, ServerId, actions};
use decibell_shell::probes;
use decibell_ui::visible_window_bounds;
use gpui::{App, Bounds, Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, Pixels, PlatformInput, Point, point, px};

use crate::browse::BrowseView;
use crate::create_channel::CreateChannelOverlay;
use crate::invite_friends::{InviteFriendsOverlay, RowState};
use crate::invites::InviteManageOverlay;
use crate::reorder;

const MARK: &str = "SERVERS-MARK";
const DESIGN_LAB: &str = "Design Lab";
const CHILD_LIMIT: Duration = Duration::from_secs(600);

// ── The parents ──────────────────────────────────────────────────────────────────────────────

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "servers-browse-join",
            about: "throwaway account: the directory lists the stack's public communities; join Design Lab from it; leave by the menu",
            run: |cx| Box::pin(throwaway(cx, "servers-browse-join")),
        },
        SelfTest {
            name: "servers-join-link",
            about: "throwaway account: join by the deep-link dialog, a pasted code-only link and an old host:port link; bad links' errors",
            run: |cx| Box::pin(throwaway(cx, "servers-join-link")),
        },
        SelfTest {
            name: "invite-manage-live",
            about: "carol: create an invite with expiry and max uses, it lists, Copy link puts the code-only link on the clipboard, revoke",
            run: |cx| Box::pin(invites_test(cx)),
        },
        SelfTest {
            name: "invite-friends-live",
            about: "three throwaway accounts: the picker lists only the friend who isn't a member; the invite reaches them as a DM",
            run: |cx| Box::pin(friends(cx)),
        },
        SelfTest {
            name: "channel-create-delete",
            about: "carol, Design Lab: create text, voice, category and encrypted channels through the menu and dialog; delete each",
            run: |cx| Box::pin(carol(cx, "channel-create-delete")),
        },
        SelfTest {
            name: "channel-reorder",
            about: "carol, Design Lab: drag own channels within and across categories and a category; echo; a second session agrees; Esc",
            run: |cx| Box::pin(reorder_test(cx)),
        },
        SelfTest {
            name: "category-sync-drop",
            about: "carol, Design Lab: a drop into a syncing category asks; Cancel puts it back, Keep keeps, Sync syncs the overwrites",
            run: |cx| Box::pin(carol(cx, "category-sync-drop")),
        },
        SelfTest {
            name: "welcome-banner",
            about: "carol, Design Lab: a fresh channel shows the welcome banner; Invite friends opens the dialog",
            run: |cx| Box::pin(carol(cx, "welcome-banner")),
        },
        SelfTest {
            name: "servers-idle",
            about: "Browse and the Invites dialog at rest: 0 frames over 5 s; a drag draws only while the pointer moves",
            run: |cx| Box::pin(carol(cx, "servers-idle")),
        },
    ]
}

struct Stack {
    central: String,
    password: String,
}

fn dev_dir() -> PathBuf {
    std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"))
}

fn is_loopback(central: &str) -> bool {
    let host = central.rsplit_once(':').map_or(central, |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn accounts_json() -> Result<Option<serde_json::Value>> {
    let path = dev_dir().join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    Ok(Some(serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?))
}

fn stack() -> Result<Option<Stack>> {
    let Some(json) = accounts_json()? else { return Ok(None) };
    let central = std::env::var("DECIBELL_CENTRAL")
        .ok()
        .or_else(|| json["central"].as_str().map(str::to_string))
        .context("no central address")?;
    let password = json["password"].as_str().context("accounts.json has no password")?.to_string();
    ensure!(is_loopback(&central), "DECIBELL_CENTRAL={central} is not loopback: these tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
}

/// A community of the stack: central's id, its endpoint and its seeded invite code.
struct Community {
    id: ServerId,
    host: String,
    port: u16,
    invite: String,
}

fn community(name: &str) -> Result<Community> {
    let json = accounts_json()?.context("no accounts.json")?;
    let c = json["communities"]
        .as_array()
        .and_then(|a| a.iter().find(|c| c["name"].as_str() == Some(name)))
        .with_context(|| format!("{name} isn't in accounts.json"))?;
    Ok(Community {
        id: ServerId::from_central(c["server_id"].as_i64().context("server_id")? as i32),
        host: c["host"].as_str().context("host")?.to_string(),
        port: c["port"].as_u64().context("port")? as u16,
        invite: c["invite"].as_str().context("invite")?.to_string(),
    })
}

/// A temporary profile, removed when dropped.
struct Profile(PathBuf);

impl Profile {
    fn new(name: &str) -> Result<Profile> {
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

fn random_hex(n: usize) -> String {
    let mut h = RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    let a = h.finish();
    let mut h2 = RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())[..n].to_string()
}

/// A child app running a check.
struct Running {
    name: String,
    child: std::process::Child,
    lines: Arc<Mutex<Vec<String>>>,
    started: Instant,
}

fn spawn(check: &str, login: Option<&str>, stack: &Stack, profile: &Path, env: &[(&str, String)]) -> Result<Running> {
    spawn_without(check, login, stack, profile, env, &[])
}

fn spawn_without(check: &str, login: Option<&str>, stack: &Stack, profile: &Path, env: &[(&str, String)], remove: &[&str]) -> Result<Running> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command.arg("--check").arg(check);
    if let Some(account) = login {
        command.arg("--login").arg(account);
    }
    command
        .env("DECIBELL_USER_DATA_DIR", profile)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env("DECIBELL_TEST_RUN", std::process::id().to_string())
        .env_remove("DECIBELL_PERF")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
    }
    for key in remove {
        command.env_remove(key);
    }
    let mut child = command.spawn().context("starting the app")?;
    let lines: Arc<Mutex<Vec<String>>> = Arc::default();
    let streams: [(Option<Box<dyn std::io::Read + Send>>, &str); 2] = [
        (child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), ""),
        (child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), "stderr: "),
    ];
    for (stream, tag) in streams {
        let Some(stream) = stream else { continue };
        let lines = lines.clone();
        std::thread::Builder::new()
            .name("decibell-test-pipe".into())
            .spawn(move || {
                for line in std::io::BufReader::new(stream).lines().map_while(Result::ok) {
                    lines.lock().unwrap_or_else(|e| e.into_inner()).push(format!("{tag}{line}"));
                }
            })
            .ok();
    }
    Ok(Running { name: check.to_string(), child, lines, started: Instant::now() })
}

impl Running {
    fn has(&self, needle: &str) -> bool {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).iter().any(|l| l.contains(needle))
    }

    fn mark_value(&self, mark: &str) -> Option<String> {
        let needle = format!("{MARK} {mark} ");
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).iter().find_map(|l| l.split_once(&needle).map(|(_, v)| v.to_string()))
    }

    fn no_dev_login(&self) -> bool {
        self.has("--login refused") && self.has("dev-local")
    }

    async fn wait_mark(&mut self, cx: &TestCx, mark: &str, limit: Duration) -> Result<()> {
        let needle = format!("{MARK} {mark}");
        let started = Instant::now();
        loop {
            if self.has(&needle) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                self.relay(cx, true);
                bail!("{} ended ({status}) before {mark}", self.name);
            }
            ensure!(started.elapsed() < limit, "{}: no {mark} after {} s", self.name, limit.as_secs());
            cx.sleep(Duration::from_millis(100)).await;
        }
    }

    fn relay(&self, cx: &TestCx, errors: bool) {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        for line in lines.iter().filter(|l| !l.starts_with("stderr: ") && !l.starts_with("selftest:")) {
            cx.log(format_args!("│ {}: {line}", self.name));
        }
        if errors {
            let stderr: Vec<&String> = lines.iter().filter(|l| l.starts_with("stderr: ")).collect();
            for line in stderr.iter().rev().take(16).rev() {
                cx.log(format_args!("│ {}: {line}", self.name));
            }
        }
    }

    async fn finish(mut self, cx: &TestCx, limit: Duration) -> Result<()> {
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.relay(cx, !status.success());
                ensure!(status.success(), "{} failed in the app ({status}, after {:.1} s)", self.name, self.started.elapsed().as_secs_f64());
                return Ok(());
            }
            if self.started.elapsed() > limit {
                let _ = self.child.kill();
                self.relay(cx, true);
                bail!("{} still running after {} s: killed", self.name, limit.as_secs());
            }
            cx.sleep(Duration::from_millis(100)).await;
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

macro_rules! skip_without_stack {
    ($cx:expr) => {
        match stack()? {
            Some(stack) => stack,
            None => {
                $cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
                return Ok(());
            }
        }
    };
}

/// A throwaway account for one run: never one of the stack's.
fn throwaway_env(tag: &str) -> Vec<(&'static str, String)> {
    // `sv…`: these tests' namespace (the invite test sweeps personal invites they left).
    let name = format!("sv{}{tag}", random_hex(8));
    vec![
        ("DECIBELL_TEST_ACCOUNT", name.clone()),
        ("DECIBELL_TEST_EMAIL", format!("{name}@example.test")),
        ("DECIBELL_TEST_PASSWORD", format!("pw-{}", random_hex(12))),
    ]
}

async fn throwaway(cx: TestCx, check: &'static str) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let env = throwaway_env("");
    cx.log(format_args!("throwaway account {}", env[0].1));
    let profile = Profile::new(check)?;
    let result = spawn(check, None, &stack, &profile.0, &env)?.finish(&cx, CHILD_LIMIT).await;
    sweep(&cx, &stack).await?;
    result
}

/// As Design Lab's owner, remove the throwaway accounts a test left a member.
async fn sweep(cx: &TestCx, stack: &Stack) -> Result<()> {
    let profile = Profile::new("servers-sweep")?;
    spawn("servers-sweep", Some("carol"), stack, &profile.0, &[])?.finish(cx, Duration::from_secs(120)).await
}

/// A child signed in as carol; skipped when the binary has no `--login`.
async fn carol(cx: TestCx, check: &'static str) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let profile = Profile::new(check)?;
    let mut running = spawn(check, Some("carol"), &stack, &profile.0, &[])?;
    loop {
        if running.child.try_wait()?.is_some() {
            break;
        }
        ensure!(running.started.elapsed() < CHILD_LIMIT, "{check} still running after {} s", CHILD_LIMIT.as_secs());
        cx.sleep(Duration::from_millis(100)).await;
    }
    if running.no_dev_login() {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    }
    running.finish(&cx, CHILD_LIMIT).await
}

/// Whether the session's clipboard is empty (`wl-paste`); `None` when that can't be told.
fn clipboard_empty() -> Option<bool> {
    let out = Command::new("wl-paste").arg("--list-types").stdin(Stdio::null()).output().ok()?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Some(!out.status.success() && text.contains("Nothing is copied"))
}

/// Invites: carol, under XWayland (an X11 client owns the clipboard without the keyboard focus;
/// Wayland hands it to the focused client only, and other windows share this desktop). The Copy
/// link check runs only on an empty clipboard, which is emptied again after (`wl-copy --clear`
/// leaves no process behind, unlike putting text back would).
async fn invites_test(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let profile = Profile::new("invite-manage-live")?;
    let empty = clipboard_empty();
    let clipboard = if empty == Some(true) { "1" } else { "0" };
    if clipboard == "0" {
        cx.log("the clipboard holds something (or wl-paste is missing): the Copy link check is skipped, the clipboard left alone");
    }
    let running = spawn_without(
        "invite-manage-live",
        Some("carol"),
        &stack,
        &profile.0,
        &[("DECIBELL_TEST_CLIPBOARD", clipboard.into())],
        &["WAYLAND_DISPLAY"],
    )?;
    let result = running.finish(&cx, CHILD_LIMIT).await;
    if empty == Some(true) {
        let _ = Command::new("wl-copy").arg("--clear").stdin(Stdio::null()).status();
        cx.log(format_args!("clipboard emptied again: {}", if clipboard_empty() == Some(true) { "empty" } else { "NOT EMPTY" }));
    }
    result
}

/// The reorder: carol drags; a second carol session must see the same order before the cleanup.
async fn reorder_test(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let main_profile = Profile::new("channel-reorder")?;
    let observer_profile = Profile::new("channel-reorder-observer")?;
    let go = main_profile.0.join("observed");
    let mut main = spawn("channel-reorder", Some("carol"), &stack, &main_profile.0, &[("DECIBELL_TEST_GO", go.display().to_string())])?;
    if let Err(e) = main.wait_mark(&cx, "order", Duration::from_secs(180)).await {
        if main.no_dev_login() {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            return Ok(());
        }
        return Err(e);
    }
    let order = main.mark_value("order").context("the order")?;
    cx.log("the drags settled: a second session of carol checks the order");
    let observer = spawn("servers-observe", Some("carol"), &stack, &observer_profile.0, &[("DECIBELL_TEST_ORDER", order)])?;
    let observed = observer.finish(&cx, Duration::from_secs(90)).await;
    std::fs::write(&go, b"go")?;
    main.finish(&cx, CHILD_LIMIT).await?;
    observed
}

/// Invite friends: A invites; B is a friend who isn't a member, C one who is.
async fn friends(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let (a, b, c) = (throwaway_env("a"), throwaway_env("b"), throwaway_env("c"));
    let profiles = (Profile::new("invite-friends-a")?, Profile::new("invite-friends-b")?, Profile::new("invite-friends-c")?);
    let done = profiles.2.0.join("done");
    cx.log(format_args!("inviter {}, friends {} (not a member) and {} (a member)", a[0].1, b[0].1, c[0].1));
    let with = |env: &[(&'static str, String)], extra: &[(&'static str, String)]| -> Vec<(&'static str, String)> {
        env.iter().chain(extra).cloned().collect()
    };
    let inviter = ("DECIBELL_TEST_INVITER", a[0].1.clone());
    let mut friend_b = spawn("servers-friend", None, &stack, &profiles.1.0, &with(&b, &[inviter.clone(), ("DECIBELL_TEST_ROLE", "outside".into())]))?;
    let mut friend_c = spawn(
        "servers-friend",
        None,
        &stack,
        &profiles.2.0,
        &with(&c, &[inviter.clone(), ("DECIBELL_TEST_ROLE", "member".into()), ("DECIBELL_TEST_GO", done.display().to_string())]),
    )?;
    // A sign-up can wait for central's per-address limit (minutes).
    friend_b.wait_mark(&cx, "ready", Duration::from_secs(400)).await?;
    friend_c.wait_mark(&cx, "ready", Duration::from_secs(400)).await?;
    cx.log("both friends registered (and C joined Design Lab): the inviter starts");
    let main = spawn(
        "invite-friends-live",
        None,
        &stack,
        &profiles.0.0,
        &with(&a, &[("DECIBELL_TEST_FRIEND_OUT", b[0].1.clone()), ("DECIBELL_TEST_FRIEND_IN", c[0].1.clone())]),
    )?;
    let result = main.finish(&cx, CHILD_LIMIT).await;
    let dm = friend_b.wait_mark(&cx, "dm", Duration::from_secs(60)).await;
    std::fs::write(&done, b"done")?;
    result?;
    dm?;
    cx.log(format_args!("the friend received: {}", friend_b.mark_value("dm").unwrap_or_default()));
    friend_b.finish(&cx, Duration::from_secs(60)).await?;
    friend_c.finish(&cx, Duration::from_secs(90)).await?;
    sweep(&cx, &stack).await
}

// ── Inside the app ───────────────────────────────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "servers-browse-join", about: "(in the app) join from Browse, leave", run: |cx| Box::pin(check_browse_join(cx)) },
        SelfTest { name: "servers-join-link", about: "(in the app) join by links", run: |cx| Box::pin(check_join_link(cx)) },
        SelfTest { name: "invite-manage-live", about: "(in the app) create, copy, revoke", run: |cx| Box::pin(check_invites(cx)) },
        SelfTest { name: "invite-friends-live", about: "(in the app) invite a friend", run: |cx| Box::pin(check_invite_friends(cx)) },
        SelfTest { name: "servers-friend", about: "(in the app) a friend of the inviter", run: |cx| Box::pin(check_friend(cx)) },
        SelfTest { name: "channel-create-delete", about: "(in the app) create and delete", run: |cx| Box::pin(check_create_delete(cx)) },
        SelfTest { name: "channel-reorder", about: "(in the app) drag to reorder", run: |cx| Box::pin(check_reorder(cx)) },
        SelfTest { name: "servers-observe", about: "(in the app) a second session's order", run: |cx| Box::pin(check_observe(cx)) },
        SelfTest { name: "category-sync-drop", about: "(in the app) the sync prompt", run: |cx| Box::pin(check_sync_drop(cx)) },
        SelfTest { name: "welcome-banner", about: "(in the app) the banner", run: |cx| Box::pin(check_welcome(cx)) },
        SelfTest { name: "servers-idle", about: "(in the app) at rest and dragging", run: |cx| Box::pin(check_idle(cx)) },
        SelfTest { name: "servers-sweep", about: "(in the app) remove throwaway members", run: |cx| Box::pin(check_sweep(cx)) },
    ]
}

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

fn mark(what: impl std::fmt::Display) {
    println!("{MARK} {what}");
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is not set"))
}

/// What this run names its objects: `sv-<pid of the test parent>`.
fn tag() -> String {
    format!("sv-{}", std::env::var("DECIBELL_TEST_RUN").unwrap_or_else(|_| std::process::id().to_string()))
}

async fn wait_for(cx: &TestCx, limit: Duration, what: &str, ready: impl Fn(&mut App) -> bool) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if cx.update(|_, cx| ready(cx))? {
            return Ok(started.elapsed());
        }
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {what}", limit.as_secs());
        cx.sleep(Duration::from_millis(50)).await;
    }
}

async fn signed_in(cx: &TestCx) -> Result<()> {
    let t = wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.log(format_args!("signed in after {} ms", t.as_millis()));
    Ok(())
}

/// The core honours `DECIBELL_CENTRAL` in this build and it is loopback: only then may a check
/// register or sign in by password.
fn local_only() -> Result<()> {
    ensure!(
        cfg!(any(debug_assertions, feature = "dev-local")),
        "this build ignores DECIBELL_CENTRAL (a release build without dev-local): refusing to talk to any server"
    );
    let central = env("DECIBELL_CENTRAL")?;
    ensure!(is_loopback(&central), "DECIBELL_CENTRAL={central} is not loopback: refusing");
    Ok(())
}

async fn throwaway_sign_in(cx: &TestCx) -> Result<String> {
    local_only()?;
    let name = env("DECIBELL_TEST_ACCOUNT")?;
    let password = env("DECIBELL_TEST_PASSWORD")?;
    wait_for(cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    // Central allows 10 sign-ups per address, then one per 2 minutes, and other tests on this
    // machine sign up from the same loopback address: wait for the bucket rather than fail.
    let started = Instant::now();
    loop {
        cx.update(|_, cx| model(cx).auth.update(cx, |a, cx| a.set_register_result(None, cx)))?;
        let (n, e, p) = (name.clone(), env("DECIBELL_TEST_EMAIL")?, password.clone());
        cx.update(move |_, cx| actions::auth::register(n, e, p, cx))?;
        wait_for(cx, Duration::from_secs(20), "the registration's answer", |cx| model(cx).auth.read(cx).register_result.is_some()).await?;
        let result = cx.update(|_, cx| model(cx).auth.read(cx).register_result.clone())?.context("no answer")?;
        if result.success {
            break;
        }
        let limited = result.message.contains("Too many sign-ups");
        ensure!(limited && started.elapsed() < Duration::from_secs(300), "registering {name} was refused: {}", result.message);
        cx.log("central's sign-up limit for this address (shared with other tests): waiting 30 s");
        cx.sleep(Duration::from_secs(30)).await;
    }
    let n = name.clone();
    cx.update(move |_, cx| actions::auth::sign_in(n, password, cx))?;
    signed_in(cx).await?;
    cx.log(format_args!("registered and signed in as {name}"));
    Ok(name)
}

/// Log out (forgetting this device at central).
async fn log_out(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::auth::sign_out(cx))?;
    wait_for(cx, Duration::from_secs(15), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}

/// The middle of the window (the chat area: a menu opened there fits below the point).
fn visible_window_bounds_center(cx: &mut App) -> Point<Pixels> {
    let window = cx.active_window().or_else(|| cx.windows().first().copied());
    let bounds = window.and_then(|w| w.update(cx, |_, window, cx| visible_window_bounds(window, cx)).ok());
    bounds.map_or(point(px(640.), px(300.)), |b| point(b.center().x, b.top() + px(200.)))
}

fn visible(cx: &TestCx) -> Result<Bounds<Pixels>> {
    cx.update(|window, cx| visible_window_bounds(window, cx))
}

/// The pointer to a corner where it hovers nothing.
fn park(cx: &TestCx) -> Result<()> {
    let v = visible(cx)?;
    cx.mouse_move(point(v.right() - px(6.), v.bottom() - px(6.)))
}

async fn wait_probe(cx: &TestCx, key: &str) -> Result<Bounds<Pixels>> {
    let key_owned = key.to_string();
    wait_for(cx, Duration::from_secs(15), &format!("the probe {key}"), move |_| probes::bounds(&key_owned).is_some()).await?;
    probes::bounds(key).context("probe")
}

async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    let bounds = wait_probe(cx, key).await?;
    cx.click(bounds.center())?;
    Ok(())
}

fn right_click(cx: &TestCx, at: Point<Pixels>) -> Result<()> {
    cx.mouse_move(at)?;
    cx.update(move |window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Right, position: at, modifiers: Modifiers::default(), click_count: 1, first_mouse: false }),
            cx,
        );
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Right, position: at, modifiers: Modifiers::default(), click_count: 1 }), cx);
    })
}

/// Settle (nothing drawn for 1 s) and return the counters.
async fn settle_drawn(cx: &TestCx) -> Result<Frames> {
    let started = Instant::now();
    let mut last = cx.frames()?;
    let mut quiet = Instant::now();
    loop {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames()?;
        if now.drawn != last.drawn {
            quiet = Instant::now();
        } else if quiet.elapsed() >= Duration::from_secs(1) {
            return Ok(now);
        }
        last = now;
        ensure!(started.elapsed() < Duration::from_secs(15), "still drawing after 15 s");
    }
}

fn channels_of(server: ServerId, cx: &App) -> Vec<Channel> {
    model(cx).community(server, cx).map(|c| c.channels.read(cx).list().to_vec()).unwrap_or_default()
}

fn find(server: ServerId, name: &str, cx: &App) -> Option<Channel> {
    channels_of(server, cx).into_iter().find(|c| &*c.name == name)
}

/// Open a community from its tile and wait for its channels. The tiles move as the others
/// connect: wait for the bar to settle, and click again if a moved tile took the click.
async fn open_community(cx: &TestCx, server: ServerId) -> Result<()> {
    wait_for(cx, Duration::from_secs(30), "the community to connect", move |cx| {
        let m = model(cx);
        let c = m.connections.read(cx);
        c.is_connected(server) && m.servers.read(cx).servers().iter().all(|s| !c.is_pending(s.id))
    })
    .await?;
    let shown = move |cx: &mut App| {
        let m = model(cx);
        let nav = m.nav.read(cx);
        nav.active_server == Some(server) && nav.active_view == ActiveView::Server && !channels_of(server, cx).is_empty()
    };
    for _ in 0..3 {
        settle_drawn(cx).await?;
        click_probe(cx, &format!("server-tile:{}", server.get())).await?;
        if wait_for(cx, Duration::from_secs(3), "the server view", shown).await.is_ok() {
            settle_drawn(cx).await?;
            return Ok(());
        }
    }
    // The tile's click is the shell's (its own tests click it); this test is about what follows.
    cx.log("NOTE: three clicks on the tile didn't open it: opening it as the tile does");
    cx.update(move |_, cx| actions::servers::open_server(server, cx))?;
    wait_for(cx, Duration::from_secs(10), "the server view", shown).await?;
    settle_drawn(cx).await?;
    Ok(())
}

/// Create a channel through the core (the tests that aren't about the dialog) and wait for it.
async fn api_create(cx: &TestCx, server: ServerId, name: &str, kind: &str, category: Option<&ChannelId>) -> Result<ChannelId> {
    let args = api::channels::CreateChannelArgs {
        server_id: server.wire(),
        name: name.to_string(),
        channel_type: kind.to_string(),
        voice_bitrate_kbps: 0,
        category_id: category.map(|c| c.to_string()),
        encrypted: Some(false),
    };
    api::channels::create_channel(args).await.map_err(|e| anyhow::anyhow!("create_channel: {e}"))?;
    let n = name.to_string();
    wait_for(cx, Duration::from_secs(15), &format!("{name} to appear"), move |cx| find(server, &n, cx).is_some()).await?;
    Ok(cx.update(|_, cx| find(server, name, cx))?.context("created")?.id)
}

async fn api_delete(cx: &TestCx, server: ServerId, id: &ChannelId) -> Result<()> {
    let args = api::channels::DeleteChannelArgs { server_id: server.wire(), channel_id: id.to_string() };
    api::channels::delete_channel(args).await.map_err(|e| anyhow::anyhow!("delete_channel: {e}"))?;
    let id = id.clone();
    wait_for(cx, Duration::from_secs(15), "the deletion", move |cx| !channels_of(server, cx).iter().any(|c| c.id == id)).await?;
    Ok(())
}

/// Delete what these tests left in `server`: names `sv-<digits>-…` (this run's, or an earlier
/// run's that failed before its own clean-up).
async fn clean_up(cx: &TestCx, server: ServerId) -> Result<()> {
    let ours = |name: &str| name.strip_prefix("sv-").is_some_and(|rest| rest.split('-').next().is_some_and(|pid| !pid.is_empty() && pid.bytes().all(|b| b.is_ascii_digit())));
    let mine: Vec<Channel> = cx.update(|_, cx| channels_of(server, cx))?.into_iter().filter(|c| ours(&c.name)).collect();
    // Channels first, then the categories (deleting one keeps its channels).
    for c in mine.iter().filter(|c| c.kind != ChannelKind::Category).chain(mine.iter().filter(|c| c.kind == ChannelKind::Category)) {
        api_delete(cx, server, &c.id).await?;
    }
    sweep_members(cx, server).await
}

/// These tests' throwaway accounts that stayed members (the core's reconnect race above, or a
/// run that failed before it left): `sv<8 hex>[abc]`, and `t<9 hex>[abc]` from before they were
/// named so. Kicked (as the owner).
async fn sweep_members(cx: &TestCx, server: ServerId) -> Result<()> {
    let ours = |name: &str| {
        let hex = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_hexdigit());
        let tagged = |rest: &str, n: usize| (rest.len() == n && hex(rest)) || (rest.len() == n + 1 && hex(&rest[..n]) && matches!(rest.as_bytes()[n], b'a' | b'b' | b'c'));
        name.strip_prefix("sv").is_some_and(|r| tagged(r, 8)) || name.strip_prefix('t').is_some_and(|r| tagged(r, 9) && r.len() == 10)
    };
    wait_for(cx, Duration::from_secs(10), "the roster", move |cx| model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).meta().is_some())).await?;
    let stale: Vec<(i64, String)> = cx.update(move |_, cx| {
        model(cx)
            .community(server, cx)
            .map(|c| c.roster.read(cx).members().values().filter(|m| m.uid > 0 && ours(&m.username)).map(|m| (m.uid, m.username.to_string())).collect())
            .unwrap_or_default()
    })?;
    for (uid, name) in stale {
        api::community::kick_member(api::community::KickMemberArgs { server_id: server.wire(), username: Some(name.clone()), uid, reason: None })
            .await
            .map_err(|e| anyhow::anyhow!("kick_member: {e}"))?;
        cx.log(format_args!("removed {name}, a throwaway account an earlier run left a member"));
    }
    Ok(())
}

/// The order of the seeded channels (everything not this run's) as a list of ids.
fn seeded_order(server: ServerId, cx: &App) -> Vec<ChannelId> {
    let tag = tag();
    channels_of(server, cx).into_iter().filter(|c| !c.name.starts_with("sv-")).map(|c| c.id).filter(|_| !tag.is_empty()).collect()
}

/// A real drag: press `from`, move in steps (a frame apart), release at `to`.
async fn drag(cx: &TestCx, from: Point<Pixels>, to: Point<Pixels>) -> Result<()> {
    cx.mouse_down(from, 1, Modifiers::default())?;
    let steps = 10;
    for i in 1..=steps {
        let f = i as f32 / steps as f32;
        cx.mouse_drag(point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f))?;
        cx.sleep(Duration::from_millis(16)).await;
    }
    cx.sleep(Duration::from_millis(60)).await;
    cx.mouse_up(to)?;
    Ok(())
}

/// A row's probe key.
fn row_key(c: &Channel) -> String {
    match c.kind {
        ChannelKind::Category => format!("category:{}", c.id),
        ChannelKind::Voice => format!("voice:{}", c.id),
        _ => format!("channel:{}", c.id),
    }
}

/// The pointer over a row: its top or bottom quarter.
async fn over(cx: &TestCx, server: ServerId, id: &ChannelId, top: bool) -> Result<Point<Pixels>> {
    let c = cx.update(|_, cx| channels_of(server, cx))?.into_iter().find(|c| c.id == *id).context("the row")?;
    let b = wait_probe(cx, &row_key(&c)).await?;
    let y = if top { b.top() + b.size.height * 0.25 } else { b.top() + b.size.height * 0.75 };
    Ok(point(b.left() + px(40.).min(b.size.width / 2.), y))
}

/// The reorder's answer arrives (`channel_action_responded`, action "reorder") after `act`.
async fn reorder_answered(cx: &TestCx, act: impl AsyncFnOnce(&TestCx) -> Result<()>) -> Result<()> {
    let answered = Rc::new(Cell::new(None::<bool>));
    let flag = answered.clone();
    let _subscription = cx.update(move |_, cx| {
        let model = model(cx);
        cx.subscribe(&model.answers, move |_, e: &CoreEvent, _| {
            if let CoreEvent::ChannelActionResponded(p) = e
                && p.action == "reorder"
            {
                flag.set(Some(p.success));
            }
        })
    })?;
    act(cx).await?;
    let a = answered.clone();
    wait_for(cx, Duration::from_secs(15), "the reorder's answer", move |_| a.get().is_some()).await?;
    ensure!(answered.get() == Some(true), "the community refused the reorder");
    // The broadcast follows the answer.
    cx.sleep(Duration::from_millis(400)).await;
    Ok(())
}

fn ids(list: &[Channel]) -> Vec<ChannelId> {
    list.iter().map(|c| c.id.clone()).collect()
}

/// `a` comes right before `b` (the list order).
fn right_before(list: &[ChannelId], a: &ChannelId, b: &ChannelId) -> bool {
    list.iter().position(|x| x == a).is_some_and(|i| list.get(i + 1) == Some(b))
}

// ── servers-browse-join ─────────────────────────────────────────────────────────────────────

async fn check_browse_join(cx: TestCx) -> Result<()> {
    throwaway_sign_in(&cx).await?;
    let design = community(DESIGN_LAB)?;
    let server = design.id;
    cx.update(|_, cx| model(cx).nav.update(cx, |n, cx| n.set_active_view(ActiveView::Browse, cx)))?;
    wait_for(&cx, Duration::from_secs(20), "the directory", |cx| {
        let s = model(cx).servers.read(cx).discoverable().iter().map(|s| s.name.to_string()).collect::<Vec<_>>();
        s.iter().any(|n| n == DESIGN_LAB) && s.iter().any(|n| n == "Daily Gaming")
    })
    .await?;
    let listed: Vec<String> = cx.update(|_, cx| model(cx).servers.read(cx).discoverable().iter().map(|s| s.name.to_string()).collect())?;
    ensure!(!listed.iter().any(|n| n == "Rustaceans"), "the unlisted Rustaceans is in the directory: {listed:?}");
    cx.log(format_args!("the directory lists {listed:?}"));
    click_probe(&cx, &format!("browse-card:{DESIGN_LAB}")).await?;
    let t = wait_for(&cx, Duration::from_secs(30), "Design Lab to connect and open", move |cx| {
        let m = model(cx);
        let nav = m.nav.read(cx);
        m.connections.read(cx).is_connected(server) && nav.active_server == Some(server) && nav.active_view == ActiveView::Server
    })
    .await?;
    cx.log(format_args!("joined and opened after {} ms", t.as_millis()));
    wait_probe(&cx, &format!("server-tile:{}", server.get())).await?;
    wait_for(&cx, Duration::from_secs(10), "a channel", move |cx| model(cx).nav.read(cx).active_channel.is_some()).await?;
    // Leave: the menu, its Leave Server, the confirmation (Tab to Leave, Enter).
    leave_by_menu(&cx, server).await?;
    log_out(&cx).await
}

async fn leave_by_menu(cx: &TestCx, server: ServerId) -> Result<()> {
    // A dialog that just closed covers the window while it fades out.
    settle_drawn(cx).await?;
    click_probe(cx, "server-menu-button").await?;
    wait_for(cx, Duration::from_secs(5), "the server menu", |_| probes::flag_set("server-menu-open")).await?;
    click_probe(cx, "server-menu-leave").await?;
    wait_for(cx, Duration::from_secs(5), "the confirmation", |cx| model(cx).overlays.read(cx).is_open(modal::LEAVE_SERVER_CONFIRM)).await?;
    cx.sleep(Duration::from_millis(350)).await;
    cx.keystroke("tab")?;
    cx.keystroke("tab")?;
    cx.keystroke("enter")?;
    wait_for(cx, Duration::from_secs(15), "the community to go", move |cx| {
        let m = model(cx);
        let c = m.connections.read(cx);
        !c.is_connected(server) && !c.is_pending(server)
    })
    .await?;
    wait_for(cx, Duration::from_secs(10), "the leave's answer (Browse)", |cx| model(cx).nav.read(cx).active_view == ActiveView::Browse).await?;
    cx.log("left: the tile is gone and Browse shows");
    ensure_out(cx, server).await
}

/// After a leave the core may be back in: the community closes the connection right after its
/// answer, and the core reconnects — and, Design Lab being public, joins again — before it acts
/// on the answer (`decibell-core`'s `net/community.rs`; the Electron client shares it). Not this
/// crate's to fix: said in the log; the parent's sweep (`servers-sweep`, as the owner) removes the
/// throwaway account from the community afterwards.
async fn ensure_out(cx: &TestCx, server: ServerId) -> Result<()> {
    cx.sleep(Duration::from_millis(2500)).await;
    let back = cx.update(move |_, cx| {
        let m = model(cx);
        let c = m.connections.read(cx);
        c.is_connected(server) || c.is_pending(server)
    })?;
    if back {
        cx.log("NOTE: after the leave the core reconnected and joined again (decibell-core's reconnect race; swept after the test)");
    }
    Ok(())
}

// ── servers-join-link ───────────────────────────────────────────────────────────────────────

async fn check_join_link(cx: TestCx) -> Result<()> {
    local_only()?;
    let design = community(DESIGN_LAB)?;
    let server = design.id;
    // 1. A deep link that arrives before the sign-in (the link started the app): the join dialog
    // shows once signed in, resolves the code, Accept joins.
    let link = format!("decibell://invite/{}", design.invite);
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    ensure!(cx.update(|_, cx| crate::open_invite_link(&link, cx))?, "the link didn't parse");
    throwaway_sign_in(&cx).await?;
    wait_for(&cx, Duration::from_secs(15), "the dialog to resolve the code", |cx| {
        crate::current::<crate::join::JoinOverlay>(cx).is_some_and(|j| j.read(cx).state(cx).name.as_deref() == Some(DESIGN_LAB))
    })
    .await?;
    settle_drawn(&cx).await?;
    click_probe(&cx, "join-accept").await?;
    let joined = wait_for(&cx, Duration::from_secs(30), "the join (dialog)", move |cx| {
        let m = model(cx);
        m.connections.read(cx).is_connected(server) && m.nav.read(cx).active_server == Some(server) && m.servers.read(cx).pending_invite.is_none()
    })
    .await;
    if let Err(e) = joined {
        let (pending, connected, active, error) = cx.update(move |_, cx| {
            let m = model(cx);
            let error = crate::current::<crate::join::JoinOverlay>(cx).map(|j| j.read(cx).error_for_tests());
            (m.servers.read(cx).pending_invite.is_some(), m.connections.read(cx).is_connected(server), m.nav.read(cx).active_server, error)
        })?;
        bail!("{e} (pending {pending}, connected {connected}, active {active:?}, dialog {error:?}, accept at {:?})", probes::bounds("join-accept"));
    }
    cx.log("a link from before the sign-in: the dialog showed after it, resolved the code and joined");
    leave_by_menu(&cx, server).await?;

    // 2. A pasted code-only link, then 3. the old host:port form, on Browse.
    for (what, text) in [
        ("a code-only link", link.clone()),
        ("an old host:port link", format!("decibell://invite/{}:{}/{}", design.host, design.port, design.invite)),
    ] {
        paste_and_join(&cx, &text).await?;
        wait_for(&cx, Duration::from_secs(30), what, move |cx| {
            let m = model(cx);
            m.connections.read(cx).is_connected(server) && m.nav.read(cx).active_server == Some(server)
        })
        .await?;
        cx.log(format_args!("joined by {what}"));
        leave_by_menu(&cx, server).await?;
    }

    // 4. Errors, in the renderer's words.
    paste_and_join(&cx, "decibell://invite/ab").await?;
    let error = browse_error(&cx, "a malformed link").await?;
    ensure!(error == "That isn't a valid decibell:// invite link", "malformed link: {error}");
    paste_and_join(&cx, "decibell://invite/ZZZZ9999ZZ").await?;
    let error = browse_error(&cx, "an unknown code").await?;
    ensure!(error.ends_with(". Enter the server's host and port below."), "unknown code: {error}");
    cx.log(format_args!("an unknown code: {error}"));
    // Still out of Design Lab (no rejoin behind our back).
    cx.sleep(Duration::from_secs(3)).await;
    let (connected, member) = cx.update(move |_, cx| {
        let m = model(cx);
        let c = m.connections.read(cx);
        (c.is_connected(server) || c.is_pending(server), m.servers.read(cx).get(server).is_some())
    })?;
    if connected {
        cx.log(format_args!("NOTE: after the last leave the core is back in Design Lab (decibell-core's reconnect race; tile entry {member}; swept after the test)"));
    }
    log_out(&cx).await
}

async fn paste_and_join(cx: &TestCx, text: &str) -> Result<()> {
    settle_drawn(cx).await?;
    cx.update(|_, cx| model(cx).nav.update(cx, |n, cx| n.set_active_view(ActiveView::Browse, cx)))?;
    wait_for(cx, Duration::from_secs(5), "Browse", |cx| crate::current::<BrowseView>(cx).is_some()).await?;
    let text = text.to_string();
    cx.update(move |_, cx| {
        if let Some(view) = crate::current::<BrowseView>(cx) {
            view.update(cx, |v, cx| v.set_invite(&text, cx));
        }
    })?;
    cx.sleep(Duration::from_millis(100)).await;
    click_probe(cx, "browse-join").await
}

async fn browse_error(cx: &TestCx, what: &str) -> Result<String> {
    wait_for(cx, Duration::from_secs(15), what, |cx| crate::current::<BrowseView>(cx).is_some_and(|v| v.read(cx).invite_error().is_some())).await?;
    Ok(cx.update(|_, cx| crate::current::<BrowseView>(cx).and_then(|v| v.read(cx).invite_error()).map(|e| e.to_string()))?.unwrap_or_default())
}

// ── invite-manage-live ──────────────────────────────────────────────────────────────────────

async fn check_invites(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    click_probe(&cx, "server-menu-button").await?;
    click_probe(&cx, "server-menu-invites").await?;
    wait_for(&cx, Duration::from_secs(5), "the Invites dialog", |cx| model(cx).overlays.read(cx).is_open(modal::INVITE_MANAGE)).await?;
    let invites = move |cx: &App| -> Vec<decibell_model::stores::community::Invite> {
        model(cx).community(server, cx).map(|c| c.moderation.read(cx).invites.clone()).unwrap_or_default()
    };
    wait_for(&cx, Duration::from_secs(10), "the list", move |cx| !invites(cx).is_empty()).await?;
    // What an earlier run of these tests may have left: carol's 5-use, one-hour invites, and the
    // personal invites the invite-friends test's throwaway inviter made (`sv…a` → `sv…b`; earlier
    // runs named them `t…a` → `t…b`).
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    let throwaway = |name: &str, tag: char| {
        let hex = |s: &str| s.bytes().all(|b| b.is_ascii_hexdigit());
        name.ends_with(tag)
            && ((name.len() == 11 && name.starts_with("sv") && hex(&name[2..10])) || (name.len() == 11 && name.starts_with('t') && hex(&name[1..10])))
    };
    let leftovers: Vec<String> = cx.update(move |_, cx| {
        invites(cx)
            .iter()
            .filter(|i| {
                (i.created_by_uid == me && i.max_uses == 5 && i.expires_at - i.created_at == 3600)
                    || (throwaway(&i.created_by, 'a') && throwaway(&i.for_user, 'b'))
            })
            .map(|i| i.code.to_string())
            .collect()
    })?;
    for code in leftovers {
        revoke(&cx, server, &code).await?;
        cx.log(format_args!("revoked {code}, left by an earlier run"));
    }
    let before: Vec<String> = cx.update(move |_, cx| invites(cx).iter().map(|i| i.code.to_string()).collect())?;
    // One hour, at most 5 uses.
    cx.update(|_, cx| {
        if let Some(v) = crate::current::<InviteManageOverlay>(cx) {
            v.update(cx, |v, cx| v.set_form(3600, Some(5), cx));
        }
    })?;
    cx.sleep(Duration::from_millis(100)).await;
    click_probe(&cx, "invite-create").await?;
    let b = before.clone();
    wait_for(&cx, Duration::from_secs(15), "the new invite", move |cx| invites(cx).iter().any(|i| !b.contains(&i.code.to_string()))).await?;
    let created = cx.update(move |_, cx| invites(cx).into_iter().find(|i| !before.contains(&i.code.to_string())))?.context("created")?;
    let code = created.code.to_string();
    let checked = async {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH)?.as_secs() as i64;
        ensure!(created.max_uses == 5, "max uses {}", created.max_uses);
        ensure!((created.expires_at - now - 3600).abs() < 120, "expires at {} (now {now})", created.expires_at);
        cx.log(format_args!("created {code}: 5 uses, an hour"));
        wait_probe(&cx, &format!("invite:{code}")).await?;
        copy_link(&cx, &code).await
    }
    .await;
    // Revoke through the row's button (and make sure it is gone either way).
    click_probe(&cx, &format!("invite-revoke:{code}")).await?;
    let c2 = code.clone();
    let gone = wait_for(&cx, Duration::from_secs(15), "the revoke", move |cx| !invites(cx).iter().any(|i| *i.code == *c2)).await;
    if gone.is_err() {
        revoke(&cx, server, &code).await?;
    }
    checked?;
    gone?;
    cx.log("revoked: gone from the list");
    cx.keystroke("escape")?;
    log_out(&cx).await
}

async fn revoke(cx: &TestCx, server: ServerId, code: &str) -> Result<()> {
    let args = api::community::RevokeInviteArgs { server_id: server.wire(), code: code.to_string() };
    api::community::revoke_invite(args).await.map_err(|e| anyhow::anyhow!("revoke_invite: {e}"))?;
    let code = code.to_string();
    wait_for(cx, Duration::from_secs(15), "the revoke", move |cx| {
        !model(cx).community(server, cx).is_some_and(|c| c.moderation.read(cx).invites.iter().any(|i| *i.code == *code))
    })
    .await?;
    Ok(())
}

/// Copy link: the clipboard holds the code-only link (run only when the parent found the
/// clipboard empty: it empties it again after).
async fn copy_link(cx: &TestCx, code: &str) -> Result<()> {
    if std::env::var("DECIBELL_TEST_CLIPBOARD").as_deref() != Ok("1") {
        cx.log("SKIP the clipboard check (the clipboard isn't empty: left alone)");
        return Ok(());
    }
    click_probe(cx, &format!("invite-copy:{code}")).await?;
    let want = format!("decibell://invite/{code}");
    let mut copied = None;
    for _ in 0..20 {
        cx.sleep(Duration::from_millis(100)).await;
        copied = cx.update(|_, cx| cx.read_from_clipboard().and_then(|c| c.text()))?;
        if copied.as_deref() == Some(want.as_str()) {
            break;
        }
    }
    ensure!(copied.as_deref() == Some(want.as_str()), "the clipboard held {copied:?}");
    // The button says so for 1.5 s.
    cx.log("Copy link put decibell://invite/<code> on the clipboard");
    Ok(())
}

// ── invite-friends-live ─────────────────────────────────────────────────────────────────────

async fn join_by_code(cx: &TestCx, design: &Community) -> Result<()> {
    let (host, port, code) = (design.host.clone(), design.port, design.invite.clone());
    cx.update(move |_, cx| actions::servers::redeem_invite(host, port, code, cx, |_, _| {}))?;
    let server = design.id;
    wait_for(cx, Duration::from_secs(30), "joining Design Lab", move |cx| model(cx).connections.read(cx).is_connected(server)).await?;
    Ok(())
}

async fn leave_quietly(cx: &TestCx, server: ServerId) -> Result<()> {
    cx.update(move |_, cx| actions::servers::leave(server, cx))?;
    wait_for(cx, Duration::from_secs(15), "the leave", move |cx| !model(cx).connections.read(cx).is_connected(server)).await?;
    ensure_out(cx, server).await
}

fn friend_status(name: &str, cx: &App) -> Option<(i64, FriendStatus)> {
    model(cx).live.friends.read(cx).list().iter().find(|f| f.username.eq_ignore_ascii_case(name)).map(|f| (f.uid, f.status))
}

async fn check_invite_friends(cx: TestCx) -> Result<()> {
    throwaway_sign_in(&cx).await?;
    let design = community(DESIGN_LAB)?;
    let server = design.id;
    join_by_code(&cx, &design).await?;
    let (outside, inside) = (env("DECIBELL_TEST_FRIEND_OUT")?, env("DECIBELL_TEST_FRIEND_IN")?);
    for name in [outside.clone(), inside.clone()] {
        cx.update(move |_, cx| actions::friends::add_friend(name, cx, |r, _| if let Err(e) = r { log::error!("add friend: {e}") }))?;
    }
    let (o, i) = (outside.clone(), inside.clone());
    wait_for(&cx, Duration::from_secs(60), "both friends to accept", move |cx| {
        [&o, &i].iter().all(|n| friend_status(n, cx).is_some_and(|(_, s)| s.is_accepted()))
    })
    .await?;
    let out_uid = cx.update(|_, cx| friend_status(&outside, cx))?.context("friend")?.0;
    let in_uid = cx.update(|_, cx| friend_status(&inside, cx))?.context("friend")?.0;
    cx.log("both friend requests accepted");
    open_community(&cx, server).await?;
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.open_modal(modal::INVITE_FRIENDS, cx)))?;
    wait_for(&cx, Duration::from_secs(15), "central's answer", |cx| crate::current::<InviteFriendsOverlay>(cx).is_some_and(|v| v.read(cx).central_answered())).await?;
    let shown = cx.update(|_, cx| crate::current::<InviteFriendsOverlay>(cx).map(|v| v.read(cx).shown(cx)).unwrap_or_default())?;
    cx.log(format_args!("the picker lists {:?}", shown.iter().map(|s| s.1.to_string()).collect::<Vec<_>>()));
    ensure!(shown.iter().any(|s| s.0 == out_uid), "the friend who isn't a member is missing");
    ensure!(!shown.iter().any(|s| s.0 == in_uid), "the friend who is a member is listed");
    click_probe(&cx, &format!("invite-friend:{out_uid}")).await?;
    wait_for(&cx, Duration::from_secs(20), "Sent", move |cx| {
        crate::current::<InviteFriendsOverlay>(cx).is_some_and(|v| v.read(cx).shown(cx).iter().any(|s| s.0 == out_uid && s.2 == Some(RowState::Sent)))
    })
    .await?;
    cx.log("the row says Sent");
    cx.keystroke("escape")?;
    // Revoke the personal invite (ours to revoke: CREATE_INVITE covers your own).
    api::community::list_invites(api::community::ListInvitesArgs { server_id: server.wire() })
        .await
        .map_err(|e| anyhow::anyhow!("list_invites: {e}"))?;
    let personal = move |cx: &App| {
        model(cx).community(server, cx).and_then(|c| c.moderation.read(cx).invites.iter().find(|i| i.for_uid == out_uid).map(|i| i.code.to_string()))
    };
    wait_for(&cx, Duration::from_secs(10), "the personal invite in the list", move |cx| personal(cx).is_some()).await?;
    let code = cx.update(move |_, cx| personal(cx))?.context("personal invite")?;
    revoke(&cx, server, &code).await?;
    cx.log(format_args!("revoked the personal invite {code}"));
    leave_quietly(&cx, server).await?;
    // The friendships go with the throwaway accounts; remove them anyway.
    for (uid, name) in [(out_uid, outside), (in_uid, inside)] {
        cx.update(move |_, cx| actions::friends::act(FriendAction::Remove, uid, name.into(), cx))?;
    }
    cx.sleep(Duration::from_millis(500)).await;
    log_out(&cx).await
}

/// A friend of the inviter: registers, (C: joins Design Lab), accepts the request; B waits for the
/// invite's DM, C for the go to leave.
async fn check_friend(cx: TestCx) -> Result<()> {
    throwaway_sign_in(&cx).await?;
    let role = env("DECIBELL_TEST_ROLE")?;
    let inviter = env("DECIBELL_TEST_INVITER")?;
    let design = community(DESIGN_LAB)?;
    if role == "member" {
        join_by_code(&cx, &design).await?;
    }
    mark("ready");
    let i = inviter.clone();
    wait_for(&cx, Duration::from_secs(120), "the inviter's request", move |cx| {
        friend_status(&i, cx).is_some_and(|(_, s)| s == FriendStatus::PendingIncoming)
    })
    .await?;
    let uid = cx.update(|_, cx| friend_status(&inviter, cx))?.context("request")?.0;
    let name = inviter.clone();
    cx.update(move |_, cx| actions::friends::act(FriendAction::Accept, uid, name.into(), cx))?;
    if role == "member" {
        let go = PathBuf::from(env("DECIBELL_TEST_GO")?);
        let started = Instant::now();
        while !go.exists() {
            ensure!(started.elapsed() < Duration::from_secs(200), "no go");
            cx.sleep(Duration::from_millis(200)).await;
        }
        leave_quietly(&cx, design.id).await?;
    } else {
        wait_for(&cx, Duration::from_secs(120), "the invite's DM", move |cx| {
            model(cx).dm.list.read(cx).get(uid).and_then(|c| c.last_message.clone()).is_some_and(|m| m.content.contains("decibell://invite/"))
        })
        .await?;
        let text = cx.update(move |_, cx| model(cx).dm.list.read(cx).get(uid).and_then(|c| c.last_message.clone()).map(|m| m.content.to_string()))?;
        let text = text.unwrap_or_default();
        ensure!(decibell_model::invite_link::parse_invite_link(&text).is_some_and(|p| p.host.is_none()), "not a code-only link: {text}");
        mark(format_args!("dm {text}"));
    }
    log_out(&cx).await
}

// ── channel-create-delete ───────────────────────────────────────────────────────────────────

/// Open the channel list's context menu on `target` at `at` (the shell's request: what its
/// right-click handlers do once they found the row under the pointer) and click its `row`th row.
async fn menu_action(cx: &TestCx, at: Point<Pixels>, target: decibell_shell::ChannelMenuTarget, row: usize) -> Result<()> {
    // A dialog that just closed covers the window while it fades out.
    settle_drawn(cx).await?;
    cx.update(move |_, cx| decibell_shell::ShellRequests::open_sidebar_menu(decibell_shell::SidebarMenu::Channel(at, target), cx))?;
    wait_for(cx, Duration::from_secs(5), "the list's menu", |_| probes::flag_set("channel-menu-open")).await?;
    cx.sleep(Duration::from_millis(250)).await;
    // The kit's menu opens with its top-left at the point: a border, `p-1`, rows of 35.5 px.
    cx.click(point(at.x + px(60.), at.y + px(5. + 35.5 * row as f32 + 17.75)))?;
    Ok(())
}

async fn check_create_delete(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    clean_up(&cx, server).await?;
    let seeded = cx.update(|_, cx| seeded_order(server, cx))?;
    let tag = tag();
    // A right click on a row opens the list's menu (Escape closes it).
    let first = cx.update(|_, cx| channels_of(server, cx))?.first().cloned().context("rows")?;
    right_click(&cx, wait_probe(&cx, &row_key(&first)).await?.center())?;
    wait_for(&cx, Duration::from_secs(5), "a right click's menu", |_| probes::flag_set("channel-menu-open")).await?;
    cx.sleep(Duration::from_millis(200)).await;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape to close it", |_| !probes::flag_set("channel-menu-open")).await?;
    // (menu row from the top, kind, name, encrypted)
    let plan = [
        (0, ChannelKind::Text, format!("{tag}-text"), false),
        (1, ChannelKind::Voice, format!("{tag}-voice"), false),
        (2, ChannelKind::Category, format!("{tag}-category"), false),
        (0, ChannelKind::Text, format!("{tag}-sealed"), true),
    ];
    for (row, kind, name, encrypted) in plan.iter().cloned() {
        let at = cx.update(|_, cx| visible_window_bounds_center(cx))?;
        menu_action(&cx, at, decibell_shell::ChannelMenuTarget::Empty { server }, row).await?;
        wait_for(&cx, Duration::from_secs(5), "the create dialog", |cx| crate::current::<CreateChannelOverlay>(cx).is_some_and(|v| v.read(cx).is_open())).await?;
        cx.sleep(Duration::from_millis(100)).await;
        // The name field has the focus: type it; the switch is a click.
        cx.type_text(&name)?;
        if encrypted {
            click_probe(&cx, "create-channel-encrypted").await?;
        }
        cx.keystroke("enter")?;
        let n = name.clone();
        wait_for(&cx, Duration::from_secs(15), &format!("{name} in the list"), move |cx| find(server, &n, cx).is_some()).await?;
        let list = cx.update(|_, cx| channels_of(server, cx))?;
        let c = list.iter().find(|c| *c.name == name).context("created")?.clone();
        ensure!(c.kind == kind, "{name} is {:?}", c.kind);
        ensure!(c.encrypted == encrypted, "{name}: encrypted {}", c.encrypted);
        let i = list.iter().position(|x| x.id == c.id).unwrap_or(0);
        let first_category = list.iter().position(|x| x.kind == ChannelKind::Category && x.id != c.id).unwrap_or(list.len());
        match kind {
            // The end of the uncategorized area (text above voice).
            ChannelKind::Text => ensure!(i < first_category && list[i + 1..first_category].iter().all(|x| x.kind == ChannelKind::Voice), "{name} at {i}"),
            ChannelKind::Voice => ensure!(i + 1 == first_category.min(list.len()) || i < first_category, "{name} at {i}"),
            _ => ensure!(i + 1 == list.len(), "the category isn't last ({i} of {})", list.len()),
        }
        if kind == ChannelKind::Text {
            let id = c.id.clone();
            wait_for(&cx, Duration::from_secs(5), "the new channel to open", move |cx| model(cx).nav.read(cx).active_channel.as_ref() == Some(&id)).await?;
        }
        cx.log(format_args!("created {name} ({kind:?}{}) where the renderer puts it", if encrypted { ", encrypted" } else { "" }));
    }
    // Delete each through its menu and the confirmation.
    for (_, _, name, _) in plan.iter().rev() {
        let c = cx.update(|_, cx| find(server, name, cx))?.context("to delete")?;
        let at = cx.update(|_, cx| visible_window_bounds_center(cx))?;
        let target = if c.kind == ChannelKind::Category {
            decibell_shell::ChannelMenuTarget::Category { server, channel: c.id.clone() }
        } else {
            decibell_shell::ChannelMenuTarget::Channel { server, channel: c.id.clone() }
        };
        // "Channel Settings", then "Delete Channel" ("Category …" on a category).
        menu_action(&cx, at, target, 1).await?;
        cx.sleep(Duration::from_millis(400)).await;
        // The confirmation's card takes Enter (`ConfirmModal`).
        cx.keystroke("tab")?;
        cx.keystroke("enter")?;
        let n = name.clone();
        wait_for(&cx, Duration::from_secs(15), &format!("{name} to go"), move |cx| find(server, &n, cx).is_none()).await?;
        cx.log(format_args!("deleted {name}"));
    }
    let after = cx.update(|_, cx| seeded_order(server, cx))?;
    ensure!(after == seeded, "the seeded order moved: {seeded:?} → {after:?}");
    log_out(&cx).await
}

// ── channel-reorder ─────────────────────────────────────────────────────────────────────────

async fn check_reorder(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    clean_up(&cx, server).await?;
    let seeded = cx.update(|_, cx| seeded_order(server, cx))?;
    let result = reorders(&cx, server).await;
    // Wait for the second session (or not, when this failed), then put everything back.
    if result.is_ok() {
        let go = PathBuf::from(env("DECIBELL_TEST_GO")?);
        let started = Instant::now();
        while !go.exists() && started.elapsed() < Duration::from_secs(120) {
            cx.sleep(Duration::from_millis(200)).await;
        }
    }
    clean_up(&cx, server).await?;
    let after = cx.update(|_, cx| seeded_order(server, cx))?;
    ensure!(after == seeded, "the seeded order moved: {seeded:?} → {after:?}");
    cx.log("cleaned up; the seeded order is as it was");
    result?;
    log_out(&cx).await
}

async fn reorders(cx: &TestCx, server: ServerId) -> Result<()> {
    let tag = tag();
    let a = api_create(cx, server, &format!("{tag}-a"), "text", None).await?;
    let b = api_create(cx, server, &format!("{tag}-b"), "text", None).await?;
    let cat1 = api_create(cx, server, &format!("{tag}-cat1"), "category", None).await?;
    let c = api_create(cx, server, &format!("{tag}-c"), "text", Some(&cat1)).await?;
    let cat2 = api_create(cx, server, &format!("{tag}-cat2"), "category", None).await?;
    settle_drawn(cx).await?;

    // Within its group: b above a.
    let (from, to) = (over(cx, server, &b, true).await?, over(cx, server, &a, true).await?);
    reorder_answered(cx, async |cx| drag(cx, from, to).await).await?;
    let order = cx.update(|_, cx| ids(&channels_of(server, cx)))?;
    ensure!(right_before(&order, &b, &a), "b didn't land above a: {order:?}");
    cx.log("dragged b above a (within its group)");

    // Across groups: a into cat1, above c.
    let (from, to) = (over(cx, server, &a, true).await?, over(cx, server, &c, true).await?);
    reorder_answered(cx, async |cx| drag(cx, from, to).await).await?;
    let order = cx.update(|_, cx| ids(&channels_of(server, cx)))?;
    ensure!(right_before(&order, &a, &c) && right_before(&order, &cat1, &a), "a didn't land in cat1 above c: {order:?}");
    cx.log("dragged a into cat1 (across groups)");

    // A category: cat2 above cat1 (with its block).
    let (from, to) = (over(cx, server, &cat2, true).await?, over(cx, server, &cat1, true).await?);
    reorder_answered(cx, async |cx| drag(cx, from, to).await).await?;
    let order = cx.update(|_, cx| ids(&channels_of(server, cx)))?;
    ensure!(right_before(&order, &cat2, &cat1) && right_before(&order, &cat1, &a), "cat2 didn't land above cat1's block: {order:?}");
    cx.log("dragged cat2 above cat1");

    // Escape cancels: a drag over b, Esc, then the release does nothing (no reorder, no click).
    let before = order.clone();
    let active = cx.update(|_, cx| model(cx).nav.read(cx).active_channel.clone())?;
    let (from, to) = (over(cx, server, &c, true).await?, over(cx, server, &b, true).await?);
    cx.mouse_down(from, 1, Modifiers::default())?;
    for i in 1..=8 {
        let f = i as f32 / 8.;
        cx.mouse_drag(point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f))?;
        cx.sleep(Duration::from_millis(16)).await;
    }
    ensure!(cx.update(|_, cx| reorder::state(cx).read(cx).is_dragging())?, "no drag started");
    cx.keystroke("escape")?;
    ensure!(!cx.update(|_, cx| reorder::state(cx).read(cx).is_dragging())?, "Escape didn't cancel the drag");
    cx.mouse_up(to)?;
    cx.sleep(Duration::from_millis(800)).await;
    let after = cx.update(|_, cx| ids(&channels_of(server, cx)))?;
    ensure!(after == before, "the cancelled drag moved something: {after:?}");
    ensure!(cx.update(|_, cx| model(cx).nav.read(cx).active_channel.clone())? == active, "the release clicked a row");
    cx.log("Escape cancelled a drag; the release did nothing");

    // What a second session must see: this run's channels in this order.
    let mine: Vec<String> = after.iter().filter(|id| id.starts_with(&tag)).map(|id| id.to_string()).collect();
    mark(format_args!("order {}", mine.join(",")));
    Ok(())
}

/// A second carol session: this run's channels arrive in the order the first one left them.
async fn check_observe(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    let expected: Vec<String> = env("DECIBELL_TEST_ORDER")?.split(',').map(str::to_string).collect();
    let tag = tag();
    let e = expected.clone();
    wait_for(&cx, Duration::from_secs(45), "the same order", move |cx| {
        let mine: Vec<String> = channels_of(server, cx).iter().filter(|c| c.id.starts_with(&tag)).map(|c| c.id.to_string()).collect();
        mine == e
    })
    .await?;
    cx.log(format_args!("a second session sees {expected:?}"));
    log_out(&cx).await
}

// ── category-sync-drop ──────────────────────────────────────────────────────────────────────

async fn check_sync_drop(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    clean_up(&cx, server).await?;
    let seeded = cx.update(|_, cx| seeded_order(server, cx))?;
    let result = sync_drops(&cx, server).await;
    clean_up(&cx, server).await?;
    let after = cx.update(|_, cx| seeded_order(server, cx))?;
    ensure!(after == seeded, "the seeded order moved: {seeded:?} → {after:?}");
    result?;
    log_out(&cx).await
}

fn prompt_open(cx: &mut App) -> bool {
    reorder::state_read(cx).is_some_and(|p| p.open)
}

/// The prompt's Keep or Sync button.
async fn prompt_button(cx: &TestCx, sync: bool) -> Result<Point<Pixels>> {
    Ok(wait_probe(cx, if sync { "sync-prompt-sync" } else { "sync-prompt-keep" }).await?.center())
}

async fn sync_drops(cx: &TestCx, server: ServerId) -> Result<()> {
    let tag = tag();
    let x = api_create(cx, server, &format!("{tag}-x"), "text", None).await?;
    let sync = api_create(cx, server, &format!("{tag}-sync"), "category", None).await?;
    let y = api_create(cx, server, &format!("{tag}-y"), "text", Some(&sync)).await?;
    api::channels::set_category_sync(api::channels::SetCategorySyncArgs { server_id: server.wire(), channel_id: sync.to_string(), enabled: true })
        .await
        .map_err(|e| anyhow::anyhow!("set_category_sync: {e}"))?;
    let s = sync.clone();
    wait_for(cx, Duration::from_secs(10), "the category to sync", move |cx| channels_of(server, cx).iter().any(|c| c.id == s && c.category_sync)).await?;
    // x has its own overwrite: @everyone may not attach files.
    let everyone = cx.update(|_, cx| model(cx).community(server, cx).and_then(|c| c.roles.read(cx).list().iter().find(|r| r.is_default).map(|r| r.id)))?.context("@everyone")?;
    api::channels::set_channel_overwrite(api::channels::SetChannelOverwriteArgs {
        server_id: server.wire(),
        channel_id: x.to_string(),
        target_type: "role".into(),
        target_id: everyone.to_string(),
        target_uid: None,
        allow: 0,
        deny: decibell_model::stores::permissions::perm::ATTACH_FILES as i64,
    })
    .await
    .map_err(|e| anyhow::anyhow!("set_channel_overwrite: {e}"))?;
    let overwrites_of = move |cx: &App, id: &ChannelId| model(cx).community(server, cx).and_then(|c| c.channels.read(cx).overwrites(id).map(|o| o.len()));
    let xi = x.clone();
    wait_for(cx, Duration::from_secs(10), "x's overwrite", move |cx| overwrites_of(cx, &xi).is_some_and(|n| n > 0)).await?;
    settle_drawn(cx).await?;

    let into_sync = async |cx: &TestCx| -> Result<()> {
        let (from, to) = (over(cx, server, &x, true).await?, over(cx, server, &y, true).await?);
        drag(cx, from, to).await?;
        wait_for(cx, Duration::from_secs(5), "the prompt", prompt_open).await?;
        cx.sleep(Duration::from_millis(350)).await;
        Ok(())
    };
    let category_of_x = |cx: &App| {
        let list = channels_of(server, cx);
        reorder::category_of(&ids(&list), &list, &x)
    };

    // Cancel: nothing moves.
    let before = cx.update(|_, cx| ids(&channels_of(server, cx)))?;
    into_sync(cx).await?;
    cx.keystroke("escape")?;
    cx.sleep(Duration::from_millis(600)).await;
    ensure!(!cx.update(|_, cx| prompt_open(cx))?, "the prompt is still open");
    ensure!(cx.update(|_, cx| ids(&channels_of(server, cx)))? == before, "Cancel moved x");
    cx.log("Cancel: x stays where it was");

    // Keep: the move, x's own permissions kept.
    reorder_answered(cx, async |cx| {
        into_sync(cx).await?;
        cx.click(prompt_button(cx, false).await?)
    })
    .await?;
    ensure!(cx.update(|_, cx| category_of_x(cx))? == Some(sync.clone()), "Keep didn't move x into the category");
    let xi = x.clone();
    cx.sleep(Duration::from_millis(500)).await;
    let (synced, overwrites) = cx.update(move |_, cx| (channels_of(server, cx).iter().find(|c| c.id == xi).is_some_and(|c| c.permissions_synced), overwrites_of(cx, &xi)))?;
    ensure!(!synced && overwrites.is_some_and(|n| n > 0), "Keep changed x's permissions (synced {synced}, {overwrites:?} overwrites)");
    cx.log("Keep: moved, its overwrite kept");

    // Back out (no prompt: out of a category) above the first channel (text stays above voice:
    // right above the category would be under the seeded voice channels, no spot), then Sync.
    let first = cx.update(|_, cx| channels_of(server, cx))?.first().map(|c| c.id.clone()).context("rows")?;
    let (from, to) = (over(cx, server, &x, true).await?, over(cx, server, &first, true).await?);
    reorder_answered(cx, async |cx| drag(cx, from, to).await).await?;
    ensure!(cx.update(|_, cx| category_of_x(cx))?.is_none(), "x didn't move back out");
    reorder_answered(cx, async |cx| {
        into_sync(cx).await?;
        cx.click(prompt_button(cx, true).await?)
    })
    .await?;
    let xi = x.clone();
    wait_for(cx, Duration::from_secs(15), "x to follow the category", move |cx| {
        channels_of(server, cx).iter().find(|c| c.id == xi).is_some_and(|c| c.permissions_synced) && overwrites_of(cx, &xi).is_none_or(|n| n == 0)
    })
    .await?;
    cx.log("Sync: moved, and it follows the category's permissions");
    Ok(())
}

// ── welcome-banner ──────────────────────────────────────────────────────────────────────────

async fn check_welcome(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    clean_up(&cx, server).await?;
    let id = api_create(&cx, server, &format!("{}-welcome", tag()), "text", None).await?;
    let result = async {
        // Created by us: the channel opens (`channel_action_responded`); click it anyway.
        let c = cx.update(|_, cx| channels_of(server, cx))?.into_iter().find(|c| c.id == id).context("row")?;
        click_probe(&cx, &row_key(&c)).await?;
        wait_probe(&cx, "welcome-invite").await?;
        wait_probe(&cx, "welcome-manage").await?;
        cx.log("the banner shows both actions");
        click_probe(&cx, "welcome-invite").await?;
        wait_for(&cx, Duration::from_secs(5), "Invite friends", |cx| model(cx).overlays.read(cx).is_open(modal::INVITE_FRIENDS)).await?;
        cx.log("Invite friends opened the dialog");
        // Its search field takes the focus (a blinking caret: no settling here).
        cx.sleep(Duration::from_millis(500)).await;
        cx.keystroke("escape")?;
        wait_for(&cx, Duration::from_secs(5), "the dialog to close", |cx| !model(cx).overlays.read(cx).is_open(modal::INVITE_FRIENDS)).await?;
        anyhow::Ok(())
    }
    .await;
    api_delete(&cx, server, &id).await?;
    result?;
    log_out(&cx).await
}

async fn check_sweep(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    wait_for(&cx, Duration::from_secs(30), "Design Lab", move |cx| model(cx).connections.read(cx).is_connected(server)).await?;
    // Let a rejoin that is under way land first.
    cx.sleep(Duration::from_secs(2)).await;
    sweep_members(&cx, server).await?;
    log_out(&cx).await
}

// ── servers-idle ────────────────────────────────────────────────────────────────────────────

async fn quiet_for(cx: &TestCx, what: &str) -> Result<()> {
    let mut last = String::new();
    for attempt in 1..=4 {
        park(cx)?;
        let before = settle_drawn(cx).await?;
        let events = cx.update(|_, cx| model(cx).pump_stats().events)?;
        cx.sleep(Duration::from_secs(5)).await;
        let frames = cx.frames()?.since(before);
        let events = cx.update(|_, cx| model(cx).pump_stats().events)? - events;
        last = format!("{what}: {} drawn, {} presented in 5 s (try {attempt}; {events} core events)", frames.drawn, frames.presented);
        if frames.drawn == 0 {
            cx.log(&last);
            return Ok(());
        }
    }
    bail!("not idle: {last}")
}

async fn check_idle(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let server = community(DESIGN_LAB)?.id;
    open_community(&cx, server).await?;
    clean_up(&cx, server).await?;
    // Browse at rest.
    cx.update(|_, cx| model(cx).nav.update(cx, |n, cx| n.set_active_view(ActiveView::Browse, cx)))?;
    wait_for(&cx, Duration::from_secs(10), "the directory", |cx| !model(cx).servers.read(cx).discoverable().is_empty()).await?;
    quiet_for(&cx, "Browse").await?;
    // The Invites dialog at rest (Unlimited: nothing focused that blinks).
    open_community(&cx, server).await?;
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.open_modal(modal::INVITE_MANAGE, cx)))?;
    wait_for(&cx, Duration::from_secs(10), "the list", move |cx| {
        model(cx).community(server, cx).is_some_and(|c| !c.moderation.read(cx).invites.is_empty())
    })
    .await?;
    quiet_for(&cx, "the Invites dialog").await?;
    cx.keystroke("escape")?;
    // A drag: frames while the pointer moves, none while it holds still, none after Escape.
    let id = api_create(&cx, server, &format!("{}-idle", tag()), "text", None).await?;
    let result = async {
        settle_drawn(&cx).await?;
        let first = cx.update(|_, cx| channels_of(server, cx))?.first().cloned().context("rows")?;
        let (from, to) = (over(&cx, server, &id, true).await?, over(&cx, server, &first.id, true).await?);
        cx.mouse_down(from, 1, Modifiers::default())?;
        let start = cx.frames()?;
        for i in 1..=10 {
            let f = i as f32 / 10.;
            cx.mouse_drag(point(from.x + (to.x - from.x) * f, from.y + (to.y - from.y) * f))?;
            cx.sleep(Duration::from_millis(30)).await;
        }
        let moving = cx.frames()?.since(start);
        ensure!(cx.update(|_, cx| reorder::state(cx).read(cx).is_dragging())?, "no drag");
        cx.sleep(Duration::from_millis(400)).await;
        // Held still: up to three 2 s windows, a window that saw core events (other test clients'
        // presence redraws a dot) taken again, as `quiet_for` does.
        let mut still = Frames { drawn: 0, presented: 0 };
        for attempt in 1..=3 {
            let held = cx.frames()?;
            let events = cx.update(|_, cx| model(cx).pump_stats().events)?;
            cx.sleep(Duration::from_secs(2)).await;
            still = cx.frames()?.since(held);
            let events = cx.update(|_, cx| model(cx).pump_stats().events)? - events;
            if still.drawn == 0 || events == 0 {
                break;
            }
            cx.log(format_args!("held still: {} drawn with {events} core events (try {attempt}): again", still.drawn));
        }
        cx.keystroke("escape")?;
        cx.mouse_up(to)?;
        let after = settle_drawn(&cx).await?;
        cx.sleep(Duration::from_secs(2)).await;
        let rest = cx.frames()?.since(after);
        cx.log(format_args!(
            "dragging: {} frames over 10 moves, {} while held still 2 s, {} in 2 s after Escape",
            moving.drawn, still.drawn, rest.drawn
        ));
        ensure!(moving.drawn > 0 && moving.drawn <= 12, "{} frames for 10 moves", moving.drawn);
        ensure!(still.drawn == 0, "{} frames while the pointer held still", still.drawn);
        ensure!(rest.drawn == 0, "{} frames after the drag ended", rest.drawn);
        anyhow::Ok(())
    }
    .await;
    api_delete(&cx, server, &id).await?;
    result?;
    log_out(&cx).await
}
