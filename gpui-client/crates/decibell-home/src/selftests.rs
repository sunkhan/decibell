//! The people self-tests, against the local test stack (`tools/dev/README.md`).
//!
//! Each starts the app again in child processes — `decibell --check home-…` with its own temporary
//! profile (`DECIBELL_USER_DATA_DIR`), the stack's `DECIBELL_CENTRAL` and `DECIBELL_TEST_PROBES=1`
//! — and passes when every child does. They refuse to run unless `DECIBELL_CENTRAL` is loopback and
//! skip, saying so, when the stack is down or the binary has no `dev-local`.
//!
//! - Read-only checks sign in as the seeded `alice` (`--login alice`): the conversation with bob
//!   (paging back), the Friends page's tabs, carol's popout from Daily Gaming, the user menu's
//!   moderation entries (a nickname set and reverted on a placeholder member, the ban dialog opened
//!   and cancelled), idle frames.
//! - Anything that changes state (DMs, friend requests, encryption, statuses) runs between two
//!   **throwaway accounts** registered once per run through the core's `register` (two sign-ups),
//!   each process signing in by password. The second participant is a second app process, not
//!   `decibell-headless` (written before its `register` / `login-as` steps existed). The children
//!   meet through cue files (`<dir>/<name>` written by one, waited for by the other). Every child
//!   signs out at its end (central forgets the device), and holds its window "active"
//!   (`decibell_harness::focus`): a DM is read only in a focused window, and other windows share
//!   this desktop.
//! - `dm-read-rules` (`selftests/read_rules.rs`): the app as the reader, its account's second
//!   device and the sender as `decibell-headless` clients of two accounts the headless registers.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_model::chat::Message;
use decibell_model::live::e2ee::{E2eeStatus, PassphraseMode};
use decibell_model::live::friends::{FriendAction, FriendStatus};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::stores::overlays::modal;
use decibell_model::{Model, ServerId, Uid, actions};
use decibell_shell::probes;
use gpui::{App, Bounds, Pixels, point, px};

use crate::dm::Conversation;
use crate::e2ee::{PeerInfos, current_key};
use crate::friends::FriendsPage;
use crate::menu::UserMenu;
use crate::profile::ProfilePopup;
use crate::status::{ClearAfter, CustomStatusModal};

#[path = "selftests/read_rules.rs"]
mod read_rules;

#[path = "selftests/people.rs"]
mod people;

const ALICE: &str = "alice";
const BOB_UID: Uid = 2;
const CAROL_UID: Uid = 3;
const DAVE_UID: Uid = 4;
const ERIN_UID: Uid = 5;
const DAILY_GAMING: i32 = 1;
const RUSTACEANS: i32 = 2;
const PASSPHRASE: &str = "home-selftest-passphrase";

// ── The parents ───────────────────────────────────────────────────────────────────────────────

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "dm-open-history", about: "as alice: bob's conversation, its newest page, three older pages with 0 px anchor movement", run: |cx| Box::pin(alice(cx, "home-dm-open-history")) },
        SelfTest { name: "dm-send-receive", about: "two throwaway accounts: quick DM + composer send, the reply arrives, unread badge and tile, cleared on open", run: |cx| Box::pin(parties(cx, "dm-send-receive", &["a", "b"])) },
        SelfTest { name: "dm-read-sync", about: "two devices of a throwaway account: reading on one clears the unread count on the other", run: |cx| Box::pin(parties(cx, "dm-read-sync", &["a", "a2", "b"])) },
        SelfTest { name: "dm-read-rules", about: "a DM is read only while open, focused and at the bottom: unfocused / scrolled up / minimised it stays unread here and on a second device until that holds", run: |cx| Box::pin(read_rules::parent(cx)) },
        SelfTest { name: "dm-e2ee", about: "two throwaway accounts set up encryption: the chip reads Encrypted, a message arrives decrypted; a fresh profile is locked until unlocked", run: |cx| Box::pin(e2ee(cx)) },
        SelfTest { name: "friends-flow", about: "throwaway accounts: request by name, accept, both lists, remove; block / unblock", run: |cx| Box::pin(parties(cx, "friends-flow", &["a", "b"])) },
        SelfTest { name: "friends-page", about: "as alice: the tabs show the seeded state (bob, carol friends; dave pending; erin blocked)", run: |cx| Box::pin(alice(cx, "home-friends-page")) },
        SelfTest { name: "profile-popup", about: "as alice: carol's popout from Daily Gaming's member list shows her roles; Escape closes it", run: |cx| Box::pin(alice(cx, "home-profile-popup")) },
        SelfTest { name: "status-custom", about: "a throwaway account sets a status message clearing after 30 min, a watcher sees it, then it is cleared", run: |cx| Box::pin(parties(cx, "status-custom", &["a", "b"])) },
        SelfTest { name: "user-menu-moderation", about: "as alice (owner): a placeholder's menu, nickname set and reverted, the ban dialog opened and cancelled; none as a member", run: |cx| Box::pin(alice(cx, "home-user-menu-moderation")) },
        SelfTest { name: "home-idle", about: "as alice: the Friends page and an open conversation at rest draw 0 frames over 5 s", run: |cx| Box::pin(alice(cx, "home-idle")) },
    ]
    .into_iter()
    .chain(people::selftests())
    .collect()
}

/// The stack as `accounts.json` describes it.
struct Stack {
    central: String,
    password: String,
}

fn dev_dir() -> PathBuf {
    std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"))
}

/// `"key": "value"` in `accounts.json`, first match (top-level strings only are read).
fn json_str(text: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let at = text.find(&needle)? + needle.len();
    let rest = text[at..].trim_start().strip_prefix(':')?.trim_start().strip_prefix('"')?;
    Some(rest[..rest.find('"')?].to_string())
}

fn stack() -> Result<Option<Stack>> {
    let path = dev_dir().join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json_str(&text, "central")).context("no central address")?;
    let password = json_str(&text, "password").context("accounts.json has no password")?;
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: these tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
}

