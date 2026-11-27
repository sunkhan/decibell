//! The conversation flows the first round of tests left out (the parity audit's T20–T23, and the
//! profile popout's quick DM), each between throwaway accounts a `decibell-headless` registers:
//! this app signs in as A, headless clients are the other people (and A's other devices).
//!
//! - `dm-quick-send`: the popout's quick DM while that conversation is scrolled back in history
//!   (windowed): it goes out with no bubble, the popup closes and the conversation opens, B gets it.
//! - `dm-list-order`: the DM list's drawn order and previews follow each new message (three
//!   accounts: B and C write, then A).
//! - `dm-delete-edit`: delete through the row's Delete (the confirmation, Enter) and with Shift
//!   (no confirmation), each gone on B's side too; a delete and an edit central refuses (B's
//!   message): the row comes back, the toasts carry central's text.
//! - `dm-friends-only`: B takes DMs from friends only; A's composer send to B is withdrawn with
//!   "Message not sent" and central's text, and a call to B is refused ("Can't call").
//! - `dm-e2ee-keys`: the set-up nudge dismissed; Settings → Privacy's Change passphrase (a fresh
//!   device unlocks with the new one) and Reset keys (A's other device locks, B sees the change);
//!   B resetting: the "keys changed" notice here, dismissed; A's keys reset on another device:
//!   locked here, the quick DM and the composer refused for B (pinned, never in plaintext) until
//!   unlocked; B's keys gone from central (deleted from the local stack's database, B being a
//!   throwaway account): the send refused, never plaintext.

use std::path::Path;

use decibell_model::live::calls::CallStatus;
use decibell_model::stores::toasts::Severity;
use gpui::{Modifiers, MouseButton, MouseDownEvent, MouseUpEvent, PlatformInput};

use super::read_rules::{Headless, headless_exe, register};
use super::*;

/// The parent: `accounts` throwaway accounts (registered by headless clients), then the app as
/// the first, its check `home-<check>`.
pub(super) async fn parent(cx: TestCx, check: &'static str, accounts: usize) -> Result<()> {
    let Some(stack) = stack()? else {
        cx.log("SKIP: the local stack is down (tools/dev/local-stack.sh up)");
        return Ok(());
    };
    if headless_exe().is_none() {
        cx.log("SKIP: decibell-headless is not built next to this binary (cargo build --release -p decibell-model --features local-central --bin decibell-headless)");
        return Ok(());
    }
    let tag = format!("{:x}", std::process::id() ^ (std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_secs() as u32)));
    let names: Vec<String> = ["a", "b", "c"].iter().take(accounts).map(|r| format!("ht{tag}{r}")).collect();
    let password = format!("home-test-{tag}-pw");
    cx.log(format_args!("registering the throwaway accounts {} (decibell-headless)", names.join(", ")));
    for name in &names {
        register(&cx, &stack, name, &password).await?;
    }
    let cues = TempDir::new(&format!("{check}-cues"))?;
    let app_profile = TempDir::new(&format!("{check}-a"))?;
    let mut env = actor_env(check, "a", &names[0], &names[1], &password, &cues.0);
    if let Some(third) = names.get(2) {
        env.push(("DECIBELL_HOME_THIRD", third.clone()));
    }
    let child = spawn(&format!("home-{check}"), "a", None, &stack, &app_profile.0, &env)?;
    match finish(&cx, vec![child]).await? {
        Outcome::Passed => Ok(()),
        Outcome::NoDevLogin => {
            cx.log("SKIP: this binary has no --login (build with --features dev-local)");
            Ok(())
        }
    }
}

// ── shared by the checks ─────────────────────────────────────────────────────────────────────

/// Signed in as A, the window held "active" (a conversation is read only in a focused window;
/// other windows share this desktop).
async fn begin(cx: &TestCx) -> Result<(Actor, Uid, String)> {
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
    cx.log(format_args!("this app is {} (uid {me})", actor.me));
    Ok((actor, me, central))
}

/// Something unique to this run, for message texts.
fn run_tag() -> String {
    format!("{:x}", std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.subsec_nanos()))
}

fn strings(list: &[&str]) -> Vec<String> {
    list.iter().map(|s| s.to_string()).collect()
}

/// A headless client signed in as `name` that prints its uid (`DUMP auth`) and `MARK ready`,
/// then runs `script`; returns it with that uid.
async fn person(cx: &TestCx, label: &'static str, profile: &Path, central: &str, name: &str, password: &str, script: Vec<String>) -> Result<(Headless, Uid)> {
    let mut steps = strings(&["login-as", name, password, "wait", "logged-in", "dump", "auth", "print", "ready"]);
    steps.extend(script);
    let mut client = Headless::start(label, profile, central, steps)?;
    let auth = client.line(cx, "DUMP auth", Duration::from_secs(60)).await?;
    let uid: Uid = auth
        .split("\"uid\":")
        .nth(1)
        .and_then(|rest| rest.split(|c: char| !c.is_ascii_digit()).next())
        .and_then(|digits| digits.parse().ok())
        .with_context(|| format!("{label}'s uid in its auth dump"))?;
    client.mark(cx, "ready").await?;
    Ok((client, uid))
}

fn cue_path(actor: &Actor, name: &str) -> String {
    actor.cues.join(name).display().to_string()
}

/// Open the conversation with `peer` and wait until it is the one shown.
async fn open_conversation(cx: &TestCx, peer: Uid, name: &str) -> Result<()> {
    let name = name.to_string();
    cx.update(move |_, cx| actions::dm::open_dm(peer, name.into(), cx))?;
    wait_for(cx, Duration::from_secs(10), "the conversation", move |cx| crate::hooks::shown::<Conversation>(cx).is_some_and(|c| c.read(cx).peer() == peer)).await?;
    Ok(())
}

fn shown_list(cx: &App) -> Option<gpui::Entity<decibell_chat::MessageList>> {
    crate::hooks::shown::<Conversation>(cx).map(|c| c.read(cx).list().clone())
}

fn row_id(peer: Uid, text: &str, cx: &App) -> Option<i64> {
    conversation_rows(peer, cx).iter().find(|m| m.id > 0 && &*m.content == text).map(|m| m.id)
}

fn rows_with(peer: Uid, text: &str, cx: &App) -> usize {
    conversation_rows(peer, cx).iter().filter(|m| &*m.content == text).count()
}

/// The newest toast titled `title`, as (severity, body).
fn toast(title: &str, cx: &App) -> Option<(Severity, String)> {
    model(cx).toasts.read(cx).toasts().iter().rev().find(|t| &*t.title == title).map(|t| (t.severity, t.body.as_ref().map(|b| b.to_string()).unwrap_or_default()))
}

