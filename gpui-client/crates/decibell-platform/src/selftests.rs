//! The platform's self-tests. Each parent starts the app again as children (`decibell --check
//! platform-…`, `decibell decibell://…`, `decibell --hotkey=…`) with throwaway profiles on a private
//! session bus with nothing on it (no Secret Service, no portals: nothing reaches the desktop's
//! keyring or launcher), reads their `decibell: platform …` / `platform-check: …` lines and, where
//! the compositor can be asked (KDE: KWin scripting through `tools/perf/linux/kwin.py`), what it says
//! about the children's windows. Development builds only (`dev-local`: the markers and `--login`).
//! `idle-x11` also starts private Xvfb servers (`selftests/idle_x11.rs`): the only place any test here
//! fakes input, never the desktop's own display.
//!
//! The ones that sign in (`deep-link-arg`, `attention`) use the local stack (`tools/dev/`) and accounts
//! other tests don't: carol (deep link) and dave called by erin (`decibell-headless`, attention).
//! Each signs out at the end, which forgets its device at central, and `attention` puts dave's status
//! back to Online.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, ExitStatus, Stdio};
use std::sync::atomic::Ordering;
use std::sync::mpsc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx};
use decibell_model::Model;
use gpui::App;

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "profile-guard",
            about: "a development build's default profile is ~/.cache/decibell-dev/gpui-default and it refuses the shipped one (app and decibell-headless)",
            run: |cx| Box::pin(profile_guard(cx)),
        },
        SelfTest {
            name: "single-instance",
            about: "a second start on the same profile hands over and exits; the first comes forward; two profiles run side by side",
            run: |cx| Box::pin(single_instance(cx)),
        },
        SelfTest {
            name: "deep-link-arg",
            about: "decibell://invite/… on the first start is the pending invite before (and after) sign-in; a second start's replaces it",
            run: |cx| Box::pin(deep_link_arg(cx)),
        },
        SelfTest {
            name: "hotkey-cli",
            about: "--hotkey=<action> with nothing running exits; with an instance running it fires there and exits",
            run: |cx| Box::pin(hotkey_cli(cx)),
        },
        SelfTest {
            name: "attention",
            about: "an incoming call (a headless peer) flashes the unfocused window, as KWin reports; not under Do Not Disturb",
            run: |cx| Box::pin(attention(cx)),
        },
        SelfTest {
            name: "taskbar-progress",
            about: "a download's progress as LauncherEntry signals on a private bus (values, pause, clear); closing with an upload asks",
            run: |cx| Box::pin(taskbar_progress(cx)),
        },
        SelfTest {
            name: "keyring-label",
            about: "credentials are stored, read and deleted under the label Decibell (a throwaway gnome-keyring on a private bus; never the session's keyring)",
            run: |cx| Box::pin(keyring_label(cx)),
        },
        SelfTest {
            name: "window-state",
            about: "resize, quit, start: the same size; maximised comes back maximised; a place off every screen is dropped (X11)",
            run: |cx| Box::pin(window_state_test(cx)),
        },
        SelfTest {
            name: "idle-x11",
            about: "auto-away on X11: SYNC IDLETIME alarms in a private Xvfb (idle at 2 s, active at once on XTEST input there, re-armed, threshold changes, thread gone when off, 0 wakeups at rest), MIT-SCREEN-SAVER, neither; the session's own source on Wayland and XWayland",
            run: |cx| Box::pin(idle_x11_test(cx)),
        },
    ]
}

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "platform-profile", about: "(in the app) print the profile", run: |cx| Box::pin(check_profile(cx)) },
        SelfTest { name: "platform-instance", about: "(in the app) wait for a second start", run: |cx| Box::pin(check_instance(cx)) },
        SelfTest { name: "platform-deep-link", about: "(in the app) the pending invite", run: |cx| Box::pin(check_deep_link(cx)) },
        SelfTest { name: "platform-hotkey", about: "(in the app) hotkeys from second starts", run: |cx| Box::pin(check_hotkey(cx)) },
        SelfTest { name: "platform-attention", about: "(in the app) an incoming call", run: |cx| Box::pin(check_attention(cx)) },
        SelfTest { name: "platform-progress", about: "(in the app) fake transfers", run: |cx| Box::pin(check_progress(cx)) },
        SelfTest { name: "platform-window-state", about: "(in the app) the window's bounds", run: |cx| Box::pin(check_window_state(cx)) },
        SelfTest { name: "platform-keyring", about: "(in the app) a credentials round trip", run: |cx| Box::pin(check_keyring(cx)) },
        SelfTest { name: "platform-idle-source", about: "(in the app) the idle source, auto-away on and off", run: |cx| Box::pin(check_idle_source(cx)) },
    ]
}

// ── idle-x11 (`selftests/idle_x11.rs`: X11 and its markers only) ───────────────────────────────

#[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "dev-local"))]
mod idle_x11;

#[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "dev-local"))]
async fn idle_x11_test(cx: TestCx) -> Result<()> {
    idle_x11::run(cx).await
}

#[cfg(all(any(target_os = "linux", target_os = "freebsd"), feature = "dev-local"))]
async fn check_idle_source(cx: TestCx) -> Result<()> {
    idle_x11::check(cx).await
}

#[cfg(not(all(any(target_os = "linux", target_os = "freebsd"), feature = "dev-local")))]
async fn idle_x11_test(cx: TestCx) -> Result<()> {
    cx.log("SKIP: X11 only, in development builds (--features dev-local)");
    Ok(())
}

#[cfg(not(all(any(target_os = "linux", target_os = "freebsd"), feature = "dev-local")))]
async fn check_idle_source(_: TestCx) -> Result<()> {
    Ok(())
}

// ── Helpers ────────────────────────────────────────────────────────────────────────────────────

/// One line for the parent, at once.
fn say(message: std::fmt::Arguments) {
    use std::io::Write as _;
    println!("platform-check: {message}");
    let _ = std::io::stdout().flush();
}

fn dev_markers(cx: &TestCx) -> bool {
    if !cfg!(feature = "dev-local") {
        cx.log("SKIP: this binary has no development markers (build with --features dev-local)");
        return false;
    }
    true
}

/// A temporary directory, removed when dropped.
struct TempDir(PathBuf);

impl TempDir {
    fn new(name: &str) -> Result<TempDir> {
        let nanos = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos());
        let dir = std::env::temp_dir().join(format!("decibell-platform-{name}-{}-{nanos}", std::process::id()));
        std::fs::create_dir_all(&dir)?;
        Ok(TempDir(dir))
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

/// A private session bus with nothing on it (no service directories: nothing can be activated),
/// monitorable, for as long as it is held.
struct EmptyBus {
    pid: u32,
    address: String,
}

impl EmptyBus {
    fn start(dir: &Path) -> Result<EmptyBus> {
        if !cfg!(target_os = "linux") {
            // No session bus to keep a child away from: an empty address, which nothing reads.
            return Ok(EmptyBus { pid: 0, address: String::new() });
        }
        let socket = dir.join("bus");
        let config = dir.join("bus.conf");
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
        Ok(EmptyBus { pid, address })
    }
}

impl Drop for EmptyBus {
    fn drop(&mut self) {
        // Our own daemon, by its pid.
        if self.pid != 0 {
            let _ = Command::new("kill").arg(self.pid.to_string()).status();
        }
    }
}

/// A child process whose stdout lines we read; killed (by its pid) if still running when dropped.
struct Child {
    name: String,
    child: std::process::Child,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
    stderr: PathBuf,
}

impl Child {
    fn spawn(name: &str, program: &Path, args: &[&str], env: &[(&str, String)], remove: &[&str], dir: &Path) -> Result<Child> {
        let stderr = dir.join(format!("{name}.stderr"));
        let mut command = Command::new(program);
        command
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::from(std::fs::File::create(&stderr)?));
        for key in ["DECIBELL_PERF", "XDG_ACTIVATION_TOKEN", "DESKTOP_STARTUP_ID", "DECIBELL_USER_DATA_DIR", "DECIBELL_DEV_CONTROL"] {
            command.env_remove(key);
        }
        for key in remove {
            command.env_remove(key);
        }
        for (key, value) in env {
            command.env(key, value);
        }
        let mut child = command.spawn().with_context(|| format!("starting {name}"))?;
        let stdout = child.stdout.take().context("piped")?;
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new().name(format!("platform-test-{name}")).spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
        Ok(Child { name: name.to_string(), child, lines, seen: Vec::new(), stderr })
    }

