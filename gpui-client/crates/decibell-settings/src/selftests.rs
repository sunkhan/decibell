//! Settings' self-tests, against the local test stack (`tools/dev/README.md`).
//!
//! Each `settings-*` self-test starts the app again in a child process (`decibell --check <name>`,
//! its own temporary profile, the stack's `DECIBELL_CENTRAL`, `DECIBELL_TEST_PROBES=1`), as the
//! shell's do, and passes when the check inside passes; some run two children (a second device of
//! the account) or the same profile twice (a restart). They refuse unless `DECIBELL_CENTRAL` is a
//! loopback address and skip, saying so, when the stack is down or the binary has no `--login`
//! (`dev-local`).
//!
//! Accounts: `perf1` for what changes nothing on the account (or puts it back: friends-only DMs);
//! a **throwaway account** registered for the run (`t<random>`) for the password, rename and E2EE
//! flows, which must never touch the stack's seeded accounts. A check that registers refuses
//! unless this build's core honours `DECIBELL_CENTRAL` (a debug build, or `dev-local`).
//!
//! The checks click the real window where the probes say (`decibell_harness::probes`: the views',
//! the user panel's gear, the sidebar's tabs) and type into the forms' fields as key presses
//! (`TestCx::type_text`: key listeners, then the focused field's input handler), Tab between
//! them and Enter to submit, as a person would.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher as _, Hasher as _};
use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx, test_audio};
use decibell_model::live::e2ee::E2eeStatus;
use decibell_model::live::hotkeys::InputMode;
use decibell_model::stores::overlays::modal;
use decibell_model::{Model, actions};
use decibell_shell::probes;
use decibell_theme::ThemeId;
use decibell_ui::{ActiveTheme as _, visible_window_bounds};
use gpui::{App, Bounds, KeyCode, Keystroke, Modifiers, MouseButton, Pixels, PlatformInput, point, px};

use crate::tabs::{TabId, TabView};

const MARK: &str = "SETTINGS-MARK";

// ── The parents ──────────────────────────────────────────────────────────────────────────────

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "settings-open",
            about: "the user panel's gear opens Settings, tabs switch by clicks, Esc closes: frames only for each, then 0",
            run: |cx| Box::pin(perf1(cx, "settings-open", Phases::One)),
        },
        SelfTest {
            name: "settings-appearance-apply",
            about: "a theme switch is one frame; theme, text size and density apply live and outlive a restart",
            run: |cx| Box::pin(perf1(cx, "settings-appearance-apply", Phases::Restart)),
        },
        SelfTest {
            name: "settings-devices-signout",
            about: "a second perf1 device appears in Devices; signing it out removes it and closes that client",
            run: |cx| Box::pin(devices(cx)),
        },
        SelfTest {
            name: "settings-password",
            about: "throwaway account: change the password, the other device is signed out, this one stays; the new one signs in",
            run: |cx| Box::pin(password(cx)),
        },
        SelfTest {
            name: "settings-rename",
            about: "throwaway account: rename through the dialog; the account, the names store and the shell follow",
            run: |cx| Box::pin(throwaway(cx, "settings-rename", 1)),
        },
        SelfTest {
            name: "settings-privacy-dms",
            about: "friends-only DMs toggles, settles on the account's echo and outlives a restart (then put back)",
            run: |cx| Box::pin(perf1(cx, "settings-privacy-dms", Phases::Restart)),
        },
        SelfTest {
            name: "settings-e2ee",
            about: "throwaway account: set up encryption; a fresh profile is locked, a wrong passphrase refused, the right one unlocks",
            run: |cx| Box::pin(throwaway(cx, "settings-e2ee", 2)),
        },
        SelfTest {
            name: "settings-audio-meter",
            about: "device lists load and follow a device appearing / going (no poll); the meter holds the input-level reporting only while shown; on a null sink: the mic test meters a cue and the looping ring and rests at -96, no thread stays; the input mode outlives a restart",
            run: |cx| Box::pin(perf1(cx, "settings-audio-meter", Phases::Restart)),
        },
        SelfTest {
            name: "settings-keybinds-record",
            about: "record bindings from physical key codes (X11 listener): under German and Turkish layouts the physical key, modifiers alone with their side, a mouse button; Escape cancels, a conflict is named, remove; saved",
            run: |cx| Box::pin(keybinds(cx)),
        },
        SelfTest {
            name: "settings-network-caps",
            about: "a speed preset and a typed custom cap apply and are saved",
            run: |cx| Box::pin(perf1(cx, "settings-network-caps", Phases::One)),
        },
        SelfTest {
            name: "settings-network-leave",
            about: "a typed custom speed never holds the window (no save bar, as the renderer): a press on another tab applies it (onBlur) and opens that tab; Esc and ✕ apply it and close",
            run: |cx| Box::pin(perf1(cx, "settings-network-leave", Phases::One)),
        },
        SelfTest {
            name: "settings-idle",
            about: "the window open on each tab: 0 frames drawn over 5 s",
            run: |cx| Box::pin(perf1(cx, "settings-idle", Phases::One)),
        },
        SelfTest {
            name: "settings-close-ways",
            about: "the pane's ✕ and a press on the backdrop close Settings (as Esc does: settings-open); About shows a failed-update notice and opening it marks the notice read",
            run: |cx| Box::pin(perf1(cx, "settings-close-ways", Phases::One)),
        },
    ]
    .into_iter()
    .chain(accounts::selftests())
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