/// Our send's text is back in the conversation's message box (and its inline error, if any).
fn draft_and_error(peer: Uid, cx: &App) -> (String, Option<String>) {
    let m = model(cx);
    let draft = m.chat.drafts.read(cx).get(&decibell_model::chat::DraftKey::Dm(peer)).to_string();
    let error = m.dm.list.read(cx).send_error.as_ref().filter(|(p, _)| *p == peer).map(|(_, e)| e.to_string());
    (draft, error)
}

/// The profile popout for `peer`, its quick-DM field focused.
async fn open_popout(cx: &TestCx, peer: Uid) -> Result<()> {
    cx.update(move |_, cx| decibell_common::open_profile_at(peer, point(px(420.), px(160.)), None, cx))?;
    wait_for(cx, Duration::from_secs(5), "the popout", move |cx| model(cx).overlays.read(cx).profile_popup.as_ref().is_some_and(|p| p.uid == peer)).await?;
    // Its entrance (250 ms); the field's caret blinks from then on, so no wait for quiet.
    cx.sleep(Duration::from_millis(400)).await;
    Ok(())
}

fn popout(cx: &App) -> Option<(bool, String)> {
    let shown = model(cx).overlays.read(cx).profile_popup.is_some();
    crate::hooks::shown::<ProfilePopup>(cx).filter(|_| shown).map(|p| {
        let p = p.read(cx);
        (p.sending(), p.quick_dm_text(cx))
    })
}

// ── dm-quick-send ────────────────────────────────────────────────────────────────────────────

pub(super) async fn check_quick_send(cx: TestCx) -> Result<()> {
    let (actor, me, central) = begin(&cx).await?;
    let b_name = actor.peer.clone();
    let run = run_tag();
    // Enough history that the oldest isn't in the newest page (50) and that a window around it,
    // with the page of newer messages the list loads under it (25 + 50), still ends short of the
    // present: the list stays scrolled back.
    const HALF: usize = 55;
    let seed = |who: &str, n: usize| format!("seed {run} {who}{n}");
    let quick = format!("quick DM while scrolled back, run {run}");
    let b_profile = TempDir::new("quick-b")?;
    let mut script = strings(&["wait-file", &cue_path(&actor, "seed")]);
    for n in 1..=HALF {
        script.extend(["dm-send".to_string(), format!("uid:{me}"), seed("b", n)]);
    }
    script.extend(["wait-dm".to_string(), format!("uid:{me}"), seed("b", HALF), "print".into(), "seeded".into()]);
    script.extend(["wait-dm".to_string(), format!("uid:{me}"), quick.clone(), "print".into(), "got-quick".into()]);
    script.extend(strings(&["wait-file", &cue_path(&actor, "done"), "logout", "wait", "logged-out"]));
    let (mut b, peer) = person(&cx, "b", &b_profile.0, &central, &b_name, &actor.password, script).await?;

    open_conversation(&cx, peer, &b_name).await?;
    actor.cue("seed", 1)?;
    for n in 1..=HALF {
        let text = seed("a", n);
        cx.update(move |_, cx| {
            actions::dm::send_dm(peer, &text, 0, cx);
        })?;
    }
    b.mark(&cx, "seeded").await?;
    {
        let (last_a, last_b) = (seed("a", HALF), seed("b", HALF));
        wait_for(&cx, Duration::from_secs(45), "both halves of the history", move |cx| has_echo(peer, &last_a, cx) && has_echo(peer, &last_b, cx)).await?;
    }
    let oldest = cx.update(move |_, cx| conversation_rows(peer, cx).iter().filter(|m| m.id > 0).map(|m| m.id).min())?.context("no rows")?;
    // Scrolled back in history: the newest page, then a jump to the oldest message (not in it)
    // as a reply's link would: the list holds a window around it, the present not loaded.
    cx.update(move |_, cx| {
        actions::dm::jump_to_present(peer, cx);
    })?;
    wait_for(&cx, Duration::from_secs(15), "the newest page again", move |cx| {
        model(cx).dm.list.read(cx).slice(peer).is_some_and(|s| s.read(cx).fetched) && row_id_any(peer, oldest, cx).is_none()
    })
    .await?;
    cx.update(move |_, cx| {
        if let Some(list) = shown_list(cx) {
            list.update(cx, |list, cx| list.jump_to_message(oldest, cx));
        }
    })?;
    let windowed = move |cx: &mut App| model(cx).dm.list.read(cx).slice(peer).is_some_and(|s| s.read(cx).list_windowed());
    let landed = wait_for(&cx, Duration::from_secs(15), "the window around the oldest message", move |cx| windowed(cx) && row_id_any(peer, oldest, cx).is_some()).await;
    if let Err(e) = landed {
        let state = cx.update(move |_, cx| slice_state(peer, cx))?;
        bail!("{e:#} (oldest {oldest}; the conversation: {state})");
    }
    cx.settle().await?;
    cx.log(format_args!("{} messages; the conversation is scrolled back to the oldest (windowed, the present not loaded)", HALF * 2));

    // The popout's quick DM: typed, Enter.
    open_popout(&cx, peer).await?;
    cx.type_text(&quick)?;
    cx.keystroke("enter")?;
    let (sending, bubble) = {
        let q = quick.clone();
        cx.update(move |_, cx| (popout(cx).is_none_or(|(sending, _)| sending), conversation_rows(peer, cx).iter().any(|m| m.pending && *m.content == q)))?
    };
    ensure!(!bubble, "the quick DM put a bubble in the conversation (the renderer sends it without one)");
    cx.log(format_args!("Enter: on its way (sending {sending}), no bubble in the conversation"));
    wait_for(&cx, Duration::from_secs(10), "the popout to close and the conversation to open", move |cx| {
        let m = model(cx);
        m.overlays.read(cx).profile_popup.is_none() && m.nav.read(cx).active_view == ActiveView::Dm && m.dm.list.read(cx).active_peer == Some(peer)
    })
    .await?;
    b.mark(&cx, "got-quick").await?;
    cx.log("sent: the popout closed, the conversation is open, B received it");
    // Back at the present it is there, once.
    cx.update(|_, cx| {
        if let Some(list) = shown_list(cx) {
            list.update(cx, |list, cx| list.jump_to_present(cx));
        }
    })?;
    {
        let q = quick.clone();
        wait_for(&cx, Duration::from_secs(15), "the quick DM at the present", move |cx| has_echo(peer, &q, cx) && !windowed(cx)).await?;
    }
    let copies = cx.update({
        let q = quick.clone();
        move |_, cx| rows_with(peer, &q, cx)
    })?;
    ensure!(copies == 1, "the quick DM shows {copies} times");
    // Opened again, the field is empty.
    open_popout(&cx, peer).await?;
    let field = cx.update(|_, cx| popout(cx).map(|(_, text)| text))?;
    ensure!(field.as_deref() == Some(""), "the reopened popout's field holds {field:?}");
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "Escape to close it", |cx| model(cx).overlays.read(cx).profile_popup.is_none()).await?;
    cx.log("at the present: the quick DM once; the popout opens empty again");

    actor.cue("done", 1)?;
    b.line(&cx, "DONE", Duration::from_secs(30)).await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    sign_out(&cx).await
}

