//! `session-lock-away` (parity audit A, T15): the screen lock makes us Away at once and the unlock
//! brings us back (`useAutoAway`'s `lock-screen` / `unlock-screen`), and the account shows Away
//! only while every device of it is idle.
//!
//! The lock is `org.freedesktop.ScreenSaver.ActiveChanged(bool)`, emitted with `dbus-send` on a
//! **private** session bus the app child is started on (never the desktop's bus or its locker).
//! Two throwaway accounts, registered by `decibell-headless`: A is the app (and later a headless
//! second device), B a headless watcher of A. The app's auto-away threshold is set to 60 min so
//! that the desktop's own idle state can't intervene during the test.

use std::process::Command;

use super::*;

/// Registers `name` with a headless client on the private bus (central's sign-up bucket is shared
/// with every test of the stack: waited out, up to 5 min).
async fn register(cx: &TestCx, headless: &Path, stack: &Stack, bus: &EmptyBus, dir: &Path, name: &str, password: &str) -> Result<()> {
    let started = Instant::now();
    loop {
        let profile = dir.join(format!("register-{name}"));
        let steps = ["--user-data-dir", &profile.display().to_string(), "--timeout", "60", "register", name, password];
        let env = peer_env(stack, bus);
        let mut client = Child::spawn(&format!("register-{name}"), headless, &steps, &env, &[], dir)?;
        let status = client.wait_exit(cx, Duration::from_secs(90)).await?;
        if status.success() {
            return Ok(());
        }
        let tail = client.tail();
        if tail.contains("Too many sign-ups") && started.elapsed() < Duration::from_secs(300) {
            cx.log("central's sign-up bucket is empty (shared with other tests): waiting for a token");
            cx.sleep(Duration::from_secs(30)).await;
            continue;
        }
        bail!("registering {name} failed:\n{tail}");
    }
}

fn peer_env(stack: &Stack, bus: &EmptyBus) -> Vec<(&'static str, String)> {
    vec![("DECIBELL_CENTRAL", stack.central.clone()), ("DECIBELL_DEV_PASSWORD", stack.password.clone()), ("DBUS_SESSION_BUS_ADDRESS", bus.address.clone())]
}

/// `ActiveChanged(active)` on the private bus, as KDE's locker sends it.
fn screensaver(bus: &EmptyBus, active: bool) -> Result<()> {
    let status = Command::new("dbus-send")
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .args(["--session", "--type=signal", "/ScreenSaver", "org.freedesktop.ScreenSaver.ActiveChanged", &format!("boolean:{active}")])
        .status()
        .context("dbus-send")?;
    ensure!(status.success(), "dbus-send failed ({status})");
    Ok(())
}

