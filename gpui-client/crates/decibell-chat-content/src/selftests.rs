//! The content self-tests, against the local test stack (`tools/dev/README.md`) as `perf2`.
//!
//! Each `content-*` self-test starts the app again in a child — `decibell --check content-… --login
//! perf2` with its own temporary profile, the stack's `DECIBELL_CENTRAL` and `DECIBELL_TEST_PROBES=1`
//! — and passes when the check inside it does (the shell's pattern). They need a `dev-local` binary,
//! refuse a non-loopback `DECIBELL_CENTRAL` and skip, saying so, when the stack is down. Nothing they
//! do stays on the stack: the GIF upload is deleted again, the link and invite messages are routed
//! into the client only, nothing is joined.

use std::io::{BufRead as _, Read as _};
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::rc::Rc;
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
        SelfTest {
            name: "content-scroll-fixture",
            about: "240 picture rows (memory fetcher, 20 ms answers, real decodes off the UI thread) scrolled at 3000 px/s: frame intervals and missed frames",
            run: |cx| Box::pin(fixture_parent(cx, "content-scroll-fixture")),
        },
        SelfTest {
            name: "content-rows",
            about: "every row's top and height on the content fixture channels (written to $DECIBELL_CHAT_ROWS_DIR for the Electron comparison)",
            run: |cx| Box::pin(fixture_parent(cx, "content-rows")),
        },
        SelfTest {
            name: "media-idle",
            about: "video and audio cards on screen, nothing playing: 0 frames in 5 s, no player, decoder or sink thread",
            run: |cx| Box::pin(fixture_parent(cx, "media-idle")),
        },
        SelfTest {
            name: "media-video-play",
            about: "a click on a video card (fixture, null audio sink): first picture, pictures at the file's rate with no list frames, pause = nothing runs, resume, seek, the end; which decoder; memory before / during / after",
            run: |cx| Box::pin(fixture_parent(cx, "media-video-play")),
        },
        SelfTest {
            name: "media-audio-play",
            about: "an audio attachment (fixture, null sink): plays, the mirrored time advances, pause, seek, the end; the device stream closes when silent",
            run: |cx| Box::pin(fixture_parent(cx, "media-audio-play")),
        },
        SelfTest {
            name: "media-persist",
            about: "a playing video's row scrolled away: the file plays on, no picture is presented; scrolled back: pictures again; another channel: stopped (the renderer's clearChannel); a second video replaces the first, audio plays beside video",
            run: |cx| Box::pin(fixture_parent(cx, "media-persist")),
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
        SelfTest { name: "content-rows", about: "(in the app)", run: |cx| Box::pin(check_rows(cx)) },
        SelfTest { name: "content-scroll-fixture", about: "(in the app)", run: |cx| Box::pin(check_scroll_fixture(cx)) },
        SelfTest { name: "media-idle", about: "(in the app)", run: |cx| Box::pin(check_media_idle(cx)) },
        SelfTest { name: "media-video-play", about: "(in the app)", run: |cx| Box::pin(check_media_video(cx)) },
        SelfTest { name: "media-audio-play", about: "(in the app)", run: |cx| Box::pin(check_media_audio(cx)) },
        SelfTest { name: "media-persist", about: "(in the app)", run: |cx| Box::pin(check_media_persist(cx)) },
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

/// A private session bus with nothing on it, for as long as it is held (as `chat-live`'s).
struct EmptyBus {
    pid: u32,
    address: String,
}

impl EmptyBus {
    fn start(dir: &Path) -> Result<EmptyBus> {
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
        let _ = Command::new("kill").arg(self.pid.to_string()).status();
    }
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
    std::fs::create_dir_all(profile.0.join("profile"))?;
    std::fs::create_dir_all(profile.0.join("bus"))?;
    // The child's at-rest key must not reach the desktop's keyring (a throwaway profile would
    // leave an item there): it gets a session bus of its own with nothing on it.
    let bus = EmptyBus::start(&profile.0.join("bus"))?;
    let design_invite = stack.accounts["communities"].as_array().and_then(|c| c.iter().find(|c| c["name"] == "Design Lab")).and_then(|c| c["invite"].as_str()).unwrap_or_default().to_string();
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--check", check, "--login", ACCOUNT])
        .env("DECIBELL_USER_DATA_DIR", profile.0.join("profile"))
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("DECIBELL_TEST_DESKTOP_BUS", std::env::var("DBUS_SESSION_BUS_ADDRESS").unwrap_or_default())
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

/// The fixture checks need no stack: a child only so the fixture they install (a memory fetcher,
/// the shell's fixture state) stays out of this process.
async fn fixture_parent(cx: TestCx, check: &'static str) -> Result<()> {
    // An empty profile (no saved sign-in: the app stays signed out), its own empty session bus,
    // and a central nothing listens on.
    let profile = Profile(std::env::temp_dir().join(format!("decibell-{check}-{}", std::process::id())));
    let _ = std::fs::remove_dir_all(&profile.0);
    std::fs::create_dir_all(profile.0.join("profile"))?;
    std::fs::create_dir_all(profile.0.join("bus"))?;
    let bus = EmptyBus::start(&profile.0.join("bus"))?;
    let mut command = Command::new(std::env::current_exe()?);
    command
        .args(["--check", check])
        .env("DECIBELL_USER_DATA_DIR", profile.0.join("profile"))
        .env("DBUS_SESSION_BUS_ADDRESS", &bus.address)
        .env("DECIBELL_CENTRAL", "127.0.0.1:9")
        // Nothing a self-test plays is audible: the players render into a sink that discards.
        .env("DECIBELL_MEDIA_OUTPUT", "null")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    let output = cx.app().background_executor().spawn(async move { command.output() }).await?;
    drop(bus);
    for line in String::from_utf8_lossy(&output.stdout).lines().filter(|l| !l.starts_with("selftest:")) {
        cx.log(format_args!("│ {line}"));
    }
    if !output.status.success() {
        for line in String::from_utf8_lossy(&output.stderr).lines().rev().take(16).collect::<Vec<_>>().into_iter().rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{check} failed in the app ({})", output.status);
    }
    Ok(())
}

/// A window on a fixture channel of the content crate (its own, closed by the caller).
fn fixture_window(cx: &TestCx, channel: &'static str) -> Result<(TestCx, Entity<crate::screens::ContentPane>)> {
    use gpui::{AppContext as _, Bounds, TitlebarOptions, WindowBounds, WindowDecorations, WindowOptions, size};
    let (handle, pane) = cx.app().update(|cx| -> Result<(gpui::AnyWindowHandle, Entity<crate::screens::ContentPane>)> {
        let bounds = Bounds::centered(None, decibell_ui::outer_window_size(size(px(1280.), px(800.))), cx);
        let options = WindowOptions {
            window_bounds: Some(WindowBounds::Windowed(bounds)),
            titlebar: Some(TitlebarOptions { title: Some("Decibell content test".into()), appears_transparent: true, traffic_light_position: None }),
            window_decorations: Some(WindowDecorations::Client),
            app_id: Some("decibell".into()),
            ..Default::default()
        };
        let mut pane = None;
        let window = cx.open_window(options, |window, cx| {
            let view = cx.new(|cx| crate::screens::ContentPane::new(channel, window, cx));
            pane = Some(view.clone());
            cx.new(|_| Framed(view.into()))
        })?;
        Ok((window.into(), pane.context("no pane")?))
    })?;
    Ok((TestCx::new(cx.app().clone(), handle), pane))
}

/// The pane in the app's window frame and title bar (as the screens and the app draw it).
struct Framed(gpui::AnyView);

impl gpui::Render for Framed {
    fn render(&mut self, window: &mut gpui::Window, cx: &mut gpui::Context<Self>) -> impl gpui::IntoElement {
        use decibell_ui::ActiveTheme as _;
        use gpui::{ParentElement as _, Styled as _};
        let theme = cx.theme();
        let content = gpui::div()
            .size_full()
            .flex()
            .flex_col()
            .font_family(theme.font(decibell_ui::FontRole::Sans))
            .text_size(px(theme.scaled.text_body))
            .text_color(theme.color_text_primary)
            .child(decibell_ui::TitleBar::new("Decibell content test"))
            .child(gpui::div().flex_1().min_h_0().child(self.0.clone().cached(gpui::StyleRefinement::default().size_full())));
        decibell_ui::window_frame(content, window, cx)
    }
}

async fn check_scroll_fixture(cx: TestCx) -> Result<()> {
    let (w, pane) = fixture_window(&cx, crate::fixture::GALLERY)?;
    let result = async {
        let list = w.update(|_, cx| pane.read(cx).list.clone())?;
        // Every file made before the run (the memory fetcher encodes on the UI thread), then
        // answers after 20 ms and decodes off the UI thread, as on the network.
        w.update(|_, cx| {
            let fetcher = crate::fixture::fetcher(cx);
            let ids: Vec<i64> = fetcher.attachments.borrow().keys().copied().collect();
            for id in ids {
                for thumb in [0u32, 320, 640, 1280] {
                    let _ = fetcher.answer(&crate::pictures::Source::Attachment { server: ServerId::from_central(decibell_shell::fixture::DAILY_GAMING).wire().into(), id, thumb });
                }
            }
            fetcher.latency.set(Duration::from_millis(20));
            if let Some(p) = Pictures::global(cx) {
                p.update(cx, |p, _| p.set_sync(false));
            }
        })?;
        rest(&w, &list).await?;
        wait_for(&w, Duration::from_secs(10), "the visible pictures", |cx| {
            let (wanted, ready, failed) = visible_pictures(cx);
            wanted > 0 && wanted == ready + failed
        })
        .await?;
        let rows = w.update(|_, cx| list.read(cx).row_count())?;
        let (screen_bytes, screen_count) = w.update(|_, cx| Pictures::global(cx).map(|p| p.read(cx).visible_bytes()))?.unwrap_or_default();
        let scale = w.update(|window, _| window.scale_factor())?;
        cx.log(format_args!("the newest screen: {screen_count} pictures in, {} KB decoded (scale {scale})", screen_bytes / 1024));
        let before = w.update(|_, cx| Pictures::global(cx).map(|p| p.read(cx).stats))?.unwrap_or_default();
        w.update(|_, cx| list.update(cx, |l, cx| l.begin_bench(3000., cx)))?;
        w.sleep(Duration::from_secs(12)).await;
        let recording = w.update(|_, cx| list.update(cx, |l, _| l.end_bench()))?.context("no recording")?;
        let summary = decibell_chat::bench::summarize(&recording, 3000., 12., None);
        let after = w.update(|_, cx| Pictures::global(cx).map(|p| (p.read(cx).stats, p.read(cx).bytes())))?;
        cx.log(format_args!(
            "{rows} picture rows, 3000 px/s for 12 s: {} frames, interval p50 {:.2} / p99 {:.2} ms, {} missed vsyncs, {} late, rows laid out {}",
            summary["frames"],
            summary["interval_ms"]["p50"].as_f64().unwrap_or(0.),
            summary["interval_ms"]["p99"].as_f64().unwrap_or(0.),
            summary["missed_vsyncs"],
            summary["late_frames"],
            summary["rows_laid_out"]
        ));
        if let Some((stats, (decoded, encoded))) = after {
            cx.log(format_args!(
                "pictures during the run: {} decoded (mean {} µs off the UI thread), {} first paints (mean {} µs on it), {} cancelled, {} evicted; decoded {} KB, encoded {} KB at the end",
                stats.decoded - before.decoded,
                (stats.decode_us - before.decode_us) / (stats.decoded - before.decoded).max(1),
                stats.uploads - before.uploads,
                (stats.upload_us - before.upload_us) / (stats.uploads - before.uploads).max(1),
                stats.cancelled - before.cancelled,
                stats.evicted - before.evicted,
                decoded / 1024,
                encoded / 1024
            ));
        }
        println!("selftest: scroll-summary {summary}");
        anyhow::Ok(())
    }
    .await;
    let _ = w.update(|window, _| window.remove_window());
    result
}

/// Dumps (id, top, height) of every row of the two content fixture channels as a 1280 px window
/// lays them out, for `tools/electron-captures/chat-rows.mjs … content-attachments-tall
/// content-embeds-tall`.
async fn check_rows(cx: TestCx) -> Result<()> {
    for (name, channel) in [("content-attachments-tall", crate::fixture::ATTACHMENTS), ("content-embeds-tall", crate::fixture::EMBEDS)] {
        let (w, pane) = fixture_window(&cx, channel)?;
        let dump = async {
            let list = w.update(|_, cx| pane.read(cx).list.clone())?;
            rest(&w, &list).await?;
            let (width, height) = w.update(|_, cx| (f32::from(list.read(cx).bounds().size.width), f32::from(list.read(cx).bounds().size.height)))?;
            let rows = w.update(|_, cx| list.update(cx, |l, cx| l.row_tops(cx)))?;
            anyhow::Ok((width, height, rows))
        }
        .await;
        let _ = w.update(|window, _| window.remove_window());
        let (width, height, rows) = dump?;
        let total: f64 = rows.iter().map(|row| row.2 as f64).sum();
        cx.log(format_args!("{name}: {} rows, {total} px, list {width} × {height}", rows.len()));
        ensure!(!rows.is_empty(), "{name} has rows");
        if let Some(dir) = std::env::var_os("DECIBELL_CHAT_ROWS_DIR") {
            let json = serde_json::json!({
                "list": { "width": width, "height": height },
                "rows": rows.iter().map(|(id, top, height)| serde_json::json!({ "id": id, "top": top, "height": height })).collect::<Vec<_>>(),
            });
            std::fs::write(Path::new(&dir).join(format!("{name}-rows.json")), serde_json::to_vec_pretty(&json)?)?;
        }
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

/// How often each model store notified over `window` (a diagnosis for a window that keeps
/// drawing).
async fn store_notifications(cx: &TestCx, window: Duration) -> Result<(String, usize)> {
    use std::cell::RefCell;
    use std::rc::Rc;
    let counts: Rc<RefCell<std::collections::BTreeMap<&'static str, usize>>> = Rc::default();
    let frames = cx.frames()?;
    let subscriptions = cx.update(|_, cx| {
        let m = model(cx);
        let mut subs = Vec::new();
        macro_rules! watch {
            ($($name:literal => $entity:expr),* $(,)?) => {
                $(
                    let c = counts.clone();
                    subs.push(cx.observe(&$entity, move |_, _| *c.borrow_mut().entry($name).or_default() += 1));
                )*
            };
        }
        watch!(
            "auth" => m.auth, "users" => m.users, "nav" => m.nav, "overlays" => m.overlays, "settings" => m.settings,
            "layout" => m.layout, "servers" => m.servers, "connections" => m.connections, "communities" => m.communities,
            "toasts" => m.toasts, "invites" => m.invites, "update" => m.update, "answers" => m.answers,
            "chat.channels" => m.chat.channels, "chat.history" => m.chat.history, "chat.pacing" => m.chat.pacing,
            "chat.drafts" => m.chat.drafts, "chat.composer" => m.chat.composer, "chat.link_previews" => m.chat.link_previews,
            "chat.audio" => m.chat.audio, "chat.video" => m.chat.video, "chat.viewer" => m.chat.viewer,
            "live.transfers" => m.live.transfers, "live.presence.statuses" => m.live.presence.statuses,
            "live.presence.own" => m.live.presence.own, "live.presence.interest" => m.live.presence.interest,
            "live.presence.auto_away" => m.live.presence.auto_away, "live.presence.avatars" => m.live.presence.avatars,
            "live.voice.session" => m.live.voice.session, "live.voice.speaking" => m.live.voice.speaking,
            "live.voice.input_level" => m.live.voice.input_level, "live.cues" => m.live.cues, "live.notices" => m.live.notices,
        );
        subs
    })?;
    cx.sleep(window).await;
    drop(subscriptions);
    let drawn = cx.frames()?.since(frames).drawn;
    let mut text = format!("{drawn} frames;");
    for (name, n) in counts.borrow().iter() {
        text.push_str(&format!(" {name} {n}"));
    }
    if let Some(p) = cx.update(|_, cx| Pictures::global(cx).map(|p| p.read(cx).stats))? {
        text.push_str(&format!("; pictures fetched {} decoded {} frames {}", p.fetched, p.decoded, p.frames));
    }
    let total = counts.borrow().values().sum();
    Ok((text, total))
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
    let (screen_bytes, _) = cx.update(|_, cx| Pictures::global(cx).map(|p| p.read(cx).visible_bytes()))?.unwrap_or_default();
    cx.log(format_args!("the screen's pictures hold {} KB decoded", screen_bytes / 1024));
    ensure!(failed == 0, "{failed} pictures failed");
    // No row moved between the first look and the loaded state.
    for (id, y) in &first.0 {
        if let Some((_, y2)) = last.iter().find(|(i, _)| i == id) {
            ensure!((y - y2).abs() < 0.01, "row {id} moved from {y} to {y2} while pictures arrived");
        }
    }
    cx.log(format_args!("{} row tops unchanged", first.0.len()));
    let settled = cx.settle().await;
    if settled.is_err() {
        // Still drawing: which stores notify meanwhile.
        let (counts, _) = store_notifications(&cx, Duration::from_secs(3)).await?;
        cx.log(format_args!("still drawing; store notifications in 3 s: {counts}"));
    }
    let settled = settled?;
    let quiet = cx.frames()?;
    let (counts, outside) = store_notifications(&cx, Duration::from_secs(2)).await?;
    let idle = cx.frames()?.since(quiet);
    cx.log(format_args!("settled after {} more frames; then {} drawn in 2 s (store notifications: {counts})", settled.drawn, idle.drawn));
    // At rest nothing of ours draws; what the stack sends meanwhile (other test clients' presence)
    // may redraw a dot or a name, a few frames each.
    ensure!(idle.drawn as usize <= 4 * outside, "{} frames at rest with {outside} store notifications", idle.drawn);
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
    let (decoded, encoded, unpinned, (ready, count)) = cx.update(|_, cx| {
        Pictures::global(cx).map_or((0, 0, 0, (0, 0)), |p| {
            let p = p.read(cx);
            (p.bytes().0, p.bytes().1, p.unpinned_bytes(), p.ready_bytes())
        })
    })?;
    let left = private_mb();
    cx.log(format_args!(
        "private memory: {baseline:.1} MB before, {scrolled:.1} MB scrolled, {left:.1} MB after leaving; caches after leaving: {count} pictures decoded, {} KB ({} KB no list wants, budget {} KB; running total {} KB), encoded {} KB (budget {} KB)",
        ready / 1024,
        unpinned / 1024,
        crate::pictures::DECODED_BUDGET / 1024,
        decoded / 1024,
        encoded / 1024,
        crate::pictures::ENCODED_BUDGET / 1024
    ));
    ensure!(unpinned <= crate::pictures::DECODED_BUDGET, "pictures no list wants went past the decoded budget after leaving");
    ensure!(decoded.abs_diff(ready) < 4096, "the running total of decoded bytes drifted from the entries");
    ensure!(encoded <= crate::pictures::ENCODED_BUDGET + (8 << 20), "the encoded cache kept more than its budget");
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
    // Minimised: it stops; brought back (KWin, through the desktop's bus), it plays.
    let pid = std::process::id().to_string();
    let winctl = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../../tools/perf/winctl.sh");
    let desktop_bus = std::env::var("DECIBELL_TEST_DESKTOP_BUS").unwrap_or_default();
    cx.update(|window, _| window.minimize_window())?;
    cx.sleep(Duration::from_millis(800)).await;
    let f2 = cx.frames()?;
    let s2 = cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).stats.frames))?;
    cx.sleep(Duration::from_secs(1)).await;
    let (drawn, shown) = (cx.frames()?.since(f2).drawn, cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).stats.frames))? - s2);
    let playing = cx.update(|_, cx| Pictures::global(cx).map_or(0, |p| p.read(cx).playing()))?;
    cx.log(format_args!("minimised: {shown} GIF frames, {drawn} window frames, {playing} timers in 1 s"));
    let restored = !desktop_bus.is_empty()
        && Command::new(&winctl).args(["activate", &pid]).env("DBUS_SESSION_BUS_ADDRESS", &desktop_bus).status().is_ok_and(|s| s.success());
    ensure!(shown <= 1 && drawn <= 1, "the GIF kept going while minimised");
    if restored {
        wait_for(cx, Duration::from_secs(10), "the GIF to play after restoring", |cx| Pictures::global(cx).is_some_and(|p| p.read(cx).playing() == 1)).await?;
        cx.log("restored: playing again");
    } else {
        cx.log("winctl.sh couldn't bring the window back here: the restore part skipped");
    }
    Ok(())
}

