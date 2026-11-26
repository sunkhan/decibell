//! The chat's checks against the local test stack (`tools/dev/README.md`), run inside the
//! signed-in app: `decibell --check chat-live --login perf2` (the `chat-live` self-test starts
//! that child with its own temporary profile, as the shell's tests do, and skips when the stack
//! is down).
//!
//! `chat-live`: Daily Gaming's `#general` (10,000 seeded messages) paged back 2,000 messages by
//! scrolling, each page landing with 0 px of movement of the row being read; a jump to a known
//! message far below; back to the present.

use std::io::BufRead as _;
use std::path::PathBuf;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_harness::{SelfTest, TestCx};
use decibell_model::stores::navigation::ActiveView;
use decibell_model::{ChannelId, Model, ServerId, actions};
use gpui::{App, Entity};

use crate::list::MessageList;

const ACCOUNT: &str = "perf2";

/// The checks `decibell --check <name>` runs inside the signed-in app.
pub fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest {
            name: "chat-live",
            about: "(in the app) #general paged back 2,000 messages by scrolling (0 px per page), a jump, back to the present",
            run: |cx| Box::pin(check_live(cx)),
        },
        SelfTest {
            name: "chat-copy",
            about: "(in the app) DECIBELL_COPY=<channel>:<from id>[@offset]:<to id>[@offset]: copy that selection; prints the text, compares it with DECIBELL_COPY_EXPECT (a file: the Electron client's clipboard for the same selection)",
            run: |cx| Box::pin(check_copy(cx)),
        },
    ]
}

/// The parent self-test: one child app signed in as perf2.
pub fn selftest() -> SelfTest {
    SelfTest {
        name: "chat-live",
        about: "--login perf2 (local stack): #general paged back 2,000 messages, 0 px of movement per page; a jump; the present",
        run: |cx| Box::pin(parent(cx)),
    }
}

// ── The parent ───────────────────────────────────────────────────────────────────────────────

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
    let central = std::env::var("DECIBELL_CENTRAL")
        .ok()
        .or_else(|| json["central"].as_str().map(str::to_string))
        .context("no central address")?;
    let password = json["password"].as_str().context("accounts.json has no password")?.to_string();
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h).trim_start_matches('[').trim_end_matches(']');
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the chat's live tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
}

/// A private session bus with nothing on it, for as long as it is held: the child's at-rest key
/// then can't reach the desktop's keyring (a throwaway profile would leave an item there).
struct EmptyBus {
    pid: u32,
    address: String,
}

