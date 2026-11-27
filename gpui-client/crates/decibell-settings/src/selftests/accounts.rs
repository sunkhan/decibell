//! The account's devices, password, username and Log Out through Settings (`docs/FEATURES.md`
//! §1.5–§1.8), each on a throwaway account the check registers (phase 1 of [`super::throwaway`]),
//! with `decibell-headless` clients as the account's other devices and as a peer. Every headless
//! client signs out (forgetting its device) before it ends.
//!
//! - `settings-devices-sign-out-all`: two other devices online; "Sign out of all other devices",
//!   confirmed: the toast "Signed out — 2 other devices were signed out.", only this device
//!   listed, both others signed out.
//! - `settings-new-sign-in-toast`: a password sign-in on a new device while the app is open: the
//!   "New sign-in to your account" toast naming it; its Review devices opens Settings → Devices.
//! - `settings-password-wrong`: a wrong current password: central's refusal in the dialog; this
//!   device and the other one stay signed in.
//! - `settings-rename-refusals`: a taken name refused; two renames; the third refused (two a day);
//!   the community we are in and a watcher see each new name live; the first name still signs in
//!   and can't be registered by anyone else.
//! - `settings-log-out`: in a call, Account's Log Out: the peer sees the call end, the sign-in page
//!   shows, and the account's other device no longer lists this one. The call's sound stays on the
//!   parent's null sink (both clients' devices are set to it), never the desktop's.

use std::process::Command;
use std::sync::mpsc;

use decibell_model::live::calls::CallStatus;
use decibell_model::stores::toasts::Severity;

use super::*;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "settings-devices-sign-out-all",
            about: "throwaway account with two other devices: Sign out of all other devices, confirmed: the toast, only this device left, both signed out",
            run: |cx| Box::pin(throwaway(cx, "settings-devices-sign-out-all", 1)),
        },
        SelfTest {
            name: "settings-new-sign-in-toast",
            about: "throwaway account: a new device signs in by password: \"New sign-in to your account\"; Review devices opens Settings → Devices",
            run: |cx| Box::pin(throwaway(cx, "settings-new-sign-in-toast", 1)),
        },
        SelfTest {
            name: "settings-password-wrong",
            about: "throwaway account: a wrong current password is refused in the dialog with central's words; nothing is signed out",
            run: |cx| Box::pin(throwaway(cx, "settings-password-wrong", 1)),
        },
        SelfTest {
            name: "settings-rename-refusals",
            about: "throwaway account: a taken name and a third rename in a day refused; the community and a watcher follow each rename; the first name still signs in",
            run: |cx| Box::pin(throwaway(cx, "settings-rename-refusals", 1)),
        },
        SelfTest {
            name: "settings-log-out",
            about: "throwaway account in a call: Account's Log Out ends the call for the peer, shows the sign-in page and leaves the account's device list",
            run: |cx| Box::pin(with_sink(cx, "settings-log-out")),
        },
    ]
}

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "settings-devices-sign-out-all", about: "(in the app) sign out all others", run: |cx| Box::pin(check_sign_out_all(cx)) },
        SelfTest { name: "settings-new-sign-in-toast", about: "(in the app) the new sign-in toast", run: |cx| Box::pin(check_new_sign_in(cx)) },
        SelfTest { name: "settings-password-wrong", about: "(in the app) a wrong current password", run: |cx| Box::pin(check_password_wrong(cx)) },
        SelfTest { name: "settings-rename-refusals", about: "(in the app) rename refusals", run: |cx| Box::pin(check_rename_refusals(cx)) },
        SelfTest { name: "settings-log-out", about: "(in the app) Log Out in a call", run: |cx| Box::pin(check_log_out(cx)) },
    ]
}