/// What a conversation's slice holds, for a failure's message.
fn slice_state(peer: Uid, cx: &App) -> String {
    let Some(slice) = model(cx).dm.list.read(cx).slice(peer).cloned() else { return "no slice".into() };
    let s = slice.read(cx);
    let ids: Vec<i64> = s.rows().iter().map(|m| m.id).filter(|id| *id > 0).collect();
    format!(
        "{} rows (ids {:?}..{:?}), fetched {}, loading {}, has_more_before {}, has_more_after {}, resyncing {}",
        s.rows().len(),
        ids.first(),
        ids.last(),
        s.fetched,
        s.loading,
        s.has_more_before,
        s.has_more_after,
        s.resyncing
    )
}

fn row_id_any(peer: Uid, id: i64, cx: &App) -> Option<i64> {
    conversation_rows(peer, cx).iter().find(|m| m.id == id).map(|m| m.id)
}

// ── dm-list-order ────────────────────────────────────────────────────────────────────────────

/// The DM list as drawn: (peer, preview) top to bottom (`DmRows` records it with probes on).
fn drawn_rows() -> Vec<(Uid, String)> {
    probes::text_of("dm-rows")
        .map(|t| {
            t.lines()
                .filter_map(|line| {
                    let (uid, preview) = line.split_once('\t')?;
                    Some((uid.parse().ok()?, preview.to_string()))
                })
                .collect()
        })
        .unwrap_or_default()
}

/// The list's drawn order and previews are `want` (peer, preview), top first, and the rows' probes
/// sit in that order on screen.
async fn list_is(cx: &TestCx, what: &str, want: Vec<(Uid, String)>) -> Result<()> {
    let expected = want.clone();
    let result = wait_for(cx, Duration::from_secs(10), what, move |_| {
        let drawn = drawn_rows();
        drawn.len() >= expected.len() && drawn[..expected.len()] == expected[..]
    })
    .await;
    if let Err(e) = result {
        bail!("{e:#}: drawn {:?}", drawn_rows());
    }
    let ys: Vec<f32> = want.iter().filter_map(|(uid, _)| probes::bounds(&format!("dm-row:{uid}")).map(|b| f32::from(b.top()))).collect();
    ensure!(ys.len() == want.len() && ys.windows(2).all(|w| w[0] < w[1]), "{what}: the rows sit at {ys:?} (top first expected)");
    cx.log(format_args!("{what}: {:?}", want.iter().map(|(_, p)| p.as_str()).collect::<Vec<_>>()));
    Ok(())
}

/// Past the second the previous message was stamped in.
const NEXT_SECOND: Duration = Duration::from_millis(1100);

pub(super) async fn check_list_order(cx: TestCx) -> Result<()> {
    let (actor, me, central) = begin(&cx).await?;
    let (b_name, c_name) = (actor.peer.clone(), std::env::var("DECIBELL_HOME_THIRD").context("DECIBELL_HOME_THIRD")?);
    let run = run_tag();
    let (c1, b1, c2, a1) = (
        format!("first, from C ({run})"),
        format!("then **B** writes ({run})"),
        format!("C again ({run})"),
        format!("A answers B ({run})"),
    );
    let send = |cue: &str, text: &str, mark: &str| -> Vec<String> {
        vec!["wait-file".into(), cue_path(&actor, cue), "dm-send".into(), format!("uid:{me}"), text.into(), "wait-dm".into(), format!("uid:{me}"), text.into(), "print".into(), mark.into()]
    };
    let tail = strings(&["wait-file", &cue_path(&actor, "done"), "logout", "wait", "logged-out"]);
    let (b_profile, c_profile) = (TempDir::new("order-b")?, TempDir::new("order-c")?);
    let mut b_script = send("b1", &b1, "b1");
    b_script.extend(["wait-dm".to_string(), format!("uid:{me}"), a1.clone(), "print".into(), "got-a1".into()]);
    b_script.extend(tail.clone());
    let mut c_script = send("c1", &c1, "c1");
    c_script.extend(send("c2", &c2, "c2"));
    c_script.extend(tail);
    let (mut b, b_uid) = person(&cx, "b", &b_profile.0, &central, &b_name, &actor.password, b_script).await?;
    let (mut c, c_uid) = person(&cx, "c", &c_profile.0, &central, &c_name, &actor.password, c_script).await?;
    // Home: the conversation list beside the Friends page.
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    park_pointer(&cx)?;

    actor.cue("c1", 1)?;
    c.mark(&cx, "c1").await?;
    list_is(&cx, "C wrote", vec![(c_uid, c1.clone())]).await?;
    // Message times are whole seconds and a tie keeps the lower uid first (both clients sort by
    // activity, then uid): each message goes out in a later second than the one before.
    cx.sleep(NEXT_SECOND).await;
    actor.cue("b1", 1)?;
    b.mark(&cx, "b1").await?;
    // The preview is the message as plain text (its markup gone), as `MessageText preview`.
    let b1_plain = b1.replace("**", "");
    list_is(&cx, "B wrote: B on top", vec![(b_uid, b1_plain.clone()), (c_uid, c1.clone())]).await?;
    cx.sleep(NEXT_SECOND).await;
    actor.cue("c2", 1)?;
    c.mark(&cx, "c2").await?;
    list_is(&cx, "C wrote again: C on top, its preview the new message", vec![(c_uid, c2.clone()), (b_uid, b1_plain.clone())]).await?;
    // Our own message moves its conversation up too, its text the preview.
    cx.sleep(NEXT_SECOND).await;
    open_conversation(&cx, b_uid, &b_name).await?;
    click_probe(&cx, "dm-composer").await?;
    cx.type_text(&a1)?;
    cx.keystroke("enter")?;
    b.mark(&cx, "got-a1").await?;
    list_is(&cx, "A answered B: B on top again with A's text", vec![(b_uid, a1.clone()), (c_uid, c2.clone())]).await?;

    actor.cue("done", 1)?;
    b.line(&cx, "DONE", Duration::from_secs(30)).await?;
    c.line(&cx, "DONE", Duration::from_secs(30)).await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    sign_out(&cx).await
}

