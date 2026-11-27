//! The people self-tests of parity audit A's §2 (T13 / T14 / T16 / T17 / T18): the app as a
//! throwaway account A against `decibell-headless` participants (B, and A's second device), each
//! signed in by password with its own profile; the run's two throwaway accounts are registered by
//! a headless client (`read_rules::headless_accounts`). The app's check drives the headless
//! scripts through cue files (`wait-file`) and reads their `MARK`s.
//!
//! - `people-friends`: Decline, Cancel, Accept, Message and Unblock by click on the Friends page;
//!   the inline error for an unknown name and for yourself, gone after 5 s; A's second device
//!   follows every change.
//! - `people-status`: each status chosen in the status menu as a watcher sees it, Invisible as
//!   offline in Daily Gaming's roster (both accounts join it and leave again) and a call to an
//!   invisible user ("Not available"); the custom status's 128-character cap and counter, the
//!   menu's edit and clear, hidden while invisible, a 60 s clear-after reaching the watcher.
//! - `people-interest`: a member row drawn → watched; the member's status change redraws its dot;
//!   scrolled away → released (central's subscription set follows).
//! - `people-profile`: the popout's avatar opens the profile screen; its friend actions; the
//!   Privacy tab with both accounts encrypted (safety number drawn and copied); a value selected
//!   by a double click and copied.
//!
//! Every test that writes the clipboard puts back what was there. Statuses are put back to
//! Online, memberships left, friendships removed.

use std::rc::Rc;

use gpui::{ClipboardItem, Modifiers, PlatformInput, ScrollDelta, ScrollEvent, TouchPhase};

use super::read_rules::{Headless, headless_accounts, headless_exe};
use super::*;
use decibell_model::live::presence::{DisplayStatus, OwnStatus, StatusMessage};

pub(super) fn selftests() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "people-friends", about: "throwaway accounts: Decline, Cancel, Accept, Message, Unblock by click; the inline errors (unknown name, yourself) gone after 5 s; the account's other device follows", run: |cx| Box::pin(parent(cx, "people-friends")) },
        SelfTest { name: "people-status", about: "throwaway accounts in Daily Gaming: each status from the menu as a watcher sees it, Invisible offline in the roster and not callable; the custom status cap, counter, edit, clear, hidden while invisible, a 60 s clear-after", run: |cx| Box::pin(parent(cx, "people-status")) },
        SelfTest { name: "people-interest", about: "throwaway accounts in Daily Gaming: a member row drawn is watched, the member's status change redraws its dot, scrolled away it is released", run: |cx| Box::pin(parent(cx, "people-interest")) },
        SelfTest { name: "people-profile", about: "throwaway accounts: the popout's avatar opens the profile screen, its friend actions, the Privacy tab encrypted (safety number drawn and copied), a value selected and copied", run: |cx| Box::pin(parent(cx, "people-profile")) },
        SelfTest { name: "people-popout-stream", about: "as perf3 on a null sink: perf4 streams in Lounge; perf4's popout shows the live section (Live in #Lounge); its click joins Lounge and watches the stream large", run: |cx| Box::pin(popout_stream_parent(cx)) },
    ]
}

pub(super) fn checks() -> Vec<SelfTest> {
    vec![
        SelfTest { name: "home-people-friends", about: "(in the app) people-friends as A", run: |cx| Box::pin(check_friends(cx)) },
        SelfTest { name: "home-people-status", about: "(in the app) people-status as A", run: |cx| Box::pin(check_status(cx)) },
        SelfTest { name: "home-people-interest", about: "(in the app) people-interest as A", run: |cx| Box::pin(check_interest(cx)) },
        SelfTest { name: "home-people-profile", about: "(in the app) people-profile as A", run: |cx| Box::pin(check_profile(cx)) },
        SelfTest { name: "home-people-popout-stream", about: "(in the app) people-popout-stream as perf3", run: |cx| Box::pin(check_popout_stream(cx)) },
    ]
}

