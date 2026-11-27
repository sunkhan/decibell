//! The avatar editor's self-test (development builds only).
//!
//! `avatar-editor` (parent) starts the app again as a child signed in to the local stack as a
//! throwaway account (`--check avatar-editor`: registered for the run, never one of the stack's) and
//! a `decibell-headless` watcher (another throwaway) that watches it. The child drops a generated
//! PNG on Settings → Account, saves in the editor, then a real JPEG whose EXIF says Orientation 6
//! (`assets/orientation-6.jpg`: stored 400 × 200, red left / blue right, so upright it is red on top),
//! then removes the picture; after each step the watcher must see the new version (the files in the
//! cue directory carry them; the parent hands the watcher's marks back as cue files).

use super::*;

use std::io::BufRead as _;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx, probes};

const MARK: &str = "AVATAR-MARK";
/// Stored 400 × 200 (red left, blue right) with EXIF Orientation 6 written by exiv2.
const ORIENTATION_6_JPEG: &[u8] = include_bytes!("../../assets/orientation-6.jpg");

pub fn selftests() -> Vec<SelfTest> {
    vec![SelfTest {
        name: "avatar-editor",
        about: "throwaway account: Settings → Account → a dropped PNG, then a real Orientation=6 JPEG (saved upright) through the picture editor; a second client sees each new version; Remove",
        run: |cx| Box::pin(parent(cx)),
    }]
}

pub fn checks() -> Vec<SelfTest> {
    vec![SelfTest { name: "avatar-editor", about: "(in the app) the avatar editor's flow", run: |cx| Box::pin(check(cx)) }]
}

struct Stack {
    central: String,
    password: String,
}

fn stack() -> Result<Option<Stack>> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let path = dev.join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json["central"].as_str().map(str::to_string)).context("no central address")?;
    let password = json["password"].as_str().context("accounts.json has no password")?.to_string();
    ensure!(loopback(&central), "DECIBELL_CENTRAL={central} is not loopback: the avatar test runs against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
}

fn loopback(central: &str) -> bool {
    let host = central.rsplit_once(':').map_or(central, |(h, _)| h);
    host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback())
}

