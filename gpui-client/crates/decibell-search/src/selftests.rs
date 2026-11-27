//! The search self-tests, against the local test stack (`tools/dev/README.md`) as `perf1`.
//!
//! Each `search-*` / `header-*` self-test starts the app again in a child process — `decibell --check
//! <name> --login perf1` with its own temporary profile (`DECIBELL_USER_DATA_DIR`), the stack's
//! `DECIBELL_CENTRAL` and `DECIBELL_TEST_PROBES=1` — as the shell's do. They refuse unless
//! `DECIBELL_CENTRAL` is loopback, skip (saying so) when the stack is down or the binary has no
//! `--login` (`dev-local`), and are **read-only**: they post, edit and delete nothing.
//!
//! The community allows one search at a time per session and refuses one over its bucket (6, then
//! 1 a second) with `retry_after_ms`: the checks pace their searches, and a refusal is the
//! documented path (the store waits it out and asks again), not a failure.

use std::io::BufRead as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result, bail, ensure};
use chrono::Days;
use decibell_harness::{Frames, SelfTest, TestCx};
use decibell_model::chat::Message;
use decibell_model::chat::search::{SearchFilter, SearchQuery, SearchSort, SearchStatus, fold};
use decibell_model::{ChannelId, Model, ServerId, actions};
use decibell_shell::probes;
use gpui::{App, Bounds, Modifiers, Pixels, PlatformInput, ScrollDelta, ScrollEvent, TouchPhase, point, px, size};

const ACCOUNT: &str = "perf1";

fn daily_gaming() -> ServerId {
    ServerId::from_central(1)
}

// ── The parents: one child app each ───────────────────────────────────────────────────────

macro_rules! parent {
    ($name:literal, $about:literal) => {
        SelfTest { name: $name, about: $about, run: |cx| Box::pin(parent(cx, $name)) }
    };
}

pub fn selftests() -> Vec<SelfTest> {
    vec![
        parent!("search-live", "(local stack) words, from:, in:, has:, a date range, inside words and ı / i: counts against the fixture, every result as filtered"),
        parent!("search-supersede", "(local stack) a second search while the first runs: only the second's results show"),
        parent!("search-refused", "(local stack) searches over the community's bucket: the refusal is waited out and the retry answers"),
        parent!("search-paging", "(local stack) scrolling the results pages more in; the rows laid out stay bounded by the window"),
        parent!("search-jump", "(local stack) Jump to a result far back in #general: lands with 0 px later movement, flashes, back to the present"),
        parent!("search-narrow", "(local stack) below 1024 px the results float over the chat; Jump closes them"),
        parent!("search-encrypted", "(local stack) a word search reports #vault (encrypted) as not searched, and the notice says so"),
        parent!("search-nickname", "(local stack) results by a member the roster hasn't loaded show their server nickname (looked up), not their account name"),
        SelfTest {
            name: "search-nothing-stored",
            about: "(local stack) after searching and quitting, nothing about the queries is in the profile",
            run: |cx| Box::pin(nothing_stored(cx)),
        },
        SelfTest {
            name: "header-members-toggle",
            about: "(local stack) the members toggle hides and shows the list, persists across a restart; the box stays centred over the column",
            run: |cx| Box::pin(members_toggle(cx)),
        },
        parent!("search-idle", "(local stack) box focused: only the caret's frames; results at rest: 0 frames; closed: the panel's view is gone"),
        parent!("search-typing-cost", "(local stack) one frame per key typed in the box; the message list doesn't draw"),
    ]
}

/// The stack as `accounts.json` describes it.
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
    ensure!(loopback, "DECIBELL_CENTRAL={central} is not loopback: the search tests run against the local stack only");
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

enum Child {
    Passed,
    NoDevLogin,
}

/// Run `decibell --check <check> --login perf1` and relay its log.
async fn run_child(cx: &TestCx, check: &str, stack: &Stack, profile: &Path, env: &[(&str, String)]) -> Result<Child> {
    let exe = std::env::current_exe()?;
    let mut command = Command::new(exe);
    command
        .arg("--check")
        .arg(check)
        .arg("--login")
        .arg(ACCOUNT)
        .env("DECIBELL_USER_DATA_DIR", profile)
        .env("DECIBELL_CENTRAL", &stack.central)
        .env("DECIBELL_DEV_PASSWORD", &stack.password)
        .env("DECIBELL_TEST_PROBES", "1")
        .env_remove("DECIBELL_PERF")
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (key, value) in env {
        command.env(key, value);
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
        return Ok(Child::NoDevLogin);
    }
    if !status.success() {
        for line in err.iter().rev().take(12).rev() {
            cx.log(format_args!("│ stderr: {line}"));
        }
        bail!("{check} failed in the app ({status}, after {:.1} s)", started.elapsed().as_secs_f64());
    }
    Ok(Child::Passed)
}