/// A throwaway account's check with a null sink of this process's for its sound (the call).
async fn with_sink(cx: TestCx, check: &'static str) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let sink = match test_audio::NullSink::load("") {
        Ok(sink) => sink,
        Err(e) => {
            cx.log(format_args!("SKIP: no null sink ({e:#}): a call would use the desktop's devices"));
            return Ok(());
        }
    };
    cx.log(format_args!("null sink {} (module {}) for the call", sink.name(), sink.module()));
    let mut env = throwaway_env();
    cx.log(format_args!("throwaway account {}", env[0].1));
    env.push(("DECIBELL_CHECK_PHASE", "1".to_string()));
    env.push((CUE_SINK_ENV, sink.name().to_string()));
    let profile = Profile::new(check)?;
    let result = spawn(check, None, &stack, &profile.0, &env, &[])?.finish(&cx, CHILD_LIMIT).await;
    // The child (and the peer it started) have gone: their streams are closed.
    sink.unload()?;
    cx.log("null sink unloaded; the default devices are as they were");
    result
}

/// `decibell-headless` next to this binary.
fn headless_exe() -> Result<PathBuf> {
    let candidate = std::env::current_exe()?.parent().context("no parent")?.join("decibell-headless");
    ensure!(
        candidate.exists(),
        "decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)"
    );
    Ok(candidate)
}

/// A headless client running a script (the account's other device, a peer); its profile lives
/// as long as it.
struct Headless {
    label: String,
    child: std::process::Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
    _profile: Profile,
}

impl Headless {
    fn start(label: &str, steps: &[&str]) -> Result<Headless> {
        let profile = Profile::new(&format!("{label}-{}", random_hex(6)))?;
        let mut child = Command::new(headless_exe()?)
            .arg("--user-data-dir")
            .arg(&profile.0)
            .arg("--timeout")
            .arg("120")
            .args(steps)
            .env("DECIBELL_CENTRAL", env("DECIBELL_CENTRAL")?)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting decibell-headless")?;
        let stdout = child.stdout.take().context("piped")?;
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new().name(format!("settings-{label}")).spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
        Ok(Headless { label: label.to_string(), child, lines, seen: Vec::new(), _profile: profile })
    }

    fn take(&mut self) -> Result<()> {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line.clone());
            if line.starts_with("FAILED") {
                bail!("{}: {line}", self.label);
            }
        }
        Ok(())
    }

    /// Until a line starting with `prefix`; returns it.
    async fn line(&mut self, cx: &TestCx, prefix: &str, limit: Duration) -> Result<String> {
        let started = Instant::now();
        loop {
            self.take()?;
            if let Some(line) = self.seen.iter().find(|l| l.starts_with(prefix)) {
                return Ok(line.clone());
            }
            ensure!(started.elapsed() < limit, "{} never printed {prefix:?} (it printed {:?})", self.label, self.seen);
            cx.sleep(Duration::from_millis(30)).await;
        }
    }

    /// Still running, and nothing failed.
    fn alive(&mut self) -> Result<bool> {
        self.take()?;
        Ok(matches!(self.child.try_wait(), Ok(None)))
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

/// A file this check's headless clients wait for (`wait-file`): created to let them go on.
struct Cue(PathBuf);

impl Cue {
    fn new(name: &str) -> Result<Cue> {
        Ok(Cue(PathBuf::from(env("DECIBELL_USER_DATA_DIR")?).join(format!("cue-{name}"))))
    }

    fn path(&self) -> String {
        self.0.display().to_string()
    }

    fn give(&self) -> Result<()> {
        Ok(std::fs::write(&self.0, b"")?)
    }
}

fn has_toast(cx: &App, severity: Severity, title: &str, body: Option<&str>) -> bool {
    model(cx).toasts.read(cx).toasts().iter().any(|t| t.severity == severity && &*t.title == title && t.body.as_deref() == body)
}

/// Another device of the throwaway account, signed in until `stop` is given.
fn other_device(label: &str, name: &str, password: &str, stop: &Cue) -> Result<Headless> {
    Headless::start(label, &["login-as", name, password, "wait", "logged-in", "print", "up", "wait-file", &stop.path(), "logout", "wait", "logged-out"])
}

// ── settings-devices-sign-out-all (T6) ───────────────────────────────────────────────────────

async fn check_sign_out_all(cx: TestCx) -> Result<()> {
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let name = throwaway_sign_in(&cx, true, &password).await?;
    // Two other devices, which stay until central signs them out.
    let devices: Vec<Headless> = (0..2)
        .map(|i| Headless::start(&format!("other-{i}"), &["login-as", &name, &password, "wait", "logged-in", "print", "up", "wait", "logged-out", "print", "signed-out"]))
        .collect::<Result<_>>()?;
    let mut devices = devices;
    for d in &mut devices {
        d.line(&cx, "MARK up", Duration::from_secs(60)).await?;
    }
    open_on(&cx, TabId::Devices).await?;
    wait_for(&cx, Duration::from_secs(20), "two other devices in the list", |cx| {
        device_ids(cx).is_some_and(|l| l.iter().filter(|d| !d.1).count() == 2)
    })
    .await?;
    cx.log("two other devices listed: Sign out of all other devices");
    click_probe(&cx, "devices-sign-out-all").await?;
    wait_for(&cx, Duration::from_secs(5), "the confirmation", |cx| model(cx).live.devices.read(cx).confirming.is_some()).await?;
    // Enter confirms as the dialog opens (nothing in it focused).
    cx.settle().await?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(20), "the toast", |cx| {
        has_toast(cx, Severity::Success, "Signed out", Some("2 other devices were signed out."))
    })
    .await?;
    wait_for(&cx, Duration::from_secs(20), "only this device in the list", |cx| device_ids(cx).is_some_and(|l| l.iter().all(|d| d.1))).await?;
    for d in &mut devices {
        d.line(&cx, "MARK signed-out", Duration::from_secs(30)).await?;
    }
    cx.log("toast \"Signed out — 2 other devices were signed out.\"; only this device listed; both others signed out");
    ensure!(cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?, "this device was signed out too");
    for d in devices {
        d.done(&cx).await?;
    }
    log_out(&cx).await
}