    fn drain(&mut self) {
        while let Ok(line) = self.lines.try_recv() {
            self.seen.push(line);
        }
    }

    fn pid(&self) -> u32 {
        self.child.id()
    }

    /// The first line (from the start) that starts with `prefix`.
    async fn wait_line(&mut self, cx: &TestCx, prefix: &str, limit: Duration) -> Result<String> {
        let started = Instant::now();
        let mut from = 0;
        loop {
            self.drain();
            if let Some(line) = self.seen[from..].iter().find(|l| l.starts_with(prefix)) {
                return Ok(line.clone());
            }
            from = self.seen.len();
            if let Ok(Some(status)) = self.child.try_wait() {
                self.drain();
                if let Some(line) = self.seen.iter().find(|l| l.starts_with(prefix)) {
                    return Ok(line.clone());
                }
                let tail = self.tail();
                bail!("{} ended ({status}) without printing {prefix:?}:\n{tail}", self.name);
            }
            if started.elapsed() > limit {
                let tail = self.tail();
                bail!("{} printed no {prefix:?} within {} s:\n{tail}", self.name, limit.as_secs());
            }
            cx.sleep(Duration::from_millis(50)).await;
        }
    }

    fn printed(&mut self, prefix: &str) -> bool {
        self.drain();
        self.seen.iter().any(|l| l.starts_with(prefix))
    }

    async fn wait_exit(&mut self, cx: &TestCx, limit: Duration) -> Result<ExitStatus> {
        let started = Instant::now();
        loop {
            if let Some(status) = self.child.try_wait()? {
                self.drain();
                return Ok(status);
            }
            if started.elapsed() > limit {
                let tail = self.tail();
                bail!("{} still runs after {} s:\n{tail}", self.name, limit.as_secs());
            }
            cx.sleep(Duration::from_millis(20)).await;
        }
    }

    fn running(&mut self) -> bool {
        matches!(self.child.try_wait(), Ok(None))
    }

    fn tail(&mut self) -> String {
        self.drain();
        let stderr = std::fs::read_to_string(&self.stderr).unwrap_or_default();
        let out: Vec<&String> = self.seen.iter().rev().take(25).collect();
        let err: Vec<&str> = stderr.lines().rev().take(15).collect();
        format!(
            "  stdout: {}\n  stderr: {}",
            out.into_iter().rev().map(String::as_str).collect::<Vec<_>>().join("\n          "),
            err.into_iter().rev().collect::<Vec<_>>().join("\n          ")
        )
    }

    fn relay(&mut self, cx: &TestCx, filter: impl Fn(&str) -> bool) {
        self.drain();
        for line in self.seen.iter().filter(|l| filter(l)) {
            cx.log(format_args!("│ {}: {line}", self.name));
        }
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if let Ok(None) = self.child.try_wait() {
            // Only the child this test started, by its own pid.
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

fn exe() -> Result<PathBuf> {
    Ok(std::env::current_exe()?)
}

/// A binary built beside this one (`decibell-headless`).
fn beside(name: &str) -> Option<PathBuf> {
    let path = std::env::current_exe().ok()?.parent()?.join(name);
    path.exists().then_some(path)
}

/// The environment of an app child with its own profile on `bus`: no central unless one is given.
fn app_env(profile: &Path, bus: &EmptyBus, central: Option<&Stack>) -> Vec<(&'static str, String)> {
    let mut env = vec![
        ("DECIBELL_USER_DATA_DIR", profile.display().to_string()),
        ("DBUS_SESSION_BUS_ADDRESS", bus.address.clone()),
        ("DECIBELL_TEST_PROBES", "1".to_string()),
    ];
    match central {
        Some(stack) => {
            env.push(("DECIBELL_CENTRAL", stack.central.clone()));
            env.push(("DECIBELL_DEV_PASSWORD", stack.password.clone()));
        }
        None => env.push(("DECIBELL_CENTRAL", "127.0.0.1:9".to_string())),
    }
    env
}

/// The local stack, when it runs (`tools/dev/local-stack.sh up`).
struct Stack {
    central: String,
    password: String,
}

fn stack() -> Result<Option<Stack>> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let Ok(text) = std::fs::read_to_string(dev.join("run/accounts.json")) else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_str(&text)?;
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json["central"].as_str().map(str::to_string)).context("no central")?;
    let (host, port) = central.rsplit_once(':').context("central host:port")?;
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: these tests run against the local stack only");
    if std::net::TcpStream::connect_timeout(&format!("{host}:{port}").parse()?, Duration::from_secs(1)).is_err() {
        return Ok(None);
    }
    let password = std::env::var("DECIBELL_DEV_PASSWORD").ok().or_else(|| json["password"].as_str().map(str::to_string)).context("no password")?;
    Ok(Some(Stack { central, password }))
}

/// `tools/perf` above this binary (the KWin bridge and `winctl.sh`).
fn perf_tools() -> Option<PathBuf> {
    std::env::current_exe().ok()?.ancestors().map(|d| d.join("tools/perf")).find(|p| p.join("linux/kwin.py").exists())
}

/// What KWin says about a process's main window.
#[derive(Debug, Default)]
struct KwinWindow {
    active: bool,
    demands_attention: bool,
}

/// KDE's compositor, asked through `tools/perf` (`None` elsewhere: those checks are skipped).
struct Kwin {
    perf: PathBuf,
    script: PathBuf,
}

impl Kwin {
    fn find(dir: &Path) -> Option<Kwin> {
        if !std::env::var("XDG_CURRENT_DESKTOP").is_ok_and(|d| d.to_uppercase().contains("KDE")) {
            return None;
        }
        let perf = perf_tools()?;
        let script = dir.join("kwin_window.py");
        std::fs::write(&script, include_str!("../tools/kwin_window.py")).ok()?;
        Some(Kwin { perf, script })
    }

    fn window(&self, pid: u32) -> Result<Option<KwinWindow>> {
        let out = Command::new("python3")
            .arg("-I")
            .arg(&self.script)
            .arg(self.perf.join("linux"))
            .arg(pid.to_string())
            .output()
            .context("python3")?;
        let json: serde_json::Value = serde_json::from_slice(&out.stdout).with_context(|| String::from_utf8_lossy(&out.stderr).to_string())?;
        if let Some(error) = json["error"].as_str() {
            bail!("KWin: {error}");
        }
        Ok(json["windows"].as_array().and_then(|w| w.first()).map(|w| KwinWindow {
            active: w["active"].as_bool().unwrap_or(false),
            demands_attention: w["demandsAttention"].as_bool().unwrap_or(false),
        }))
    }