async fn parent(cx: TestCx, check: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile::new(check)?;
    match run_child(&cx, check, &stack, &profile.0, &[]).await? {
        Child::Passed => Ok(()),
        Child::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

/// Strings typed or run by `search-nothing-stored`: none may reach the profile.
const SECRETS: [&str; 3] = ["zzqxkw", "unsentqvz", "hayirqvz"];

async fn nothing_stored(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile::new("search-nothing-stored")?;
    if let Child::NoDevLogin = run_child(&cx, "search-nothing-stored", &stack, &profile.0, &[]).await? {
        cx.log("SKIP: this binary has no --login (build with --features dev-local)");
        return Ok(());
    }
    // The app has quit: every file it left, read whole.
    let mut files = Vec::new();
    let mut stack_dirs = vec![profile.0.clone()];
    while let Some(dir) = stack_dirs.pop() {
        for entry in std::fs::read_dir(&dir)? {
            let path = entry?.path();
            if path.is_dir() {
                stack_dirs.push(path);
            } else {
                files.push(path);
            }
        }
    }
    let mut found = Vec::new();
    let mut bytes_read = 0usize;
    for file in &files {
        let Ok(bytes) = std::fs::read(file) else { continue };
        bytes_read += bytes.len();
        for secret in SECRETS {
            if bytes.windows(secret.len()).any(|w| w.eq_ignore_ascii_case(secret.as_bytes())) {
                found.push(format!("{secret} in {}", file.display()));
            }
        }
    }
    cx.log(format_args!("{} files in the profile ({bytes_read} bytes) read: {}", files.len(), files.iter().map(|f| f.strip_prefix(&profile.0).unwrap_or(f).display().to_string()).collect::<Vec<_>>().join(", ")));
    ensure!(found.is_empty(), "a search reached the profile: {found:?}");
    Ok(())
}

async fn members_toggle(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    let profile = Profile::new("header-members-toggle")?;
    for phase in ["1", "2"] {
        cx.log(format_args!("phase {phase}{}", if phase == "2" { ": the same profile again (ui.json kept)" } else { "" }));
        match run_child(&cx, "header-members-toggle", &stack, &profile.0, &[("DECIBELL_CHECK_PHASE", phase.to_string())]).await? {
            Child::Passed => {}
            Child::NoDevLogin => {
                cx.log("SKIP: this binary has no --login (build with --features dev-local)");
                return Ok(());
            }
        }
    }
    Ok(())
}

// ── The checks, inside the signed-in app ────────────────────────────────────────────────────

pub fn checks() -> Vec<SelfTest> {
    macro_rules! check {
        ($name:literal, $f:expr) => {
            SelfTest { name: $name, about: "(in the app)", run: |cx| Box::pin($f(cx)) }
        };
    }
    vec![
        check!("search-live", check_live),
        check!("search-supersede", check_supersede),
        check!("search-refused", check_refused),
        check!("search-paging", check_paging),
        check!("search-jump", check_jump),
        check!("search-narrow", check_narrow),
        check!("search-encrypted", check_encrypted),
        check!("search-nickname", check_nickname),
        check!("search-nothing-stored", check_nothing_stored),
        check!("header-members-toggle", check_members_toggle),
        check!("search-idle", check_idle),
        check!("search-typing-cost", check_typing_cost),
    ]
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
        ensure!(started.elapsed() < limit, "timed out after {} s waiting for {what}", limit.as_secs());
        cx.sleep(Duration::from_millis(30)).await;
    }
}

/// Draws the window's views again, so every probe records where its element is now: a cached view
/// that a layout change moved can replay its paint without running its probes' canvases.
async fn refresh_probes(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| {
        if let Some(b) = crate::shown_box(cx) {
            b.update(cx, |_, cx| cx.notify());
        }
    })?;
    cx.sleep(Duration::from_millis(150)).await;
    Ok(())
}

async fn wait_probe(cx: &TestCx, key: &str) -> Result<Bounds<Pixels>> {
    let owned = key.to_string();
    wait_for(cx, Duration::from_secs(10), &format!("{key} on screen"), move |_| probes::bounds(&owned).is_some()).await?;
    probes::bounds(key).context("probe vanished")
}

/// Signed in, Daily Gaming connected, `#general` open with its newest page.
async fn ready(cx: &TestCx) -> Result<()> {
    let t = wait_for(cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    cx.log(format_args!("signed in as {ACCOUNT} after {} ms", t.as_millis()));
    wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    wait_for(cx, Duration::from_secs(10), "Daily Gaming's channels", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.channels.read(cx).contains("general"))
    })
    .await?;
    cx.update(|_, cx| {
        actions::servers::open_server(daily_gaming(), cx);
        let m = model(cx);
        m.nav.update(cx, |nav, cx| nav.set_active_channel(Some(ChannelId::from("general")), cx));
    })?;
    wait_for(cx, Duration::from_secs(15), "#general's newest page", |cx| {
        model(cx).chat.channels.read(cx).get(daily_gaming(), "general").is_some_and(|s| s.read(cx).fetched)
    })
    .await?;
    wait_probe(cx, "search-box").await?;
    park_pointer(cx)?;
    Ok(())
}

/// The pointer over the chat's middle (no hover on anything that reacts).
fn park_pointer(cx: &TestCx) -> Result<()> {
    let at = cx.update(|window, _| {
        let size = window.viewport_size();
        point(size.width * 0.4, size.height * 0.55)
    })?;
    cx.mouse_move(at)
}

/// Searches go out at most this often (the community's search bucket refills 1 a second).
const PACE: Duration = Duration::from_millis(1250);

struct Searcher {
    last: Option<Instant>,
}

impl Searcher {
    fn new() -> Self {
        Self { last: None }
    }

    async fn pace(&mut self, cx: &TestCx) {
        if let Some(last) = self.last {
            let since = last.elapsed();
            if since < PACE {
                cx.sleep(PACE - since).await;
            }
        }
        self.last = Some(Instant::now());
    }

    /// Clears the box (its text and chips), types `text` as keys, presses Enter, and waits for the
    /// answer (a refusal is waited out by the store). Returns the store's state.
    async fn type_search(&mut self, cx: &TestCx, text: &str) -> Result<Answer> {
        self.pace(cx).await;
        let seq_before = cx.update(|_, cx| model(cx).chat.search.read(cx).seq)?;
        focus_box(cx).await?;
        clear_box(cx).await?;
        cx.type_text(text)?;
        // Enter in an open menu picks its active row (a picker's first match): close the menu
        // first, as a user who wants the search does, then Enter runs it.
        if cx.update(|_, cx| crate::shown_box(cx).is_some_and(|b| b.read(cx).menu_open()))? {
            cx.keystroke("escape")?;
        }
        cx.keystroke("enter")?;
        wait_answer(cx, seq_before).await
    }

    /// Runs `query` through the store (what the box's Enter does), and waits for the answer.
    async fn run(&mut self, cx: &TestCx, query: SearchQuery) -> Result<Answer> {
        self.pace(cx).await;
        let seq_before = cx.update(|_, cx| model(cx).chat.search.read(cx).seq)?;
        cx.update(|_, cx| {
            let m = model(cx);
            m.chat.search.update(cx, |s, cx| s.run(daily_gaming(), &query, cx));
        })?;
        wait_answer(cx, seq_before).await
    }
}

/// What a finished search shows.
struct Answer {
    query: SearchQuery,
    total: i64,
    capped: bool,
    results: Vec<std::sync::Arc<Message>>,
    has_more: bool,
    skipped: Vec<ChannelId>,
    refused_then_retried: bool,
}

