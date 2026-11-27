//! The settings windows' self-tests.
//!
//! **Against the local stack** (`tools/dev/README.md`): each starts the app again in a child
//! process, `decibell --check <check> --login alice` (or perf2) with its own temporary profile
//! (`DECIBELL_USER_DATA_DIR`), as the shell's tests do, and passes when the check inside it does.
//! They need a binary built with `dev-local`, refuse to run unless `DECIBELL_CENTRAL` is loopback,
//! and skip, saying so, when the stack is down. Other agents test against the same communities at
//! the same time, so a test changes only what it creates — a role and channels named `st-<pid>…`,
//! a throwaway account `stsettings` (registered by the first run, reused after) joined with the
//! seeded invite — and deletes them afterwards (a cleanup child runs whatever happened); the one
//! seeded value it touches, Daily Gaming's description, is changed and put back inside one test.
//! The tests that change a roster (the throwaway joins, is banned, is kicked) run in Design Lab as
//! its owner carol: other tests count Daily Gaming's members.
//!
//! **In this process** (fixture model, no network): the picture editor, the rail below 1024 px,
//! and idleness (no frame at rest on any tab; a closed window drops its state).

use std::cell::RefCell;
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_core::events::CoreEvent;
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_model::stores::community::{ChannelKind, Member};
use decibell_model::stores::overlays::modal;
use decibell_model::stores::permissions::perm;
use decibell_model::{ChannelId, Model, ServerId, actions};
use decibell_picture_editor::{EditorOptions, PictureEditor, PictureEditorEvent, PictureSource, geometry};
use decibell_shell::Shell;
use gpui::{
    AnyView, AnyWindowHandle, App, AppContext as _, Bounds, Context, Entity, IntoElement, ParentElement as _, Render,
    Styled as _, Subscription, TitlebarOptions, Window, WindowBounds, WindowDecorations, WindowOptions, div, px, size,
};

use crate::channel::{ChannelSettingsWindow, Confirm, Tab as ChannelTab};
use crate::channel::permissions::Bits;
use crate::host;
use crate::member_search::MemberSearch;
use crate::requests::{self, Request};
use crate::server::audit::{Category, actions_of};
use crate::server::members::ModKind;
use crate::server::roles::Selected;
use crate::server::{ServerSettingsWindow, Tab, TabId, visible_tabs};

const DAILY_GAMING: i32 = 1;
/// Where the roster-changing tests run (owner carol).
const DESIGN_LAB: i32 = 3;

/// The community a check works in: Daily Gaming, or what the parent chose
/// (`DECIBELL_ST_SERVER`).
fn daily_gaming() -> ServerId {
    let id = std::env::var("DECIBELL_ST_SERVER").ok().and_then(|s| s.parse().ok()).unwrap_or(DAILY_GAMING);
    ServerId::from_central(id)
}

/// `st-<pid>`: everything a test creates is named from it.
fn tag() -> String {
    std::env::var("DECIBELL_ST_TAG").unwrap_or_else(|_| format!("st-{}", std::process::id()))
}

#[path = "selftests_admin.rs"]
mod admin;

// ── The parents ─────────────────────────────────────────────────────────────────────────────

pub fn selftests() -> Vec<SelfTest> {
    let mut tests = vec![
        SelfTest {
            name: "server-settings-open",
            about: "(stack) alice opens every tab on real data; perf2 sees only the tabs its permissions allow",
            run: |cx| Box::pin(parent_open(cx)),
        },
        SelfTest {
            name: "overview-save",
            about: "(stack) Daily Gaming's description changed, saved, echoed; then put back and echoed",
            run: |cx| Box::pin(parent(cx, "overview-save", false)),
        },
        SelfTest {
            name: "roles-edit-echo",
            about: "(stack) a role st-<pid> created, edited and deleted from the Roles tab, each settling on the echo",
            run: |cx| Box::pin(parent(cx, "roles-edit-echo", true)),
        },
        SelfTest {
            name: "members-search",
            about: "(stack) the Members tab searches on the server: typed text and a role filter go out, results match",
            run: |cx| Box::pin(parent(cx, "members-search", false)),
        },
        SelfTest {
            name: "audit-log-live",
            about: "(stack) the audit log's search and action filter are sent with the request; results follow them",
            run: |cx| Box::pin(parent(cx, "audit-log-live", false)),
        },
        SelfTest {
            name: "bans-live",
            about: "(stack) a throwaway account joins, is banned from Members, listed in Bans, unbanned there",
            run: |cx| Box::pin(parent_bans(cx)),
        },
        SelfTest {
            name: "channel-settings-echo",
            about: "(stack) a channel st-<pid>: rename / topic / slowmode / retention echo, a deny overwrite refuses a second \
                    session's send, unsaved close shakes, wipe toast, delete",
            run: |cx| Box::pin(parent_channel(cx)),
        },
        SelfTest {
            name: "category-sync",
            about: "(stack) a category st-<pid>: following switched on and off, Sync all brings its channel in line",
            run: |cx| Box::pin(parent(cx, "category-sync", true)),
        },
        SelfTest {
            name: "picture-editor",
            about: "the picture editor: a PNG and a sideways (EXIF 6) JPEG load upright, zoom and pan stop at their limits, Save gives the output size",
            run: |cx| Box::pin(picture_editor(cx)),
        },
        SelfTest {
            name: "server-settings-narrow",
            about: "Server and Channel Settings below 1024 px fold the sidebar into the rail; above, the full sidebar",
            run: |cx| Box::pin(settings_narrow(cx)),
        },
        SelfTest {
            name: "members-nickname",
            about: "(local stack) Daily Gaming's Members tab: alice (owner) sets and clears her own nickname; not in Rustaceans, where @everyone lacks Change Nickname; carol (Manage Nicknames) a member's; a nickname above her refused",
            run: |cx| Box::pin(parent_nickname(cx)),
        },
        SelfTest {
            name: "server-settings-idle",
            about: "every Server Settings tab settles to 0 frames over 5 s; closing drops the window and its state",
            run: |cx| Box::pin(settings_idle(cx)),
        },
    ];
    tests.extend(admin::selftests());
    tests
}

/// The stack as `accounts.json` describes it.
struct Stack {
    central: String,
    password: String,
    /// The community's seeded invite and where it is.
    invite: String,
    host: String,
    port: u16,
    /// The community the children work in, and its owner (who signs in for them).
    server: i32,
    owner: String,
}

fn stack() -> Result<Option<Stack>> {
    stack_for(DAILY_GAMING)
}

/// The stack, with `server`'s invite.
fn stack_for(server: i32) -> Result<Option<Stack>> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let path = dev.join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json["central"].as_str().map(str::to_string)).context("no central address")?;
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the settings tests run against the local stack only");
    let password = json["password"].as_str().context("accounts.json has no password")?.to_string();
    let community = json["communities"]
        .as_array()
        .and_then(|c| c.iter().find(|c| c["server_id"].as_i64() == Some(server as i64)))
        .with_context(|| format!("accounts.json has no community {server}"))?;
    let server_id = server;
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then(|| Stack {
        central,
        password,
        invite: community["invite"].as_str().unwrap_or_default().to_string(),
        host: community["host"].as_str().unwrap_or_default().to_string(),
        port: community["port"].as_u64().unwrap_or(0) as u16,
        server: server_id,
        owner: community["owner"].as_str().unwrap_or("alice").to_string(),
    }))
}

/// A temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Result<TempDir> {
        let dir = std::env::temp_dir().join(format!("decibell-{name}-{}", std::process::id()));
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

enum Child {
    Passed,
    /// The binary has no `--login` (not built with `dev-local`).
    NoDevLogin,
}

/// Run `decibell --check <check>` (signed in as `login` when given) on a fresh profile and relay
/// its log.
async fn run_child(cx: &TestCx, check: &str, login: Option<&str>, stack: &Stack, env: &[(&str, String)]) -> Result<Child> {
    let profile = TempDir::new(&format!("{check}-{}", login.unwrap_or("guest")))?;
    let mut command = Command::new(std::env::current_exe()?);
    command.arg("--check").arg(check);
    if let Some(account) = login {
        command.arg("--login").arg(account);
    }
    command
        .env("DECIBELL_USER_DATA_DIR", &profile.0)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_ST_TAG", tag())
        .env("DECIBELL_ST_INVITE", &stack.invite)
        .env("DECIBELL_ST_HOST", &stack.host)
        .env("DECIBELL_ST_PORT", stack.port.to_string())
        .env("DECIBELL_ST_SERVER", stack.server.to_string())
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
    let (status, out, err) = cx
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
    let status = status?;
    for line in out.iter().filter(|l| !l.starts_with("selftest:")) {
        cx.log(format_args!("│ {line}"));
    }
    if err.iter().any(|l| l.contains("--login refused")) && err.iter().any(|l| l.contains("dev-local")) {
        return Ok(Child::NoDevLogin);
    }
    if !status.success() {
        // A panic: everything from its message on (the backtrace names the element; the last twelve
        // lines of one are GPUI's outermost frames and say nothing). Otherwise the tail.
        let from = match err.iter().position(|l| l.contains("panicked at")) {
            Some(at) => at,
            None => err.len().saturating_sub(12),
        };
        for line in err.iter().skip(from).take(400) {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{check} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(Child::Passed)
}

fn skip_down(cx: &TestCx) {
    cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
}

fn skip_no_login(cx: &TestCx) {
    cx.log("SKIP: this binary has no --login (build with --features dev-local)");
}

/// One check as alice; `cleanup` runs the cleanup child afterwards, whatever happened.
async fn parent(cx: TestCx, check: &'static str, cleanup: bool) -> Result<()> {
    let Some(stack) = stack()? else {
        skip_down(&cx);
        return Ok(());
    };
    let result = run_child(&cx, check, Some(stack.owner.as_str()), &stack, &[]).await;
    if cleanup && !matches!(result, Ok(Child::NoDevLogin)) {
        clean_up(&cx, &stack, None).await;
    }
    match result? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            skip_no_login(&cx);
            Ok(())
        }
    }
}

async fn clean_up(cx: &TestCx, stack: &Stack, throwaway: Option<&str>) {
    let env: Vec<(&str, String)> = throwaway.map(|u| vec![("DECIBELL_ST_USER", u.to_string())]).unwrap_or_default();
    if let Err(e) = run_child(cx, "settings-cleanup", Some(stack.owner.as_str()), stack, &env).await {
        cx.log(format_args!("cleanup failed: {e:#}"));
    }
}