/// A temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Result<TempDir> {
        let dir = std::env::temp_dir().join(format!("decibell-home-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

enum Outcome {
    Passed,
    NoDevLogin,
}

/// One child app: `decibell --check <check>` with its profile and environment.
struct Child {
    label: String,
    process: std::process::Child,
}

fn spawn(check: &str, label: &str, login: Option<&str>, stack: &Stack, profile: &Path, env: &[(&str, String)]) -> Result<Child> {
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--check").arg(check);
    if let Some(account) = login {
        command.arg("--login").arg(account);
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
    Ok(Child { label: label.to_string(), process: command.spawn().context("starting the app")? })
}

/// Wait for every child, relay their logs (prefixed), fail when one failed.
async fn finish(cx: &TestCx, children: Vec<Child>) -> Result<Outcome> {
    let started = Instant::now();
    let results = cx
        .app()
        .background_executor()
        .spawn(async move {
            let handles: Vec<_> = children
                .into_iter()
                .map(|mut child| {
                    let stdout = child.process.stdout.take().expect("piped");
                    let stderr = child.process.stderr.take().expect("piped");
                    let label = child.label.clone();
                    std::thread::Builder::new()
                        .name(format!("decibell-test-{label}"))
                        .spawn(move || {
                            let err = std::thread::Builder::new()
                                .name("decibell-test-stderr".into())
                                .spawn(move || std::io::BufReader::new(stderr).lines().map_while(Result::ok).collect::<Vec<_>>())
                                .ok();
                            let out: Vec<String> = std::io::BufReader::new(stdout).lines().map_while(Result::ok).collect();
                            let status = child.process.wait();
                            let err = err.and_then(|h| h.join().ok()).unwrap_or_default();
                            (label, status, out, err)
                        })
                        .expect("a thread")
                })
                .collect();
            handles.into_iter().filter_map(|h| h.join().ok()).collect::<Vec<_>>()
        })
        .await;
    let mut failed = Vec::new();
    let mut no_dev = false;
    for (label, status, out, err) in results {
        for line in out.iter().filter(|l| !l.starts_with("selftest:")) {
            cx.log(format_args!("│ {label}: {line}"));
        }
        if err.iter().any(|l| l.contains("--login refused")) && err.iter().any(|l| l.contains("dev-local")) {
            no_dev = true;
            continue;
        }
        let status = status?;
        if !status.success() {
            for line in err.iter().rev().take(12).rev() {
                cx.log(format_args!("│ {label} stderr: {line}"));
            }
            failed.push(format!("{label} ({status})"));
        }
    }
    if no_dev {
        return Ok(Outcome::NoDevLogin);
    }
    ensure!(failed.is_empty(), "failed in the app: {} (after {:.1} s)", failed.join(", "), started.elapsed().as_secs_f64());
    Ok(Outcome::Passed)
}

async fn alice(cx: TestCx, check: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = TempDir::new(check)?;
    let child = spawn(check, "alice", Some(ALICE), &stack, &profile.0, &[])?;
    match finish(&cx, vec![child]).await? {
        Outcome::Passed => Ok(()),
        Outcome::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

/// The run's two throwaway accounts.
#[derive(Clone)]
struct Accounts {
    a: String,
    b: String,
    password: String,
    registered: bool,
}

static ACCOUNTS: Mutex<Option<Accounts>> = Mutex::new(None);

/// The throwaway accounts, registered on first use (two sign-ups per run of the binary).
async fn accounts(cx: &TestCx, stack: &Stack) -> Result<Option<Accounts>> {
    let current = ACCOUNTS.lock().expect("lock").clone();
    let accounts = current.unwrap_or_else(|| {
        let tag = format!("{:x}", std::process::id() ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)));
        Accounts { a: format!("ht{tag}a"), b: format!("ht{tag}b"), password: format!("home-test-{tag}-pw"), registered: false }
    });
    if accounts.registered {
        return Ok(Some(accounts));
    }
    cx.log(format_args!("registering the throwaway accounts {} and {}", accounts.a, accounts.b));
    let mut children = Vec::new();
    let mut dirs = Vec::new();
    for (role, name) in [("a", &accounts.a), ("b", &accounts.b)] {
        let dir = TempDir::new(&format!("register-{role}"))?;
        let env = actor_env("register", role, name, "", &accounts.password, &dir.0);
        children.push(spawn("home-actor", role, None, stack, &dir.0, &env)?);
        dirs.push(dir);
    }
    if let Outcome::NoDevLogin = finish(cx, children).await? {
        return Ok(None);
    }
    let accounts = Accounts { registered: true, ..accounts };
    *ACCOUNTS.lock().expect("lock") = Some(accounts.clone());
    Ok(Some(accounts))
}

fn actor_env(script: &str, role: &str, me: &str, peer: &str, password: &str, cues: &Path) -> Vec<(&'static str, String)> {
    vec![
        ("DECIBELL_HOME_SCRIPT", script.to_string()),
        ("DECIBELL_HOME_ROLE", role.to_string()),
        ("DECIBELL_HOME_ME", me.to_string()),
        ("DECIBELL_HOME_PEER", peer.to_string()),
        ("DECIBELL_HOME_PASSWORD", password.to_string()),
        ("DECIBELL_HOME_CUES", cues.display().to_string()),
    ]
}

/// Run `script` with one child app per role (`a`, `a2` sign in as account A, `b` as B).
async fn run_roles(cx: &TestCx, stack: &Stack, accounts: &Accounts, script: &str, roles: &[&str], cues: &Path) -> Result<Outcome> {
    let mut children = Vec::new();
    let mut profiles = Vec::new();
    for role in roles {
        let (me, peer) = if role.starts_with('a') { (&accounts.a, &accounts.b) } else { (&accounts.b, &accounts.a) };
        let profile = TempDir::new(&format!("{script}-{role}"))?;
        let env = actor_env(script, role, me, peer, &accounts.password, cues);
        children.push(spawn("home-actor", role, None, stack, &profile.0, &env)?);
        profiles.push(profile);
    }
    finish(cx, children).await
}

async fn parties(cx: TestCx, script: &'static str, roles: &'static [&'static str]) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let Some(accounts) = accounts(&cx, &stack).await? else {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    };
    let cues = TempDir::new(&format!("{script}-cues"))?;
    run_roles(&cx, &stack, &accounts, script, roles, &cues.0).await.map(|_| ())
}

async fn e2ee(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let Some(accounts) = accounts(&cx, &stack).await? else {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    };
    let cues = TempDir::new("dm-e2ee-cues")?;
    run_roles(&cx, &stack, &accounts, "dm-e2ee", &["a", "b"], &cues.0).await?;
    cx.log("a fresh profile of the first account: locked until the passphrase unlocks it");
    run_roles(&cx, &stack, &accounts, "dm-e2ee", &["a-fresh"], &cues.0).await.map(|_| ())
}

// ── The checks, inside the app ───────────────────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "home-dm-open-history", about: "(in the app) bob's conversation pages back", run: |cx| Box::pin(check_open_history(cx)) },
        SelfTest { name: "home-friends-page", about: "(in the app) the Friends page's tabs", run: |cx| Box::pin(check_friends_page(cx)) },
        SelfTest { name: "home-profile-popup", about: "(in the app) carol's popout", run: |cx| Box::pin(check_profile_popup(cx)) },
        SelfTest { name: "home-user-menu-moderation", about: "(in the app) the moderation entries", run: |cx| Box::pin(check_user_menu(cx)) },
        SelfTest { name: "home-idle", about: "(in the app) idle frames", run: |cx| Box::pin(check_idle(cx)) },
        SelfTest { name: "home-actor", about: "(in the app) one participant of a two-account test", run: |cx| Box::pin(actor(cx)) },
        SelfTest { name: "home-dm-read-rules", about: "(in the app) the reader of dm-read-rules", run: |cx| Box::pin(read_rules::check(cx)) },
    ]
    .into_iter()
    .chain(people::checks())
    .collect()
}

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

/// Poll `ready` every 50 ms for at most `limit`.
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

async fn sign_out(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::auth::sign_out(cx))?;
    wait_for(cx, Duration::from_secs(10), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}

/// Until nothing has been drawn for half a second (presents alone don't count: the active server
/// tile's glow re-presents; other windows on a shared desktop can keep redrawing ours, so after
/// 10 s it goes on, saying so).
async fn quiet(cx: &TestCx) -> Result<()> {
    let started = Instant::now();
    let mut last = cx.frames()?.drawn;
    let mut since = Instant::now();
    loop {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames()?.drawn;
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(500) {
            return Ok(());
        }
        if started.elapsed() > Duration::from_secs(10) {
            cx.log("the window kept drawing for 10 s; going on");
            return Ok(());
        }
    }
}

async fn wait_probe(cx: &TestCx, key: &str) -> Result<Bounds<Pixels>> {
    let owned = key.to_string();
    wait_for(cx, Duration::from_secs(10), &format!("{key} on screen"), move |_| probes::bounds(&owned).is_some()).await?;
    probes::bounds(key).context("probe vanished")
}

/// The pointer over the window's empty lower middle.
fn park_pointer(cx: &TestCx) -> Result<()> {
    let at = cx.update(|window, _| {
        let size = window.viewport_size();
        point(size.width * 0.55, size.height * 0.85)
    })?;
    cx.mouse_move(at)
}

async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    let bounds = wait_probe(cx, key).await?;
    cx.click(bounds.center())?;
    park_pointer(cx)
}

fn daily_gaming() -> ServerId {
    ServerId::from_central(DAILY_GAMING)
}

async fn connected(cx: &TestCx, server: ServerId) -> Result<()> {
    wait_for(cx, Duration::from_secs(30), "the community to connect", move |cx| model(cx).connections.read(cx).is_connected(server)).await?;
    wait_for(cx, Duration::from_secs(15), "its roster", move |cx| model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).meta().is_some())).await?;
    Ok(())
}

/// Rows in view as (id, screen y), captured by an observer.
type Captured = std::rc::Rc<std::cell::RefCell<Option<Vec<(i64, f32)>>>>;

/// The shown conversation's list: (id, screen y) of the rows in the viewport.
fn visible_rows(cx: &mut App) -> Vec<(i64, f32)> {
    let Some(conversation) = crate::hooks::shown::<Conversation>(cx) else { return Vec::new() };
    let list = conversation.read(cx).list().clone();
    let list = list.read(cx);
    list.visible_ids().into_iter().filter(|id| *id > 0).filter_map(|id| list.row_screen_y(id).map(|y| (id, y))).collect()
}

