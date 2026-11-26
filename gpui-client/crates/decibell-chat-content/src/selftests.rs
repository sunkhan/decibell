//! The content self-tests, against the local test stack (`tools/dev/README.md`) as `perf2`.
//!
//! Each `content-*` self-test starts the app again in a child — `decibell --check content-… --login
//! perf2` with its own temporary profile, the stack's `DECIBELL_CENTRAL` and `DECIBELL_TEST_PROBES=1`
//! — and passes when the check inside it does (the shell's pattern). They need a `dev-local` binary,
//! refuse a non-loopback `DECIBELL_CENTRAL` and skip, saying so, when the stack is down. Nothing they
//! do stays on the stack: the GIF upload is deleted again, the link and invite messages are routed
//! into the client only, nothing is joined.

use std::io::{BufRead as _, Read as _, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{ChannelId, Model, ServerId, actions};
use gpui::{App, Entity, Modifiers, point, px};

use crate::pictures::{Look, Pictures};

const ACCOUNT: &str = "perf2";

pub fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "content-images-live",
            about: "#general (local stack): placeholders first, every visible picture loads, no row moves, frames only for the swaps",
            run: |cx| Box::pin(parent(cx, "content-images-live")),
        },
        SelfTest {
            name: "content-scroll-images",
            about: "#general scrolled into history at 3000 px/s with its pictures: frame intervals and missed frames",
            run: |cx| Box::pin(parent(cx, "content-scroll-images")),
        },
        SelfTest {
            name: "content-cache-bound",
            about: "500 picture rows of #general: decoded pictures stay in budget, memory returns after leaving",
            run: |cx| Box::pin(parent(cx, "content-cache-bound")),
        },
        SelfTest {
            name: "content-gif",
            about: "an uploaded GIF animates only while visible and the window shows it (then deleted)",
            run: |cx| Box::pin(parent(cx, "content-gif")),
        },
        SelfTest {
            name: "content-viewer",
            about: "the viewer over #clips' gallery: open, zoom, pan, next / previous, copy, Esc",
            run: |cx| Box::pin(parent(cx, "content-viewer")),
        },
        SelfTest {
            name: "content-embed",
            about: "a link to a loopback page unfurls into a card; the Privacy toggle hides it",
            run: |cx| Box::pin(parent(cx, "content-embed")),
        },
        SelfTest {
            name: "content-invite",
            about: "an invite link to Design Lab renders its card from central (not joined)",
            run: |cx| Box::pin(parent(cx, "content-invite")),
        },
    ]
}

pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "content-images-live", about: "(in the app)", run: |cx| Box::pin(check_images_live(cx)) },
        SelfTest { name: "content-scroll-images", about: "(in the app)", run: |cx| Box::pin(check_scroll(cx)) },
        SelfTest { name: "content-cache-bound", about: "(in the app)", run: |cx| Box::pin(check_cache_bound(cx)) },
        SelfTest { name: "content-gif", about: "(in the app)", run: |cx| Box::pin(check_gif(cx)) },
        SelfTest { name: "content-viewer", about: "(in the app)", run: |cx| Box::pin(check_viewer(cx)) },
        SelfTest { name: "content-embed", about: "(in the app)", run: |cx| Box::pin(check_embed(cx)) },
        SelfTest { name: "content-invite", about: "(in the app)", run: |cx| Box::pin(check_invite(cx)) },
    ]
}

// ── The parents ─────────────────────────────────────────────────────────────────────────────

struct Stack {
    central: String,
    password: String,
    accounts: serde_json::Value,
}

fn stack() -> Result<Option<Stack>> {
    let dev = std::env::var_os("DECIBELL_DEV_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from(std::env::var_os("HOME").unwrap_or_default()).join(".cache/decibell-dev"));
    let path = dev.join("run/accounts.json");
    let Ok(text) = std::fs::read_to_string(&path) else { return Ok(None) };
    let accounts: serde_json::Value = serde_json::from_str(&text).with_context(|| format!("parsing {}", path.display()))?;
    let central = std::env::var("DECIBELL_CENTRAL").ok().or_else(|| accounts["central"].as_str().map(str::to_string)).context("no central address")?;
    let password = accounts["password"].as_str().context("accounts.json has no password")?.to_string();
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the content tests run against the local stack only");
    let reachable = central.parse::<std::net::SocketAddr>().ok().is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password, accounts }))
}