    /// Moves the focus from `child`'s window to the desktop of its output, and waits until KWin says so.
    /// (Not to this test's own window: KWin treats two windows with the app id `decibell` as one
    /// application, and lets one hand the focus to the other without asking for attention.)
    async fn unfocus(&self, cx: &TestCx, child: u32) -> Result<()> {
        // The child's window may still be mapping: a few tries.
        let mut tries = 0;
        loop {
            let out = Command::new(self.perf.join("winctl.sh"))
                .args(["unfocus", &child.to_string()])
                .output()?;
            if out.status.success() {
                break;
            }
            tries += 1;
            ensure!(tries < 10, "winctl.sh unfocus failed ({}): {}{}", out.status, String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
            cx.sleep(Duration::from_millis(300)).await;
        }
        let started = Instant::now();
        while self.window(child)?.is_some_and(|w| w.active) {
            ensure!(started.elapsed() < Duration::from_secs(5), "the child's window kept the focus");
            cx.sleep(Duration::from_millis(100)).await;
        }
        Ok(())
    }
}

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

async fn wait_for(cx: &TestCx, limit: Duration, what: &str, ready: impl Fn(&mut App) -> bool) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if cx.update(|_, cx| ready(cx))? {
            return Ok(started.elapsed());
        }
        if started.elapsed() > limit {
            bail!("{what}: not within {} s", limit.as_secs());
        }
        cx.sleep(Duration::from_millis(50)).await;
    }
}

/// A file the parent creates to let a child go on.
async fn wait_file(cx: &TestCx, path: &Path, limit: Duration) -> Result<()> {
    let started = Instant::now();
    while !path.exists() {
        ensure!(started.elapsed() < limit, "the parent never created {}", path.display());
        cx.sleep(Duration::from_millis(50)).await;
    }
    Ok(())
}

fn test_dir() -> Result<PathBuf> {
    std::env::var_os("DECIBELL_PLATFORM_TEST_DIR").map(PathBuf::from).context("DECIBELL_PLATFORM_TEST_DIR is not set")
}

// ── profile-guard ──────────────────────────────────────────────────────────────────────────────

async fn profile_guard(cx: TestCx) -> Result<()> {
    if !decibell_model::core::DEVELOPMENT_BUILD {
        cx.log("SKIP: a shipped build uses the shipped profile by design");
        return Ok(());
    }
    let real_shipped = decibell_model::core::shipped_profile_dir()?;
    let existed = real_shipped.exists();
    let dir = TempDir::new("profile-guard")?;
    let home = dir.0.join("home");
    std::fs::create_dir_all(&home)?;
    let bus = EmptyBus::start(&dir.0)?;
    let mut env = vec![
        ("HOME", home.display().to_string()),
        ("DBUS_SESSION_BUS_ADDRESS", bus.address.clone()),
        ("DECIBELL_CENTRAL", "127.0.0.1:9".to_string()),
    ];
    let remove = ["XDG_CONFIG_HOME", "XDG_CACHE_HOME"];
    // Where the two profiles are, by platform (`decibell_model::core`): the cache and the
    // configuration directory under the throwaway home.
    let (expected, shipped) = if cfg!(windows) {
        let (local, roaming) = (home.join("AppData").join("Local"), home.join("AppData").join("Roaming"));
        env.push(("LOCALAPPDATA", local.display().to_string()));
        env.push(("APPDATA", roaming.display().to_string()));
        (local.join("decibell-dev").join("gpui-default"), roaming.join("decibell"))
    } else if cfg!(target_os = "macos") {
        (home.join("Library/Caches/decibell-dev/gpui-default"), home.join("Library/Application Support/decibell"))
    } else {
        (home.join(".cache/decibell-dev/gpui-default"), home.join(".config/decibell"))
    };

    let mut app = Child::spawn("app", &exe()?, &["--check", "platform-profile"], &env, &remove, &dir.0)?;
    let line = app.wait_line(&cx, "platform-check: profile ", Duration::from_secs(30)).await?;
    let status = app.wait_exit(&cx, Duration::from_secs(30)).await?;
    { let tail = app.tail(); ensure!(status.success(), "the app's check failed ({status}):\n{}", tail); }
    let profile = PathBuf::from(line.trim_start_matches("platform-check: profile "));
    ensure!(profile == expected, "the default profile is {}, expected {}", profile.display(), expected.display());
    ensure!(!shipped.exists(), "{} was created", shipped.display());
    cx.log(format_args!("the app without DECIBELL_USER_DATA_DIR: {} (HOME={})", profile.display(), home.display()));

    let mut env_shipped = env.clone();
    env_shipped.push(("DECIBELL_USER_DATA_DIR", shipped.display().to_string()));
    let mut refused = Child::spawn("app-shipped", &exe()?, &["--check", "platform-profile"], &env_shipped, &remove, &dir.0)?;
    let status = refused.wait_exit(&cx, Duration::from_secs(30)).await?;
    let stderr = std::fs::read_to_string(&refused.stderr).unwrap_or_default();
    { let tail = refused.tail(); ensure!(status.code() == Some(2), "pointed at the shipped profile, the app exited with {status}:\n{}", tail); }
    ensure!(stderr.contains("never uses the shipped client's profile"), "no refusal message:\n{stderr}");
    ensure!(!shipped.exists(), "{} was created", shipped.display());
    cx.log("pointed at the shipped profile: refused (exit 2), nothing created");

    match beside("decibell-headless") {
        None => cx.log("decibell-headless is not built beside this binary: its default not checked"),
        Some(headless) => {
            let mut h = Child::spawn("headless", &headless, &["--timeout", "10", "dump", "core"], &env, &remove, &dir.0)?;
            let line = h.wait_line(&cx, "DUMP core ", Duration::from_secs(30)).await?;
            let status = h.wait_exit(&cx, Duration::from_secs(30)).await?;
            { let tail = h.tail(); ensure!(status.success(), "decibell-headless failed ({status}):\n{}", tail); }
            let json: serde_json::Value = serde_json::from_str(line.trim_start_matches("DUMP core "))?;
            let dir_used = json["userDataDir"].as_str().unwrap_or_default();
            ensure!(Path::new(dir_used) == expected, "decibell-headless used {dir_used}");
            ensure!(!shipped.exists(), "{} was created by decibell-headless", shipped.display());
            let mut h = Child::spawn("headless-shipped", &headless, &["--user-data-dir", &shipped.display().to_string(), "dump", "core"], &env, &remove, &dir.0)?;
            let status = h.wait_exit(&cx, Duration::from_secs(30)).await?;
            ensure!(status.code() == Some(2) && !shipped.exists(), "decibell-headless took the shipped profile ({status})");
            cx.log(format_args!("decibell-headless: {dir_used} by default, the shipped profile refused"));
        }
    }
    ensure!(existed || !real_shipped.exists(), "{} appeared during the test", real_shipped.display());
    Ok(())
}

async fn check_profile(cx: TestCx) -> Result<()> {
    wait_for(&cx, Duration::from_secs(20), "the core", |_| decibell_model::core::is_running()).await?;
    let dir = decibell_model::core::user_data_dir().context("no profile")?;
    say(format_args!("profile {}", dir.display()));
    Ok(())
}

// ── single-instance ────────────────────────────────────────────────────────────────────────────

/// How long a second start may take to hand over and exit (it never starts GPUI: ~10 ms here; the
/// limit leaves room for a loaded machine).
const FORWARD_LIMIT: Duration = Duration::from_millis(1500);