fn is_loopback(central: &str) -> bool {
    let host = central.rsplit_once(':').map_or(central, |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn stack() -> Result<Option<Stack>> {
    let path = dev_dir().join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let central = std::env::var("DECIBELL_CENTRAL")
        .ok()
        .or_else(|| json["central"].as_str().map(str::to_string))
        .context("no central address")?;
    let password = json["password"].as_str().context("accounts.json has no password")?.to_string();
    ensure!(is_loopback(&central), "DECIBELL_CENTRAL={central} is not loopback: Settings' tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
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

fn spawn(check: &str, login: Option<&str>, stack: &Stack, profile: &Path, env: &[(&str, String)], remove: &[&str]) -> Result<Running> {
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
    for (stream, tag) in [(child.stdout.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), ""), (child.stderr.take().map(|s| Box::new(s) as Box<dyn std::io::Read + Send>), "stderr: ")] {
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

    fn no_dev_login(&self) -> bool {
        self.has("--login refused") && self.has("dev-local")
    }

    /// Wait for `MARK <mark>` from the child (or its end).
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
        let stderr: Vec<&String> = lines.iter().filter(|l| l.starts_with("stderr: ")).collect();
        for line in lines.iter().filter(|l| !l.starts_with("stderr: ") && !l.starts_with("selftest:")) {
            cx.log(format_args!("│ {}: {line}", self.name));
        }
        if errors {
            for line in stderr.iter().rev().take(16).rev() {
                cx.log(format_args!("│ {}: {line}", self.name));
            }
        }
    }

    /// Wait for the child to end; it must pass.
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

/// The sink a check may play on (`decibell-app/src/audio.rs`): set only by the test that made one.
const CUE_SINK_ENV: &str = "DECIBELL_CUE_SINK";

const CHILD_LIMIT: Duration = Duration::from_secs(180);

#[derive(Clone, Copy, PartialEq)]
enum Phases {
    One,
    /// The same profile again, signed in by the saved sign-in.
    Restart,
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

/// One child (two with a restart) signed in as perf1.
async fn perf1(cx: TestCx, check: &'static str, phases: Phases) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let profile = Profile::new(check)?;
    // The sound part plays on a null sink and meters its monitor, never the desktop's devices.
    // This process owns the sink and unloads it after the child has gone (declared before the
    // child: dropped after it), so no stream of the child can be moved to the real output.
    let mut env = vec![("DECIBELL_CHECK_PHASE", "1".to_string())];
    let sink = if check == "settings-audio-meter" {
        match test_audio::NullSink::load("") {
            Ok(sink) => {
                cx.log(format_args!("null sink {} (module {}) for the sound part", sink.name(), sink.module()));
                env.push((CUE_SINK_ENV, sink.name().to_string()));
                Some(sink)
            }
            Err(e) => {
                cx.log(format_args!("no null sink ({e:#}): the sound part is skipped"));
                None
            }
        }
    } else {
        None
    };
    // Only the sink made here is ever named to the child.
    let stale: &[&str] = if sink.is_some() { &[] } else { &[CUE_SINK_ENV] };
    let env: Vec<(&str, String)> = env;
    let first = spawn(check, Some("perf1"), &stack, &profile.0, &env, stale)?;
    let first = wait_end_or_skip(&cx, first).await?;
    if let Some(sink) = sink {
        sink.unload()?;
        cx.log("null sink unloaded; the default devices are as they were");
    }
    if !first {
        return Ok(());
    }
    if phases == Phases::Restart {
        cx.log("restarting on the same profile (the saved sign-in signs in)");
        let second = spawn(check, None, &stack, &profile.0, &[("DECIBELL_CHECK_PHASE", "2".into())], &[CUE_SINK_ENV])?;
        second.finish(&cx, CHILD_LIMIT).await?;
    }
    Ok(())
}

/// Wait for a child; `false` when the binary has no `--login` (skipped, said so).
async fn wait_end_or_skip(cx: &TestCx, mut running: Running) -> Result<bool> {
    loop {
        if running.child.try_wait()?.is_some() {
            break;
        }
        ensure!(running.started.elapsed() < CHILD_LIMIT, "{} still running after {} s", running.name, CHILD_LIMIT.as_secs());
        cx.sleep(Duration::from_millis(100)).await;
    }
    if running.no_dev_login() {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(false);
    }
    running.finish(cx, CHILD_LIMIT).await?;
    Ok(true)
}

/// A throwaway account for one run: never one of the stack's.
fn throwaway_env() -> Vec<(&'static str, String)> {
    let name = format!("t{}", random_hex(10));
    vec![
        ("DECIBELL_TEST_ACCOUNT", name.clone()),
        ("DECIBELL_TEST_EMAIL", format!("{name}@example.test")),
        ("DECIBELL_TEST_PASSWORD", format!("pw-{}", random_hex(12))),
        ("DECIBELL_TEST_NEW_PASSWORD", format!("pw2-{}", random_hex(12))),
        ("DECIBELL_TEST_PASSPHRASE", format!("settings-test-{}", random_hex(12))),
        ("DECIBELL_TEST_RENAME", format!("t{}r", random_hex(9))),
    ]
}

/// A throwaway account's check: phase 1 registers; each later phase starts on a fresh profile.
async fn throwaway(cx: TestCx, check: &'static str, phases: u32) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let env = throwaway_env();
    cx.log(format_args!("throwaway account {}", env[0].1));
    for phase in 1..=phases {
        let profile = Profile::new(&format!("{check}-{phase}"))?;
        let mut env = env.clone();
        env.push(("DECIBELL_CHECK_PHASE", phase.to_string()));
        if phase > 1 {
            cx.log(format_args!("phase {phase}: a fresh profile (another device)"));
        }
        spawn(check, None, &stack, &profile.0, &env, &[])?.finish(&cx, CHILD_LIMIT).await?;
    }
    Ok(())
}

/// Devices: perf1 here, and a second perf1 device (another child) that this one signs out.
async fn devices(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let main_profile = Profile::new("settings-devices-signout")?;
    let peer_profile = Profile::new("settings-devices-signout-peer")?;
    let mut main = spawn("settings-devices-signout", Some("perf1"), &stack, &main_profile.0, &[], &[])?;
    match main.wait_mark(&cx, "ready", Duration::from_secs(60)).await {
        Ok(()) => {}
        Err(e) if main.no_dev_login() => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            let _ = e;
            return Ok(());
        }
        Err(e) => return Err(e),
    }
    cx.log("the Devices tab is open: starting a second perf1 device");
    let mut peer = spawn("settings-peer", Some("perf1"), &stack, &peer_profile.0, &[], &[])?;
    peer.wait_mark(&cx, "peer-signed-in", Duration::from_secs(60)).await?;
    main.finish(&cx, CHILD_LIMIT).await?;
    peer.wait_mark(&cx, "peer-signed-out", Duration::from_secs(60)).await?;
    peer.finish(&cx, Duration::from_secs(60)).await?;
    Ok(())
}

/// Password: a throwaway account on this device and a second one; changing the password here
/// signs the other out.
async fn password(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let env = throwaway_env();
    cx.log(format_args!("throwaway account {}", env[0].1));
    let main_profile = Profile::new("settings-password")?;
    let peer_profile = Profile::new("settings-password-peer")?;
    let mut main = spawn("settings-password", None, &stack, &main_profile.0, &env, &[])?;
    main.wait_mark(&cx, "signed-in", Duration::from_secs(60)).await?;
    cx.log("registered and signed in: starting the account's second device");
    let mut peer = spawn("settings-peer", None, &stack, &peer_profile.0, &env, &[])?;
    peer.wait_mark(&cx, "peer-signed-in", Duration::from_secs(60)).await?;
    main.finish(&cx, CHILD_LIMIT).await?;
    peer.wait_mark(&cx, "peer-signed-out", Duration::from_secs(60)).await?;
    peer.finish(&cx, Duration::from_secs(60)).await?;
    Ok(())
}

/// Keybinds: perf1 under XWayland, so the listener is X11's (passive grabs on exactly the bound
/// combos, released when the app exits) and no desktop shortcut dialog opens.
async fn keybinds(cx: TestCx) -> Result<()> {
    let stack = skip_without_stack!(cx);
    let profile = Profile::new("settings-keybinds-record")?;
    let running = spawn("settings-keybinds-record", Some("perf1"), &stack, &profile.0, &[], &["WAYLAND_DISPLAY"])?;
    wait_end_or_skip(&cx, running).await?;
    Ok(())
}

// ── The checks, inside the app ───────────────────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "settings-open", about: "(in the app) open, switch tabs, close", run: |cx| Box::pin(check_open(cx)) },
        SelfTest { name: "settings-appearance-apply", about: "(in the app) themes, text size, density", run: |cx| Box::pin(check_appearance(cx)) },
        SelfTest { name: "settings-devices-signout", about: "(in the app) a second device, signed out", run: |cx| Box::pin(check_devices(cx)) },
        SelfTest { name: "settings-peer", about: "(in the app) a second device of the account until signed out", run: |cx| Box::pin(check_peer(cx)) },
        SelfTest { name: "settings-password", about: "(in the app) change the password", run: |cx| Box::pin(check_password(cx)) },
        SelfTest { name: "settings-rename", about: "(in the app) rename", run: |cx| Box::pin(check_rename(cx)) },
        SelfTest { name: "settings-privacy-dms", about: "(in the app) friends-only DMs", run: |cx| Box::pin(check_privacy(cx)) },
        SelfTest { name: "settings-e2ee", about: "(in the app) set up, lock, unlock", run: |cx| Box::pin(check_e2ee(cx)) },
        SelfTest { name: "settings-audio-meter", about: "(in the app) devices, the meter's hold, the input mode", run: |cx| Box::pin(check_audio(cx)) },
        SelfTest { name: "settings-keybinds-record", about: "(in the app) the recorder", run: |cx| Box::pin(check_keybinds(cx)) },
        SelfTest { name: "settings-network-caps", about: "(in the app) speed caps", run: |cx| Box::pin(check_network(cx)) },
        SelfTest { name: "settings-network-leave", about: "(in the app) leaving Network applies a typed speed", run: |cx| Box::pin(check_network_leave(cx)) },
        SelfTest { name: "settings-idle", about: "(in the app) every tab at rest", run: |cx| Box::pin(check_idle(cx)) },
        SelfTest { name: "settings-close-ways", about: "(in the app) ✕, the backdrop, About's notice", run: |cx| Box::pin(check_close_ways(cx)) },
    ]
    .into_iter()
    .chain(accounts::checks())
    .collect()
}

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

fn mark(what: impl std::fmt::Display) {
    println!("{MARK} {what}");
}

fn phase() -> u32 {
    std::env::var("DECIBELL_CHECK_PHASE").ok().and_then(|p| p.parse().ok()).unwrap_or(1)
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is not set"))
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

/// The core honours `DECIBELL_CENTRAL` in this build (a debug build, or `dev-local`), and it
/// names a loopback address: only then may a check register or sign in by password.
fn local_only() -> Result<()> {
    ensure!(
        cfg!(any(debug_assertions, feature = "dev-local")),
        "this build ignores DECIBELL_CENTRAL (a release build without dev-local): refusing to talk to any server"
    );
    let central = env("DECIBELL_CENTRAL")?;
    ensure!(is_loopback(&central), "DECIBELL_CENTRAL={central} is not loopback: refusing");
    Ok(())
}

/// Register the throwaway account (phase 1) and sign in with `password`.
async fn throwaway_sign_in(cx: &TestCx, register: bool, password: &str) -> Result<String> {
    local_only()?;
    let name = env("DECIBELL_TEST_ACCOUNT")?;
    wait_for(cx, Duration::from_secs(20), "the core to boot", |cx| model(cx).auth.read(cx).booted).await?;
    if register {
        let (n, e, p) = (name.clone(), env("DECIBELL_TEST_EMAIL")?, password.to_string());
        cx.update(move |_, cx| actions::auth::register(n, e, p, cx))?;
        wait_for(cx, Duration::from_secs(20), "the registration's answer", |cx| model(cx).auth.read(cx).register_result.is_some()).await?;
        let result = cx.update(|_, cx| model(cx).auth.read(cx).register_result.clone())?.context("no answer")?;
        ensure!(result.success, "registering {name} was refused: {}", result.message);
        cx.log(format_args!("registered {name}"));
    }
    let (n, p) = (name.clone(), password.to_string());
    cx.update(move |_, cx| actions::auth::sign_in(n, p, cx))?;
    signed_in(cx).await?;
    Ok(name)
}

/// Forget this device at central (Log out) so test devices don't pile up in the account's list.
async fn log_out(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::auth::log_out(cx))?;
    wait_for(cx, Duration::from_secs(15), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}

/// The window's visible area (inside the client-side margin).
fn visible(cx: &TestCx) -> Result<Bounds<Pixels>> {
    cx.update(|window, cx| visible_window_bounds(window, cx))
}

async fn wait_probe(cx: &TestCx, key: &str) -> Result<Bounds<Pixels>> {
    let owned = key.to_string();
    wait_for(cx, Duration::from_secs(10), &format!("{key} on screen"), move |_| probes::bounds(&owned).is_some()).await?;
    probes::bounds(key).context("probe vanished")
}

/// The pointer where nothing reacts to it (the backdrop's corner).
fn park(cx: &TestCx) -> Result<()> {
    let v = visible(cx)?;
    cx.mouse_move(point(v.right() - px(6.), v.bottom() - px(6.)))
}

/// Click a probed element, scrolling the pane under the pointer until it is in view (a probe
/// records where its element is laid out, scrolled out of the pane or not).
async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    let mut bounds = wait_probe(cx, key).await?;
    let v = visible(cx)?;
    let (top, bottom) = (v.top() + px(120.), v.bottom() - px(80.));
    for _ in 0..40 {
        let below = bounds.bottom() > bottom;
        let above = bounds.top() < top;
        if !below && !above {
            break;
        }
        let at = v.center();
        let dy = if below { px(-160.) } else { px(160.) };
        cx.update(move |window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(gpui::ScrollEvent {
                    position: at,
                    delta: gpui::ScrollDelta::Pixels(point(px(0.), dy)),
                    modifiers: Modifiers::default(),
                    touch_phase: gpui::TouchPhase::Moved,
                    source: Default::default(),
                }),
                cx,
            );
        })?;
        cx.sleep(Duration::from_millis(120)).await;
        bounds = probes::bounds(key).context("probe vanished")?;
    }
    cx.click(bounds.center())?;
    park(cx)
}