async fn check_viewer(cx: TestCx) -> Result<()> {
    let (_, list) = open_channel(&cx, "clips").await?;
    rest(&cx, &list).await?;
    // The gallery: the newest loaded message with two pictures or more, brought on screen.
    let gallery = cx.update(|_, cx| {
        list.update(cx, |l, cx| {
            let ids: Vec<i64> = l.row_tops(cx).into_iter().map(|(id, _, _)| id).collect();
            ids.into_iter().filter_map(|id| l.row(id)).map(|(m, _)| m).rfind(|m| m.attachments.iter().filter(|a| a.kind == decibell_attachments::AttachmentKind::Image).count() >= 2)
        })
    })?.context("no gallery loaded in #clips")?;
    let gid = gallery.id;
    if !cx.update(|_, cx| list.read(cx).visible_ids().contains(&gid))? {
        cx.update(|_, cx| list.update(cx, |l, cx| l.jump_to_message(gid, cx)))?;
        rest(&cx, &list).await?;
    }
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

#[cfg(feature = "loopback")]
/// A one-page HTTP server on 127.0.0.1 (a thread that serves until the test ends).
fn page_server() -> Result<(u16, std::sync::Arc<std::sync::atomic::AtomicBool>)> {
    use std::io::Write as _;
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
    #[cfg(not(feature = "loopback"))]
    {
        cx.log("SKIP: this build has no loopback unfurler (build with --features dev-local)");
        Ok(())
    }
    #[cfg(feature = "loopback")]
    embed_with_loopback(cx).await
}

#[cfg(feature = "loopback")]
async fn embed_with_loopback(cx: TestCx) -> Result<()> {
    let (channel, list) = open_channel(&cx, "off-topic").await?;
    let (port, stop) = page_server()?;
    // The unfurler that may reach 127.0.0.1 (this build's `loopback` feature), on the core's runtime.
    cx.update(|_, cx| {
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
    })?;
    let id = 9_000_000_001;
    let url = format!("http://127.0.0.1:{port}/page");
    let content = format!("Look: {url}");
    let ch = channel.clone();
    cx.update(|_, cx| inject(&ch, id, &content, cx))?;
    let l = list.clone();
    wait_for(&cx, Duration::from_secs(5), "the message", move |cx| l.read(cx).row_height(id).is_some()).await?;
    let u = url.clone();
    let unfurled = wait_for(&cx, Duration::from_secs(15), "the unfurl", move |cx| {
        matches!(model(cx).chat.link_previews.read(cx).get(&u), Some(decibell_model::chat::link_previews::PreviewEntry::Done(Some(_))))
    })
    .await?;
    rest(&cx, &list).await?;
    let with_card = cx.update(|_, cx| list.read(cx).row_height(id))?.unwrap_or(0.);
    // The Privacy toggle off: the card goes (the row is then the message alone).
    let was = cx.update(|_, cx| model(cx).settings.read(cx).get().link_previews_enabled)?;
    cx.update(|_, cx| model(cx).settings.update(cx, |s, cx| s.change(|v| v.link_previews_enabled = false, cx)))?;
    let l = list.clone();
    let gone = wait_for(&cx, Duration::from_secs(5), "the card to go", move |cx| l.read(cx).row_height(id).is_some_and(|h| h < with_card - 40.)).await;
    let bare = cx.update(|_, cx| list.read(cx).row_height(id))?.unwrap_or(0.);
    cx.update(|_, cx| model(cx).settings.update(cx, |s, cx| s.change(|v| v.link_previews_enabled = was, cx)))?;
    let l = list.clone();
    let back = wait_for(&cx, Duration::from_secs(5), "the card to come back", move |cx| l.read(cx).row_height(id).is_some_and(|h| (h - with_card).abs() < 0.5)).await;
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    let _ = std::net::TcpStream::connect(("127.0.0.1", port));
    cx.log(format_args!("unfurled after {} ms; the row is {with_card} px with the card, {bare} px without", unfurled.as_millis()));
    gone?;
    back?;
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

// ── Media playback (fixture channel, memory sources, the null audio sink) ────────────────────

/// The clip the fixture's video attachments play: `DECIBELL_MEDIA_CLIP` (a file, for
/// measurements with a real 1080p clip), else the media crate's 60 s test file.
fn media_clip() -> std::sync::Arc<Vec<u8>> {
    let bytes = std::env::var_os("DECIBELL_MEDIA_CLIP")
        .and_then(|path| std::fs::read(path).ok())
        .unwrap_or_else(|| include_bytes!("../../decibell-media/tests/assets/sync60.mp4").to_vec());
    std::sync::Arc::new(bytes)
}

fn media_setup(w: &TestCx) -> Result<Entity<crate::playback::Playback>> {
    ensure!(
        std::env::var("DECIBELL_MEDIA_OUTPUT").is_ok_and(|v| v == "null"),
        "media self-tests run with DECIBELL_MEDIA_OUTPUT=null (nothing audible)"
    );
    w.update(|_, cx| {
        let playback = crate::playback::Playback::global(cx).context("no playback")?;
        let clip = media_clip();
        playback.update(cx, |p, _| {
            p.set_source_provider(Rc::new(move |_, _, _| Some(Box::new(std::io::Cursor::new(clip.to_vec())) as Box<dyn decibell_media::Source>)))
        });
        Ok(playback)
    })?
}

/// Names of this process's threads that belong to playback.
fn media_threads() -> Vec<String> {
    let mut names = Vec::new();
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            if let Ok(name) = std::fs::read_to_string(task.path().join("comm")) {
                let name = name.trim().to_string();
                if name.starts_with("decibell-media") || name.starts_with("decibell-video") || name.starts_with("decibell-msink") {
                    names.push(name);
                }
            }
        }
    }
    names.sort();
    names
}