async fn parent_open(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        skip_down(&cx);
        return Ok(());
    };
    if let Child::NoDevLogin = run_child(&cx, "server-settings-open", Some("alice"), &stack, &[]).await? {
        skip_no_login(&cx);
        return Ok(());
    }
    run_child(&cx, "server-settings-gating", Some("perf2"), &stack, &[]).await?;
    Ok(())
}

/// The throwaway account's name. One account, registered by the first run that needs it and
/// reused after (central limits sign-ups per address); every test takes it out of the community
/// again, and each child signs out, so central forgets its device.
fn throwaway() -> String {
    "stsettings".to_string()
}

/// Sign the throwaway in (registering it first if it doesn't exist yet) and join the community with
/// it (a child without `--login`).
async fn join_throwaway(cx: &TestCx, stack: &Stack, user: &str) -> Result<()> {
    run_child(cx, "settings-throwaway", None, stack, &[("DECIBELL_ST_USER", user.to_string()), ("DECIBELL_ST_PHASE", "join".into())]).await?;
    Ok(())
}

async fn parent_bans(cx: TestCx) -> Result<()> {
    let Some(stack) = stack_for(DESIGN_LAB)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let result = async {
        join_throwaway(&cx, &stack, &user).await?;
        run_child(&cx, "bans-live", Some(stack.owner.as_str()), &stack, &[("DECIBELL_ST_USER", user.clone())]).await
    }
    .await;
    if !matches!(result, Ok(Child::NoDevLogin)) {
        clean_up(&cx, &stack, Some(&user)).await;
    }
    match result? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            skip_no_login(&cx);
            Ok(())
        }
    }
}

/// T19 (parity audit A): nicknames by permission, through Server Settings → Members.
async fn parent_nickname(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        skip_down(&cx);
        return Ok(());
    };
    for (check, login) in [("members-nickname-own", "alice"), ("members-nickname-mod", "carol")] {
        if let Child::NoDevLogin = run_child(&cx, check, Some(login), &stack, &[]).await? {
            skip_no_login(&cx);
            return Ok(());
        }
    }
    Ok(())
}

/// A member's nickname as the roster holds it (`None`: not loaded).
fn roster_nickname(server: ServerId, uid: decibell_model::Uid, cx: &App) -> Option<String> {
    model(cx).community(server, cx).and_then(|c| c.roster.read(cx).member(uid).map(|m| m.nickname.to_string()))
}

/// `name` typed into the Members tab's search, as a person finds a member past the first page;
/// their record as the tab lists it.
async fn search_in_tab(cx: &TestCx, members: &Entity<crate::server::members::MembersTab>, name: &str) -> Result<Member> {
    cx.update(|w, cx| members.update(cx, |t, cx| t.focus_search(w, cx)))?;
    cx.sleep(Duration::from_millis(80)).await;
    cx.keystroke("ctrl-a")?;
    cx.type_text(name)?;
    let search = cx.update(|_, cx| members.read(cx).search().clone())?;
    let wanted = name.to_string();
    wait_for(cx, Duration::from_secs(10), &format!("the tab's search for {name}"), move |cx| {
        let s = search.read(cx);
        s.query() == wanted && !s.settling() && s.loaded && !s.loading && s.members.iter().any(|m| m.username == wanted.as_str())
    })
    .await?;
    let name = name.to_string();
    cx.update(move |_, cx| members.read(cx).search().read(cx).members.iter().find(|m| m.username == name.as_str()).cloned())?.context("vanished")
}

/// Edit `member`'s nickname in the Members tab as its row's "Set nickname" does: the field
/// opens on the current one, `nickname` replaces it, Enter saves; the field closes on the echo.
async fn set_nickname_in_tab(cx: &TestCx, members: &Entity<crate::server::members::MembersTab>, member: &Member, nickname: &str) -> Result<Duration> {
    let m = member.clone();
    cx.update(move |w, cx| members.update(cx, |t, cx| t.open_nick(&m, w, cx)))?;
    cx.sleep(Duration::from_millis(150)).await;
    cx.keystroke("ctrl-a")?;
    cx.keystroke("backspace")?;
    if !nickname.is_empty() {
        cx.type_text(nickname)?;
    }
    cx.keystroke("enter")?;
    let (server, uid, want) = (daily_gaming(), member.uid, nickname.to_string());
    // The tab's rows (the server's search, patched by the roster's deltas), else the roster.
    let shown = move |members: &Entity<crate::server::members::MembersTab>, cx: &App| {
        let listed = members.read(cx).search().read(cx).members.iter().find(|m| m.uid == uid).map(|m| m.nickname.to_string());
        listed.or_else(|| roster_nickname(server, uid, cx))
    };
    let tab = members.clone();
    let result = wait_for(cx, Duration::from_secs(10), "the nickname's echo", move |cx| {
        shown(&tab, cx).as_deref() == Some(want.as_str()) && tab.read(cx).nickname_editor().0.is_none()
    })
    .await;
    if result.is_err() {
        let state = cx.update(|_, cx| {
            let toasts: Vec<String> = model(cx).toasts.read(cx).toasts().iter().map(|t| format!("{}: {}", t.title, t.body.as_deref().unwrap_or_default())).collect();
            format!("shown {:?}, editor {:?}, toasts {toasts:?}", shown(members, cx), members.read(cx).nickname_editor())
        })?;
        cx.log(format_args!("no echo for {nickname:?}: {state}"));
    }
    result
}

async fn check_nickname_own(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let (_, tab) = open_settings(&cx, TabId::Members).await?;
    let Tab::Members(members) = tab else { bail!("not the Members tab") };
    let me = search_in_tab(&cx, &members, "alice").await?;
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    let gate = cx.update(|_, cx| crate::server::members::Gates::read(daily_gaming(), cx).acts(&me, now).nick)?;
    ensure!(gate, "alice's own row offers no nickname in Daily Gaming");
    let original = me.nickname.to_string();
    let nickname = format!("alice {}", tag());
    let t = set_nickname_in_tab(&cx, &members, &me, &nickname).await?;
    cx.log(format_args!("alice set her own nickname {nickname:?} from her row: on the record after {} ms, the field closed", t.as_millis()));
    set_nickname_in_tab(&cx, &members, &me, &original).await?;
    cx.log(format_args!("and put it back to {original:?}"));
    // Rustaceans: @everyone has no Change Nickname, and alice holds nothing that grants it.
    let rust = ServerId::from_central(2);
    wait_for(&cx, Duration::from_secs(30), "Rustaceans' roster", move |cx| model(cx).community(rust, cx).is_some_and(|c| c.roster.read(cx).member(1).is_some())).await?;
    let (gate, has) = cx.update(move |_, cx| {
        let me = model(cx).community(rust, cx).and_then(|c| c.roster.read(cx).member(1).cloned()).expect("own row");
        (crate::server::members::Gates::read(rust, cx).acts(&me, now).nick, model(cx).has_permission(rust, perm::CHANGE_NICKNAME, cx))
    })?;
    cx.log(format_args!("Rustaceans: Change Nickname {has}, her own row's nickname entry {gate}"));
    ensure!(!has && !gate, "a nickname entry where @everyone lacks Change Nickname");
    Ok(())
}

async fn check_nickname_mod(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let (_, tab) = open_settings(&cx, TabId::Members).await?;
    let Tab::Members(members) = tab else { bail!("not the Members tab") };
    let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as i64);
    // A Member-role placeholder (not the one user-menu-moderation edits), found in the tab.
    let target = search_in_tab(&cx, &members, "emreaydn").await?;
    let gate = cx.update(|_, cx| crate::server::members::Gates::read(daily_gaming(), cx).acts(&target, now).nick)?;
    ensure!(gate, "carol (Moderator: Manage Nicknames) gets no nickname entry on a Member");
    let original = target.nickname.to_string();
    let nickname = format!("by carol {}", tag());
    let t = set_nickname_in_tab(&cx, &members, &target, &nickname).await?;
    cx.log(format_args!("carol set {:?}'s nickname {nickname:?}: on the record after {} ms", target.username, t.as_millis()));
    set_nickname_in_tab(&cx, &members, &target, &original).await?;
    // bob is an Admin, above her: the row offers nothing; asked anyway, the server refuses.
    let bob = search_in_tab(&cx, &members, "bob").await?;
    let gate = cx.update(|_, cx| crate::server::members::Gates::read(daily_gaming(), cx).acts(&bob, now).nick)?;
    ensure!(!gate, "carol is offered bob's nickname");
    let before = bob.nickname.to_string();
    let toasts_before = cx.update(|_, cx| model(cx).toasts.read(cx).toasts().len())?;
    let (b, tab) = (bob.clone(), members.clone());
    cx.update(move |w, cx| tab.update(cx, |t, cx| t.open_nick(&b, w, cx)))?;
    cx.sleep(Duration::from_millis(150)).await;
    cx.keystroke("ctrl-a")?;
    cx.type_text("not allowed")?;
    cx.keystroke("enter")?;
    let m = members.clone();
    wait_for(&cx, Duration::from_secs(10), "the refusal", move |cx| model(cx).toasts.read(cx).toasts().len() > toasts_before && !m.read(cx).nickname_editor().1).await?;
    let (toast, editing, after) = cx.update(|_, cx| {
        let t = model(cx).toasts.read(cx).toasts().last().map(|t| format!("{}: {}", t.title, t.body.as_deref().unwrap_or_default()));
        (t, members.read(cx).nickname_editor().0, roster_nickname(daily_gaming(), 2, cx))
    })?;
    cx.log(format_args!("bob's nickname asked for anyway: refused ({toast:?}); the field still open: {}; his nickname {after:?}", editing == Some(2)));
    ensure!(editing == Some(2), "the field closed on a refusal (the draft is kept)");
    ensure!(after.as_deref().is_none_or(|n| n == before), "bob's nickname changed to {after:?}");
    cx.keystroke("escape")?;
    Ok(())
}

async fn parent_channel(cx: TestCx) -> Result<()> {
    let Some(stack) = stack_for(DESIGN_LAB)? else {
        skip_down(&cx);
        return Ok(());
    };
    let user = throwaway();
    let state = TempDir::new("settings-channel-state")?;
    let file = state.0.join("state.json");
    let env = |phase: &str| {
        vec![
            ("DECIBELL_ST_USER", user.clone()),
            ("DECIBELL_ST_PHASE", phase.to_string()),
            ("DECIBELL_ST_STATE", file.display().to_string()),
        ]
    };
    let result = async {
        join_throwaway(&cx, &stack, &user).await?;
        if let Child::NoDevLogin = run_child(&cx, "channel-settings-echo", Some(stack.owner.as_str()), &stack, &env("setup")).await? {
            return Ok(Child::NoDevLogin);
        }
        run_child(&cx, "settings-throwaway", None, &stack, &env("send")).await?;
        run_child(&cx, "channel-settings-echo", Some(stack.owner.as_str()), &stack, &env("finish")).await
    }
    .await;
    if !matches!(result, Ok(Child::NoDevLogin)) {
        clean_up(&cx, &stack, Some(&user)).await;
    }
    match result? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            skip_no_login(&cx);
            Ok(())
        }
    }
}