async fn parent(cx: TestCx, test: &'static str) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    if headless_exe().is_none() {
        cx.log("SKIP: decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)");
        return Ok(());
    }
    let accounts = headless_accounts(&cx, &stack).await?;
    let cues = TempDir::new(&format!("{test}-cues"))?;
    let profile = TempDir::new(&format!("{test}-a"))?;
    let mut env = actor_env(test, "a", &accounts.a, &accounts.b, &accounts.password, &cues.0);
    if test == "people-profile" {
        // It copies: under XWayland (GPUI picks X11 when WAYLAND_DISPLAY is empty), where the
        // clipboard is ours to write and read back without the desktop's keyboard focus, which a
        // test window sharing this desktop may not have. What was there is put back.
        env.push(("WAYLAND_DISPLAY", String::new()));
    }
    let child = spawn(&format!("home-{test}"), "a", None, &stack, &profile.0, &env)?;
    match finish(&cx, vec![child]).await? {
        Outcome::Passed => Ok(()),
        Outcome::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

// ── Inside the app ─────────────────────────────────────────────────────────────────────────────

struct Session {
    actor: Actor,
    me: Uid,
    central: String,
}

impl Session {
    fn cue_path(&self, name: &str) -> String {
        self.actor.cues.join(name).display().to_string()
    }

    fn cue(&self, name: &str) -> Result<()> {
        self.actor.cue(name, 1)
    }

    /// A headless participant signed in as `name`, running `steps` after `print ready`; its uid.
    async fn participant(&self, cx: &TestCx, label: &'static str, name: &str, steps: Vec<String>) -> Result<(Headless, Uid, TempDir)> {
        let profile = TempDir::new(&format!("people-{label}"))?;
        let mut all: Vec<String> = ["login-as", name, &self.actor.password, "wait", "logged-in", "dump", "auth", "print", "ready"].iter().map(|s| s.to_string()).collect();
        all.extend(steps);
        let mut client = Headless::start(label, &profile.0, &self.central, all)?;
        let auth = client.line(cx, "DUMP auth", Duration::from_secs(60)).await?;
        let uid: Uid = auth
            .split("\"uid\":")
            .nth(1)
            .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
            .and_then(|digits| digits.parse().ok())
            .context("a uid in the auth dump")?;
        client.mark(cx, "ready").await?;
        Ok((client, uid, profile))
    }
}

/// Boot, the window held active (other windows share this desktop), signed in as A by password.
async fn session(cx: &TestCx) -> Result<Session> {
    let actor = Actor::from_env()?;
    let central = std::env::var("DECIBELL_CENTRAL").context("DECIBELL_CENTRAL")?;
    wait_for(cx, Duration::from_secs(30), "the app to boot", |cx| model(cx).auth.read(cx).booted).await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    {
        let (name, password) = (actor.me.clone(), actor.password.clone());
        cx.update(move |_, cx| actions::auth::sign_in(name, password, cx))?;
    }
    signed_in(cx).await?;
    let me = cx.update(|_, cx| own_uid(cx))?;
    cx.log(format_args!("A is {} (uid {me}); B is {}", actor.me, actor.peer));
    Ok(Session { actor, me, central })
}

fn steps(parts: &[&str]) -> Vec<String> {
    parts.iter().map(|s| s.to_string()).collect()
}

/// The clipboard as it was, put back when dropped (tests that copy must not keep the owner's).
struct ClipboardGuard {
    cx: gpui::AsyncApp,
    saved: Option<ClipboardItem>,
}

impl ClipboardGuard {
    fn take(cx: &TestCx) -> Result<ClipboardGuard> {
        let saved = cx.update(|_, cx| cx.read_from_clipboard())?;
        Ok(ClipboardGuard { cx: cx.app().clone(), saved })
    }
}

impl Drop for ClipboardGuard {
    fn drop(&mut self) {
        let saved = self.saved.take().unwrap_or_else(|| ClipboardItem::new_string(String::new()));
        self.cx.update(|cx| cx.write_to_clipboard(saved));
    }
}

fn clipboard_text(cx: &App) -> Option<String> {
    cx.read_from_clipboard().and_then(|item| item.text())
}

/// Join Daily Gaming from the directory as Browse's Join does, and wait for its roster.
async fn join_daily_gaming(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::servers::request_server_list(cx))?;
    wait_for(cx, Duration::from_secs(15), "the directory", |cx| model(cx).servers.read(cx).servers().iter().any(|s| s.id == daily_gaming())).await?;
    let failed: Rc<std::cell::RefCell<Option<String>>> = Rc::default();
    {
        let failed = failed.clone();
        cx.update(move |_, cx| {
            let (host, port) = model(cx).servers.read(cx).servers().iter().find(|s| s.id == daily_gaming()).map(|s| (s.host.to_string(), s.port)).expect("listed");
            actions::servers::connect(daily_gaming(), host, port, cx, move |result, _| {
                if let Err(e) = result {
                    *failed.borrow_mut() = Some(e.js_string().to_string());
                }
            })
        })?;
    }
    let f = failed.clone();
    wait_for(cx, Duration::from_secs(30), "Daily Gaming to connect", move |cx| f.borrow().is_some() || model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    if let Some(e) = failed.borrow().clone() {
        bail!("joining Daily Gaming: {e}");
    }
    connected(cx, daily_gaming()).await
}

/// Leave Daily Gaming (the membership a test made).
async fn leave_daily_gaming(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| actions::servers::leave(daily_gaming(), cx))?;
    wait_for(cx, Duration::from_secs(15), "leaving Daily Gaming", |cx| !model(cx).connections.read(cx).is_connected(daily_gaming())).await?;
    Ok(())
}

fn toast_titles(cx: &App) -> Vec<String> {
    model(cx).toasts.read(cx).toasts().iter().map(|t| t.title.to_string()).collect()
}

// ── people-friends (T18) ────────────────────────────────────────────────────────────────────────