/// The first video surface the list drew (attachment id, the centre of its box).
fn video_host(cx: &App) -> Option<(i64, gpui::Point<gpui::Pixels>, f32)> {
    let hosts = cx.try_global::<crate::player::VideoHosts>()?;
    let hosts = hosts.bounds.borrow();
    let (id, bounds) = hosts.iter().filter(|(_, b)| b.size.height > px(60.)).min_by_key(|(id, _)| **id)?;
    Some((*id, bounds.center(), f32::from(bounds.top())))
}

/// Scrolls the fixture channel until a video card's surface is drawn well inside the window
/// and nothing on screen animates; its attachment id and the centre of its box.
async fn find_video(w: &TestCx, list: &Entity<decibell_chat::MessageList>) -> Result<(i64, gpui::Point<gpui::Pixels>)> {
    for step in 0..60 {
        // Only what the next frame draws counts.
        w.update(|_, cx| {
            if let Some(hosts) = cx.try_global::<crate::player::VideoHosts>() {
                hosts.bounds.borrow_mut().clear();
            }
            let delta = if step == 0 { 1. } else { -240. };
            scroll_by(list, delta, cx);
        })?;
        w.sleep(Duration::from_millis(500)).await;
        let Some((id, at, top)) = w.update(|_, cx| video_host(cx))? else { continue };
        if top <= 90. || f32::from(at.y) >= 560. {
            continue;
        }
        // Nothing else on screen may be drawing (the channel has animated pictures too).
        let before = w.frames()?;
        w.sleep(Duration::from_millis(2500)).await;
        if w.frames()?.since(before).drawn == 0 {
            return Ok((id, at));
        }
    }
    bail!("no video card at rest in the fixture channel")
}

