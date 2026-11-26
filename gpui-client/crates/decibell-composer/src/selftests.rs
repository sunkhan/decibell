//! The composer's self-tests, against the local test stack (`tools/dev/README.md`) as `perf2`.
//!
//! Each `composer-*` / `emoji-picker` / `gif-picker` self-test starts the app again in a child
//! process — `decibell --check <name> --login perf2` with its own temporary profile, the stack's
//! `DECIBELL_CENTRAL` — and passes when the check inside it does (the core runs once per process
//! and owns its profile). They need a `dev-local` build, refuse a non-loopback `DECIBELL_CENTRAL`,
//! and skip, saying so, when the stack is down. They post only in `#off-topic` (and `#slow-lane`
//! for slowmode) and delete everything they post.
//!
//! The checks drive the real window: keys go through the harness (`TestCx::keystroke`,
//! `type_text`), the model is read for what the server did.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use decibell_attachments::upload::UploadSource;
use decibell_chat::{ListEvent, MessageList};
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_model::chat::{DraftKey, Message};
use decibell_model::{ChannelId, Model, ServerId, actions};
use gpui::{App, Entity, Focusable as _};

use crate::Composer;
use crate::picker::Tab;

const ACCOUNT: &str = "perf2";
const OFF_TOPIC: &str = "off-topic";
const SLOW_LANE: &str = "slow-lane";

macro_rules! tests {
    ($($name:literal, $about:literal, $check:ident;)*) => {
        /// The parents: one child app each.
        pub fn selftests() -> Vec<SelfTest> {
            vec![$(SelfTest { name: $name, about: $about, run: |cx| Box::pin(parent(cx, $name)) },)*]
        }

        /// The checks, inside the signed-in child app.
        pub fn checks() -> Vec<SelfTest> {
            vec![$(SelfTest { name: $name, about: concat!("(in the app) ", $about), run: |cx| Box::pin($check(cx)) },)*]
        }
    };
}

tests! {
    "composer-type-send", "type a formatted message (the preview strip shows), Enter sends: pending bubble, settled, box cleared; Shift+Enter and the open fence break lines", check_type_send;
    "composer-draft", "a draft typed in #off-topic survives a visit to #general and back; sending clears it", check_draft;
    "composer-burst", "15 quick sends are paced, none dropped, order kept", check_burst;
    "composer-slowmode", "#slow-lane: the second send is held (text kept, the pill counts down once a second), goes out after the window", check_slowmode;
    "composer-refused", "a message over the server's 64 KB is refused: the bubble goes, the text comes back", check_refused;
    "composer-edit", "ArrowUp on an empty box edits the last own message inline; Enter saves (\"(edited)\"), Escape cancels", check_edit;
    "composer-reply", "Reply on a message: the bar and the rail; the sent row carries the reply", check_reply;
    "composer-attach", "a generated PNG and a text file attached as the dialog delivers them: chips, sent, the message has both", check_attach;
    "composer-paste-image", "X11: an image on the clipboard pasted into the box becomes a chip (the clipboard is restored)", check_paste_image;
    "emoji-picker", "open the picker (open time, frames), 0 frames at rest, search \"smile\", pick: an atom in the box, sent as the character", check_emoji_picker;
    "gif-picker", "GIFs tab: search (debounced), pick: the URL is sent (the canned provider without a key)", check_gif_picker;
    "composer-idle", "focused in the active window only the caret's blink draws; blurred: 0 frames over 5 s", check_idle;
    "composer-typing-cost", "frames per key, µs per key, and the message list draws nothing while typing", check_typing_cost;
}

// ── The parent side ────────────────────────────────────────────────────────────────────────────

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
    let host = central.rsplit_once(':').map_or(central.as_str(), |(h, _)| h);
    let loopback = host == "localhost" || host.parse::<std::net::IpAddr>().is_ok_and(|ip| ip.is_loopback());
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the composer's tests run against the local stack only");
    let reachable = central
        .parse::<std::net::SocketAddr>()
        .ok()
        .is_some_and(|addr| std::net::TcpStream::connect_timeout(&addr, Duration::from_secs(1)).is_ok());
    Ok(reachable.then_some(Stack { central, password }))
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

async fn parent(cx: TestCx, check: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = TempDir::new(check)?;
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--check")
        .arg(check)
        .arg("--login")
        .arg(ACCOUNT)
        .env("DECIBELL_USER_DATA_DIR", &profile.0)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    if check == "composer-paste-image" {
        // X11 (XWayland): GPUI reads clipboard pictures there. The clipboard itself is set with
        // the Wayland tools, which KWin bridges to X11.
        command.env("DECIBELL_TEST_WAYLAND_DISPLAY", std::env::var("WAYLAND_DISPLAY").unwrap_or_default()).env("WAYLAND_DISPLAY", "");
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
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    }
    if !status.success() {
        for line in err.iter().rev().take(16).rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{check} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(())
}

// ── Helpers inside the app ─────────────────────────────────────────────────────────────────────

fn model(cx: &App) -> Model {
    Model::global(cx).clone()
}

fn daily_gaming() -> ServerId {
    ServerId::from_central(1)
}

/// Polls `ready` every 20 ms for at most `limit`.
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

/// A word that names this run's posts.
fn tag() -> String {
    let millis = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis());
    format!("ct{}-{}", std::process::id(), millis % 1_000_000)
}