async fn single_instance(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    let dir = TempDir::new("single-instance")?;
    let bus = EmptyBus::start(&dir.0)?;
    let kwin = Kwin::find(&dir.0);
    let (one, two) = (dir.0.join("one"), dir.0.join("two"));
    let checked_env = |profile: &Path| {
        let mut env = app_env(profile, &bus, None);
        env.push(("DECIBELL_PLATFORM_TEST_DIR", dir.0.display().to_string()));
        env
    };
    let done = |child: &Child| std::fs::write(dir.0.join(format!("cue-done-{}", child.pid())), b"");
    let mut a = Child::spawn("a", &exe()?, &["--check", "platform-instance"], &checked_env(&one), &[], &dir.0)?;
    a.wait_line(&cx, "decibell: platform instance listening", Duration::from_secs(30)).await?;
    a.wait_line(&cx, "platform-check: ready", Duration::from_secs(30)).await?;
    if let Some(kwin) = &kwin {
        // The test's own window takes the focus: the first instance must come forward.
        kwin.unfocus(&cx, a.pid()).await?;
        let before = kwin.window(a.pid())?.unwrap_or_default();
        cx.log(format_args!("a unfocused: active {}, demands attention {}", before.active, before.demands_attention));
    }

    let started = Instant::now();
    let mut b = Child::spawn("b", &exe()?, &[], &app_env(&one, &bus, None), &[], &dir.0)?;
    let status = b.wait_exit(&cx, Duration::from_secs(10)).await?;
    let took = started.elapsed();
    { let tail = b.tail(); ensure!(status.success(), "the second start exited with {status}:\n{}", tail); }
    let said = b.wait_line(&cx, "decibell: platform forwarded in", Duration::from_secs(1)).await?;
    ensure!(!b.printed("decibell: platform instance listening"), "the second start became an instance");
    cx.log(format_args!("second start: exited 0 after {} ms ({said})", took.as_millis()));
    ensure!(took < FORWARD_LIMIT, "the second start took {} ms", took.as_millis());
    a.wait_line(&cx, "decibell: platform instance forwarded", Duration::from_secs(10)).await?;
    let active = a.wait_line(&cx, "platform-check: active ", Duration::from_secs(10)).await?;
    cx.log(format_args!("a: {}", active.trim_start_matches("platform-check: ")));
    if let Some(kwin) = &kwin {
        let started = Instant::now();
        let mut after = kwin.window(a.pid())?.unwrap_or_default();
        while !(after.active || after.demands_attention) && started.elapsed() < Duration::from_secs(3) {
            cx.sleep(Duration::from_millis(150)).await;
            after = kwin.window(a.pid())?.unwrap_or_default();
        }
        cx.log(format_args!("KWin: a active {}, demands attention {}", after.active, after.demands_attention));
        ensure!(
            after.active || after.demands_attention,
            "the first instance neither came forward nor asked for attention"
        );
    } else {
        cx.log("no KWin: the activation was requested (marker), not checked with the compositor");
    }

    let mut c = Child::spawn("c", &exe()?, &["--check", "platform-instance"], &checked_env(&two), &[], &dir.0)?;
    c.wait_line(&cx, "decibell: platform instance listening", Duration::from_secs(30)).await?;
    c.wait_line(&cx, "platform-check: ready", Duration::from_secs(30)).await?;
    ensure!(a.running(), "the first profile's instance ended when the second's started");
    cx.log("another profile: its own instance, side by side with the first");
    done(&a)?;
    let mut d = Child::spawn("d", &exe()?, &[], &app_env(&two, &bus, None), &[], &dir.0)?;
    ensure!(d.wait_exit(&cx, Duration::from_secs(10)).await?.success(), "d failed");
    c.wait_line(&cx, "decibell: platform instance forwarded", Duration::from_secs(10)).await?;
    done(&c)?;
    for child in [&mut a, &mut c] {
        let status = child.wait_exit(&cx, Duration::from_secs(20)).await?;
        let tail = child.tail();
        ensure!(status.success(), "{} failed ({status}):\n{tail}", child.name);
    }
    Ok(())
}

async fn check_instance(cx: TestCx) -> Result<()> {
    wait_for(&cx, Duration::from_secs(20), "the core", |_| decibell_model::core::is_running()).await?;
    let before = crate::FORWARDED.load(Ordering::Relaxed);
    say(format_args!("ready"));
    wait_for(&cx, Duration::from_secs(60), "a second start", move |_| crate::FORWARDED.load(Ordering::Relaxed) > before).await?;
    say(format_args!("forwarded"));
    // The compositor answers the activation within a frame or two (or marks the window instead).
    let started = Instant::now();
    let mut active = false;
    while started.elapsed() < Duration::from_secs(2) && !active {
        active = cx.update(|window, _| window.is_window_active())?;
        cx.sleep(Duration::from_millis(50)).await;
    }
    say(format_args!("active {active}"));
    // The window stays until the parent has asked the compositor about it.
    if let Ok(dir) = test_dir() {
        wait_file(&cx, &dir.join(format!("cue-done-{}", std::process::id())), Duration::from_secs(30)).await?;
    }
    Ok(())
}

// ── deep-link-arg ──────────────────────────────────────────────────────────────────────────────

const DEEP_LINK_ACCOUNT: &str = "carol";

async fn deep_link_arg(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    let stack = stack()?;
    let dir = TempDir::new("deep-link")?;
    let bus = EmptyBus::start(&dir.0)?;
    let profile = dir.0.join("profile");
    let mut args = vec!["--check", "platform-deep-link", "decibell://invite/PLATFORMA1"];
    if stack.is_some() {
        args.extend(["--login", DEEP_LINK_ACCOUNT]);
    } else {
        cx.log("the local stack is down: the link is checked before sign-in only");
    }
    let mut env = app_env(&profile, &bus, stack.as_ref());
    env.push(("DECIBELL_CHECK_LOGIN", if stack.is_some() { "1" } else { "0" }.to_string()));
    let mut a = Child::spawn("a", &exe()?, &args, &env, &[], &dir.0)?;
    let first = a.wait_line(&cx, "platform-check: first ", Duration::from_secs(30)).await?;
    cx.log(format_args!("first start: {}", first.trim_start_matches("platform-check: ")));
    if stack.is_some() {
        let after = a.wait_line(&cx, "platform-check: signed-in ", Duration::from_secs(60)).await?;
        cx.log(format_args!("{}", after.trim_start_matches("platform-check: ")));
    }
    let mut b = Child::spawn("b", &exe()?, &["decibell://invite/PLATFORMB2"], &env, &[], &dir.0)?;
    let status = b.wait_exit(&cx, Duration::from_secs(10)).await?;
    { let tail = b.tail(); ensure!(status.success(), "the second start failed ({status}):\n{}", tail); }
    let forwarded = a.wait_line(&cx, "platform-check: forwarded ", Duration::from_secs(15)).await?;
    cx.log(format_args!("second start: {}", forwarded.trim_start_matches("platform-check: ")));
    let status = a.wait_exit(&cx, Duration::from_secs(30)).await?;
    { let tail = a.tail(); ensure!(status.success(), "the app's check failed ({status}):\n{}", tail); }
    Ok(())
}

fn pending_code(cx: &App) -> Option<String> {
    model(cx).servers.read(cx).pending_invite.as_ref().map(|i| i.code.to_string())
}