fn active_video(cx: &App) -> Option<i64> {
    model(cx).chat.video.read(cx).active.as_ref().map(|a| a.attachment_id)
}

/// Frames in a window of `seconds` in which nothing should draw. Another window taking or
/// giving the focus redraws ours once (GPUI; the desktop is shared with other tests): a
/// disturbed window is measured again, up to three times, and the quietest one counts.
async fn frames_at_rest(w: &TestCx, seconds: u64) -> Result<Frames> {
    let mut best: Option<Frames> = None;
    for _ in 0..3 {
        let before = w.frames()?;
        w.sleep(Duration::from_secs(seconds)).await;
        let frames = w.frames()?.since(before);
        if best.is_none_or(|b| frames.drawn + frames.presented < b.drawn + b.presented) {
            best = Some(frames);
        }
        if frames.drawn == 0 && frames.presented == 0 {
            break;
        }
    }
    best.context("no measurement")
}

async fn check_media_idle(cx: TestCx) -> Result<()> {
    let (w, pane) = fixture_window(&cx, crate::fixture::ATTACHMENTS)?;
    let result = async {
        media_setup(&w)?;
        let list = w.update(|_, cx| pane.read(cx).list.clone())?;
        find_video(&w, &list).await?;
        w.mouse_move(point(px(4.), px(4.)))?;
        w.sleep(Duration::from_millis(1500)).await;
        w.settle().await?;
        let frames = frames_at_rest(&w, 5).await?;
        let threads = media_threads();
        let decoders = w.update(|_, cx| decibell_video::VideoHub::global(cx).decoder_threads())?;
        cx.log(format_args!(
            "video and audio cards at rest for 5 s: {} frames drawn, {} presented; playback threads {threads:?}; decoder threads {decoders}",
            frames.drawn, frames.presented
        ));
        ensure!(frames.drawn == 0 && frames.presented == 0, "{} frames drawn, {} presented with nothing playing", frames.drawn, frames.presented);
        ensure!(threads.is_empty() && decoders == 0, "threads with nothing playing: {threads:?}");
        anyhow::Ok(())
    }
    .await;
    let _ = w.update(|window, _| window.remove_window());
    result
}

