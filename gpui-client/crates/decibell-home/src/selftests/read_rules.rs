//! `dm-read-rules`: a DM is marked read only while it is being read (owner, 2026-11-26): the
//! conversation open, the window focused, its list at the live bottom.
//!
//! Three clients of two throwaway accounts, registered by a `decibell-headless` (`register`):
//! this app as account A (the reader), a headless second device of A (`a2`: what the account's
//! other devices see, read state synced by central) and a headless B (the sender). The window's
//! "active" state is held by the test (`decibell_harness::focus::hold_window_focus`): other
//! windows share this desktop, real focus isn't ours. Scenarios, each a DM from B:
//!
//! - (a) the conversation open at the bottom, the window unfocused: unread here (the count and
//!   the server bar's tile) and on A's other device; the focus comes back: read on both.
//! - (b) focused, the list scrolled up: unread; jump to present (the pill): read on both.
//! - (c) focused at the bottom: never counted, read on both at once.
//! - (d) minimised (the platform's activation, the real minimise): unread; restored
//!   (`tools/perf/winctl.sh activate`): read on both.

use std::path::{Path, PathBuf};
use std::process::{Child as Process, Command, Stdio};
use std::sync::mpsc;

use super::*;

/// `decibell-headless` next to this binary.
fn headless_exe() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    let candidate = exe.parent()?.join("decibell-headless");
    candidate.exists().then_some(candidate)
}

/// One headless client running a script; its stdout lines arrive on a channel.
struct Headless {
    label: &'static str,
    process: Process,
    lines: mpsc::Receiver<String>,
    seen: Vec<String>,
}

impl Headless {
    fn start(label: &'static str, profile: &Path, central: &str, steps: Vec<String>) -> Result<Headless> {
        let exe = headless_exe().context("decibell-headless is not built next to this binary")?;
        let mut args: Vec<String> = vec!["--user-data-dir".into(), profile.display().to_string(), "--timeout".into(), "60".into()];
        args.extend(steps);
        let mut process = Command::new(exe)
            .args(&args)
            .env("DECIBELL_CENTRAL", central)
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .context("starting decibell-headless")?;
        let stdout = process.stdout.take().expect("piped");
        let (tx, lines) = mpsc::channel();
        std::thread::Builder::new().name(format!("dm-read-rules-{label}")).spawn(move || {
            for line in std::io::BufReader::new(stdout).lines().map_while(Result::ok) {
                if tx.send(line).is_err() {
                    break;
                }
            }
        })?;
        Ok(Headless { label, process, lines, seen: Vec::new() })
    }

    /// Until a line starting with `prefix` (FAILED fails); returns it.
    async fn line(&mut self, cx: &TestCx, prefix: &str, limit: Duration) -> Result<String> {
        let started = Instant::now();
        loop {
            while let Ok(line) = self.lines.try_recv() {
                self.seen.push(line.clone());
                if line.starts_with("FAILED") {
                    bail!("{}: {line}", self.label);
                }
                if line.starts_with(prefix) {
                    return Ok(line);
                }
            }
            ensure!(started.elapsed() < limit, "{} never printed {prefix:?} (it printed {:?})", self.label, self.seen);
            cx.sleep(Duration::from_millis(30)).await;
        }
    }

    async fn mark(&mut self, cx: &TestCx, name: &str) -> Result<()> {
        self.line(cx, &format!("MARK {name}"), Duration::from_secs(60)).await.map(|_| ())
    }
}

impl Drop for Headless {
    fn drop(&mut self) {
        let _ = self.process.kill();
        let _ = self.process.wait();
    }
}