/// Replace what the focused field holds: select all, then type.
fn retype(cx: &TestCx, text: &str) -> Result<()> {
    cx.keystroke("ctrl-a")?;
    cx.type_text(text)
}

fn shown_tab(cx: &App) -> Option<TabId> {
    crate::handles::settings(cx)?.read(cx).view().map(TabView::tab)
}

/// Open Settings on `tab` the way a test that isn't about opening does.
async fn open_on(cx: &TestCx, tab: TabId) -> Result<()> {
    cx.update(move |_, cx| {
        let m = model(cx);
        m.nav.update(cx, |n, cx| n.set_settings_tab(tab.id(), cx));
        m.overlays.update(cx, |o, cx| o.open_modal(modal::SETTINGS, cx));
    })?;
    wait_for(cx, Duration::from_secs(5), &format!("Settings → {}", tab.label()), move |cx| shown_tab(cx) == Some(tab)).await?;
    Ok(())
}

/// Waits until nothing has been drawn for 1 s.
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

/// One interaction: what it drew, then a second without a drawn frame (three tries: another
/// window taking the focus redraws the whole window).
async fn step(cx: &TestCx, what: &str, act: impl AsyncFnOnce(&TestCx) -> Result<()>, done: impl Fn(&mut App) -> bool) -> Result<Frames> {
    let before = settle_drawn(cx).await?;
    act(cx).await?;
    wait_for(cx, Duration::from_secs(5), what, done).await?;
    let mut after = settle_drawn(cx).await?;
    let frames = after.since(before);
    let mut idle = Frames { drawn: 0, presented: 0 };
    for _ in 0..3 {
        cx.sleep(Duration::from_secs(1)).await;
        idle = cx.frames()?.since(after);
        if idle.drawn == 0 {
            break;
        }
        after = settle_drawn(cx).await?;
    }
    cx.log(format_args!("{what}: {} drawn / {} presented; then {} drawn in 1 s", frames.drawn, frames.presented, idle.drawn));
    ensure!(frames.drawn >= 1, "{what} drew nothing");
    ensure!(idle.drawn == 0, "{what}: frames drawn after it settled");
    Ok(frames)
}

fn settings_open(cx: &App) -> bool {
    model(cx).overlays.read(cx).is_open(modal::SETTINGS)
}

async fn check_open(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    // The main layout and the user panel are drawn.
    settle_drawn(&cx).await?;
    // The gear sits at the window's foot, under no scrolling pane: clicked where it is.
    step(&cx, "the user panel's gear", async |cx| {
        let gear = wait_probe(cx, "user-panel-settings").await?;
        cx.click(gear.center())?;
        park(cx)
    }, |cx| settings_open(cx))
    .await?;
    for tab in [TabId::Appearance, TabId::Audio, TabId::About, TabId::Account] {
        let key = format!("settings-tab:{}", tab.id());
        step(&cx, &format!("the {} tab", tab.label()), async |cx| click_probe(cx, &key).await, move |cx| {
            shown_tab(cx) == Some(tab) && model(cx).nav.read(cx).settings_tab.as_ref() == tab.id()
        })
        .await?;
    }
    step(&cx, "Esc", async |cx| cx.keystroke("escape").map(|_| ()), |cx| !settings_open(cx)).await?;
    // The fade-out drops the tab's view.
    wait_for(&cx, Duration::from_secs(2), "the tab's view to go", |cx| shown_tab(cx).is_none()).await?;
    log_out(&cx).await
}

async fn check_appearance(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    if phase() == 2 {
        let (theme, text, density, applied) = cx.update(|_, cx| {
            let s = model(cx).settings.read(cx);
            (s.theme(), s.text_size_px(), s.row_scale(), (cx.theme().id, cx.theme().scaled.text_body))
        })?;
        cx.log(format_args!("after the restart: theme {theme}, text {text} px, density {density}; window {} at body {} px", applied.0, applied.1));
        ensure!(theme == ThemeId::Graphite, "the theme didn't outlive the restart ({theme})");
        ensure!(applied.0 == ThemeId::Graphite, "the window didn't open in the saved theme");
        ensure!(text == 16.0 && density == 1.15, "text size / density didn't outlive the restart");
        return log_out(&cx).await;
    }
    open_on(&cx, TabId::Appearance).await?;
    // Each switch is one frame (the app applies the theme to every window once). A theme whose
    // fonts change the user panel's height costs the shell one more: the panel publishes its
    // measured height for the lists above it a frame later (`user_panel.rs`).
    // The desktop is shared: the window's activation changing or the pointer crossing it during a
    // switch redraws it too; such a switch is measured again from the theme before it.
    let desktop = |cx: &TestCx| cx.update(|window, _| (window.is_window_active(), window.mouse_position()));
    for theme in [ThemeId::Matinee, ThemeId::GraphiteLight, ThemeId::Console] {
        let panel_before = cx.update(|_, cx| model(cx).settings.read(cx).theme())?;
        let (a, b) = (panel_before.palette(), theme.palette());
        let fonts_changed = (a.font_sans, a.font_channel, a.font_meta) != (b.font_sans, b.font_channel, b.font_meta);
        for attempt in 1..=3 {
            let state = desktop(&cx)?;
            let frames = step(&cx, &format!("the {} theme", theme.display_name()), async |cx| {
                cx.update(move |_, cx| model(cx).settings.update(cx, |s, cx| s.set_theme(theme, cx)))
            }, move |cx| cx.theme().id == theme)
            .await?;
            if frames.drawn == 1 || (fonts_changed && frames.drawn == 2) {
                break;
            }
            let what = format!("a theme switch drew {} frames (1, or 2 where the UI font changes)", frames.drawn);
            ensure!(desktop(&cx)? != state, "{what}");
            if attempt == 3 {
                return Err(decibell_harness::inconclusive(format!("{what}, and the desktop disturbed the window on three tries")));
            }
            cx.log(format_args!("{what} while the window's focus or the pointer changed: again"));
            cx.update(move |_, cx| model(cx).settings.update(cx, |s, cx| s.set_theme(panel_before, cx)))?;
        }
    }
    // A real click on a card.
    step(&cx, "a click on Graphite's card", async |cx| click_probe(cx, "theme-card:graphite").await, |cx| {
        cx.theme().id == ThemeId::Graphite
    })
    .await?;
    // Text size and density apply live.
    let body_before = cx.update(|_, cx| cx.theme().scaled.text_body)?;
    cx.update(|_, cx| {
        let s = model(cx).settings.clone();
        s.update(cx, |s, cx| {
            s.set_text_size_px(16., cx);
            s.set_row_scale(1.15, cx);
        })
    })?;
    wait_for(&cx, Duration::from_secs(2), "the text size and density to apply", |cx| {
        let t = cx.theme();
        t.scaled.text_body != body_before && (t.scaled.list_row_avatar_scale - 1.15).abs() < 1e-4
    })
    .await?;
    cx.log(format_args!("body text {body_before} px → {} px, density 1.15 live", cx.update(|_, cx| cx.theme().scaled.text_body)?));
    // Let the debounced save reach config.json before the restart.
    cx.sleep(Duration::from_millis(600)).await;
    Ok(())
}