// ── settings-new-sign-in-toast (T7) ──────────────────────────────────────────────────────────

async fn check_new_sign_in(cx: TestCx) -> Result<()> {
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let name = throwaway_sign_in(&cx, true, &password).await?;
    let stop = Cue::new("stop")?;
    let mut other = other_device("new-device", &name, &password, &stop)?;
    other.line(&cx, "MARK up", Duration::from_secs(60)).await?;
    let toast = |cx: &App| {
        model(cx).toasts.read(cx).toasts().iter().find(|t| &*t.title == "New sign-in to your account").map(|t| {
            (t.severity, t.body.as_ref().map(|b| b.to_string()).unwrap_or_default(), t.action.as_ref().map(|a| a.label.to_string()))
        })
    };
    wait_for(&cx, Duration::from_secs(20), "the new sign-in toast", move |cx| toast(cx).is_some()).await?;
    let (severity, body, action) = cx.update(move |_, cx| toast(cx))?.context("the toast went")?;
    cx.log(format_args!("toast \"New sign-in to your account\": {body:?}"));
    ensure!(severity == Severity::Warning, "a {severity:?} toast");
    ensure!(body.ends_with(". If this wasn't you, sign that device out and change your password."), "the body reads {body:?}");
    ensure!(action.as_deref() == Some("Review devices"), "its action is {action:?}");
    // The toast's link, clicked where it is drawn (at the window's top: no pane to scroll it into
    // view), in the frame as it is now.
    draw_pending(&cx)?;
    let link = wait_probe(&cx, "toast-action:Review devices").await?;
    cx.click(link.center())?;
    park(&cx)?;
    wait_for(&cx, Duration::from_secs(5), "Settings → Devices", |cx| settings_open(cx) && shown_tab(cx) == Some(TabId::Devices)).await?;
    wait_for(&cx, Duration::from_secs(5), "the toast to go", move |cx| toast(cx).is_none()).await?;
    wait_for(&cx, Duration::from_secs(20), "the new device in the list", |cx| device_ids(cx).is_some_and(|l| l.iter().any(|d| !d.1))).await?;
    cx.log("Review devices: Settings opened on Devices, the new device listed");
    stop.give()?;
    other.done(&cx).await?;
    log_out(&cx).await
}

// ── settings-password-wrong (T8) ─────────────────────────────────────────────────────────────