async fn check_media_video(cx: TestCx) -> Result<()> {
    let (w, pane) = fixture_window(&cx, crate::fixture::ATTACHMENTS)?;
    let result = async {
        let playback = media_setup(&w)?;
        let list = w.update(|_, cx| pane.read(cx).list.clone())?;
        let (id, at) = find_video(&w, &list).await?;
        w.settle().await?;
        let memory_before = private_mb();
        let hub = w.update(|_, cx| decibell_video::VideoHub::global(cx))?;
        let stream = hub.stream(&format!("media:{}:{id}", daily_gaming().wire()));
        let clicked = Instant::now();
        w.click(at)?;
        wait_for(&w, Duration::from_secs(2), "the card to become the playing one", move |cx| active_video(cx) == Some(id)).await?;
        let s = stream.clone();
        wait_for(&w, Duration::from_secs(10), "the first picture", move |_| s.stats().presented > 0).await?;
        let first = clicked.elapsed();
        let stats = stream.stats();
        cx.log(format_args!(
            "click to first picture on screen: {} ms; {}x{} {} decoded with {}",
            first.as_millis(),
            stats.width,
            stats.height,
            stats.codec.map_or("?", |c| c.label()),
            stats.backend.map_or("no decoder", |b| b.label())
        ));
        // The pointer leaves the video: the controls go (3 s after the last move while it is on
        // it), and from then on a picture is a present, not a frame of the list.
        w.mouse_move(point(px(4.), px(4.)))?;
        w.sleep(Duration::from_millis(600)).await;
        let (frames_before, stats_before, at_before, threads_before) = (w.frames()?, stream.stats(), Instant::now(), thread_table());
        w.sleep(Duration::from_secs(4)).await;
        let (frames, stats, seconds) = (w.frames()?.since(frames_before), stream.stats(), at_before.elapsed().as_secs_f64());
        cx.log(format_args!("threads while playing: {}", thread_delta(&threads_before, seconds)));
        let presented = stats.presented - stats_before.presented;
        let published = stats.published - stats_before.published;
        cx.log(format_args!(
            "playing, controls hidden, {seconds:.2} s: {published} pictures published ({:.2} per s), {presented} presented, {} overtaken, {} dropped; the window drew {} frames and presented {}; playback threads {:?}",
            published as f64 / seconds,
            stats.replaced - stats_before.replaced,
            stats.dropped - stats_before.dropped,
            frames.drawn,
            frames.presented,
            media_threads()
        ));
        ensure!(published > 0 && stats.dropped == stats_before.dropped, "pictures were shed");
        ensure!(frames.drawn <= 2, "the list drew {} frames while a video played with its controls hidden", frames.drawn);
        let memory_playing = private_mb();
        // Pause: a click on the picture (220 ms later, unless a second click follows).
        w.click(at)?;
        w.sleep(Duration::from_millis(900)).await;
        w.mouse_move(point(px(4.), px(4.)))?;
        w.settle().await?;
        let stats_before = stream.stats();
        let cpu_before = thread_cpu_ms();
        let frames = frames_at_rest(&w, 3).await?;
        let stats = stream.stats();
        let cpu = thread_cpu_ms() - cpu_before;
        cx.log(format_args!(
            "paused 3 s: {} pictures published, {} frames drawn, {} presented; playback threads {:?} used {cpu} ms of CPU",
            stats.published - stats_before.published,
            frames.drawn,
            frames.presented,
            media_threads()
        ));
        ensure!(stats.published == stats_before.published, "pictures while paused");
        ensure!(frames.drawn == 0 && frames.presented == 0, "{} frames drawn, {} presented while paused", frames.drawn, frames.presented);
        ensure!(!media_threads().iter().any(|t| t.starts_with("decibell-msink")), "the audio sink is still open 3 s after a pause");
        ensure!(cpu <= 2, "the paused player's threads used {cpu} ms of CPU in 3 s");
        // Resume, seek, the end.
        w.click(at)?;
        let (s, n) = (stream.clone(), stats.published);
        wait_for(&w, Duration::from_secs(5), "pictures after the resume", move |_| s.stats().published > n + 5).await?;
        let duration = w.update(|_, cx| playback.read(cx).video_times().1)?;
        let target = (duration - 2_000_000).max(0);
        let sought = Instant::now();
        let n = stream.stats().published;
        w.update(|_, cx| playback.update(cx, |p, cx| p.seek_video_to(target, cx)))?;
        let s = stream.clone();
        wait_for(&w, Duration::from_secs(10), "the picture of the seek", move |_| s.stats().published > n).await?;
        let position = w.update(|_, cx| playback.read(cx).video_times().0)?;
        cx.log(format_args!(
            "seek to {:.2} s of {:.2} s: picture after {} ms, position {:.2} s",
            target as f64 / 1e6,
            duration as f64 / 1e6,
            sought.elapsed().as_millis(),
            position as f64 / 1e6
        ));
        ensure!((position - target).abs() < 500_000, "the position after the seek is {position}");
        let p = playback.clone();
        wait_for(&w, Duration::from_secs(15), "the end", move |cx| p.read(cx).video_phase() == Some(decibell_media::Phase::Ended)).await?;
        // Gone: another channel's worth of nothing.
        w.update(|_, cx| model(cx).chat.video.update(cx, |v, cx| v.set_active(None, cx)))?;
        wait_for(&w, Duration::from_secs(10), "the player's threads to end", |_| media_threads().is_empty()).await?;
        w.sleep(Duration::from_millis(800)).await;
        let memory_after = private_mb();
        cx.log(format_args!(
            "private memory: {memory_before:.1} MB before, {memory_playing:.1} MB playing, {memory_after:.1} MB after ({:+.1} MB)",
            memory_after - memory_before
        ));
        anyhow::Ok(())
    }
    .await;
    let _ = w.update(|window, _| window.remove_window());
    result
}