async fn check_peer(cx: TestCx) -> Result<()> {
    if std::env::var("DECIBELL_TEST_ACCOUNT").is_ok() {
        // A throwaway account: it exists once the main device registered it.
        throwaway_sign_in(&cx, false, &env("DECIBELL_TEST_PASSWORD")?).await?;
    } else {
        signed_in(&cx).await?;
    }
    mark("peer-signed-in");
    wait_for(&cx, Duration::from_secs(120), "being signed out by the other device", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    let notice = cx.update(|_, cx| model(cx).auth.read(cx).login_notice.clone())?;
    cx.log(format_args!("signed out; the sign-in page says: {notice:?}"));
    ensure!(notice.is_some(), "signed out without the sign-in page's notice");
    mark("peer-signed-out");
    Ok(())
}

fn device_ids(cx: &App) -> Option<Vec<(i64, bool)>> {
    model(cx).live.devices.read(cx).list.as_ref().map(|l| l.iter().map(|d| (d.id, d.current)).collect())
}

/// This device's address is text to select (`select-text`): a triple click takes all of it, a
/// drag across it the same, a press elsewhere clears it; nothing is copied (the clipboard is the
/// owner's).
async fn select_address(cx: &TestCx) -> Result<()> {
    let (id, address) = cx
        .update(|_, cx| model(cx).live.devices.read(cx).current().map(|d| (d.id, d.address.to_string())))?
        .context("no current device")?;
    if address.is_empty() {
        cx.log("this device has no address in the list: selection not checked");
        return Ok(());
    }
    let label = wait_probe(cx, &format!("device-address:{id}")).await?;
    let selected = |cx: &TestCx| cx.update(|_, cx| decibell_text::selected_label_text(cx));
    draw_pending(cx)?;
    cx.mouse_down(label.center(), 3, Modifiers::default())?;
    cx.mouse_up(label.center())?;
    draw_pending(cx)?;
    let tripled = selected(cx)?;
    park(cx)?;
    cx.click(point(label.right() + px(60.), label.center().y))?;
    draw_pending(cx)?;
    let after_press = selected(cx)?;
    cx.mouse_down(point(label.left() + px(1.), label.center().y), 1, Modifiers::default())?;
    cx.mouse_drag(point(label.right() + px(4.), label.center().y))?;
    cx.mouse_up(point(label.right() + px(4.), label.center().y))?;
    draw_pending(cx)?;
    let dragged = selected(cx)?;
    park(cx)?;
    cx.click(point(label.right() + px(60.), label.center().y))?;
    cx.log(format_args!("the address {address:?}: a triple click selected {tripled:?}, a press elsewhere left {after_press:?}, a drag selected {dragged:?}"));
    ensure!(tripled.as_deref() == Some(address.as_str()), "a triple click didn't select the address");
    ensure!(after_press.is_none(), "a press elsewhere didn't clear the selection");
    ensure!(dragged.as_deref() == Some(address.as_str()), "a drag across the address didn't select it");
    Ok(())
}

async fn check_devices(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    open_on(&cx, TabId::Devices).await?;
    wait_for(&cx, Duration::from_secs(10), "the device list", |cx| device_ids(cx).is_some()).await?;
    let before = cx.update(|_, cx| device_ids(cx))?.unwrap_or_default();
    cx.log(format_args!("{} devices listed ({} other)", before.len(), before.iter().filter(|d| !d.1).count()));
    select_address(&cx).await?;
    mark("ready");
    let known: Vec<i64> = before.iter().map(|d| d.0).collect();
    wait_for(&cx, Duration::from_secs(60), "the second device in the list", {
        let known = known.clone();
        move |cx| device_ids(cx).is_some_and(|l| l.iter().any(|d| !known.contains(&d.0)))
    })
    .await?;
    let new = cx.update(move |_, cx| device_ids(cx).and_then(|l| l.into_iter().find(|d| !known.contains(&d.0)).map(|d| d.0)))?
        .context("the new device vanished")?;
    cx.log(format_args!("the new device appeared (id {new}): signing it out"));
    click_probe(&cx, &format!("device-sign-out:{new}")).await?;
    wait_for(&cx, Duration::from_secs(5), "the confirmation", |cx| model(cx).live.devices.read(cx).confirming.is_some()).await?;
    // Enter confirms as the dialog opens (the renderer's document listener; nothing in it focused).
    cx.settle().await?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(15), "the device to leave the list", move |cx| {
        device_ids(cx).is_some_and(|l| !l.iter().any(|d| d.0 == new))
    })
    .await?;
    cx.log("signed out: it left the list");
    log_out(&cx).await
}

async fn check_password(cx: TestCx) -> Result<()> {
    let old = env("DECIBELL_TEST_PASSWORD")?;
    let new = env("DECIBELL_TEST_NEW_PASSWORD")?;
    throwaway_sign_in(&cx, true, &old).await?;
    open_on(&cx, TabId::Devices).await?;
    mark("signed-in");
    wait_for(&cx, Duration::from_secs(60), "the account's second device", |cx| {
        device_ids(cx).is_some_and(|l| l.iter().any(|d| !d.1))
    })
    .await?;
    cx.log("the second device is signed in: changing the password");
    open_on(&cx, TabId::Account).await?;
    click_probe(&cx, "account-password").await?;
    wait_for(&cx, Duration::from_secs(5), "the password dialog", |cx| model(cx).live.devices.read(cx).password.is_some()).await?;
    // The current password has the focus (the renderer's autoFocus): type, Tab, type, Enter.
    let error = |cx: &App| model(cx).live.devices.read(cx).password.clone().and_then(|p| p.error);
    cx.type_text(&old)?;
    cx.keystroke("tab")?;
    cx.type_text("short")?;
    cx.keystroke("tab")?;
    cx.type_text("short")?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(5), "the length check", move |cx| error(cx).is_some()).await?;
    let refused = cx.update(move |_, cx| error(cx))?;
    cx.log(format_args!("too short, typed and submitted with Enter: {refused:?}"));
    ensure!(refused.as_deref() == Some("Your new password must be at least 8 characters."), "the length check");
    cx.keystroke("shift-tab")?;
    retype(&cx, &new)?;
    cx.keystroke("tab")?;
    retype(&cx, &new)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(20), "the change to finish", |cx| model(cx).live.devices.read(cx).password.is_none()).await?;
    cx.log("password changed; the dialog closed");
    open_on(&cx, TabId::Devices).await?;
    wait_for(&cx, Duration::from_secs(30), "the other device to leave the list", |cx| {
        device_ids(cx).is_some_and(|l| l.iter().all(|d| d.1))
    })
    .await?;
    ensure!(cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?, "this device was signed out too");
    cx.log("only this device is left, still signed in");
    log_out(&cx).await?;
    let name = env("DECIBELL_TEST_ACCOUNT")?;
    cx.update(move |_, cx| actions::auth::sign_in(name, new, cx))?;
    signed_in(&cx).await?;
    cx.log("signed in again with the new password");
    log_out(&cx).await
}

async fn check_rename(cx: TestCx) -> Result<()> {
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let old = throwaway_sign_in(&cx, true, &password).await?;
    let next = env("DECIBELL_TEST_RENAME")?;
    open_on(&cx, TabId::Account).await?;
    click_probe(&cx, "account-rename").await?;
    let account = |cx: &App| match crate::handles::settings(cx).and_then(|o| o.read(cx).view().map(|v| v.view())) {
        Some(view) => view.downcast::<crate::tabs::account::AccountTab>().ok(),
        None => None,
    };
    wait_for(&cx, Duration::from_secs(5), "the rename dialog", move |cx| account(cx).is_some_and(|a| a.read(cx).rename.open)).await?;
    // The rules first: the same name is refused before anything is sent. The name has the focus.
    cx.type_text(&old)?;
    cx.keystroke("tab")?;
    cx.type_text(&password)?;
    cx.keystroke("enter")?;
    let error = move |cx: &App| account(cx).and_then(|a| a.read(cx).rename.error.clone());
    wait_for(&cx, Duration::from_secs(5), "the same-name check", move |cx| error(cx).is_some()).await?;
    let refused = cx.update(move |_, cx| error(cx))?;
    ensure!(refused.as_deref() == Some("That's already your username."), "the same-name check: {refused:?}");
    // Typed in capitals: the field lower-cases as typed (`onChange`), else the rules refuse it.
    cx.keystroke("shift-tab")?;
    retype(&cx, &next.to_uppercase())?;
    cx.keystroke("enter")?;
    let uid = cx.update(|_, cx| model(cx).auth.read(cx).uid)?;
    let expected = next.clone();
    wait_for(&cx, Duration::from_secs(20), "the rename to land", move |cx| {
        let m = model(cx);
        m.auth.read(cx).username.as_deref() == Some(expected.as_str())
            && m.name_of(uid, &"".into(), cx).as_ref() == expected.as_str()
            && account(cx).is_some_and(|a| !a.read(cx).rename.open)
    })
    .await?;
    cx.log(format_args!("renamed {old} → {next}: the account, the names store (what the user panel and every list draw) follow"));
    log_out(&cx).await
}

async fn check_privacy(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let value = |cx: &App| model(cx).settings.read(cx).get().friends_only_dms;
    // The account's value arrives with the sign-in (`own_presence`).
    cx.sleep(Duration::from_millis(1500)).await;
    if phase() == 2 {
        let kept = cx.update(|_, cx| value(cx))?;
        // Put perf1 back before judging: other tests DM it.
        open_on(&cx, TabId::Privacy).await?;
        if kept {
            click_probe(&cx, "privacy-friends-only").await?;
            wait_for(&cx, Duration::from_secs(5), "friends-only off again", move |cx| !value(cx)).await?;
            cx.sleep(Duration::from_millis(1500)).await;
            ensure!(!cx.update(|_, cx| value(cx))?, "the account's echo turned it back on");
        }
        ensure!(kept, "friends-only DMs didn't outlive the restart");
        cx.log("after the restart: on (the account's value); turned off again");
        return log_out(&cx).await;
    }
    let before = cx.update(|_, cx| value(cx))?;
    ensure!(!before, "perf1 starts with friends-only DMs on: another run left it so (turn it off)");
    open_on(&cx, TabId::Privacy).await?;
    let events_before = cx.update(|_, cx| model(cx).pump_stats().events)?;
    step(&cx, "the friends-only switch", async |cx| click_probe(cx, "privacy-friends-only").await, move |cx| value(cx)).await?;
    // Central answers with the account's state (`own_presence`); the switch stays where it is.
    cx.sleep(Duration::from_millis(1500)).await;
    let events = cx.update(|_, cx| model(cx).pump_stats().events)? - events_before;
    ensure!(cx.update(|_, cx| value(cx))?, "the switch went back after central's answer");
    cx.log(format_args!("on, settled ({events} core events since)"));
    cx.sleep(Duration::from_millis(500)).await;
    Ok(())
}