async fn wait_answer(cx: &TestCx, seq_before: u32) -> Result<Answer> {
    let started = Instant::now();
    let mut retried = false;
    loop {
        let (seq, status, retry) = cx.update(|_, cx| {
            let s = model(cx).chat.search.read(cx);
            (s.seq, s.status, s.waiting().1)
        })?;
        retried |= retry;
        if seq > seq_before && matches!(status, SearchStatus::Done | SearchStatus::Error) {
            break;
        }
        if started.elapsed() > Duration::from_secs(20) {
            let state = cx.update(|_, cx| {
                let s = model(cx).chat.search.read(cx);
                let draft = crate::shown_box(cx).map(|b| format!("{:?} menu {}", b.read(cx).draft(), b.read(cx).menu_open()));
                format!("seq {} (before {seq_before}), status {:?}, query {:?}, open {}, box {draft:?}", s.seq, s.status, s.query, s.open)
            })?;
            bail!("no answer to the search after 20 s: {state}");
        }
        cx.sleep(Duration::from_millis(20)).await;
    }
    cx.update(|_, cx| {
        let s = model(cx).chat.search.read(cx);
        if s.status == SearchStatus::Error {
            bail!("the search failed: {}", s.error.as_deref().unwrap_or("?"));
        }
        Ok(Answer {
            query: s.query.clone(),
            total: s.total,
            capped: s.total_capped,
            results: s.results.clone(),
            has_more: s.has_more,
            skipped: s.text_skipped.clone(),
            refused_then_retried: retried,
        })
    })?
}

/// The middle of a result's card on screen, from the list's own geometry (its viewport, the row
/// offsets and the scroll position as last drawn). (The cards' probes can be stale: a cached view
/// moved by a layout change above it replays without running their canvases again.)
async fn card_center(cx: &TestCx, id: i64) -> Result<gpui::Point<Pixels>> {
    wait_probe(cx, &format!("search-result:{id}")).await?;
    // The list's first frames: the scrollbar's width decides the rows' heights in the second.
    cx.sleep(Duration::from_millis(400)).await;
    cx.update(|_, cx| {
        let view = crate::shown_results(cx).context("no results")?;
        let view = view.read(cx);
        let (_, top, height) = view.row_offsets().into_iter().find(|(row, _, _)| *row == id).context("the result isn't in the list")?;
        let viewport = view.scroll_handle().viewport_bounds();
        let y = viewport.top() + px(top) - view.scroll_handle().offset() + px((height - crate::result_row::CARD_GAP) / 2.);
        anyhow::ensure!(y > viewport.top() && y < viewport.bottom(), "the card isn't in view");
        Ok(point(viewport.center().x, y))
    })?
}

/// Ctrl+F (the box takes the focus, its text selected).
async fn focus_box(cx: &TestCx) -> Result<()> {
    cx.keystroke("ctrl-f")?;
    // GPUI's focus, not the box's own `focused` (its focus event waits for the window to be
    // active: other windows on a shared desktop take it).
    let started = Instant::now();
    loop {
        let focused = cx.update(|window, cx| {
            crate::shown_box(cx).is_some_and(|b| gpui::Focusable::focus_handle(b.read(cx).input().read(cx), cx).is_focused(window))
        })?;
        if focused {
            return Ok(());
        }
        ensure!(started.elapsed() < Duration::from_secs(5), "Ctrl+F didn't focus the box");
        cx.sleep(Duration::from_millis(30)).await;
    }
}

/// The box emptied with keys: its text away (it is selected after Ctrl+F), then a Backspace per
/// chip at the start of the field.
async fn clear_box(cx: &TestCx) -> Result<()> {
    let (text, chips) = cx.update(|_, cx| crate::shown_box(cx).map(|b| (b.read(cx).draft().text.clone(), b.read(cx).draft().filters.len())))?.context("no box")?;
    if !text.is_empty() {
        cx.keystroke("backspace")?;
    }
    for _ in 0..chips {
        cx.keystroke("backspace")?;
    }
    let empty = cx.update(|_, cx| crate::shown_box(cx).is_some_and(|b| b.read(cx).draft() == &SearchQuery::default()))?;
    ensure!(empty, "Backspace didn't empty the box");
    Ok(())
}

/// Messages of the 10,000-message fixture (`tools/dev/fixture.py`, the stack's #general) holding
/// `word` as the server's index matches it: a whole word, or (3+ letters) a word starting with it.
fn fixture_count(word: &str) -> usize {
    let word = fold(word);
    let messages = decibell_chat::fixture::generate(decibell_chat::fixture::COUNT);
    messages
        .iter()
        .filter(|m| {
            fold(&m.content)
                .split(|c: char| !c.is_alphanumeric())
                .any(|w| if word.chars().count() >= 3 { w.starts_with(word.as_str()) } else { w == word })
        })
        .count()
}

/// A stack count against the fixture's: the seed deleted one fixture message (a moderator's
/// delete) and posted five live ones.
fn near(total: i64, fixture: usize) -> bool {
    let total = total.max(0) as usize;
    total + 1 >= fixture && total <= fixture + 5
}

fn general() -> SearchFilter {
    SearchFilter::In { channel: ChannelId::from("general") }
}