async fn check_deep_link(cx: TestCx) -> Result<()> {
    let code = cx.update(|_, cx| pending_code(cx))?;
    let signed_in = cx.update(|_, cx| model(cx).auth.read(cx).is_authenticated)?;
    ensure!(code.as_deref() == Some("PLATFORMA1"), "the first start's link is not the pending invite: {code:?}");
    say(format_args!("first {} (signed in: {signed_in})", code.unwrap_or_default()));
    let login = std::env::var("DECIBELL_CHECK_LOGIN").as_deref() == Ok("1");
    if login {
        wait_for(&cx, Duration::from_secs(60), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
        let code = cx.update(|_, cx| pending_code(cx))?;
        ensure!(code.as_deref() == Some("PLATFORMA1"), "the sign-in dropped the pending invite: {code:?}");
        say(format_args!("signed-in still {}", code.unwrap_or_default()));
    }
    let before = crate::FORWARDED.load(Ordering::Relaxed);
    wait_for(&cx, Duration::from_secs(60), "the second start's link", move |cx| {
        crate::FORWARDED.load(Ordering::Relaxed) > before && pending_code(cx).as_deref() == Some("PLATFORMB2")
    })
    .await?;
    say(format_args!("forwarded {}", cx.update(|_, cx| pending_code(cx))?.unwrap_or_default()));
    if login {
        // Forget this device at central again.
        cx.update(|_, cx| decibell_model::actions::auth::sign_out(cx))?;
        wait_for(&cx, Duration::from_secs(20), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    }
    Ok(())
}

// ── hotkey-cli ─────────────────────────────────────────────────────────────────────────────────

async fn hotkey_cli(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    let dir = TempDir::new("hotkey-cli")?;
    let bus = EmptyBus::start(&dir.0)?;

    let lone = dir.0.join("lone");
    let started = Instant::now();
    let mut solo = Child::spawn("alone", &exe()?, &["--hotkey=toggle_mute"], &app_env(&lone, &bus, None), &[], &dir.0)?;
    let status = solo.wait_exit(&cx, Duration::from_secs(10)).await?;
    let stderr = std::fs::read_to_string(&solo.stderr).unwrap_or_default();
    ensure!(status.success(), "--hotkey with nothing running exited with {status}");
    ensure!(!solo.printed("decibell: platform instance listening"), "--hotkey with nothing running started the app");
    ensure!(stderr.contains("isn't running"), "no message:\n{stderr}");
    cx.log(format_args!("nothing running: exited 0 after {} ms without starting (\"{}\")", started.elapsed().as_millis(), stderr.trim()));

    let profile = dir.0.join("profile");
    let mut a = Child::spawn("a", &exe()?, &["--check", "platform-hotkey"], &app_env(&profile, &bus, None), &[], &dir.0)?;
    a.wait_line(&cx, "platform-check: ready", Duration::from_secs(30)).await?;
    for value in ["toggle_mute", "toggle_mute", "push_to_talk:down", "push_to_talk:up"] {
        let arg = format!("--hotkey={value}");
        let started = Instant::now();
        let mut b = Child::spawn("b", &exe()?, &[&arg], &app_env(&profile, &bus, None), &[], &dir.0)?;
        let status = b.wait_exit(&cx, Duration::from_secs(10)).await?;
        { let tail = b.tail(); ensure!(status.success(), "{arg} failed ({status}):\n{}", tail); }
        let took = started.elapsed();
        let line = a.wait_line(&cx, &format!("decibell: platform hotkey {value} "), Duration::from_secs(10)).await?;
        ensure!(line.ends_with("injected"), "{line}");
        cx.log(format_args!("{arg}: the second start exited after {} ms; the instance: {line}", took.as_millis()));
        // Each line once: wait for the next only after this one.
        a.seen.retain(|l| !l.starts_with("decibell: platform hotkey"));
    }
    let cues = a.wait_line(&cx, "platform-check: cues ", Duration::from_secs(15)).await?;
    cx.log(format_args!("the instance ran them: {}", cues.trim_start_matches("platform-check: ")));
    let status = a.wait_exit(&cx, Duration::from_secs(20)).await?;
    { let tail = a.tail(); ensure!(status.success(), "the app's check failed ({status}):\n{}", tail); }
    ensure!(!a.printed("decibell: platform instance forwarded links=0 hotkey=-"), "a hotkey start activated the window");
    Ok(())
}

async fn check_hotkey(cx: TestCx) -> Result<()> {
    use decibell_model::live::{Cue, Sound};
    wait_for(&cx, Duration::from_secs(20), "the core", |_| decibell_model::core::is_running()).await?;
    let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::<Cue>::new()));
    let _subscription = cx.update(|_, cx| {
        let seen = seen.clone();
        cx.subscribe(&model(cx).live.cues, move |_, cue: &Cue, _| seen.borrow_mut().push(*cue))
    })?;
    say(format_args!("ready"));
    let started = Instant::now();
    loop {
        let sounds = seen.borrow().iter().filter(|c| matches!(c, Cue::Play(Sound::Mute | Sound::Unmute))).count();
        if sounds >= 2 {
            break;
        }
        ensure!(started.elapsed() < Duration::from_secs(60), "the two toggle_mute presses did not reach the voice actions: {:?}", seen.borrow());
        cx.sleep(Duration::from_millis(50)).await;
    }
    // The push to talk press and release reach the core's gate (no cue: logged by the marker).
    let ptt = std::time::Instant::now();
    while crate::FORWARDED.load(Ordering::Relaxed) < 4 && ptt.elapsed() < Duration::from_secs(30) {
        cx.sleep(Duration::from_millis(50)).await;
    }
    say(format_args!("cues {:?}", seen.borrow()));
    Ok(())
}

// ── attention ──────────────────────────────────────────────────────────────────────────────────

const CALLEE: &str = "dave";
const CALLER: &str = "erin";