fn random_hex(n: usize) -> String {
    use std::hash::{BuildHasher as _, Hasher as _};
    let mut h = std::collections::hash_map::RandomState::new().build_hasher();
    h.write_u128(std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
    let a = h.finish();
    let mut h2 = std::collections::hash_map::RandomState::new().build_hasher();
    h2.write_u64(a);
    format!("{a:016x}{:016x}", h2.finish())[..n].to_string()
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

/// A child process whose output lines are collected; killed if still running when dropped.
struct Child {
    name: &'static str,
    child: std::process::Child,
    lines: Arc<Mutex<Vec<String>>>,
}

impl Child {
    fn spawn(name: &'static str, mut command: Command) -> Result<Child> {
        command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        let mut child = command.spawn().with_context(|| format!("starting {name}"))?;
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
        Ok(Child { name, child, lines })
    }

    fn line_with(&self, needle: &str) -> Option<String> {
        self.lines.lock().unwrap_or_else(|e| e.into_inner()).iter().find(|l| l.contains(needle)).cloned()
    }

    fn relay(&self, cx: &TestCx, stderr: bool) {
        let lines = self.lines.lock().unwrap_or_else(|e| e.into_inner());
        for line in lines.iter().filter(|l| !l.starts_with("selftest:") && (stderr || !l.starts_with("stderr: "))) {
            if !line.starts_with("stderr: ") || stderr {
                cx.log(format_args!("│ {}: {line}", self.name));
            }
        }
    }

    fn ended(&mut self) -> Result<Option<std::process::ExitStatus>> {
        Ok(self.child.try_wait()?)
    }
}

impl Drop for Child {
    fn drop(&mut self) {
        if matches!(self.child.try_wait(), Ok(None)) {
            let _ = self.child.kill();
            let _ = self.child.wait();
        }
    }
}

async fn parent(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let exe = std::env::current_exe()?;
    let headless = exe.with_file_name("decibell-headless");
    ensure!(headless.exists(), "no decibell-headless beside {} (cargo build --release -p decibell-model --features local-central --bin decibell-headless)", exe.display());
    let profile = TempDir::new("avatar-editor")?;
    let watcher_profile = TempDir::new("avatar-editor-watcher")?;
    let cues = TempDir::new("avatar-editor-cues")?;
    let account = format!("a{}", random_hex(10));
    let password = format!("pw-{}", random_hex(12));
    cx.log(format_args!("throwaway account {account}"));

    let mut command = Command::new(&exe);
    command
        .arg("--check")
        .arg("avatar-editor")
        .env("DECIBELL_USER_DATA_DIR", &profile.0)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env("DECIBELL_TEST_ACCOUNT", &account)
        .env("DECIBELL_TEST_PASSWORD", &password)
        .env("DECIBELL_AVATAR_CUES", &cues.0)
        .env_remove("DECIBELL_PERF");
    let mut app = Child::spawn("avatar-editor", command)?;

    let started = Instant::now();
    let limit = Duration::from_secs(180);
    let mut watcher: Option<Child> = None;
    let mut relayed = std::collections::HashSet::new();
    loop {
        if watcher.is_none()
            && let Some(line) = app.line_with(&format!("{MARK} uid "))
        {
            let uid = line.rsplit(' ').next().unwrap_or_default().trim().to_string();
            let user = format!("#{uid}");
            let name = format!("w{}", random_hex(10));
            let pw = format!("pw-{}", random_hex(12));
            let mut command = Command::new(&headless);
            command
                .env("DECIBELL_CENTRAL", &stack.central)
                .env("DECIBELL_USER_DATA_DIR", &watcher_profile.0)
                .args(["--user-data-dir"])
                .arg(&watcher_profile.0)
                .args(["--timeout", "90", "register", &name, &pw, "login-as", &name, &pw, "wait", "logged-in"])
                .args(["watch", &user, "print", "watching"])
                .args(["wait", "avatar", &user])
                .arg(format!("@{}", cues.0.join("v1").display()))
                .args(["print", "seen-1", "wait", "avatar", &user])
                .arg(format!("@{}", cues.0.join("v2").display()))
                .args(["print", "seen-2", "wait", "avatar", &user, "-", "print", "seen-none", "logout", "wait", "logged-out"]);
            cx.log(format_args!("the child is signed in as uid {uid}: a second client ({name}) watches it"));
            watcher = Some(Child::spawn("watcher", command)?);
        }
        // The watcher's marks become the child's cue files.
        if let Some(w) = &watcher {
            for cue in ["watching", "seen-1", "seen-2", "seen-none"] {
                if !relayed.contains(cue) && w.line_with(&format!("MARK {cue}")).is_some() {
                    std::fs::write(cues.0.join(cue), b"1")?;
                    relayed.insert(cue);
                    cx.log(format_args!("watcher: {cue}"));
                }
            }
        }
        if let Some(status) = app.ended()? {
            app.relay(&cx, !status.success());
            if !status.success() {
                if let Some(w) = &watcher {
                    w.relay(&cx, true);
                }
                bail!("the check failed in the app ({status})");
            }
            break;
        }
        if let Some(w) = &mut watcher
            && let Some(status) = w.ended()?
            && !status.success()
        {
            w.relay(&cx, true);
            app.relay(&cx, true);
            bail!("the watcher failed ({status})");
        }
        if started.elapsed() > limit {
            app.relay(&cx, true);
            if let Some(w) = &watcher {
                w.relay(&cx, true);
            }
            bail!("still running after {} s", limit.as_secs());
        }
        cx.sleep(Duration::from_millis(100)).await;
    }
    // The watcher logs out after it saw the removal.
    let mut watcher = watcher.context("the child never said who it is")?;
    let wait_started = Instant::now();
    loop {
        if let Some(status) = watcher.ended()? {
            watcher.relay(&cx, !status.success());
            ensure!(status.success(), "the watcher failed ({status})");
            break;
        }
        ensure!(wait_started.elapsed() < Duration::from_secs(60), "the watcher is still running");
        cx.sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

// ── The check, inside the app ─────────────────────────────────────────────────────────────────

fn mark(what: impl std::fmt::Display) {
    println!("{MARK} {what}");
}

fn env(key: &str) -> Result<String> {
    std::env::var(key).with_context(|| format!("{key} is not set"))
}

async fn wait_for(cx: &TestCx, limit: Duration, what: &str, ready: impl Fn(&mut gpui::App) -> bool) -> Result<()> {
    let started = Instant::now();
    loop {
        if cx.update(|_, cx| ready(cx))? {
            return Ok(());
        }
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {what}", limit.as_secs());
        cx.sleep(Duration::from_millis(50)).await;
    }
}

async fn wait_cue(cx: &TestCx, cues: &Path, cue: &str) -> Result<()> {
    let path = cues.join(cue);
    let started = Instant::now();
    while !path.exists() {
        ensure!(started.elapsed() < Duration::from_secs(90), "the parent never cued {cue}");
        cx.sleep(Duration::from_millis(100)).await;
    }
    Ok(())
}

fn own_version(cx: &gpui::App) -> SharedString {
    let model = Model::global(cx);
    let uid = model.auth.read(cx).uid;
    model.live.presence.avatars.read(cx).get(uid).map(|e| e.version.clone()).unwrap_or_default()
}

async fn check(cx: TestCx) -> Result<()> {
    ensure!(
        cfg!(any(debug_assertions, feature = "dev-local")),
        "this build ignores DECIBELL_CENTRAL (a release build without dev-local): refusing to talk to any server"
    );
    let central = env("DECIBELL_CENTRAL")?;
    ensure!(loopback(&central), "DECIBELL_CENTRAL={central} is not loopback: refusing");
    let cues = PathBuf::from(env("DECIBELL_AVATAR_CUES")?);
    let (name, password) = (env("DECIBELL_TEST_ACCOUNT")?, env("DECIBELL_TEST_PASSWORD")?);
    let model = cx.update(|_, cx| Model::global(cx).clone())?;

    // Register the throwaway account and sign in.
    wait_for(&cx, Duration::from_secs(20), "the core to boot", |cx| Model::global(cx).auth.read(cx).booted).await?;
    {
        let (n, e, p) = (name.clone(), format!("{name}@example.test"), password.clone());
        cx.update(move |_, cx| actions::auth::register(n, e, p, cx))?;
    }
    wait_for(&cx, Duration::from_secs(20), "the registration's answer", |cx| Model::global(cx).auth.read(cx).register_result.is_some()).await?;
    let result = cx.update(|_, cx| Model::global(cx).auth.read(cx).register_result.clone())?.context("no answer")?;
    ensure!(result.success, "registering {name} was refused: {}", result.message);
    {
        let (n, p) = (name.clone(), password.clone());
        cx.update(move |_, cx| actions::auth::sign_in(n, p, cx))?;
    }
    wait_for(&cx, Duration::from_secs(30), "the sign-in", |cx| Model::global(cx).auth.read(cx).is_authenticated).await?;
    let uid = cx.update(|_, cx| model.auth.read(cx).uid)?;
    cx.log(format_args!("registered and signed in as {name} (uid {uid})"));
    mark(format_args!("uid {uid}"));
    let result = steps(&cx, &model, &cues).await;
    // Forget this device at central (and leave the account as the test found it: no picture).
    let _ = cx.update(|_, cx| actions::auth::log_out(cx));
    let _ = wait_for(&cx, Duration::from_secs(15), "the sign-out", |cx| !Model::global(cx).auth.read(cx).is_authenticated).await;
    result
}

async fn steps(cx: &TestCx, model: &Model, cues: &Path) -> Result<()> {
    use decibell_model::stores::overlays::modal;
    wait_cue(cx, cues, "watching").await?;
    cx.update(|_, cx| {
        model.nav.update(cx, |nav, cx| nav.set_settings_tab("account", cx));
        model.overlays.update(cx, |o, cx| o.open_modal(modal::SETTINGS, cx));
    })?;
    wait_for(cx, Duration::from_secs(10), "Settings → Account", |_| probes::bounds("account-card").is_some()).await?;
    cx.settle().await.ok();

    // 1. A generated PNG, dropped on the card.
    let files = TempDir::new("avatar-editor-files")?;
    let png = files.0.join("generated.png");
    {
        let mut image = image::RgbImage::new(320, 240);
        for (x, y, p) in image.enumerate_pixels_mut() {
            *p = image::Rgb([(x * 255 / 320) as u8, (y * 255 / 240) as u8, 160]);
        }
        image.save(&png).context("writing the PNG")?;
    }
    let v1 = save_dropped(cx, &png, None).await?;
    cx.log(format_args!("the PNG saved: our avatar's version is {v1}"));
    std::fs::write(cues.join("v1"), v1.as_bytes())?;
    wait_cue(cx, cues, "seen-1").await?;
    cx.log("the second client saw it");

    // 2. A real JPEG whose EXIF says Orientation 6: shown and saved upright.
    let jpeg = files.0.join("photo.jpg");
    std::fs::write(&jpeg, ORIENTATION_6_JPEG)?;
    let v2 = save_dropped(cx, &jpeg, Some((200., 400.))).await?;
    ensure!(v2 != v1, "the second picture has the first one's version");
    // Our picture as every view draws it: fetched by the avatar store, the JPEG central keeps.
    let uid = cx.update(|_, cx| model.auth.read(cx).uid)?;
    wait_for(cx, Duration::from_secs(15), "our new picture fetched", |cx| {
        Model::global(cx).live.presence.avatars.read(cx).get(uid).is_some_and(|e| e.version == v2 && e.image.is_some())
    })
    .await?;
    let bytes = cx.update(|_, cx| model.live.presence.avatars.read(cx).image(uid).map(|i| i.bytes().to_vec()))?.context("no picture")?;
    let decoded = image::load_from_memory(&bytes).context("decoding our avatar")?.to_rgb8();
    let (w, h) = decoded.dimensions();
    let at = |x: u32, y: u32| decoded.get_pixel(x * w / 256, y * h / 256).0;
    let (top, bottom) = (at(192, 32), at(64, 224));
    cx.log(format_args!("the saved photo is {w} × {h}: top right {top:?}, bottom left {bottom:?}"));
    let red = |p: [u8; 3]| p[0] > 150 && p[2] < 110;
    let blue = |p: [u8; 3]| p[2] > 150 && p[0] < 110;
    ensure!((w, h) == (256, 256), "the avatar is {w} × {h}, not 256 × 256");
    ensure!(red(top) && blue(bottom), "the photo was saved in its stored layout, not upright (top {top:?}, bottom {bottom:?})");
    std::fs::write(cues.join("v2"), v2.as_bytes())?;
    wait_cue(cx, cues, "seen-2").await?;
    cx.log("saved upright (red on top, blue below); the second client saw the new version");

    // 3. Remove.
    cx.settle().await.ok();
    let remove = probes::bounds("account-remove-picture").context("no Remove button with a picture set")?;
    cx.click(remove.center())?;
    wait_for(cx, Duration::from_secs(15), "the picture removed", |cx| own_version(cx).is_empty()).await?;
    wait_cue(cx, cues, "seen-none").await?;
    cx.log("Remove: our version is empty; the second client saw the removal");
    cx.update(|_, cx| model.overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    Ok(())
}

/// Drop `path` on the Account card, wait for the editor to read it (its natural size `nat`, if
/// given, is the picture as shown: after EXIF orientation), press Save and wait for the upload:
/// the editor closes and our avatar has a new version.
async fn save_dropped(cx: &TestCx, path: &Path, nat: Option<(f32, f32)>) -> Result<SharedString> {
    use gpui::{ExternalPaths, FileDropEvent, InputEvent as _};
    let before = cx.update(|_, cx| own_version(cx))?;
    let card = probes::bounds("account-card").context("the Account card")?;
    let at = card.center();
    let paths = ExternalPaths([path.to_path_buf()].into_iter().collect());
    for event in [FileDropEvent::Entered { position: at, paths }, FileDropEvent::Pending { position: at }, FileDropEvent::Submit { position: at }] {
        cx.update(|window, cx| {
            window.dispatch_event(event.to_platform_input(), cx);
        })?;
        cx.sleep(Duration::from_millis(60)).await;
    }
    wait_for(cx, Duration::from_secs(15), "the editor to read the file", |cx| {
        shown_editor().is_some_and(|e| e.read(cx).picture().is_some() || e.read(cx).error().is_some())
    })
    .await?;
    let editor = shown_editor().context("the editor")?;
    if let Some(error) = cx.update(|_, cx| editor.read(cx).error().cloned())? {
        bail!("the editor refused {}: {error}", path.display());
    }
    let shown = cx.update(|_, cx| editor.read(cx).picture().map(|p| (p.nat.w, p.nat.h)))?.context("no picture")?;
    cx.log(format_args!("the editor read {} ({} × {} as shown)", path.file_name().unwrap_or_default().to_string_lossy(), shown.0, shown.1));
    if let Some(nat) = nat {
        ensure!(shown == nat, "the picture is shown {} × {}, expected {} × {} (EXIF orientation)", shown.0, shown.1, nat.0, nat.1);
    }
    cx.settle().await.ok();
    let save = probes::bounds("picture-save").context("the editor's Save")?;
    cx.click(save.center())?;
    wait_for(cx, Duration::from_secs(20), "the save (the editor closes)", |_| shown_editor().is_none()).await?;
    wait_for(cx, Duration::from_secs(10), "our avatar's new version", |cx| {
        let now = own_version(cx);
        !now.is_empty() && now != before
    })
    .await?;
    cx.update(|_, cx| own_version(cx))
}