async fn check_live(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let mut s = Searcher::new();

    // A word, typed: the count against the fixture (+1: the stack's live "…stable on wayland…").
    let a = s.type_search(&cx, "wayland in:general").await?;
    let expected = fixture_count("wayland");
    cx.log(format_args!("\"wayland in:general\": {} results{} (fixture {expected} + the live one); first page {}", a.total, if a.capped { "+" } else { "" }, a.results.len()));
    ensure!(a.query.filters == vec![general()], "in:general didn't become a chip: {:?}", a.query.filters);
    // The stack's #general is the fixture, less the message its seed deleted as a moderator, plus
    // its live messages: within a few of the fixture's count.
    ensure!(near(a.total, expected), "count {} against the fixture's {expected}", a.total);
    let words = decibell_model::chat::search::search_words("wayland");
    let h = crate::highlight::Highlighter::words(&words).context("a highlighter")?;
    for m in &a.results {
        ensure!(&*m.channel == "general", "a result from #{}", m.channel);
        ensure!(!h.marks(&m.content).is_empty(), "result {} has nothing to mark: {:?}", m.id, m.content);
    }
    let grouped = decibell_model::regional::Regional::get().group(a.total as u64);
    wait_for(&cx, Duration::from_secs(5), "the panel's title", move |_| {
        probes::text_of("search-title").is_some_and(|t| t.starts_with(&grouped) && t.ends_with("results"))
    })
    .await
    .ok();
    let title = probes::text_of("search-title").map(|t| t.to_string()).unwrap_or_default();
    cx.log(format_args!("the panel's title: {title:?}"));
    ensure!(title.ends_with("results") && title.starts_with(&decibell_model::regional::Regional::get().group(a.total as u64)), "title {title:?}");
    // The first card is drawn, marked words and all.
    wait_probe(&cx, &format!("search-result:{}", a.results[0].id)).await?;

    // from: narrows to an author by uid (typed: "from:alice " makes the chip).
    let f = s.type_search(&cx, "wayland in:general from:alice ").await?;
    cx.log(format_args!("+ from:alice: {} results", f.total));
    ensure!(f.query.filters.iter().any(|x| matches!(x, SearchFilter::From { uid: 1, .. })), "from:alice isn't a chip for uid 1: {:?}", f.query.filters);
    ensure!(f.total > 0 && f.total < a.total, "from:alice didn't narrow ({} of {})", f.total, a.total);
    ensure!(f.results.iter().all(|m| m.sender_uid == 1), "a result not by alice");

    // in: another channel.
    let o = s.type_search(&cx, "in:off-topic the").await?;
    cx.log(format_args!("\"the\" in #off-topic: {} results", o.total));
    ensure!(o.total > 0 && o.results.iter().all(|m| &*m.channel == "off-topic"), "in:off-topic let other channels through");

    // has: a kind, no words.
    let h_img = s.type_search(&cx, "has:image in:general").await?;
    cx.log(format_args!("has:image in #general: {} results", h_img.total));
    ensure!(h_img.total > 0, "no picture in #general");
    ensure!(
        h_img.results.iter().all(|m| m.attachments.iter().any(|a| a.kind == decibell_attachments::AttachmentKind::Image)),
        "a has:image result without a picture"
    );

    // A date range around a result of the first search.
    let pivot = a.results[a.results.len() / 2].timestamp.context("a time")?;
    let day = decibell_model::regional::local(pivot).context("a local day")?.date;
    let day = chrono::NaiveDate::from_ymd_opt(day.year, day.month as u32, day.day as u32).context("a date")?;
    let (after, before) = (day - Days::new(2), day + Days::new(1));
    let text = format!("wayland in:general after:{} before:{}", after.format("%Y-%m-%d"), before.format("%Y-%m-%d"));
    let d = s.type_search(&cx, &text).await?;
    let (lo, hi) = decibell_model::chat::search::date_bounds(&d.query.filters);
    cx.log(format_args!("{text:?}: {} results between {lo} and {hi}", d.total));
    ensure!(d.query.filters.iter().filter(|f| matches!(f, SearchFilter::After(_) | SearchFilter::Before(_))).count() == 2, "the dates aren't chips");
    ensure!(d.total > 0 && d.total < a.total, "the range didn't narrow ({} of {})", d.total, a.total);
    ensure!(d.results.iter().all(|m| m.timestamp.is_some_and(|t| t >= lo && t < hi)), "a result outside the range");

    // Inside words: "ayla" isn't a word (nor a word's start), but it is inside "wayland". The box's
    // last row toggles the option (↑ from no row, Enter), Esc closes the menu, Enter runs.
    let w = s.type_search(&cx, "ayla in:general").await?;
    cx.log(format_args!("\"ayla\" as words: {} results", w.total));
    ensure!(w.total == 0, "\"ayla\" matched {} whole words", w.total);
    s.pace(&cx).await;
    let seq = cx.update(|_, cx| model(cx).chat.search.read(cx).seq)?;
    focus_box(&cx).await?;
    cx.keystroke("end")?;
    // The menu opens on focus; in a window another one took the focus from, ↓ opens it.
    if !cx.update(|_, cx| crate::shown_box(cx).is_some_and(|b| b.read(cx).menu_open()))? {
        cx.keystroke("down")?;
    }
    cx.keystroke("up")?;
    cx.keystroke("enter")?;
    let inside_on = cx.update(|_, cx| crate::shown_box(cx).is_some_and(|b| b.read(cx).draft().matches_inside()))?;
    ensure!(inside_on, "the Inside words row didn't turn the option on");
    cx.keystroke("escape")?;
    cx.keystroke("enter")?;
    let i = wait_answer(&cx, seq).await?;
    cx.log(format_args!("\"ayla\" inside words: {} results", i.total));
    ensure!(i.query.matches_inside(), "the search didn't carry the option");
    ensure!(near(i.total, expected), "inside words found {} (\"wayland\": {expected})", i.total);
    ensure!(i.results.iter().all(|m| fold(&m.content).contains("ayla")), "an inside-words result without \"ayla\"");

    // ı and i match each other: "hayir" finds "hayır".
    let t = s.type_search(&cx, "hayir in:general").await?;
    let expected = fixture_count("hayır");
    cx.log(format_args!("\"hayir\": {} results (the fixture's \"hayır\": {expected})", t.total));
    ensure!(near(t.total, expected), "\"hayir\" found {} of the fixture's {expected} \"hayır\"", t.total);
    ensure!(t.results.iter().all(|m| m.content.to_lowercase().contains("hayır")), "a result without \"hayır\"");
    let h = crate::highlight::Highlighter::words(&decibell_model::chat::search::search_words("hayir")).context("a highlighter")?;
    ensure!(t.results.iter().all(|m| !h.marks(&m.content).is_empty()), "\"hayır\" isn't marked for \"hayir\"");
    Ok(())
}

/// Members of Daily Gaming with a server nickname (`tools/dev/seed.py`), none of them a real account
/// that signs in: offline, so off the roster's first pages until something looks them up.
const NICKNAMED: [(i64, &str, &str); 4] = [
    (100014, "zeynep_k", "Zeynep ✨"),
    (100015, "mert.the.long.name.example", "Mert (AFK)"),
    (100013, "deniz", "Deniz | mod"),
    (100002, "battalocelot", "Ocelot"),
];