async fn attention(cx: TestCx) -> Result<()> {
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
    let dir = TempDir::new("attention")?;
    let Some(kwin) = Kwin::find(&dir.0) else {
        cx.log("SKIP: no KWin to unfocus the window and read its state (the request itself is unit-sized: attention::request)");
        return Ok(());
    };
    let bus = EmptyBus::start(&dir.0)?;
    let profile = dir.0.join("app");
    let mut env = app_env(&profile, &bus, Some(&stack));
    env.push(("DECIBELL_PLATFORM_TEST_DIR", dir.0.display().to_string()));
    let mut app = Child::spawn("app", &exe()?, &["--check", "platform-attention", "--login", CALLEE], &env, &[], &dir.0)?;
    app.wait_line(&cx, "platform-check: ready", Duration::from_secs(60)).await?;
    kwin.unfocus(&cx, app.pid()).await?;
    let before = kwin.window(app.pid())?.context("KWin doesn't list the app's window")?;
    ensure!(!before.active, "the app's window kept the focus");
    cx.log(format_args!("{CALLEE}'s window unfocused (demands attention: {})", before.demands_attention));

    let cue = |name: &str| dir.0.join(format!("cue-{name}"));
    let peer_profile = dir.0.join("peer");
    let steps: Vec<String> = [
        "--user-data-dir", &peer_profile.display().to_string(), "--timeout", "60",
        "login", CALLER, "wait", "logged-in",
        "call", CALLEE, "print", "calling", "wait-file", &cue("hangup1").display().to_string(), "hangup", "wait", "call", "idle", "print", "ended",
        "wait-file", &cue("call2").display().to_string(),
        "call", CALLEE, "print", "calling2", "wait-file", &cue("hangup2").display().to_string(), "hangup", "wait", "call", "idle", "print", "ended2",
        "logout", "wait", "logged-out",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let steps: Vec<&str> = steps.iter().map(String::as_str).collect();
    let peer_env = vec![("DECIBELL_CENTRAL", stack.central.clone()), ("DECIBELL_DEV_PASSWORD", stack.password.clone()), ("DBUS_SESSION_BUS_ADDRESS", bus.address.clone())];
    let mut peer = Child::spawn("peer", &headless, &steps, &peer_env, &[], &dir.0)?;

    peer.wait_line(&cx, "MARK calling", Duration::from_secs(60)).await?;
    app.wait_line(&cx, "platform-check: incoming", Duration::from_secs(20)).await?;
    app.wait_line(&cx, "decibell: platform attention requested", Duration::from_secs(10)).await?;
    let started = Instant::now();
    let mut seen = KwinWindow::default();
    while started.elapsed() < Duration::from_secs(5) {
        seen = kwin.window(app.pid())?.unwrap_or_default();
        if seen.demands_attention {
            break;
        }
        cx.sleep(Duration::from_millis(200)).await;
    }
    ensure!(seen.demands_attention, "KWin doesn't report the ringing window as demanding attention");
    ensure!(!seen.active, "the incoming call took the focus");
    cx.log(format_args!("ringing: the request was made and KWin reports the window as demanding attention ({} ms)", started.elapsed().as_millis()));
    std::fs::write(cue("hangup1"), b"")?;
    peer.wait_line(&cx, "MARK ended", Duration::from_secs(30)).await?;

    // Do Not Disturb: the call shows silently, no flash.
    std::fs::write(cue("dnd"), b"")?;
    app.wait_line(&cx, "platform-check: dnd on", Duration::from_secs(30)).await?;
    kwin.unfocus(&cx, app.pid()).await?;
    std::fs::write(cue("call2"), b"")?;
    peer.wait_line(&cx, "MARK calling2", Duration::from_secs(60)).await?;
    let quiet = app.wait_line(&cx, "platform-check: incoming under dnd", Duration::from_secs(20)).await?;
    ensure!(quiet.ends_with("no request"), "{quiet}");
    cx.log("under Do Not Disturb: the call rang in, no attention request");
    std::fs::write(cue("hangup2"), b"")?;
    peer.wait_line(&cx, "MARK ended2", Duration::from_secs(30)).await?;
    let status = app.wait_exit(&cx, Duration::from_secs(60)).await?;
    { let tail = app.tail(); ensure!(status.success(), "the app's check failed ({status}):\n{}", tail); }
    let status = peer.wait_exit(&cx, Duration::from_secs(30)).await?;
    { let tail = peer.tail(); ensure!(status.success(), "the peer failed ({status}):\n{}", tail); }
    Ok(())
}

async fn check_attention(cx: TestCx) -> Result<()> {
    use decibell_model::live::calls::CallStatus;
    use decibell_model::live::presence::{DisplayStatus, OwnStatus};
    let dir = test_dir()?;
    wait_for(&cx, Duration::from_secs(60), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    say(format_args!("ready"));
    let status = |cx: &mut App| model(cx).live.calls.read(cx).status;
    wait_for(&cx, Duration::from_secs(90), "the first call", move |cx| status(cx) == CallStatus::Incoming).await?;
    say(format_args!("incoming"));
    wait_for(&cx, Duration::from_secs(10), "the attention request", |_| crate::attention::REQUESTS.load(Ordering::Relaxed) > 0).await?;
    wait_for(&cx, Duration::from_secs(60), "the first call to end", move |cx| status(cx) == CallStatus::Idle).await?;

    wait_file(&cx, &dir.join("cue-dnd"), Duration::from_secs(60)).await?;
    cx.update(|_, cx| decibell_model::actions::presence::choose_status(OwnStatus::Dnd, cx))?;
    // Online again whatever happens below.
    struct Restore(gpui::AsyncApp);
    impl Drop for Restore {
        fn drop(&mut self) {
            self.0.update(|cx| decibell_model::actions::presence::choose_status(OwnStatus::Online, cx));
            // (A no-op once it is Online.)
        }
    }
    let restore = Restore(cx.app().clone());
    wait_for(&cx, Duration::from_secs(20), "Do Not Disturb", |cx| model(cx).live.presence.own.read(cx).display() == DisplayStatus::Dnd).await?;
    say(format_args!("dnd on"));
    let requests = crate::attention::REQUESTS.load(Ordering::Relaxed);
    wait_for(&cx, Duration::from_secs(90), "the second call", move |cx| status(cx) == CallStatus::Incoming).await?;
    cx.sleep(Duration::from_millis(1500)).await;
    let now = crate::attention::REQUESTS.load(Ordering::Relaxed);
    say(format_args!("incoming under dnd: {}", if now == requests { "no request" } else { "a request was made" }));
    wait_for(&cx, Duration::from_secs(60), "the second call to end", move |cx| status(cx) == CallStatus::Idle).await?;
    drop(restore);
    wait_for(&cx, Duration::from_secs(20), "Online again", |cx| model(cx).live.presence.own.read(cx).display() != DisplayStatus::Dnd).await?;
    cx.update(|_, cx| decibell_model::actions::auth::sign_out(cx))?;
    wait_for(&cx, Duration::from_secs(20), "the sign-out", |cx| !model(cx).auth.read(cx).is_authenticated).await?;
    Ok(())
}

// ── taskbar-progress ───────────────────────────────────────────────────────────────────────────

async fn taskbar_progress(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
        cx.log("SKIP: the launcher entry is Linux's");
        return Ok(());
    }
    let dir = TempDir::new("progress")?;
    let bus = EmptyBus::start(&dir.0)?;
    let log = dir.0.join("monitor.log");
    let mut monitor = Command::new("dbus-monitor")
        .args(["--address", &bus.address, "type='signal',interface='com.canonical.Unity.LauncherEntry'"])
        .stdout(Stdio::from(std::fs::File::create(&log)?))
        .stderr(Stdio::null())
        .spawn()
        .context("starting dbus-monitor")?;
    cx.sleep(Duration::from_millis(300)).await;
    let mut app = Child::spawn("app", &exe()?, &["--check", "platform-progress"], &app_env(&dir.0.join("profile"), &bus, None), &[], &dir.0)?;
    let status = app.wait_exit(&cx, Duration::from_secs(60)).await;
    cx.sleep(Duration::from_millis(300)).await;
    // Our own dbus-monitor, by its pid.
    let _ = monitor.kill();
    let _ = monitor.wait();
    app.relay(&cx, |l| l.starts_with("decibell: platform progress") || l.starts_with("platform-check:"));
    let status = status?;
    { let tail = app.tail(); ensure!(status.success(), "the app's check failed ({status}):\n{}", tail); }
    let updates = parse_launcher_entry(&std::fs::read_to_string(&log)?);
    cx.log(format_args!("signals seen on the bus: {updates:?}"));
    // The download (the pause repeats the value: the protocol has no paused state), then the upload
    // the quit guard is tried with.
    let expected: Vec<(String, bool, f64)> = [(true, 0.0), (true, 0.25), (true, 0.5), (true, 0.5), (false, 0.0), (true, 0.1), (false, 0.0)]
        .iter()
        .map(|(v, p)| ("application://decibell.desktop".to_string(), *v, *p))
        .collect();
    ensure!(updates == expected, "expected {expected:?}");
    Ok(())
}

/// `dbus-monitor`'s text of `Update(s, a{sv})` signals → (app uri, progress-visible, progress).
fn parse_launcher_entry(text: &str) -> Vec<(String, bool, f64)> {
    let mut out = Vec::new();
    let mut current: Option<(String, bool, f64)> = None;
    let mut key = String::new();
    for line in text.lines().map(str::trim) {
        if line.starts_with("signal ") && line.contains("member=Update") {
            out.extend(current.take());
            current = Some((String::new(), false, 0.0));
        } else if let Some(cur) = current.as_mut() {
            if let Some(s) = line.strip_prefix("string \"").and_then(|s| s.strip_suffix('"')) {
                if s.starts_with("application://") {
                    cur.0 = s.to_string();
                } else {
                    key = s.to_string();
                }
            } else if let Some(v) = line.strip_prefix("variant") {
                let v = v.trim();
                match key.as_str() {
                    "progress-visible" => cur.1 = v.ends_with("true"),
                    "progress" => cur.2 = v.trim_start_matches("double").trim().parse().unwrap_or(-1.0),
                    _ => {}
                }
            }
        }
    }
    out.extend(current);
    out
}

async fn check_progress(cx: TestCx) -> Result<()> {
    use decibell_model::live::transfers::{AttachmentKind, DownloadContext, DownloadState, DownloadView, PendingStatus, PendingUpload};
    let view = |received: u64, state: DownloadState| DownloadView {
        id: "platform-test".into(),
        server_id: "1".into(),
        attachment_id: 1,
        filename: "progress.bin".into(),
        mime: "application/octet-stream".into(),
        kind: AttachmentKind::Document,
        total_bytes: 1000,
        received_bytes: received,
        state,
        error: None,
        started_at: 0,
        finished_at: None,
        context: DownloadContext::default(),
        speed_bps: 0.,
        eta_s: -1,
        missing: false,
        encrypted: false,
    };
    // The downloads manager attaches once the core runs and lists its own (none): after that.
    wait_for(&cx, Duration::from_secs(30), "the downloads manager", |cx| model(cx).live.transfers.read(cx).control().is_some()).await?;
    cx.sleep(Duration::from_millis(300)).await;
    // A download 0 → 25 → 50 %, paused, finished: each step after the 250 ms floor.
    for (received, state) in [(0, DownloadState::Active), (250, DownloadState::Active), (500, DownloadState::Active), (500, DownloadState::Paused), (1000, DownloadState::Done)] {
        cx.update(|_, cx| model(cx).live.transfers.update(cx, |t, cx| t.upsert_download(view(received, state), cx)))?;
        cx.sleep(Duration::from_millis(350)).await;
    }
    cx.update(|_, cx| model(cx).live.transfers.update(cx, |t, cx| t.remove_downloads(&["platform-test".to_string()], cx)))?;
    cx.sleep(Duration::from_millis(300)).await;
    say(format_args!("progress done"));

    // The quit guard: closing with an upload in flight asks and keeps the window.
    let upload = PendingUpload {
        pending_id: "platform-upload".into(),
        server: decibell_model::ServerId::from_central(1),
        channel: "general".into(),
        filename: "big.bin".into(),
        kind: AttachmentKind::Document,
        status: PendingStatus::Uploading,
        paused: false,
        cancelled: false,
        error: None,
        preview: None,
        attachment_id: None,
        total_bytes: 100,
        transferred_bytes: 10,
    };
    cx.update(|_, cx| model(cx).live.transfers.update(cx, |t, cx| t.sync_uploads(std::slice::from_ref(&upload), |_, _| ("Daily Gaming".into(), "general".into()), cx)))?;
    let guard = cx.update(|_, cx| crate::quit_guard(cx))?.context("no quit guard")?;
    cx.update(decibell_ui::request_window_close)?;
    cx.sleep(Duration::from_millis(100)).await;
    let asking = cx.update(|_, cx| guard.read(cx).is_open())?;
    ensure!(asking, "closing with an upload in flight didn't ask");
    say(format_args!("guard asked, window kept"));
    cx.update(|_, cx| guard.update(cx, |g, cx| g.keep_uploading(cx)))?;
    // The updater's Restart quits through the guard too (`before-quit` asks): with the upload in
    // flight it asks and the restart waits; "Keep uploading" calls it off.
    let restart = std::rc::Rc::new(std::cell::Cell::new(false));
    let flag = restart.clone();
    cx.update(|_, cx| guard.update(cx, |g, cx| g.quit_app(move |_| flag.set(true), cx)))?;
    cx.sleep(Duration::from_millis(100)).await;
    ensure!(cx.update(|_, cx| guard.read(cx).is_open())?, "Restart to update with an upload in flight didn't ask");
    ensure!(!restart.get(), "Restart to update went ahead with an upload in flight");
    cx.update(|_, cx| guard.update(cx, |g, cx| g.keep_uploading(cx)))?;
    cx.sleep(Duration::from_millis(100)).await;
    ensure!(!restart.get(), "\"Keep uploading\" didn't call the restart off");
    say(format_args!("guard asked before the update's restart; Keep uploading called it off"));
    cx.update(|_, cx| {
        model(cx).live.transfers.update(cx, |t, cx| {
            t.sync_uploads(&[], |_, _| ("".into(), "".into()), cx);
            t.remove_upload("platform-upload", cx);
        })
    })?;
    let allowed = cx.update(|_, cx| guard.update(cx, |g, cx| g.allow_close(cx)))?;
    ensure!(allowed, "with nothing uploading the guard still holds the window");
    say(format_args!("guard lets the window close once nothing uploads"));
    Ok(())
}

// ── keyring-label ─────────────────────────────────────────────────────────────────────────────

/// A gnome-keyring of its own (home, data and runtime directories in `dir`), serving the Secret
/// Service on `bus` only, unlocked with a throwaway password; killed (its pid) when dropped.
struct ThrowawayKeyring {
    child: std::process::Child,
}

impl ThrowawayKeyring {
    fn start(dir: &Path, bus: &EmptyBus) -> Result<ThrowawayKeyring> {
        use std::io::Write as _;
        let run = dir.join("run");
        std::fs::create_dir_all(&run)?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            std::fs::set_permissions(&run, std::fs::Permissions::from_mode(0o700))?;
        }
        let mut child = Command::new("gnome-keyring-daemon")
            .args(["--foreground", "--components=secrets", "--unlock"])
            .env_clear()
            .env("PATH", std::env::var_os("PATH").unwrap_or_default())
            .env("HOME", dir.join("home"))
            .env("XDG_DATA_HOME", dir.join("home/data"))
            .env("XDG_RUNTIME_DIR", &run)
            .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .context("starting gnome-keyring-daemon")?;
        let mut stdin = child.stdin.take().context("piped")?;
        stdin.write_all(b"throwaway")?;
        drop(stdin);
        Ok(ThrowawayKeyring { child })
    }
}