// ── The checks, inside the signed-in app ────────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    let mut checks = vec![
        SelfTest { name: "server-settings-open", about: "(in the app) every tab on real data", run: |cx| Box::pin(check_open(cx)) },
        SelfTest { name: "server-settings-gating", about: "(in the app) the tabs follow the permissions", run: |cx| Box::pin(check_gating(cx)) },
        SelfTest { name: "overview-save", about: "(in the app) the description round trip", run: |cx| Box::pin(check_overview_save(cx)) },
        SelfTest { name: "roles-edit-echo", about: "(in the app) a role created, edited, deleted", run: |cx| Box::pin(check_roles(cx)) },
        SelfTest { name: "members-search", about: "(in the app) member search", run: |cx| Box::pin(check_members_search(cx)) },
        SelfTest { name: "audit-log-live", about: "(in the app) audit log filters", run: |cx| Box::pin(check_audit(cx)) },
        SelfTest { name: "bans-live", about: "(in the app) ban and unban the throwaway", run: |cx| Box::pin(check_bans(cx)) },
        SelfTest { name: "channel-settings-echo", about: "(in the app) the channel settings round trips", run: |cx| Box::pin(check_channel(cx)) },
        SelfTest { name: "category-sync", about: "(in the app) category following", run: |cx| Box::pin(check_category(cx)) },
        SelfTest { name: "settings-throwaway", about: "(in the app) the throwaway account: join, or send", run: |cx| Box::pin(check_throwaway(cx)) },
        SelfTest { name: "settings-cleanup", about: "(in the app) delete what the settings tests created", run: |cx| Box::pin(check_cleanup(cx)) },
        SelfTest { name: "members-nickname-own", about: "(in the app) alice's own nickname", run: |cx| Box::pin(check_nickname_own(cx)) },
        SelfTest { name: "members-nickname-mod", about: "(in the app) carol's Manage Nicknames", run: |cx| Box::pin(check_nickname_mod(cx)) },
    ];
    checks.extend(admin::checks());
    checks
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

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

async fn signed_in(cx: &TestCx) -> Result<()> {
    let t = wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.log(format_args!("signed in after {} ms", t.as_millis()));
    wait_for(cx, Duration::from_secs(30), "the community to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    // The community's first pushes (channels, roles, our own member row).
    wait_for(cx, Duration::from_secs(15), "the community's channels and roles", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| !c.channels.read(cx).list().is_empty() && !c.roles.read(cx).list().is_empty())
    })
    .await?;
    Ok(())
}

/// Every event the core answered with, by name, while held.
struct Answers {
    seen: Rc<RefCell<Vec<CoreEvent>>>,
    _sub: Subscription,
}

impl Answers {
    fn record(cx: &TestCx) -> Result<Answers> {
        let seen: Rc<RefCell<Vec<CoreEvent>>> = Rc::default();
        let sink = seen.clone();
        let sub = cx.update(|_, cx| {
            let answers = model(cx).answers.clone();
            cx.subscribe(&answers, move |_, event: &CoreEvent, _| sink.borrow_mut().push(event.clone()))
        })?;
        Ok(Answers { seen, _sub: sub })
    }

    fn any(&self, f: impl Fn(&CoreEvent) -> bool) -> bool {
        self.seen.borrow().iter().any(f)
    }
}

/// Every request the windows send, recorded on the way out (they still go to the core).
struct Sent(Rc<RefCell<Vec<Request>>>);

impl Sent {
    fn record(cx: &TestCx) -> Result<Sent> {
        let log: Rc<RefCell<Vec<Request>>> = Rc::default();
        let sink = log.clone();
        cx.update(|_, cx| {
            requests::install(
                Rc::new(move |request, _| {
                    sink.borrow_mut().push(request.clone());
                    None
                }),
                cx,
            )
        })?;
        Ok(Sent(log))
    }

    fn find(&self, f: impl Fn(&Request) -> bool) -> Option<Request> {
        self.0.borrow().iter().rev().find(|r| f(r)).cloned()
    }
}

/// Send one request (and fail if it couldn't be sent).
async fn send(cx: &TestCx, request: Request) -> Result<()> {
    let result: Rc<RefCell<Option<requests::Sent>>> = Rc::default();
    let sink = result.clone();
    cx.update(|_, cx| requests::send(request, cx, move |r, _| *sink.borrow_mut() = Some(r)))?;
    let started = Instant::now();
    loop {
        if let Some(r) = result.borrow_mut().take() {
            return r.map_err(|e| anyhow::anyhow!("{e}"));
        }
        ensure!(started.elapsed() < Duration::from_secs(10), "the request was never sent");
        cx.sleep(Duration::from_millis(20)).await;
    }
}

/// The community open and its Server Settings with it, on `tab`.
async fn open_settings(cx: &TestCx, tab: TabId) -> Result<(Entity<ServerSettingsWindow>, Tab)> {
    cx.update(|_, cx| {
        let m = model(cx);
        actions::servers::open_server(daily_gaming(), cx);
        if !m.overlays.read(cx).is_open(modal::SERVER_SETTINGS) {
            m.overlays.update(cx, |o, cx| o.open_modal(modal::SERVER_SETTINGS, cx));
        }
    })?;
    wait_for(cx, Duration::from_secs(10), "Server Settings to open", |cx| host::server_window(cx).is_some()).await?;
    let window = cx.update(|_, cx| host::server_window(cx))?.context("the window went away")?;
    show(cx, &window, tab).await
}

async fn show(cx: &TestCx, window: &Entity<ServerSettingsWindow>, tab: TabId) -> Result<(Entity<ServerSettingsWindow>, Tab)> {
    cx.update(|w, cx| window.update(cx, |s, cx| s.show(tab, w, cx)))?;
    let open = cx.update(|_, cx| window.read(cx).tab().clone())?;
    ensure!(open.id() == tab, "{tab:?} didn't open (still on {:?})", open.id());
    Ok((window.clone(), open))
}

fn community_meta(cx: &App) -> (String, String) {
    model(cx)
        .community(daily_gaming(), cx)
        .map(|c| {
            let m = c.meta.read(cx);
            (m.name.to_string(), m.description.to_string())
        })
        .unwrap_or_default()
}

/// A member found by the server's search (the roster pages only what it shows).
async fn find_member(cx: &TestCx, name: &str) -> Result<Member> {
    let search = cx.update(|_, cx| cx.new(|cx| MemberSearch::new(daily_gaming(), name, 0, cx)))?;
    let name_owned = name.to_string();
    wait_for(cx, Duration::from_secs(10), &format!("the search for {name}"), |cx| {
        let s = search.read(cx);
        s.loaded && !s.loading && s.members.iter().any(|m| m.username == name_owned.as_str())
    })
    .await?;
    cx.update(|_, cx| search.read(cx).members.iter().find(|m| m.username == name).cloned())?.context("vanished")
}

async fn check_open(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    // The ban list lands in the community's moderation store: count its updates.
    let bans_updated: Rc<RefCell<u32>> = Rc::default();
    let sink = bans_updated.clone();
    let _bans = cx.update(|_, cx| {
        let moderation = model(cx).community(daily_gaming(), cx).map(|c| c.moderation.clone());
        moderation.map(|m| cx.observe(&m, move |_, _| *sink.borrow_mut() += 1))
    })?;
    let (window, _) = open_settings(&cx, TabId::Overview).await?;
    let tabs = cx.update(|_, cx| visible_tabs(daily_gaming(), cx))?;
    cx.log(format_args!("tabs: {:?}", tabs.iter().map(|t| t.label()).collect::<Vec<_>>()));
    ensure!(tabs.len() == 6, "the owner sees every tab, got {tabs:?}");
    let (name, _) = cx.update(|_, cx| community_meta(cx))?;
    ensure!(name == "Daily Gaming", "the Overview shows {name}");

    let (_, tab) = show(&cx, &window, TabId::Members).await?;
    let Tab::Members(members) = tab else { bail!("not the Members tab") };
    let search = cx.update(|_, cx| members.read(cx).search().clone())?;
    wait_for(&cx, Duration::from_secs(10), "the member search", |cx| search.read(cx).loaded && !search.read(cx).loading).await?;
    let (total, shown) = cx.update(|_, cx| (search.read(cx).total, search.read(cx).members.len()))?;
    cx.log(format_args!("Members: {total} members, {shown} on the first page"));
    ensure!(total >= 290 && shown == 100, "expected Daily Gaming's ~298 members, 100 per page: {total} / {shown}");

    let (_, tab) = show(&cx, &window, TabId::Roles).await?;
    let Tab::Roles(roles) = tab else { bail!("not the Roles tab") };
    let selected = cx.update(|_, cx| roles.read(cx).selected())?;
    let count = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).map_or(0, |c| c.roles.read(cx).list().len()))?;
    cx.log(format_args!("Roles: {count} roles, {selected:?} selected"));
    ensure!(count >= 2 && selected.is_some(), "the roles didn't load");

    let (_, tab) = show(&cx, &window, TabId::Audit).await?;
    let Tab::Audit(audit) = tab else { bail!("not the Audit Log tab") };
    wait_for(&cx, Duration::from_secs(10), "the audit log", |cx| audit.read(cx).entries().1).await?;
    let n = cx.update(|_, cx| audit.read(cx).entries().0.len())?;
    cx.log(format_args!("Audit Log: {n} entries on the first page"));

    show(&cx, &window, TabId::Bans).await?;
    wait_for(&cx, Duration::from_secs(10), "the ban list", |_| *bans_updated.borrow() > 0).await?;
    let bans = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).map_or(0, |c| c.moderation.read(cx).bans.len()))?;
    cx.log(format_args!("Bans: {bans}"));

    let (_, tab) = show(&cx, &window, TabId::Storage).await?;
    let Tab::Storage(storage) = tab else { bail!("not the Storage tab") };
    wait_for(&cx, Duration::from_secs(10), "the storage answer", |cx| storage.read(cx).info().is_some()).await?;
    let (volume, attachments) = cx.update(|_, cx| storage.read(cx).info().map(|i| (i.volume_total_bytes, i.attachment_count)).unwrap_or_default())?;
    cx.log(format_args!("Storage: volume {volume} bytes, {attachments} attachments"));
    ensure!(volume > 0, "no volume size");

    // Closing drops the window.
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    let weak = window.downgrade();
    drop(window);
    wait_for(&cx, Duration::from_secs(3), "the window to be dropped", |cx| host::server_window(cx).is_none()).await?;
    cx.sleep(Duration::from_millis(100)).await;
    ensure!(weak.upgrade().is_none(), "the closed window is still alive");
    Ok(())
}