async fn check_friends(cx: TestCx) -> Result<()> {
    let s = session(&cx).await?;
    let (a_name, b_name) = (s.actor.me.clone(), s.actor.peer.clone());
    let b_steps = steps(&[
        "friends", "wait-file", &s.cue_path("b-request1"), "friend-add", &a_name, "print", "requested1",
        "wait-file", &s.cue_path("b-request2"), "friend-add", &a_name, "print", "requested2",
        "wait-file", &s.cue_path("done"), "logout", "wait", "logged-out",
    ]);
    let (mut b, peer, _b_profile) = s.participant(&cx, "b", &b_name, b_steps).await?;
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    // What an earlier test of this pair left between them goes first.
    if let Some(status) = cx.update(move |_, cx| friend_status(peer, cx))? {
        cx.log(format_args!("an earlier friendship state with B ({status:?}): removed first"));
        let name = b_name.clone();
        cx.update(move |_, cx| actions::friends::act(FriendAction::Remove, peer, name.into(), cx))?;
        wait_for(&cx, Duration::from_secs(10), "the old state to go", move |cx| friend_status(peer, cx).is_none()).await?;
    }
    let p = format!("#{peer}");
    let mut a2_steps = steps(&["friends"]);
    for (want, mark) in [
        ("pending_incoming", "a2-incoming"),
        ("none", "a2-declined"),
        ("pending_outgoing", "a2-outgoing"),
        ("none", "a2-cancelled"),
        ("pending_incoming", "a2-incoming2"),
        ("online|offline", "a2-friends"),
        ("blocked", "a2-blocked"),
        ("none", "a2-unblocked"),
    ] {
        a2_steps.extend(steps(&["wait", "friend", &p, want, "print", mark]));
    }
    a2_steps.extend(steps(&["wait-file", &s.cue_path("done"), "logout", "wait", "logged-out"]));
    let (mut a2, _, _a2_profile) = s.participant(&cx, "a2", &a_name, a2_steps).await?;
    quiet(&cx).await?;

    // B asks; Decline on the Pending tab.
    s.cue("b-request1")?;
    b.mark(&cx, "requested1").await?;
    wait_for(&cx, Duration::from_secs(10), "B's request", move |cx| friend_status(peer, cx) == Some(FriendStatus::PendingIncoming)).await?;
    a2.mark(&cx, "a2-incoming").await?;
    click_probe(&cx, "friends-filter-pending").await?;
    click_probe(&cx, &format!("friend-decline:{peer}")).await?;
    wait_for(&cx, Duration::from_secs(10), "the decline", move |cx| friend_status(peer, cx).is_none()).await?;
    a2.mark(&cx, "a2-declined").await?;
    cx.log("Decline by click: gone here and on A's other device");

    // A asks (the action: the form is the friends-flow test's); Cancel by click.
    {
        let name = b_name.clone();
        cx.update(move |_, cx| actions::friends::add_friend(name, cx, |_, _| {}))?;
    }
    wait_for(&cx, Duration::from_secs(10), "our request", move |cx| friend_status(peer, cx) == Some(FriendStatus::PendingOutgoing)).await?;
    a2.mark(&cx, "a2-outgoing").await?;
    click_probe(&cx, &format!("friend-cancel:{peer}")).await?;
    wait_for(&cx, Duration::from_secs(10), "the cancel", move |cx| friend_status(peer, cx).is_none()).await?;
    a2.mark(&cx, "a2-cancelled").await?;
    cx.log("Cancel by click: gone here and on A's other device");

    // B asks again; Accept; then Message from the All tab.
    s.cue("b-request2")?;
    b.mark(&cx, "requested2").await?;
    wait_for(&cx, Duration::from_secs(10), "B's second request", move |cx| friend_status(peer, cx) == Some(FriendStatus::PendingIncoming)).await?;
    a2.mark(&cx, "a2-incoming2").await?;
    click_probe(&cx, &format!("friend-accept:{peer}")).await?;
    wait_for(&cx, Duration::from_secs(10), "the friendship", move |cx| friend_status(peer, cx).is_some_and(FriendStatus::is_accepted)).await?;
    a2.mark(&cx, "a2-friends").await?;
    click_probe(&cx, "friends-filter-all").await?;
    hover_row(&cx, peer).await?;
    click_probe(&cx, &format!("friend-message:{peer}")).await?;
    wait_for(&cx, Duration::from_secs(10), "the conversation", move |cx| {
        let m = model(cx);
        m.nav.read(cx).active_view == ActiveView::Dm && m.dm.list.read(cx).active_peer == Some(peer)
    })
    .await?;
    cx.log("Accept, then Message by click: the conversation with B is open");
    cx.update(|_, cx| actions::servers::go_home(cx))?;

    // Block (the action: Electron has no Block entry); Unblock by click on the Blocked tab.
    {
        let name = b_name.clone();
        cx.update(move |_, cx| actions::friends::act(FriendAction::Block, peer, name.into(), cx))?;
    }
    wait_for(&cx, Duration::from_secs(10), "the block", move |cx| friend_status(peer, cx) == Some(FriendStatus::Blocked)).await?;
    a2.mark(&cx, "a2-blocked").await?;
    click_probe(&cx, "friends-filter-blocked").await?;
    hover_row(&cx, peer).await?;
    click_probe(&cx, &format!("friend-unblock:{peer}")).await?;
    wait_for(&cx, Duration::from_secs(10), "the unblock", move |cx| friend_status(peer, cx).is_none()).await?;
    a2.mark(&cx, "a2-unblocked").await?;
    cx.log("Unblock by click: gone here and on A's other device");

    // The inline errors: an unknown name, then our own; each goes 5 s after it shows.
    let run = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()));
    for (what, name) in [("an unknown name", format!("nobody{run}")), ("our own name", a_name.clone())] {
        click_probe(&cx, "friends-add").await?;
        quiet(&cx).await?;
        cx.keystroke("ctrl-a")?;
        cx.type_text(&name)?;
        cx.keystroke("enter")?;
        wait_for(&cx, Duration::from_secs(10), &format!("the error for {what}"), |_| friends_error().is_some()).await?;
        let text = friends_error().unwrap_or_default();
        let started = Instant::now();
        wait_for(&cx, Duration::from_secs(8), "the error to go", |_| friends_error().is_none()).await?;
        let lasted = started.elapsed();
        cx.log(format_args!("{what}: {text:?}, shown for {:.1} s", lasted.as_secs_f64()));
        ensure!(!text.is_empty(), "{what}: an empty error");
        ensure!(lasted >= Duration::from_millis(4500) && lasted <= Duration::from_millis(6500), "{what}: the error lasted {lasted:?}, not 5 s");
    }
    s.cue("done")?;
    sign_out(&cx).await
}

/// The pointer over a friend's row: its buttons show on hover only (a request's always do).
async fn hover_row(cx: &TestCx, uid: Uid) -> Result<()> {
    let row = wait_probe(cx, &format!("friend:{uid}")).await?;
    cx.mouse_move(point(row.left() + px(60.), row.center().y))?;
    cx.sleep(Duration::from_millis(250)).await;
    Ok(())
}