/// G5 (parity audit): a result card names its sender as `useDisplayName` does, looking up a member
/// the roster hasn't loaded, so an offline member's nickname shows rather than their account name.
async fn check_nickname(cx: TestCx) -> Result<()> {
    // Not `ready`: #general's newest rows would look their authors up (the nicknamed members
    // among them). #announcements has none of them.
    wait_for(&cx, Duration::from_secs(30), "the sign-in", |cx| model(cx).auth.read(cx).is_authenticated).await?;
    wait_for(&cx, Duration::from_secs(30), "Daily Gaming to connect", |cx| model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    wait_for(&cx, Duration::from_secs(10), "Daily Gaming's channels", |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.channels.read(cx).contains("announcements"))
    })
    .await?;
    cx.update(|_, cx| {
        let m = model(cx);
        m.nav.update(cx, |nav, cx| nav.set_active_channel(Some(ChannelId::from("announcements")), cx));
        actions::servers::open_server(daily_gaming(), cx);
        m.nav.update(cx, |nav, cx| nav.set_active_channel(Some(ChannelId::from("announcements")), cx));
    })?;
    wait_for(&cx, Duration::from_secs(15), "#announcements' newest page", |cx| {
        model(cx).chat.channels.read(cx).get(daily_gaming(), "announcements").is_some_and(|s| s.read(cx).fetched)
    })
    .await?;
    wait_probe(&cx, "search-box").await?;
    park_pointer(&cx)?;
    let server = daily_gaming();
    // The roster's first page is in (its generation is what a lookup is asked under).
    wait_for(&cx, Duration::from_secs(15), "Daily Gaming's roster", move |cx| {
        model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).meta().is_some_and(|m| !m.refetching))
    })
    .await?;
    // The roster's first page holds 500 members and every community of the stack fits in it: one
    // of the nicknamed members is dropped from the loaded roster to stand for a member past it.
    let (uid, account, nickname) = NICKNAMED[0];
    cx.update(move |_, cx| {
        if let Some(c) = model(cx).community(server, cx) {
            c.roster.update(cx, |r, cx| r.forget(uid, cx));
        }
    })?;
    cx.sleep(Duration::from_millis(200)).await;
    let loaded = cx.update(move |_, cx| model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).member(uid).is_some()))?;
    ensure!(!loaded, "{account} is still on the roster (something drawn looked them up again)");
    cx.log(format_args!("{account} (uid {uid}, nickname {nickname:?}) dropped from the loaded roster (a member past its first page)"));
    let mut s = Searcher::new();
    let query = SearchQuery { text: "".into(), filters: vec![SearchFilter::From { uid, username: account.into() }] };
    let a = s.run(&cx, query).await?;
    cx.log(format_args!("from:{account}: {} results", a.total));
    ensure!(!a.results.is_empty(), "no message by {account} in Daily Gaming");
    ensure!(a.results.iter().all(|m| m.sender_uid == uid), "a result by someone else");
    let first = a.results[0].id;
    wait_probe(&cx, &format!("search-result:{first}")).await?;
    let key = format!("search-result-author:{first}");
    let shown = wait_for(&cx, Duration::from_secs(10), "the card to show the nickname", move |_| {
        probes::text_of(&key).is_some_and(|t| t == nickname)
    })
    .await;
    let drawn = probes::text_of(&format!("search-result-author:{first}")).map(|t| t.to_string()).unwrap_or_default();
    let loaded = cx.update(|_, cx| model(cx).community(server, cx).is_some_and(|c| c.roster.read(cx).member(uid).is_some()))?;
    cx.log(format_args!("the first card names its sender {drawn:?}; the member looked up: {loaded}"));
    shown.map_err(|e| anyhow::anyhow!("{e}: the card shows {drawn:?}"))?;
    ensure!(loaded, "the member was never looked up");
    Ok(())
}

async fn check_supersede(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let mut s = Searcher::new();
    s.pace(&cx).await;
    let seq = cx.update(|_, cx| model(cx).chat.search.read(cx).seq)?;
    // Two searches in one turn: the first is on the wire when the second supersedes it.
    cx.update(|_, cx| {
        let m = model(cx);
        let first = SearchQuery { text: "selam".into(), filters: vec![general()] };
        let second = SearchQuery { text: "naber".into(), filters: vec![general()] };
        m.chat.search.update(cx, |st, cx| st.run(daily_gaming(), &first, cx));
        m.chat.search.update(cx, |st, cx| st.run(daily_gaming(), &second, cx));
    })?;
    let a = wait_answer(&cx, seq + 1).await?;
    cx.sleep(Duration::from_millis(800)).await;
    let (query, results) = cx.update(|_, cx| {
        let st = model(cx).chat.search.read(cx);
        (st.query.text.clone(), st.results.clone())
    })?;
    cx.log(format_args!("\"naber\" after \"selam\": {} results shown for {query:?}", a.total));
    ensure!(&*query == "naber", "the store shows {query:?}");
    ensure!(!results.is_empty(), "nothing shown");
    let h = crate::highlight::Highlighter::words(&decibell_model::chat::search::search_words("naber")).context("a highlighter")?;
    let stray = results.iter().filter(|m| h.marks(&m.content).is_empty()).count();
    ensure!(stray == 0, "{stray} results are the first search's");
    Ok(())
}

async fn check_refused(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    // The bucket holds 6 and refills 1 a second: 8 searches in a burst run it dry. Each
    // supersedes the one before; the last is refused and waited out.
    cx.sleep(Duration::from_secs(2)).await;
    let seq = cx.update(|_, cx| model(cx).chat.search.read(cx).seq)?;
    let words = ["the", "stream", "tonight", "build", "server", "patch", "voice", "channel"];
    for word in words {
        cx.update(|_, cx| {
            let m = model(cx);
            let q = SearchQuery { text: word.into(), filters: vec![general()] };
            m.chat.search.update(cx, |st, cx| st.run(daily_gaming(), &q, cx));
        })?;
        cx.sleep(Duration::from_millis(15)).await;
    }
    let started = Instant::now();
    let a = wait_answer(&cx, seq + words.len() as u32 - 1).await?;
    cx.log(format_args!(
        "8 searches in a burst: the last ({:?}) {} and answered {} results after {} ms",
        a.query.text,
        if a.refused_then_retried { "was refused, waited out," } else { "went through" },
        a.total,
        started.elapsed().as_millis()
    ));
    ensure!(a.refused_then_retried, "no refusal: the community didn't refuse a search over its bucket");
    ensure!(&*a.query.text == "channel" && a.total > 0, "the retried search didn't answer");
    Ok(())
}