/// Per thread name: CPU milliseconds and voluntary context switches (wakeups) so far.
fn thread_table() -> std::collections::BTreeMap<String, (u64, u64)> {
    let mut table = std::collections::BTreeMap::new();
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default().trim().to_string();
            let stat = std::fs::read_to_string(task.path().join("stat")).unwrap_or_default();
            let fields: Vec<&str> = stat.rsplit(") ").next().unwrap_or("").split_whitespace().collect();
            let cpu = (fields.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + fields.get(12).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0)) * 10;
            let status = std::fs::read_to_string(task.path().join("status")).unwrap_or_default();
            let wakes = status.lines().find(|l| l.starts_with("voluntary_ctxt_switches")).and_then(|l| l.split_whitespace().nth(1)).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            let entry = table.entry(name).or_insert((0, 0));
            entry.0 += cpu;
            entry.1 += wakes;
        }
    }
    table
}

/// What each thread name did between two tables, busiest first: "name cpu ms / wakeups".
fn thread_delta(before: &std::collections::BTreeMap<String, (u64, u64)>, seconds: f64) -> String {
    let now = thread_table();
    let mut rows: Vec<(String, u64, u64)> = now
        .iter()
        .map(|(name, (cpu, wakes))| {
            let was = before.get(name).copied().unwrap_or((0, 0));
            (name.clone(), cpu.saturating_sub(was.0), wakes.saturating_sub(was.1))
        })
        .filter(|(_, cpu, wakes)| *cpu > 0 || *wakes as f64 / seconds >= 1.)
        .collect();
    rows.sort_by(|a, b| b.1.cmp(&a.1).then(b.2.cmp(&a.2)));
    let total: u64 = rows.iter().map(|r| r.1).sum();
    let list: Vec<String> = rows.iter().take(10).map(|(name, cpu, wakes)| format!("{name} {:.1} % / {:.0} wakes per s", *cpu as f64 / seconds / 10., *wakes as f64 / seconds)).collect();
    format!("{:.1} % of a core in all; {}", total as f64 / seconds / 10., list.join("; "))
}