async fn check_open_history(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    cx.update(|_, cx| actions::dm::open_dm(BOB_UID, "bob".into(), cx))?;
    let t = wait_for(&cx, Duration::from_secs(15), "bob's newest page", |cx| {
        let m = model(cx);
        let fetched = m.dm.list.read(cx).slice(BOB_UID).is_some_and(|s| s.read(cx).fetched);
        fetched && !visible_rows(cx).is_empty()
    })
    .await?;
    let rows = cx.update(|_, cx| model(cx).dm.list.read(cx).slice(BOB_UID).map_or(0, |s| s.read(cx).rows().len()))?;
    cx.log(format_args!("the newest page is drawn {} ms after opening: {rows} rows", t.as_millis()));
    ensure!(rows >= 20, "a page of bob's conversation has {rows} rows");
    cx.settle().await?;
    // Where the rows in view were drawn the moment a page landed (the list draws it a frame
    // later): the slice's observer runs before the list renders the change.
    let before = std::rc::Rc::new(std::cell::Cell::new(usize::MAX));
    let captured: Captured = Default::default();
    let _watch = cx.update({
        let (before, captured) = (before.clone(), captured.clone());
        move |_, cx| {
            let slice = model(cx).dm.list.read(cx).slice(BOB_UID).cloned().expect("the slice");
            cx.observe(&slice, move |slice, cx| {
                if slice.read(cx).rows().len() > before.get() && captured.borrow().is_none() {
                    *captured.borrow_mut() = Some(visible_rows(cx));
                }
            })
        }
    })?;
    let mut worst = 0f32;
    let mut pages = 0;
    for _ in 0..20 {
        if pages >= 3 {
            break;
        }
        let (rows, more) = cx.update(|_, cx| {
            let m = model(cx);
            let slice = m.dm.list.read(cx).slice(BOB_UID).cloned().expect("the slice");
            let s = slice.read(cx);
            (s.rows().len(), s.has_more_before)
        })?;
        if !more {
            break;
        }
        before.set(rows);
        *captured.borrow_mut() = None;
        // Up by touchpad (no animation) to 300 px below the loaded top, where the list asks for
        // the older page.
        cx.update(|_, cx| {
            if let Some(conversation) = crate::hooks::shown::<Conversation>(cx) {
                let list = conversation.read(cx).list().clone();
                list.update(cx, |list, cx| {
                    let (min, _) = list.scroll_range();
                    let position = list.scroll_position(cx);
                    list.scroll_by_precise(min + 300. - position, cx);
                });
            }
        })?;
        let captured_now = captured.clone();
        if wait_for(&cx, Duration::from_secs(5), "an older page", move |_| captured_now.borrow().is_some()).await.is_err() {
            continue;
        }
        cx.settle().await?;
        let anchor = captured.borrow().clone().unwrap_or_default();
        let after = cx.update(|_, cx| visible_rows(cx))?;
        let shared: Vec<f32> =
            anchor.iter().filter_map(|(id, y)| after.iter().find(|(i, _)| i == id).map(|(_, y2)| (y2 - y).abs())).collect();
        ensure!(!shared.is_empty(), "no row stayed in view across the page");
        let moved = shared.iter().copied().fold(0f32, f32::max);
        worst = worst.max(moved);
        pages += 1;
        let total = cx.update(|_, cx| model(cx).dm.list.read(cx).slice(BOB_UID).map_or(0, |s| s.read(cx).rows().len()))?;
        cx.log(format_args!("page {pages}: {rows} → {total} rows; {} rows in view moved at most {moved:.2} px", shared.len()));
    }
    ensure!(pages > 0, "no older page arrived");
    ensure!(worst < 0.5, "rows in view moved {worst:.2} px when a page arrived above them");
    sign_out(&cx).await
}

async fn check_friends_page(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    wait_for(&cx, Duration::from_secs(15), "the friend list", |cx| !model(cx).live.friends.read(cx).list().is_empty()).await?;
    let status_of = |cx: &mut App, uid: Uid| model(cx).live.friends.read(cx).get(uid).map(|f| f.status);
    let seeded = cx.update(|_, cx| (status_of(cx, BOB_UID), status_of(cx, CAROL_UID), status_of(cx, DAVE_UID), status_of(cx, ERIN_UID)))?;
    cx.log(format_args!("bob {:?}, carol {:?}, dave {:?}, erin {:?}", seeded.0, seeded.1, seeded.2, seeded.3));
    ensure!(seeded.0.is_some_and(FriendStatus::is_accepted) && seeded.1.is_some_and(FriendStatus::is_accepted), "bob and carol are friends");
    ensure!(seeded.2 == Some(FriendStatus::PendingIncoming), "dave's request is pending");
    ensure!(seeded.3 == Some(FriendStatus::Blocked), "erin is blocked");
    let tabs: [(&str, &[Uid], &[Uid]); 4] = [
        ("friends-filter-pending", &[DAVE_UID], &[BOB_UID, ERIN_UID]),
        ("friends-filter-blocked", &[ERIN_UID], &[BOB_UID, DAVE_UID]),
        ("friends-filter-online", &[], &[DAVE_UID, ERIN_UID]),
        ("friends-filter-all", &[BOB_UID, CAROL_UID, DAVE_UID, ERIN_UID], &[]),
    ];
    for (tab, shown, hidden) in tabs {
        click_probe(&cx, tab).await?;
        cx.settle().await?;
        let rows: Vec<Uid> = crate::hooks::drawn("friends-rows").iter().filter_map(|s| s.parse().ok()).collect();
        cx.log(format_args!("{tab}: rows {rows:?}"));
        for uid in shown {
            ensure!(rows.contains(uid), "{tab} doesn't list {uid}");
        }
        for uid in hidden {
            ensure!(!rows.contains(uid), "{tab} lists {uid}");
        }
    }
    let filter = cx.update(|_, cx| crate::hooks::shown::<FriendsPage>(cx).map(|p| p.read(cx).filter()))?;
    ensure!(filter == Some(crate::friends::Filter::All), "the last tab clicked is All");
    sign_out(&cx).await
}

async fn open_daily_gaming(cx: &TestCx) -> Result<()> {
    connected(cx, daily_gaming()).await?;
    click_probe(cx, "server-tile:1").await?;
    wait_for(cx, Duration::from_secs(10), "Daily Gaming", |cx| model(cx).nav.read(cx).active_server == Some(daily_gaming())).await?;
    Ok(())
}

async fn check_profile_popup(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    open_daily_gaming(&cx).await?;
    let server = daily_gaming();
    cx.update(|_, cx| model(cx).community(server, cx).map(|c| c.roster.update(cx, |r, cx| r.want(CAROL_UID, cx))))?;
    wait_probe(&cx, "member-list").await?;
    quiet(&cx).await?;
    // A member row drawn in the list whose member holds a role: carol (Moderator) when she is in
    // view, else whoever else is (a probe keeps the last place a row was drawn; only rows inside
    // the list's bounds count).
    let (uid, roles) = cx.update(|_, cx| {
        let community = model(cx).community(server, cx).expect("connected");
        let roles = community.roles.read(cx).list().to_vec();
        let roster = community.roster.read(cx);
        let list = probes::bounds("member-list");
        let in_view = |uid: Uid| {
            probes::bounds(&format!("member:{uid}")).is_some_and(|row| list.is_some_and(|l| l.contains(&row.center())))
        };
        let named = |ids: &[i64]| ids.iter().filter_map(|id| roles.iter().find(|r| r.id == *id).map(|r| r.name.to_string())).collect::<Vec<_>>();
        let pick = std::iter::once(CAROL_UID)
            .chain(roster.visible().online.iter().copied())
            .filter_map(|uid| roster.member(uid).map(|m| (uid, named(&m.role_ids))))
            .find(|(uid, held)| !held.is_empty() && in_view(*uid));
        pick.unwrap_or((CAROL_UID, roster.member(CAROL_UID).map(|m| named(&m.role_ids)).unwrap_or_default()))
    })?;
    let row = probes::bounds(&format!("member:{uid}")).filter(|row| probes::bounds("member-list").is_some_and(|l| l.contains(&row.center())));
    match row {
        Some(row) => {
            cx.log(format_args!("clicking member {uid}'s row (roles {roles:?})"));
            cx.click(row.center())?;
            park_pointer(&cx)?;
        }
        None => {
            cx.log("no member with a role is in view: opening carol's popout as her row would");
            cx.update(|_, cx| decibell_common::open_profile_at(CAROL_UID, point(px(900.), px(160.)), Some(server), cx))?;
        }
    }
    wait_for(&cx, Duration::from_secs(5), "the popout", move |cx| model(cx).overlays.read(cx).profile_popup.as_ref().is_some_and(|p| p.uid == uid)).await?;
    let want = roles.clone();
    wait_for(&cx, Duration::from_secs(10), "the member's roles", move |_| crate::hooks::drawn("profile-popup-roles") == want).await?;
    cx.log(format_args!("the popout shows the roles {:?}", crate::hooks::drawn("profile-popup-roles")));
    ensure!(!roles.is_empty(), "no role to show");
    let opened = cx.update(|_, cx| crate::hooks::shown::<ProfilePopup>(cx).is_some())?;
    ensure!(opened, "the popout view");
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "Escape to close it", |cx| model(cx).overlays.read(cx).profile_popup.is_none()).await?;
    sign_out(&cx).await
}