/// The friends page's error as drawn (the action's, else the add form's).
fn friends_error() -> Option<String> {
    ["friends-error", "friends-add-error"].iter().find_map(|key| probes::text_of(key).filter(|t| !t.is_empty()).map(|t| t.to_string()))
}

// ── people-status (T14, T16) ────────────────────────────────────────────────────────────────────

fn own_display(cx: &App) -> DisplayStatus {
    model(cx).live.presence.own.read(cx).display()
}

fn own_message(cx: &App) -> Option<String> {
    model(cx).live.presence.own.read(cx).message.as_ref().map(|m| m.text.to_string())
}

/// The status menu opened from the user panel, then `row` clicked.
async fn status_menu(cx: &TestCx, row: &str) -> Result<()> {
    click_probe(cx, "user-panel-identity").await?;
    quiet(cx).await?;
    click_probe(cx, row).await?;
    quiet(cx).await
}

async fn check_status(cx: TestCx) -> Result<()> {
    let s = session(&cx).await?;
    let (a_name, b_name) = (s.actor.me.clone(), s.actor.peer.clone());
    let me = format!("#{}", s.me);
    let run = format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()));
    let (text1, text3) = (format!("status one {run}"), format!("clears in a minute {run}"));
    let dg = "Daily Gaming";
    let mut b_steps = steps(&["join", dg, "wait", "connected", dg, "open-server", dg, "watch", &me, "print", "watching"]);
    for (wait, mark) in [
        (vec!["message", &me, &text1], "b-msg1"),
        (vec!["presence", &me, "away"], "b-away"),
        (vec!["presence", &me, "dnd"], "b-dnd"),
        (vec!["presence", &me, "offline"], "b-offline"),
        (vec!["message", &me, "-"], "b-msg-hidden"),
        (vec!["offline", dg, &a_name], "b-roster-offline"),
        (vec!["presence", &me, "online"], "b-online"),
        (vec!["message", &me, &text1], "b-msg-back"),
        (vec!["online", dg, &a_name], "b-roster-online"),
        (vec!["message", &me, "-"], "b-cleared"),
    ] {
        b_steps.push("wait".into());
        b_steps.extend(wait.iter().map(|s| s.to_string()));
        b_steps.extend(steps(&["print", mark]));
    }
    b_steps.extend(steps(&[
        "wait-file", &s.cue_path("b-invisible"), "status", "invisible", "print", "b-is-invisible",
        "wait-file", &s.cue_path("b-visible"), "status", "online", "print", "b-is-online",
        "wait", "message", &me, &text3, "print", "b-msg3", "idle", "40", "wait", "message", &me, "-", "print", "b-msg3-gone",
        "wait-file", &s.cue_path("done"), "leave", dg, "logout", "wait", "logged-out",
    ]));
    let (mut b, peer, _b_profile) = s.participant(&cx, "b", &b_name, b_steps).await?;
    // Online, whatever happens below (the account keeps its status for the next sign-in).
    struct Restore(gpui::AsyncApp);
    impl Drop for Restore {
        fn drop(&mut self) {
            self.0.update(|cx| actions::presence::choose_status(OwnStatus::Online, cx));
        }
    }
    let _restore = Restore(cx.app().clone());
    join_daily_gaming(&cx).await?;
    b.line(&cx, "MARK watching", Duration::from_secs(90)).await?;
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    quiet(&cx).await?;

    // The custom status dialog from the menu: 140 characters typed, 128 kept, the counter says so.
    status_menu(&cx, "status-menu-set-custom").await?;
    wait_probe(&cx, "status-save").await?;
    quiet(&cx).await?;
    cx.type_text(&"x".repeat(140))?;
    let (kept, counter) = cx.update(|_, cx| {
        let kept = crate::hooks::shown::<CustomStatusModal>(cx).map_or(0, |m| m.read(cx).text().read(cx).value().chars().count());
        (kept, probes::text_of("status-counter").map(|t| t.to_string()))
    })?;
    cx.log(format_args!("140 characters typed: {kept} kept, the counter reads {counter:?}"));
    ensure!(kept == 128, "the cap: {kept} characters kept");
    ensure!(counter.as_deref() == Some("128/128"), "the counter: {counter:?}");
    cx.keystroke("ctrl-a")?;
    cx.type_text(&text1)?;
    click_probe(&cx, "status-save").await?;
    b.mark(&cx, "b-msg1").await?;
    cx.log("saved from the dialog: the watcher sees it");

    // Edit from the menu: the dialog opens on the message; Escape leaves it as it was.
    status_menu(&cx, "status-menu-edit").await?;
    let t1 = text1.clone();
    wait_for(&cx, Duration::from_secs(5), "the dialog on our message", move |cx| {
        crate::hooks::shown::<CustomStatusModal>(cx).is_some_and(|m| m.read(cx).text().read(cx).value() == t1) && model(cx).overlays.read(cx).is_open(modal::CUSTOM_STATUS)
    })
    .await?;
    quiet(&cx).await?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape to close it", |cx| !model(cx).overlays.read(cx).is_open(modal::CUSTOM_STATUS)).await?;
    cx.log("Edit custom status opens the dialog on the message");

    // Each status from the menu, as the watcher sees it.
    for (row, want, mark) in [
        ("status-menu:Away", DisplayStatus::Away, "b-away"),
        ("status-menu:Do Not Disturb", DisplayStatus::Dnd, "b-dnd"),
        ("status-menu:Invisible", DisplayStatus::Invisible, "b-offline"),
    ] {
        status_menu(&cx, row).await?;
        wait_for(&cx, Duration::from_secs(5), &format!("our status {want:?}"), move |cx| own_display(cx) == want).await?;
        b.mark(&cx, mark).await?;
        cx.log(format_args!("{row}: ours {want:?}, the watcher's mark {mark}"));
    }
    b.mark(&cx, "b-msg-hidden").await?;
    b.mark(&cx, "b-roster-offline").await?;
    cx.log("Invisible: the watcher sees us offline, without the message, offline in Daily Gaming's roster");
    status_menu(&cx, "status-menu:Online").await?;
    wait_for(&cx, Duration::from_secs(5), "Online again", |cx| own_display(cx) == DisplayStatus::Online).await?;
    b.mark(&cx, "b-online").await?;
    b.mark(&cx, "b-msg-back").await?;
    b.mark(&cx, "b-roster-online").await?;
    cx.log("Online: the watcher sees us, the message again, online in the roster");

    // Clear from the menu.
    status_menu(&cx, "status-menu-clear").await?;
    wait_for(&cx, Duration::from_secs(5), "our message cleared", |cx| own_message(cx).is_none()).await?;
    b.mark(&cx, "b-cleared").await?;
    cx.log("Clear custom status: gone here and for the watcher");

    // A call to an invisible user: "Not available".
    s.cue("b-invisible")?;
    b.mark(&cx, "b-is-invisible").await?;
    cx.sleep(Duration::from_millis(500)).await;
    cx.update(move |_, cx| actions::calls::start_call(peer, cx))?;
    wait_for(&cx, Duration::from_secs(15), "the \"Not available\" notice", |cx| toast_titles(cx).iter().any(|t| t == "Not available")).await?;
    wait_for(&cx, Duration::from_secs(10), "the call to end", |cx| model(cx).live.calls.read(cx).status == decibell_model::live::calls::CallStatus::Idle).await?;
    cx.log("calling the invisible B: \"Not available\", the call ended");
    s.cue("b-visible")?;
    b.mark(&cx, "b-is-online").await?;

    // A message clearing after 60 s: ours goes at the deadline, the watcher's with central's sweep.
    {
        let text = text3.clone();
        cx.update(move |_, cx| actions::presence::save_status_message(StatusMessage::of(&text, ""), 60, cx))?;
    }
    b.mark(&cx, "b-msg3").await?;
    let started = Instant::now();
    wait_for(&cx, Duration::from_secs(70), "ours to clear at 60 s", |cx| own_message(cx).is_none()).await?;
    let ours = started.elapsed();
    b.line(&cx, "MARK b-msg3-gone", Duration::from_secs(120)).await?;
    let theirs = started.elapsed();
    cx.log(format_args!("clear-after 60 s: ours cleared after {:.0} s, the watcher's after {:.0} s", ours.as_secs_f64(), theirs.as_secs_f64()));
    ensure!(ours >= Duration::from_secs(55) && ours <= Duration::from_secs(65), "ours cleared after {ours:?}");
    ensure!(theirs <= Duration::from_secs(100), "the watcher's cleared after {theirs:?}");

    s.cue("done")?;
    leave_daily_gaming(&cx).await?;
    sign_out(&cx).await
}