async fn check_password_wrong(cx: TestCx) -> Result<()> {
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let new = env("DECIBELL_TEST_NEW_PASSWORD")?;
    let name = throwaway_sign_in(&cx, true, &password).await?;
    let stop = Cue::new("stop")?;
    let mut other = other_device("other", &name, &password, &stop)?;
    other.line(&cx, "MARK up", Duration::from_secs(60)).await?;
    open_on(&cx, TabId::Account).await?;
    click_probe(&cx, "account-password").await?;
    wait_for(&cx, Duration::from_secs(5), "the password dialog", |cx| model(cx).live.devices.read(cx).password.is_some()).await?;
    let error = |cx: &App| model(cx).live.devices.read(cx).password.clone().and_then(|p| if p.busy { None } else { p.error });
    cx.type_text("not-the-current-password")?;
    cx.keystroke("tab")?;
    cx.type_text(&new)?;
    cx.keystroke("tab")?;
    cx.type_text(&new)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(20), "central's refusal", move |cx| error(cx).is_some()).await?;
    let refused = cx.update(move |_, cx| error(cx))?.unwrap_or_default();
    cx.log(format_args!("a wrong current password: {refused:?} in the dialog"));
    ensure!(&*refused == "Wrong password.", "the dialog says {refused:?}");
    cx.sleep(Duration::from_secs(1)).await;
    ensure!(cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?, "this device was signed out");
    ensure!(other.alive()?, "the other device was signed out");
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "the dialog to close", |cx| model(cx).live.devices.read(cx).password.is_none()).await?;
    open_on(&cx, TabId::Devices).await?;
    wait_for(&cx, Duration::from_secs(20), "the other device still listed", |cx| device_ids(cx).is_some_and(|l| l.iter().any(|d| !d.1))).await?;
    cx.log("nothing signed out: this device and the other one stay");
    stop.give()?;
    other.done(&cx).await?;
    // The old password still signs in (log out, sign in with it).
    log_out(&cx).await?;
    let (n, p) = (name.clone(), password.clone());
    cx.update(move |_, cx| actions::auth::sign_in(n, p, cx))?;
    signed_in(&cx).await?;
    cx.log("the unchanged password still signs in");
    log_out(&cx).await
}

// ── settings-rename-refusals (T9) ────────────────────────────────────────────────────────────

const COMMUNITY: &str = "Design Lab";

fn account_tab(cx: &App) -> Option<gpui::Entity<crate::tabs::account::AccountTab>> {
    crate::handles::settings(cx).and_then(|o| o.read(cx).view().map(|v| v.view())).and_then(|v| v.downcast::<crate::tabs::account::AccountTab>().ok())
}

/// Our own member row's name in the community, as the community tells it.
fn community_name(cx: &App) -> Option<String> {
    let m = model(cx);
    let uid = m.auth.read(cx).uid;
    let server = m.servers.read(cx).servers().iter().find(|s| &*s.name == COMMUNITY)?.id;
    let community = m.community(server, cx)?;
    let roster = community.roster.read(cx);
    roster.member(uid).map(|row| row.username.to_string())
}

/// Rename through the dialog: `name` and the password, Enter; the outcome (`Ok(new name)` or the
/// dialog's error).
async fn rename_to(cx: &TestCx, name: &str, password: &str) -> Result<std::result::Result<(), String>> {
    // A dialog that just closed takes every click for its 300 ms fade-out.
    cx.sleep(Duration::from_millis(400)).await;
    draw_pending(cx)?;
    click_probe(cx, "account-rename").await?;
    wait_for(cx, Duration::from_secs(5), "the rename dialog", |cx| account_tab(cx).is_some_and(|a| a.read(cx).rename.open)).await?;
    cx.type_text(name)?;
    cx.keystroke("tab")?;
    cx.type_text(password)?;
    cx.keystroke("enter")?;
    let settled = |cx: &mut App| account_tab(cx).is_some_and(|a| {
        let f = &a.read(cx).rename;
        !f.busy && (!f.open || f.error.is_some())
    });
    wait_for(cx, Duration::from_secs(20), "the rename's answer", settled).await?;
    let error = cx.update(|_, cx| account_tab(cx).and_then(|a| a.read(cx).rename.error.clone()))?;
    match error {
        Some(error) => {
            // Cancel the dialog for the next try.
            cx.keystroke("escape")?;
            wait_for(cx, Duration::from_secs(5), "the dialog to close", |cx| account_tab(cx).is_some_and(|a| !a.read(cx).rename.open)).await?;
            Ok(Err(error.to_string()))
        }
        None => Ok(Ok(())),
    }
}