async fn check_paging(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let mut s = Searcher::new();
    let a = s.run(&cx, SearchQuery { text: "the".into(), filters: vec![general()] }).await?;
    cx.log(format_args!("\"the\": {} results, first page {}, more: {}", a.total, a.results.len(), a.has_more));
    ensure!(a.has_more, "one page only");
    let list = wait_probe(&cx, "search-results").await?;
    let started = Instant::now();
    let mut peak_built = 0usize;
    let mut scrolls = 0;
    while cx.update(|_, cx| model(cx).chat.search.read(cx).results.len())? < 125 {
        if started.elapsed() >= Duration::from_secs(60) {
            // Where it stopped: the store's paging state, the list's rows, the window.
            let state = cx.update(|window, cx| {
                let st = model(cx).chat.search.read(cx);
                format!(
                    "{} results, has more {}, loading more {}, status {:?}, error {:?}, next cursor {} / offset {}; {} rows laid out; {scrolls} scrolls; window active {}",
                    st.results.len(),
                    st.has_more,
                    st.loading_more,
                    st.status,
                    st.error,
                    st.next_cursor,
                    st.next_offset,
                    crate::shown_results(cx).map_or(0, |r| r.read(cx).built_rows()),
                    window.is_window_active()
                )
            })?;
            bail!("paging stopped ({state})");
        }
        cx.update(|window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(ScrollEvent {
                    position: point(list.center().x, list.bottom() - px(80.)),
                    delta: ScrollDelta::Pixels(point(px(0.), px(-900.))),
                    modifiers: Modifiers::default(),
                    touch_phase: TouchPhase::Moved,
                    source: Default::default(),
                }),
                cx,
            )
        })?;
        scrolls += 1;
        cx.sleep(Duration::from_millis(250)).await;
        let built = cx.update(|_, cx| crate::shown_results(cx).map_or(0, |r| r.read(cx).built_rows()))?;
        peak_built = peak_built.max(built);
    }
    let (results, measured, built) = cx.update(|_, cx| {
        let r = crate::shown_results(cx).expect("the panel");
        (model(cx).chat.search.read(cx).results.len(), r.read(cx).measured(), r.read(cx).built_rows())
    })?;
    cx.log(format_args!(
        "{scrolls} scrolls: {results} results held ({measured} heights known), {built} rows laid out now, at most {peak_built} at once"
    ));
    ensure!(results >= 125, "pages didn't load");
    // The panel's height holds about 6 cards; the window is the viewport and half a viewport
    // either side.
    ensure!(peak_built <= 24, "{peak_built} rows laid out at once: the list isn't windowed");
    Ok(())
}

async fn check_jump(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let mut s = Searcher::new();
    cx.update(|_, cx| model(cx).chat.search.update(cx, |st, _| st.sort = SearchSort::Oldest))?;
    let a = s.run(&cx, SearchQuery { text: "wayland".into(), filters: vec![general()] }).await?;
    let target = a.results.first().context("no result")?.clone();
    let target_id = target.id;
    let newest = cx.update(|_, cx| model(cx).chat.channels.read(cx).get(daily_gaming(), "general").map_or(0, |s| s.read(cx).newest_stored_id()))?;
    cx.log(format_args!("oldest \"wayland\": message {} (the newest in #general: {newest})", target.id));
    let card = card_center(&cx, target.id).await?;
    cx.click(card)?;
    park_pointer(&cx)?;
    let landed = wait_for(&cx, Duration::from_secs(10), "the list to land on the result", move |cx| {
        decibell_chat::shown_list(cx).is_some_and(|l| l.read(cx).highlighted() == Some(target_id))
    })
    .await;
    let t = match landed {
        Ok(t) => t,
        Err(e) => {
            let state = cx.update(|_, cx| {
                let jumped = model(cx).chat.search.read(cx).jumped_id;
                let list = decibell_chat::shown_list(cx).map(|l| {
                    let l = l.read(cx);
                    let ids = l.row_ids();
                    (l.jump_pending(), l.highlighted(), ids.first().copied(), ids.last().copied())
                });
                let request = model(cx).chat.channels.read(cx).jump_request().map(|r| r.message_id);
                format!("jumped_id {jumped}, list (pending, flash, first, last) {list:?}, request {request:?}, nav {:?}", model(cx).nav.read(cx).active_channel)
            })?;
            bail!("{e}: {state}");
        }
    };
    let jumped = cx.update(|_, cx| model(cx).chat.search.read(cx).jumped_id)?;
    ensure!(jumped == target.id, "the result isn't marked as jumped to");
    let y0 = cx.update(|_, cx| decibell_chat::shown_list(cx).and_then(|l| l.read(cx).row_screen_y(target.id)))?.context("the row isn't laid out")?;
    // Rows measured around it after the landing must not move it.
    let mut samples = Vec::new();
    for _ in 0..12 {
        cx.sleep(Duration::from_millis(150)).await;
        let y = cx.update(|_, cx| decibell_chat::shown_list(cx).and_then(|l| l.read(cx).row_screen_y(target.id)))?.context("the row went")?;
        samples.push(y);
    }
    let moved = samples.iter().map(|y| (y - y0).abs()).fold(0f32, f32::max);
    cx.log(format_args!("landed after {} ms at y {y0}; over 1.8 s it moved {moved} px", t.as_millis()));
    ensure!(moved < 0.5, "the row moved {moved} px after the landing");
    // The panel stays open above 1024 px.
    ensure!(cx.update(|_, cx| model(cx).chat.search.read(cx).open)?, "the panel closed on a wide window");
    // Back to the present.
    cx.update(|_, cx| {
        if let Some(list) = decibell_chat::shown_list(cx) {
            list.update(cx, |l, cx| l.jump_to_present(cx));
        }
    })?;
    wait_for(&cx, Duration::from_secs(15), "the present", |cx| {
        decibell_chat::shown_list(cx).is_some_and(|l| l.read(cx).at_bottom())
            && model(cx).chat.channels.read(cx).get(daily_gaming(), "general").is_some_and(|s| !s.read(cx).list_windowed())
    })
    .await?;
    cx.log("Jump to present: at the bottom, the slice contiguous again");
    Ok(())
}

async fn check_narrow(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let original = cx.update(|window, _| window.bounds().size)?;
    cx.update(|window, _| window.resize(decibell_ui::outer_window_size(size(px(900.), px(700.)))))?;
    wait_for(&cx, Duration::from_secs(5), "the narrow window", |_| true).await?;
    let result = async {
        cx.sleep(Duration::from_millis(500)).await;
        let narrow = cx.update(|window, cx| crate::is_narrow(window, cx))?;
        ensure!(narrow, "the window didn't get narrower than 1024 px");
        let mut s = Searcher::new();
        let a = s.run(&cx, SearchQuery { text: "wayland".into(), filters: vec![general()] }).await?;
        let panel = wait_probe(&cx, "search-results").await?;
        let box_ = wait_probe(&cx, "search-box").await?;
        cx.log(format_args!("results float: the panel {:.0} px wide at x {:.0}..{:.0} (box ends at {:.0})", f32::from(panel.size.width), f32::from(panel.left()), f32::from(panel.right()), f32::from(box_.right())));
        ensure!((f32::from(panel.size.width) - crate::FLOATING_WIDTH).abs() <= 1., "the floating panel is {} px wide", f32::from(panel.size.width));
        let card = card_center(&cx, a.results[0].id).await?;
        cx.click(card)?;
        park_pointer(&cx)?;
        wait_for(&cx, Duration::from_secs(5), "Jump to close the panel", |cx| !model(cx).chat.search.read(cx).open).await?;
        cx.log("Jump closed the floating panel");
        Ok(())
    }
    .await;
    cx.update(|window, _| window.resize(original))?;
    result
}