impl EmptyBus {
    fn start(dir: &std::path::Path) -> Result<EmptyBus> {
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

async fn parent(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = std::env::temp_dir().join(format!("decibell-chat-live-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&profile);
    std::fs::create_dir_all(profile.join("bus"))?;
    let result = match EmptyBus::start(&profile.join("bus")) {
        Ok(bus) => run_child(&cx, &stack, &profile.join("profile"), &bus.address).await,
        Err(e) => Err(e),
    };
    let _ = std::fs::remove_dir_all(&profile);
    result
}

async fn run_child(cx: &TestCx, stack: &Stack, profile: &std::path::Path, bus: &str) -> Result<()> {
    std::fs::create_dir_all(profile)?;
    let mut child = Command::new(std::env::current_exe()?)
        .args(["--check", "chat-live", "--login", ACCOUNT])
        .env("DBUS_SESSION_BUS_ADDRESS", bus)
        .env("DECIBELL_USER_DATA_DIR", profile)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("starting the app")?;
    let stdout = child.stdout.take().expect("piped");
    let stderr = child.stderr.take().expect("piped");
    let started = Instant::now();
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
        bail!("chat-live failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(())
}

// ── The check ────────────────────────────────────────────────────────────────────────────────

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
        cx.sleep(Duration::from_millis(20)).await;
    }
}

fn shown_list(cx: &App) -> Option<Entity<MessageList>> {
    crate::shown_list(cx)
}

struct Live<'a> {
    cx: &'a TestCx,
    list: Entity<MessageList>,
    channel: ChannelId,
}

impl Live<'_> {
    fn with<R>(&self, f: impl FnOnce(&mut MessageList, &mut gpui::Context<MessageList>) -> R) -> Result<R> {
        let list = self.list.clone();
        self.cx.update(|_, cx| list.update(cx, f))
    }

    /// The stored ids of `#general`'s slice.
    fn ids(&self) -> Result<Vec<i64>> {
        let channel = self.channel.clone();
        self.cx.update(|_, cx| {
            let model = model(cx);
            let slice = model.chat.channels.read(cx).get(daily_gaming(), &channel).cloned();
            slice.map_or(Vec::new(), |slice| slice.read(cx).rows().iter().map(|row| row.id).filter(|id| *id > 0).collect())
        })
    }

    /// (oldest stored id, newest stored id, rows) of `#general`'s slice.
    fn slice(&self) -> Result<(i64, i64, usize)> {
        let channel = self.channel.clone();
        self.cx.update(|_, cx| {
            let model = model(cx);
            let slice = model.chat.channels.read(cx).get(daily_gaming(), &channel).cloned();
            slice.map_or((0, 0, 0), |slice| {
                let slice = slice.read(cx);
                (slice.oldest_stored_id(), slice.newest_stored_id(), slice.rows().len())
            })
        })
    }

    /// Until every row is measured and nothing scrolls (without waiting for drawing to stop:
    /// what a copy needs).
    async fn measured(&self) -> Result<()> {
        for _ in 0..500 {
            if self.with(|list, _| list.rows_unmeasured() == 0 && !list.is_animating())? {
                return Ok(());
            }
            self.cx.sleep(Duration::from_millis(20)).await;
        }
        bail!("the list never measured its rows")
    }

    /// Until every row is measured and nothing scrolls, and a frame later (it draws what was
    /// measured). Not "until drawing stops": the signed-in app may keep drawing for reasons of
    /// its own (pictures arriving, presence), which says nothing about the list.
    async fn rest(&self) -> Result<()> {
        self.measured().await?;
        let before = self.cx.frames()?;
        let started = Instant::now();
        while self.cx.frames()?.since(before).drawn == 0 && started.elapsed() < Duration::from_millis(500) {
            self.cx.sleep(Duration::from_millis(16)).await;
        }
        self.measured().await
    }

    /// The first fully visible row and its screen y.
    fn reading(&self) -> Result<(i64, f32)> {
        self.with(|list, _| {
            list.visible_ids()
                .into_iter()
                .filter(|id| *id > 0)
                .filter_map(|id| Some((id, list.row_screen_y(id)?)))
                .find(|(_, y)| *y >= 0.)
                .context("no row on screen")
        })?
    }
}

/// Signed in, Daily Gaming connected, `name` open as the channel list's click opens it.
async fn open_channel<'a>(cx: &'a TestCx, name: &str) -> Result<Live<'a>> {
    let t = wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.log(format_args!("signed in as {ACCOUNT} after {} ms", t.as_millis()));
    wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
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
    wait_for(cx, Duration::from_secs(10), "the chat pane", |cx| shown_list(cx).is_some()).await?;
    let list = cx.update(|_, cx| shown_list(cx))?.context("no list")?;
    Ok(Live { cx, list, channel })
}