struct Profile(PathBuf);

impl Drop for Profile {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

async fn parent(cx: TestCx, check: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile(std::env::temp_dir().join(format!("decibell-{check}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&profile.0);
    std::fs::create_dir_all(&profile.0)?;
    let design_invite = stack.accounts["communities"].as_array().and_then(|c| c.iter().find(|c| c["name"] == "Design Lab")).and_then(|c| c["invite"].as_str()).unwrap_or_default().to_string();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--check", check, "--login", ACCOUNT])
        .env("DECIBELL_USER_DATA_DIR", &profile.0)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env("DECIBELL_TEST_INVITE", design_invite)
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
    for line in out.iter().filter(|l| !l.starts_with("selftest:")) {
        cx.log(format_args!("│ {line}"));
    }
    if err.iter().any(|l| l.contains("--login refused")) && err.iter().any(|l| l.contains("dev-local")) {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    }
    let status = status?;
    if !status.success() {
        for line in err.iter().rev().take(16).rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{check} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(())
}

// ── Helpers inside the app ──────────────────────────────────────────────────────────────────

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

fn daily_gaming() -> ServerId {
    ServerId::from_central(1)
}

async fn wait_for(cx: &TestCx, limit: Duration, what: &str, ready: impl Fn(&mut App) -> bool) -> Result<Duration> {
    let started = Instant::now();
    loop {
        if cx.update(|_, cx| ready(cx))? {
            return Ok(started.elapsed());
        }
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {what}", limit.as_secs());
        cx.sleep(Duration::from_millis(25)).await;
    }
}

/// The list the chat pane shows.
fn shown(cx: &App) -> Option<Entity<decibell_chat::MessageList>> {
    decibell_chat::shown_list(cx)
}

/// Signed in, Daily Gaming connected, `name` open as the channel list opens it; the list.
async fn open_channel(cx: &TestCx, name: &str) -> Result<(ChannelId, Entity<decibell_chat::MessageList>)> {
    let t = wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.log(format_args!("signed in as {ACCOUNT} after {} ms", t.as_millis()));
    wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    // The attachment endpoint arrives with the community's answer.
    wait_for(cx, Duration::from_secs(15), "Daily Gaming's attachment endpoint", |_| {
        decibell_model::chat::composer::CoreAttachmentService::attachments().is_some_and(|svc| svc.target(&daily_gaming().wire()).is_some())
    })
    .await?;
    wait_for(cx, Duration::from_secs(15), "Daily Gaming's channels", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.channels.read(cx).list().iter().any(|c| &*c.name == name))
    })
    .await?;
    let channel = cx.update(|_, cx| -> Result<ChannelId> {
        let model = model(cx);
        let community = model.community(daily_gaming(), cx).context("not connected")?;
        let channel = community.channels.read(cx).list().iter().find(|c| &*c.name == name).map(|c| c.id.clone()).with_context(|| format!("no #{name}"))?;
        actions::servers::open_server(daily_gaming(), cx);
        model.nav.update(cx, |nav, cx| {
            nav.set_active_channel(Some(channel.clone()), cx);
            nav.set_active_view(ActiveView::Server, cx);
        });
        Ok(channel)
    })??;
    wait_for(cx, Duration::from_secs(10), "the chat pane", |cx| shown(cx).is_some()).await?;
    let list = cx.update(|_, cx| shown(cx))?.context("no list")?;
    let l = list.clone();
    wait_for(cx, Duration::from_secs(20), "the newest page", move |cx| l.read(cx).row_count() > 0 && l.read(cx).rows_unmeasured() == 0).await?;
    Ok((channel, list))
}

/// The pictures the list wants on screen now, and how many are in.
fn visible_pictures(cx: &App) -> (usize, usize, usize) {
    let Some(pictures) = Pictures::global(cx) else { return (0, 0, 0) };
    let p = pictures.read(cx);
    let keys = p.visible_keys();
    let ready = keys.iter().filter(|k| matches!(p.look(k), Look::Ready(_))).count();
    let failed = keys.iter().filter(|k| matches!(p.look(k), Look::Failed(_))).count();
    (keys.len(), ready, failed)
}

/// (id, screen y) of the rows on screen.
fn tops(list: &Entity<decibell_chat::MessageList>, cx: &App) -> Vec<(i64, f32)> {
    let l = list.read(cx);
    l.visible_ids().into_iter().filter_map(|id| Some((id, l.row_screen_y(id)?))).collect()
}

/// Private memory of this process (`Private_Clean + Private_Dirty`), MB.
fn private_mb() -> f64 {
    let mut text = String::new();
    let _ = std::fs::File::open("/proc/self/smaps_rollup").and_then(|mut f| f.read_to_string(&mut text));
    let kb = |name: &str| text.lines().find(|l| l.starts_with(name)).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<f64>().ok()).unwrap_or(0.);
    (kb("Private_Clean:") + kb("Private_Dirty:")) / 1024.
}

fn scroll_by(list: &Entity<decibell_chat::MessageList>, delta: f64, cx: &mut App) {
    list.update(cx, |l, cx| l.scroll_by_precise(delta, cx));
}

async fn rest(cx: &TestCx, list: &Entity<decibell_chat::MessageList>) -> Result<()> {
    for _ in 0..200 {
        cx.settle().await?;
        let l = list.clone();
        if cx.update(|_, cx| l.read(cx).rows_unmeasured() == 0 && !l.read(cx).is_animating())? {
            return Ok(());
        }
    }
    bail!("the list never came to rest")
}

// ── The checks ──────────────────────────────────────────────────────────────────────────────

async fn check_images_live(cx: TestCx) -> Result<()> {
    let (_, list) = open_channel(&cx, "general").await?;
    // The first frame with the rows: placeholders where pictures will be.
    let l = list.clone();
    wait_for(&cx, Duration::from_secs(5), "the slots to see the rows", |cx| visible_pictures(cx).0 > 0 || l.read(cx).visible_ids().is_empty()).await?;
    let first = cx.update(|_, cx| (tops(&list, cx), visible_pictures(cx)))?;
    cx.log(format_args!("first look: {} rows on screen, {} pictures wanted on screen, {} already in", first.0.len(), first.1.0, first.1.1));
    let before = cx.frames()?;
    let started = Instant::now();
    let loaded = wait_for(&cx, Duration::from_secs(20), "every visible picture", |cx| {
        let (wanted, ready, failed) = visible_pictures(cx);
        wanted > 0 && ready + failed == wanted
    })
    .await?;
    let after_load = cx.frames()?.since(before);
    let (last, (wanted, ready, failed)) = cx.update(|_, cx| (tops(&list, cx), visible_pictures(cx)))?;
    cx.log(format_args!("{ready} of {wanted} pictures in after {} ms ({failed} failed), {} frames drawn meanwhile", loaded.as_millis(), after_load.drawn));
    ensure!(failed == 0, "{failed} pictures failed");
    // No row moved between the first look and the loaded state.
    for (id, y) in &first.0 {
        if let Some((_, y2)) = last.iter().find(|(i, _)| i == id) {
            ensure!((y - y2).abs() < 0.01, "row {id} moved from {y} to {y2} while pictures arrived");
        }
    }
    cx.log(format_args!("{} row tops unchanged", first.0.len()));
    let settled = cx.settle().await?;
    let quiet = cx.frames()?;
    cx.sleep(Duration::from_secs(2)).await;
    let idle = cx.frames()?.since(quiet);
    cx.log(format_args!("settled after {} more frames; then {} drawn in 2 s", settled.drawn, idle.drawn));
    ensure!(idle.drawn == 0, "{} frames at rest", idle.drawn);
    let stats = cx.update(|_, cx| Pictures::global(cx).map(|p| (p.read(cx).stats, p.read(cx).bytes())))?;
    if let Some((stats, (decoded, encoded))) = stats {
        cx.log(format_args!(
            "pipeline: {} fetched ({} KB), {} decoded (mean {} µs, max {} µs, off the UI thread), {} first paints with upload (mean {} µs, max {} µs, UI thread), decoded {} KB, encoded {} KB, {} ms total",
            stats.fetched,
            stats.fetched_bytes / 1024,
            stats.decoded,
            stats.decode_us / stats.decoded.max(1),
            stats.decode_max_us,
            stats.uploads,
            stats.upload_us / stats.uploads.max(1),
            stats.upload_max_us,
            decoded / 1024,
            encoded / 1024,
            started.elapsed().as_millis()
        ));
    }
    Ok(())
}

async fn check_scroll(cx: TestCx) -> Result<()> {
    let (_, list) = open_channel(&cx, "general").await?;
    rest(&cx, &list).await?;
    wait_for(&cx, Duration::from_secs(20), "the visible pictures", |cx| {
        let (w, r, f) = visible_pictures(cx);
        w == r + f
    })
    .await?;
    let summary = decibell_chat::bench::run_live(cx.app().clone(), 8., 3000., None).await.context("no chat pane")?;
    cx.log(format_args!(
        "3000 px/s for 8 s: {} frames, interval p50 {:.2} / p99 {:.2} ms, {} missed vsyncs, {} late, rows laid out {}",
        summary["frames"], summary["interval_ms"]["p50"].as_f64().unwrap_or(0.), summary["interval_ms"]["p99"].as_f64().unwrap_or(0.), summary["missed_vsyncs"], summary["late_frames"], summary["rows_laid_out"]
    ));
    let stats = cx.update(|_, cx| Pictures::global(cx).map(|p| (p.read(cx).stats, p.read(cx).bytes())))?;
    if let Some((stats, (decoded, encoded))) = stats {
        cx.log(format_args!("pictures fetched {}, decoded {} (mean {} µs), cancelled {}, evicted {}; decoded {} KB, encoded {} KB", stats.fetched, stats.decoded, stats.decode_us / stats.decoded.max(1), stats.cancelled, stats.evicted, decoded / 1024, encoded / 1024));
    }
    println!("selftest: scroll-summary {summary}");
    Ok(())
}

async fn check_cache_bound(cx: TestCx) -> Result<()> {
    let baseline = private_mb();
    cx.log(format_args!("signed in, nothing open: {baseline:.1} MB private"));
    let (_, list) = open_channel(&cx, "general").await?;
    let mut seen = std::collections::HashSet::new();
    let mut max_unpinned_ok = true;
    let mut max_decoded = 0usize;
    let started = Instant::now();
    while seen.len() < 500 && started.elapsed() < Duration::from_secs(180) {
        cx.update(|_, cx| scroll_by(&list, -1600., cx))?;
        cx.sleep(Duration::from_millis(120)).await;
        let (ids, decoded, over) = cx.update(|_, cx| {
            let l = list.read(cx);
            let ids: Vec<i64> = l.visible_ids().into_iter().filter(|id| l.row(*id).is_some_and(|(m, _)| !m.attachments.is_empty())).collect();
            let p = Pictures::global(cx).map(|p| (p.read(cx).bytes().0, p.read(cx).unpinned_bytes()));
            (ids, p.map_or(0, |p| p.0), p.is_some_and(|p| p.1 > crate::pictures::DECODED_BUDGET))
        })?;
        seen.extend(ids);
        max_decoded = max_decoded.max(decoded);
        if over {
            max_unpinned_ok = false;
        }
    }
    cx.log(format_args!("{} picture rows scrolled through in {:.0} s; decoded bytes peaked at {} KB", seen.len(), started.elapsed().as_secs_f64(), max_decoded / 1024));
    ensure!(max_unpinned_ok, "pictures no list wants went past the decoded budget");
    let scrolled = private_mb();
    // Leave: Home.
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    cx.settle().await?;
    cx.sleep(Duration::from_secs(1)).await;
    let (decoded, encoded) = cx.update(|_, cx| Pictures::global(cx).map_or((0, 0), |p| p.read(cx).bytes()))?;
    let left = private_mb();
    cx.log(format_args!(
        "private memory: {baseline:.1} MB before, {scrolled:.1} MB scrolled, {left:.1} MB after leaving (caches: decoded {} KB ≤ {} KB, encoded {} KB ≤ {} KB)",
        decoded / 1024,
        crate::pictures::DECODED_BUDGET / 1024,
        encoded / 1024,
        crate::pictures::ENCODED_BUDGET / 1024
    ));
    ensure!(decoded <= crate::pictures::DECODED_BUDGET && encoded <= crate::pictures::ENCODED_BUDGET + (8 << 20), "the caches kept more than their budgets");
    Ok(())
}

/// A small looping GIF (12 frames).
fn gif_bytes() -> Vec<u8> {
    use image::codecs::gif::{GifEncoder, Repeat};
    use image::{Delay, Frame, Rgba, RgbaImage};
    let mut out = Vec::new();
    {
        let mut enc = GifEncoder::new_with_speed(&mut out, 30);
        let _ = enc.set_repeat(Repeat::Infinite);
        for i in 0..12u32 {
            let frame = RgbaImage::from_fn(240, 135, |x, y| Rgba([((x + i * 20) % 255) as u8, (y * 2) as u8, 160, 255]));
            let _ = enc.encode_frame(Frame::from_parts(frame, 0, 0, Delay::from_numer_denom_ms(80, 1)));
        }
    }
    out
}

async fn check_gif(cx: TestCx) -> Result<()> {
    let (channel, list) = open_channel(&cx, "off-topic").await?;
    // Upload a GIF and send it (deleted again at the end).
    let nonce_before = cx.update(|_, cx| list.read(cx).visible_ids().last().copied().unwrap_or(0))?;
    cx.update(|_, cx| {
        let source = decibell_attachments::upload::UploadSource::Memory { name: "selftest-loop.gif".into(), mime: "image/gif".into(), data: bytes::Bytes::from(gif_bytes()) };
        actions::chat::attach_file(daily_gaming(), channel.clone(), source, cx);
    })?;
    let ch = channel.clone();
    wait_for(&cx, Duration::from_secs(15), "the GIF to be probed", move |cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), &ch).count() > 0).await?;
    cx.update(|_, cx| actions::chat::send_message(daily_gaming(), channel.clone(), "", 0, cx))?;
    let l = list.clone();
    let posted = wait_for(&cx, Duration::from_secs(30), "the GIF's echo", move |cx| {
        let l = l.read(cx);
        l.visible_ids().into_iter().any(|id| id > nonce_before && l.row(id).is_some_and(|(m, _)| m.attachments.iter().any(|a| &*a.filename == "selftest-loop.gif")))
    })
    .await?;
    let id = cx.update(|_, cx| {
        let l = list.read(cx);
        l.visible_ids().into_iter().find(|id| l.row(*id).is_some_and(|(m, _)| m.attachments.iter().any(|a| &*a.filename == "selftest-loop.gif")))
    })?.context("the GIF's message")?;
    cx.log(format_args!("posted message {id} after {} ms", posted.as_millis()));
    let result = gif_checks(&cx, &list).await;
    // Delete it whatever happened.
    cx.update(|_, cx| actions::chat::delete_message(daily_gaming(), channel.clone(), id, cx))?;
    let l = list.clone();
    wait_for(&cx, Duration::from_secs(15), "the GIF's message to go", move |cx| l.read(cx).row(id).is_none()).await?;
    cx.log(format_args!("deleted message {id}"));
    result
}