async fn check_encrypted(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let readable = cx.update(|_, cx| {
        let m = model(cx);
        m.community(daily_gaming(), cx).and_then(|c| c.channels.read(cx).get("vault").map(|v| (v.encrypted, crate::tokens::is_searchable_channel(v, false))))
    })?;
    let Some((true, true)) = readable else {
        bail!("#vault isn't an encrypted channel perf1 can read on this stack ({readable:?})");
    };
    let mut s = Searcher::new();
    let a = s.type_search(&cx, "wayland").await?;
    cx.log(format_args!("\"wayland\" over every channel: {} results; text not searched in {:?}", a.total, a.skipped));
    ensure!(a.skipped.iter().any(|c| &**c == "vault"), "#vault isn't reported");
    wait_for(&cx, Duration::from_secs(5), "the notice", |_| probes::text_of("search-skipped").is_some_and(|t| t.split(',').any(|n| n == "vault"))).await?;
    // Filters alone search it: no words, no skip.
    let f = s.run(&cx, SearchQuery { text: "".into(), filters: vec![SearchFilter::In { channel: "vault".into() }] }).await?;
    cx.log(format_args!("in:vault alone: {} results, skipped {:?}", f.total, f.skipped));
    ensure!(f.skipped.is_empty(), "a filter-only search skipped #vault");
    Ok(())
}

async fn check_nothing_stored(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let mut s = Searcher::new();
    // A run search, a typed draft never run, a from: lookup: none may be written anywhere.
    s.type_search(&cx, SECRETS[0]).await.ok();
    s.type_search(&cx, &format!("{} in:general", SECRETS[2])).await.ok();
    focus_box(&cx).await?;
    clear_box(&cx).await?;
    cx.type_text(&format!("{} from:{}", SECRETS[1], SECRETS[1]))?;
    cx.sleep(Duration::from_millis(600)).await;
    // Something the app does save, so the profile's files are fresh: the members toggle (ui.json).
    cx.update(|_, cx| model(cx).layout.update(cx, |l, cx| l.toggle_members_panel(cx)))?;
    cx.update(|_, cx| model(cx).layout.update(cx, |l, cx| l.toggle_members_panel(cx)))?;
    cx.sleep(Duration::from_millis(800)).await;
    cx.log("searched, typed a draft and a from: lookup; quitting");
    Ok(())
}

async fn check_members_toggle(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let phase = std::env::var("DECIBELL_CHECK_PHASE").unwrap_or_else(|_| "1".into());
    let shown = cx.update(|_, cx| model(cx).layout.read(cx).prefs().members_panel)?;
    // The members column: the workspace's last 260 px (inside its 8 px gutter and 1 px border).
    let centred = |cx: &TestCx| -> Result<(f32, f32)> {
        // The probe fills the box inside its 1 px border.
        let box_ = probes::bounds("search-box").context("no box")?.dilate(px(1.));
        let right = cx.update(|window, cx| decibell_ui::popover::visible_window_bounds(window, cx).right() - px(8. + 1.))?;
        let left = right - px(decibell_shell::MEMBERS_WIDTH);
        Ok((f32::from(box_.left() - left), f32::from(right - box_.right())))
    };
    // The toggle: 32 px, `ml-4` before the box (the probe is inside the box's 1 px border).
    let toggle_at = |box_: Bounds<Pixels>| point(box_.left() - px(1. + 16. + 16.), box_.center().y);
    refresh_probes(&cx).await?;
    if phase == "1" {
        ensure!(shown, "a fresh profile hides the members");
        wait_probe(&cx, "member-list").await?;
        let (left, right) = centred(&cx)?;
        cx.log(format_args!("the box sits {left} px in from the members column's left and {right} px from its right"));
        ensure!((left - 8.).abs() < 0.5 && (right - 8.).abs() < 0.5, "the box isn't centred over the members column ({left} / {right})");
        let box_before = wait_probe(&cx, "search-box").await?;
        cx.click(toggle_at(box_before))?;
        park_pointer(&cx)?;
        wait_for(&cx, Duration::from_secs(5), "the members to hide", |cx| !model(cx).layout.read(cx).prefs().members_panel).await?;
        cx.settle().await.ok();
        refresh_probes(&cx).await?;
        let box_after = wait_probe(&cx, "search-box").await?;
        ensure!(box_after == box_before, "the box moved when the members hid ({box_before:?} → {box_after:?})");
        // ui.json is written 250 ms after a change (and on quit).
        cx.sleep(Duration::from_millis(600)).await;
        cx.log("hidden by the toggle; the box stayed where it was");
    } else {
        ensure!(!shown, "the hidden members came back after a restart (ui.json)");
        cx.log("still hidden after the restart");
        let box_ = wait_probe(&cx, "search-box").await?;
        cx.click(toggle_at(box_))?;
        park_pointer(&cx)?;
        wait_for(&cx, Duration::from_secs(5), "the members to show", |cx| model(cx).layout.read(cx).prefs().members_panel).await?;
        wait_probe(&cx, "member-list").await?;
        cx.settle().await.ok();
        refresh_probes(&cx).await?;
        let (left, right) = centred(&cx)?;
        ensure!((left - 8.).abs() < 0.5 && (right - 8.).abs() < 0.5, "the box isn't centred over the members column ({left} / {right})");
        cx.sleep(Duration::from_millis(600)).await;
        cx.log("shown again by the toggle, the box centred over the column");
    }
    Ok(())
}