fn composer_of(channel: &str, cx: &App) -> Option<Entity<Composer>> {
    Composer::active(cx).filter(|c| c.read(cx).target().draft_key() == DraftKey::Channel(daily_gaming(), ChannelId::from(channel.to_string())))
}

/// Signed in, Daily Gaming connected, `channel` on screen with its first page in and its
/// composer mounted.
async fn open(cx: &TestCx, channel: &'static str) -> Result<Entity<Composer>> {
    wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    cx.update(|_, cx| {
        actions::servers::open_server(daily_gaming(), cx);
    })?;
    wait_for(cx, Duration::from_secs(10), "Daily Gaming's channels", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.channels.read(cx).contains(channel))
    })
    .await?;
    cx.update(|_, cx| {
        let model = model(cx);
        model.nav.update(cx, |nav, cx| nav.set_active_channel(Some(channel.into()), cx));
    })?;
    wait_for(cx, Duration::from_secs(15), &format!("#{channel} and its composer"), |cx| {
        composer_of(channel, cx).is_some()
            && model(cx).chat.channels.read(cx).get(daily_gaming(), channel).is_some_and(|s| s.read(cx).fetched)
    })
    .await?;
    let composer = cx.update(|_, cx| composer_of(channel, cx))?.context("no composer")?;
    Ok(composer)
}

fn focus_input(cx: &TestCx, composer: &Entity<Composer>) -> Result<()> {
    cx.update(|window, cx| {
        let input = composer.read(cx).input().clone();
        input.update(cx, |input, cx| input.focus(window, cx));
    })
}

fn value(cx: &TestCx, composer: &Entity<Composer>) -> Result<String> {
    cx.update(|_, cx| composer.read(cx).input().read(cx).value().to_string())
}

fn set_value(cx: &TestCx, composer: &Entity<Composer>, text: &str) -> Result<()> {
    cx.update(|_, cx| {
        let input = composer.read(cx).input().clone();
        input.update(cx, |input, cx| input.set_value(text, cx));
    })
}

/// Our rows in `channel` whose text is `content`: (id, pending).
fn own_rows(channel: &str, content: &str, cx: &App) -> Vec<(i64, bool)> {
    let model = model(cx);
    let me = model.auth.read(cx).uid;
    let Some(slice) = model.chat.channels.read(cx).get(daily_gaming(), channel).cloned() else { return Vec::new() };
    slice.read(cx).rows().iter().filter(|m| m.sender_uid == me && *m.content == *content).map(|m| (m.id, m.pending)).collect()
}

fn row(channel: &str, id: i64, cx: &App) -> Option<std::sync::Arc<Message>> {
    model(cx).chat.channels.read(cx).get(daily_gaming(), channel).and_then(|s| s.read(cx).find(id).cloned())
}

/// Waits until our `content` is stored in `channel` (the echo), and returns its id.
async fn echoed(cx: &TestCx, channel: &str, content: &str) -> Result<i64> {
    let (channel_owned, content_owned) = (channel.to_string(), content.to_string());
    wait_for(cx, Duration::from_secs(20), &format!("the echo of {content:.40?}"), move |cx| {
        own_rows(&channel_owned, &content_owned, cx).iter().any(|(id, pending)| *id > 0 && !pending)
    })
    .await?;
    let id = cx.update(|_, cx| own_rows(channel, content, cx).into_iter().find(|(id, pending)| *id > 0 && !pending).map(|(id, _)| id))?;
    id.context("echo vanished")
}

/// Sends `text` through the box with Enter and returns the stored id.
async fn send(cx: &TestCx, composer: &Entity<Composer>, channel: &str, text: &str) -> Result<i64> {
    focus_input(cx, composer)?;
    set_value(cx, composer, text)?;
    cx.keystroke("enter")?;
    echoed(cx, channel, text).await
}

/// Deletes what a test posted, and waits until it's gone.
async fn delete(cx: &TestCx, channel: &'static str, ids: &[i64]) -> Result<()> {
    for id in ids.iter().copied().filter(|id| *id > 0) {
        cx.update(|_, cx| actions::chat::delete_message(daily_gaming(), channel.into(), id, cx))?;
    }
    let count = ids.len();
    let ids = ids.to_vec();
    // Gone from the slice at once (optimistic); deleted once the server confirmed it (the pending
    // deletion clears), which must happen before the app quits.
    wait_for(cx, Duration::from_secs(20), "the server to confirm the deletes", move |cx| {
        let slice = model(cx).chat.channels.read(cx).get(daily_gaming(), channel).cloned();
        ids.iter().all(|id| row(channel, *id, cx).is_none() && !slice.as_ref().is_some_and(|s| s.read(cx).is_deleting(*id)))
    })
    .await?;
    cx.log(format_args!("deleted the {count} message(s) posted"));
    Ok(())
}