impl Drop for ThrowawayKeyring {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// `secret-tool search --all <attribute> <value>` on `bus`: the items' labels.
fn keyring_labels(bus: &EmptyBus, attribute: &str, value: &str) -> Result<Vec<String>> {
    let out = Command::new("secret-tool")
        .args(["search", "--all", attribute, value])
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .output()
        .context("secret-tool")?;
    let text = format!("{}{}", String::from_utf8_lossy(&out.stdout), String::from_utf8_lossy(&out.stderr));
    Ok(text.lines().filter_map(|l| l.strip_prefix("label = ").map(str::to_string)).collect())
}

async fn keyring_label(cx: TestCx) -> Result<()> {
    if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
        cx.log("SKIP: the Secret Service is Linux's");
        return Ok(());
    }
    let tools = ["gnome-keyring-daemon", "secret-tool", "dbus-daemon"];
    if let Some(missing) = tools.iter().find(|t| Command::new("sh").args(["-c", &format!("command -v {t}")]).output().is_ok_and(|o| !o.status.success())) {
        cx.log(format_args!("SKIP: {missing} is not installed (a throwaway Secret Service is needed: the session's keyring is never used)"));
        return Ok(());
    }
    let dir = TempDir::new("keyring")?;
    let bus = EmptyBus::start(&dir.0)?;
    let _keyring = ThrowawayKeyring::start(&dir.0, &bus)?;
    // The daemon owns org.freedesktop.secrets on the bus once it has unlocked its keyring.
    let started = Instant::now();
    while Command::new("secret-tool").args(["search", "--all", "nothing", "here"]).env("DBUS_SESSION_BUS_ADDRESS", &bus.address).output().is_ok_and(|o| !o.status.success() && !o.stderr.is_empty()) {
        ensure!(started.elapsed() < Duration::from_secs(10), "the throwaway keyring didn't come up");
        cx.sleep(Duration::from_millis(100)).await;
    }
    let profile = dir.0.join("profile");
    let mut env = app_env(&profile, &bus, None);
    env.push(("DECIBELL_DEV_KEYCHAIN", "real".to_string()));
    env.push(("DECIBELL_PLATFORM_TEST_DIR", dir.0.display().to_string()));
    let mut app = Child::spawn("app", &exe()?, &["--check", "platform-keyring"], &env, &[], &dir.0)?;
    app.wait_line(&cx, "platform-check: roundtrip ok", Duration::from_secs(40)).await?;
    let ours = keyring_labels(&bus, "url", "decibell://platform-test/keyring")?;
    let key = keyring_labels(&bus, "username", "decibell")?;
    cx.log(format_args!("items on the throwaway keyring: the test's {ours:?}, the at-rest key's {key:?}"));
    ensure!(ours == ["Decibell"], "the test's item is labelled {ours:?}");
    ensure!(!key.is_empty() && key.iter().all(|l| l == "Decibell"), "the at-rest key's item is labelled {key:?}");
    std::fs::write(dir.0.join("cue-delete"), b"")?;
    app.wait_line(&cx, "platform-check: deleted", Duration::from_secs(20)).await?;
    let status = app.wait_exit(&cx, Duration::from_secs(20)).await?;
    { let tail = app.tail(); ensure!(status.success(), "the app's check failed ({status}):\n{}", tail); }
    let left = keyring_labels(&bus, "url", "decibell://platform-test/keyring")?;
    ensure!(left.is_empty(), "the deleted item is still there: {left:?}");
    cx.log("written, read and deleted under \"Decibell\"; the at-rest key's item too; the session keyring untouched (private bus)");
    Ok(())
}