fn menu_sections() -> Vec<String> {
    crate::hooks::drawn("user-menu-sections")
}

async fn check_user_menu(cx: TestCx) -> Result<()> {
    // The menu closes when its window loses the focus; other windows share this desktop.
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    signed_in(&cx).await?;
    open_daily_gaming(&cx).await?;
    let server = daily_gaming();
    // A placeholder member (no central account: the stack seeds them from uid 100001; a
    // community's own placeholders are negative), from the roster, paging it in if needed.
    let find = move |cx: &mut App| {
        let community = model(cx).community(server, cx)?;
        let roster = community.roster.read(cx);
        roster
            .members()
            .values()
            .filter(|m| (m.uid < 0 || m.uid > 100_000) && !m.is_owner && m.timed_out_until == 0)
            .map(|m| (m.uid, m.nickname.to_string()))
            .min()
    };
    for _ in 0..10 {
        if cx.update(|_, cx| find(cx))?.is_some() {
            break;
        }
        cx.update(|_, cx| model(cx).community(server, cx).map(|c| c.roster.update(cx, |r, cx| r.load_more(cx))))?;
        cx.sleep(Duration::from_millis(500)).await;
    }
    let placeholder = cx.update(|_, cx| find(cx))?.context("no placeholder member in the roster")?;
    let (uid, original) = placeholder;
    cx.log(format_args!("placeholder {uid}, nickname {original:?}"));
    cx.update(|_, cx| decibell_common::open_user_menu(uid, point(px(700.), px(200.)), Some(server), cx))?;
    wait_for(&cx, Duration::from_secs(5), "the menu", |_| menu_sections().iter().any(|s| s == "500")).await?;
    quiet(&cx).await?;
    for row in ["menu-nickname", "menu-roles", "menu-kick", "menu-ban"] {
        wait_probe(&cx, &format!("user-menu:{row}")).await?;
    }
    cx.log("the owner's menu has Set / Change nickname, Edit roles, Kick, Ban");
    // The voice crate's sections, joined by the app into this one menu: local mute and the
    // volume (300); a drag of the slider applies live and keeps the menu open.
    let sections = menu_sections();
    ensure!(sections.iter().any(|s| s == "300"), "the voice sections are in the menu: {sections:?}");
    let volume = |cx: &App| decibell_model::live::voice::gain::user_db(model(cx).settings.read(cx).get(), uid);
    let before = cx.update(move |_, cx| volume(cx))?;
    let slider = wait_probe(&cx, "user-menu:user-menu-volume").await?;
    let (from, to) = (slider.center(), point(slider.center().x + px(36.), slider.center().y));
    cx.mouse_down(from, 1, Default::default())?;
    let mut moved = Vec::new();
    for step in 1..=6 {
        cx.mouse_drag(point(from.x + px(6. * step as f32), from.y))?;
        cx.sleep(Duration::from_millis(30)).await;
        moved.push(cx.update(move |_, cx| volume(cx))?);
    }
    cx.mouse_up(to)?;
    park_pointer(&cx)?;
    let (after, open) = cx.update(move |_, cx| (volume(cx), model(cx).overlays.read(cx).context_menu.is_some()))?;
    cx.log(format_args!("the volume slider dragged: {before:?} → {after:?} dB (live while dragging: {moved:?}); the menu stayed open: {open}"));
    ensure!(open, "dragging the volume closed the menu");
    // The press puts the thumb under the pointer; the drag to the right raises it from there.
    ensure!(after.is_some() && after > moved[0], "the drag raised the volume ({:?} → {after:?})", moved[0]);
    ensure!(moved.windows(2).any(|w| w[0] != w[1]), "the volume didn't follow the drag live: {moved:?}");
    cx.update(move |_, cx| actions::voice::set_user_volume(uid, decibell_model::live::voice::gain::DEFAULT_DB, cx))?;
    quiet(&cx).await?;
    // Nickname: set, settle on the record, then back.
    for (step, nickname) in [("set", "selftest-nick"), ("revert", original.as_str())] {
        if step == "set" {
            click_probe(&cx, "user-menu:menu-nickname").await?;
        } else {
            cx.update(|_, cx| crate::menu::request_dialog(crate::menu::DialogRequest::Nickname { server, uid }, cx))?;
        }
        wait_for(&cx, Duration::from_secs(5), "the nickname dialog", |cx| crate::hooks::shown::<UserMenu>(cx).is_some_and(|m| m.read(cx).dialog().is_some()))
            .await?;
        quiet(&cx).await?;
        cx.keystroke("ctrl-a")?;
        cx.keystroke("backspace")?;
        cx.type_text(nickname)?;
        cx.keystroke("enter")?;
        let want = nickname.to_string();
        let t = wait_for(&cx, Duration::from_secs(10), "the nickname on the member", move |cx| {
            model(cx).community(server, cx).and_then(|c| c.roster.read(cx).member(uid).map(|m| m.nickname.to_string())) == Some(want.clone())
        })
        .await?;
        wait_for(&cx, Duration::from_secs(5), "the dialog to close on the echo", |cx| crate::hooks::shown::<UserMenu>(cx).is_some_and(|m| m.read(cx).dialog().is_none()))
            .await?;
        cx.log(format_args!("nickname {step} to {nickname:?}: on the record after {} ms, the dialog closed", t.as_millis()));
    }
    // The ban dialog: opened from the menu, cancelled.
    cx.update(|_, cx| decibell_common::open_user_menu(uid, point(px(700.), px(200.)), Some(server), cx))?;
    quiet(&cx).await?;
    click_probe(&cx, "user-menu:menu-ban").await?;
    wait_for(&cx, Duration::from_secs(5), "the ban dialog", |cx| crate::hooks::shown::<UserMenu>(cx).is_some_and(|m| m.read(cx).dialog().is_some())).await?;
    quiet(&cx).await?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape to cancel it", |cx| crate::hooks::shown::<UserMenu>(cx).is_some_and(|m| m.read(cx).dialog().is_none())).await?;
    let banned = cx.update(|_, cx| model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).member(uid).is_none()))?;
    ensure!(!banned, "the placeholder left the roster");
    cx.log("the ban dialog opened and was cancelled; nobody was banned");
    // As a plain member (alice in Rustaceans, owned by bob): no moderation section.
    let rust = ServerId::from_central(RUSTACEANS);
    connected(&cx, rust).await?;
    cx.update(|_, cx| decibell_common::open_user_menu(BOB_UID, point(px(700.), px(200.)), Some(rust), cx))?;
    wait_for(&cx, Duration::from_secs(5), "the menu on bob", |cx| model(cx).overlays.read(cx).context_menu.as_ref().is_some_and(|m| m.uid == BOB_UID)).await?;
    quiet(&cx).await?;
    let sections = menu_sections();
    cx.log(format_args!("bob's menu in Rustaceans, as a member: sections {sections:?}"));
    ensure!(!sections.iter().any(|s| s == "500"), "a member sees moderation entries");
    cx.update(|_, cx| crate::menu::close_user_menu(cx))?;
    // Her own menu: "Set nickname" where @everyone may change theirs (Daily Gaming), nothing in
    // Rustaceans, where @everyone lacks Change Nickname.
    let me = cx.update(|_, cx| own_uid(cx))?;
    for (server, offered) in [(server, true), (rust, false)] {
        cx.update(move |_, cx| decibell_common::open_user_menu(me, point(px(700.), px(200.)), Some(server), cx))?;
        wait_for(&cx, Duration::from_secs(5), "our own menu", move |cx| model(cx).overlays.read(cx).context_menu.as_ref().is_some_and(|m| m.uid == me && m.server == Some(server))).await?;
        quiet(&cx).await?;
        let sections = menu_sections();
        let has = sections.iter().any(|s| s == "500");
        cx.log(format_args!("our own menu in {server:?}: sections {sections:?} (nickname offered: {has})"));
        ensure!(has == offered, "our own nickname entry in {server:?}: {has}, expected {offered}");
        if offered {
            wait_probe(&cx, "user-menu:menu-nickname").await?;
        }
        cx.update(|_, cx| crate::menu::close_user_menu(cx))?;
        quiet(&cx).await?;
    }
    sign_out(&cx).await
}