/// What the permissions allow, worked out here from the roles (not by `visible_tabs`).
fn expected_tabs(cx: &App) -> Vec<TabId> {
    let m = model(cx);
    let Some(c) = m.community(daily_gaming(), cx) else { return Vec::new() };
    let me = m.auth.read(cx).uid;
    let owner = c.meta.read(cx).owner == me;
    let roles = c.roles.read(cx).list().to_vec();
    let mine: Vec<i64> = c.roster.read(cx).members().get(&me).map(|r| r.role_ids.to_vec()).unwrap_or_default();
    let mut bits = roles.iter().filter(|r| r.is_default || mine.contains(&r.id)).fold(0u64, |b, r| b | r.permissions);
    if owner || bits & perm::ADMINISTRATOR != 0 {
        bits = perm::ALL;
    }
    let mut tabs = vec![TabId::Overview, TabId::Members];
    for (bit, tab) in [(perm::MANAGE_ROLES, TabId::Roles), (perm::VIEW_AUDIT_LOG, TabId::Audit), (perm::BAN_MEMBERS, TabId::Bans), (perm::MANAGE_SERVER, TabId::Storage)] {
        if bits & bit != 0 {
            tabs.push(tab);
        }
    }
    tabs
}

async fn check_gating(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    wait_for(&cx, Duration::from_secs(15), "our own member row", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.roster.read(cx).members().contains_key(&me))
    })
    .await?;
    let (window, _) = open_settings(&cx, TabId::Overview).await?;
    let (shown, expected) = cx.update(|_, cx| (visible_tabs(daily_gaming(), cx), expected_tabs(cx)))?;
    cx.log(format_args!("perf2 sees {:?}", shown.iter().map(|t| t.label()).collect::<Vec<_>>()));
    ensure!(shown == expected, "tabs {shown:?}, but the permissions allow {expected:?}");
    ensure!(!shown.contains(&TabId::Storage), "a member without Manage Server sees Storage");
    // A tab it can't see doesn't open (nor ask the server).
    let sent = Sent::record(&cx)?;
    cx.update(|w, cx| window.update(cx, |s, cx| s.show(TabId::Storage, w, cx)))?;
    let open = cx.update(|_, cx| window.read(cx).tab().id())?;
    ensure!(open == TabId::Overview, "Storage opened for perf2");
    ensure!(sent.find(|r| matches!(r, Request::StorageInfo { .. })).is_none(), "a hidden tab asked for storage info");
    cx.update(|_, cx| requests::uninstall(cx))?;
    Ok(())
}

async fn check_overview_save(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let (_, tab) = open_settings(&cx, TabId::Overview).await?;
    let Tab::Overview(overview) = tab else { bail!("not the Overview tab") };
    let (_, original) = cx.update(|_, cx| community_meta(cx))?;
    let changed = format!("{} · {}", original.trim(), tag()).trim_start_matches(" · ").to_string();
    let result = async {
        cx.update(|_, cx| overview.update(cx, |t, cx| t.type_description(&changed, cx)))?;
        ensure!(cx.update(|_, cx| overview.read(cx).meta_dirty(cx))?, "typing didn't make a draft");
        cx.update(|_, cx| overview.update(cx, |t, cx| t.save_meta(cx)))?;
        ensure!(cx.update(|_, cx| overview.read(cx).saving_meta())?, "Save didn't wait for the echo");
        let t = wait_for(&cx, Duration::from_secs(8), "the description's echo", |cx| {
            !overview.read(cx).saving_meta() && community_meta(cx).1 == changed && !overview.read(cx).meta_dirty(cx)
        })
        .await?;
        cx.log(format_args!("saved and echoed in {} ms", t.as_millis()));
        Ok(())
    }
    .await;
    // Put it back, whatever happened.
    cx.update(|_, cx| overview.update(cx, |t, cx| t.type_description(&original, cx)))?;
    let restore = async {
        wait_for(&cx, Duration::from_secs(8), "the save to settle", |cx| !overview.read(cx).saving_meta()).await?;
        cx.update(|_, cx| overview.update(cx, |t, cx| t.save_meta(cx)))?;
        wait_for(&cx, Duration::from_secs(8), "the original description's echo", |cx| {
            !overview.read(cx).saving_meta() && community_meta(cx).1 == original.trim()
        })
        .await
    }
    .await;
    result?;
    restore.context("putting the description back")?;
    cx.log("the original description is back");
    Ok(())
}

async fn check_roles(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let name = tag();
    let (window, tab) = open_settings(&cx, TabId::Roles).await?;
    let Tab::Roles(roles) = tab else { bail!("not the Roles tab") };
    let find = |cx: &App, name: &str| model(cx).community(daily_gaming(), cx).and_then(|c| c.roles.read(cx).list().iter().find(|r| r.name == name).cloned());

    // Create.
    cx.update(|w, cx| roles.update(cx, |t, cx| t.start_create(w, cx)))?;
    ensure!(cx.update(|_, cx| roles.read(cx).selected())? == Some(Selected::New), "no draft role");
    cx.update(|_, cx| {
        roles.update(cx, |t, cx| {
            t.set_name_field(&name, cx);
            t.change(cx, |f| {
                f.name = name.clone();
                f.color = 0x3498db;
            });
            t.save(cx);
        })
    })?;
    let t = wait_for(&cx, Duration::from_secs(8), "the new role's echo", |cx| {
        find(cx, &name).is_some_and(|r| roles.read(cx).selected() == Some(Selected::Role(r.id))) && !roles.read(cx).is_dirty(cx)
    })
    .await?;
    let role = cx.update(|_, cx| find(cx, &name))?.context("the role vanished")?;
    cx.log(format_args!("created {name} (id {}) in {} ms", role.id, t.as_millis()));

    // Edit: colour and a permission, one save.
    cx.update(|_, cx| {
        roles.update(cx, |t, cx| {
            t.change(cx, |f| {
                f.color = 0xe91e63;
                f.permissions |= perm::EMBED_LINKS;
            });
            t.save(cx);
        })
    })?;
    let t = wait_for(&cx, Duration::from_secs(8), "the edit's echo", |cx| {
        find(cx, &name).is_some_and(|r| r.color == 0xe91e63 && r.permissions & perm::EMBED_LINKS != 0) && !roles.read(cx).is_dirty(cx)
    })
    .await?;
    cx.log(format_args!("colour and permission echoed in {} ms", t.as_millis()));

    // Unsaved edits hold the selection: switching shakes the bar.
    let other = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).and_then(|c| c.roles.read(cx).list().iter().find(|r| r.is_default).map(|r| r.id)))?;
    let nudges = cx.update(|_, cx| roles.read(cx).nudges())?;
    cx.update(|_, cx| roles.update(cx, |t, cx| t.change(cx, |f| f.name = format!("{name}-x"))))?;
    if let Some(other) = other {
        cx.update(|_, cx| roles.update(cx, |t, cx| t.select(Selected::Role(other), cx)))?;
        let (after, selected) = cx.update(|_, cx| (roles.read(cx).nudges(), roles.read(cx).selected()))?;
        ensure!(after == nudges + 1 && selected == Some(Selected::Role(role.id)), "switching away from unsaved edits didn't shake ({nudges} → {after}, {selected:?})");
    }
    cx.update(|_, cx| roles.update(cx, |t, cx| t.change(cx, |f| f.name = name.clone())))?;
    ensure!(!cx.update(|_, cx| roles.read(cx).is_dirty(cx))?, "still dirty after undoing the edit");

    // Delete.
    let current = cx.update(|_, cx| find(cx, &name))?.context("the role vanished")?;
    cx.update(|_, cx| roles.update(cx, |t, cx| t.run_delete(current, cx)))?;
    wait_for(&cx, Duration::from_secs(8), "the role to be deleted", |cx| find(cx, &name).is_none()).await?;
    cx.log(format_args!("deleted {name}"));
    drop(window);
    Ok(())
}

async fn check_members_search(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let sent = Sent::record(&cx)?;
    let (_, tab) = open_settings(&cx, TabId::Members).await?;
    let Tab::Members(members) = tab else { bail!("not the Members tab") };
    let search = cx.update(|_, cx| members.read(cx).search().clone())?;
    wait_for(&cx, Duration::from_secs(10), "the first page", |cx| search.read(cx).loaded).await?;
    // Typed into the box, as a person would.
    cx.update(|w, cx| members.update(cx, |t, cx| t.focus_search(w, cx)))?;
    cx.sleep(Duration::from_millis(50)).await;
    cx.type_text("perf")?;
    let t = wait_for(&cx, Duration::from_secs(10), "the search for \"perf\"", |cx| {
        let s = search.read(cx);
        s.query() == "perf" && !s.settling() && s.loaded && !s.loading
    })
    .await?;
    let request = sent.find(|r| matches!(r, Request::SearchMembers { query, .. } if query == "perf")).context("no MEMBER_SEARCH_REQ carried \"perf\"")?;
    cx.log(format_args!("sent {request:?}; answered in {} ms", t.as_millis()));
    let (names, total) = cx.update(|_, cx| (search.read(cx).members.iter().map(|m| m.username.to_string()).collect::<Vec<_>>(), search.read(cx).total))?;
    cx.log(format_args!("{total} match: {names:?}"));
    ensure!(names.iter().all(|n| n.contains("perf")), "a result doesn't contain \"perf\": {names:?}");
    ensure!(total >= 5 && names.len() as i64 == total.min(100), "expected the five perf accounts: {total}");

    // A role filter on top: every result holds it.
    let role = cx.update(|_, cx| {
        let c = model(cx).community(daily_gaming(), cx)?;
        let held: Vec<i64> = search.read(cx).members.iter().flat_map(|m| m.role_ids.iter().copied()).collect();
        c.roles.read(cx).list().iter().find(|r| !r.is_default && held.contains(&r.id)).map(|r| (r.id, r.name.to_string()))
    })?;
    if let Some((id, role_name)) = role {
        cx.update(|_, cx| members.update(cx, |t, cx| t.set_role_filter(id, cx)))?;
        wait_for(&cx, Duration::from_secs(10), "the role filter's answer", |cx| {
            let s = search.read(cx);
            s.role() == id && s.loaded && !s.loading
        })
        .await?;
        ensure!(sent.find(|r| matches!(r, Request::SearchMembers { query, role_id, .. } if query == "perf" && *role_id == id)).is_some(), "the role wasn't sent");
        let all_hold = cx.update(|_, cx| search.read(cx).members.iter().all(|m| m.role_ids.contains(&id)))?;
        let n = cx.update(|_, cx| search.read(cx).members.len())?;
        cx.log(format_args!("\"perf\" holding {role_name}: {n}"));
        ensure!(all_hold && n > 0, "a result doesn't hold {role_name}");
    }
    cx.update(|_, cx| requests::uninstall(cx))?;
    Ok(())
}