// ── dm-delete-edit ───────────────────────────────────────────────────────────────────────────

/// Hover row `id`, then its Delete action, clicked with `modifiers` (Shift skips the confirmation).
async fn click_delete(cx: &TestCx, id: i64, modifiers: Modifiers) -> Result<()> {
    let body = cx.update(move |_, cx| shown_list(cx).and_then(|l| l.read(cx).body_point(id, 0.5)))?.with_context(|| format!("row {id}'s text on screen"))?;
    cx.mouse_move(body)?;
    cx.settle().await?;
    let at = cx
        .update(move |_, cx| shown_list(cx).and_then(|l| l.read(cx).action_point(id, decibell_chat::row::RowAction::Delete, cx)))?
        .with_context(|| format!("row {id}'s Delete action"))?;
    cx.mouse_move(at)?;
    cx.update(move |window, cx| {
        window.dispatch_event(PlatformInput::MouseDown(MouseDownEvent { button: MouseButton::Left, position: at, modifiers, click_count: 1, first_mouse: false }), cx);
    })?;
    // A frame between press and release (CONVENTIONS, GPUI gotcha 14).
    cx.sleep(Duration::from_millis(30)).await;
    cx.update(move |window, cx| {
        window.dispatch_event(PlatformInput::MouseUp(MouseUpEvent { button: MouseButton::Left, position: at, modifiers, click_count: 1 }), cx);
    })?;
    park_pointer(cx)
}

fn confirming(cx: &App) -> Option<i64> {
    crate::hooks::shown::<Conversation>(cx).and_then(|c| c.read(cx).confirming_delete())
}

fn deletions_pending(peer: Uid, cx: &App) -> usize {
    model(cx).dm.list.read(cx).slice(peer).map_or(0, |s| s.read(cx).pending_deletion_count())
}

pub(super) async fn check_delete_edit(cx: TestCx) -> Result<()> {
    let (actor, me, central) = begin(&cx).await?;
    let b_name = actor.peer.clone();
    let run = run_tag();
    let (m1, m2, m3, theirs) =
        (format!("delete me with the confirmation ({run})"), format!("delete me with Shift ({run})"), format!("I stay ({run})"), format!("B's own message ({run})"));
    let b_profile = TempDir::new("delete-b")?;
    let u = format!("uid:{me}");
    let script: Vec<String> = [
        vec!["wait-file".to_string(), cue_path(&actor, "open"), "dm-open".into(), u.clone(), "wait-dm".into(), u.clone(), m3.clone(), "print".into(), "has-all".into()],
        vec!["wait-dm-gone".to_string(), u.clone(), m1.clone(), "print".into(), "gone1".into()],
        vec!["wait-dm-gone".to_string(), u.clone(), m2.clone(), "print".into(), "gone2".into()],
        vec!["dm-send".to_string(), u.clone(), theirs.clone(), "wait-dm".into(), u.clone(), theirs.clone(), "print".into(), "sent-theirs".into()],
        vec!["wait-file".to_string(), cue_path(&actor, "done"), "dump".into(), "dm".into(), u.clone(), "logout".into(), "wait".into(), "logged-out".into()],
    ]
    .concat();
    let (mut b, peer) = person(&cx, "b", &b_profile.0, &central, &b_name, &actor.password, script).await?;
    open_conversation(&cx, peer, &b_name).await?;
    for text in [&m1, &m2, &m3] {
        let text = text.clone();
        cx.update(move |_, cx| {
            actions::dm::send_dm(peer, &text, 0, cx);
        })?;
    }
    {
        let (a, c) = (m1.clone(), m3.clone());
        wait_for(&cx, Duration::from_secs(15), "our three messages", move |cx| has_echo(peer, &a, cx) && has_echo(peer, &c, cx)).await?;
    }
    actor.cue("open", 1)?;
    b.mark(&cx, "has-all").await?;
    cx.settle().await?;
    let id = |text: &String| {
        let text = text.clone();
        cx.update(move |_, cx| row_id(peer, &text, cx))
    };
    let (id1, id2) = (id(&m1)?.context("m1")?, id(&m2)?.context("m2")?);

    // The row's Delete: the confirmation opens; Enter deletes.
    click_delete(&cx, id1, Modifiers::default()).await?;
    wait_for(&cx, Duration::from_secs(5), "the delete confirmation", move |cx| confirming(cx) == Some(id1)).await?;
    cx.settle().await?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(5), "the row to go", move |cx| row_id_any(peer, id1, cx).is_none() && confirming(cx).is_none()).await?;
    wait_for(&cx, Duration::from_secs(10), "central's confirmation", move |cx| deletions_pending(peer, cx) == 0).await?;
    b.mark(&cx, "gone1").await?;
    cx.log("Delete → the confirmation → Enter: gone here, and on B's side");

    // Shift + Delete: no confirmation.
    click_delete(&cx, id2, Modifiers { shift: true, ..Default::default() }).await?;
    wait_for(&cx, Duration::from_secs(5), "the row to go at once", move |cx| row_id_any(peer, id2, cx).is_none()).await?;
    ensure!(cx.update(|_, cx| confirming(cx))?.is_none(), "Shift+Delete asked for a confirmation");
    wait_for(&cx, Duration::from_secs(10), "central's confirmation", move |cx| deletions_pending(peer, cx) == 0).await?;
    b.mark(&cx, "gone2").await?;
    cx.log("Shift+Delete: gone without a confirmation, and on B's side");

    // Central refuses a delete and an edit of a message that isn't ours (the views never offer
    // them for one; the actions are what they would call).
    b.mark(&cx, "sent-theirs").await?;
    {
        let t = theirs.clone();
        wait_for(&cx, Duration::from_secs(10), "B's message", move |cx| has_echo(peer, &t, cx)).await?;
    }
    let their_id = id(&theirs)?.context("B's message")?;
    cx.update(move |_, cx| actions::dm::delete_dm(peer, their_id, cx))?;
    wait_for(&cx, Duration::from_secs(10), "central's refusal to put the row back", move |cx| {
        row_id_any(peer, their_id, cx).is_some() && deletions_pending(peer, cx) == 0 && toast("Couldn't delete message", cx).is_some()
    })
    .await?;
    let (severity, body) = cx.update(|_, cx| toast("Couldn't delete message", cx))?.context("the toast")?;
    ensure!(severity == Severity::Error && body == "Message not found or not deletable.", "the delete refusal's toast: {severity:?} {body:?}");
    cx.log(format_args!("a delete central refused: the row is back, toast \"Couldn't delete message\" / {body:?}"));
    cx.update(move |_, cx| actions::dm::edit_dm(peer, their_id, "an edit central refuses", cx))?;
    wait_for(&cx, Duration::from_secs(10), "the edit's refusal", |cx| toast("Couldn't edit message", cx).is_some()).await?;
    let (severity, body) = cx.update(|_, cx| toast("Couldn't edit message", cx))?.context("the toast")?;
    ensure!(severity == Severity::Error && body == "Message not found or not editable.", "the edit refusal's toast: {severity:?} {body:?}");
    let kept = {
        let t = theirs.clone();
        cx.update(move |_, cx| has_echo(peer, &t, cx))?
    };
    ensure!(kept, "the refused edit changed the row");
    cx.log(format_args!("an edit central refused: toast \"Couldn't edit message\" / {body:?}, the row unchanged"));

    actor.cue("done", 1)?;
    let dump = b.line(&cx, "DUMP dm", Duration::from_secs(30)).await?;
    ensure!(!dump.contains(&m1) && !dump.contains(&m2) && dump.contains(&m3), "B's conversation after the deletes: {dump}");
    b.line(&cx, "DONE", Duration::from_secs(30)).await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    sign_out(&cx).await
}