async fn check_e2ee(cx: TestCx) -> Result<()> {
    let password = env("DECIBELL_TEST_PASSWORD")?;
    let passphrase = env("DECIBELL_TEST_PASSPHRASE")?;
    let status = |cx: &App| model(cx).live.e2ee.read(cx).status;
    let flow = |cx: &App| model(cx).live.e2ee.read(cx).passphrase.clone();
    if phase() == 1 {
        throwaway_sign_in(&cx, true, &password).await?;
        wait_for(&cx, Duration::from_secs(20), "the E2EE status (not set up)", move |cx| status(cx) == E2eeStatus::NotSetUp).await?;
        open_on(&cx, TabId::Privacy).await?;
        click_probe(&cx, "e2ee-setup").await?;
        wait_for(&cx, Duration::from_secs(5), "the set-up dialog", move |cx| flow(cx).is_some()).await?;
        // The passphrase has the focus: type it, Tab, a different confirmation, Enter.
        cx.type_text(&passphrase)?;
        cx.keystroke("tab")?;
        cx.type_text("a different one!")?;
        cx.keystroke("enter")?;
        wait_for(&cx, Duration::from_secs(5), "the confirmation check", move |cx| flow(cx).is_some_and(|f| f.error.is_some())).await?;
        let mismatch = cx.update(move |_, cx| flow(cx).and_then(|f| f.error))?;
        ensure!(mismatch.as_deref() == Some("The passphrases don't match."), "the confirmation check: {mismatch:?}");
        let started = Instant::now();
        retype(&cx, &passphrase)?;
        cx.keystroke("enter")?;
        ensure!(cx.update(move |_, cx| flow(cx).is_some_and(|f| f.busy))?, "the dialog isn't busy while the core works");
        wait_for(&cx, Duration::from_secs(60), "encryption to be ready", move |cx| status(cx) == E2eeStatus::Ready && flow(cx).is_none()).await?;
        let fingerprint = cx.update(|_, cx| model(cx).live.e2ee.read(cx).fingerprint.clone())?;
        cx.log(format_args!("set up in {:.1} s; fingerprint {fingerprint}", started.elapsed().as_secs_f64()));
        ensure!(!fingerprint.is_empty(), "no fingerprint");
        return log_out(&cx).await;
    }
    // A fresh profile: the keys are on central, this device hasn't unlocked them.
    throwaway_sign_in(&cx, false, &password).await?;
    wait_for(&cx, Duration::from_secs(20), "the E2EE status (locked)", move |cx| status(cx) == E2eeStatus::Locked).await?;
    wait_for(&cx, Duration::from_secs(5), "the unlock prompt (opened by the first locked report)", move |cx| {
        flow(cx).is_some_and(|f| f.mode == decibell_model::live::e2ee::PassphraseMode::Unlock)
    })
    .await?;
    // The prompt focuses its field as it opens.
    cx.type_text("not-the-passphrase-at-all")?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(60), "the wrong passphrase's refusal", move |cx| {
        flow(cx).is_some_and(|f| !f.busy && f.error.is_some())
    })
    .await?;
    let refusal = cx.update(move |_, cx| flow(cx).and_then(|f| f.error))?;
    cx.log(format_args!("a wrong passphrase: {refusal:?}"));
    ensure!(cx.update(move |_, cx| status(cx))? == E2eeStatus::Locked, "a wrong passphrase changed the status");
    retype(&cx, &passphrase)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(60), "the unlock", move |cx| status(cx) == E2eeStatus::Ready && flow(cx).is_none()).await?;
    cx.log("unlocked with the passphrase: ready");
    log_out(&cx).await
}

async fn check_audio(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let meters = |cx: &App| model(cx).live.voice.input_level.read(cx).meters();
    let mode = |cx: &App| InputMode::of(model(cx).settings.read(cx).get());
    if phase() == 2 {
        let kept = cx.update(move |_, cx| mode(cx))?;
        ensure!(kept == InputMode::PushToTalk, "the input mode didn't outlive the restart ({kept:?})");
        cx.log("after the restart: push to talk");
        cx.update(|_, cx| actions::hotkeys::set_input_mode(false, None, cx))?;
        cx.sleep(Duration::from_millis(500)).await;
        return log_out(&cx).await;
    }
    ensure!(cx.update(move |_, cx| meters(cx))? == 0, "a meter is held before the Audio tab shows");
    open_on(&cx, TabId::Audio).await?;
    wait_for(&cx, Duration::from_secs(10), "the device lists", |cx| model(cx).live.voice.devices.read(cx).loaded).await?;
    let (inputs, outputs) = cx.update(|_, cx| {
        let d = model(cx).live.voice.devices.read(cx);
        (d.inputs.len(), d.outputs.len())
    })?;
    cx.log(format_args!("{inputs} inputs, {outputs} outputs listed"));
    ensure!(cx.update(move |_, cx| meters(cx))? == 1, "the Audio tab (voice activity) holds no meter");
    cx.update(|_, cx| actions::hotkeys::set_input_mode(true, None, cx))?;
    wait_for(&cx, Duration::from_secs(2), "push to talk to hide the meter and let go of it", move |cx| meters(cx) == 0).await?;
    cx.update(|_, cx| actions::hotkeys::set_input_mode(false, None, cx))?;
    wait_for(&cx, Duration::from_secs(2), "voice activity to hold it again", move |cx| meters(cx) == 1).await?;
    open_on(&cx, TabId::About).await?;
    wait_for(&cx, Duration::from_secs(2), "another tab to let go of it", move |cx| meters(cx) == 0).await?;
    open_on(&cx, TabId::Audio).await?;
    wait_for(&cx, Duration::from_secs(2), "the Audio tab again", move |cx| meters(cx) == 1).await?;
    audio_devices_follow(&cx).await?;
    audio_sound(&cx).await?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(2), "closing to let go of it (after the fade-out)", move |cx| meters(cx) == 0).await?;
    cx.log("the meter is held exactly while the threshold bar shows");
    wait_for(&cx, Duration::from_secs(2), "the closed window to stop watching the devices", |cx| {
        model(cx).live.voice.devices.read(cx).watchers() == 0
    })
    .await?;
    wait_gone(&cx, "decibell-audio-devices", Duration::from_secs(2)).await?;
    for thread in ["decibell-cue", "decibell-mic-test"] {
        ensure!(!test_audio::has_thread(thread), "a {thread} thread is alive with Settings closed");
    }
    cx.log("Settings closed: nothing watches the devices, no audio thread of ours exists");
    cx.update(|_, cx| actions::hotkeys::set_input_mode(true, Some(250), cx))?;
    cx.sleep(Duration::from_millis(600)).await;
    Ok(())
}

fn audio_tab(cx: &TestCx) -> Result<gpui::Entity<crate::tabs::audio::AudioTab>> {
    cx.update(|_, cx| {
        let settings = crate::handles::settings(cx).context("Settings is not open")?;
        match settings.read(cx).view() {
            Some(TabView::Audio(tab)) => Ok(tab.clone()),
            _ => bail!("Settings → Audio is not the open tab"),
        }
    })?
}

/// Wait until no thread named `name` exists.
async fn wait_gone(cx: &TestCx, name: &'static str, limit: Duration) -> Result<Duration> {
    wait_for(cx, limit, &format!("the {name} thread to end"), move |_| !test_audio::has_thread(name)).await
}

/// With the Audio tab open the device lists follow the devices: a sink that appears is listed
/// within a second and one that goes is gone, from the core's notifications (the tab's watch),
/// never a poll.
async fn audio_devices_follow(cx: &TestCx) -> Result<()> {
    let listed = |cx: &App, name: &str| model(cx).live.voice.devices.read(cx).outputs.iter().any(|d| d.name.as_ref() == name);
    ensure!(cx.update(|_, cx| model(cx).live.voice.devices.read(cx).watchers())? == 1, "the Audio tab doesn't watch the device lists");
    if !cfg!(target_os = "linux") {
        return Ok(());
    }
    wait_for(cx, Duration::from_secs(3), "the core's device listener", |_| test_audio::has_thread("decibell-audio-devices")).await?;
    let extra = match test_audio::NullSink::load("-plug") {
        Ok(extra) => extra,
        Err(e) => {
            cx.log(format_args!("no null sink ({e:#}): device changes not checked"));
            return Ok(());
        }
    };
    let name = extra.name().to_string();
    let appeared = {
        let name = name.clone();
        wait_for(cx, Duration::from_secs(3), "the new sink in the output list", move |cx| listed(cx, &name)).await?
    };
    ensure!(appeared < Duration::from_millis(1500), "the new sink took {} ms to be listed", appeared.as_millis());
    extra.unload()?;
    let went = {
        let name = name.clone();
        wait_for(cx, Duration::from_secs(3), "the removed sink to leave the output list", move |cx| !listed(cx, &name)).await?
    };
    ensure!(went < Duration::from_millis(1500), "the removed sink took {} ms to go", went.as_millis());
    cx.log(format_args!(
        "a sink appearing was listed after {} ms, gone {} ms after it was removed (the 400 ms debounce included)",
        appeared.as_millis(),
        went.as_millis()
    ));
    Ok(())
}