/// CPU time (user + system) of this process's playback threads, in milliseconds.
fn thread_cpu_ms() -> u64 {
    let mut ticks = 0u64;
    if let Ok(tasks) = std::fs::read_dir("/proc/self/task") {
        for task in tasks.flatten() {
            let name = std::fs::read_to_string(task.path().join("comm")).unwrap_or_default();
            if !(name.starts_with("decibell-media") || name.starts_with("decibell-video") || name.starts_with("decibell-msink")) {
                continue;
            }
            if let Ok(stat) = std::fs::read_to_string(task.path().join("stat"))
                && let Some(rest) = stat.rsplit(") ").next()
            {
                let fields: Vec<&str> = rest.split_whitespace().collect();
                ticks += fields.get(11).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0) + fields.get(12).and_then(|v| v.parse::<u64>().ok()).unwrap_or(0);
            }
        }
    }
    ticks * 10
}

fn fixture_media(id: i64) -> decibell_model::chat::viewers::ActiveMedia {
    decibell_model::chat::viewers::ActiveMedia {
        attachment_id: id,
        server: daily_gaming(),
        channel: ChannelId::from("media-test"),
        filename: "clip".into(),
        width: 0,
        height: 0,
    }
}

async fn check_media_audio(cx: TestCx) -> Result<()> {
    let (w, pane) = fixture_window(&cx, crate::fixture::ATTACHMENTS)?;
    let result = async {
        media_setup(&w)?;
        let list = w.update(|_, cx| pane.read(cx).list.clone())?;
        rest(&w, &list).await?;
        let audio = w.update(|_, cx| model(cx).chat.audio.clone())?;
        w.update(|_, cx| audio.update(cx, |a, cx| a.set_active(Some(fixture_media(990_001)), cx)))?;
        let a = audio.clone();
        let t = wait_for(&w, Duration::from_secs(10), "the sound to start", move |cx| a.read(cx).playing && a.read(cx).time_secs > 0.3).await?;
        let (time, duration) = w.update(|_, cx| (audio.read(cx).time_secs, audio.read(cx).duration_secs))?;
        cx.log(format_args!("audio: playing {} ms after it became the active one, at {time:.2} s of {duration:.2} s; threads {:?}", t.as_millis(), media_threads()));
        ensure!(duration > 59. && duration < 61., "duration {duration}");
        ensure!(media_threads().iter().any(|t| t.starts_with("decibell-msink")), "no sink stream while playing");
        ensure!(!media_threads().iter().any(|t| t.starts_with("decibell-video")), "a video decoder for an audio attachment");
        // The card's button: the mirrored state flips, the player follows.
        w.update(|_, cx| audio.update(cx, |a, cx| a.set_playback(Some(false), None, None, cx)))?;
        w.sleep(Duration::from_millis(900)).await;
        let paused_at = w.update(|_, cx| audio.read(cx).time_secs)?;
        w.settle().await?;
        let frames = frames_at_rest(&w, 2).await?;
        let now = w.update(|_, cx| audio.read(cx).time_secs)?;
        cx.log(format_args!("audio paused at {paused_at:.2} s: {} frames in 2 s, threads {:?}", frames.drawn, media_threads()));
        ensure!((now - paused_at).abs() < 0.01, "the time moved while paused");
        ensure!(frames.drawn == 0, "{} frames while paused", frames.drawn);
        ensure!(!media_threads().iter().any(|t| t.starts_with("decibell-msink")), "the sink is open while paused");
        w.update(|_, cx| {
            if let Some(p) = crate::playback::Playback::global(cx) {
                p.update(cx, |p, cx| p.seek_audio(58. / 60., cx));
            }
            audio.update(cx, |a, cx| a.set_playback(Some(true), None, None, cx));
        })?;
        let a = audio.clone();
        wait_for(&w, Duration::from_secs(5), "the seek", move |cx| a.read(cx).time_secs > 57.9).await?;
        let a = audio.clone();
        wait_for(&w, Duration::from_secs(10), "the end", move |cx| !a.read(cx).playing).await?;
        let end = w.update(|_, cx| audio.read(cx).time_secs)?;
        cx.log(format_args!("audio: sought to 58 s, ended at {end:.2} s"));
        w.update(|_, cx| audio.update(cx, |a, cx| a.set_active(None, cx)))?;
        wait_for(&w, Duration::from_secs(10), "the player's thread to end", |_| media_threads().is_empty()).await?;
        anyhow::Ok(())
    }
    .await;
    let _ = w.update(|window, _| window.remove_window());
    result
}