async fn gif_checks(cx: &TestCx, list: &Entity<decibell_chat::MessageList>) -> Result<()> {
    wait_for(cx, Duration::from_secs(20), "the GIF to play", |cx| Pictures::global(cx).is_some_and(|p| p.read(cx).playing() == 1)).await?;
    let f0 = cx.frames()?;
    let s0 = cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).stats.frames))?;
    cx.sleep(Duration::from_secs(2)).await;
    let (drawn, shown) = (cx.frames()?.since(f0).drawn, cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).stats.frames))? - s0);
    cx.log(format_args!("visible: {shown} GIF frames shown, {drawn} window frames drawn in 2 s"));
    ensure!(shown >= 15, "the GIF barely moved ({shown} frames in 2 s at 80 ms a frame)");
    // Scrolled away: it stops.
    cx.update(|_, cx| scroll_by(list, -4000., cx))?;
    rest(cx, list).await?;
    let playing = cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).playing()))?;
    let f1 = cx.frames()?;
    cx.sleep(Duration::from_secs(1)).await;
    let drawn = cx.frames()?.since(f1).drawn;
    cx.log(format_args!("scrolled away: {playing} playing, {drawn} frames in 1 s"));
    ensure!(playing == 0 && drawn == 0, "the GIF kept drawing off screen");
    // Back: it plays again.
    cx.update(|_, cx| list.update(cx, |l, cx| l.jump_to_present(cx)))?;
    wait_for(cx, Duration::from_secs(10), "the GIF to play again", |cx| Pictures::global(cx).is_some_and(|p| p.read(cx).playing() == 1)).await?;
    // Minimised (KDE): it stops; restored, it plays.
    let winctl = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/perf/winctl.sh");
    let pid = std::process::id().to_string();
    let minimised = Command::new(&winctl).args(["minimize", &pid]).status().is_ok_and(|s| s.success());
    if minimised {
        cx.sleep(Duration::from_millis(600)).await;
        let f2 = cx.frames()?;
        let s2 = cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).stats.frames))?;
        cx.sleep(Duration::from_secs(1)).await;
        let (drawn, shown) = (cx.frames()?.since(f2).drawn, cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).stats.frames))? - s2);
        cx.log(format_args!("minimised: {shown} GIF frames, {drawn} window frames in 1 s"));
        let _ = Command::new(&winctl).args(["restore", &pid]).status();
        ensure!(shown <= 1 && drawn <= 1, "the GIF kept going while minimised");
        wait_for(cx, Duration::from_secs(10), "the GIF to play after restoring", |cx| Pictures::global(cx).is_some_and(|p| p.read(cx).playing() == 1)).await?;
    } else {
        cx.log("winctl.sh couldn't minimise the window here: the minimise part skipped");
    }
    Ok(())
}

