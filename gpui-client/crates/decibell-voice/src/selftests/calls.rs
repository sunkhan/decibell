//! DM calls end to end (the parity audit's T24 / T25): this app as `perf3`, headless peers as
//! `perf4` (the other side), `perf5` (someone perf4 is already calling) and a second device of
//! `perf3`. Every client that connects a call plays and captures only on the parent's null sink
//! (`audio-devices`); the app's own devices are put there by [`ready`].
//!
//! - `call-out`: the DM header's call button; Calling… then Ringing…; the peer answers (P2P over
//!   loopback); hang up; a peer already in a call answers Busy.
//! - `call-timeout`: nobody answers: 45 real seconds (the ring timer is the product's constant, and
//!   only GPUI's test dispatcher can move time), then "No answer" here and a missed call there.
//! - `call-elsewhere`: perf4 calls perf3; perf3's other device answers; the card here says
//!   "Answered on another device" for 3 s.
//! - `call-voice-exclusive`: calling from a voice channel leaves it; joining one ends the call.
//! - `call-share`: perf4 shares a recording in the call (`stream-file`); the stage's tile, focus,
//!   back to the tiles, theater, fullscreen, the mini-player; stopped while fullscreen; shared
//!   again and perf4 hangs up.

use std::time::{Duration, Instant};

use decibell_model::live::calls::CallStatus;

use super::*;

fn call_status(cx: &App) -> CallStatus {
    model(cx).live.calls.read(cx).status
}

fn end_reason(cx: &App) -> Option<String> {
    model(cx).live.calls.read(cx).end_reason.as_ref().map(|r| r.to_string())
}

/// The newest toast titled `title`: its body.
fn toast(title: &str, cx: &App) -> Option<String> {
    model(cx).toasts.read(cx).toasts().iter().rev().find(|t| &*t.title == title).map(|t| t.body.as_ref().map(|b| b.to_string()).unwrap_or_default())
}

/// The call store and the toasts, for a failure's message.
fn call_state(cx: &App) -> String {
    let c = model(cx).live.calls.read(cx);
    let toasts: Vec<String> = model(cx).toasts.read(cx).toasts().iter().map(|t| format!("{} / {:?}", t.title, t.body)).collect();
    format!("status {:?}, peer {:?}, ringing acked {}, ended {:?}, signaling {}; toasts {toasts:?}", c.status, c.peer, c.ringing_acked, c.end_reason, c.call_signaling)
}

/// A peer's script starts with the null sink as its devices (none when there is no sink: then the
/// call checks don't connect anyone, see [`connects`]).
fn on_sink(mut script: Vec<String>) -> Vec<String> {
    if let Some(sink) = test_sink() {
        script.splice(0..0, ["audio-devices".to_string(), sink]);
    }
    script
}

/// Without a null sink a connected call would open the desktop's real devices in the headless peer:
/// the checks that connect one skip then.
fn connects(cx: &TestCx) -> bool {
    if test_sink().is_some() {
        return true;
    }
    cx.log("SKIP: no null sink (pactl): a connected call would open the desktop's devices");
    false
}

/// The conversation with `peer` open and its call button usable (they're online, calls are relayed,
/// no call is running).
async fn conversation_with(cx: &TestCx, peer: Uid, name: &str) -> Result<()> {
    let name = name.to_string();
    cx.update(move |_, cx| actions::dm::open_dm(peer, name.into(), cx))?;
    wait_probe(cx, "dm-call").await?;
    wait_for(cx, Duration::from_secs(15), "the call button to be usable", move |cx| {
        let calls = model(cx).live.calls.read(cx);
        calls.call_signaling && calls.status == CallStatus::Idle && decibell_common::presence_of(peer, false, cx).is_online()
    })
    .await?;
    Ok(())
}

// ── call-out ────────────────────────────────────────────────────────────────────────────────