/// Nothing drawn or presented for `seconds` after drawing stopped (a pointer crossing the window or
/// another window taking the focus draws: up to three tries, as the shell's idle test).
async fn assert_idle(cx: &TestCx, seconds: u64, what: &str) -> Result<()> {
    let mut last = Frames { drawn: 0, presented: 0 };
    for attempt in 1..=3 {
        let before = cx.settle().await?;
        cx.sleep(Duration::from_secs(seconds)).await;
        last = cx.frames()?.since(before);
        cx.log(format_args!("{what}: try {attempt}: {} drawn, {} presented in {seconds} s", last.drawn, last.presented));
        if last.drawn == 0 && last.presented == 0 {
            return Ok(());
        }
    }
    bail!("{what}: not idle ({} drawn, {} presented in {seconds} s)", last.drawn, last.presented)
}

async fn check_idle(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    wait_for(&cx, Duration::from_secs(15), "the friend list", |cx| !model(cx).live.friends.read(cx).list().is_empty()).await?;
    wait_probe(&cx, "friends-filter-all").await?;
    park_pointer(&cx)?;
    assert_idle(&cx, 5, "the Friends page").await?;
    cx.update(|_, cx| actions::dm::open_dm(BOB_UID, "bob".into(), cx))?;
    wait_for(&cx, Duration::from_secs(15), "bob's conversation", |cx| !visible_rows(cx).is_empty()).await?;
    park_pointer(&cx)?;
    assert_idle(&cx, 5, "an open conversation").await?;
    sign_out(&cx).await
}

// ── The throwaway participants ────────────────────────────────────────────────────────────────

struct Actor {
    script: String,
    role: String,
    me: String,
    peer: String,
    password: String,
    cues: PathBuf,
}

impl Actor {
    fn from_env() -> Result<Actor> {
        let var = |k: &str| std::env::var(k).with_context(|| format!("{k} is not set"));
        Ok(Actor {
            script: var("DECIBELL_HOME_SCRIPT")?,
            role: var("DECIBELL_HOME_ROLE")?,
            me: var("DECIBELL_HOME_ME")?,
            peer: std::env::var("DECIBELL_HOME_PEER").unwrap_or_default(),
            password: var("DECIBELL_HOME_PASSWORD")?,
            cues: var("DECIBELL_HOME_CUES")?.into(),
        })
    }

    /// (account A's name, account B's name): roles `a…` sign in as A.
    fn accounts(&self) -> (String, String) {
        if self.role.starts_with('a') { (self.me.clone(), self.peer.clone()) } else { (self.peer.clone(), self.me.clone()) }
    }

    fn cue(&self, name: &str, content: impl std::fmt::Display) -> Result<()> {
        let tmp = self.cues.join(format!(".{name}.tmp"));
        std::fs::write(&tmp, content.to_string())?;
        std::fs::rename(tmp, self.cues.join(name))?;
        Ok(())
    }

    async fn wait_cue(&self, cx: &TestCx, name: &str) -> Result<String> {
        let path = self.cues.join(name);
        let started = Instant::now();
        loop {
            if let Ok(text) = std::fs::read_to_string(&path) {
                return Ok(text);
            }
            ensure!(started.elapsed() < Duration::from_secs(60), "{}: timed out waiting for the cue {name}", self.role);
            cx.sleep(Duration::from_millis(50)).await;
        }
    }
}

fn own_uid(cx: &App) -> Uid {
    model(cx).auth.read(cx).uid
}

fn conversation_rows(peer: Uid, cx: &App) -> Vec<std::sync::Arc<Message>> {
    model(cx).dm.list.read(cx).slice(peer).map(|s| s.read(cx).rows().to_vec()).unwrap_or_default()
}

fn unread(peer: Uid, cx: &App) -> i64 {
    model(cx).dm.list.read(cx).get(peer).map_or(0, |c| c.unread)
}

fn has_echo(peer: Uid, text: &str, cx: &App) -> bool {
    conversation_rows(peer, cx).iter().any(|m| m.id > 0 && &*m.content == text)
}

async fn actor(cx: TestCx) -> Result<()> {
    let actor = Actor::from_env()?;
    wait_for(&cx, Duration::from_secs(30), "the app to boot", |cx| model(cx).auth.read(cx).booted).await?;
    // A conversation is read only in a focused window: these flows open one and expect it read.
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    if actor.script == "register" {
        let (name, email, password) = (actor.me.clone(), format!("{}@example.test", actor.me), actor.password.clone());
        cx.update(move |_, cx| actions::auth::register(name, email, password, cx))?;
        wait_for(&cx, Duration::from_secs(20), "the registration's answer", |cx| model(cx).auth.read(cx).register_result.is_some()).await?;
        let result = cx.update(|_, cx| model(cx).auth.read(cx).register_result.clone())?.expect("checked");
        ensure!(result.success, "registering {}: {}", actor.me, result.message);
        cx.log(format_args!("registered {}", actor.me));
        return Ok(());
    }
    let (name, password) = (actor.me.clone(), actor.password.clone());
    cx.update(move |_, cx| actions::auth::sign_in(name, password, cx))?;
    signed_in(&cx).await?;
    let me = cx.update(|_, cx| own_uid(cx))?;
    cx.log(format_args!("{} is {} (uid {me})", actor.role, actor.me));
    match actor.script.as_str() {
        "dm-send-receive" => send_receive(&cx, &actor, me).await?,
        "dm-read-sync" => read_sync(&cx, &actor, me).await?,
        "dm-e2ee" => dm_e2ee(&cx, &actor, me).await?,
        "friends-flow" => friends_flow(&cx, &actor, me).await?,
        "status-custom" => status_custom(&cx, &actor, me).await?,
        other => bail!("no script {other}"),
    }
    sign_out(&cx).await
}