/// The interface sounds and the microphone test through the core, on the null sink the parent
/// made (`DECIBELL_CUE_SINK`) and its monitor: nothing reaches the desktop's devices.
async fn audio_sound(cx: &TestCx) -> Result<()> {
    use decibell_core::api::audio;
    use decibell_model::live::Sound;
    let Ok(sink) = std::env::var(CUE_SINK_ENV) else {
        cx.log("no test sink: the sound part is skipped");
        return Ok(());
    };
    let monitor = format!("{sink}.monitor");
    let tab = audio_tab(cx)?;
    let level = {
        let tab = tab.clone();
        move |cx: &App| tab.read(cx).level()
    };
    let on_sink = |what: &str| -> Result<test_audio::Streams> {
        test_audio::ensure_only_on(&sink).with_context(|| format!("while {what}"))
    };
    ensure!(
        cx.update(|_, cx| model(cx).live.voice.devices.read(cx).outputs.iter().any(|d| d.name.as_ref() == sink))?,
        "the test sink {sink} is not in the output list"
    );
    // A voice session and the sounds in it play on the saved output device: the sink, before
    // anything sounds. The app's player hands the core nothing otherwise.
    {
        let (sink, monitor) = (sink.clone(), monitor.clone());
        cx.update(move |_, cx| {
            actions::voice::set_output_device(Some(sink), cx);
            actions::voice::set_input_device(Some(monitor), cx);
        })?;
    }
    // Where a sound really goes, shown with silence first: the core's stream for sounds opens
    // on the sink it is given.
    audio::play_sound(&audio::Sound::from_pcm(vec![0; 9600], 48_000), Some(&sink));
    wait_for(cx, Duration::from_secs(3), "the core's stream for sounds", |_| test_audio::has_thread("decibell-cue")).await?;
    let started = Instant::now();
    loop {
        let streams = on_sink("playing silence")?;
        if !streams.sinks.is_empty() {
            break;
        }
        if !test_audio::has_thread("decibell-cue") || started.elapsed() > Duration::from_secs(2) {
            bail!("the stream for sounds was never seen on the sound server");
        }
        cx.sleep(Duration::from_millis(20)).await;
    }
    let closed = wait_gone(cx, "decibell-cue", Duration::from_secs(3)).await?;
    cx.log(format_args!("silence played on {sink} only; its stream and thread were gone {} ms later", closed.as_millis()));

    // "Test mic": the core's test on the sink's monitor, through the user's processing.
    cx.update(|_, cx| tab.update(cx, |tab, cx| tab.toggle_test(cx)))?;
    ensure!(cx.update(|_, cx| tab.read(cx).test_running())?, "the mic test didn't start (see the log)");
    ensure!(audio::mic_test_running(), "the core runs no mic test");
    let started = Instant::now();
    loop {
        let streams = on_sink("the mic test opens its device")?;
        if !streams.sources.is_empty() {
            break;
        }
        ensure!(started.elapsed() < Duration::from_secs(3), "the mic test's capture stream was never seen on the sound server");
        cx.sleep(Duration::from_millis(20)).await;
    }
    ensure!(test_audio::has_thread("decibell-mic-test"), "no decibell-mic-test thread while the test runs");
    cx.sleep(Duration::from_millis(600)).await;
    let rest = cx.update(|_, cx| level(cx))?;
    ensure!(rest == -96., "the meter reads {rest} dB on a silent input");

    // A sound, as the model asks for one: the app's player → the core → the sink → the meter.
    cx.update(|_, cx| model(cx).live.cues.update(cx, |c, cx| c.play(Sound::UserJoin, cx)))?;
    let mut peak = -96f32;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(1500) {
        peak = peak.max(cx.update(|_, cx| level(cx))?);
        cx.sleep(Duration::from_millis(20)).await;
    }
    on_sink("a sound plays")?;
    ensure!(peak > -60., "the meter never rose above -60 dB while user_join played (peak {peak:.1})");
    let t = {
        let level = level.clone();
        wait_for(cx, Duration::from_secs(4), "the meter to rest after the sound", move |cx| level(cx) == -96.).await?
    };
    cx.log(format_args!("user_join on the sink: the meter peaked at {peak:.1} dB and rested at -96 dB {} ms later", t.as_millis()));

    // The ring loops (0.8 s of tone in each 2 s) until it is stopped.
    cx.update(|_, cx| model(cx).live.cues.update(cx, |c, cx| c.start_ring(cx)))?;
    // A burst: the tone after at least 0.7 s without it (the tone itself dips once).
    let (mut bursts, mut first, mut last, mut heard_at) = (0u32, None, None, None::<Instant>);
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(6600) {
        if cx.update(|_, cx| level(cx))? > -45. {
            if heard_at.is_none_or(|at| at.elapsed() > Duration::from_millis(700)) {
                bursts += 1;
                first.get_or_insert(started.elapsed());
                last = Some(started.elapsed());
            }
            heard_at = Some(Instant::now());
        }
        cx.sleep(Duration::from_millis(20)).await;
    }
    on_sink("the ring plays")?;
    let span = last.unwrap_or_default().saturating_sub(first.unwrap_or_default());
    ensure!(bursts >= 3, "the ring was heard {bursts} time(s) in 6.6 s: it doesn't loop");
    let period = span.as_secs_f64() / f64::from(bursts - 1);
    ensure!((1.8..2.2).contains(&period), "the ring repeats every {period:.2} s, not every 2 s");
    cx.update(|_, cx| model(cx).live.cues.update(cx, |c, cx| c.stop_ring(cx)))?;
    {
        let level = level.clone();
        wait_for(cx, Duration::from_secs(3), "the meter to rest after the ring stopped", move |cx| level(cx) == -96.).await?;
    }
    let mut after = -96f32;
    let started = Instant::now();
    while started.elapsed() < Duration::from_millis(2500) {
        after = after.max(cx.update(|_, cx| level(cx))?);
        cx.sleep(Duration::from_millis(50)).await;
    }
    ensure!(after == -96., "the ring was heard again after it was stopped ({after:.1} dB)");
    cx.log(format_args!("the ring looped {bursts} times in 6.6 s (every {period:.2} s) and stopped"));

    // A processing switch flipped under a running test: the test starts over with it.
    let aec = cx.update(|_, cx| model(cx).settings.read(cx).get().aec_enabled)?;
    cx.update(move |_, cx| actions::voice::set_aec_enabled(!aec, cx))?;
    cx.sleep(Duration::from_millis(300)).await;
    ensure!(cx.update(|_, cx| tab.read(cx).test_running())? && audio::mic_test_running(), "the test didn't survive a processing switch");
    on_sink("the test restarted")?;
    cx.update(move |_, cx| actions::voice::set_aec_enabled(aec, cx))?;
    cx.sleep(Duration::from_millis(300)).await;

    // "Stop test": the device closes and the thread goes; the sounds' stream closed by itself.
    cx.update(|_, cx| tab.update(cx, |tab, cx| tab.toggle_test(cx)))?;
    ensure!(!audio::mic_test_running(), "the core still runs the mic test after Stop");
    let mic = wait_gone(cx, "decibell-mic-test", Duration::from_secs(2)).await?;
    let cue = wait_gone(cx, "decibell-cue", Duration::from_secs(3)).await?;
    let left = test_audio::streams_of(std::process::id())?;
    ensure!(left.sinks.is_empty() && left.sources.is_empty(), "streams left open: {left:?}");
    cx.log(format_args!(
        "test stopped: the mic test's thread gone after {} ms, the sounds' after {} ms, no stream left",
        mic.as_millis(),
        cue.as_millis()
    ));
    // The profile is used again (the restart): back to the defaults before the sink goes.
    cx.update(|_, cx| {
        actions::voice::set_output_device(None, cx);
        actions::voice::set_input_device(None, cx);
    })?;
    Ok(())
}

// ── Keys as GPUI's Linux layer reports them (patch 0012) ─────────────────────────────────────
//
// Each event carries what the layout makes of the key (`Keystroke`) and which physical key it is
// (`KeyCode::Evdev`): the recorder must keep the second. The layouts are the desktop's business,
// never touched here: the events are built as xkb's German and Turkish keymaps produce them
// (the `hotkeys-layout` self-test checks the platform delivers them so).

const CONTROL_L: u16 = 29;
const SHIFT_L: u16 = 42;
const SHIFT_R: u16 = 54;
const ESCAPE: u16 = 1;
const F9: u16 = 67;

fn ctrl_shift() -> Modifiers {
    Modifiers { control: true, shift: true, ..Default::default() }
}

/// A key pressed and released: `key` / `typed` what the layout makes of it, `evdev` which key.
fn press(cx: &TestCx, key: &str, typed: Option<&str>, evdev: u16, modifiers: Modifiers) -> Result<()> {
    let keystroke = Keystroke { modifiers, key: key.into(), key_char: typed.map(str::to_string) };
    cx.key_event(keystroke.clone(), KeyCode::Evdev(evdev), true)?;
    cx.key_event(keystroke, KeyCode::Evdev(evdev), false)
}

/// A modifier key going down or up, with the modifiers as they are after it.
fn modifier(cx: &TestCx, evdev: u16, pressed: bool, after: Modifiers) -> Result<()> {
    cx.modifier_key(KeyCode::Evdev(evdev), pressed, after)
}

/// Draw what is pending now. A window in the background (this one, most of the time) draws only
/// when GPUI dispatches a key event, which draws first; a click on the frame from before the last
/// change (a binding's new id) would reach that frame's handlers. A modifiers event that names no
/// key changes nothing else.
fn draw_pending(cx: &TestCx) -> Result<()> {
    cx.update(|window, cx| {
        window.dispatch_event(PlatformInput::ModifiersChanged(gpui::ModifiersChangedEvent::default()), cx);
    })
}

fn mouse(cx: &TestCx, button: MouseButton, at: gpui::Point<Pixels>, modifiers: Modifiers) -> Result<()> {
    cx.mouse_move(at)?;
    cx.update(move |window, cx| {
        window.dispatch_event(
            PlatformInput::MouseDown(gpui::MouseDownEvent { button, position: at, modifiers, click_count: 1, first_mouse: false }),
            cx,
        );
        window.dispatch_event(PlatformInput::MouseUp(gpui::MouseUpEvent { button, position: at, modifiers, click_count: 1 }), cx);
    })
}