/// Waits until nothing has been drawn for 500 ms; the counters then.
async fn settle_drawn(cx: &TestCx) -> Result<Frames> {
    let started = Instant::now();
    let mut last = cx.frames()?;
    let mut quiet = Instant::now();
    loop {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames()?;
        if now.drawn != last.drawn {
            quiet = Instant::now();
        } else if quiet.elapsed() >= Duration::from_millis(500) {
            return Ok(now);
        }
        last = now;
        ensure!(started.elapsed() < Duration::from_secs(15), "the window was still drawing after 15 s");
    }
}

fn list_of(cx: &TestCx, composer: &Entity<Composer>) -> Result<Entity<MessageList>> {
    cx.update(|_, cx| composer.read(cx).list())?.context("the composer has no list")
}

// ── The checks ─────────────────────────────────────────────────────────────────────────────────

async fn check_type_send(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let tag = tag();
    focus_input(&cx, &composer)?;
    let before = settle_drawn(&cx).await?;
    let text = format!("**hello** from the composer test `{tag}`");
    cx.type_text(&text)?;
    let after = settle_drawn(&cx).await?;
    let typed = after.since(before);
    ensure!(value(&cx, &composer)? == text, "typed text");
    ensure!(cx.update(|_, cx| composer.read(cx).preview_shown())?, "the preview strip shows for a formatted draft");
    cx.log(format_args!("typed {} keys: {} frames drawn; the preview strip shows", text.chars().count(), typed.drawn));
    ensure!(typed.drawn <= text.chars().count() as u64 + 2, "more than a frame per key ({})", typed.drawn);

    // Enter: the bubble at once (faded), then the echo settles it; the box empties.
    cx.keystroke("enter")?;
    let pending = cx.update(|_, cx| own_rows(OFF_TOPIC, &text, cx))?;
    ensure!(pending.iter().any(|(id, pending)| *id == 0 && *pending), "the optimistic bubble right after Enter: {pending:?}");
    ensure!(value(&cx, &composer)?.is_empty(), "the box cleared on send");
    let id = echoed(&cx, OFF_TOPIC, &text).await?;
    ensure!(!cx.update(|_, cx| composer.read(cx).preview_shown())?, "the strip went with the draft");
    let draft = cx.update(|_, cx| model(cx).chat.drafts.read(cx).get(&DraftKey::Channel(daily_gaming(), OFF_TOPIC.into())))?;
    ensure!(draft.is_empty(), "the draft cleared");
    cx.log(format_args!("sent: pending bubble, then stored as {id}"));

    // Shift+Enter and the open fence break the line instead of sending.
    cx.type_text("a")?;
    cx.keystroke("shift-enter")?;
    ensure!(value(&cx, &composer)? == "a\n", "Shift+Enter breaks the line: {:?}", value(&cx, &composer)?);
    cx.type_text("```")?;
    cx.keystroke("enter")?;
    ensure!(value(&cx, &composer)? == "a\n```\n", "Enter inside an open fence breaks the line: {:?}", value(&cx, &composer)?);
    set_value(&cx, &composer, "")?;
    delete(&cx, OFF_TOPIC, &[id]).await
}

async fn check_draft(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let text = format!("a draft {}", tag());
    focus_input(&cx, &composer)?;
    cx.type_text(&text)?;
    // Away to #general: its box is its own.
    cx.update(|_, cx| model(cx).nav.update(cx, |nav, cx| nav.set_active_channel(Some("general".into()), cx)))?;
    wait_for(&cx, Duration::from_secs(10), "#general's composer", |cx| composer_of("general", cx).is_some()).await?;
    let general = cx.update(|_, cx| composer_of("general", cx))?.context("general")?;
    ensure!(value(&cx, &general)?.is_empty(), "#general's box is empty");
    cx.sleep(Duration::from_millis(300)).await;
    // Back: the draft is there.
    cx.update(|_, cx| model(cx).nav.update(cx, |nav, cx| nav.set_active_channel(Some(OFF_TOPIC.into()), cx)))?;
    wait_for(&cx, Duration::from_secs(10), "#off-topic's composer again", |cx| composer_of(OFF_TOPIC, cx).is_some()).await?;
    let back = cx.update(|_, cx| composer_of(OFF_TOPIC, cx))?.context("off-topic")?;
    ensure!(back != composer, "a fresh composer was mounted");
    ensure!(value(&cx, &back)? == text, "the draft came back: {:?}", value(&cx, &back)?);
    cx.log("the draft survived #general and back");
    focus_input(&cx, &back)?;
    cx.keystroke("enter")?;
    let id = echoed(&cx, OFF_TOPIC, &text).await?;
    let draft = cx.update(|_, cx| model(cx).chat.drafts.read(cx).get(&DraftKey::Channel(daily_gaming(), OFF_TOPIC.into())))?;
    ensure!(draft.is_empty(), "sending cleared the draft");
    delete(&cx, OFF_TOPIC, &[id]).await
}