async fn check_rename_refusals(cx: TestCx) -> Result<()> {
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let first = throwaway_sign_in(&cx, true, &password).await?;
    let uid = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    // In a community (Browse's Join of a public one).
    let target = cx.update(|_, cx| {
        model(cx).servers.read(cx).discoverable().iter().find(|s| &*s.name == COMMUNITY).map(|s| (s.id, s.host.to_string(), s.port))
    })?;
    let target = match target {
        Some(t) => t,
        None => {
            wait_for(&cx, Duration::from_secs(20), "central's directory", |cx| {
                model(cx).servers.read(cx).discoverable().iter().any(|s| &*s.name == COMMUNITY)
            })
            .await?;
            cx.update(|_, cx| {
                model(cx).servers.read(cx).discoverable().iter().find(|s| &*s.name == COMMUNITY).map(|s| (s.id, s.host.to_string(), s.port))
            })?
            .context("gone")?
        }
    };
    cx.update(move |_, cx| actions::servers::connect(target.0, target.1, target.2, cx, |_, _| {}))?;
    wait_for(&cx, Duration::from_secs(30), "our row in the community", |cx| community_name(cx).is_some()).await?;
    let next = env("DECIBELL_TEST_RENAME")?;
    let (n1, n2, n3) = (format!("{next}1"), format!("{next}2"), format!("{next}3"));
    // A watcher of ours: another throwaway account that registers itself.
    let watcher_name = format!("w{}", random_hex(10));
    let w = format!("#{uid}");
    let mut watcher = Headless::start(
        "watcher",
        &["register", &watcher_name, &password, "login-as", &watcher_name, &password, "wait", "logged-in", "watch", &w, "print", "watching",
          "wait", "name", &w, &n1, "print", "saw-1", "wait", "name", &w, &n2, "print", "saw-2", "logout", "wait", "logged-out"],
    )?;
    watcher.line(&cx, "MARK watching", Duration::from_secs(60)).await?;
    open_on(&cx, TabId::Account).await?;

    let taken = rename_to(&cx, "perf1", &password).await?;
    ensure!(taken == Err("That username is taken.".to_string()), "renaming to perf1: {taken:?}");
    cx.log("a taken name: \"That username is taken.\"");
    for (i, name) in [&n1, &n2].into_iter().enumerate() {
        let renamed = rename_to(&cx, name, &password).await?;
        ensure!(renamed.is_ok(), "rename {} to {name}: {renamed:?}", i + 1);
        let want = name.clone();
        wait_for(&cx, Duration::from_secs(20), "the community's row to follow", move |cx| community_name(cx).as_deref() == Some(want.as_str())).await?;
        watcher.line(&cx, &format!("MARK saw-{}", i + 1), Duration::from_secs(30)).await?;
        cx.log(format_args!("rename {}: {name}; the community and the watcher follow live", i + 1));
    }
    let third = rename_to(&cx, &n3, &password).await?;
    ensure!(third == Err("You can change your username twice a day. Try again tomorrow.".to_string()), "a third rename: {third:?}");
    cx.log("a third rename in a day: \"You can change your username twice a day. Try again tomorrow.\"");
    watcher.done(&cx).await?;
    // The first name is reserved for the account and still signs it in (as its current name).
    let old = Headless::start("old-name", &["login-as", &first, &password, "wait", "logged-in", "dump", "auth", "logout", "wait", "logged-out"])?;
    let seen = old.done(&cx).await?;
    let auth = seen.iter().find(|l| l.starts_with("DUMP auth ")).context("no auth dump")?;
    ensure!(auth.contains(&format!("\"username\":\"{n2}\"")), "signing in as {first}: {auth}");
    cx.log(format_args!("{first} still signs in: as {n2}"));
    // And it is the account's: nobody else can take it meanwhile.
    let mut squatter = Headless::start("squatter", &["register", &first, &password])?;
    // A FAILED line ends `line` with it as the error.
    let refused = match squatter.line(&cx, "FAILED", Duration::from_secs(60)).await {
        Err(e) => e.to_string(),
        Ok(line) => line,
    };
    ensure!(refused.contains("Username or email already exists."), "registering {first} again: {refused}");
    cx.log(format_args!("registering {first} for another account: refused (reserved for 14 days)"));
    let server = target.0;
    cx.update(move |_, cx| actions::servers::leave(server, cx))?;
    cx.sleep(Duration::from_millis(500)).await;
    log_out(&cx).await
}

