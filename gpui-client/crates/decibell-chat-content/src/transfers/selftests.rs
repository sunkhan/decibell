//! `content-download`: against the local stack (`tools/dev/README.md`), signed in as `perf2` in a
//! child app with its own temporary profile (`decibell --check content-download --login perf2`,
//! the shell's pattern): download Daily Gaming `#clips`' seeded `patch-notes.txt` through the
//! panel's path and compare it with the bytes the seed uploaded (the community's stored file, and
//! the seed's literal text); download `ultrawide.png` under the 100 KiB/s cap, pause it from its
//! row, resume it, let it finish and compare it the same way; check "Show in folder" names a file
//! that exists; then delete both files and remove both rows. Downloads go to a folder inside the
//! temporary profile (Settings' download folder), never the user's Downloads.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_attachments::manager::RevealTarget;
use decibell_harness::{SelfTest, TestCx};
use decibell_model::chat::Attachment;
use decibell_model::live::transfers::{DownloadState, DownloadView, TransfersTab};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{ChannelId, Model, ServerId, actions};
use gpui::App;

const ACCOUNT: &str = "perf2";
const CHECK: &str = "content-download";
/// What `tools/dev/seed.py` uploads as `patch-notes.txt`.
const PATCH_NOTES: &str =
    "Patch 0.9.4\n- voice: smoother jitter buffer\n- streams: 5 fps slides mode\n- chat: link previews for clips\n";

pub fn selftests() -> Vec<SelfTest> {
    vec![SelfTest {
        name: CHECK,
        about: "local stack: download #clips' patch-notes.txt and ultrawide.png through the panel (pause, resume), bytes as uploaded",
        run: |cx| Box::pin(parent(cx)),
    }]
}

pub fn checks() -> Vec<SelfTest> {
    vec![SelfTest { name: CHECK, about: "(in the app) the downloads of content-download", run: |cx| Box::pin(check(cx)) }]
}

// ── The parent: one child app ─────────────────────────────────────────────────────────────

fn dev_dir() -> PathBuf {
    std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"))
}

/// (central, password) when the stack is up; an error for a non-loopback central.
fn stack() -> Result<Option<(String, String)>> {
    let path = dev_dir().join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let json: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| json["central"].as_str().map(str::to_string)).context("no central address")?;
    let password = json["password"].as_str().context("accounts.json has no password")?.to_string();
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: {CHECK} runs against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some((central, password)))
}