// ── dm-friends-only ──────────────────────────────────────────────────────────────────────────

pub(super) async fn check_friends_only(cx: TestCx) -> Result<()> {
    let (actor, me, central) = begin(&cx).await?;
    let b_name = actor.peer.clone();
    let run = run_tag();
    let text = format!("hello, stranger ({run})");
    let b_profile = TempDir::new("friends-only-b")?;
    let script = strings(&["dm-privacy", "friends", "print", "private", "wait-file", &cue_path(&actor, "done"), "dump", "dm", &format!("uid:{me}"), "logout", "wait", "logged-out"]);
    let (mut b, peer) = person(&cx, "b", &b_profile.0, &central, &b_name, &actor.password, script).await?;
    b.mark(&cx, "private").await?;
    open_conversation(&cx, peer, &b_name).await?;
    click_probe(&cx, "dm-composer").await?;
    cx.type_text(&text)?;
    let watch = {
        let seen = std::rc::Rc::new(std::cell::Cell::new(false));
        let t = text.clone();
        let flag = seen.clone();
        let sub = cx.update(move |_, cx| {
            let slice = model(cx).dm.list.read(cx).slice(peer).cloned();
            slice.map(|slice| cx.observe(&slice, move |s, cx| flag.set(flag.get() || s.read(cx).rows().iter().any(|m| m.pending && *m.content == t))))
        })?;
        (seen, sub)
    };
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(10), "central's refusal", |cx| toast("Message not sent", cx).is_some()).await?;
    let (severity, body) = cx.update(|_, cx| toast("Message not sent", cx))?.context("the toast")?;
    let left = {
        let t = text.clone();
        cx.update(move |_, cx| rows_with(peer, &t, cx))?
    };
    cx.log(format_args!("the send to a friends-only stranger: bubble shown {}, then withdrawn ({left} left); toast {severity:?} \"Message not sent\" / {body:?}", watch.0.get()));
    ensure!(watch.0.get() || watch.1.is_none(), "no bubble showed for the send");
    ensure!(left == 0, "the refused message is still in the conversation");
    ensure!(
        severity == Severity::Error && body == "This user only accepts direct messages from users in their friends list.",
        "the refusal's toast: {severity:?} {body:?}"
    );
    drop(watch);

    // A call to them: central answers NOT_ALLOWED.
    let calls = |cx: &App| {
        let c = model(cx).live.calls.read(cx);
        (c.status, c.end_reason.as_ref().map(|r| r.to_string()))
    };
    wait_for(&cx, Duration::from_secs(10), "calls to be offered", |cx| model(cx).live.calls.read(cx).call_signaling).await?;
    cx.update(move |_, cx| actions::calls::start_call(peer, cx))?;
    wait_for(&cx, Duration::from_secs(15), "the call's refusal", move |cx| toast("Can't call", cx).is_some() && calls(cx).0 == CallStatus::Idle).await?;
    let (severity, body) = cx.update(|_, cx| toast("Can't call", cx))?.context("the toast")?;
    let reason = cx.update(move |_, cx| calls(cx).1)?;
    cx.log(format_args!("a call to them: {severity:?} \"Can't call\" / {body:?}, ended: {reason:?}"));
    ensure!(severity == Severity::Warning && body == format!("{b_name} only takes calls from friends."), "the call refusal's toast: {severity:?} {body:?}");
    ensure!(reason.as_deref() == Some("Not allowed"), "the call ended with {reason:?}");

    actor.cue("done", 1)?;
    let dump = b.line(&cx, "DUMP dm", Duration::from_secs(30)).await?;
    ensure!(!dump.contains(&text), "B received the refused message: {dump}");
    b.line(&cx, "DONE", Duration::from_secs(30)).await?;
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    sign_out(&cx).await
}

// ── dm-e2ee-keys ─────────────────────────────────────────────────────────────────────────────

/// The conversation's message box, emptied as a person would (its draft goes with it).
async fn clear_composer(cx: &TestCx) -> Result<()> {
    click_probe(cx, "dm-composer").await?;
    cx.keystroke("ctrl-a")?;
    cx.keystroke("backspace")?;
    Ok(())
}

fn e2ee_status(cx: &App) -> E2eeStatus {
    model(cx).live.e2ee.read(cx).status
}

fn shown_notice(peer: Uid, cx: &mut App) -> Option<crate::dm::Notice> {
    let key = current_key(peer, cx);
    let info = PeerInfos::existing(cx).and_then(|s| s.read(cx).get(peer, key).cloned());
    let e = model(cx).live.e2ee.read(cx);
    crate::dm::notice(e.status, e.changed_peers.get(&peer).copied().unwrap_or(0), e.setup_nudge_dismissed, info.as_ref())
}

fn peer_info(peer: Uid, cx: &mut App) -> Option<crate::e2ee::PeerInfo> {
    let key = current_key(peer, cx);
    PeerInfos::existing(cx).and_then(|s| s.read(cx).get(peer, key).cloned())
}

/// Run the passphrase dialog as a person would: its first field has the focus; the second (when
/// asked twice) after Tab; Enter submits. Waits for the core to finish.
async fn passphrase_dialog(cx: &TestCx, passphrase: &str, twice: bool) -> Result<()> {
    wait_for(cx, Duration::from_secs(5), "the passphrase dialog", |cx| model(cx).live.e2ee.read(cx).passphrase.is_some()).await?;
    cx.settle().await?;
    cx.type_text(passphrase)?;
    if twice {
        cx.keystroke("tab")?;
        cx.type_text(passphrase)?;
    }
    cx.keystroke("enter")?;
    wait_for(cx, Duration::from_secs(60), "the key operation", |cx| model(cx).live.e2ee.read(cx).passphrase.as_ref().is_none_or(|f| f.error.is_some())).await?;
    if let Some(error) = cx.update(|_, cx| model(cx).live.e2ee.read(cx).passphrase.as_ref().and_then(|f| f.error.clone()))? {
        bail!("the passphrase dialog refused: {error}");
    }
    // It takes every click while it fades (300 ms).
    cx.sleep(Duration::from_millis(400)).await;
    Ok(())
}