// ── people-interest (T17) ───────────────────────────────────────────────────────────────────────

fn watched(uid: Uid, cx: &App) -> bool {
    model(cx).live.presence.interest.read(cx).is_on_screen(uid)
}

fn interest_settled(cx: &App) -> bool {
    !model(cx).live.presence.interest.read(cx).pending()
}

/// A wheel turn of `dy` px over the member list.
fn scroll_members(cx: &TestCx, dy: f32) -> Result<()> {
    let list = probes::bounds("member-list").context("the member list isn't drawn")?;
    cx.update(move |window, cx| {
        window.dispatch_event(
            PlatformInput::Scroll(ScrollEvent {
                position: list.center(),
                delta: ScrollDelta::Pixels(point(px(0.), px(-dy))),
                modifiers: Modifiers::default(),
                touch_phase: TouchPhase::Moved,
                source: Default::default(),
            }),
            cx,
        )
    })?;
    Ok(())
}

async fn check_interest(cx: TestCx) -> Result<()> {
    let s = session(&cx).await?;
    let b_name = s.actor.peer.clone();
    let dg = "Daily Gaming";
    let b_steps = steps(&[
        "join", dg, "wait", "connected", dg, "print", "joined",
        "wait-file", &s.cue_path("dnd"), "status", "dnd", "print", "b-dnd",
        "wait-file", &s.cue_path("done"), "status", "online", "leave", dg, "logout", "wait", "logged-out",
    ]);
    let (mut b, peer, _b_profile) = s.participant(&cx, "b", &b_name, b_steps).await?;
    b.line(&cx, "MARK joined", Duration::from_secs(60)).await?;
    join_daily_gaming(&cx).await?;
    cx.update(|_, cx| actions::servers::open_server(daily_gaming(), cx))?;
    wait_probe(&cx, "member-list").await?;
    wait_for(&cx, Duration::from_secs(15), "B online in the roster", move |cx| {
        model(cx).community(daily_gaming(), cx).is_some_and(|c| c.roster.read(cx).member(peer).is_some_and(|m| m.is_online))
    })
    .await?;
    // B's row into view (the online part comes first; a few turns at most).
    let in_view = move |_: &mut App| {
        let list = probes::bounds("member-list");
        probes::bounds(&format!("member:{peer}")).is_some_and(|row| list.is_some_and(|l| l.contains(&row.center())))
    };
    for _ in 0..20 {
        if cx.update(|_, cx| in_view(cx))? && cx.update(move |_, cx| watched(peer, cx))? {
            break;
        }
        scroll_members(&cx, 240.)?;
        cx.sleep(Duration::from_millis(250)).await;
    }
    wait_for(&cx, Duration::from_secs(5), "B's row drawn and watched", move |cx| in_view(cx) && watched(peer, cx)).await?;
    wait_for(&cx, Duration::from_secs(5), "the watch set sent", |cx| interest_settled(cx)).await?;
    let drawn = probes::text_of(&format!("member-status:{peer}")).map(|t| t.to_string());
    cx.log(format_args!("B's row is drawn and B is watched; its dot reads {drawn:?}"));

    // B's status change reaches us and redraws the row's dot.
    s.cue("dnd")?;
    b.mark(&cx, "b-dnd").await?;
    let key = format!("member-status:{peer}");
    let t = wait_for(&cx, Duration::from_secs(10), "the dot to redraw as Do Not Disturb", move |_| probes::text_of(&key).is_some_and(|t| t == "dnd")).await?;
    cx.log(format_args!("B chose Do Not Disturb: the row's dot redrew {} ms after B's mark", t.as_millis()));

    // Scrolled away: the row goes, and the watch with it.
    for _ in 0..12 {
        scroll_members(&cx, 4000.)?;
        cx.sleep(Duration::from_millis(150)).await;
    }
    wait_for(&cx, Duration::from_secs(10), "B released", move |cx| !watched(peer, cx) && interest_settled(cx)).await?;
    cx.log("scrolled to the bottom of the roster: B's row is gone and B is no longer watched (the set re-sent)");
    s.cue("done")?;
    b.line(&cx, "DONE", Duration::from_secs(60)).await.ok();
    leave_daily_gaming(&cx).await?;
    sign_out(&cx).await
}