async fn send_receive(cx: &TestCx, actor: &Actor, me: Uid) -> Result<()> {
    // The same texts on both sides: named by account A and B, whichever this one is.
    let (a_name, b_name) = actor.accounts();
    let first = format!("hello from {a_name} (quick DM)");
    let second = format!("and from the composer, {a_name}");
    let reply = format!("got it, {a_name}, from {b_name}");
    let later = format!("one more while you're away, {a_name}");
    match actor.role.as_str() {
        "a" => {
            actor.cue("a-ready", me)?;
            let peer: Uid = actor.wait_cue(cx, "b-ready").await?.parse()?;
            // The popout's quick-DM field: typed, Enter sends and opens the conversation.
            cx.update(move |_, cx| decibell_common::open_profile_at(peer, point(px(400.), px(160.)), None, cx))?;
            wait_for(cx, Duration::from_secs(5), "the popout", move |cx| model(cx).overlays.read(cx).profile_popup.as_ref().is_some_and(|p| p.uid == peer)).await?;
            quiet(cx).await?;
            cx.type_text(&first)?;
            cx.keystroke("enter")?;
            wait_for(cx, Duration::from_secs(5), "the conversation to open", move |cx| {
                let m = model(cx);
                m.nav.read(cx).active_view == ActiveView::Dm && m.dm.list.read(cx).active_peer == Some(peer)
            })
            .await?;
            let text = first.clone();
            wait_for(cx, Duration::from_secs(10), "the quick DM's echo", move |cx| has_echo(peer, &text, cx)).await?;
            cx.log("quick DM: sent from the popout, the conversation opened, central echoed it");
            // The composer under the conversation (the fallback field until the composer crate's).
            click_probe(cx, "dm-composer").await?;
            cx.type_text(&second)?;
            cx.keystroke("enter")?;
            let text = second.clone();
            wait_for(cx, Duration::from_secs(10), "the composer's echo", move |cx| has_echo(peer, &text, cx)).await?;
            cx.log("composer: typed, Enter sent it, central echoed it");
            actor.cue("a-sent", &first)?;
            actor.wait_cue(cx, "b-replied").await?;
            let text = reply.clone();
            wait_for(cx, Duration::from_secs(10), "the reply drawn in the list", move |cx| {
                let Some(id) = conversation_rows(peer, cx).iter().find(|m| *m.content == text).map(|m| m.id) else { return false };
                visible_rows(cx).iter().any(|(i, _)| *i == id)
            })
            .await?;
            cx.log("the reply arrived and is drawn in the open conversation");
            dm_composer(cx, peer, &reply, &a_name).await?;
            cx.update(|_, cx| actions::servers::go_home(cx))?;
            actor.cue("a-home", 1)?;
            actor.wait_cue(cx, "b-sent2").await?;
            wait_for(cx, Duration::from_secs(10), "an unread count while away", move |cx| unread(peer, cx) > 0).await?;
            let tile = wait_probe(cx, &format!("unread-dm:{peer}")).await?;
            cx.log(format_args!("away on Home: {} unread, the server bar's tile is drawn", cx.update(move |_, cx| unread(peer, cx))?));
            cx.click(tile.center())?;
            park_pointer(cx)?;
            wait_for(cx, Duration::from_secs(10), "opening it to clear the count", move |cx| {
                let m = model(cx);
                m.dm.list.read(cx).active_peer == Some(peer) && unread(peer, cx) == 0 && m.dm.list.read(cx).total_unread() == 0
            })
            .await?;
            cx.log("the tile opened the conversation; the count cleared (and the tile with it)");
            actor.cue("a-done", 1)?;
        }
        _ => {
            actor.cue("b-ready", me)?;
            let peer: Uid = actor.wait_cue(cx, "a-ready").await?.parse()?;
            cx.update(|_, cx| actions::servers::go_home(cx))?;
            let sent = actor.wait_cue(cx, "a-sent").await?;
            wait_for(cx, Duration::from_secs(10), "the DM, unread while elsewhere", move |cx| unread(peer, cx) > 0 && has_echo(peer, &sent, cx)).await?;
            let count = cx.update(move |_, cx| unread(peer, cx))?;
            cx.log(format_args!("received both messages while on Home: {count} unread"));
            ensure!(count >= 2, "{count} unread for two messages");
            let text = reply.clone();
            cx.update(move |_, cx| {
                actions::dm::send_dm(peer, &text, 0, cx);
            })?;
            let text = reply.clone();
            wait_for(cx, Duration::from_secs(10), "the reply's echo", move |cx| has_echo(peer, &text, cx)).await?;
            actor.cue("b-replied", &reply)?;
            actor.wait_cue(cx, "a-home").await?;
            let text = later.clone();
            cx.update(move |_, cx| {
                actions::dm::send_dm(peer, &text, 0, cx);
            })?;
            let text = later.clone();
            wait_for(cx, Duration::from_secs(10), "the second message's echo", move |cx| has_echo(peer, &text, cx)).await?;
            actor.cue("b-sent2", 1)?;
            actor.wait_cue(cx, "a-done").await?;
        }
    }
    Ok(())
}

/// The composer under the conversation (the composer crate's, joined by the app): a reply to
/// their message, ArrowUp in the empty box edits ours inline, a draft per conversation, the
/// inline send error above the card.
async fn dm_composer(cx: &TestCx, peer: Uid, their: &str, a_name: &str) -> Result<()> {
    let list = |cx: &App| crate::hooks::shown::<Conversation>(cx).map(|c| c.read(cx).list().clone());
    // A reply: the list's reply arrow (the composer follows `ListEvent::Reply`), typed, Enter.
    let parent = {
        let their = their.to_string();
        cx.update(move |_, cx| conversation_rows(peer, cx).iter().find(|m| *m.content == their).map(|m| m.id))?.context("their row")?
    };
    cx.update(move |_, cx| {
        if let Some(list) = list(cx) {
            list.update(cx, |_, cx| cx.emit(decibell_chat::ListEvent::Reply(parent)));
        }
    })?;
    quiet(cx).await?;
    let answer = format!("a reply to that, from {a_name}");
    cx.type_text(&answer)?;
    cx.keystroke("enter")?;
    {
        let answer = answer.clone();
        wait_for(cx, Duration::from_secs(10), "the reply's echo", move |cx| {
            conversation_rows(peer, cx).iter().any(|m| m.id > 0 && *m.content == answer && m.reply_to() == parent)
        })
        .await?;
    }
    cx.log("reply: the arrow put the bar up, Enter sent it answering their message");
    // ArrowUp in the empty box: our newest message in the inline editor; Enter saves.
    let own = cx.update(move |_, cx| conversation_rows(peer, cx).iter().rev().find(|m| m.id > 0 && m.sender_uid == own_uid(cx)).map(|m| m.id))?.context("our row")?;
    let frames_before_up = cx.frames()?;
    cx.keystroke("up")?;
    wait_for(cx, Duration::from_secs(5), "the inline editor", move |cx| list(cx).is_some_and(|l| l.read(cx).editing() == Some(own))).await?;
    quiet(cx).await?;
    let drawn_since_up = cx.frames()?.since(frames_before_up).drawn;
    let window_state = cx.update(move |window, cx| {
        format!(
            "{drawn_since_up} frames drawn since ArrowUp; window active {}, visible {}; a handle focused {}",
            window.is_window_active(),
            window.is_visible(),
            window.focused(cx).is_some()
        )
    })?;
    let edited = format!("a reply to that, edited by {a_name}");
    let typing_into = |cx: &TestCx| -> Result<String> {
        cx.update(move |window, cx| {
            format!(
                "a text field focused: {}; editing {:?}; the box's draft {:?}",
                decibell_text::input::text_field_focused(window),
                list(cx).and_then(|l| l.read(cx).editing()),
                model(cx).chat.drafts.read(cx).get(&decibell_model::chat::DraftKey::Dm(peer)).to_string()
            )
        })
    };
    let before = typing_into(cx)?;
    cx.keystroke("ctrl-a")?;
    cx.type_text(&edited)?;
    let typed = typing_into(cx)?;
    cx.keystroke("enter")?;
    {
        let edited = edited.clone();
        let echoed = wait_for(cx, Duration::from_secs(10), "the edit's echo", move |cx| {
            conversation_rows(peer, cx).iter().any(|m| m.id == own && *m.content == edited && m.edited_at > 0)
        })
        .await;
        if let Err(e) = echoed {
            bail!("{e:#} (before typing: {before}; after: {typed}; {window_state})");
        }
    }
    cx.log("ArrowUp: our last message edited inline, Enter saved it (edited)");
    // A draft per conversation: kept in the model, back in the box when the conversation is again.
    let key = decibell_model::chat::DraftKey::Dm(peer);
    let draft = |key: decibell_model::chat::DraftKey, cx: &App| model(cx).chat.drafts.read(cx).get(&key).to_string();
    click_probe(cx, "dm-composer").await?;
    cx.type_text("half a thought")?;
    let other: Uid = BOB_UID;
    cx.update(move |_, cx| actions::dm::open_dm(other, "bob".into(), cx))?;
    wait_for(cx, Duration::from_secs(5), "another conversation", move |cx| crate::hooks::shown::<Conversation>(cx).is_some_and(|c| c.read(cx).peer() == other)).await?;
    click_probe(cx, "dm-composer").await?;
    cx.type_text("something else")?;
    {
        let name = actor_peer_name(peer, cx)?;
        cx.update(move |_, cx| actions::dm::open_dm(peer, name.into(), cx))?;
    }
    wait_for(cx, Duration::from_secs(5), "back in the conversation", move |cx| crate::hooks::shown::<Conversation>(cx).is_some_and(|c| c.read(cx).peer() == peer)).await?;
    click_probe(cx, "dm-composer").await?;
    cx.keystroke("end")?;
    cx.type_text("!")?;
    let (here, there) = {
        let key = key.clone();
        cx.update(move |_, cx| (draft(key.clone(), cx), draft(decibell_model::chat::DraftKey::Dm(other), cx)))?
    };
    cx.log(format_args!("drafts per conversation: {here:?} here, {there:?} with bob"));
    ensure!(here == "half a thought!" && there == "something else", "drafts: {here:?} / {there:?}");
    cx.keystroke("ctrl-a")?;
    cx.keystroke("backspace")?;
    cx.update(move |_, cx| actions::dm::set_draft(other, Default::default(), cx))?;
    // The inline send error above the card (`sendError`); the next send clears it.
    cx.update(move |_, cx| model(cx).dm.list.update(cx, |l, cx| l.set_send_error(Some((peer, "Error: selftest".into())), cx)))?;
    wait_for(cx, Duration::from_secs(5), "the inline error", |_| probes::text_of("composer-send-error").is_some_and(|t| &*t == "Error: selftest")).await?;
    let after = format!("after the error, from {a_name}");
    cx.type_text(&after)?;
    cx.keystroke("enter")?;
    {
        let after = after.clone();
        wait_for(cx, Duration::from_secs(10), "the next send's echo", move |cx| has_echo(peer, &after, cx)).await?;
    }
    wait_for(cx, Duration::from_secs(5), "the error to clear", |_| probes::text_of("composer-send-error").is_some_and(|t| t.is_empty())).await?;
    cx.log("the inline send error showed above the card; the next send cleared it");
    Ok(())
}