async fn check_audit(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let sent = Sent::record(&cx)?;
    let (_, tab) = open_settings(&cx, TabId::Audit).await?;
    let Tab::Audit(audit) = tab else { bail!("not the Audit Log tab") };
    wait_for(&cx, Duration::from_secs(10), "the first page", |cx| audit.read(cx).entries().1).await?;
    let first = cx.update(|_, cx| audit.read(cx).entries().0.len())?;
    cx.update(|_, cx| audit.update(cx, |t, cx| t.set_category(Category::Moderation, cx)))?;
    cx.update(|w, cx| audit.update(cx, |t, cx| t.focus_search(w, cx)))?;
    cx.sleep(Duration::from_millis(50)).await;
    cx.type_text("alice")?;
    let moderation = actions_of(Category::Moderation);
    let t = wait_for(&cx, Duration::from_secs(10), "the filtered request", |_| {
        sent.find(|r| matches!(r, Request::ListAuditLog { query, actions, .. } if query == "alice" && *actions == moderation)).is_some()
    })
    .await?;
    let request = sent.find(|r| matches!(r, Request::ListAuditLog { query, .. } if query == "alice")).context("vanished")?;
    cx.log(format_args!("sent {request:?} ({} ms after typing)", t.as_millis()));
    let Request::ListAuditLog { seq, .. } = request else { bail!("not an audit request") };
    wait_for(&cx, Duration::from_secs(10), "its answer", |cx| {
        let a = audit.read(cx);
        a.entries().1 && matches!(a.request(0), Request::ListAuditLog { seq: s, .. } if s == seq)
    })
    .await?;
    let actions: Vec<String> = cx.update(|_, cx| audit.read(cx).entries().0.iter().map(|e| e.action.clone()).collect())?;
    cx.log(format_args!("{first} entries unfiltered; {} moderation entries matching \"alice\"", actions.len()));
    ensure!(actions.iter().all(|a| moderation.contains(a)), "an entry outside the filter: {actions:?}");
    cx.update(|_, cx| requests::uninstall(cx))?;
    Ok(())
}

/// The ban list, asked for again until `ready` holds.
async fn wait_bans(cx: &TestCx, what: &str, ready: impl Fn(&[decibell_model::stores::community::Ban]) -> bool) -> Result<()> {
    let started = Instant::now();
    loop {
        let done = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).is_some_and(|c| ready(&c.moderation.read(cx).bans)))?;
        if done {
            return Ok(());
        }
        ensure!(started.elapsed() < Duration::from_secs(10), "timed out waiting for {what}");
        send(cx, Request::ListBans { server: daily_gaming() }).await?;
        cx.sleep(Duration::from_millis(400)).await;
    }
}

async fn check_bans(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let user = std::env::var("DECIBELL_ST_USER").context("DECIBELL_ST_USER")?;
    let member = find_member(&cx, &user).await?;
    let (window, tab) = open_settings(&cx, TabId::Members).await?;
    let Tab::Members(members) = tab else { bail!("not the Members tab") };
    cx.update(|_, cx| {
        members.update(cx, |t, cx| {
            t.open_confirm(ModKind::Ban, member.clone(), cx);
            t.run_mod(cx);
        })
    })?;
    wait_bans(&cx, "the ban to be listed", |bans| bans.iter().any(|b| b.uid == member.uid)).await?;
    cx.log(format_args!("{user} (uid {}) banned and listed", member.uid));
    let (_, tab) = show(&cx, &window, TabId::Bans).await?;
    let Tab::Bans(bans) = tab else { bail!("not the Bans tab") };
    cx.update(|_, cx| bans.update(cx, |t, cx| t.unban(member.uid, member.username.clone(), cx)))?;
    wait_bans(&cx, "the unban", |bans| !bans.iter().any(|b| b.uid == member.uid)).await?;
    cx.log(format_args!("{user} unbanned from the Bans tab"));
    Ok(())
}

/// What the channel test's phases hand each other.
struct ChannelState {
    channel: String,
    role: i64,
    user_uid: i64,
}

impl ChannelState {
    fn write(&self, path: &Path) -> Result<()> {
        let json = serde_json::json!({ "channel": self.channel, "role": self.role, "user_uid": self.user_uid });
        std::fs::write(path, serde_json::to_vec(&json)?)?;
        Ok(())
    }

    fn read(path: &Path) -> Result<ChannelState> {
        let json: serde_json::Value = serde_json::from_slice(&std::fs::read(path).context("the setup phase's state")?)?;
        Ok(ChannelState {
            channel: json["channel"].as_str().context("no channel")?.to_string(),
            role: json["role"].as_i64().context("no role")?,
            user_uid: json["user_uid"].as_i64().context("no uid")?,
        })
    }
}

fn channel_of(cx: &App, id: &str) -> Option<decibell_model::stores::community::Channel> {
    model(cx).community(daily_gaming(), cx).and_then(|c| c.channels.read(cx).get(id).cloned())
}

fn channel_named(cx: &App, name: &str) -> Option<decibell_model::stores::community::Channel> {
    model(cx).community(daily_gaming(), cx).and_then(|c| c.channels.read(cx).list().iter().find(|c| c.name == name).cloned())
}

async fn create_channel(cx: &TestCx, name: &str, kind: &'static str, category: Option<ChannelId>) -> Result<String> {
    send(cx, Request::CreateChannel { server: daily_gaming(), name: name.to_string(), kind, category, encrypted: false }).await?;
    wait_for(cx, Duration::from_secs(8), &format!("#{name} to exist"), |cx| channel_named(cx, name).is_some()).await?;
    Ok(cx.update(|_, cx| channel_named(cx, name))?.context("vanished")?.id.to_string())
}

async fn open_channel_settings(cx: &TestCx, channel: &str) -> Result<Entity<ChannelSettingsWindow>> {
    let id = ChannelId::from(channel.to_string());
    cx.update(|_, cx| {
        actions::servers::open_server(daily_gaming(), cx);
        model(cx).overlays.update(cx, |o, cx| o.open_channel_settings(id, cx));
    })?;
    wait_for(cx, Duration::from_secs(10), "Channel Settings to open", |cx| host::channel_window(cx).is_some()).await?;
    cx.update(|_, cx| host::channel_window(cx))?.context("the window went away")
}

async fn check_channel(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let phase = std::env::var("DECIBELL_ST_PHASE").unwrap_or_default();
    let state_file = PathBuf::from(std::env::var("DECIBELL_ST_STATE").context("DECIBELL_ST_STATE")?);
    match phase.as_str() {
        "setup" => channel_setup(&cx, &state_file).await,
        "finish" => channel_finish(&cx, &state_file).await,
        other => bail!("unknown phase {other:?}"),
    }
}

async fn channel_setup(cx: &TestCx, state_file: &Path) -> Result<()> {
    let name = tag();
    let user = std::env::var("DECIBELL_ST_USER").context("DECIBELL_ST_USER")?;
    // The test's role, held by the throwaway.
    send(cx, Request::CreateRole { server: daily_gaming(), name: name.clone(), color: 0, permissions: 0, manage_each_other: false }).await?;
    wait_for(cx, Duration::from_secs(8), "the test role", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.roles.read(cx).list().iter().any(|r| r.name == name.as_str()))
    })
    .await?;
    let role = cx
        .update(|_, cx| model(cx).community(daily_gaming(), cx).and_then(|c| c.roles.read(cx).list().iter().find(|r| r.name == name.as_str()).map(|r| r.id)))?
        .context("the role vanished")?;
    let member = find_member(cx, &user).await?;
    send(cx, Request::SetMemberRoles { server: daily_gaming(), username: user.clone(), uid: member.uid, role_ids: vec![role] }).await?;
    cx.log(format_args!("role {name} (id {role}) given to {user}"));

    // The channel, then its settings.
    let channel = create_channel(cx, &name, "text", None).await?;
    let window = open_channel_settings(cx, &channel).await?;
    let renamed = format!("{name}-r");
    // Typed (the fields' change events reach the draft once this update ends), then picked and
    // saved, as separate input events.
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.name.update(cx, |s, cx| s.set_value(&renamed, cx));
            w.topic.update(cx, |s, cx| s.set_value("What the settings test talks about", cx));
        })
    })?;
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.change(cx, |d| d.slowmode = 5);
            w.save(cx);
        })
    })?;
    ensure!(cx.update(|_, cx| window.read(cx).saving())?, "Save didn't wait for the echo");
    let t = wait_for(cx, Duration::from_secs(8), "rename, topic and slowmode to echo", |cx| {
        let w = window.read(cx);
        !w.saving()
            && !w.dirty(cx)
            && channel_of(cx, &channel).is_some_and(|c| c.name == renamed.as_str() && c.topic == "What the settings test talks about" && c.slowmode_seconds == 5)
    })
    .await?;
    cx.log(format_args!("rename, topic, slowmode echoed in {} ms", t.as_millis()));

    // The role may not send here.
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.select_tab(ChannelTab::Permissions, cx);
            w.stage_bits(&format!("role:{role}"), Bits { allow: 0, deny: perm::SEND_MESSAGES }, cx);
            w.save(cx);
        })
    })?;
    let t = wait_for(cx, Duration::from_secs(8), "the overwrite to echo", |cx| {
        let w = window.read(cx);
        !w.saving() && !w.dirty(cx) && w.overwrites(cx).iter().any(|o| o.target_id == role.to_string() && o.deny == perm::SEND_MESSAGES)
    })
    .await?;
    cx.log(format_args!("deny Send Messages for {name} echoed in {} ms", t.as_millis()));
    ChannelState { channel, role, user_uid: member.uid }.write(state_file)?;
    Ok(())
}