/// Settings → Privacy, then a click on one of the encryption card's buttons (scrolled into view).
async fn privacy_button(cx: &TestCx, key: &str) -> Result<()> {
    cx.update(|_, cx| {
        let m = model(cx);
        m.nav.update(cx, |n, cx| n.set_settings_tab("privacy", cx));
        m.overlays.update(cx, |o, cx| o.open_modal(decibell_model::stores::overlays::modal::SETTINGS, cx));
    })?;
    let mut bounds = wait_probe(cx, key).await?;
    cx.settle().await?;
    let viewport = cx.update(|window, cx| decibell_ui::visible_window_bounds(window, cx))?;
    let (top, bottom) = (viewport.top() + px(120.), viewport.bottom() - px(80.));
    for _ in 0..40 {
        let (below, above) = (bounds.bottom() > bottom, bounds.top() < top);
        if !below && !above {
            break;
        }
        let at = viewport.center();
        let dy = if below { px(-160.) } else { px(160.) };
        cx.update(move |window, cx| {
            window.dispatch_event(
                PlatformInput::Scroll(gpui::ScrollEvent {
                    position: at,
                    delta: gpui::ScrollDelta::Pixels(point(px(0.), dy)),
                    modifiers: Modifiers::default(),
                    touch_phase: gpui::TouchPhase::Moved,
                    source: Default::default(),
                }),
                cx,
            );
        })?;
        cx.settle().await?;
        bounds = probes::bounds(key).context("the probe vanished")?;
    }
    cx.click(bounds.center())?;
    park_pointer(cx)
}

async fn close_settings(cx: &TestCx) -> Result<()> {
    cx.update(|_, cx| model(cx).overlays.update(cx, |o, cx| o.close_modal(cx)))?;
    cx.sleep(Duration::from_millis(400)).await;
    Ok(())
}

/// The local stack's database: B's published keys and backup removed (B is a throwaway account
/// this test registered; nothing else is touched). Central then answers "no keys" for B.
fn remove_keys_from_stack(name: &str) -> Result<()> {
    ensure!(name.starts_with("ht") && name.chars().all(|c| c.is_ascii_alphanumeric()), "{name} is not a throwaway account of these tests");
    let dev = dev_dir();
    let psql = std::fs::read_dir(dev.join("deps/central/pgvenv/lib"))?
        .filter_map(Result::ok)
        .map(|d| d.path().join("site-packages/pgserver/pginstall/bin/psql"))
        .find(|p| p.exists())
        .context("the stack's psql (deps/central/pgvenv)")?;
    let conn = format!("host={} port=54329 dbname=decibell user=postgres", dev.join("run/pg").display());
    let sql = format!(
        "DELETE FROM e2ee_keys WHERE uid = (SELECT uid FROM users WHERE username = '{name}'); \
         DELETE FROM e2ee_backups WHERE uid = (SELECT uid FROM users WHERE username = '{name}');"
    );
    let out = Command::new(psql).arg(conn).arg("-v").arg("ON_ERROR_STOP=1").arg("-qtAc").arg(sql).output()?;
    ensure!(out.status.success(), "psql: {}", String::from_utf8_lossy(&out.stderr));
    Ok(())
}