async fn check_copy(cx: TestCx) -> Result<()> {
    let spec = std::env::var("DECIBELL_COPY").context("DECIBELL_COPY=<channel>:<from id>:<to id>")?;
    // `<id>` (the whole row) or `<id>@<offset>` (that byte of the body's first paragraph).
    let end_of = |part: Option<&str>, start: bool| -> Option<(i64, crate::row::RowPos)> {
        let (id, offset) = match part?.split_once('@') {
            Some((id, offset)) => (id.parse().ok()?, Some(offset.parse::<u32>().ok()?)),
            None => (part?.parse().ok()?, None),
        };
        let pos = match offset {
            Some(offset) => crate::row::RowPos { part: crate::row::Part::Body, pos: decibell_text::RichPos { block: 0, offset, ..Default::default() } },
            None if start => crate::row::RowPos::START,
            None => crate::row::RowPos::END,
        };
        Some((id, pos))
    };
    let mut parts = spec.split(':');
    let name = parts.next().unwrap_or_default().to_string();
    let (Some((from, from_pos)), Some((to, to_pos))) = (end_of(parts.next(), true), end_of(parts.next(), false)) else {
        bail!("DECIBELL_COPY=<channel>:<from id>[@offset]:<to id>[@offset], not {spec:?}");
    };
    let name = name.as_str();
    let live = open_channel(&cx, name).await?;
    wait_for(&cx, Duration::from_secs(15), "the newest page", |cx| shown_list(cx).is_some_and(|list| list.read(cx).row_count() > 0)).await?;
    live.measured().await?;
    if live.with(|list, _| list.row(from).is_none() || list.row(to).is_none())? {
        live.with(|list, cx| list.jump_to_message(from, cx))?;
        wait_for(&cx, Duration::from_secs(10), "the rows", |cx| {
            shown_list(cx).is_some_and(|list| list.read(cx).row(from).is_some() && list.read(cx).row(to).is_some())
        })
        .await?;
        live.measured().await?;
    }
    // Nicknames arrive with the roster's lookups after the rows are built: copy until two copies
    // a second apart agree.
    let mut text = String::new();
    for _ in 0..10 {
        let now = live
            .with(|list, cx| {
                list.select((from, from_pos), (to, to_pos), cx);
                list.selected_text(cx)
            })?
            .context("nothing selected")?;
        if now == text {
            break;
        }
        text = now;
        cx.sleep(Duration::from_secs(1)).await;
    }
    println!("decibell: copy {}", serde_json::to_string(&text)?);
    if let Ok(path) = std::env::var("DECIBELL_COPY_EXPECT") {
        let expected = std::fs::read_to_string(&path).with_context(|| format!("reading {path}"))?;
        if text != expected {
            let at = text.char_indices().zip(expected.char_indices()).find(|((_, a), (_, b))| a != b).map_or(text.len().min(expected.len()), |((i, _), _)| i);
            bail!(
                "the copy differs from {path} at byte {at}:\nours:     {:?}\nexpected: {:?}",
                &text[text.floor_char_boundary(at.saturating_sub(40))..text.floor_char_boundary((at + 80).min(text.len()))],
                &expected[expected.floor_char_boundary(at.saturating_sub(40))..expected.floor_char_boundary((at + 80).min(expected.len()))]
            );
        }
        cx.log(format_args!("the copy of {name} {from}..={to} equals {path}, byte for byte ({} bytes)", text.len()));
    }
    Ok(())
}