async fn check_burst(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let tag = tag();
    focus_input(&cx, &composer)?;
    let started = Instant::now();
    let texts: Vec<String> = (0..15).map(|i| format!("burst {i:02} {tag}")).collect();
    for text in &texts {
        set_value(&cx, &composer, text)?;
        cx.keystroke("enter")?;
    }
    cx.log(format_args!("15 sends in {} ms", started.elapsed().as_millis()));
    // When each echo lands (and whether a refusal put a text back in the box).
    let mut landed: Vec<Option<u128>> = vec![None; texts.len()];
    while landed.iter().any(Option::is_none) && started.elapsed() < Duration::from_secs(20) {
        let now = started.elapsed().as_millis();
        cx.update(|_, cx| {
            for (i, text) in texts.iter().enumerate() {
                if landed[i].is_none() && own_rows(OFF_TOPIC, text, cx).iter().any(|(id, pending)| *id > 0 && !pending) {
                    landed[i] = Some(now);
                }
            }
        })?;
        cx.sleep(Duration::from_millis(10)).await;
    }
    cx.log(format_args!("echoes at (ms): {landed:?}; the box holds {:?}", value(&cx, &composer)?));
    let mut ids = Vec::new();
    for text in &texts {
        ids.push(echoed(&cx, OFF_TOPIC, text).await?);
    }
    cx.log(format_args!("all 15 stored after {} ms (paced: 10 at once, then 2.7 / s)", started.elapsed().as_millis()));
    ensure!(ids.windows(2).all(|pair| pair[0] < pair[1]), "stored in the order sent: {ids:?}");
    ensure!(started.elapsed() >= Duration::from_millis(1500), "the burst was not paced ({} ms)", started.elapsed().as_millis());
    delete(&cx, OFF_TOPIC, &ids).await
}

async fn check_slowmode(cx: TestCx) -> Result<()> {
    let composer = open(&cx, SLOW_LANE).await?;
    let (interval, ..) = cx.update(|_, cx| composer.read(cx).slowmode_state(cx))?;
    ensure!(interval == 30, "#slow-lane's interval applies to {ACCOUNT}: {interval}");
    // A window left from an earlier run.
    wait_for(&cx, Duration::from_secs(35), "no slowmode window", |cx| Composer::active(cx).is_some_and(|c| c.read(cx).slowmode_state(cx).1 == 0)).await?;
    let tag = tag();
    let first = format!("slow one {tag}");
    let first_id = send(&cx, &composer, SLOW_LANE, &first).await?;
    let second = format!("slow two {tag}");
    set_value(&cx, &composer, &second)?;
    cx.keystroke("enter")?;
    let (_, remaining, nudges, ticking) = cx.update(|_, cx| composer.read(cx).slowmode_state(cx))?;
    ensure!(value(&cx, &composer)? == second, "the held send keeps its text");
    ensure!(remaining > 0 && nudges >= 1 && ticking, "held: {remaining} s left, {nudges} nudges, ticking {ticking}");
    ensure!(cx.update(|_, cx| own_rows(SLOW_LANE, &second, cx))?.is_empty(), "nothing went out");
    cx.log(format_args!("the second send is held: {remaining} s left, the pill shook"));
    // The countdown draws once a second (the input blurred, so the caret doesn't blink).
    cx.update(|window, cx| {
        let list = composer.read(cx).list();
        if let Some(list) = list {
            list.read(cx).focus_handle().clone().focus(window, cx);
        }
    })?;
    let before = settle_window(&cx, Duration::from_millis(200)).await?;
    cx.sleep(Duration::from_secs(4)).await;
    let ticks = cx.frames()?.since(before);
    cx.log(format_args!("countdown: {} frames drawn in 4 s", ticks.drawn));
    ensure!((3..=6).contains(&ticks.drawn), "the countdown draws once a second ({} in 4 s)", ticks.drawn);
    wait_for(&cx, Duration::from_secs(35), "the window to end", |cx| Composer::active(cx).is_some_and(|c| c.read(cx).slowmode_state(cx).1 == 0)).await?;
    let (_, _, _, ticking) = cx.update(|_, cx| composer.read(cx).slowmode_state(cx))?;
    ensure!(!ticking, "no timer once the window ended");
    focus_input(&cx, &composer)?;
    cx.keystroke("enter")?;
    let second_id = echoed(&cx, SLOW_LANE, &second).await?;
    cx.log("after the window the held text went out");
    delete(&cx, SLOW_LANE, &[first_id, second_id]).await
}

/// A short quiet spell (the frames of what happened just before have drawn).
async fn settle_window(cx: &TestCx, quiet: Duration) -> Result<Frames> {
    let mut last = cx.frames()?;
    let started = Instant::now();
    let mut since = Instant::now();
    loop {
        cx.sleep(Duration::from_millis(20)).await;
        let now = cx.frames()?;
        if now.drawn != last.drawn {
            since = Instant::now();
        } else if since.elapsed() >= quiet {
            return Ok(now);
        }
        last = now;
        if started.elapsed() > Duration::from_secs(5) {
            return Ok(now);
        }
    }
}