async fn channel_finish(cx: &TestCx, state_file: &Path) -> Result<()> {
    let state = ChannelState::read(state_file)?;
    cx.log(format_args!("the test channel {}, role {}, throwaway uid {}", state.channel, state.role, state.user_uid));
    let channel = state.channel.clone();
    wait_for(cx, Duration::from_secs(8), "the test channel", |cx| channel_of(cx, &channel).is_some()).await?;
    let window = open_channel_settings(cx, &channel).await?;

    // Unsaved changes hold the window: closing shakes the bar.
    let nudges = cx.update(|_, cx| window.read(cx).nudges())?;
    cx.update(|_, cx| window.update(cx, |w, cx| w.change(cx, |d| d.slowmode = 10)))?;
    cx.update(|_, cx| window.update(cx, |w, cx| w.request_close(cx)))?;
    let (after, open) = cx.update(|_, cx| (window.read(cx).nudges(), model(cx).overlays.read(cx).is_open(modal::CHANNEL_SETTINGS)))?;
    ensure!(after == nudges + 1 && open, "closing with unsaved changes didn't shake ({nudges} → {after}, open {open})");
    cx.update(|_, cx| window.update(cx, |w, cx| w.change(cx, |d| d.slowmode = 5)))?;
    ensure!(!cx.update(|_, cx| window.read(cx).dirty(cx))?, "still dirty after undoing");
    cx.log("closing with unsaved changes shook the save bar and kept the window");

    // Retention.
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.select_tab(ChannelTab::Retention, cx);
            w.change(cx, |d| d.retention[1] = 30);
            w.save(cx);
        })
    })?;
    let t = wait_for(cx, Duration::from_secs(8), "the retention echo", |cx| {
        !window.read(cx).saving() && !window.read(cx).dirty(cx) && channel_of(cx, &channel).is_some_and(|c| c.retention_days_image == 30)
    })
    .await?;
    cx.log(format_args!("image retention 30 days echoed in {} ms", t.as_millis()));

    // One message, then the wipe: the toast counts it.
    let server = daily_gaming();
    let id = ChannelId::from(channel.clone());
    cx.update(|_, cx| {
        actions::chat::open_channel(server, id.clone(), cx);
    })?;
    cx.sleep(Duration::from_millis(500)).await;
    cx.update(|_, cx| actions::chat::send_message(server, id.clone(), "settings test", 0, cx))?;
    wait_for(cx, Duration::from_secs(8), "our message's echo", |cx| {
        model(cx).chat.channels.read(cx).get(server, &id).is_some_and(|s| s.read(cx).rows().iter().any(|m| m.content == "settings test" && !m.pending))
    })
    .await?;
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.ask(Confirm::Wipe, cx);
            w.run_confirm(cx);
        })
    })?;
    wait_for(cx, Duration::from_secs(8), "the wipe's toast", |cx| model(cx).toasts.read(cx).toasts().iter().any(|t| t.title == "Channel history wiped")).await?;
    let body = cx.update(|_, cx| {
        model(cx).toasts.read(cx).toasts().iter().find(|t| t.title == "Channel history wiped").and_then(|t| t.body.clone()).unwrap_or_default().to_string()
    })?;
    cx.log(format_args!("wipe toast: {body}"));
    ensure!(body == "Removed 1 message(s) and 0 attachment(s).", "the toast counts {body:?}");

    // Delete through the confirmation.
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.ask(Confirm::Delete, cx);
            ensure!(w.asking() == Some(Confirm::Delete), "no confirmation");
            w.run_confirm(cx);
            Ok(())
        })
    })??;
    wait_for(cx, Duration::from_secs(8), "the channel to be deleted and the window to close", |cx| {
        channel_of(cx, &channel).is_none() && !model(cx).overlays.read(cx).is_open(modal::CHANNEL_SETTINGS)
    })
    .await?;
    cx.log("deleted through the confirmation; the window closed");
    let _ = std::fs::remove_file(state_file);
    Ok(())
}

async fn check_category(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let name = tag();
    let category = create_channel(&cx, &format!("{name}-cat"), "category", None).await?;
    let inside = create_channel(&cx, &format!("{name}-in"), "text", Some(ChannelId::from(category.clone()))).await?;
    let window = open_channel_settings(&cx, &category).await?;
    cx.update(|_, cx| window.update(cx, |w, cx| w.select_tab(ChannelTab::Permissions, cx)))?;

    // Following on.
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.stage_category_sync(true, cx);
            w.save(cx);
        })
    })?;
    wait_for(&cx, Duration::from_secs(8), "following to switch on", |cx| {
        !window.read(cx).saving() && !window.read(cx).dirty(cx) && channel_of(cx, &category).is_some_and(|c| c.category_sync)
    })
    .await?;
    cx.log("the category's channels follow it");

    // The category's own rule, then Sync all: the channel inside matches it.
    let everyone = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).and_then(|c| c.roles.read(cx).list().iter().find(|r| r.is_default).map(|r| r.id)))?.context("no @everyone")?;
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.stage_bits(&format!("role:{everyone}"), Bits { allow: perm::EMBED_LINKS, deny: 0 }, cx);
            w.save(cx);
        })
    })?;
    wait_for(&cx, Duration::from_secs(8), "the category's overwrite", |cx| !window.read(cx).saving() && !window.read(cx).dirty(cx)).await?;
    let before = cx.update(|_, cx| channel_of(cx, &inside).map(|c| c.permissions_synced))?;
    cx.log(format_args!("#{name}-in synced before Sync all: {before:?}"));
    cx.update(|_, cx| window.update(cx, |w, cx| w.sync_now(cx)))?;
    let t = wait_for(&cx, Duration::from_secs(8), "the channel inside to be synced", |cx| channel_of(cx, &inside).is_some_and(|c| c.permissions_synced)).await?;
    cx.log(format_args!("Sync all: #{name}-in synced in {} ms", t.as_millis()));

    // Following off.
    cx.update(|_, cx| {
        window.update(cx, |w, cx| {
            w.stage_category_sync(false, cx);
            w.save(cx);
        })
    })?;
    wait_for(&cx, Duration::from_secs(8), "following to switch off", |cx| {
        !window.read(cx).saving() && channel_of(cx, &category).is_some_and(|c| !c.category_sync)
    })
    .await?;
    cx.log("following switched off");
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    for id in [inside, category] {
        send(&cx, Request::DeleteChannel { server: daily_gaming(), channel: ChannelId::from(id.clone()) }).await?;
        wait_for(&cx, Duration::from_secs(8), "the delete", |cx| channel_of(cx, &id).is_none()).await?;
    }
    Ok(())
}