async fn check_keyring(cx: TestCx) -> Result<()> {
    let dir = test_dir()?;
    wait_for(&cx, Duration::from_secs(20), "the core", |_| decibell_model::core::is_running()).await?;
    let url = "decibell://platform-test/keyring";
    let write = cx.app().update(|cx| cx.write_credentials(url, "platform-test", b"s3cret"));
    write.await.context("writing")?;
    let read = cx.app().update(|cx| cx.read_credentials(url)).await.context("reading")?;
    ensure!(read == Some(("platform-test".to_string(), b"s3cret".to_vec())), "read back {read:?}");
    say(format_args!("roundtrip ok"));
    wait_file(&cx, &dir.join("cue-delete"), Duration::from_secs(30)).await?;
    cx.app().update(|cx| cx.delete_credentials(url)).await.context("deleting")?;
    let read = cx.app().update(|cx| cx.read_credentials(url)).await.context("reading")?;
    ensure!(read.is_none(), "still there after deleting: {read:?}");
    say(format_args!("deleted"));
    Ok(())
}

// ── window-state ───────────────────────────────────────────────────────────────────────────────

async fn window_state_test(cx: TestCx) -> Result<()> {
    if !dev_markers(&cx) {
        return Ok(());
    }
    let dir = TempDir::new("window-state")?;
    let bus = EmptyBus::start(&dir.0)?;
    let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some_and(|d| !d.is_empty());
    let sessions: Vec<(&str, Vec<(&str, String)>)> = if wayland {
        vec![("wayland", vec![]), ("x11", vec![("WAYLAND_DISPLAY", String::new())])]
    } else {
        vec![("x11", vec![])]
    };
    for (session, extra) in sessions {
        let profile = dir.0.join(session);
        let run = async |phase: &str| -> Result<Vec<String>> {
            let mut env = app_env(&profile, &bus, None);
            env.extend(extra.iter().cloned());
            env.push(("DECIBELL_CHECK_PHASE", phase.to_string()));
            let mut child = Child::spawn(&format!("{session}-{phase}"), &exe()?, &["--check", "platform-window-state"], &env, &[], &dir.0)?;
            let status = child.wait_exit(&cx, Duration::from_secs(40)).await?;
            { let tail = child.tail(); ensure!(status.success(), "{session} {phase}: failed ({status}):\n{}", tail); }
            child.drain();
            Ok(child.seen.iter().filter_map(|l| l.strip_prefix("platform-check: ").map(str::to_string)).collect())
        };
        let saved = || crate::window_state::load(&profile);
        let resized = run("resize").await?;
        let first = saved().context("nothing saved at quit")?;
        ensure!(!first.maximized, "saved maximised");
        cx.log(format_args!("{session}: resized ({}) → quit → saved {}x{}", resized.join("; "), first.width, first.height));
        let verified = run("verify").await?;
        let opened = verified.iter().find_map(|l| l.strip_prefix("opened ")).context("no size from the second start")?;
        ensure!(opened == format!("{}x{} windowed", first.width, first.height), "{session}: opened {opened}, saved {}x{}", first.width, first.height);
        let second = saved().context("nothing saved")?;
        ensure!(second.maximized, "{session}: maximised but saved {second:?}");
        cx.log(format_args!("{session}: started at {opened}; maximised → quit → saved maximised (restore {}x{})", second.width, second.height));
        let restored = run("maximized").await?;
        cx.log(format_args!("{session}: {}", restored.join("; ")));
        ensure!(restored.iter().any(|l| l == "opened maximized"), "{session}: not maximised at start");
        let third = saved().context("nothing saved")?;
        ensure!(
            !third.maximized && third.width == first.width && third.height == first.height,
            "{session}: unmaximised to {third:?}, expected {first:?}"
        );
        if session == "x11" {
            // A place on no screen (a monitor that is gone) is dropped: the window opens on one.
            let mut off = third;
            off.x = Some(100_000.);
            off.y = Some(100_000.);
            std::fs::write(profile.join(crate::window_state::FILE), serde_json::to_vec(&off)?)?;
            let placed = run("place").await?;
            let line = placed.iter().find_map(|l| l.strip_prefix("placed ")).context("no place")?;
            ensure!(line.ends_with("on screen"), "{line}");
            cx.log(format_args!("x11: saved at 100000,100000 → opened at {line}"));
        }
    }
    Ok(())
}

async fn check_window_state(cx: TestCx) -> Result<()> {
    use gpui::{WindowBounds, px, size};
    let phase = std::env::var("DECIBELL_CHECK_PHASE").unwrap_or_default();
    let describe = |b: WindowBounds| match b {
        WindowBounds::Windowed(b) => format!("{}x{} windowed", f32::from(b.size.width), f32::from(b.size.height)),
        WindowBounds::Maximized(b) => format!("{}x{} maximized", f32::from(b.size.width), f32::from(b.size.height)),
        WindowBounds::Fullscreen(b) => format!("{}x{} fullscreen", f32::from(b.size.width), f32::from(b.size.height)),
    };
    // As the state is saved (`window_state::current_state`).
    let state = |window: &gpui::Window| match crate::window_state::current_state(window) {
        Some(s) => format!("{}x{} {}", s.width, s.height, if s.maximized { "maximized" } else { "windowed" }),
        None => "fullscreen".to_string(),
    };
    cx.sleep(Duration::from_millis(500)).await;
    match phase.as_str() {
        "resize" => {
            let target = decibell_ui::outer_window_size(size(px(1000.), px(700.)));
            cx.update(|window, _| window.resize(target))?;
            let started = Instant::now();
            loop {
                let now = cx.update(|window, _| window.window_bounds())?;
                if let WindowBounds::Windowed(b) = now
                    && b.size == target
                {
                    say(format_args!("now {} (saved as {})", describe(now), cx.update(|window, _| state(window))?));
                    break;
                }
                ensure!(started.elapsed() < Duration::from_secs(10), "the window never took {target:?}: {now:?}");
                cx.sleep(Duration::from_millis(50)).await;
            }
        }
        "verify" => {
            let opened = cx.update(|window, _| state(window))?;
            say(format_args!("opened {opened}"));
            cx.update(|window, _| window.zoom_window())?;
            let started = Instant::now();
            while !cx.update(|window, _| window.is_maximized())? {
                ensure!(started.elapsed() < Duration::from_secs(10), "never maximised");
                cx.sleep(Duration::from_millis(50)).await;
            }
            cx.sleep(Duration::from_millis(300)).await;
            say(format_args!("maximized"));
        }
        "maximized" => {
            let started = Instant::now();
            while !cx.update(|window, _| window.is_maximized())? {
                ensure!(started.elapsed() < Duration::from_secs(5), "not maximised at start");
                cx.sleep(Duration::from_millis(50)).await;
            }
            say(format_args!("opened maximized"));
            cx.update(|window, _| window.zoom_window())?;
            let started = Instant::now();
            while cx.update(|window, _| window.is_maximized())? {
                ensure!(started.elapsed() < Duration::from_secs(10), "never unmaximised");
                cx.sleep(Duration::from_millis(50)).await;
            }
            cx.sleep(Duration::from_millis(300)).await;
            say(format_args!("restored {}", cx.update(|window, _| state(window))?));
        }
        "place" => {
            let (bounds, screens) = cx.update(|window, cx| (window.bounds(), cx.displays().iter().map(|d| d.bounds()).collect::<Vec<_>>()))?;
            let on = screens.iter().any(|s| s.intersects(&bounds));
            say(format_args!(
                "placed {},{} {}",
                f32::from(bounds.origin.x),
                f32::from(bounds.origin.y),
                if on { "on screen" } else { "off every screen" }
            ));
        }
        other => bail!("unknown phase {other:?}"),
    }
    Ok(())
}