async fn check_refused(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    // Over the community's 64 KB cap: refused with CHANNEL_MSG_REJECTED.
    let text = format!("{} {}", tag(), "x".repeat(64 * 1024));
    focus_input(&cx, &composer)?;
    set_value(&cx, &composer, &text)?;
    cx.keystroke("enter")?;
    ensure!(value(&cx, &composer)?.is_empty(), "the box cleared on send");
    wait_for(&cx, Duration::from_secs(15), "the refusal to put the text back", |cx| {
        Composer::active(cx).is_some_and(|c| c.read(cx).input().read(cx).value().len() == text.len())
    })
    .await?;
    ensure!(cx.update(|_, cx| own_rows(OFF_TOPIC, &text, cx))?.is_empty(), "the bubble went");
    cx.log("refused: the bubble went and the text is back in the box");
    set_value(&cx, &composer, "")
}

async fn check_edit(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let tag = tag();
    let original = format!("edit me {tag}");
    let id = send(&cx, &composer, OFF_TOPIC, &original).await?;
    let list = list_of(&cx, &composer)?;
    // ArrowUp in the empty box.
    focus_input(&cx, &composer)?;
    cx.keystroke("up")?;
    ensure!(cx.update(|_, cx| list.read(cx).editing())? == Some(id), "ArrowUp edits the last own message");
    wait_for(&cx, Duration::from_secs(5), "the inline editor", |cx| crate::editor::current_editor(cx).is_some_and(|e| e.read(cx).message_id == id)).await?;
    let editor = cx.update(|_, cx| crate::editor::current_editor(cx))?.context("editor")?;
    let (field, focused) = cx.update(|window, cx| {
        let field = editor.read(cx).input().clone();
        let focused = field.read(cx).focus_handle(cx).is_focused(window);
        (field, focused)
    })?;
    ensure!(focused, "the editor took the focus");
    ensure!(cx.update(|_, cx| field.read(cx).value().to_string())? == original, "prefilled with the message's text");
    let edited = format!("edited {tag}");
    cx.update(|_, cx| field.update(cx, |f, cx| f.set_value(&edited, cx)))?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(10), "the edit to land", |cx| row(OFF_TOPIC, id, cx).is_some_and(|m| *m.content == *edited && m.edited_at > 0)).await?;
    ensure!(cx.update(|_, cx| list.read(cx).editing())?.is_none(), "the editor closed");
    cx.log("Enter saved the edit; the row shows \"(edited)\"");
    // Escape cancels.
    focus_input(&cx, &composer)?;
    cx.keystroke("up")?;
    wait_for(&cx, Duration::from_secs(5), "the editor again", |cx| crate::editor::current_editor(cx).is_some()).await?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape to cancel", |_| true).await?;
    ensure!(cx.update(|_, cx| list.read(cx).editing())?.is_none(), "Escape left edit mode");
    ensure!(cx.update(|_, cx| row(OFF_TOPIC, id, cx).is_some_and(|m| *m.content == *edited))?, "unchanged");
    cx.log("Escape cancelled the second edit");
    delete(&cx, OFF_TOPIC, &[id]).await
}

async fn check_reply(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let tag = tag();
    let base = format!("reply to me {tag}");
    let base_id = send(&cx, &composer, OFF_TOPIC, &base).await?;
    let list = list_of(&cx, &composer)?;
    // The reply arrow of the hover bar emits this.
    cx.update(|_, cx| list.update(cx, |_, cx| cx.emit(ListEvent::Reply(base_id))))?;
    ensure!(cx.update(|_, cx| composer.read(cx).reply())? == Some(base_id), "the composer replies to it");
    let answer = format!("the answer {tag}");
    cx.type_text(&answer)?;
    cx.keystroke("enter")?;
    let id = echoed(&cx, OFF_TOPIC, &answer).await?;
    let replied = cx.update(|_, cx| row(OFF_TOPIC, id, cx).and_then(|m| m.reply.as_ref().map(|r| r.id)))?;
    ensure!(replied == Some(base_id), "the row carries the reply: {replied:?}");
    ensure!(cx.update(|_, cx| composer.read(cx).reply())?.is_none(), "the bar went with the send");
    cx.log("the reply bar showed, the sent row carries the reply line");
    delete(&cx, OFF_TOPIC, &[id, base_id]).await
}

/// A small PNG (a gradient), written to `path`.
fn write_png(path: &Path) -> Result<()> {
    let image = image::RgbaImage::from_fn(64, 48, |x, y| image::Rgba([(x * 4) as u8, (y * 5) as u8, 180, 255]));
    image.save(path)?;
    Ok(())
}