// ── people-profile (T13: the screen) ────────────────────────────────────────────────────────────

fn profile_open_for(uid: Uid, cx: &App) -> bool {
    let o = model(cx).overlays.read(cx);
    o.is_open(modal::USER_PROFILE) && o.user_profile_user == Some(uid)
}

async fn check_profile(cx: TestCx) -> Result<()> {
    let s = session(&cx).await?;
    let b_name = s.actor.peer.clone();
    let a = format!("#{}", s.me);
    let b_steps = steps(&[
        "e2ee-ready", PASSPHRASE, "wait", "e2ee", "ready", "print", "b-keys",
        "friends", "wait", "friend", &a, "pending_incoming", "print", "b-asked", "friend-accept", &a, "print", "b-accepted",
        "wait", "friend", &a, "none", "print", "b-removed",
        "wait-file", &s.cue_path("done"), "logout", "wait", "logged-out",
    ]);
    let (mut b, peer, _b_profile) = s.participant(&cx, "b", &b_name, b_steps).await?;
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    if cx.update(move |_, cx| friend_status(peer, cx))?.is_some() {
        let name = b_name.clone();
        cx.update(move |_, cx| actions::friends::act(FriendAction::Remove, peer, name.into(), cx))?;
        wait_for(&cx, Duration::from_secs(10), "an old friendship to go", move |cx| friend_status(peer, cx).is_none()).await?;
    }
    e2ee_ready(&cx).await?;
    b.mark(&cx, "b-keys").await?;

    // The popout's avatar opens the profile screen.
    cx.update(move |_, cx| decibell_common::open_profile_at(peer, point(px(500.), px(160.)), None, cx))?;
    wait_for(&cx, Duration::from_secs(5), "the popout", move |cx| model(cx).overlays.read(cx).profile_popup.as_ref().is_some_and(|p| p.uid == peer)).await?;
    quiet(&cx).await?;
    click_probe(&cx, "popup-avatar").await?;
    wait_for(&cx, Duration::from_secs(5), "the profile screen", move |cx| profile_open_for(peer, cx)).await?;
    quiet(&cx).await?;
    cx.log("the popout's avatar opened B's profile screen");

    // Its friend actions: Add friend; B accepts; Remove friend.
    click_probe(&cx, "profile-add").await?;
    wait_for(&cx, Duration::from_secs(10), "the request", move |cx| friend_status(peer, cx) == Some(FriendStatus::PendingOutgoing)).await?;
    b.mark(&cx, "b-asked").await?;
    b.mark(&cx, "b-accepted").await?;
    wait_for(&cx, Duration::from_secs(10), "the friendship", move |cx| friend_status(peer, cx).is_some_and(FriendStatus::is_accepted)).await?;
    quiet(&cx).await?;
    click_probe(&cx, "profile-remove").await?;
    wait_for(&cx, Duration::from_secs(10), "the removal", move |cx| friend_status(peer, cx).is_none()).await?;
    b.mark(&cx, "b-removed").await?;
    cx.log("the screen's Add friend and Remove friend: the request, accepted by B, removed on both sides");

    // A value selected by a double click and copied (`select-text`).
    let clipboard = ClipboardGuard::take(&cx)?;
    quiet(&cx).await?;
    let username = wait_for(&cx, Duration::from_secs(5), "the About values", |cx| profile_value("about:Username", cx).is_some()).await.map(|_| ());
    username?;
    let at = cx.update(|_, cx| profile_value("about:Username", cx).map(|state| state.read(cx).bounds()))?.context("the username field")?;
    let start = point(at.left() + px(4.), at.center().y);
    // A frame between each press and release (focus moves when the window next draws).
    for clicks in [1, 2] {
        cx.mouse_down(start, clicks, Modifiers::default())?;
        cx.sleep(Duration::from_millis(40)).await;
        cx.mouse_up(start)?;
        cx.sleep(Duration::from_millis(40)).await;
    }
    let selected = cx.update(|_, cx| profile_value("about:Username", cx).and_then(|s| s.read(cx).selected_text().map(str::to_string)))?;
    cx.keystroke("ctrl-c")?;
    cx.sleep(Duration::from_millis(200)).await;
    let copied = cx.update(|_, cx| clipboard_text(cx))?;
    cx.log(format_args!("a double click on the username selects {selected:?}; Ctrl+C copies {copied:?}"));
    ensure!(selected.as_deref() == Some(b_name.as_str()), "the double click selected {selected:?}");
    ensure!(copied.as_deref() == Some(b_name.as_str()), "the clipboard holds {copied:?}");

    // The Privacy tab, both encrypted: the safety number drawn as the core computes it; Copy.
    click_probe(&cx, "profile-tab-privacy").await?;
    let want = wait_for(&cx, Duration::from_secs(15), "the safety number", move |cx| {
        PeerInfos::existing(cx).and_then(|p| p.read(cx).get(peer, current_key(peer, cx)).map(|i| !i.safety_number.is_empty())).unwrap_or(false)
    })
    .await
    .and_then(|_| cx.update(move |_, cx| PeerInfos::existing(cx).and_then(|p| p.read(cx).get(peer, current_key(peer, cx)).map(|i| i.safety_number.to_string()))))?
    .context("no safety number")?;
    let w = want.clone();
    wait_for(&cx, Duration::from_secs(5), "the groups drawn", move |cx| drawn_safety(cx) == w).await?;
    quiet(&cx).await?;
    click_probe(&cx, "copy-safety-number").await?;
    cx.sleep(Duration::from_millis(200)).await;
    let copied = cx.update(|_, cx| clipboard_text(cx))?;
    cx.log(format_args!("the safety number drawn in its {} groups; Copy put {} characters on the clipboard", want.split(' ').count(), copied.as_ref().map_or(0, |c| c.len())));
    ensure!(copied.as_deref() == Some(want.as_str()), "Copy: {copied:?}, the number {want:?}");
    let fingerprint = cx.update(|_, cx| profile_value("fingerprint:peer", cx).map(|s| s.read(cx).value().to_string()))?;
    ensure!(fingerprint.is_some_and(|f| !f.is_empty()), "B's fingerprint isn't drawn");
    drop(clipboard);
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "the screen to close", |cx| !model(cx).overlays.read(cx).is_open(modal::USER_PROFILE)).await?;
    s.cue("done")?;
    sign_out(&cx).await
}