pub(super) async fn parent(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let Some(headless) = beside("decibell-headless") else {
        cx.log("SKIP: decibell-headless is not built beside this binary");
        return Ok(());
    };
    let dir = TempDir::new("session-lock")?;
    let bus = EmptyBus::start(&dir.0)?;
    let tag = format!("{:x}", std::process::id() ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)));
    let (a, b, password) = (format!("sl{tag}a"), format!("sl{tag}b"), format!("session-lock-{tag}-pw"));
    cx.log(format_args!("registering the throwaway accounts {a} and {b}"));
    register(&cx, &headless, &stack, &bus, &dir.0, &a, &password).await?;
    register(&cx, &headless, &stack, &bus, &dir.0, &b, &password).await?;

    let profile = dir.0.join("app");
    let mut env = app_env(&profile, &bus, Some(&stack));
    env.push(("DECIBELL_PLATFORM_TEST_DIR", dir.0.display().to_string()));
    env.push(("DECIBELL_SL_NAME", a.clone()));
    env.push(("DECIBELL_SL_PASSWORD", password.clone()));
    let mut app = Child::spawn("app", &exe()?, &["--check", "platform-session-lock"], &env, &[], &dir.0)?;
    let ready = app.wait_line(&cx, "platform-check: ready", Duration::from_secs(60)).await?;
    let me: i64 = ready.rsplit(' ').next().and_then(|s| s.parse().ok()).context("A's uid")?;
    let cue = |name: &str| dir.0.join(format!("cue-{name}")).display().to_string();
    let watched = format!("#{me}");

    let b_profile = dir.0.join("b");
    let b_steps: Vec<String> = [
        "--user-data-dir", &b_profile.display().to_string(), "--timeout", "60",
        "login-as", &b, &password, "wait", "logged-in", "watch", &watched,
        "wait", "presence", &watched, "online", "print", "b-online",
        "wait", "presence", &watched, "away", "print", "b-away",
        "wait", "presence", &watched, "online", "print", "b-back",
        "wait", "presence", &watched, "away", "print", "b-away2",
        "wait", "presence", &watched, "online", "print", "b-back2",
        "wait-file", &cue("done"), "logout", "wait", "logged-out",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let b_steps: Vec<&str> = b_steps.iter().map(String::as_str).collect();
    let mut watcher = Child::spawn("b", &headless, &b_steps, &peer_env(&stack, &bus), &[], &dir.0)?;
    watcher.wait_line(&cx, "MARK b-online", Duration::from_secs(60)).await?;

    // One device: the lock is Away at once, for others too; the unlock is back.
    screensaver(&bus, true)?;
    app.wait_line(&cx, "decibell: platform session locked", Duration::from_secs(10)).await?;
    let away = app.wait_line(&cx, "platform-check: away", Duration::from_secs(10)).await?;
    let started = Instant::now();
    watcher.wait_line(&cx, "MARK b-away", Duration::from_secs(20)).await?;
    cx.log(format_args!("locked: {} ; the watcher saw Away {} ms later", away.trim_start_matches("platform-check: "), started.elapsed().as_millis()));
    screensaver(&bus, false)?;
    app.wait_line(&cx, "decibell: platform session unlocked", Duration::from_secs(10)).await?;
    app.wait_line(&cx, "platform-check: back", Duration::from_secs(10)).await?;
    watcher.wait_line(&cx, "MARK b-back", Duration::from_secs(20)).await?;
    cx.log("unlocked: back, for the watcher too");

    // A second device of A, active: the lock makes this device idle but not the account.
    let a2_profile = dir.0.join("a2");
    let a2_steps: Vec<String> = [
        "--user-data-dir", &a2_profile.display().to_string(), "--timeout", "60",
        "login-as", &a, &password, "wait", "logged-in", "print", "a2-ready",
        "wait-file", &cue("a2-idle"), "device-idle", "on", "print", "a2-idle",
        "wait-file", &cue("done"), "logout", "wait", "logged-out",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let a2_steps: Vec<&str> = a2_steps.iter().map(String::as_str).collect();
    let mut a2 = Child::spawn("a2", &headless, &a2_steps, &peer_env(&stack, &bus), &[], &dir.0)?;
    a2.wait_line(&cx, "MARK a2-ready", Duration::from_secs(60)).await?;
    std::fs::write(dir.0.join("cue-second-device"), b"")?;
    screensaver(&bus, true)?;
    app.wait_line(&cx, "decibell: platform session locked", Duration::from_secs(10)).await?;
    let shown = app.wait_line(&cx, "platform-check: device idle", Duration::from_secs(15)).await?;
    cx.sleep(Duration::from_secs(3)).await;
    ensure!(!watcher.printed("MARK b-away2"), "the watcher saw Away while A's other device was active");
    ensure!(shown.ends_with("shown Online"), "{shown}");
    cx.log(format_args!("locked with another device active: {}; the watcher still sees Online after 3 s", shown.trim_start_matches("platform-check: ")));
    std::fs::write(dir.0.join("cue-a2-idle"), b"")?;
    a2.wait_line(&cx, "MARK a2-idle", Duration::from_secs(20)).await?;
    app.wait_line(&cx, "platform-check: account idle", Duration::from_secs(20)).await?;
    watcher.wait_line(&cx, "MARK b-away2", Duration::from_secs(20)).await?;
    cx.log("the other device idle too: the account is Away, here and for the watcher");
    screensaver(&bus, false)?;
    app.wait_line(&cx, "platform-check: back2", Duration::from_secs(15)).await?;
    watcher.wait_line(&cx, "MARK b-back2", Duration::from_secs(20)).await?;
    cx.log("unlocked: this device is active again, so the account is back");

    std::fs::write(dir.0.join("cue-done"), b"")?;
    let status = app.wait_exit(&cx, Duration::from_secs(60)).await?;
    {
        let tail = app.tail();
        ensure!(status.success(), "the app's check failed ({status}):\n{tail}");
    }
    for (name, child) in [("the watcher", &mut watcher), ("A's other device", &mut a2)] {
        let status = child.wait_exit(&cx, Duration::from_secs(30)).await?;
        let tail = child.tail();
        ensure!(status.success(), "{name} failed ({status}):\n{tail}");
    }
    Ok(())
}

pub(super) async fn check(cx: TestCx) -> Result<()> {
    use decibell_model::live::presence::DisplayStatus;
    let dir = test_dir()?;
    let var = |k: &str| std::env::var(k).with_context(|| format!("{k} is not set"));
    let (name, password) = (var("DECIBELL_SL_NAME")?, var("DECIBELL_SL_PASSWORD")?);
    wait_for(&cx, Duration::from_secs(30), "the app to boot", |cx| model(cx).auth.read(cx).booted).await?;
    cx.update(move |_, cx| decibell_model::actions::auth::sign_in(name, password, cx))?;
    wait_for(&cx, Duration::from_secs(60), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    wait_for(&cx, Duration::from_secs(20), "central's statuses", |cx| model(cx).live.presence.own.read(cx).supported).await?;
    // A threshold the desktop's own idle time can't reach while the test runs.
    cx.update(|_, cx| model(cx).settings.update(cx, |s, cx| s.change(|v| v.away_after_minutes = 60, cx)))?;
    let me = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    say(format_args!("ready {me}"));
    let own = |cx: &mut App| {
        let o = model(cx).live.presence.own.read(cx);
        (o.idle, o.display())
    };
    wait_for(&cx, Duration::from_secs(60), "the lock", move |cx| own(cx).0).await?;
    wait_for(&cx, Duration::from_secs(10), "Away shown", move |cx| own(cx).1 == DisplayStatus::Away).await?;
    say(format_args!("away (this device idle, shown {:?})", cx.update(move |_, cx| own(cx).1)?));
    wait_for(&cx, Duration::from_secs(60), "the unlock", move |cx| !own(cx).0).await?;
    wait_for(&cx, Duration::from_secs(10), "Online shown", move |cx| own(cx).1 == DisplayStatus::Online).await?;
    say(format_args!("back"));
    wait_file(&cx, &dir.join("cue-second-device"), Duration::from_secs(120)).await?;
    wait_for(&cx, Duration::from_secs(60), "the second lock", move |cx| own(cx).0).await?;
    // Central answers for the account: give it a moment to have said "not idle".
    cx.sleep(Duration::from_secs(1)).await;
    say(format_args!("device idle, shown {:?}", cx.update(move |_, cx| own(cx).1)?));
    wait_for(&cx, Duration::from_secs(30), "the account idle", move |cx| own(cx).1 == DisplayStatus::Away).await?;
    say(format_args!("account idle"));
    wait_for(&cx, Duration::from_secs(60), "the second unlock", move |cx| !own(cx).0 && own(cx).1 == DisplayStatus::Online).await?;
    say(format_args!("back2"));
    wait_file(&cx, &dir.join("cue-done"), Duration::from_secs(60)).await?;
    cx.update(|_, cx| decibell_model::actions::auth::log_out(cx))?;
    wait_for(&cx, Duration::from_secs(20), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}