async fn check_media_persist(cx: TestCx) -> Result<()> {
    let (w, pane) = fixture_window(&cx, crate::fixture::ATTACHMENTS)?;
    let result = async {
        let playback = media_setup(&w)?;
        let list = w.update(|_, cx| pane.read(cx).list.clone())?;
        let (id, at) = find_video(&w, &list).await?;
        w.settle().await?;
        let hub = w.update(|_, cx| decibell_video::VideoHub::global(cx))?;
        let stream = hub.stream(&format!("media:{}:{id}", daily_gaming().wire()));
        w.click(at)?;
        let s = stream.clone();
        wait_for(&w, Duration::from_secs(10), "the first picture", move |_| s.stats().presented > 0).await?;
        // Scroll its row away.
        let (low, high) = w.update(|_, cx| list.read(cx).scroll_range())?;
        let here = w.update(|_, cx| list.update(cx, |l, cx| l.scroll_position(cx)))?;
        let away = if here - low > high - here { low - here } else { high - here };
        ensure!(away.abs() > 600., "the fixture channel is too short to scroll the video away ({low}..{high}, at {here})");
        w.update(|_, cx| scroll_by(&list, away, cx))?;
        rest(&w, &list).await?;
        w.sleep(Duration::from_millis(400)).await;
        let (before, position_before) = (stream.stats(), w.update(|_, cx| playback.read(cx).video_times().0)?);
        w.sleep(Duration::from_secs(2)).await;
        let (after, position_after) = (stream.stats(), w.update(|_, cx| playback.read(cx).video_times().0)?);
        cx.log(format_args!(
            "row scrolled away for 2 s: the position went {:.2} → {:.2} s, {} pictures published, threads {:?}",
            position_before as f64 / 1e6,
            position_after as f64 / 1e6,
            after.published - before.published,
            media_threads()
        ));
        ensure!(position_after - position_before > 1_500_000, "playback did not go on while the row was away");
        ensure!(after.published == before.published, "pictures were published for a video nothing shows");
        ensure!(w.update(|_, cx| active_video(cx))? == Some(id), "still the active video");
        // Back: pictures again.
        w.update(|_, cx| scroll_by(&list, -away, cx))?;
        let (s, n) = (stream.clone(), after.published);
        wait_for(&w, Duration::from_secs(5), "pictures after scrolling back", move |_| s.stats().published > n + 5).await?;
        cx.log("scrolled back: pictures again");
        // A second video replaces the first; an audio attachment plays beside it.
        w.update(|_, cx| {
            let m = model(cx);
            m.chat.video.update(cx, |v, cx| v.set_active(Some(fixture_media(990_002)), cx));
            m.chat.audio.update(cx, |a, cx| a.set_active(Some(fixture_media(990_003)), cx));
        })?;
        let a = w.update(|_, cx| model(cx).chat.audio.clone())?;
        wait_for(&w, Duration::from_secs(10), "the audio beside the video", move |cx| a.read(cx).time_secs > 0.3).await?;
        let p = playback.clone();
        wait_for(&w, Duration::from_secs(10), "the second video", move |cx| p.read(cx).video_times().0 > 300_000).await?;
        let threads = media_threads();
        let controls = threads.iter().filter(|t| t.as_str() == "decibell-media").count();
        cx.log(format_args!("a second video and an audio attachment: threads {threads:?}"));
        ensure!(controls == 2, "{controls} players for one video and one audio");
        ensure!(w.update(|_, cx| playback.read(cx).active_ids())? == (Some(990_002), Some(990_003)), "the active attachments");
        // Leaving the channel stops both (`clearChannel`).
        w.update(|_, cx| model(cx).nav.update(cx, |nav, cx| nav.set_active_channel(Some(ChannelId::from("elsewhere")), cx)))?;
        w.sleep(Duration::from_millis(200)).await;
        ensure!(w.update(|_, cx| playback.read(cx).active_ids())? == (None, None), "playback survived the channel change");
        wait_for(&w, Duration::from_secs(10), "the players' threads to end", |_| media_threads().is_empty()).await?;
        cx.log("another channel: both stopped, no thread left");
        anyhow::Ok(())
    }
    .await;
    let _ = w.update(|window, _| window.remove_window());
    result
}