pub(super) async fn check_out(cx: TestCx) -> Result<()> {
    if !connects(&cx) {
        return Ok(());
    }
    ready(&cx).await?;
    let perf4 = account_uid(PEER)?;
    let peer = Peer::start(&on_sink(steps(&[
        "print", "ready-for-call", "wait", "call", "incoming", "print", "ringing", "wait-file", "@cue:answer", "answer", "wait", "call",
        "active", "print", "connected", "wait", "call", "idle", "print", "ended", "wait-file", "@cue:busy", "call", "perf5", "wait", "call",
        "outgoing", "print", "outgoing", "wait-file", "@cue:busy-done", "hangup", "wait", "call", "idle", "print", "idle-again", "logout",
    ])))?;
    // Someone perf4 calls later: online, ringing, never answering.
    let busy_done = peer.cues.join("busy-done").display().to_string();
    let perf5 = Peer::start_as("perf5", &steps(&["print", "online", "wait", "call", "incoming", "print", "rung", "wait-file", &busy_done, "logout"]))?;
    peer.wait_line(&cx, "MARK ready-for-call", Duration::from_secs(60)).await?;
    perf5.wait_line(&cx, "MARK online", Duration::from_secs(60)).await?;

    conversation_with(&cx, perf4, PEER).await?;
    click_probe(&cx, "dm-call").await?;
    wait_for(&cx, Duration::from_secs(5), "the outgoing call", |cx| call_status(cx) == CallStatus::Outgoing).await?;
    wait_probe(&cx, "call-hang-up").await?;
    peer.wait_line(&cx, "MARK ringing", Duration::from_secs(20)).await?;
    wait_for(&cx, Duration::from_secs(10), "Ringing… (the callee's client acked)", |cx| model(cx).live.calls.read(cx).ringing_acked).await?;
    cx.log("the DM header's call button: Calling…, then Ringing… once perf4's client rang; the stage shows Cancel");
    peer.cue("answer")?;
    let t = wait_for(&cx, Duration::from_secs(20), "the call to connect", |cx| call_status(cx) == CallStatus::Active).await?;
    peer.wait_line(&cx, "MARK connected", Duration::from_secs(10)).await?;
    let (path, verified) = cx.update(|_, cx| {
        let c = model(cx).live.calls.read(cx);
        (c.path, c.verified)
    })?;
    ensure!(verified == Some(true), "perf4 has keys: the call should be verified ({verified:?})");
    session_on_test_sink(&cx, "in the call").await?;
    cx.log(format_args!("answered: connected after {} ms ({path:?}, verified); our streams are on the test sink", t.as_millis()));
    click_probe(&cx, "call-hang-up").await?;
    wait_for(&cx, Duration::from_secs(5), "idle after hanging up", |cx| call_status(cx) == CallStatus::Idle).await?;
    peer.wait_line(&cx, "MARK ended", Duration::from_secs(10)).await?;
    ensure!(cx.update(|_, cx| end_reason(cx))?.as_deref() == Some("Call ended"), "hung up: {:?}", cx.update(|_, cx| end_reason(cx))?);
    closed_after_session(&cx).await?;
    cx.log("hung up from the stage: both sides idle, \"Call ended\"");

    // perf4 is in another call (ringing perf5): Busy.
    peer.cue("busy")?;
    peer.wait_line(&cx, "MARK outgoing", Duration::from_secs(20)).await?;
    perf5.wait_line(&cx, "MARK rung", Duration::from_secs(20)).await?;
    conversation_with(&cx, perf4, PEER).await?;
    calm(&cx).await;
    click_probe(&cx, "dm-call").await?;
    let placed = wait_for(&cx, Duration::from_secs(5), "the second call to start", |cx| call_status(cx) != CallStatus::Idle || toast("Busy", cx).is_some()).await;
    if let Err(e) = placed {
        bail!("{e:#} ({})", cx.update(|_, cx| call_state(cx))?);
    }
    let busy = wait_for(&cx, Duration::from_secs(15), "Busy", |cx| call_status(cx) == CallStatus::Idle && toast("Busy", cx).is_some()).await;
    if let Err(e) = busy {
        bail!("{e:#} ({})", cx.update(|_, cx| call_state(cx))?);
    }
    let (body, reason) = cx.update(|_, cx| (toast("Busy", cx), end_reason(cx)))?;
    ensure!(body.as_deref() == Some("perf4 is in another call.") && reason.as_deref() == Some("Busy"), "Busy: toast {body:?}, ended {reason:?}");
    cx.log(format_args!("perf4 already calling someone: toast \"Busy\" / {body:?}, the call ended ({reason:?})"));
    peer.cue("busy-done")?;
    peer.wait_line(&cx, "MARK idle-again", Duration::from_secs(20)).await?;
    peer.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    perf5.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    Ok(())
}

// ── call-timeout ────────────────────────────────────────────────────────────────────────────