/// The other side's name as this client knows it.
fn actor_peer_name(peer: Uid, cx: &TestCx) -> Result<String> {
    cx.update(move |_, cx| model(cx).dm.list.read(cx).get(peer).map(|c| c.username.to_string()).unwrap_or_default())
}

async fn read_sync(cx: &TestCx, actor: &Actor, me: Uid) -> Result<()> {
    let (a_name, _) = actor.accounts();
    let text = format!("read me on one device, {a_name}");
    match actor.role.as_str() {
        "b" => {
            actor.cue("b-ready", me)?;
            let peer: Uid = actor.wait_cue(cx, "a-ready").await?.parse()?;
            actor.wait_cue(cx, "a2-ready").await?;
            let t = text.clone();
            cx.update(move |_, cx| {
                actions::dm::send_dm(peer, &t, 0, cx);
            })?;
            let t = text.clone();
            wait_for(cx, Duration::from_secs(10), "the echo", move |cx| has_echo(peer, &t, cx)).await?;
            actor.cue("b-sent", 1)?;
            actor.wait_cue(cx, "a-synced").await?;
        }
        role => {
            let other = if role == "a" { "a2" } else { "a" };
            actor.cue(&format!("{role}-ready"), me)?;
            let peer: Uid = actor.wait_cue(cx, "b-ready").await?.parse()?;
            cx.update(|_, cx| actions::servers::go_home(cx))?;
            actor.wait_cue(cx, "b-sent").await?;
            wait_for(cx, Duration::from_secs(10), "the message, unread on this device", move |cx| unread(peer, cx) > 0).await?;
            actor.cue(&format!("{role}-unread"), 1)?;
            actor.wait_cue(cx, &format!("{other}-unread")).await?;
            if role == "a2" {
                // This device reads it (the conversation row, as a click would).
                click_probe(cx, &format!("dm-row:{peer}")).await?;
                wait_for(cx, Duration::from_secs(10), "reading it here", move |cx| unread(peer, cx) == 0).await?;
                cx.log("read on this device");
                actor.cue("a2-read", 1)?;
                actor.wait_cue(cx, "a-synced").await?;
            } else {
                actor.wait_cue(cx, "a2-read").await?;
                let t = wait_for(cx, Duration::from_secs(10), "the other device's read", move |cx| unread(peer, cx) == 0).await?;
                cx.log(format_args!("the other device's read reached this one {} ms later (DM_READ_SYNC)", t.as_millis()));
                actor.cue("a-synced", 1)?;
            }
        }
    }
    Ok(())
}