async fn check_keybinds(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let hotkeys = |cx: &App| model(cx).settings.read(cx).get().hotkeys.clone();
    let recording = |cx: &App| model(cx).live.hotkeys.read(cx).recording;
    let keys_of = move |cx: &App, row: usize| hotkeys(cx).get(row).map(|b| b.keys.clone()).unwrap_or_default();
    ensure!(cx.update(move |_, cx| hotkeys(cx))?.is_empty(), "a fresh profile has keybinds");
    open_on(&cx, TabId::Keybinds).await?;
    let status = cx.update(|_, cx| model(cx).live.hotkeys.read(cx).status.clone())?;
    let status = status.context("no listener status from the core")?;
    cx.log(format_args!("the core's listener: {} ({}): \"{}\"", status.backend, status.state, crate::tabs::keybinds::backend_summary(Some(&status))));
    // Add: a new row records at once; the core's listener is paused meanwhile.
    click_probe(&cx, "keybind-add").await?;
    wait_for(&cx, Duration::from_secs(3), "the new row to record", move |cx| recording(cx) && hotkeys(cx).len() == 1).await?;
    let paused = cx.update(|_, cx| model(cx).live.hotkeys.read(cx).recording)?;
    ensure!(paused, "the listener isn't paused while recording");
    let ctrl = Modifiers::control();
    modifier(&cx, CONTROL_L, true, ctrl)?;
    modifier(&cx, SHIFT_L, true, ctrl_shift())?;
    press(&cx, "f9", None, F9, ctrl_shift())?;
    modifier(&cx, SHIFT_L, false, ctrl)?;
    modifier(&cx, CONTROL_L, false, Modifiers::default())?;
    wait_for(&cx, Duration::from_secs(3), "the combo to be recorded", move |cx| {
        !recording(cx) && keys_of(cx, 0) == ["Control", "Shift", "F9"]
    })
    .await?;
    let first = cx.update(move |_, cx| hotkeys(cx))?;
    cx.log(format_args!("recorded {} as {:?} under a fresh id", first[0].action, first[0].keys));
    // Escape alone cancels: the new row goes.
    click_probe(&cx, "keybind-add").await?;
    wait_for(&cx, Duration::from_secs(3), "the second row to record", move |cx| recording(cx) && hotkeys(cx).len() == 2).await?;
    press(&cx, "escape", None, ESCAPE, Modifiers::default())?;
    wait_for(&cx, Duration::from_secs(3), "Escape to cancel and drop the empty row", move |cx| !recording(cx) && hotkeys(cx).len() == 1).await?;
    ensure!(cx.update(|_, cx| settings_open(cx))?, "Escape while recording closed Settings");
    // The same combo on a second action is named as a conflict.
    click_probe(&cx, "keybind-add").await?;
    wait_for(&cx, Duration::from_secs(3), "the third row to record", move |cx| recording(cx) && hotkeys(cx).len() == 2).await?;
    modifier(&cx, CONTROL_L, true, ctrl)?;
    modifier(&cx, SHIFT_L, true, ctrl_shift())?;
    press(&cx, "f9", None, F9, ctrl_shift())?;
    modifier(&cx, SHIFT_L, false, ctrl)?;
    modifier(&cx, CONTROL_L, false, Modifiers::default())?;
    wait_for(&cx, Duration::from_secs(3), "the second combo", move |cx| !recording(cx) && hotkeys(cx).iter().all(|b| !b.keys.is_empty())).await?;
    let all = cx.update(move |_, cx| hotkeys(cx))?;
    let problem = crate::tabs::keybinds::binding_problem(&all[1], &all, Some(&status));
    cx.log(format_args!("the twin: {problem:?}"));
    ensure!(problem.as_deref().is_some_and(|p| p.starts_with("Same keys as")), "no conflict named");

    // Record the second row again, as other keyboards send their keys: the physical key is
    // stored, whatever the layout types with it. With Ctrl + Shift held, so the X server's grab
    // of the recorded combo (until the next case replaces it) can't swallow anyone's typing.
    // (layout, the key, what it types there, its evdev code, the key recorded)
    let cases = [
        ("de", "the key labelled Z", "z", 21, "KeyY"),
        ("de", "the key labelled Ö", "ö", 39, "Semicolon"),
        ("tr", "the key labelled Ş", "ş", 39, "Semicolon"),
        ("tr", "the key labelled i (dotted)", "i", 40, "Quote"),
    ];
    for (layout, what, typed, evdev, code) in cases {
        draw_pending(&cx)?;
        click_probe(&cx, "keybind-record:1").await?;
        wait_for(&cx, Duration::from_secs(3), "the row to record again", move |cx| recording(cx)).await?;
        modifier(&cx, CONTROL_L, true, ctrl)?;
        modifier(&cx, SHIFT_L, true, ctrl_shift())?;
        press(&cx, typed, None, evdev, ctrl_shift())?;
        modifier(&cx, SHIFT_L, false, ctrl)?;
        modifier(&cx, CONTROL_L, false, Modifiers::default())?;
        wait_for(&cx, Duration::from_secs(3), &format!("{layout}: Ctrl + Shift + {what}"), move |cx| {
            !recording(cx) && keys_of(cx, 1) == ["Control", "Shift", code]
        })
        .await?;
        cx.log(format_args!("{layout}: Ctrl + Shift + {what} (types {typed:?}, evdev {evdev}) → Control + Shift + {code}"));
    }
    // Modifiers alone: complete as the last one is released, sides kept.
    draw_pending(&cx)?;
    click_probe(&cx, "keybind-record:1").await?;
    wait_for(&cx, Duration::from_secs(3), "the row to record again", move |cx| recording(cx)).await?;
    let shift = Modifiers::shift();
    modifier(&cx, SHIFT_R, true, shift)?;
    modifier(&cx, CONTROL_L, true, ctrl_shift())?;
    modifier(&cx, SHIFT_R, false, ctrl)?;
    modifier(&cx, CONTROL_L, false, Modifiers::default())?;
    wait_for(&cx, Duration::from_secs(3), "right Shift + left Ctrl alone", move |cx| {
        !recording(cx) && keys_of(cx, 1) == ["ControlLeft", "ShiftRight"]
    })
    .await?;
    cx.log("right Shift + left Ctrl, released: [\"ControlLeft\", \"ShiftRight\"]");
    // A mouse side button with a modifier held, pressed over the row. (Where to press is read
    // first: the window going to the background cancels a recording, as the renderer's blur
    // does, and the desktop may move the focus whenever this test yields; from the click that
    // starts the recording to the last event nothing here yields.)
    draw_pending(&cx)?;
    let at = wait_probe(&cx, "keybind-record:1").await?.center();
    click_probe(&cx, "keybind-record:1").await?;
    wait_for(&cx, Duration::from_secs(3), "the row to record again", move |cx| recording(cx)).await?;
    modifier(&cx, SHIFT_L, true, shift)?;
    mouse(&cx, MouseButton::Navigate(gpui::NavigationDirection::Back), at, shift)?;
    modifier(&cx, SHIFT_L, false, Modifiers::default())?;
    wait_for(&cx, Duration::from_secs(3), "Shift + the back button", move |cx| !recording(cx) && keys_of(cx, 1) == ["Shift", "Mouse4"]).await?;
    cx.log("Shift + the back button: [\"Shift\", \"Mouse4\"]");
    park(&cx)?;

    // Remove both.
    click_probe(&cx, "keybind-remove:1").await?;
    click_probe(&cx, "keybind-remove:0").await?;
    wait_for(&cx, Duration::from_secs(3), "the rows to go", move |cx| hotkeys(cx).is_empty()).await?;
    // Saved: config.json has what the tab shows (after the save's debounce).
    let a = all[0].id.clone();
    cx.update(move |_, cx| {
        let s = model(cx).settings.clone();
        s.update(cx, |s, cx| {
            s.change(
                |v| {
                    v.hotkeys = vec![decibell_core::config::HotkeyBindingSetting {
                        id: a,
                        action: "toggle_mute".into(),
                        keys: vec!["Control".into(), "Shift".into(), "F9".into()],
                    }]
                },
                cx,
            )
        })
    })?;
    cx.sleep(Duration::from_millis(600)).await;
    let config = decibell_core::api::settings::load_config().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let saved = config["settings"]["hotkeys"].as_array().map_or(0, Vec::len);
    ensure!(saved == 1, "config.json holds {saved} keybinds, not 1");
    cx.log("the keybinds are saved in config.json");
    cx.update(|_, cx| actions::hotkeys::set_bindings(Vec::new(), cx))?;
    cx.sleep(Duration::from_millis(600)).await;
    log_out(&cx).await
}

fn network(cx: &App) -> Option<gpui::Entity<crate::tabs::network::NetworkTab>> {
    crate::handles::settings(cx)?.read(cx).view()?.view().downcast::<crate::tabs::network::NetworkTab>().ok()
}

async fn check_network(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let caps = |cx: &App| {
        let s = model(cx).settings.read(cx).get().clone();
        (s.upload_limit_bps, s.download_limit_bps)
    };
    open_on(&cx, TabId::Network).await?;
    click_probe(&cx, "rate-up:10 MB/s").await?;
    wait_for(&cx, Duration::from_secs(3), "the upload preset", move |cx| caps(cx).0 == 10 * 1024 * 1024).await?;
    // The custom box: a click, typed, Enter.
    click_probe(&cx, "rate-down-custom").await?;
    retype(&cx, "3.5")?;
    ensure!(cx.update(|_, cx| network(cx).is_some_and(|n| n.read(cx).typed(cx)))?, "the box doesn't hold the typed value");
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(3), "the custom download cap", move |cx| caps(cx).1 == (3.5 * 1024. * 1024.) as u64).await?;
    cx.sleep(Duration::from_millis(600)).await;
    let config = decibell_core::api::settings::load_config().await.map_err(|e| anyhow::anyhow!("{e}"))?;
    let saved = (config["settings"]["upload_limit_bps"].as_u64(), config["settings"]["download_limit_bps"].as_u64());
    cx.log(format_args!("typed 3.5 and Enter: saved {saved:?}"));
    ensure!(saved == (Some(10 * 1024 * 1024), Some((3.5 * 1024. * 1024.) as u64)), "config.json doesn't hold the caps");
    if let Some(service) = decibell_model::chat::CoreAttachmentService::attachments() {
        let (up, down) = (service.limits().upload_rate(), service.limits().download_rate());
        cx.log(format_args!("the attachment service runs: its caps are {up} / {down} B/s"));
        ensure!((up, down) == (10 * 1024 * 1024, (3.5 * 1024. * 1024.) as u64), "the service didn't get the caps");
    }
    click_probe(&cx, "rate-up:Unlimited").await?;
    click_probe(&cx, "rate-down:Unlimited").await?;
    wait_for(&cx, Duration::from_secs(3), "Unlimited", move |cx| caps(cx) == (0, 0)).await?;
    cx.sleep(Duration::from_millis(600)).await;
    log_out(&cx).await
}