pub(super) async fn check_timeout(cx: TestCx) -> Result<()> {
    ready(&cx).await?;
    let perf4 = account_uid(PEER)?;
    // perf4 rings and never answers; its own ring ends at 45 s too (or our CANCEL first).
    let peer = Peer::start(&steps(&[
        "print", "ready-for-call", "wait", "call", "incoming", "print", "ringing", "wait", "call", "idle", "print", "missed", "dump", "toasts",
        "logout",
    ]))?;
    peer.wait_line(&cx, "MARK ready-for-call", Duration::from_secs(60)).await?;
    conversation_with(&cx, perf4, PEER).await?;
    click_probe(&cx, "dm-call").await?;
    wait_for(&cx, Duration::from_secs(15), "the INVITE to leave (the ring armed)", |cx| model(cx).live.calls.read(cx).ring_armed()).await?;
    let armed = Instant::now();
    peer.wait_line(&cx, "MARK ringing", Duration::from_secs(20)).await?;
    cx.log("ringing out; nobody answers (45 s, real time)");
    let t = wait_for(&cx, Duration::from_secs(60), "the call to give up", |cx| call_status(cx) == CallStatus::Idle).await?;
    let reason = cx.update(|_, cx| end_reason(cx))?;
    let after = armed.elapsed();
    ensure!(reason.as_deref() == Some("No answer"), "the unanswered call ended with {reason:?}");
    ensure!(after >= Duration::from_millis(44_500) && after <= Duration::from_secs(50), "it gave up after {after:?}");
    cx.log(format_args!("the caller gave up {:.1} s after the INVITE: \"No answer\" (waited {:.1} s here)", after.as_secs_f64(), t.as_secs_f64()));
    peer.wait_line(&cx, "MARK missed", Duration::from_secs(10)).await?;
    let toasts = peer.wait_line(&cx, "DUMP toasts", Duration::from_secs(10)).await?;
    ensure!(toasts.contains("Missed call") && toasts.contains("perf3 tried to call you."), "perf4's toasts: {toasts}");
    cx.log("the callee: the ring ended, a missed call (\"perf3 tried to call you.\")");
    peer.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    Ok(())
}

// ── call-elsewhere ──────────────────────────────────────────────────────────────────────────

pub(super) async fn check_elsewhere(cx: TestCx) -> Result<()> {
    if !connects(&cx) {
        return Ok(());
    }
    ready(&cx).await?;
    let caller = Peer::start(&on_sink(steps(&[
        "print", "ready-for-call", "wait-file", "@cue:call", "call", ACCOUNT, "wait", "call", "active", "print", "connected", "wait-file",
        "@cue:hangup", "hangup", "wait", "call", "idle", "print", "ended", "logout",
    ])))?;
    // Our other device: rings with this one, answers first.
    let answer = caller.cues.join("answer").display().to_string();
    let other = Peer::start_as(
        ACCOUNT,
        &on_sink(steps(&[
            "print", "ready-for-call", "wait", "call", "incoming", "print", "ringing", "wait-file", &answer, "answer", "wait", "call", "active",
            "print", "connected", "wait", "call", "idle", "print", "ended", "logout",
        ])),
    )?;
    caller.wait_line(&cx, "MARK ready-for-call", Duration::from_secs(60)).await?;
    other.wait_line(&cx, "MARK ready-for-call", Duration::from_secs(60)).await?;
    caller.cue("call")?;
    wait_for(&cx, Duration::from_secs(20), "the incoming card here", |cx| call_status(cx) == CallStatus::Incoming && probes::flag_set("incoming-call")).await?;
    other.wait_line(&cx, "MARK ringing", Duration::from_secs(20)).await?;
    cx.log("perf4 calls: both of our devices ring");
    caller.cue("answer")?;
    wait_for(&cx, Duration::from_secs(15), "Answered on another device", |cx| {
        model(cx).live.calls.read(cx).incoming_ended.as_ref().is_some_and(|e| &*e.reason == "Answered on another device" && e.answered)
    })
    .await?;
    let shown = Instant::now();
    cx.sleep(Duration::from_millis(500)).await;
    ensure!(probes::flag_set("incoming-call"), "the card went at once instead of saying where the call went");
    ensure!(cx.update(|_, cx| call_status(cx))? == CallStatus::Idle, "this device joined the call");
    wait_for(&cx, Duration::from_secs(6), "the card to go", |_| !probes::flag_set("incoming-call")).await?;
    let lasted = shown.elapsed();
    caller.wait_line(&cx, "MARK connected", Duration::from_secs(20)).await?;
    other.wait_line(&cx, "MARK connected", Duration::from_secs(10)).await?;
    ensure!(lasted >= Duration::from_millis(2700) && lasted <= Duration::from_millis(4500), "the card said it for {lasted:?} (3 s)");
    cx.log(format_args!("the other device answered: the card here said \"Answered on another device\" for {:.1} s; the call goes on there", lasted.as_secs_f64()));
    caller.cue("hangup")?;
    caller.wait_line(&cx, "MARK ended", Duration::from_secs(15)).await?;
    other.wait_line(&cx, "MARK ended", Duration::from_secs(15)).await?;
    caller.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    other.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    Ok(())
}

// ── call-voice-exclusive ────────────────────────────────────────────────────────────────────