fn profile_value(key: &str, cx: &App) -> Option<gpui::Entity<decibell_text::input::InputState>> {
    crate::hooks::shown::<crate::profile::ProfileModal>(cx).and_then(|m| m.read(cx).value_state(key))
}

/// The safety number as the screen draws it: its groups, joined.
fn drawn_safety(cx: &App) -> String {
    (0..).map_while(|i| profile_value(&format!("safety:{i}"), cx).map(|s| s.read(cx).value().to_string())).collect::<Vec<_>>().join(" ")
}

// ── people-popout-stream (T13: the popout's live section) ───────────────────────────────────────

/// The voice tests' accounts (both have E2EE keys; MLS voice needs them).
const STREAM_VIEWER: &str = "perf3";
const STREAMER: &str = "perf4";
const VOICE_PASSPHRASE: &str = "decibell-m3-test-passphrase";
const SINK_ENV: &str = "DECIBELL_CUE_SINK";

/// A `.dbv` recording above the binary (`vendor/streams`).
fn stream_file() -> Option<PathBuf> {
    let exe = std::env::current_exe().ok()?;
    exe.ancestors().map(|d| d.join("vendor/streams/1080p60_h264.dbv")).find(|p| p.exists())
}

fn stack_uid(name: &str) -> Result<Uid> {
    let text = std::fs::read_to_string(dev_dir().join("run/accounts.json"))?;
    let needle = format!("\"name\": \"{name}\"");
    let at = text.find(&needle).with_context(|| format!("{name} is not in accounts.json"))?;
    let rest = &text[at..];
    let uid_at = rest.find("\"uid\"").context("no uid")? + 5;
    let digits: String = rest[uid_at..].chars().skip_while(|c| !c.is_ascii_digit()).take_while(|c| c.is_ascii_digit()).collect();
    Ok(digits.parse()?)
}