/// Set encryption up (a new account) or unlock it (keys already on central), as the dialog would.
async fn e2ee_ready(cx: &TestCx) -> Result<()> {
    wait_for(cx, Duration::from_secs(15), "the E2EE status", |cx| model(cx).live.e2ee.read(cx).status != E2eeStatus::Unavailable).await?;
    let status = cx.update(|_, cx| model(cx).live.e2ee.read(cx).status)?;
    let mode = match status {
        E2eeStatus::Ready => return Ok(()),
        E2eeStatus::NotSetUp => PassphraseMode::Setup,
        _ => PassphraseMode::Unlock,
    };
    cx.update(move |_, cx| {
        let e2ee = model(cx).live.e2ee.clone();
        e2ee.update(cx, |e, cx| e.open(mode, cx));
        actions::e2ee::submit_passphrase(PASSPHRASE.into(), PASSPHRASE.into(), cx);
    })?;
    let t = wait_for(cx, Duration::from_secs(30), "the keys", |cx| model(cx).live.e2ee.read(cx).status == E2eeStatus::Ready).await?;
    cx.log(format_args!("{mode:?}: ready after {} ms", t.as_millis()));
    // The passphrase dialog (the settings crate's overlay) takes every click while it is open and
    // for its 300 ms fade: a test that clicks next must not land on its backdrop.
    wait_for(cx, Duration::from_secs(10), "the passphrase dialog to close", |cx| model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    cx.sleep(Duration::from_millis(400)).await;
    Ok(())
}

fn peer_has_keys(peer: Uid, cx: &mut App) -> bool {
    let key = current_key(peer, cx);
    PeerInfos::existing(cx).is_some_and(|s| s.read(cx).get(peer, key).is_some_and(|i| i.has_keys))
}

async fn dm_e2ee(cx: &TestCx, actor: &Actor, me: Uid) -> Result<()> {
    let (_, b_name) = actor.accounts();
    let text = format!("sealed hello from {b_name}");
    match actor.role.as_str() {
        "a" => {
            actor.cue("a-ready", me)?;
            let peer: Uid = actor.wait_cue(cx, "b-ready").await?.parse()?;
            e2ee_ready(cx).await?;
            actor.cue("a-keys", 1)?;
            actor.wait_cue(cx, "b-keys").await?;
            { let name = actor.peer.clone(); cx.update(move |_, cx| actions::dm::open_dm(peer, name.into(), cx))?; }
            wait_for(cx, Duration::from_secs(10), "the peer's keys in the header", move |cx| peer_has_keys(peer, cx)).await?;
            let notice = cx.update(move |_, cx| {
                let e = model(cx).live.e2ee.read(cx);
                let key = current_key(peer, cx);
                let info = PeerInfos::existing(cx).and_then(|s| s.read(cx).get(peer, key).cloned());
                crate::dm::notice(e.status, 0, e.setup_nudge_dismissed, info.as_ref())
            })?;
            ensure!(notice.is_none(), "a notice shows over a sealed conversation: {notice:?}");
            cx.log("the chip reads Encrypted (ready, the peer has keys), no notice");
            actor.cue("a-open", 1)?;
            let sent = actor.wait_cue(cx, "b-sent").await?;
            let t = sent.clone();
            wait_for(cx, Duration::from_secs(15), "the sealed message", move |cx| has_echo(peer, &t, cx)).await?;
            let row = cx.update(move |_, cx| conversation_rows(peer, cx).into_iter().find(|m| *m.content == *sent))?.context("the row")?;
            ensure!(row.encrypted && row.decrypt_error.is_none(), "the message is not a decrypted sealed one ({:?})", row.decrypt_error);
            cx.log("the message arrived sealed and opened");
            actor.cue("a-peer", peer)?;
        }
        "b" => {
            actor.cue("b-ready", me)?;
            actor.wait_cue(cx, "a-ready").await?;
            e2ee_ready(cx).await?;
            actor.cue("b-keys", 1)?;
            actor.wait_cue(cx, "a-keys").await?;
            let peer: Uid = actor.wait_cue(cx, "a-ready").await?.parse()?;
            actor.wait_cue(cx, "a-open").await?;
            let t = text.clone();
            cx.update(move |_, cx| {
                actions::dm::send_dm(peer, &t, 0, cx);
            })?;
            let t = text.clone();
            wait_for(cx, Duration::from_secs(15), "the echo", move |cx| has_echo(peer, &t, cx)).await?;
            actor.cue("b-sent", &text)?;
            actor.wait_cue(cx, "a-peer").await?;
        }
        _ => {
            // A fresh profile of the first account: its keys are on central, not here.
            let peer: Uid = actor.wait_cue(cx, "a-peer").await?.parse()?;
            wait_for(cx, Duration::from_secs(15), "the E2EE status", |cx| model(cx).live.e2ee.read(cx).status != E2eeStatus::Unavailable).await?;
            // The first "locked" opens the passphrase prompt (Settings' dialog): dismissed.
            cx.update(|_, cx| model(cx).live.e2ee.update(cx, |e, cx| e.cancel(cx)))?;
            { let name = actor.peer.clone(); cx.update(move |_, cx| actions::dm::open_dm(peer, name.into(), cx))?; }
            let locked = cx.update(move |_, cx| {
                let e = model(cx).live.e2ee.read(cx);
                (e.status, crate::dm::notice(e.status, 0, e.setup_nudge_dismissed, None))
            })?;
            cx.log(format_args!("fresh profile: {:?}, notice {:?}", locked.0, locked.1));
            ensure!(locked.0 == E2eeStatus::Locked && locked.1 == Some(crate::dm::Notice::Locked), "a fresh device isn't locked");
            e2ee_ready(cx).await?;
            wait_for(cx, Duration::from_secs(15), "the sealed history to open", move |cx| {
                let rows = conversation_rows(peer, cx);
                !rows.is_empty() && rows.iter().all(|m| !m.encrypted || m.decrypt_error.is_none()) && peer_has_keys(peer, cx)
            })
            .await?;
            cx.log("unlocked: the notice went, the history opened");
        }
    }
    Ok(())
}

fn friend_status(uid: Uid, cx: &App) -> Option<FriendStatus> {
    model(cx).live.friends.read(cx).get(uid).map(|f| f.status)
}

async fn friends_flow(cx: &TestCx, actor: &Actor, me: Uid) -> Result<()> {
    match actor.role.as_str() {
        "a" => {
            actor.cue("a-ready", me)?;
            let peer: Uid = actor.wait_cue(cx, "b-ready").await?.parse()?;
            cx.update(|_, cx| actions::servers::go_home(cx))?;
            // The page's Add Friend form: the name typed, Enter sends it.
            click_probe(cx, "friends-add").await?;
            quiet(cx).await?;
            cx.type_text(&actor.peer)?;
            let typed = cx.update(|window, cx| {
                let page = crate::hooks::shown::<crate::friends::FriendsPage>(cx).map(|page| page.read(cx).add_state(window, cx));
                format!(
                    "after typing: {}; a text field focused {}, window active {}",
                    page.unwrap_or_else(|| "no Friends page".into()),
                    decibell_text::input::text_field_focused(window),
                    window.is_window_active()
                )
            })?;
            cx.keystroke("enter")?;
            let sent = wait_for(cx, Duration::from_secs(10), "the outgoing request", move |cx| friend_status(peer, cx) == Some(FriendStatus::PendingOutgoing)).await;
            if let Err(e) = sent {
                bail!("{e:#} ({typed})");
            }
            cx.log("request sent by name from the Add Friend form");
            actor.cue("a-requested", 1)?;
            actor.wait_cue(cx, "b-accepted").await?;
            wait_for(cx, Duration::from_secs(10), "the friendship", move |cx| friend_status(peer, cx).is_some_and(FriendStatus::is_accepted)).await?;
            cx.log("accepted on the other side: friends here");
            { let name = actor.peer.clone(); cx.update(move |_, cx| actions::friends::act(FriendAction::Remove, peer, name.into(), cx))?; }
            wait_for(cx, Duration::from_secs(10), "the removal", move |cx| friend_status(peer, cx).is_none()).await?;
            actor.cue("a-removed", 1)?;
            actor.wait_cue(cx, "b-saw-removed").await?;
            { let name = actor.peer.clone(); cx.update(move |_, cx| actions::friends::act(FriendAction::Block, peer, name.into(), cx))?; }
            wait_for(cx, Duration::from_secs(10), "the block", move |cx| friend_status(peer, cx) == Some(FriendStatus::Blocked)).await?;
            { let name = actor.peer.clone(); cx.update(move |_, cx| actions::friends::act(FriendAction::Remove, peer, name.into(), cx))?; }
            wait_for(cx, Duration::from_secs(10), "the unblock", move |cx| friend_status(peer, cx).is_none()).await?;
            cx.log("removed, blocked, unblocked: the list follows each");
            actor.cue("a-done", 1)?;
        }
        _ => {
            actor.cue("b-ready", me)?;
            let peer: Uid = actor.wait_cue(cx, "a-ready").await?.parse()?;
            cx.update(|_, cx| actions::servers::go_home(cx))?;
            actor.wait_cue(cx, "a-requested").await?;
            wait_for(cx, Duration::from_secs(10), "the incoming request", move |cx| friend_status(peer, cx) == Some(FriendStatus::PendingIncoming)).await?;
            // The row's Accept button on the Pending tab.
            click_probe(cx, "friends-filter-pending").await?;
            click_probe(cx, &format!("friend-accept:{peer}")).await?;
            wait_for(cx, Duration::from_secs(10), "the friendship", move |cx| friend_status(peer, cx).is_some_and(FriendStatus::is_accepted)).await?;
            cx.log("accepted from the Pending tab");
            actor.cue("b-accepted", 1)?;
            actor.wait_cue(cx, "a-removed").await?;
            wait_for(cx, Duration::from_secs(10), "the removal here too", move |cx| friend_status(peer, cx).is_none()).await?;
            actor.cue("b-saw-removed", 1)?;
            actor.wait_cue(cx, "a-done").await?;
        }
    }
    Ok(())
}

async fn status_custom(cx: &TestCx, actor: &Actor, me: Uid) -> Result<()> {
    let (a_name, _) = actor.accounts();
    let text = format!("selftest status of {a_name}");
    match actor.role.as_str() {
        "a" => {
            actor.cue("a-ready", me)?;
            actor.wait_cue(cx, "b-watching").await?;
            cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.open_modal(modal::CUSTOM_STATUS, cx)))?;
            wait_probe(cx, "status-save").await?;
            quiet(cx).await?;
            cx.type_text(&text)?;
            cx.update(|_, cx| {
                if let Some(modal) = crate::hooks::shown::<CustomStatusModal>(cx) {
                    modal.update(cx, |m, cx| m.set_clear_after(ClearAfter::ThirtyMinutes, cx));
                }
            })?;
            click_probe(cx, "status-save").await?;
            let t = text.clone();
            wait_for(cx, Duration::from_secs(5), "our message", move |cx| {
                let own = model(cx).live.presence.own.read(cx);
                own.message.as_ref().is_some_and(|m| *m.text == *t) && own.clears_at.is_some()
            })
            .await?;
            let left = cx.update(|_, cx| {
                let own = model(cx).live.presence.own.read(cx);
                own.clears_at.map(|at| at.saturating_duration_since(cx.background_executor().now()).as_secs())
            })?;
            cx.log(format_args!("saved from the dialog; it clears in {left:?} s"));
            ensure!(left.is_some_and(|s| (1700..=1800).contains(&s)), "clear-after 30 min");
            actor.cue("a-set", &text)?;
            actor.wait_cue(cx, "b-saw").await?;
            cx.update(|_, cx| actions::presence::save_status_message(None, 0, cx))?;
            actor.cue("a-cleared", 1)?;
            actor.wait_cue(cx, "b-saw-cleared").await?;
        }
        _ => {
            let peer: Uid = actor.wait_cue(cx, "a-ready").await?.parse()?;
            // Watch them, as a view showing them would.
            cx.update(move |_, cx| actions::presence::retain([peer], cx))?;
            cx.sleep(Duration::from_millis(1200)).await;
            actor.cue("b-watching", me)?;
            let set = actor.wait_cue(cx, "a-set").await?;
            let t = wait_for(cx, Duration::from_secs(10), "their message", move |cx| {
                model(cx).live.presence.messages.read(cx).get(peer).is_some_and(|m| *m.text == *set)
            })
            .await?;
            cx.log(format_args!("the watcher saw the message {} ms after it was saved", t.as_millis()));
            actor.cue("b-saw", 1)?;
            actor.wait_cue(cx, "a-cleared").await?;
            wait_for(cx, Duration::from_secs(10), "it to clear", move |cx| model(cx).live.presence.messages.read(cx).get(peer).is_none()).await?;
            cx.log("…and saw it cleared");
            actor.cue("b-saw-cleared", 1)?;
        }
    }
    Ok(())
}