pub(super) async fn check_voice_exclusive(cx: TestCx) -> Result<()> {
    if !connects(&cx) {
        return Ok(());
    }
    ready(&cx).await?;
    let me = me(&cx)?;
    let perf4 = account_uid(PEER)?;
    let peer = Peer::start(&on_sink(steps(&[
        "print", "ready-for-call", "wait", "call", "incoming", "answer", "wait", "call", "active", "print", "connected", "wait", "call", "idle",
        "print", "ended", "logout",
    ])))?;
    peer.wait_line(&cx, "MARK ready-for-call", Duration::from_secs(60)).await?;
    join(&cx, LOUNGE).await?;
    conversation_with(&cx, perf4, PEER).await?;
    click_probe(&cx, "dm-call").await?;
    wait_for(&cx, Duration::from_secs(10), "the voice channel left for the call", move |cx| {
        !model(cx).live.voice.session.read(cx).is_connected_to(daily_gaming(), LOUNGE) && !in_channel(cx, LOUNGE, me)
    })
    .await?;
    wait_for(&cx, Duration::from_secs(20), "the call to connect", |cx| call_status(cx) == CallStatus::Active).await?;
    peer.wait_line(&cx, "MARK connected", Duration::from_secs(10)).await?;
    cx.log("in Lounge, the call button left the channel; the call connected");
    // Joining Lounge from the sidebar ends the call first.
    click_probe(&cx, "server-tile:1").await?;
    wait_for(&cx, Duration::from_secs(10), "Daily Gaming's sidebar", |cx| {
        let nav = model(cx).nav.read(cx);
        nav.active_server == Some(daily_gaming()) && nav.active_view != ActiveView::Dm && nav.active_view != ActiveView::Home
    })
    .await?;
    calm(&cx).await;
    join(&cx, LOUNGE).await?;
    let reason = cx.update(|_, cx| (call_status(cx), end_reason(cx)))?;
    ensure!(reason == (CallStatus::Idle, Some("Joined a voice channel".to_string())), "after joining Lounge: {reason:?}");
    peer.wait_line(&cx, "MARK ended", Duration::from_secs(15)).await?;
    cx.log("joined Lounge during the call: the call ended (\"Joined a voice channel\") on both sides, MLS ready in Lounge");
    leave(&cx).await?;
    peer.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    closed_after_session(&cx).await
}

// ── call-share ──────────────────────────────────────────────────────────────────────────────

fn stream_state(uid: Uid, cx: &mut App) -> crate::video_events::StreamState {
    crate::video_events::StreamStates::global(cx).read(cx).get(uid)
}

fn watching(cx: &App) -> (Vec<Uid>, Option<Uid>, bool) {
    let w = model(cx).live.voice.watching.read(cx);
    (w.list().to_vec(), w.focused, w.fullscreen)
}

fn window_fullscreen(cx: &TestCx) -> Result<bool> {
    cx.update(|window, _| window.is_fullscreen())
}

/// A control over the focused video: the pointer goes there first and the overlays fade in (they
/// take no clicks while hidden), then the click.
async fn click_overlay(cx: &TestCx, key: &str) -> Result<()> {
    let bounds = wait_probe(cx, key).await?;
    cx.mouse_move(bounds.center())?;
    cx.sleep(Duration::from_millis(300)).await;
    cx.click(bounds.center())?;
    Ok(())
}

/// The stream is listed in the call (the tile is drawn for it, not an earlier one's place).
async fn listed(cx: &TestCx, owner: Uid) -> Result<()> {
    wait_for(cx, Duration::from_secs(10), "the stream listed in the call", move |cx| {
        model(cx).live.voice.streams.read(cx).session(None, true).iter().any(|s| s.owner_uid == owner)
    })
    .await?;
    calm(cx).await;
    Ok(())
}

/// Nothing is drawn at rest while a stream plays (a picture is a present): over 3 s with the
/// pointer parked, fewer draws than the call's 1 s duration ticks and a couple of pings explain.
/// A failure names the model stores that notified meanwhile.
async fn at_rest(cx: &TestCx, what: &str) -> Result<()> {
    // Off the stage (the overlays go when the pointer leaves the video): the title bar's middle.
    let at = cx.update(|window, cx| {
        let v = decibell_ui::visible_window_bounds(window, cx);
        point(v.left() + v.size.width * 0.4, v.top() + gpui::px(12.))
    })?;
    cx.mouse_move(at)?;
    cx.sleep(Duration::from_millis(600)).await;
    let (stores, _subs) = Changes::watch_all(cx)?;
    let before = cx.frames()?;
    let disturbed = cx.watched_sleep(Duration::from_secs(3)).await?;
    let frames = cx.frames()?.since(before);
    let notified: Vec<String> = stores.borrow().iter().map(|(k, n)| format!("{k} {n}")).collect();
    cx.log(format_args!("{what} at rest: {} drawn / {} presented in 3 s; stores that notified: [{}]{}", frames.drawn, frames.presented, notified.join(", "), disturbed.map(|d| format!(" (disturbed: {d})")).unwrap_or_default()));
    ensure!(frames.drawn <= 12, "{what} drew {} frames in 3 s at rest (stores that notified: [{}])", frames.drawn, notified.join(", "));
    Ok(())
}