async fn check_viewer(cx: TestCx) -> Result<()> {
    let (_, list) = open_channel(&cx, "clips").await?;
    rest(&cx, &list).await?;
    // The gallery: the message with the most pictures on screen.
    let gallery = cx.update(|_, cx| {
        let l = list.read(cx);
        l.visible_ids().into_iter().filter_map(|id| l.row(id)).map(|(m, _)| m).filter(|m| m.attachments.iter().filter(|a| a.kind == decibell_attachments::AttachmentKind::Image).count() >= 2).last()
    })?.context("no gallery on screen in #clips")?;
    let first = gallery.attachments.iter().filter(|a| a.kind == decibell_attachments::AttachmentKind::Image).min_by_key(|a| a.position).map(|a| a.id).context("no picture")?;
    wait_for(&cx, Duration::from_secs(10), "the gallery's tile", move |_| crate::probes::tile(first).is_some()).await?;
    let tile = crate::probes::tile(first).context("tile")?;
    cx.click(tile.center())?;
    wait_for(&cx, Duration::from_secs(5), "the viewer to open", |cx| model(cx).chat.viewer.read(cx).is_open()).await?;
    cx.settle().await?;
    let viewer_count = cx.update(|_, cx| model(cx).chat.viewer.read(cx).items.len())?;
    cx.log(format_args!("opened on a gallery of {viewer_count}"));
    ensure!(viewer_count >= 2, "the gallery holds the message's pictures");
    // Zoom to actual size, pan, step, copy, Esc.
    cx.keystroke("1")?;
    cx.settle().await?;
    let scale = cx.update(|_, cx| crate::viewer::current_scale(cx))?;
    cx.log(format_args!("zoom: {scale:?}"));
    ensure!(scale.is_some_and(|s| (s - 1.).abs() < 0.01), "1 zooms to actual size");
    let centre = cx.update(|window, _| {
        let s = window.viewport_size();
        point(s.width / 2., s.height / 2.)
    })?;
    let pan_before = cx.update(|_, cx| crate::viewer::current_pan(cx))?;
    cx.mouse_down(centre, 1, Modifiers::default())?;
    for step in 1..=6 {
        cx.mouse_drag(point(centre.x + px(30. * step as f32), centre.y))?;
    }
    cx.mouse_up(point(centre.x + px(180.), centre.y))?;
    cx.settle().await?;
    let pan_after = cx.update(|_, cx| crate::viewer::current_pan(cx))?;
    cx.log(format_args!("pan: {pan_before:?} → {pan_after:?}"));
    ensure!(pan_after != pan_before, "dragging pans a zoomed picture");
    cx.keystroke("right")?;
    cx.settle().await?;
    ensure!(cx.update(|_, cx| model(cx).chat.viewer.read(cx).index)? == 1, "→ steps to the next picture");
    cx.keystroke("left")?;
    cx.settle().await?;
    ensure!(cx.update(|_, cx| model(cx).chat.viewer.read(cx).index)? == 0, "← steps back");
    // Copy into the test's sink (the user's clipboard stays as it is).
    cx.update(|_, cx| cx.set_global(crate::actions::ClipboardSink { captured: Default::default(), capture: true }))?;
    cx.keystroke("ctrl-c")?;
    wait_for(&cx, Duration::from_secs(15), "the copy", |cx| cx.try_global::<crate::actions::ClipboardSink>().is_some_and(|s| !s.captured.borrow().is_empty())).await?;
    let format = cx.update(|_, cx| {
        cx.try_global::<crate::actions::ClipboardSink>().and_then(|s| s.captured.borrow().first().and_then(|item| item.entries().first().map(|e| format!("{e:?}").chars().take(40).collect::<String>())))
    })?;
    cx.log(format_args!("copied: {}", format.unwrap_or_default()));
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "the viewer to close", |cx| !model(cx).chat.viewer.read(cx).is_open()).await?;
    cx.settle().await?;
    cx.log("closed with Esc");
    Ok(())
}