async fn check_attach(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let dir = TempDir::new("composer-attach")?;
    let (png, txt) = (dir.0.join("composer-test.png"), dir.0.join("composer-test.txt"));
    write_png(&png)?;
    std::fs::write(&txt, "a text file from the composer test\n")?;
    // What the file dialog hands over.
    cx.update(|_, cx| composer.update(cx, |c, cx| c.attach(vec![UploadSource::Path(png.clone()), UploadSource::Path(txt.clone())], cx)))?;
    wait_for(&cx, Duration::from_secs(10), "two chips", |cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), OFF_TOPIC).count() == 2).await?;
    cx.log("two chips (queued)");
    let text = format!("attachments {}", tag());
    send_with_attachments(&cx, &composer, &text).await?;
    let id = echoed(&cx, OFF_TOPIC, &text).await?;
    wait_for(&cx, Duration::from_secs(20), "both attachments on the stored row", |cx| row(OFF_TOPIC, id, cx).is_some_and(|m| m.attachments.len() == 2)).await?;
    let names = cx.update(|_, cx| row(OFF_TOPIC, id, cx).map(|m| m.attachments.iter().map(|a| a.filename.to_string()).collect::<Vec<_>>()))?;
    cx.log(format_args!("sent with {names:?}; the chips went"));
    ensure!(cx.update(|_, cx| model(cx).chat.composer.read(cx).for_channel(daily_gaming(), OFF_TOPIC).count())? == 0, "the chips went");
    delete(&cx, OFF_TOPIC, &[id]).await
}

async fn send_with_attachments(cx: &TestCx, composer: &Entity<Composer>, text: &str) -> Result<()> {
    focus_input(cx, composer)?;
    set_value(cx, composer, text)?;
    cx.keystroke("enter")?;
    Ok(())
}

/// The Wayland clipboard as `wl-copy` / `wl-paste` see it (KWin bridges it to X11).
fn wl(tool: &str, args: &[&str], input: Option<&[u8]>) -> Result<Vec<u8>> {
    let display = std::env::var("DECIBELL_TEST_WAYLAND_DISPLAY").unwrap_or_default();
    let mut command = Command::new(tool);
    command.args(args).env("WAYLAND_DISPLAY", display).stdout(Stdio::piped()).stderr(Stdio::null()).stdin(if input.is_some() { Stdio::piped() } else { Stdio::null() });
    let mut child = command.spawn().with_context(|| format!("starting {tool}"))?;
    if let Some(input) = input {
        use std::io::Write as _;
        child.stdin.take().context("stdin")?.write_all(input)?;
    }
    let out = child.wait_with_output()?;
    Ok(out.stdout)
}

async fn check_paste_image(cx: TestCx) -> Result<()> {
    if std::env::var("WAYLAND_DISPLAY").is_ok_and(|d| !d.is_empty()) || std::env::var("DISPLAY").is_err() {
        cx.log("SKIP: X11 only (run under XWayland)");
        return Ok(());
    }
    if std::env::var("DECIBELL_TEST_WAYLAND_DISPLAY").is_ok_and(|d| d.is_empty()) || Command::new("wl-copy").arg("--version").output().is_err() {
        cx.log("SKIP: no wl-copy / Wayland session to put a picture on the clipboard");
        return Ok(());
    }
    let composer = open(&cx, OFF_TOPIC).await?;
    // What the clipboard holds now, to put back.
    let types = String::from_utf8_lossy(&wl("wl-paste", &["--list-types"], None)?).lines().map(str::to_string).collect::<Vec<_>>();
    let kept = match types.iter().find(|t| t.starts_with("text/plain")).or(types.first()) {
        Some(kind) => Some((kind.clone(), wl("wl-paste", &["--no-newline", "--type", kind], None)?)),
        None => None,
    };
    let dir = TempDir::new("composer-paste")?;
    let png = dir.0.join("pasted.png");
    write_png(&png)?;
    wl("wl-copy", &["--type", "image/png"], Some(&std::fs::read(&png)?))?;
    cx.sleep(Duration::from_millis(300)).await;
    focus_input(&cx, &composer)?;
    cx.keystroke("ctrl-v")?;
    let pasted = wait_for(&cx, Duration::from_secs(10), "a chip from the pasted picture", |cx| {
        model(cx).chat.composer.read(cx).for_channel(daily_gaming(), OFF_TOPIC).count() == 1
    })
    .await;
    // The clipboard as it was, whatever happened.
    match &kept {
        Some((kind, bytes)) => {
            wl("wl-copy", &["--type", kind], Some(bytes))?;
        }
        None => {
            wl("wl-copy", &["--clear"], None)?;
        }
    }
    pasted?;
    let (name, typed) = cx.update(|_, cx| {
        let m = model(cx);
        let store = m.chat.composer.read(cx);
        let p = store.for_channel(daily_gaming(), OFF_TOPIC).next().map(|p| (p.filename.to_string(), p.id.clone()));
        (p, composer.read(cx).input().read(cx).value().to_string())
    })?;
    ensure!(typed.is_empty(), "nothing typed by the paste");
    cx.log(format_args!("pasted picture → chip {:?}; clipboard restored ({})", name.as_ref().map(|n| &n.0), kept.as_ref().map_or("was empty", |(k, _)| k)));
    if let Some((_, id)) = name {
        cx.update(|_, cx| model(cx).chat.composer.update(cx, |c, cx| c.remove(&id, cx)))?;
    }
    Ok(())
}