async fn check_network_leave(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let upload = |cx: &App| model(cx).settings.read(cx).get().upload_limit_bps;
    let typed = |cx: &App| network(cx).is_some_and(|n| n.read(cx).typed(cx));
    let mbps = |v: f64| (v * 1024. * 1024.).round() as u64;
    // No save bar, ever: the renderer's tab has none (its box applies on blur).
    open_on(&cx, TabId::Network).await?;
    click_probe(&cx, "rate-up-custom").await?;
    retype(&cx, "12.5")?;
    ensure!(cx.update(move |_, cx| typed(cx))?, "the box doesn't hold the typed value");
    // A press on another tab: the box loses the focus (blur: apply) and the tab opens. Press and
    // release as a hand does, with a frame between them (the release is what opens the tab).
    draw_pending(&cx)?;
    let tab = wait_probe(&cx, "settings-tab:account").await?.center();
    cx.mouse_down(tab, 1, Modifiers::default())?;
    draw_pending(&cx)?;
    cx.mouse_up(tab)?;
    park(&cx)?;
    wait_for(&cx, Duration::from_secs(3), "the Account tab", move |cx| shown_tab(cx) == Some(TabId::Account))
        .await
        .context("a press on another tab with a typed value didn't open it")?;
    let applied = cx.update(move |_, cx| upload(cx))?;
    cx.log(format_args!("a press on another tab applied the typed value ({applied} B/s) and opened the tab"));
    ensure!(applied == mbps(12.5), "the press elsewhere didn't apply the typed value");
    // Esc with a typed value: the window closes and the value applies (the renderer drops it:
    // docs/electron-quirks.md #26).
    open_on(&cx, TabId::Network).await?;
    click_probe(&cx, "rate-up-custom").await?;
    retype(&cx, "7.5")?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "Settings closed by Esc", move |cx| !settings_open(cx)).await?;
    let applied = cx.update(move |_, cx| upload(cx))?;
    cx.log(format_args!("Esc closed the window and applied the typed value ({applied} B/s)"));
    ensure!(applied == mbps(7.5), "Esc didn't apply the typed value");
    // ✕ (the pane's close, as its button calls it) with the box focused: applied, closed.
    open_on(&cx, TabId::Network).await?;
    click_probe(&cx, "rate-up-custom").await?;
    retype(&cx, "4.5")?;
    cx.update(|_, cx| {
        if let Some(window) = crate::handles::settings(cx) {
            window.update(cx, |w, cx| w.request_close(cx));
        }
    })?;
    wait_for(&cx, Duration::from_secs(3), "Settings closed by ✕", move |cx| !settings_open(cx)).await?;
    let applied = cx.update(move |_, cx| upload(cx))?;
    cx.log(format_args!("✕ closed the window and applied the typed value ({applied} B/s)"));
    ensure!(applied == mbps(4.5), "✕ didn't apply the typed value");
    // A value that isn't a number applies nothing and holds nothing.
    open_on(&cx, TabId::Network).await?;
    click_probe(&cx, "rate-up-custom").await?;
    retype(&cx, "abc")?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "Settings closed with text that isn't a number", move |cx| !settings_open(cx)).await?;
    ensure!(cx.update(move |_, cx| upload(cx))? == mbps(4.5), "text that isn't a number changed the cap");
    // Put perf1's cap back.
    cx.update(|_, cx| {
        let s = model(cx).settings.clone();
        s.update(cx, |s, cx| s.change(|v| v.upload_limit_bps = 0, cx));
    })?;
    cx.sleep(Duration::from_millis(600)).await;
    log_out(&cx).await
}

/// An updater stand-in that counts what About asks of it.
#[derive(Default)]
struct CountingUpdateHooks {
    notice_seen: std::cell::Cell<u32>,
}

impl crate::UpdateHooks for CountingUpdateHooks {
    fn check(&self, _: &mut App) {}
    fn quit_and_install(&self, _: &mut App) {}
    fn open_release_page(&self, _: &mut App) {}
    fn failed_notice_seen(&self, _: &mut App) {
        self.notice_seen.set(self.notice_seen.get() + 1);
    }
}

async fn check_close_ways(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    // The pane's ✕.
    open_on(&cx, TabId::Account).await?;
    let close = wait_probe(&cx, "settings-pane-close").await?.center();
    cx.click(close)?;
    park(&cx)?;
    wait_for(&cx, Duration::from_secs(3), "Settings closed by ✕", |cx| !settings_open(cx)).await?;
    cx.log("✕ closed Settings");
    // A press on the backdrop (the window's corner, outside the Settings window).
    open_on(&cx, TabId::Privacy).await?;
    settle_drawn(&cx).await?;
    let v = visible(&cx)?;
    cx.click(point(v.left() + px(12.), v.bottom() - px(12.)))?;
    park(&cx)?;
    wait_for(&cx, Duration::from_secs(3), "Settings closed by the backdrop", |cx| !settings_open(cx)).await?;
    cx.log("a press on the backdrop closed Settings");
    wait_for(&cx, Duration::from_secs(2), "the tab's view to go", |cx| shown_tab(cx).is_none()).await?;
    // About with a failed-update notice: it shows, and opening About marks it read (once).
    let hooks = std::rc::Rc::new(CountingUpdateHooks::default());
    let (previous, before) = cx.update({
        let hooks = hooks.clone();
        move |_, cx| {
            let previous = crate::SettingsHooks::get(cx).update.clone();
            crate::SettingsHooks::set_update_hooks(hooks, cx);
            let update = model(cx).update.clone();
            let before = update.read(cx).failed_notice.clone();
            update.update(cx, |u, cx| u.set_failed_notice(Some("Version 9.9.9 didn't start (test), so 0.9.4 was kept.".into()), cx));
            (previous, before)
        }
    })?;
    let result = async {
        open_on(&cx, TabId::About).await?;
        settle_drawn(&cx).await?;
        let seen = hooks.notice_seen.get();
        let shown = probes::bounds("update-failed-notice").is_some();
        cx.log(format_args!("About opened with a failed-update notice: marked read {seen} time(s), still shown: {shown}"));
        ensure!(seen == 1, "opening About marked the notice read {seen} times");
        ensure!(shown, "About doesn't show the notice");
        cx.keystroke("escape")?;
        wait_for(&cx, Duration::from_secs(3), "Settings closed", |cx| !settings_open(cx)).await?;
        Ok::<_, anyhow::Error>(())
    }
    .await;
    cx.update(move |_, cx| {
        if let Some(previous) = previous {
            crate::SettingsHooks::set_update_hooks(previous, cx);
        }
        let update = model(cx).update.clone();
        update.update(cx, |u, cx| u.set_failed_notice(before, cx));
    })?;
    result?;
    log_out(&cx).await
}

/// Sleeps `duration` watching what draws a frame that isn't ours: the focus changing hands, the
/// pointer moving over the window. Returns what happened ("" when nothing).
async fn watched_sleep(cx: &TestCx, duration: Duration) -> Result<String> {
    let state = |cx: &TestCx| cx.update(|window, _| (window.is_window_active(), window.mouse_position()));
    let mut last = state(cx)?;
    let (mut flips, mut moved) = (0, false);
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
        (flips, moved) => format!("; disturbed: {flips} focus changes{}", if moved { ", the pointer moved" } else { "" }),
    })
}

async fn check_idle(cx: TestCx) -> Result<()> {
    signed_in(&cx).await?;
    let tabs: Vec<TabId> = TabId::ALL.into_iter().collect();
    open_on(&cx, TabId::Account).await?;
    for tab in tabs {
        open_on(&cx, tab).await?;
        park(&cx)?;
        // The stack is shared (other clients of perf1 sign in and out, presence moves) and other
        // windows take and give the focus (GPUI redraws the whole window then): a try that drew
        // counts only when no core event arrived and nothing disturbed the window; five tries.
        let mut last = String::new();
        let mut ok = false;
        for attempt in 1..=5 {
            let before = settle_drawn(&cx).await?;
            let events_before = cx.update(|_, cx| model(cx).pump_stats().events)?;
            let disturbed = watched_sleep(&cx, Duration::from_secs(5)).await?;
            let frames = cx.frames()?.since(before);
            let events = cx.update(|_, cx| model(cx).pump_stats().events)? - events_before;
            last = format!(
                "{}: {} drawn, {} presented in 5 s (try {attempt}; {events} core events{disturbed})",
                tab.label(),
                frames.drawn,
                frames.presented
            );
            if (events > 0 || !disturbed.is_empty()) && frames.drawn > 0 {
                continue;
            }
            ok = frames.drawn == 0;
            break;
        }
        cx.log(&last);
        ensure!(ok, "not idle: {last}");
    }
    cx.keystroke("escape")?;
    log_out(&cx).await
}

// The account flows with other devices (devices, the password, renames, Log Out).
mod accounts;