/// A message routed into the client only (never sent): what a live message looks like.
fn inject(channel: &ChannelId, id: i64, content: &str, cx: &mut App) {
    use decibell_core::events::{CoreEvent, MessageReceivedPayload};
    let event = CoreEvent::MessageReceived(MessageReceivedPayload {
        context: channel.to_string(),
        server_id: daily_gaming().wire(),
        sender: "carol".into(),
        sender_uid: 3,
        recipient: String::new(),
        recipient_uid: 0,
        content: content.into(),
        timestamp: std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs()).to_string(),
        id,
        attachments: Vec::new(),
        nonce: String::new(),
        edited_at: 0,
        reply_to: 0,
        reply_to_sender: String::new(),
        reply_to_sender_uid: 0,
        reply_to_content: String::new(),
        reply_to_attachment_kinds: Vec::new(),
        encrypted: false,
        decrypt_error: String::new(),
        encrypted_attachments: Vec::new(),
        suppress_embeds: false,
    });
    decibell_model::events::route(&model(cx), event, cx);
}

/// A one-page HTTP server on 127.0.0.1 (a thread that serves until the test ends).
fn page_server() -> Result<(u16, std::sync::Arc<std::sync::atomic::AtomicBool>)> {
    let listener = std::net::TcpListener::bind("127.0.0.1:0")?;
    let port = listener.local_addr()?.port();
    let stop = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
    let stop_thread = stop.clone();
    listener.set_nonblocking(false)?;
    std::thread::Builder::new().name("decibell-test-page".into()).spawn(move || {
        for stream in listener.incoming() {
            if stop_thread.load(std::sync::atomic::Ordering::Relaxed) {
                break;
            }
            let Ok(mut stream) = stream else { continue };
            let mut request = [0u8; 2048];
            let _ = stream.read(&mut request);
            let body = "<!doctype html><html><head><title>Fallback</title>\
                <meta property=\"og:site_name\" content=\"Loopback Fixture\">\
                <meta property=\"og:title\" content=\"A page served by the self-test\">\
                <meta property=\"og:description\" content=\"Nothing here leaves this machine.\">\
                <meta name=\"theme-color\" content=\"#3ba55d\"></head><body>hi</body></html>";
            let _ = write!(stream, "HTTP/1.1 200 OK\r\nContent-Type: text/html; charset=utf-8\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len());
        }
    })?;
    Ok((port, stop))
}

async fn check_embed(cx: TestCx) -> Result<()> {
    let (channel, list) = open_channel(&cx, "off-topic").await?;
    let (port, stop) = page_server()?;
    // The unfurler that may reach 127.0.0.1 (this build's `loopback` feature), on the core's runtime.
    cx.update(|_, cx| {
        #[cfg(feature = "loopback")]
        {
            let previews = std::sync::Arc::new(decibell_unfurl::LinkPreviews::loopback("selftest"));
            let unfurler: decibell_model::chat::link_previews::Unfurler = std::rc::Rc::new(move |url: gpui::SharedString, cx: &mut App| {
                let previews = previews.clone();
                let (tx, rx) = futures::channel::oneshot::channel();
                decibell_model::core::spawn_detached(async move {
                    let _ = tx.send(previews.fetch(&url).await);
                });
                cx.spawn(async move |_| rx.await.ok().flatten())
            });
            model(cx).chat.link_previews.update(cx, |m, _| m.set_unfurler(unfurler));
        }
        let _ = cx;
    })?;
    #[cfg(not(feature = "loopback"))]
    {
        cx.log("SKIP: this build has no loopback unfurler (build with --features dev-local)");
        return Ok(());
    }
    let id = 9_000_000_001;
    let url = format!("http://127.0.0.1:{port}/page");
    let content = format!("Look: {url}");
    let ch = channel.clone();
    cx.update(|_, cx| inject(&ch, id, &content, cx))?;
    let l = list.clone();
    wait_for(&cx, Duration::from_secs(5), "the message", move |cx| l.read(cx).row_height(id).is_some()).await?;
    let bare = cx.update(|_, cx| list.read(cx).row_height(id))?.unwrap_or(0.);
    let u = url.clone();
    wait_for(&cx, Duration::from_secs(15), "the unfurl", move |cx| {
        matches!(model(cx).chat.link_previews.read(cx).get(&u), Some(decibell_model::chat::link_previews::PreviewEntry::Done(Some(_))))
    })
    .await?;
    let l = list.clone();
    wait_for(&cx, Duration::from_secs(5), "the card", move |cx| l.read(cx).row_height(id).is_some_and(|h| h > bare + 40.)).await?;
    let with_card = cx.update(|_, cx| list.read(cx).row_height(id))?.unwrap_or(0.);
    cx.log(format_args!("row {bare} px without the card, {with_card} px with it"));
    // The Privacy toggle off: the card goes.
    let was = cx.update(|_, cx| model(cx).settings.read(cx).get().link_previews_enabled)?;
    cx.update(|_, cx| model(cx).settings.update(cx, |s, cx| s.change(|v| v.link_previews_enabled = false, cx)))?;
    let l = list.clone();
    let result = wait_for(&cx, Duration::from_secs(5), "the card to go", move |cx| l.read(cx).row_height(id).is_some_and(|h| (h - bare).abs() < 0.5)).await;
    cx.update(|_, cx| model(cx).settings.update(cx, |s, cx| s.change(|v| v.link_previews_enabled = was, cx)))?;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = std::net::TcpStream::connect(("127.0.0.1", port));
    result?;
    cx.log("toggle off: the card went; the toggle is back as it was");
    Ok(())
}

async fn check_invite(cx: TestCx) -> Result<()> {
    let code = std::env::var("DECIBELL_TEST_INVITE").ok().filter(|c| !c.is_empty()).context("no Design Lab invite in accounts.json")?;
    let (channel, list) = open_channel(&cx, "off-topic").await?;
    let id = 9_000_000_002;
    let content = format!("decibell://invite/{code}");
    let ch = channel.clone();
    cx.update(|_, cx| inject(&ch, id, &content, cx))?;
    let c = code.clone();
    wait_for(&cx, Duration::from_secs(15), "central's answer", move |cx| model(cx).invites.read(cx).get(&c).is_some_and(|e| !e.loading)).await?;
    let entry = cx.update(|_, cx| model(cx).invites.read(cx).get(&code).cloned())?.context("entry")?;
    let name = entry.resolved.as_ref().map(|r| r.server_name.to_string()).unwrap_or_default();
    cx.log(format_args!("resolved: {name:?} ({} members), invalid {}", entry.resolved.as_ref().map_or(0, |r| r.member_count), entry.invalid));
    ensure!(name == "Design Lab", "the invite resolves to Design Lab");
    let l = list.clone();
    wait_for(&cx, Duration::from_secs(5), "the card", move |cx| l.read(cx).row_height(id).is_some_and(|h| h > 200.)).await?;
    let h = cx.update(|_, cx| list.read(cx).row_height(id))?.unwrap_or(0.);
    cx.log(format_args!("the row with its card: {h} px (not joined)"));
    let joined = cx.update(|_, cx| model(cx).connections.read(cx).is_connected(ServerId::from_central(3)))?;
    ensure!(!joined, "nothing joined Design Lab");
    Ok(())
}

#[allow(dead_code)]
fn frames_since(f: Frames) -> u64 {
    f.drawn
}