/// Until 300 ms pass without a draw, at most 3 s (a playing stream presents all the time, so
/// `TestCx::settle`'s present rule never holds here): what is pending is drawn, so a click goes to
/// the frame on screen.
async fn calm(cx: &TestCx) {
    let started = Instant::now();
    let mut last = cx.frames().map(|f| f.drawn).unwrap_or(0);
    let mut since = Instant::now();
    while started.elapsed() < Duration::from_secs(3) {
        cx.sleep(Duration::from_millis(50)).await;
        let now = cx.frames().map(|f| f.drawn).unwrap_or(0);
        if now != last {
            last = now;
            since = Instant::now();
        } else if since.elapsed() >= Duration::from_millis(300) {
            return;
        }
    }
    cx.log("    (the window kept drawing for 3 s; going on)");
}

/// Pictures presented per second over `window`.
async fn presented_rate(cx: &TestCx, stream: &decibell_video::Stream, window: Duration) -> f64 {
    let a = stream.stats().presented;
    cx.sleep(window).await;
    (stream.stats().presented - a) as f64 / window.as_secs_f64()
}

pub(super) async fn check_share(cx: TestCx) -> Result<()> {
    if !connects(&cx) {
        return Ok(());
    }
    let Some(file) = stream_file() else {
        cx.log("SKIP: vendor/streams/*.dbv not found above the binary");
        return Ok(());
    };
    ready(&cx).await?;
    let perf4 = account_uid(PEER)?;
    let path = file.display().to_string();
    let peer = Peer::start(&on_sink(steps(&[
        "print", "ready-for-call", "wait-file", "@cue:call", "call", ACCOUNT, "wait", "call", "active", "print", "connected",
        "stream-file", &path, "print", "streaming", "wait-file", "@cue:stop", "stream-stop", "print", "stopped",
        "wait-file", "@cue:again", "stream-file", &path, "print", "streaming-again", "wait-file", "@cue:hangup", "hangup", "wait", "call", "idle",
        "print", "ended", "logout",
    ])))?;
    peer.wait_line(&cx, "MARK ready-for-call", Duration::from_secs(60)).await?;
    peer.cue("call")?;
    wait_for(&cx, Duration::from_secs(20), "the incoming card", |cx| call_status(cx) == CallStatus::Incoming).await?;
    click_probe(&cx, "call-accept").await?;
    wait_for(&cx, Duration::from_secs(20), "the call to connect", |cx| call_status(cx) == CallStatus::Active).await?;
    peer.wait_line(&cx, "MARK connected", Duration::from_secs(10)).await?;
    session_on_test_sink(&cx, "in the call").await?;
    peer.wait_line(&cx, "MARK streaming", Duration::from_secs(20)).await?;
    let tile = format!("stream-tile:{perf4}");
    listed(&cx, perf4).await?;
    wait_probe(&cx, &tile).await?;
    ensure!(cx.update(move |_, cx| stream_state(perf4, cx).picture.is_none())?, "a picture before anyone watched");
    cx.log("perf4 shares in the call: its stream tile is on the stage (its avatar: no thumbnails in calls)");

    // Watch: the tile's click; the stream takes the stage.
    click_probe(&cx, &tile).await?;
    let t = wait_for(&cx, Duration::from_secs(15), "the first picture", move |cx| stream_state(perf4, cx).picture.is_some() && watching(cx).1 == Some(perf4)).await?;
    let hub = cx.update(|_, cx| decibell_video::VideoHub::global(cx))?;
    let stream = hub.stream(&perf4.to_string());
    cx.sleep(Duration::from_secs(1)).await;
    let rate = presented_rate(&cx, &stream, Duration::from_secs(3)).await;
    let stats = stream.stats();
    cx.log(format_args!(
        "watched: first picture {} ms after the click; {} {}x{} via {}, {rate:.1} pictures/s",
        t.as_millis(),
        stats.codec.map(|c| c.label()).unwrap_or("?"),
        stats.width,
        stats.height,
        stats.path.map(|p| p.label()).unwrap_or("?")
    ));
    ensure!(rate > 50., "the focused stream presented {rate:.1} pictures/s of a 60 fps recording");

    // At rest the focused stream costs presents, not draws.
    at_rest(&cx, "the focused stream").await?;
    // A still pointer, over the video and off the stage, in 250 ms buckets: over the video the
    // overlays fade in, and out 2.5 s later (each ~150 ms, drawn at the display's rate); outside
    // those two fades, and off the stage throughout, a picture costs a present and nothing draws.
    for (what, at) in [
        ("over the video", probes::bounds("call-video").context("the video")?.center()),
        ("on the title bar", cx.update(|window, cx| {
            let v = decibell_ui::visible_window_bounds(window, cx);
            point(v.left() + v.size.width * 0.4, v.top() + gpui::px(12.))
        })?),
    ] {
        let (stores, _subs) = Changes::watch_all(&cx)?;
        cx.mouse_move(at)?;
        let mut windows = Vec::new();
        for _ in 0..16 {
            let from = cx.frames()?;
            cx.sleep(Duration::from_millis(250)).await;
            windows.push(cx.frames()?.since(from).drawn);
        }
        let notified: Vec<String> = stores.borrow().iter().map(|(k, n)| format!("{k} {n}")).collect();
        cx.log(format_args!("the pointer still {what}: drawn per 250 ms {windows:?}; stores that notified: [{}]", notified.join(", ")));
        let fades: &[usize] = if what == "over the video" { &[0, 1, 9, 10, 11] } else { &[] };
        let rest: u64 = windows.iter().enumerate().filter(|(i, _)| !fades.contains(i)).map(|(_, n)| *n).sum();
        ensure!(rest <= 12, "the pointer still {what}: {rest} drawn outside the overlays' fades in 4 s ({windows:?})");
    }
    // The overlays up over the playing video, the pointer still on its middle and then on a
    // control: pictures cost presents; the overlays go 2.5 s after the last move either way (the
    // renderer's mousemove re-arms its timer after a control's mouseenter), and nothing draws
    // after.
    for key in ["call-video", "call-stats"] {
        let b = probes::bounds(key).with_context(|| key.to_string())?;
        cx.mouse_move(b.center() + point(gpui::px(3.), gpui::px(0.)))?;
        cx.sleep(Duration::from_millis(300)).await;
        cx.mouse_move(b.center())?;
        let moved = Instant::now();
        let from = cx.frames()?;
        cx.sleep(Duration::from_millis(2000)).await;
        let up = cx.frames()?.since(from);
        let shown = probes::flag_set("call-overlay");
        cx.sleep(crate::focus::OVERLAY_HIDE.saturating_sub(moved.elapsed()) + Duration::from_millis(500)).await;
        let from = cx.frames()?;
        cx.sleep(Duration::from_millis(2000)).await;
        let after = cx.frames()?.since(from);
        let hidden = !probes::flag_set("call-overlay");
        cx.log(format_args!(
            "the pointer still on {key}: overlays up {shown}, {} drawn / {} presented in 2 s; hidden after 2.5 s {hidden}, then {} drawn in 2 s",
            up.drawn, up.presented, after.drawn
        ));
        ensure!(shown && hidden, "{key}: the overlays up {shown} after the move, hidden {hidden} 3 s after it");
        ensure!(up.drawn <= 12 && after.drawn <= 8, "{key}: {} frames drawn with the overlays up, {} after they went", up.drawn, after.drawn);
    }

    // A click on the video: back to the tiles, still watched (the tile plays it); its click: focus.
    click_probe(&cx, "call-video").await?;
    wait_for(&cx, Duration::from_secs(5), "back to the tiles", move |cx| watching(cx).1.is_none() && watching(cx).0.contains(&perf4)).await?;
    wait_probe(&cx, &tile).await?;
    let rate = presented_rate(&cx, &stream, Duration::from_secs(2)).await;
    ensure!(rate > 50., "the watched tile presented {rate:.1} pictures/s");
    // The tiles' dock under a resting pointer, its tooltip up: nothing draws.
    for key in ["call-mute", "call-hang-up"] {
        let b = probes::bounds(key).with_context(|| key.to_string())?;
        cx.mouse_move(b.center())?;
        cx.sleep(Duration::from_millis(1200)).await;
        let from = cx.frames()?;
        cx.sleep(Duration::from_secs(2)).await;
        let f = cx.frames()?.since(from);
        cx.log(format_args!("resting on {key} (its tooltip up): {} drawn / {} presented in 2 s", f.drawn, f.presented));
        ensure!(f.drawn <= 8, "resting on {key}: {} frames drawn in 2 s", f.drawn);
    }
    park_pointer(&cx)?;
    click_probe(&cx, &tile).await?;
    wait_for(&cx, Duration::from_secs(5), "focused again", move |cx| watching(cx).1 == Some(perf4)).await?;
    cx.log(format_args!("a click on the video went back to the tiles (the tile plays it: {rate:.1} pictures/s); the tile's click focused it again"));

    // The overlays hide when the pointer leaves the video and take no clicks while hidden.
    calm(&cx).await;
    let video = probes::bounds("call-video").context("the video")?;
    cx.mouse_move(video.center())?;
    cx.sleep(Duration::from_millis(300)).await;
    cx.mouse_move(point(video.center().x, video.top() - gpui::px(30.)))?;
    cx.sleep(Duration::from_millis(400)).await;
    let fs = probes::bounds("call-fullscreen").context("the fullscreen button")?;
    cx.click(fs.center())?;
    cx.sleep(Duration::from_millis(300)).await;
    let (_, focused, fullscreen) = cx.update(|_, cx| watching(cx))?;
    ensure!(!fullscreen, "a click on the hidden fullscreen button went through");
    if focused.is_none() {
        // The click reached the video under it, as in the renderer (back to the tiles): the tiles
        // are drawn before the tile is clicked.
        calm(&cx).await;
        cx.sleep(Duration::from_millis(300)).await;
        click_probe(&cx, &tile).await?;
        wait_for(&cx, Duration::from_secs(5), "focused again", move |cx| watching(cx).1 == Some(perf4)).await?;
    }
    cx.log("the pointer left the video: the controls went at once and a click on the hidden fullscreen button did nothing to it");

    // Fullscreen, then Escape.
    click_overlay(&cx, "call-fullscreen").await?;
    wait_for(&cx, Duration::from_secs(5), "fullscreen", |cx| watching(cx).2).await?;
    let started = Instant::now();
    while !window_fullscreen(&cx)? && started.elapsed() < Duration::from_secs(5) {
        cx.sleep(Duration::from_millis(50)).await;
    }
    ensure!(window_fullscreen(&cx)?, "the window didn't go fullscreen");
    calm(&cx).await;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape to leave fullscreen", |cx| !watching(cx).2).await?;
    let started = Instant::now();
    while window_fullscreen(&cx)? && started.elapsed() < Duration::from_secs(5) {
        cx.sleep(Duration::from_millis(50)).await;
    }
    ensure!(!window_fullscreen(&cx)? && cx.update(move |_, cx| watching(cx).1)? == Some(perf4), "after Escape: the window fullscreen {}, focused {:?}", window_fullscreen(&cx)?, cx.update(|_, cx| watching(cx).1)?);
    cx.log("fullscreen from the controls; Escape left it, the stream still focused");

    // Elsewhere in the app: the mini-player; its click comes back.
    cx.update(|_, cx| actions::servers::go_home(cx))?;
    wait_for(&cx, Duration::from_secs(5), "the mini-player", |_| probes::flag_set("mini-player")).await?;
    let rate = presented_rate(&cx, &stream, Duration::from_secs(2)).await;
    ensure!(rate > 50., "the mini-player presented {rate:.1} pictures/s");
    calm(&cx).await;
    // Its press and release are window listeners of its own while pressed: a frame between them
    // (CONVENTIONS, GPUI gotcha 14).
    let mini = wait_probe(&cx, "mini-player").await?;
    cx.mouse_down(mini.center(), 1, gpui::Modifiers::default())?;
    cx.sleep(Duration::from_millis(60)).await;
    cx.mouse_up(mini.center())?;
    park_pointer(&cx)?;
    wait_for(&cx, Duration::from_secs(5), "back in the call's conversation", move |cx| {
        let m = model(cx);
        m.nav.read(cx).active_view == ActiveView::Dm && m.dm.list.read(cx).active_peer == Some(perf4) && !probes::flag_set("mini-player")
    })
    .await?;
    wait_for(&cx, Duration::from_secs(5), "focused there", move |cx| watching(cx).1 == Some(perf4)).await?;
    cx.log(format_args!("on Home the mini-player kept it going ({rate:.1} pictures/s); its click came back to the call, focused"));

    // Fullscreen, and perf4 stops sharing: the tile goes, fullscreen is left, the decoder freed.
    click_overlay(&cx, "call-fullscreen").await?;
    wait_for(&cx, Duration::from_secs(5), "fullscreen", |cx| watching(cx).2).await?;
    peer.cue("stop")?;
    peer.wait_line(&cx, "MARK stopped", Duration::from_secs(15)).await?;
    wait_for(&cx, Duration::from_secs(10), "the stream gone", move |cx| {
        let w = watching(cx);
        !w.0.contains(&perf4) && w.1.is_none() && model(cx).live.voice.streams.read(cx).session(None, true).iter().all(|s| s.owner_uid != perf4)
    })
    .await?;
    let fullscreen = cx.update(|_, cx| watching(cx).2)?;
    ensure!(!fullscreen, "the stream ended while fullscreen and the stage stayed fullscreen");
    let started = Instant::now();
    while window_fullscreen(&cx)? && started.elapsed() < Duration::from_secs(5) {
        cx.sleep(Duration::from_millis(50)).await;
    }
    ensure!(!window_fullscreen(&cx)?, "the window stayed fullscreen after the stream ended");
    wait_for(&cx, Duration::from_secs(5), "the decoder to stop", {
        let hub = hub.clone();
        move |_| hub.decoder_threads() == 0
    })
    .await?;
    calm(&cx).await;
    ensure!(cx.update(|_, cx| call_status(cx))? == CallStatus::Active, "the call ended with the stream");
    cx.log("perf4 stopped sharing while we watched fullscreen: the tile went, fullscreen was left, the decoder stopped; the call goes on");

    // Shared again and watched; perf4 hangs up.
    peer.cue("again")?;
    peer.wait_line(&cx, "MARK streaming-again", Duration::from_secs(20)).await?;
    listed(&cx, perf4).await?;
    click_probe(&cx, &tile).await?;
    wait_for(&cx, Duration::from_secs(15), "the picture again", move |cx| stream_state(perf4, cx).picture.is_some() && watching(cx).1 == Some(perf4)).await?;
    // Theater: the stage takes the panel (the conversation goes); and back.
    calm(&cx).await;
    wait_for(&cx, Duration::from_secs(5), "the focused video laid out", |_| probes::bounds("call-video").is_some_and(|b| b.size.height > gpui::px(0.))).await?;
    let before = probes::bounds("call-video").context("the video")?;
    click_overlay(&cx, "call-theater").await?;
    wait_for(&cx, Duration::from_secs(5), "theater", |cx| model(cx).live.calls.read(cx).theater).await?;
    at_rest(&cx, "the stream in theater").await?;
    let during = probes::bounds("call-video").context("the video in theater")?;
    ensure!(during.size.height > before.size.height, "theater didn't give the stage the panel ({before:?} → {during:?})");
    // Escape steps back: the focused stream to the tiles, then out of theater.
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape: back to the tiles", move |cx| watching(cx).1.is_none() && model(cx).live.calls.read(cx).theater).await?;
    cx.keystroke("escape")?;
    wait_for(&cx, Duration::from_secs(5), "Escape: out of theater", |cx| !model(cx).live.calls.read(cx).theater).await?;
    cx.log(format_args!("theater from the video's controls: the video {:.0} → {:.0} px tall; Escape went back to the tiles, then out of theater", f32::from(before.size.height), f32::from(during.size.height)));
    // Quiet a while after the round trip, the pointer still on the stage (builds with the old
    // overlay hold drew every frame from a few seconds on here).
    cx.sleep(crate::focus::OVERLAY_HIDE + Duration::from_millis(600)).await;
    let from = cx.frames()?;
    cx.sleep(Duration::from_secs(6)).await;
    let after = cx.frames()?.since(from);
    cx.log(format_args!("6 s after the theater round trip (the pointer still on its button): {} drawn / {} presented", after.drawn, after.presented));
    ensure!(after.drawn <= 24, "{} frames drawn in 6 s at rest after the theater round trip", after.drawn);

    peer.cue("hangup")?;
    wait_for(&cx, Duration::from_secs(15), "the call to end", |cx| call_status(cx) == CallStatus::Idle).await?;
    let reason = cx.update(|_, cx| end_reason(cx))?;
    ensure!(reason.as_deref() == Some("Call ended"), "the sharer hung up: {reason:?}");
    wait_for(&cx, Duration::from_secs(5), "the stage and the stream gone", move |cx| {
        let w = watching(cx);
        w.0.is_empty() && w.1.is_none() && !w.2 && model(cx).live.voice.streams.read(cx).session(None, true).is_empty()
    })
    .await?;
    wait_for(&cx, Duration::from_secs(5), "the decoder to stop", {
        let hub = hub.clone();
        move |_| hub.decoder_threads() == 0
    })
    .await?;
    peer.wait_line(&cx, "MARK ended", Duration::from_secs(10)).await?;
    cx.log("watching, the sharer hung up: \"Call ended\", the stream and its decoder gone");
    peer.wait_line(&cx, "DONE", Duration::from_secs(20)).await?;
    closed_after_session(&cx).await
}