pub(super) async fn check_e2ee_keys(cx: TestCx) -> Result<()> {
    let (actor, me, central) = begin(&cx).await?;
    let (a_name, b_name, password) = (actor.me.clone(), actor.peer.clone(), actor.password.clone());
    let run = run_tag();
    let pass = |n: &str| format!("e2ee-keys-{run}-{n}");
    let (pa1, pa2, pa3, pa4, pb1, pb2) = (pass("a1"), pass("a2"), pass("a3"), pass("a4"), pass("b1"), pass("b2"));
    let a = format!("#{me}");
    let u = format!("uid:{me}");
    let (sealed_b, after_reset_a, after_reset_b, locked_quick, locked_composer, unlocked_quick, gone_text) = (
        format!("sealed from B ({run})"),
        format!("A's first message under its new keys ({run})"),
        format!("B's first message under its new keys ({run})"),
        format!("quick DM while locked ({run})"),
        format!("composer while locked ({run})"),
        format!("quick DM after unlocking ({run})"),
        format!("to B, whose keys are gone ({run})"),
    );
    let cue = |n: &str| cue_path(&actor, n);

    // B, the peer.
    let b_profile = TempDir::new("e2ee-b")?;
    let b_script: Vec<String> = [
        vec!["wait-file".to_string(), cue("b-setup"), "e2ee-ready".into(), pb1.clone(), "watch".into(), a.clone(), "print".into(), "b-keys".into()],
        vec!["wait-file".to_string(), cue("b-send"), "dm-send".into(), u.clone(), sealed_b.clone(), "wait-dm".into(), u.clone(), sealed_b.clone(), "print".into(), "b-sent".into()],
        // A reset its keys: its next message shows B the change.
        vec!["wait-dm".to_string(), u.clone(), after_reset_a.clone(), "peer-info".into(), a.clone(), "print".into(), "b-checked".into()],
        vec!["wait-file".to_string(), cue("b-reset"), "e2ee-reset".into(), pb2.clone(), "dm-send".into(), u.clone(), after_reset_b.clone(), "wait-dm".into(), u.clone(), after_reset_b.clone(), "print".into(), "b-reset".into()],
        vec!["wait-dm".to_string(), u.clone(), unlocked_quick.clone(), "print".into(), "b-got-unlocked".into()],
        vec!["wait-file".to_string(), cue("done"), "dump".into(), "dm".into(), u.clone(), "logout".into(), "wait".into(), "logged-out".into()],
    ]
    .concat();
    let (mut b, peer) = person(&cx, "b", &b_profile.0, &central, &b_name, &password, b_script).await?;

    // 1. Not set up: the nudge over the conversation, dismissed with "Not now".
    wait_for(&cx, Duration::from_secs(20), "the E2EE status (not set up)", |cx| e2ee_status(cx) == E2eeStatus::NotSetUp).await?;
    open_conversation(&cx, peer, &b_name).await?;
    wait_for(&cx, Duration::from_secs(10), "the set-up nudge", move |cx| shown_notice(peer, cx) == Some(crate::dm::Notice::SetUp)).await?;
    click_probe(&cx, "e2ee-not-now").await?;
    wait_for(&cx, Duration::from_secs(5), "the nudge to go", move |cx| shown_notice(peer, cx).is_none() && model(cx).live.e2ee.read(cx).setup_nudge_dismissed).await?;
    cx.log("not set up: the nudge showed over the conversation; Not now dismissed it");

    // 2. Both set up; A's second device unlocks; B's sealed message opens here.
    {
        let p = pa1.clone();
        cx.update(move |_, cx| {
            model(cx).live.e2ee.update(cx, |e, cx| e.open(PassphraseMode::Setup, cx));
            actions::e2ee::submit_passphrase(p.clone(), p, cx);
        })?;
    }
    wait_for(&cx, Duration::from_secs(60), "our keys", |cx| e2ee_status(cx) == E2eeStatus::Ready && model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    cx.sleep(Duration::from_millis(400)).await;
    actor.cue("b-setup", 1)?;
    b.mark(&cx, "b-keys").await?;
    let a2_profile = TempDir::new("e2ee-a2")?;
    let a2_script: Vec<String> = [
        vec!["wait".to_string(), "e2ee".into(), "locked".into(), "e2ee-ready".into(), pa1.clone(), "print".into(), "a2-ready".into()],
        vec!["wait-file".to_string(), cue("a2-watch"), "wait".into(), "e2ee".into(), "locked".into(), "print".into(), "a2-locked".into()],
        vec!["e2ee-ready".to_string(), pa3.clone(), "print".into(), "a2-unlocked".into()],
        vec!["wait-file".to_string(), cue("a2-reset"), "e2ee-reset".into(), pa4.clone(), "print".into(), "a2-reset".into()],
        vec!["wait-file".to_string(), cue("done"), "logout".into(), "wait".into(), "logged-out".into()],
    ]
    .concat();
    let (mut a2, _) = person(&cx, "a2", &a2_profile.0, &central, &a_name, &password, a2_script).await?;
    a2.mark(&cx, "a2-ready").await?;
    // B set up after we last asked (our status changing to ready asked then): opening the
    // conversation again asks again, as each mount of the renderer's hook does.
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    wait_for(&cx, Duration::from_secs(5), "the conversation closed", |cx| crate::hooks::shown::<Conversation>(cx).is_none()).await?;
    open_conversation(&cx, peer, &b_name).await?;
    wait_for(&cx, Duration::from_secs(15), "B's keys in the header", move |cx| peer_info(peer, cx).is_some_and(|i| i.has_keys)).await?;
    actor.cue("b-send", 1)?;
    b.mark(&cx, "b-sent").await?;
    {
        let t = sealed_b.clone();
        wait_for(&cx, Duration::from_secs(15), "B's sealed message", move |cx| conversation_rows(peer, cx).iter().any(|m| *m.content == t && m.encrypted && m.decrypt_error.is_none())).await?;
    }
    let before = cx.update(move |_, cx| peer_info(peer, cx))?.context("B's info")?;
    cx.log(format_args!("both set up, A's second device unlocked; B's sealed message opened here (safety number {})", before.safety_number));

    // 3. Change passphrase (Settings → Privacy): a fresh device of A unlocks with the new one.
    privacy_button(&cx, "e2ee-change").await?;
    passphrase_dialog(&cx, &pa2, true).await?;
    let changed = cx.update(|_, cx| (toast("Passphrase changed", cx).is_some(), e2ee_status(cx)))?;
    ensure!(changed == (true, E2eeStatus::Ready), "after Change passphrase: the toast and the status {changed:?}");
    close_settings(&cx).await?;
    let a3_profile = TempDir::new("e2ee-a3")?;
    let (mut a3, _) = person(
        &cx,
        "a3",
        &a3_profile.0,
        &central,
        &a_name,
        &password,
        strings(&["wait", "e2ee", "locked", "e2ee-ready", &pa2, "print", "a3-ready", "logout", "wait", "logged-out"]),
    )
    .await?;
    a3.mark(&cx, "a3-ready").await?;
    a3.line(&cx, "DONE", Duration::from_secs(30)).await?;
    cx.log("Change passphrase (Settings → Privacy, the dialog): a fresh device of A unlocked with the new passphrase");

    // 4. Reset keys (Settings → Privacy): A's other device locks; B sees the change.
    let key_before = cx.update(|_, cx| model(cx).live.e2ee.read(cx).key_id)?;
    actor.cue("a2-watch", 1)?;
    privacy_button(&cx, "e2ee-reset").await?;
    passphrase_dialog(&cx, &pa3, true).await?;
    ensure!(cx.update(|_, cx| toast("Encryption keys reset", cx).is_some())?, "no \"Encryption keys reset\" toast");
    close_settings(&cx).await?;
    let key_after = cx.update(|_, cx| model(cx).live.e2ee.read(cx).key_id)?;
    ensure!(key_after != key_before && cx.update(|_, cx| e2ee_status(cx))? == E2eeStatus::Ready, "Reset keys: key {key_before} → {key_after}");
    a2.mark(&cx, "a2-locked").await?;
    {
        let t = after_reset_a.clone();
        cx.update(move |_, cx| {
            actions::dm::send_dm(peer, &t, 0, cx);
        })?;
    }
    let info = b.line(&cx, "DUMP peer-info", Duration::from_secs(30)).await?;
    b.mark(&cx, "b-checked").await?;
    let changed_at: i64 = info.split("\"changedAt\":").nth(1).and_then(|r| r.split(|c: char| !c.is_ascii_digit()).next()).and_then(|d| d.parse().ok()).unwrap_or(0);
    ensure!(changed_at > 0 && info.contains(&format!("\"keyId\":{key_after}")), "B's view of A after the reset: {info}");
    cx.log(format_args!("Reset keys (the dialog): key {key_before} → {key_after}; A's other device locked; B saw the change with A's next message"));

    // 5. B resets: its next message shows the "keys changed" notice here; Dismiss.
    actor.cue("b-reset", 1)?;
    b.mark(&cx, "b-reset").await?;
    wait_for(&cx, Duration::from_secs(15), "the keys-changed notice", move |cx| shown_notice(peer, cx) == Some(crate::dm::Notice::KeysChanged)).await?;
    {
        let t = after_reset_b.clone();
        wait_for(&cx, Duration::from_secs(15), "B's message under its new keys", move |cx| conversation_rows(peer, cx).iter().any(|m| *m.content == t && m.decrypt_error.is_none())).await?;
    }
    {
        let old = before.safety_number.clone();
        let changed = wait_for(&cx, Duration::from_secs(10), "B's new safety number", move |cx| peer_info(peer, cx).is_some_and(|i| i.has_keys && i.safety_number != old)).await;
        if let Err(e) = changed {
            bail!("{e:#} (B's info now: {:?})", cx.update(move |_, cx| peer_info(peer, cx))?);
        }
    }
    click_probe(&cx, "e2ee-dismiss-change").await?;
    wait_for(&cx, Duration::from_secs(5), "the notice to go", move |cx| shown_notice(peer, cx).is_none()).await?;
    cx.log("B reset its keys: \"keys changed\" over the conversation (a new safety number), Dismiss took it away");

    // 6. A's keys reset on the other device: locked here; B (pinned) never gets plaintext.
    actor.cue("a2-reset", 1)?;
    a2.mark(&cx, "a2-unlocked").await?;
    a2.mark(&cx, "a2-reset").await?;
    wait_for(&cx, Duration::from_secs(15), "this device to lock", |cx| e2ee_status(cx) == E2eeStatus::Locked).await?;
    wait_for(&cx, Duration::from_secs(5), "the locked notice", move |cx| shown_notice(peer, cx) == Some(crate::dm::Notice::Locked)).await?;
    open_popout(&cx, peer).await?;
    cx.type_text(&locked_quick)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(10), "the quick DM's refusal", |cx| popout(cx).is_some_and(|(sending, _)| !sending)).await?;
    let (sending, kept) = cx.update(|_, cx| popout(cx))?.context("the popout closed")?;
    ensure!(!sending && kept == locked_quick, "the refused quick DM: sending {sending}, the field holds {kept:?}");
    cx.log("locked: the quick DM to B (pinned) was refused: the popout stays open with its text");
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(3), "the popout to close", |cx| model(cx).overlays.read(cx).profile_popup.is_none()).await?;
    click_probe(&cx, "dm-composer").await?;
    cx.type_text(&locked_composer)?;
    cx.keystroke("enter")?;
    let refused = wait_for(&cx, Duration::from_secs(10), "the composer's refusal", move |cx| draft_and_error(peer, cx).1.is_some()).await;
    if let Err(e) = refused {
        let t = locked_composer.clone();
        let state = cx.update(move |window, cx| {
            let rows: Vec<String> = conversation_rows(peer, cx).iter().filter(|m| *m.content == t).map(|m| format!("id {} pending {}", m.id, m.pending)).collect();
            format!(
                "{}; the box {:?}, a text field focused {}; rows with the text {rows:?}",
                slice_state(peer, cx),
                draft_and_error(peer, cx),
                decibell_text::input::text_field_focused(window)
            )
        })?;
        bail!("{e:#} ({state})");
    }
    let (draft, error) = cx.update(move |_, cx| draft_and_error(peer, cx))?;
    let left = {
        let t = locked_composer.clone();
        cx.update(move |_, cx| rows_with(peer, &t, cx))?
    };
    let want = format!("Error: Unlock encryption (Settings → Privacy) to send encrypted messages to {b_name}.");
    ensure!(error.as_deref() == Some(want.as_str()) && draft == locked_composer && left == 0, "the composer's refusal: error {error:?}, the box {draft:?}, {left} rows");
    cx.log(format_args!("locked: the composer's send refused ({error:?}), the bubble withdrawn, the text back in the box"));
    clear_composer(&cx).await?;
    // Unlocked (the new keys from the backup): the quick DM goes, sealed.
    {
        let p = pa4.clone();
        cx.update(move |_, cx| {
            model(cx).live.e2ee.update(cx, |e, cx| e.open(PassphraseMode::Unlock, cx));
            actions::e2ee::submit_passphrase(p.clone(), p, cx);
        })?;
    }
    wait_for(&cx, Duration::from_secs(60), "the unlock", |cx| e2ee_status(cx) == E2eeStatus::Ready && model(cx).live.e2ee.read(cx).passphrase.is_none()).await?;
    cx.sleep(Duration::from_millis(400)).await;
    open_popout(&cx, peer).await?;
    cx.type_text(&unlocked_quick)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(10), "the popout to close", |cx| model(cx).overlays.read(cx).profile_popup.is_none()).await?;
    b.mark(&cx, "b-got-unlocked").await?;
    {
        let t = unlocked_quick.clone();
        wait_for(&cx, Duration::from_secs(15), "its echo, sealed", move |cx| conversation_rows(peer, cx).iter().any(|m| *m.content == t && m.encrypted)).await?;
    }
    cx.log("unlocked: the quick DM went (sealed), the popout closed");

    // 7. B's keys gone from central: pinned, so never plaintext.
    remove_keys_from_stack(&b_name)?;
    // A new sign-in forgets this session's cached keys.
    sign_out(&cx).await?;
    {
        let (name, password) = (a_name.clone(), password.clone());
        cx.update(move |_, cx| actions::auth::sign_in(name, password, cx))?;
    }
    signed_in(&cx).await?;
    wait_for(&cx, Duration::from_secs(20), "our keys loaded again", |cx| e2ee_status(cx) == E2eeStatus::Ready).await?;
    open_conversation(&cx, peer, &b_name).await?;
    wait_for(&cx, Duration::from_secs(15), "B without keys", move |cx| peer_info(peer, cx).is_some_and(|i| !i.has_keys && i.pinned)).await?;
    clear_composer(&cx).await?;
    cx.type_text(&gone_text)?;
    cx.keystroke("enter")?;
    wait_for(&cx, Duration::from_secs(10), "the refusal", move |cx| draft_and_error(peer, cx).1.is_some()).await?;
    let (draft, error) = cx.update(move |_, cx| draft_and_error(peer, cx))?;
    let want = format!("Error: {b_name}'s encryption keys are missing from the server; refusing to send unencrypted.");
    ensure!(error.as_deref() == Some(want.as_str()) && draft == gone_text, "a pinned peer without keys: error {error:?}, the box {draft:?}");
    cx.log(format_args!("B's keys removed from central: B pinned, no keys; the send refused ({error:?}), the text back in the box"));
    clear_composer(&cx).await?;

    actor.cue("done", 1)?;
    let dump = b.line(&cx, "DUMP dm", Duration::from_secs(30)).await?;
    for refused in [&locked_quick, &locked_composer, &gone_text] {
        ensure!(!dump.contains(refused.as_str()), "B received a refused message ({refused:?}): {dump}");
    }
    b.line(&cx, "DONE", Duration::from_secs(30)).await?;
    a2.line(&cx, "DONE", Duration::from_secs(30)).await?;
    cx.log("B received none of the refused messages");
    cx.update(|_, cx| decibell_harness::focus::hold_window_focus(None, cx))?;
    sign_out(&cx).await
}