/// Frames over `seconds` once drawing stopped; a try disturbed by focus changes, the pointer or
/// core events (a shared stack) is repeated, up to three times.
/// Frames over `seconds` at rest, and whether that try was undisturbed (no core event, no focus
/// change): on a shared stack other clients' presence and roster changes redraw what they change.
async fn quiet(cx: &TestCx, seconds: u64) -> Result<(Frames, bool)> {
    let mut least: Option<Frames> = None;
    for attempt in 1..=3 {
        let before = cx.settle().await.unwrap_or(cx.frames()?);
        let events = cx.update(|_, cx| model(cx).pump_stats().events)?;
        let active = cx.update(|window, _| window.is_window_active())?;
        cx.sleep(Duration::from_secs(seconds)).await;
        let frames = cx.frames()?.since(before);
        let disturbed = cx.update(|_, cx| model(cx).pump_stats().events)? != events || cx.update(|window, _| window.is_window_active())? != active;
        if least.is_none_or(|l| frames.drawn < l.drawn) {
            least = Some(frames);
        }
        if !disturbed {
            return Ok((frames, true));
        }
        let panel = cx.update(|_, cx| crate::shown_results(cx).map_or(0, |r| r.read(cx).renders()))?;
        cx.log(format_args!("try {attempt} disturbed ({} drawn; the panel has rendered {panel} times): again", frames.drawn));
    }
    // Every try disturbed (a shared stack, other windows): the quietest one.
    Ok((least.expect("three tries"), false))
}

async fn check_idle(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    // The box focused: the caret blinks (2 frames a second while the window is active), nothing else.
    focus_box(&cx).await?;
    let active = cx.update(|window, _| window.is_window_active())?;
    let (f, clean) = quiet(&cx, 3).await?;
    cx.log(format_args!("box focused (window active: {active}): {} drawn / {} presented in 3 s", f.drawn, f.presented));
    let mut unmeasured = Vec::new();
    if clean {
        // The caret: 2 frames a second while the window is active (it may turn active meanwhile).
        ensure!(f.drawn <= 8, "more than the caret's frames");
    } else {
        cx.log("the box focused: every try had core events or focus changes (a busy shared stack)");
        unmeasured.push("the focused box");
    }
    // Results open, the box blurred: nothing.
    let mut s = Searcher::new();
    s.run(&cx, SearchQuery { text: "wayland".into(), filters: vec![general()] }).await?;
    cx.update(|window, cx| window.blur(cx))?;
    park_pointer(&cx)?;
    // The cards' pictures are fetched once they are drawn (the avatar cache): let them land.
    cx.sleep(Duration::from_secs(2)).await;
    let renders = || -> Result<u64> { cx.update(|_, cx| crate::shown_results(cx).map_or(0, |r| r.read(cx).renders())) };
    let before = renders()?;
    let (f, clean) = quiet(&cx, 5).await?;
    let panel = renders()? - before;
    cx.log(format_args!("results open at rest: {} drawn / {} presented in 5 s; the panel rendered {panel} times", f.drawn, f.presented));
    if clean {
        ensure!(f.drawn == 0, "the open results drew {} frames at rest", f.drawn);
    } else {
        cx.log("the open results: every try had core events or focus changes (a busy shared stack)");
        unmeasured.push("the open results");
    }
    // Closed: the panel's view is gone (no state, no rows).
    let weak = cx.update(|_, cx| crate::shown_results(cx).map(|r| r.downgrade()))?.context("no results view")?;
    cx.update(|_, cx| model(cx).chat.search.update(cx, |st, cx| st.close(cx)))?;
    cx.settle().await?;
    let alive = cx.update(|_, _| weak.upgrade().is_some())?;
    ensure!(!alive, "the closed panel's view is still alive");
    let looking = cx.update(|_, cx| crate::shown_box(cx).is_some_and(|b| b.read(cx).looking_up()))?;
    ensure!(!looking, "a member lookup outlived its token");
    cx.log("closed: the results view was dropped");
    if !unmeasured.is_empty() {
        // Never a pass without the measurement: say which rest couldn't be measured.
        return Err(decibell_harness::inconclusive(format!("{} at rest: every try disturbed by the shared stack or the desktop", unmeasured.join(" and "))));
    }
    Ok(())
}

async fn check_typing_cost(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    focus_box(&cx).await?;
    clear_box(&cx).await?;
    let list_frames = || -> Result<u64> { cx.update(|_, cx| decibell_chat::shown_list(cx).map_or(0, |l| l.read(cx).stats.frames)) };
    let events = || -> Result<u64> { cx.update(|_, cx| model(cx).pump_stats().events) };
    // The stack is shared: other clients of perf1 and of the channel (presence, a message, a
    // roster change) reach the list through the core and may redraw it legitimately. A try in
    // which the list drew counts only when no core event arrived meanwhile; three tries.
    let mut last = String::new();
    for attempt in 1..=3 {
        cx.settle().await.ok();
        let (list_before, events_before) = (list_frames()?, events()?);
        let mut per_key = Vec::new();
        for key in ["w", "a", "y", "l", "a", "n", "d"] {
            // Between the caret's blinks: a key moves the next blink half a second on.
            let mut counted = None;
            for _ in 0..3 {
                cx.sleep(Duration::from_millis(60)).await;
                let before = cx.frames()?;
                cx.type_text(key)?;
                cx.sleep(Duration::from_millis(120)).await;
                let drawn = cx.frames()?.since(before).drawn;
                if drawn <= 1 {
                    counted = Some(drawn);
                    break;
                }
                counted = Some(drawn);
            }
            per_key.push(counted.unwrap_or(0));
        }
        let (list_drew, arrived) = (list_frames()? - list_before, events()? - events_before);
        cx.log(format_args!("try {attempt}: frames per key {per_key:?}; the message list drew {list_drew} times meanwhile; {arrived} core events arrived"));
        ensure!(per_key.iter().all(|f| *f == 1), "a key drew {per_key:?} frames");
        // The typed word selected, then away (`clear_box` expects the selection Ctrl+F leaves).
        cx.keystroke("ctrl-a")?;
        clear_box(&cx).await?;
        if list_drew == 0 {
            return Ok(());
        }
        // GPUI patch 0014 replays clean cached views under a dirty one: a key in the box must not
        // lay the list out again.
        ensure!(arrived > 0, "the message list drew {list_drew} times while typing in the box, with no core event to redraw it");
        last = format!("the message list drew {list_drew} times while typing; {arrived} core events from the shared stack arrived meanwhile");
    }
    Err(decibell_harness::inconclusive(format!("three tries disturbed by the shared stack ({last})")))
}