/// The app as perf3, its sound on a null sink this parent owns (unloaded after the child has gone:
/// nothing is heard and the real microphone stays closed, as in the voice tests).
async fn popout_stream_parent(cx: TestCx) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    if headless_exe().is_none() || stream_file().is_none() {
        cx.log("SKIP: decibell-headless or vendor/streams/1080p60_h264.dbv is not next to this binary");
        return Ok(());
    }
    let sink = match decibell_harness::test_audio::NullSink::load("") {
        Ok(sink) => sink,
        Err(e) => {
            cx.log(format_args!("SKIP: no null sink ({e:#}): joining voice would open the desktop's devices"));
            return Ok(());
        }
    };
    let profile = TempDir::new("people-popout-stream")?;
    let child = spawn("home-people-popout-stream", "perf3", Some(STREAM_VIEWER), &stack, &profile.0, &[(SINK_ENV, sink.name().to_string())]);
    let result = match child {
        Ok(child) => finish(&cx, vec![child]).await,
        Err(e) => Err(e),
    };
    let unloaded = sink.unload();
    match result? {
        Outcome::Passed => {
            unloaded?;
            Ok(())
        }
        Outcome::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

async fn check_popout_stream(cx: TestCx) -> Result<()> {
    use decibell_model::live::voice::session::VoiceE2eePhase;
    let sink = std::env::var(SINK_ENV).context("no null sink")?;
    let file = stream_file().context("the recording")?;
    let central = std::env::var("DECIBELL_CENTRAL").context("DECIBELL_CENTRAL")?;
    signed_in(&cx).await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(Some(true), cx))?;
    connected(&cx, daily_gaming()).await?;
    // The keys (MLS voice needs them), then the devices on the sink, muted and deafened.
    let started = Instant::now();
    loop {
        let (status, busy) = cx.update(|_, cx| {
            let e = model(cx).live.e2ee.read(cx);
            (e.status, e.passphrase.as_ref().is_some_and(|f| f.busy))
        })?;
        match status {
            E2eeStatus::Ready => break,
            E2eeStatus::Locked | E2eeStatus::NotSetUp if !busy => {
                let mode = if status == E2eeStatus::Locked { PassphraseMode::Unlock } else { PassphraseMode::Setup };
                cx.update(move |_, cx| {
                    model(cx).live.e2ee.update(cx, |e, cx| e.open(mode, cx));
                    actions::e2ee::submit_passphrase(VOICE_PASSPHRASE.into(), VOICE_PASSPHRASE.into(), cx);
                })?;
            }
            _ => {}
        }
        ensure!(started.elapsed() < Duration::from_secs(30), "the E2EE keys never became ready ({status:?})");
        cx.sleep(Duration::from_millis(100)).await;
    }
    wait_for(&cx, Duration::from_secs(10), "the passphrase dialog to close", |cx| model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    cx.sleep(Duration::from_millis(400)).await;
    {
        let sink = sink.clone();
        cx.update(move |_, cx| {
            actions::voice::set_input_device(Some(format!("{sink}.monitor")), cx);
            actions::voice::set_output_device(Some(sink), cx);
            actions::voice::set_mute(true, cx);
            actions::voice::set_deafen(true, cx);
        })?;
    }

    // perf4 streams the recording in Lounge.
    let streamer = stack_uid(STREAMER)?;
    let streamer_profile = TempDir::new("people-streamer")?;
    let stop = streamer_profile.0.join("stop");
    let dg = "Daily Gaming";
    let steps = steps(&[
        "login", STREAMER, "wait", "connected", dg, "open-server", dg, "e2ee-ready", VOICE_PASSPHRASE, "mute", "deafen",
        "voice-join", "Lounge", "wait", "mls", "ready", "stream-file", &file.display().to_string(), "print", "streaming",
        "wait-file", &stop.display().to_string(), "stream-stop", "voice-leave", "logout", "wait", "logged-out",
    ]);
    let mut peer = Headless::start("streamer", &streamer_profile.0, &central, steps)?;
    peer.line(&cx, "STREAMING", Duration::from_secs(90)).await?;
    let t = wait_for(&cx, Duration::from_secs(20), "the stream in our model", move |cx| model(cx).live.voice.streams.read(cx).of_user(streamer).is_some()).await?;
    cx.log(format_args!("perf4 streams in Lounge (known here {} ms after it started)", t.as_millis()));

    // The popout of perf4, from Daily Gaming: the live section.
    cx.update(move |_, cx| {
        actions::servers::open_server(daily_gaming(), cx);
        decibell_common::open_profile_at(streamer, point(px(500.), px(160.)), Some(daily_gaming()), cx);
    })?;
    wait_for(&cx, Duration::from_secs(10), "the live section", |_| !crate::hooks::drawn("profile-popup-stream").is_empty()).await?;
    let drawn = crate::hooks::drawn("profile-popup-stream");
    cx.log(format_args!("the popout's live section: {drawn:?}"));
    ensure!(drawn.first().is_some_and(|c| c == "#Lounge"), "the section names {drawn:?}, not #Lounge");
    ensure!(drawn.get(1).is_some_and(|t| t == "Watch stream"), "the preview isn't watchable here: {drawn:?}");
    quiet(&cx).await?;

    // Its click: join Lounge, watch perf4 large.
    click_probe(&cx, "popup-stream").await?;
    let t = wait_for(&cx, Duration::from_secs(30), "Lounge joined and the stream focused", move |cx| {
        let m = model(cx);
        let s = m.live.voice.session.read(cx);
        let joined = s.connected().is_some_and(|(server, channel)| server == daily_gaming() && m.community(server, cx).is_some_and(|c| c.channels.read(cx).get(&channel).is_some_and(|ch| &*ch.name == "Lounge")))
            && s.e2ee.as_ref().is_some_and(|e| e.phase == VoiceE2eePhase::Ready);
        joined && m.live.voice.watching.read(cx).focused == Some(streamer) && m.nav.read(cx).active_view == ActiveView::Voice
    })
    .await?;
    let closed = cx.update(|_, cx| model(cx).overlays.read(cx).profile_popup.is_none())?;
    cx.log(format_args!("the click joined Lounge (MLS ready) and focused perf4's stream {} ms later; the popout closed: {closed}", t.as_millis()));
    ensure!(closed, "the popout stayed open");
    let streams = decibell_harness::test_audio::ensure_only_on(&sink).context("the session's devices")?;
    cx.log(format_args!("the session's streams are on the null sink: {streams:?}"));

    cx.update(|_, cx| actions::voice::leave_voice_channel(cx))?;
    wait_for(&cx, Duration::from_secs(15), "leaving Lounge", |cx| model(cx).live.voice.session.read(cx).connected().is_none()).await?;
    std::fs::write(&stop, b"")?;
    peer.line(&cx, "DONE", Duration::from_secs(60)).await.ok();
    sign_out(&cx).await
}