/// Signs `name` up with a headless client; central's sign-up bucket (10, then one per 2 min per
/// address, shared with every test of this stack) is waited for, up to 5 min.
async fn register(cx: &TestCx, stack: &Stack, name: &str, password: &str) -> Result<()> {
    let started = Instant::now();
    loop {
        let profile = TempDir::new("read-rules-register")?;
        let mut client = Headless::start("register", &profile.0, &stack.central, vec!["register".into(), name.into(), password.into()])?;
        match client.line(cx, "DONE", Duration::from_secs(60)).await {
            Ok(_) => return Ok(()),
            Err(e) if e.to_string().contains("Too many sign-ups") && started.elapsed() < Duration::from_secs(300) => {
                cx.log("central's sign-up bucket is empty (shared with other tests): waiting for a token");
                cx.sleep(Duration::from_secs(30)).await;
            }
            Err(e) => return Err(e),
        }
    }
}

/// The parent: the run's two throwaway accounts (registered by a headless client unless another
/// people test of this run did already), then the app as A.
pub(super) async fn parent(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    if headless_exe().is_none() {
        cx.log("SKIP: decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)");
        return Ok(());
    }
    let known = ACCOUNTS.lock().expect("lock").clone().filter(|a| a.registered);
    let accounts = match known {
        Some(accounts) => {
            cx.log(format_args!("the run's throwaway accounts {} and {}", accounts.a, accounts.b));
            accounts
        }
        None => {
            let tag = format!("{:x}", std::process::id() ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)));
            let accounts = Accounts { a: format!("ht{tag}a"), b: format!("ht{tag}b"), password: format!("home-test-{tag}-pw"), registered: true };
            cx.log(format_args!("registering the throwaway accounts {} and {} (decibell-headless)", accounts.a, accounts.b));
            register(&cx, &stack, &accounts.a, &accounts.password).await?;
            register(&cx, &stack, &accounts.b, &accounts.password).await?;
            *ACCOUNTS.lock().expect("lock") = Some(accounts.clone());
            accounts
        }
    };
    let cues = TempDir::new("read-rules-cues")?;
    let app_profile = TempDir::new("read-rules-a")?;
    let env = actor_env("dm-read-rules", "a", &accounts.a, &accounts.b, &accounts.password, &cues.0);
    let child = spawn("home-dm-read-rules", "a", None, &stack, &app_profile.0, &env)?;
    match finish(&cx, vec![child]).await? {
        Outcome::Passed => Ok(()),
        Outcome::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

fn shown_list(cx: &App) -> Option<gpui::Entity<decibell_chat::MessageList>> {
    crate::hooks::shown::<Conversation>(cx).map(|c| c.read(cx).list().clone())
}

fn reading(cx: &App) -> bool {
    crate::hooks::shown::<Conversation>(cx).is_some_and(|c| c.read(cx).reading())
}

fn has_row(peer: Uid, text: &str, cx: &App) -> bool {
    conversation_rows(peer, cx).iter().any(|m| m.id > 0 && &*m.content == text)
}

/// `tools/perf/winctl.sh <verb> <our pid>` (KWin scripting), found above the binary.
fn winctl(verb: &str) -> Result<bool> {
    let exe = std::env::current_exe()?;
    let Some(script) = exe.ancestors().map(|dir| dir.join("tools/perf/winctl.sh")).find(|path| path.exists()) else {
        return Ok(false);
    };
    let status = Command::new(script).arg(verb).arg(std::process::id().to_string()).stdout(Stdio::null()).stderr(Stdio::null()).status()?;
    Ok(status.success())
}

/// Inside the app, signed in as A.
pub(super) async fn check(cx: TestCx) -> Result<()> {
    let actor = Actor::from_env()?;
    let (a_name, b_name, password) = (actor.me.clone(), actor.peer.clone(), actor.password.clone());
    let central = std::env::var("DECIBELL_CENTRAL").context("DECIBELL_CENTRAL")?;
    wait_for(&cx, Duration::from_secs(30), "the app to boot", |cx| model(cx).auth.read(cx).booted).await?;
    // Not focused until a scenario says so, whatever the desktop does.
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(false), cx))?;
    {
        let (name, password) = (a_name.clone(), password.clone());
        cx.update(move |_, cx| actions::auth::sign_in(name, password, cx))?;
    }
    signed_in(&cx).await?;
    let me = cx.update(|_, cx| own_uid(cx))?;
    cx.log(format_args!("the reader is {a_name} (uid {me}); the sender {b_name}"));

    let run = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()));
    let text = |what: &str| format!("read rules ({what}) for {a_name}, run {run}");
    let (text_a, fill1, fill2, text_b, text_c, text_d) = (text("a"), text("fill 1"), text("fill 2"), text("b"), text("c"), text("d"));
    // Tall rows, so the list can be scrolled up away from its bottom.
    let tall = |head: &str| std::iter::once(head.to_string()).chain((1..=40).map(|n| format!("line {n}"))).collect::<Vec<_>>().join("\n");
    let (fill1, fill2) = (tall(&fill1), tall(&fill2));
    let cue_path = |name: &str| actor.cues.join(name).display().to_string();
    let send = |cue: &str, texts: &[&String], mark: &str| -> Vec<String> {
        let mut steps = vec!["wait-file".into(), cue_path(cue)];
        for t in texts {
            steps.extend(["dm-send".to_string(), format!("uid:{me}"), (*t).clone()]);
        }
        steps.extend(["wait-dm".to_string(), format!("uid:{me}"), texts.last().expect("one").to_string(), "print".into(), mark.into()]);
        steps
    };
    let b_profile = TempDir::new("read-rules-b")?;
    let mut b_steps: Vec<String> = ["login-as", &b_name, &password, "wait", "logged-in", "dump", "auth", "print", "ready"].iter().map(|s| s.to_string()).collect();
    b_steps.extend(send("send-a", &[&text_a], "sent-a"));
    b_steps.extend(send("send-fill", &[&fill1, &fill2], "sent-fill"));
    b_steps.extend(send("send-b", &[&text_b], "sent-b"));
    b_steps.extend(send("send-c", &[&text_c], "sent-c"));
    b_steps.extend(send("send-d", &[&text_d], "sent-d"));
    b_steps.extend(["wait-file".into(), cue_path("done"), "logout".into(), "wait".into(), "logged-out".into()]);
    let mut b = Headless::start("b", &b_profile.0, &central, b_steps)?;
    let auth = b.line(&cx, "DUMP auth", Duration::from_secs(60)).await?;
    let peer: Uid = auth
        .split("\"uid\":")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .context("B's uid in its auth dump")?;
    b.mark(&cx, "ready").await?;

    // A second device of A: what the account's other devices see.
    let a2_profile = TempDir::new("read-rules-a2")?;
    let mut a2_steps: Vec<String> = ["login-as", &a_name, &password, "wait", "logged-in", "print", "ready"].iter().map(|s| s.to_string()).collect();
    let wait_unread = |n: i64, mark: &str| -> Vec<String> { vec!["wait-unread".into(), b_name.clone(), n.to_string(), "print".into(), mark.into()] };
    let wait_dm = |t: &String| -> Vec<String> { vec!["wait-dm".into(), b_name.clone(), t.clone()] };
    a2_steps.extend(wait_dm(&text_a));
    a2_steps.extend(wait_unread(1, "a2-a-unread"));
    a2_steps.extend(wait_unread(0, "a2-a-read"));
    a2_steps.extend(wait_dm(&fill2));
    a2_steps.extend(wait_unread(0, "a2-fill-read"));
    a2_steps.extend(wait_dm(&text_b));
    a2_steps.extend(wait_unread(1, "a2-b-unread"));
    a2_steps.extend(wait_unread(0, "a2-b-read"));
    a2_steps.extend(wait_dm(&text_c));
    a2_steps.extend(wait_unread(0, "a2-c-read"));
    a2_steps.extend(wait_dm(&text_d));
    a2_steps.extend(wait_unread(1, "a2-d-unread"));
    a2_steps.extend(wait_unread(0, "a2-d-read"));
    a2_steps.extend(["wait-file".into(), cue_path("done"), "logout".into(), "wait".into(), "logged-out".into()]);
    let mut a2 = Headless::start("a2", &a2_profile.0, &central, a2_steps)?;
    a2.mark(&cx, "ready").await?;

    // The conversation with B, open (nobody wrote yet).
    {
        let name = b_name.clone();
        cx.update(move |_, cx| actions::dm::open_dm(peer, name.into(), cx))?;
    }
    wait_for(&cx, Duration::from_secs(10), "the conversation", move |cx| crate::hooks::shown::<Conversation>(cx).is_some_and(|c| c.read(cx).peer() == peer)).await?;
    park_pointer(&cx)?;
    // What an earlier test of this pair left unread is read first (focused at the bottom).
    if cx.update(move |_, cx| unread(peer, cx))? > 0 {
        cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
        wait_for(&cx, Duration::from_secs(15), "the earlier messages read", move |cx| unread(peer, cx) == 0).await?;
        cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(false), cx))?;
        wait_for(&cx, Duration::from_secs(5), "not being read", |cx| !reading(cx)).await?;
    }

    // (a) Unfocused: unread here and on the other device; focused: read on both.
    actor.cue("send-a", 1)?;
    {
        let t = text_a.clone();
        wait_for(&cx, Duration::from_secs(15), "B's first message", move |cx| has_row(peer, &t, cx)).await?;
    }
    wait_for(&cx, Duration::from_secs(5), "its unread count", move |cx| unread(peer, cx) == 1).await?;
    wait_probe(&cx, &format!("unread-dm:{peer}")).await?;
    let at_bottom = cx.update(|_, cx| shown_list(cx).and_then(|l| l.read(cx).at_live_bottom()))?;
    a2.mark(&cx, "a2-a-unread").await?;
    cx.sleep(Duration::from_millis(600)).await;
    let (count, being_read) = cx.update(move |_, cx| (unread(peer, cx), reading(cx)))?;
    cx.log(format_args!("(a) unfocused, the list at its bottom ({at_bottom:?}): {count} unread, the tile drawn, the other device sees it unread"));
    ensure!(count == 1 && !being_read && at_bottom == Some(true), "(a) unfocused: {count} unread, being read {being_read}, at the bottom {at_bottom:?}");
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    let t = wait_for(&cx, Duration::from_secs(5), "the focus to read it", move |cx| unread(peer, cx) == 0 && reading(cx)).await?;
    a2.mark(&cx, "a2-a-read").await?;
    cx.log(format_args!("(a) focused: read here {} ms later, then on the other device", t.as_millis()));

    // Tall rows from B while being read: read as they come.
    actor.cue("send-fill", 1)?;
    b.mark(&cx, "sent-fill").await?;
    {
        let t = fill2.clone();
        wait_for(&cx, Duration::from_secs(15), "the tall rows", move |cx| has_row(peer, &t, cx)).await?;
    }
    a2.mark(&cx, "a2-fill-read").await?;
    cx.settle().await?;

    // (b) Focused, scrolled up: unread; jump to present: read.
    cx.update(|_, cx| {
        if let Some(list) = shown_list(cx) {
            list.update(cx, |list, cx| list.scroll_by_precise(-900., cx));
        }
    })?;
    wait_for(&cx, Duration::from_secs(5), "the list off its bottom", |cx| !reading(cx) && shown_list(cx).is_some_and(|l| l.read(cx).at_live_bottom() == Some(false))).await?;
    actor.cue("send-b", 1)?;
    {
        let t = text_b.clone();
        wait_for(&cx, Duration::from_secs(15), "B's message", move |cx| has_row(peer, &t, cx)).await?;
    }
    a2.mark(&cx, "a2-b-unread").await?;
    cx.sleep(Duration::from_millis(600)).await;
    let (count, being_read) = cx.update(move |_, cx| (unread(peer, cx), reading(cx)))?;
    cx.log(format_args!("(b) focused, scrolled up: {count} unread (being read: {being_read}), unread on the other device"));
    ensure!(count == 1 && !being_read, "(b) scrolled up: {count} unread, being read {being_read}");
    cx.update(|_, cx| {
        if let Some(list) = shown_list(cx) {
            list.update(cx, |list, cx| list.jump_to_present(cx));
        }
    })?;
    let t = wait_for(&cx, Duration::from_secs(5), "jump to present to read it", move |cx| unread(peer, cx) == 0 && reading(cx)).await?;
    a2.mark(&cx, "a2-b-read").await?;
    cx.log(format_args!("(b) jumped to present: read {} ms later, then on the other device", t.as_millis()));

    // (c) Focused at the bottom: never counted.
    let peak = std::rc::Rc::new(std::cell::Cell::new(0i64));
    let _watch = cx.update({
        let peak = peak.clone();
        move |_, cx| {
            let list = model(cx).dm.list.clone();
            cx.observe(&list, move |list, cx| peak.set(peak.get().max(list.read(cx).get(peer).map_or(0, |c| c.unread))))
        }
    })?;
    actor.cue("send-c", 1)?;
    {
        let t = text_c.clone();
        wait_for(&cx, Duration::from_secs(15), "B's message", move |cx| has_row(peer, &t, cx)).await?;
    }
    a2.mark(&cx, "a2-c-read").await?;
    let (count, being_read) = cx.update(move |_, cx| (unread(peer, cx), reading(cx)))?;
    cx.log(format_args!("(c) focused at the bottom: at most {} unread here, now {count}; read on the other device", peak.get()));
    ensure!(peak.get() == 0 && count == 0 && being_read, "(c) at the bottom: peaked at {} unread, being read {being_read}", peak.get());
    drop(_watch);

    // (d) Minimised (the platform's activation and visibility): unread; restored: read.
    let restorable = std::env::current_exe()?.ancestors().any(|dir| dir.join("tools/perf/winctl.sh").exists());
    if !restorable {
        cx.log("(d) SKIP: tools/perf/winctl.sh isn't above the binary: a minimised window can't be restored from here");
    } else {
        cx.update(|window, cx| {
            decibell_harness::focus::hold_window_focus(None, cx);
            window.minimize_window();
        })?;
        wait_for(&cx, Duration::from_secs(10), "the minimised window to stop being read", |cx| !reading(cx)).await?;
        let (active, visible) = cx.update(|window, _| (window.is_window_active(), window.is_visible()))?;
        actor.cue("send-d", 1)?;
        {
            let t = text_d.clone();
            wait_for(&cx, Duration::from_secs(15), "B's message", move |cx| has_row(peer, &t, cx)).await?;
        }
        a2.mark(&cx, "a2-d-unread").await?;
        cx.sleep(Duration::from_millis(600)).await;
        let count = cx.update(move |_, cx| unread(peer, cx))?;
        cx.log(format_args!("(d) minimised (active {active}, visible {visible}): {count} unread here and on the other device"));
        ensure!(count == 1, "(d) minimised: {count} unread");
        ensure!(winctl("activate")?, "winctl.sh activate failed");
        cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
        let t = wait_for(&cx, Duration::from_secs(10), "the restored window to read it", move |cx| unread(peer, cx) == 0 && reading(cx)).await?;
        a2.mark(&cx, "a2-d-read").await?;
        cx.log(format_args!("(d) restored: read {} ms later, then on the other device", t.as_millis()));
    }

    actor.cue("done", 1)?;
    if !restorable {
        // The scripts still wait for (d): end them.
        drop(b);
        drop(a2);
    } else {
        b.line(&cx, "DONE", Duration::from_secs(30)).await?;
        a2.line(&cx, "DONE", Duration::from_secs(30)).await?;
    }
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    sign_out(&cx).await
}