// ── settings-log-out (T10) ───────────────────────────────────────────────────────────────────

async fn check_log_out(cx: TestCx) -> Result<()> {
    let sink = std::env::var(CUE_SINK_ENV).ok().filter(|s| !s.is_empty()).context("no null sink for the call")?;
    let monitor = format!("{sink}.monitor");
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let name = throwaway_sign_in(&cx, true, &password).await?;
    let uid = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    // The call's devices: the parent's null sink, muted and deafened.
    let (s, m) = (sink.clone(), monitor.clone());
    cx.update(move |_, cx| {
        actions::voice::set_input_device(Some(m), cx);
        actions::voice::set_output_device(Some(s), cx);
        actions::voice::set_mute(true, cx);
        actions::voice::set_deafen(true, cx);
    })?;
    let (device, _) = {
        open_on(&cx, TabId::Devices).await?;
        wait_for(&cx, Duration::from_secs(20), "the device list", |cx| device_ids(cx).is_some()).await?;
        let current = cx.update(|_, cx| device_ids(cx).and_then(|l| l.into_iter().find(|d| d.1)))?.context("no current device")?;
        cx.keystroke("escape")?;
        current
    };
    let listed = Cue::new("listed")?;
    let mut other = Headless::start(
        "other",
        &["login-as", &name, &password, "wait", "logged-in", "print", "up", "wait-file", &listed.path(), "devices", "wait", "devices", "dump", "devices", "logout", "wait", "logged-out"],
    )?;
    other.line(&cx, "MARK up", Duration::from_secs(60)).await?;
    // The peer: another throwaway that calls us, on the same sink.
    let peer_name = format!("c{}", random_hex(10));
    let me = format!("#{uid}");
    let mut peer = Headless::start(
        "peer",
        &["register", &peer_name, &password, "login-as", &peer_name, &password, "wait", "logged-in", "audio-devices", &monitor, &sink, "mute", "deafen",
          "call", &me, "wait", "call", "active", "print", "active", "wait", "call", "idle", "print", "ended", "logout", "wait", "logged-out"],
    )?;
    wait_for(&cx, Duration::from_secs(60), "the incoming call", |cx| model(cx).live.calls.read(cx).status == CallStatus::Incoming).await?;
    cx.update(|_, cx| actions::calls::accept_call(cx))?;
    wait_for(&cx, Duration::from_secs(30), "the call to connect", |cx| model(cx).live.calls.read(cx).status == CallStatus::Active).await?;
    peer.line(&cx, "MARK active", Duration::from_secs(30)).await?;
    cx.log(format_args!("in a call with {peer_name}: Settings → Account → Log Out"));
    open_on(&cx, TabId::Account).await?;
    click_probe(&cx, "account-log-out").await?;
    wait_for(&cx, Duration::from_secs(20), "the sign-in page", |cx| {
        !model(cx).auth.read(cx).is_authenticated && probes::flag_set("page-sign-in")
    })
    .await?;
    peer.line(&cx, "MARK ended", Duration::from_secs(30)).await?;
    cx.log("signed out; the peer saw the call end");
    listed.give()?;
    let seen = other.done(&cx).await?;
    let dump = seen.iter().find(|l| l.starts_with("DUMP devices ")).context("no device dump")?;
    ensure!(!dump.contains(&format!("\"id\":{device},")), "the account still lists this device {device}: {dump}");
    cx.log(format_args!("the account's other device lists no device {device} any more"));
    peer.done(&cx).await?;
    Ok(())
}