async fn check_emoji_picker(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let before = settle_window(&cx, Duration::from_millis(300)).await?;
    let button = cx.update(|_, cx| composer.read(cx).emoji_button_bounds())?;
    cx.click(button.center())?;
    wait_for(&cx, Duration::from_secs(5), "the picker", |cx| composer.read(cx).picker().is_some_and(|p| p.read(cx).open_time.is_some())).await?;
    let opened = settle_window(&cx, Duration::from_millis(300)).await?;
    let picker = cx.update(|_, cx| composer.read(cx).picker().cloned())?.context("picker")?;
    let (open_time, usage) = cx.update(|_, cx| (picker.read(cx).open_time, picker.read(cx).image_usage()))?;
    cx.log(format_args!(
        "opened in {:.1} ms ({} frames); {} emoji images, {:.0} KB of pixels",
        open_time.unwrap_or_default().as_secs_f64() * 1000.,
        opened.since(before).drawn,
        usage.0,
        usage.1 as f64 / 1024.
    ));
    // At rest with the picker open: nothing drawn (the search field's caret blinks while the
    // window is active: blur it first).
    cx.update(|window, cx| window.blur(cx))?;
    let rest = settle_window(&cx, Duration::from_millis(500)).await?;
    cx.sleep(Duration::from_secs(2)).await;
    let idle = cx.frames()?.since(rest);
    cx.log(format_args!("at rest with the picker open: {} frames drawn in 2 s", idle.drawn));
    ensure!(idle.drawn == 0, "the open picker draws at rest");
    // Search.
    let search = cx.update(|_, cx| picker.read(cx).search_input().clone())?;
    cx.update(|window, cx| search.update(cx, |s, cx| s.focus(window, cx)))?;
    cx.type_text("smile")?;
    let found = cx.update(|_, cx| picker.read(cx).results().len())?;
    ensure!(found > 0, "\"smile\" finds emoji");
    let picked = cx.update(|window, cx| picker.update(cx, |p, cx| p.pick_shown(0, window, cx)))?.context("a result")?;
    let (value, shown) = cx.update(|_, cx| {
        let input = composer.read(cx).input().read(cx);
        (input.value().to_string(), input.displayed_text())
    })?;
    ensure!(value.contains(picked), "the emoji is in the box: {value:?}");
    ensure!(shown.contains('\u{FFFC}'), "as an atom: {shown:?}");
    cx.log(format_args!("\"smile\": {found} results; picked {picked} (an atom in the box)"));
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(2), "Escape to close the picker", |cx| composer.read(cx).picker().is_none()).await?;
    let text = format!("{picked} {}", tag());
    let id = send(&cx, &composer, OFF_TOPIC, &text).await?;
    cx.log("sent as the character");
    delete(&cx, OFF_TOPIC, &[id]).await
}

async fn check_gif_picker(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let keyed = crate::gifs::key_file_candidates().iter().any(|p| p.is_file()) || std::env::var("GIF_API_KEY").is_ok_and(|k| !k.is_empty());
    let fixture = std::rc::Rc::new({
        let mut f = crate::gifs::FixtureGifs::pages(12, 2, None);
        // A host that never resolves: nothing leaves the machine when the row is drawn.
        for page in &mut f.pages {
            for result in &mut page.results {
                result.url = result.url.replace("static.klipy.com", "gifs.invalid");
            }
        }
        f
    });
    if keyed {
        cx.log("a GIF key is configured, but the self-test uses the canned provider (it posts the URL it picks)");
    }
    cx.update(|_, cx| crate::gifs::GifService::set(cx, fixture.clone()))?;
    let button = cx.update(|_, cx| composer.read(cx).emoji_button_bounds())?;
    cx.click(button.center())?;
    wait_for(&cx, Duration::from_secs(5), "the picker", |cx| composer.read(cx).picker().is_some()).await?;
    let picker = cx.update(|_, cx| composer.read(cx).picker().cloned())?.context("picker")?;
    cx.update(|window, cx| picker.update(cx, |p, cx| p.switch_tab(Tab::Gifs, window, cx)))?;
    wait_for(&cx, Duration::from_secs(5), "the trending page", |cx| !picker.read(cx).gif_results().is_empty()).await?;
    let search = cx.update(|_, cx| picker.read(cx).gif_search_input().cloned())?.context("gif search")?;
    cx.update(|window, cx| search.update(cx, |s, cx| s.focus(window, cx)))?;
    let typed = Instant::now();
    cx.type_text("cat")?;
    wait_for(&cx, Duration::from_secs(5), "the debounced search", |_| fixture.asked.borrow().iter().any(|(q, ..)| q == "cat")).await?;
    let waited = typed.elapsed();
    let asked = fixture.asked.borrow().iter().filter(|(q, ..)| q.starts_with('c')).count();
    ensure!(asked == 1, "one search for the typed word, not one per key ({asked})");
    ensure!(waited >= Duration::from_millis(280), "debounced ({} ms)", waited.as_millis());
    cx.log(format_args!("one request for \"cat\", {} ms after the first key", waited.as_millis()));
    let url = cx.update(|window, cx| picker.update(cx, |p, cx| p.send_gif_shown(0, window, cx)))?.context("a GIF")?;
    wait_for(&cx, Duration::from_secs(2), "picking a GIF to close the picker", |cx| composer.read(cx).picker().is_none()).await?;
    let id = echoed(&cx, OFF_TOPIC, &url).await?;
    cx.log(format_args!("sent {url}"));
    cx.update(|_, cx| model(cx).layout.update(cx, |l, cx| l.set_picker_tab(false, cx)))?;
    delete(&cx, OFF_TOPIC, &[id]).await
}