async fn check_live(cx: TestCx) -> Result<()> {
    let live = open_channel(&cx, "general").await?;
    let t = wait_for(&cx, Duration::from_secs(15), "#general's newest page", |cx| {
        let list = shown_list(cx);
        list.is_some_and(|list| list.read(cx).row_count() > 0)
    })
    .await?;
    live.rest().await?;
    let (oldest0, newest0, rows0) = live.slice()?;
    ensure!(live.with(|list, _| list.at_bottom())?, "#general opens at its newest message");
    cx.log(format_args!("#general: newest page in after {} ms, {rows0} rows, ids {oldest0}..{newest0}", t.as_millis()));

    // Back through history: rest 900 px under the top (outside the 800 px paging zone), read,
    // then scroll 200 px up (into it): the page that request brings must move nothing more.
    const STEP: f64 = 200.;
    let started = Instant::now();
    let mut pages = 0usize;
    let mut worst = 0f32;
    // Every message older than the first page that came in (the slice trims what it passed).
    let mut seen = std::collections::BTreeSet::new();
    loop {
        seen.extend(live.ids()?.into_iter().filter(|id| *id < oldest0));
        if seen.len() >= 2_000 {
            break;
        }
        ensure!(started.elapsed() < Duration::from_secs(120), "paging stopped at {} messages back", seen.len());
        live.with(|list, cx| {
            let (min, _) = list.scroll_range();
            let position = list.scroll_position(cx);
            list.scroll_by_precise(min + 900. - position, cx);
        })?;
        live.rest().await?;
        let (reading, before) = live.reading()?;
        let (oldest_before, _, _) = live.slice()?;
        live.with(|list, cx| list.scroll_by_precise(-STEP, cx))?;
        wait_for(&cx, Duration::from_secs(10), "the next older page", |cx| {
            let model = model(cx);
            let slice = model.chat.channels.read(cx).get(daily_gaming(), &live.channel).cloned();
            slice.is_some_and(|slice| slice.read(cx).oldest_stored_id() < oldest_before)
        })
        .await?;
        live.rest().await?;
        let after = live.with(|list, _| list.row_screen_y(reading))?.with_context(|| format!("row {reading} left the list"))?;
        let moved = after - (before + STEP as f32);
        worst = worst.max(moved.abs());
        ensure!(moved == 0., "page {}: row {reading} moved {moved} px when the page landed (y {before} → {after}, scrolled {STEP})", pages + 1);
        pages += 1;
    }
    let (oldest, newest, rows) = live.slice()?;
    cx.log(format_args!(
        "{pages} pages back by scrolling in {:.1} s: {} messages, ids now {oldest}..{newest} ({rows} rows kept); the row being read moved 0 px on every page (largest {worst})",
        started.elapsed().as_secs_f64(),
        seen.len()
    ));
    ensure!(live.with(|list, _| list.shows_pill())?, "\"Jump to present\" shows far back in history");

    // A jump to a message paged past and trimmed away since: 1,000 newer than the oldest seen.
    let target = *seen.iter().nth(seen.len() - 1_000).context("too few messages seen")?;
    ensure!(live.with(|list, _| list.row_screen_y(target).is_none())?, "the jump target {target} is not in the list any more");
    let jumped = Instant::now();
    live.with(|list, cx| list.jump_to_message(target, cx))?;
    wait_for(&cx, Duration::from_secs(10), "the jump to land", |cx| {
        shown_list(cx).is_some_and(|list| list.read(cx).row_screen_y(target).is_some())
    })
    .await?;
    live.rest().await?;
    let (y, height) = live.with(|list, _| (list.row_screen_y(target), f32::from(list.bounds().size.height)))?;
    let y = y.context("the jump target left the screen")?;
    ensure!(y >= 0. && y < height, "the jump target is on screen (y {y} of {height})");
    cx.log(format_args!("jump to {target}: on screen at y = {y} after {} ms", jumped.elapsed().as_millis()));

    // Back to the present: the newest message, at the bottom.
    let back = Instant::now();
    live.with(|list, cx| list.jump_to_present(cx))?;
    wait_for(&cx, Duration::from_secs(10), "the present", |cx| {
        shown_list(cx).is_some_and(|list| list.read(cx).at_bottom() && !list.read(cx).shows_pill())
    })
    .await?;
    live.rest().await?;
    let (_, newest_now, _) = live.slice()?;
    ensure!(newest_now >= newest0, "the present has the newest message ({newest_now} < {newest0})");
    let bottom = live.with(|list, _| list.visible_ids().last().copied())?;
    ensure!(bottom == Some(newest_now), "the newest message is the last row on screen ({bottom:?}, {newest_now})");
    cx.log(format_args!("back to the present in {} ms: newest {newest_now} at the bottom", back.elapsed().as_millis()));
    Ok(())
}