async fn parent(cx: TestCx) -> Result<()> {
    let Some((central, password)) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = std::env::temp_dir().join(format!("decibell-{CHECK}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(&profile)?;
    let result = run_child(&cx, &central, &password, &profile).await;
    let _ = std::fs::remove_dir_all(&profile);
    result
}

async fn run_child(cx: &TestCx, central: &str, password: &str, profile: &Path) -> Result<()> {
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--check", CHECK, "--login", ACCOUNT])
        .env("DECIBELL_USER_DATA_DIR", profile)
        .env("DECIBELL_CENTRAL", central)
        .env("DECIBELL_DEV_PASSWORD", password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let started = Instant::now();
    let mut child = command.spawn().context("starting the app")?;
    let (stdout, stderr) = (child.stdout.take().expect("piped"), child.stderr.take().expect("piped"));
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
            (status, out, err.and_then(|h| h.join().ok()).unwrap_or_default())
        })
        .await;
    let status = status?;
    for line in out.iter().filter(|l| !l.starts_with("selftest:")) {
        cx.log(format_args!("│ {line}"));
    }
    if err.iter().any(|l| l.contains("--login refused")) && err.iter().any(|l| l.contains("dev-local")) {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    }
    if !status.success() {
        for line in err.iter().rev().take(12).rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{CHECK} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(())
}

// ── The check, inside the signed-in app ─────────────────────────────────────────────────────

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

const DAILY_GAMING: i32 = 1;

fn clips_attachment(name: &str, cx: &App) -> Option<(i64, Attachment)> {
    let model = model(cx);
    let server = ServerId::from_central(DAILY_GAMING);
    let slice = model.chat.channels.read(cx).get(server, "clips")?.clone();
    let slice = slice.read(cx);
    slice.rows().iter().find_map(|m| m.attachments.iter().find(|a| &*a.filename == name).map(|a| (m.id, a.clone())))
}

fn download_of(attachment_id: i64, cx: &App) -> Option<DownloadView> {
    model(cx).live.transfers.read(cx).downloads().iter().find(|d| d.attachment_id == attachment_id).cloned()
}

fn sha256(path: &Path) -> String {
    Command::new("sha256sum")
        .arg(path)
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .and_then(|s| s.split_whitespace().next().map(str::to_string))
        .unwrap_or_else(|| "?".into())
}

/// The file the seed uploaded, as the community stored it.
fn stored(attachment: &Attachment) -> PathBuf {
    dev_dir().join(format!("run/communities/gaming/attachments/clips/{}_{}", attachment.id, attachment.filename))
}

async fn check(cx: TestCx) -> Result<()> {
    let server = ServerId::from_central(DAILY_GAMING);
    let took = wait_for(&cx, Duration::from_secs(30), "Daily Gaming connected and the downloads manager", move |cx| {
        let model = model(cx);
        model.connections.read(cx).is_connected(server) && model.live.transfers.read(cx).control().is_some()
    })
    .await?;
    cx.log(format_args!("signed in, Daily Gaming connected, downloads manager attached ({:.1} s)", took.as_secs_f64()));
    let profile = PathBuf::from(std::env::var_os("DECIBELL_USER_DATA_DIR").context("no profile")?);
    let folder = profile.join("test-downloads");
    std::fs::create_dir_all(&folder)?;
    cx.update(|_, cx| {
        actions::transfers::set_download_dir(folder.to_string_lossy().into_owned(), cx);
        // 100 KiB/s (the lowest cap): the picture takes seconds, long enough to pause.
        model(cx).settings.update(cx, |s, cx| s.change(|v| v.download_limit_bps = decibell_attachments::MIN_LIMIT_BPS, cx));
    })?;
    // #clips, as the channel list's click opens it.
    cx.update(|_, cx| {
        let model = model(cx);
        let channel: ChannelId = "clips".into();
        actions::servers::open_server(server, cx);
        model.nav.update(cx, |nav, cx| {
            nav.set_active_channel(Some(channel.clone()), cx);
            nav.set_active_view(ActiveView::Server, cx);
        });
        actions::chat::open_channel(server, channel, cx);
    })?;
    wait_for(&cx, Duration::from_secs(20), "#clips' seeded files", |cx| {
        clips_attachment("patch-notes.txt", cx).is_some() && clips_attachment("ultrawide.png", cx).is_some()
    })
    .await?;
    let (_, notes) = cx.update(|_, cx| clips_attachment("patch-notes.txt", cx))?.context("patch-notes.txt")?;
    let (_, picture) = cx.update(|_, cx| clips_attachment("ultrawide.png", cx))?.context("ultrawide.png")?;
    cx.log(format_args!("found patch-notes.txt (#{}, {} B) and ultrawide.png (#{}, {} B)", notes.id, notes.size_bytes, picture.id, picture.size_bytes));

    // The panel, by its shortcut.
    ensure!(cx.keystroke("ctrl-j")?, "Ctrl+J did nothing");
    wait_for(&cx, Duration::from_secs(5), "the panel", |cx| model(cx).live.transfers.read(cx).panel_open).await?;
    cx.update(|_, cx| model(cx).live.transfers.update(cx, |t, cx| t.set_tab(TransfersTab::Downloads, cx)))?;

    // 1. The document, through the download button's path.
    let started = cx.update(|_, cx| crate::downloads::start(server, &notes, false, cx))?;
    ensure!(started.await, "the document's download didn't start");
    let notes_id = notes.id;
    wait_for(&cx, Duration::from_secs(20), "patch-notes.txt done", move |cx| download_of(notes_id, cx).is_some_and(|d| d.state == DownloadState::Done))
        .await?;
    let d = cx.update(|_, cx| download_of(notes_id, cx))?.context("the row")?;
    let path = cx.update(|_, cx| super::manager(cx).context("manager")?.open_path(&d.id).map_err(anyhow::Error::from))??;
    let got = std::fs::read(&path)?;
    ensure!(got == PATCH_NOTES.as_bytes(), "patch-notes.txt differs from what the seed uploaded ({} B)", got.len());
    let reference = stored(&notes);
    if reference.exists() {
        ensure!(std::fs::read(&reference)? == got, "patch-notes.txt differs from the community's stored file");
    }
    cx.log(format_args!("patch-notes.txt: {} → {}, sha256 {} = stored {}", d.status_line(), path.display(), sha256(&path), sha256(&reference)));

    // 2. The picture: pause it from its row, resume it, let it finish.
    let started = cx.update(|_, cx| crate::downloads::start(server, &picture, false, cx))?;
    ensure!(started.await, "the picture's download didn't start");
    let picture_id = picture.id;
    wait_for(&cx, Duration::from_secs(10), "ultrawide.png under way", move |cx| {
        download_of(picture_id, cx).is_some_and(|d| d.state == DownloadState::Active && d.received_bytes > 0)
    })
    .await?;
    let row = cx.update(|_, cx| download_of(picture_id, cx))?.context("the row")?;
    click_probe(&cx, &format!("transfer-{}-Pause", row.id)).await?;
    wait_for(&cx, Duration::from_secs(5), "paused", move |cx| download_of(picture_id, cx).is_some_and(|d| d.state == DownloadState::Paused)).await?;
    let paused_at = cx.update(|_, cx| download_of(picture_id, cx))?.context("the row")?.received_bytes;
    cx.sleep(Duration::from_millis(600)).await;
    let still = cx.update(|_, cx| download_of(picture_id, cx))?.context("the row")?;
    ensure!(still.state == DownloadState::Paused && still.received_bytes == paused_at, "a paused download moved ({paused_at} → {})", still.received_bytes);
    ensure!(paused_at < picture.size_bytes as u64, "it finished before it could be paused");
    cx.log(format_args!("paused at {paused_at} of {} B: {}", picture.size_bytes, still.status_line()));
    click_probe(&cx, &format!("transfer-{}-Resume", row.id)).await?;
    wait_for(&cx, Duration::from_secs(30), "ultrawide.png done", move |cx| download_of(picture_id, cx).is_some_and(|d| d.state == DownloadState::Done))
        .await?;
    let done = cx.update(|_, cx| download_of(picture_id, cx))?.context("the row")?;
    let picture_path = cx.update(|_, cx| super::manager(cx).context("manager")?.open_path(&done.id).map_err(anyhow::Error::from))??;
    let reference = stored(&picture);
    ensure!(reference.exists(), "the community's stored file isn't at {}", reference.display());
    ensure!(std::fs::read(&picture_path)? == std::fs::read(&reference)?, "ultrawide.png differs from the uploaded file");
    cx.log(format_args!("ultrawide.png resumed and finished: sha256 {} = stored {}", sha256(&picture_path), sha256(&reference)));

    // 3. "Show in folder" reveals a file that exists.
    let reveal = cx.update(|_, cx| super::manager(cx).and_then(|m| m.reveal_path(&done.id)))?;
    match reveal {
        Some(RevealTarget::Item(item)) => ensure!(item.exists(), "Show in folder names {}, which doesn't exist", item.display()),
        other => bail!("Show in folder would reveal {other:?}"),
    }
    cx.log(format_args!("Show in folder: {}", picture_path.display()));

    // 4. Leave nothing: the files, the rows, the panel.
    std::fs::remove_file(&path)?;
    std::fs::remove_file(&picture_path)?;
    let ids = vec![d.id.clone(), done.id.clone()];
    cx.update(|_, cx| {
        if let Some(control) = model(cx).live.transfers.read(cx).control() {
            control.remove(&ids);
        }
        model(cx).live.transfers.update(cx, |t, cx| t.close_panel(cx));
    })?;
    wait_for(&cx, Duration::from_secs(5), "the rows gone", move |cx| download_of(notes_id, cx).is_none() && download_of(picture_id, cx).is_none()).await?;
    let _ = std::fs::remove_dir(&folder);
    cx.log("files deleted, rows removed");

    // At rest with the panel shut: what the window draws (the button runs no timer or
    // animation then; anything counted here is someone else's).
    let _ = cx.settle().await;
    let before = cx.frames()?;
    cx.sleep(Duration::from_secs(3)).await;
    let after = cx.frames()?;
    cx.log(format_args!(
        "at rest, panel shut, 3 s: {} frames drawn, {} presented",
        after.drawn - before.drawn,
        after.presented - before.presented
    ));
    Ok(())
}

async fn click_probe(cx: &TestCx, key: &str) -> Result<()> {
    let key_owned = key.to_string();
    wait_for(cx, Duration::from_secs(5), &format!("{key} on screen"), move |_| decibell_shell::probes::bounds(&key_owned).is_some()).await?;
    let bounds = decibell_shell::probes::bounds(key).context("probe vanished")?;
    cx.click(bounds.center())?;
    Ok(())
}

trait StatusLine {
    fn status_line(&self) -> String;
}

impl StatusLine for DownloadView {
    fn status_line(&self) -> String {
        super::format::download_status(self, 0)
    }
}