/// The throwaway account's side: `join` registers it and joins the community; `send` signs in
/// and tries to post where its role may not.
async fn check_throwaway(cx: TestCx) -> Result<()> {
    let user = std::env::var("DECIBELL_ST_USER").context("DECIBELL_ST_USER")?;
    let password = std::env::var("DECIBELL_DEV_PASSWORD").context("DECIBELL_DEV_PASSWORD")?;
    let phase = std::env::var("DECIBELL_ST_PHASE").unwrap_or_default();
    wait_for(&cx, Duration::from_secs(15), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    let sign_in = async |cx: &TestCx| -> Result<bool> {
        let (u, p) = (user.clone(), password.clone());
        cx.update(move |_, cx| actions::auth::sign_in(u, p, cx))?;
        wait_for(cx, Duration::from_secs(30), "the sign-in's answer", |cx| {
            let a = model(cx).auth.read(cx);
            a.is_authenticated || a.login_error.is_some()
        })
        .await?;
        cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)
    };
    if !sign_in(&cx).await? {
        let error = cx.update(|_, cx| model(cx).auth.read(cx).login_error.clone())?.unwrap_or_default();
        ensure!(matches!(phase.as_str(), "join" | "watch-membership"), "{user} couldn't sign in: {error}");
        cx.log(format_args!("{user} doesn't sign in ({error}): registering it"));
        cx.update(|_, cx| model(cx).auth.update(cx, |a, cx| a.set_login_error(None, cx)))?;
        let (u, p) = (user.clone(), password.clone());
        cx.update(move |_, cx| actions::auth::register(u.clone(), format!("{u}@example.test"), p, cx))?;
        wait_for(&cx, Duration::from_secs(15), "the registration", |cx| model(cx).auth.read(cx).register_result.is_some()).await?;
        let result = cx.update(|_, cx| model(cx).auth.read(cx).register_result.clone())?.context("no result")?;
        ensure!(result.success, "registering {user}: {}", result.message);
        cx.log(format_args!("registered {user}"));
        ensure!(sign_in(&cx).await?, "{user} couldn't sign in after registering");
    }
    let result = throwaway_phase(&cx, &user, &phase).await;
    // Signing out makes central forget this profile's device.
    cx.update(|_, cx| actions::auth::sign_out(cx))?;
    let _ = wait_for(&cx, Duration::from_secs(5), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await;
    result
}

async fn throwaway_phase(cx: &TestCx, user: &str, phase: &str) -> Result<()> {
    let cx = cx.clone();
    if phase != "join" && phase != "join-device" && phase != "send" {
        return admin::throwaway_phase(&cx, phase).await;
    }
    if phase == "join" || phase == "join-device" {
        // Still a member (a run that ended before its cleanup): nothing to join.
        let member = wait_for(&cx, Duration::from_secs(3), "a membership", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await.is_ok();
        if member {
            cx.log(format_args!("{user} is already a member"));
            return Ok(());
        }
        let host = std::env::var("DECIBELL_ST_HOST").context("DECIBELL_ST_HOST")?;
        let port: u16 = std::env::var("DECIBELL_ST_PORT").context("DECIBELL_ST_PORT")?.parse()?;
        let code = std::env::var("DECIBELL_ST_INVITE").context("DECIBELL_ST_INVITE")?;
        let failed: Rc<RefCell<Option<String>>> = Rc::default();
        let sink = failed.clone();
        // An invite joined by host and port opens under the provisional "host:port" id, re-keyed
        // onto central's only when central has an address on record for it (the directory, an
        // invite resolution): an unlisted community (Rustaceans) stays provisional until the
        // next sign-in.
        let provisional = ServerId::from_wire(&format!("{host}:{port}"));
        cx.update(move |_, cx| {
            actions::servers::redeem_invite(host, port, code, cx, move |r, _| {
                if let Err(e) = r {
                    *sink.borrow_mut() = Some(e.to_string());
                }
            })
        })?;
        let f = failed.clone();
        wait_for(&cx, Duration::from_secs(20), "the community to accept the invite", move |cx| {
            let m = model(cx);
            let c = m.connections.read(cx);
            f.borrow().is_some() || c.is_connected(daily_gaming()) || c.is_connected(provisional)
        })
        .await?;
        if let Some(e) = failed.borrow().clone() {
            bail!("joining with the invite: {e}");
        }
        cx.log(format_args!("{user} joined the community ({})", daily_gaming().wire()));
        return Ok(());
    }
    // `send`: the overwrite denies our role Send Messages in the test channel.
    let state_file = PathBuf::from(std::env::var("DECIBELL_ST_STATE").context("DECIBELL_ST_STATE")?);
    let state = ChannelState::read(&state_file)?;
    wait_for(&cx, Duration::from_secs(30), "the community to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    let channel = state.channel.clone();
    wait_for(&cx, Duration::from_secs(10), "the test channel", |cx| channel_of(cx, &channel).is_some()).await?;
    let can_send = cx.update(|_, cx| model(cx).channel_permission(daily_gaming(), &channel, perm::SEND_MESSAGES, cx))?;
    cx.log(format_args!("Send Messages in the test channel, as the client works it out: {can_send}"));
    let answers = Answers::record(&cx)?;
    let id = ChannelId::from(channel.clone());
    cx.update(|_, cx| {
        actions::chat::open_channel(daily_gaming(), id.clone(), cx);
    })?;
    cx.sleep(Duration::from_millis(500)).await;
    cx.update(|_, cx| actions::chat::send_message(daily_gaming(), id.clone(), "this should be refused", 0, cx))?;
    let t = wait_for(&cx, Duration::from_secs(10), "the refusal", |cx| {
        model(cx).toasts.read(cx).toasts().iter().any(|t| t.title == "Message not sent")
            || answers.any(|e| matches!(e, CoreEvent::ChannelMessageRejected(_)))
    })
    .await?;
    let reason = cx.update(|_, cx| {
        model(cx).toasts.read(cx).toasts().iter().find(|t| t.title == "Message not sent").and_then(|t| t.body.clone()).unwrap_or_default().to_string()
    })?;
    cx.log(format_args!("the community refused the send after {} ms: {reason}", t.as_millis()));
    Ok(())
}

/// Delete everything named after this run's tag; take the throwaway out of the community.
async fn check_cleanup(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let name = tag();
    let mine = |n: &str| n == name || n.starts_with(&format!("{name}-"));
    let channels: Vec<(String, ChannelKind)> = cx.update(|_, cx| {
        model(cx)
            .community(daily_gaming(), cx)
            .map(|c| c.channels.read(cx).list().iter().filter(|c| mine(&c.name)).map(|c| (c.id.to_string(), c.kind)).collect())
            .unwrap_or_default()
    })?;
    // Channels before their category.
    let mut ordered = channels.clone();
    ordered.sort_by_key(|(_, kind)| *kind == ChannelKind::Category);
    for (id, _) in ordered {
        send(&cx, Request::DeleteChannel { server: daily_gaming(), channel: ChannelId::from(id.clone()) }).await?;
        wait_for(&cx, Duration::from_secs(8), "a delete", |cx| channel_of(cx, &id).is_none()).await?;
        cx.log(format_args!("cleanup: deleted channel {id}"));
    }
    let roles: Vec<i64> = cx.update(|_, cx| {
        model(cx).community(daily_gaming(), cx).map(|c| c.roles.read(cx).list().iter().filter(|r| mine(&r.name)).map(|r| r.id).collect()).unwrap_or_default()
    })?;
    for id in roles {
        send(&cx, Request::DeleteRole { server: daily_gaming(), role_id: id }).await?;
        cx.log(format_args!("cleanup: deleted role {id}"));
    }
    if let Ok(user) = std::env::var("DECIBELL_ST_USER") {
        let search = cx.update(|_, cx| cx.new(|cx| MemberSearch::new(daily_gaming(), &user, 0, cx)))?;
        wait_for(&cx, Duration::from_secs(10), "the throwaway's search", |cx| search.read(cx).loaded && !search.read(cx).loading).await?;
        let member = cx.update(|_, cx| search.read(cx).members.iter().find(|m| m.username == user.as_str()).cloned())?;
        if let Some(m) = member {
            send(&cx, Request::KickMember { server: daily_gaming(), username: user.clone(), uid: m.uid, reason: "settings self-test cleanup".into() }).await?;
            cx.log(format_args!("cleanup: {user} taken out of the community"));
        }
        // A ban left by a failed bans-live.
        let banned = cx.update(|_, cx| model(cx).community(daily_gaming(), cx).and_then(|c| c.moderation.read(cx).bans.iter().find(|b| b.username == user.as_str()).map(|b| b.uid)))?;
        send(&cx, Request::ListBans { server: daily_gaming() }).await?;
        cx.sleep(Duration::from_millis(500)).await;
        let banned = banned.or(cx.update(|_, cx| model(cx).community(daily_gaming(), cx).and_then(|c| c.moderation.read(cx).bans.iter().find(|b| b.username == user.as_str()).map(|b| b.uid)))?);
        if let Some(uid) = banned {
            send(&cx, Request::UnbanMember { server: daily_gaming(), username: user.clone(), uid }).await?;
            cx.log(format_args!("cleanup: {user}'s ban lifted"));
        }
    }
    // The picture test's upload, if it got no further.
    if std::env::var("DECIBELL_ST_PICTURE").as_deref() == Ok("1") {
        let version = cx.update(|_, cx| model(cx).servers.read(cx).picture_version(daily_gaming()).to_string())?;
        if !version.is_empty() {
            send(&cx, Request::UpdateServerPicture { server: daily_gaming(), bytes: Default::default() }).await?;
            wait_for(&cx, Duration::from_secs(10), "the picture's removal", |cx| model(cx).servers.read(cx).picture_version(daily_gaming()).is_empty()).await?;
            cx.log("cleanup: the server picture removed");
        }
    }
    // Let the last requests leave before the app quits.
    cx.sleep(Duration::from_millis(500)).await;
    Ok(())
}

// ── In this process: the fixture, a window of our own ──────────────────────────────────────

/// A window like the main one (`visible` px, client-decorated) showing `build`'s view.
fn open_window(cx: &TestCx, visible: (f32, f32), build: impl FnOnce(&mut Window, &mut App) -> AnyView + 'static) -> Result<TestCx> {
    let handle = cx.app().update(|cx| -> Result<AnyWindowHandle> {
        let bounds = Bounds::centered(None, decibell_ui::outer_window_size(size(px(visible.0), px(visible.1))), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            window_min_size: Some(size(px(800.), px(600.))),
            titlebar: Some(TitlebarOptions { title: Some("Decibell settings test".into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("decibell".into()),
            ..Default::default()
        };
        let window = cx.open_window(options, |window, cx| {
            let content = build(window, cx);
            cx.new(|_| TestRoot { content })
        })?;
        Ok(window.into())
    })?;
    Ok(TestCx::new(cx.app().clone(), handle))
}

fn close_window(cx: &TestCx) {
    let _ = cx.update(|window, _| window.remove_window());
}

/// The window frame, the title bar, the content and the toasts (as the app's root draws them).
struct TestRoot {
    content: AnyView,
}

impl Render for TestRoot {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = decibell_ui::focus_navigation(div())
            .size_full()
            .flex()
            .flex_col()
            .child(decibell_ui::TitleBar::new("Decibell settings test"))
            .child(div().flex_1().min_h_0().child(self.content.clone()))
            .child(decibell_ui::ToastStack::new());
        decibell_ui::window_frame(content, window, cx)
    }
}

/// Nothing drawn for `seconds` after drawing stopped, in one of three tries (a late answer from the
/// stack may draw once after the first quiet second: a tab that keeps drawing fails all three). A
/// focused window may present the shell's glow without drawing (at most its 30 fps cap; 0
/// unfocused), so presents are only bounded. When every try drew while the shared desktop disturbed
/// the window (the focus changing hands, the pointer crossing it), nothing was measured: inconclusive.
async fn assert_idle(cx: &TestCx, seconds: u64, what: &str) -> Result<Frames> {
    let mut last = String::new();
    let mut undisturbed: Vec<String> = Vec::new();
    for _ in 0..3 {
        cx.settle_drawn().await?;
        // A frame already asked for when drawing seemed to stop (a notify a moment before, the
        // compositor's activation) lands in these 100 ms, not in the measurement.
        let gap = cx.watched_sleep(Duration::from_millis(100)).await?;
        let before = cx.frames()?;
        let disturbed = cx.watched_sleep(Duration::from_secs(seconds)).await?.or(gap);
        let frames = cx.frames()?.since(before);
        if frames.drawn == 0 && frames.presented as f32 <= 31. * seconds as f32 {
            return Ok(frames);
        }
        last = format!("{} drawn, {} presented in {seconds} s", frames.drawn, frames.presented);
        match disturbed {
            Some(d) => last = format!("{last}; disturbed: {d}"),
            None => undisturbed.push(last.clone()),
        }
        // Every failed try is in the log: a view that keeps drawing fails each one, something
        // that finished late once (a request's failure arriving on a slow machine: the Windows
        // runner drew one frame in its first window, 2026-11-27) only the first.
        cx.log(format_args!("{what}: not idle in a try ({last}); measuring again"));
    }
    // No try was quiet. One that drew with nothing disturbing it is evidence; three disturbed ones are not.
    if !undisturbed.is_empty() {
        bail!("{what}: not idle in three tries ({})", undisturbed.join("; then "));
    }
    Err(decibell_harness::inconclusive(format_args!("{what}: three tries disturbed by the desktop ({last})")))
}

trait SettleDrawn {
    async fn settle_drawn(&self) -> Result<Frames>;
}

impl SettleDrawn for TestCx {
    /// Until nothing has been drawn for 500 ms (presents may go on: the glow).
    async fn settle_drawn(&self) -> Result<Frames> {
        let started = Instant::now();
        let mut last = self.frames()?;
        let mut quiet_since = Instant::now();
        loop {
            self.sleep(Duration::from_millis(50)).await;
            let now = self.frames()?;
            if now.drawn != last.drawn {
                quiet_since = Instant::now();
            } else if quiet_since.elapsed() >= Duration::from_millis(500) {
                return Ok(now);
            }
            last = now;
            ensure!(started.elapsed() < Duration::from_secs(15), "the window was still drawing after 15 s");
        }
    }
}

/// A window with the fixture's main layout: the shell draws the settings overlays
/// (`init` registered them), as in the app.
fn open_fixture_window(cx: &TestCx, visible: (f32, f32)) -> Result<TestCx> {
    open_window(cx, visible, |_, cx| {
        crate::fixture::apply(cx);
        cx.new(Shell::new).into()
    })
}

async fn settings_idle(cx: TestCx) -> Result<()> {
    let test = open_fixture_window(&cx, (1280., 800.))?;
    let result = async {
        test.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.open_modal(modal::SERVER_SETTINGS, cx)))?;
        wait_for(&test, Duration::from_secs(5), "the window", |cx| host::server_window(cx).is_some()).await?;
        let window = test.update(|_, cx| host::server_window(cx))?.context("no window")?;
        for tab in [TabId::Overview, TabId::Members, TabId::Roles, TabId::Audit, TabId::Bans, TabId::Storage] {
            test.update(|w, cx| window.update(cx, |s, cx| s.show(tab, w, cx)))?;
            let idle = assert_idle(&test, 5, tab.label()).await?;
            test.log(format_args!("{}: 0 frames drawn over 5 s ({} presented)", tab.label(), idle.presented));
        }
        let weak = window.downgrade();
        let tab = test.update(|_, cx| window.read(cx).tab().clone())?;
        let weak_tab = match tab {
            Tab::Storage(t) => t.downgrade(),
            _ => bail!("not on Storage"),
        };
        drop(window);
        test.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
        wait_for(&test, Duration::from_secs(3), "the window to be dropped", |cx| host::server_window(cx).is_none()).await?;
        test.sleep(Duration::from_millis(100)).await;
        ensure!(weak.upgrade().is_none() && weak_tab.upgrade().is_none(), "the closed window (or its tab) is still alive");
        test.log("closed: the window and its tab are gone");
        assert_idle(&test, 2, "closed").await?;
        Ok(())
    }
    .await;
    close_window(&test);
    result
}

/// Where the settings pane starts, from the window's left edge.
async fn pane_left(cx: &TestCx, visible: (f32, f32)) -> Result<(f32, f32)> {
    let test = open_fixture_window(cx, visible)?;
    let result = async {
        test.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.open_modal(modal::SERVER_SETTINGS, cx)))?;
        wait_for(&test, Duration::from_secs(5), "the window", |cx| host::server_window(cx).is_some()).await?;
        test.settle_drawn().await?;
        let window = test.update(|_, cx| host::server_window(cx))?.context("no window")?;
        let pane = test.update(|_, cx| window.read(cx).pane_bounds())?;
        let origin = test.update(|w, cx| decibell_ui::visible_window_bounds(w, cx).origin)?;
        test.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
        wait_for(&test, Duration::from_secs(3), "the window to close", |cx| host::server_window(cx).is_none()).await?;
        Ok((f32::from(pane.left() - origin.x), f32::from(pane.size.width)))
    }
    .await;
    close_window(&test);
    result
}

async fn settings_narrow(cx: TestCx) -> Result<()> {
    let (left, width) = pane_left(&cx, (900., 700.)).await?;
    cx.log(format_args!("900 px wide: the pane starts {left} px in, {width} px wide"));
    ensure!(left < 140., "below 1024 px the sidebar should be the 64 px rail (pane at {left})");
    let (left, width) = pane_left(&cx, (1280., 800.)).await?;
    cx.log(format_args!("1280 px wide: the pane starts {left} px in, {width} px wide"));
    ensure!(left > 250., "at 1280 px the full sidebar should show (pane at {left})");
    Ok(())
}

/// Shows the editor (a modal) over an empty page.
struct EditorRoot {
    editor: Entity<PictureEditor>,
}

impl Render for EditorRoot {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(self.editor.clone())
    }
}