async fn check_idle(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    focus_input(&cx, &composer)?;
    let active = cx.update(|window, _| window.is_window_active())?;
    let mut blink = None;
    for attempt in 1..=3 {
        let start = settle_window(&cx, Duration::from_millis(300)).await?;
        cx.sleep(Duration::from_secs(5)).await;
        let frames = cx.frames()?.since(start);
        let ok = if cx.update(|window, _| window.is_window_active())? { (5..=12).contains(&frames.drawn) } else { frames.drawn == 0 };
        cx.log(format_args!("focused ({}): {} frames drawn in 5 s", if active { "active window" } else { "inactive window" }, frames.drawn));
        if ok {
            blink = Some(frames.drawn);
            break;
        }
        cx.log(format_args!("try {attempt}: disturbed (another window took the focus?); again"));
    }
    ensure!(blink.is_some(), "focused: only the caret's blink should draw (2 / s in an active window, 0 in an inactive one)");
    // Blurred: nothing.
    let list = list_of(&cx, &composer)?;
    cx.update(|window, cx| list.read(cx).focus_handle().clone().focus(window, cx))?;
    let mut idle = u64::MAX;
    for attempt in 1..=3 {
        let start = settle_window(&cx, Duration::from_millis(300)).await?;
        cx.sleep(Duration::from_secs(5)).await;
        idle = cx.frames()?.since(start).drawn;
        cx.log(format_args!("blurred: {idle} frames drawn in 5 s"));
        if idle == 0 {
            break;
        }
        cx.log(format_args!("try {attempt}: disturbed; again"));
    }
    ensure!(idle == 0, "blurred, the composer draws nothing");
    Ok(())
}

async fn check_typing_cost(cx: TestCx) -> Result<()> {
    let composer = open(&cx, OFF_TOPIC).await?;
    let list = list_of(&cx, &composer)?;
    focus_input(&cx, &composer)?;
    // The first key after pointer input redraws the window once (input modality): one key first.
    cx.type_text("w")?;
    // The caret blinks while the box is focused in the active window: "settled" is a quiet spell
    // shorter than the blink's 500 ms.
    let start = settle_window(&cx, Duration::from_millis(300)).await?;
    let (list_frames, renders) = cx.update(|_, cx| (list.read(cx).stats.frames, composer.read(cx).stats.renders))?;
    let text = "the quick brown fox jumps over the lazy dog";
    let mut dispatch = Vec::new();
    for character in text.chars() {
        let started = Instant::now();
        cx.type_text(&character.to_string())?;
        dispatch.push(started.elapsed().as_micros() as u64);
        cx.sleep(Duration::from_millis(15)).await;
    }
    let end = settle_window(&cx, Duration::from_millis(300)).await?;
    let frames = end.since(start);
    let (list_after, renders_after) = cx.update(|_, cx| (list.read(cx).stats.frames, composer.read(cx).stats.renders))?;
    dispatch.sort_unstable();
    let keys = text.chars().count() as u64;
    let (prepaint_us, paint_us) = cx.update(|_, cx| (list.read(cx).stats.prepaint_us, list.read(cx).stats.paint_us))?;
    cx.log(format_args!("the list's last frame: prepaint {prepaint_us} µs, paint {paint_us} µs"));
    cx.log(format_args!(
        "{keys} keys: {} frames drawn ({:.2} per key); key → edit µs p50 {} / p90 {} / max {}; composer renders {}; list frames {}",
        frames.drawn,
        frames.drawn as f64 / keys as f64,
        dispatch[dispatch.len() / 2],
        dispatch[dispatch.len() * 9 / 10],
        dispatch[dispatch.len() - 1],
        renders_after - renders,
        list_after - list_frames
    ));
    // One frame per key (a caret blink may land in the quiet spell after the last key).
    ensure!(frames.drawn <= keys + 2, "one frame per key ({} for {keys})", frames.drawn);
    ensure!(list_after == list_frames, "the message list drew while typing ({} frames)", list_after - list_frames);
    set_value(&cx, &composer, "")
}