fn png(img: &image::RgbaImage) -> Vec<u8> {
    let mut out = std::io::Cursor::new(Vec::new());
    img.write_to(&mut out, image::ImageFormat::Png).expect("png");
    out.into_inner()
}

/// A JPEG stored 400 × 200 (left red, right blue) whose EXIF says "rotate 90° clockwise".
fn sideways_jpeg() -> Vec<u8> {
    let stored = image::RgbImage::from_fn(400, 200, |x, _| if x < 200 { image::Rgb([220, 20, 20]) } else { image::Rgb([20, 20, 220]) });
    let mut jpeg = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpeg, 95);
    stored.write_with_encoder(enc).expect("jpeg");
    let mut tiff = Vec::new();
    tiff.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
    tiff.extend_from_slice(&1u16.to_be_bytes());
    tiff.extend_from_slice(&0x0112u16.to_be_bytes());
    tiff.extend_from_slice(&3u16.to_be_bytes());
    tiff.extend_from_slice(&1u32.to_be_bytes());
    tiff.extend_from_slice(&6u16.to_be_bytes());
    tiff.extend_from_slice(&[0, 0]);
    tiff.extend_from_slice(&0u32.to_be_bytes());
    let mut app1 = b"Exif\0\0".to_vec();
    app1.extend_from_slice(&tiff);
    let mut out = jpeg[..2].to_vec();
    out.extend_from_slice(&[0xFF, 0xE1]);
    out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
    out.extend_from_slice(&app1);
    out.extend_from_slice(&jpeg[2..]);
    out
}

/// Open the editor on `bytes`; check the size it reads, the zoom and pan limits by keyboard, and
/// the saved output.
async fn edit_one(cx: &TestCx, bytes: Vec<u8>, name: &str, options: EditorOptions, nat: (f32, f32), fit: bool) -> Result<()> {
    let slot: Rc<RefCell<Option<Entity<PictureEditor>>>> = Rc::default();
    let saved: Rc<RefCell<Option<Vec<u8>>>> = Rc::default();
    let closed: Rc<RefCell<bool>> = Rc::default();
    let (s, sv, cl) = (slot.clone(), saved.clone(), closed.clone());
    let output = options.output;
    let name = name.to_string();
    let test = open_window(cx, (1280., 800.), move |window, cx| {
        let editor = cx.new(|cx| PictureEditor::new(PictureSource::Bytes { bytes, name }, options, window, cx));
        cx.subscribe(&editor, move |editor, event: &PictureEditorEvent, cx| match event {
            PictureEditorEvent::Save(jpeg) => {
                *sv.borrow_mut() = Some(jpeg.to_vec());
                editor.update(cx, |e, cx| e.save_done(Ok(()), cx));
            }
            PictureEditorEvent::Closed { .. } => *cl.borrow_mut() = true,
        })
        .detach();
        *s.borrow_mut() = Some(editor.clone());
        cx.new(|_| EditorRoot { editor }).into()
    })?;
    let result = async {
        let editor = slot.borrow().clone().context("no editor")?;
        wait_for(&test, Duration::from_secs(10), "the picture to load", |cx| editor.read(cx).picture().is_some() || editor.read(cx).error().is_some()).await?;
        if let Some(e) = test.update(|_, cx| editor.read(cx).error().cloned())? {
            bail!("the editor refused the file: {e}");
        }
        let read = test.update(|_, cx| editor.read(cx).picture().map(|p| (p.nat.w, p.nat.h)))?.context("no picture")?;
        ensure!(read == nat, "read as {read:?}, expected {nat:?} (upright)");
        test.settle_drawn().await?;
        let (view, frame) = test.update(|_, cx| editor.update(cx, |e, _| e.crop()))?.context("no crop")?;
        let natural = geometry::Size::new(nat.0, nat.1);
        let out = geometry::Size::new(output.0 as f32, output.1 as f32);
        let range = geometry::range(natural, frame, out, fit);
        test.log(format_args!("{read:?}: frame {}×{}, scale {:.3} in [{:.3}, {:.3}]", frame.w, frame.h, view.scale, range.min, range.max));
        // Zoom in past the top: it stops at the limit.
        for _ in 0..30 {
            test.keystroke("+")?;
        }
        test.settle_drawn().await?;
        let (view, _) = test.update(|_, cx| editor.update(cx, |e, _| e.crop()))?.context("no crop")?;
        ensure!((view.scale - range.max).abs() < range.max * 0.01, "zoomed to {} (max {})", view.scale, range.max);
        // Pan far right: the picture keeps covering the frame.
        for _ in 0..80 {
            test.keystroke("shift-right")?;
        }
        test.settle_drawn().await?;
        let (view, _) = test.update(|_, cx| editor.update(cx, |e, _| e.crop()))?.context("no crop")?;
        let clamped = geometry::clamp_view(view, natural, frame, range);
        ensure!(geometry::same_view(view, clamped), "panned out of bounds: {view:?} (clamped {clamped:?})");
        // Zoom out past the bottom.
        for _ in 0..40 {
            test.keystroke("-")?;
        }
        test.settle_drawn().await?;
        let (view, _) = test.update(|_, cx| editor.update(cx, |e, _| e.crop()))?.context("no crop")?;
        ensure!((view.scale - range.min).abs() < range.min * 0.01, "zoomed out to {} (min {})", view.scale, range.min);
        test.log(format_args!("zoom stops at {:.3} and {:.3}; panning stays inside", range.max, range.min));
        // Enter saves.
        test.keystroke("enter")?;
        wait_for(&test, Duration::from_secs(10), "the saved picture", |_| saved.borrow().is_some()).await?;
        let jpeg = saved.borrow().clone().context("no bytes")?;
        let decoded = image::load_from_memory(&jpeg)?;
        ensure!((decoded.width(), decoded.height()) == output, "saved {}×{}, expected {output:?}", decoded.width(), decoded.height());
        ensure!(jpeg.len() <= 1000 * 1024, "the JPEG is {} bytes", jpeg.len());
        wait_for(&test, Duration::from_secs(3), "the editor to close", |_| *closed.borrow()).await?;
        test.log(format_args!("saved a {}×{} JPEG ({} bytes); the editor closed", decoded.width(), decoded.height(), jpeg.len()));
        Ok(())
    }
    .await;
    close_window(&test);
    result
}

async fn picture_editor(cx: TestCx) -> Result<()> {
    let card = png(&decibell_picture_editor::picture::sample_rgba(1600, 900));
    edit_one(&cx, card, "card.png", EditorOptions::server("Daily Gaming", Some("298 members".into())), (1600., 900.), true).await?;
    edit_one(
        &cx,
        sideways_jpeg(),
        "IMG_0001.jpg",
        EditorOptions::avatar(6, "perf1", decibell_ui::Presence::Online),
        (200., 400.),
        false,
    )
    .await?;
    Ok(())
}
