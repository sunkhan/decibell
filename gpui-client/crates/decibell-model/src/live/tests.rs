//! The M3 router against recorded event sequences (built by hand), through the real queue
//! and pump: voice rosters with our own moves, force-move / disconnect, a reconnect's
//! resync, streams, the call state machine against a fake transport and the test clock,
//! presence routing, hotkey routing, E2EE prompts, the device notice, sign-out. No core,
//! no network: every core call answers "not running" at once, except calls, which go
//! through the fake.

use std::cell::RefCell;
use std::rc::Rc;
use std::time::Duration;

use decibell_core::events::*;
use decibell_core::hotkeys::Action;
use futures::future::LocalBoxFuture;
use gpui::{SharedString, TestAppContext};

use crate::Model;
use crate::error::UiError;
use crate::ids::{ChannelId, ServerId, Uid};
use crate::live::calls::{CallStatus, CallTransport, ConnectArgs, OutSignal, Prepared, RING_TIMEOUT};
use crate::live::e2ee::{E2eeStatus, PassphraseMode};
use crate::live::presence::DisplayStatus;
use crate::live::voice::rosters::SELF_MOVE_TIMEOUT;
use crate::live::{Cue, Sound};
use crate::pump::Incoming;
use crate::stores::navigation::ActiveView;
use crate::stores::toasts::Severity;

// ── Harness ─────────────────────────────────────────────────────────────────

const ME: Uid = 7;

fn deliver(model: &Model, cx: &mut TestAppContext, events: impl IntoIterator<Item = CoreEvent>) {
    for event in events {
        model.sink().queue().push(Incoming::Core(event));
    }
    cx.run_until_parked();
}

fn cues(model: &Model, cx: &mut TestAppContext) -> Rc<RefCell<Vec<Cue>>> {
    let seen: Rc<RefCell<Vec<Cue>>> = Rc::default();
    cx.update(|cx| {
        let seen = seen.clone();
        cx.subscribe(&model.live.cues, move |_, c: &Cue, _| seen.borrow_mut().push(*c)).detach();
    });
    seen
}

fn toasts(model: &Model, cx: &mut TestAppContext) -> Vec<(Severity, String, Option<String>)> {
    model.toasts.read_with(cx, |t, _| {
        t.toasts().iter().map(|t| (t.severity, t.title.to_string(), t.body.as_ref().map(|b| b.to_string()))).collect()
    })
}

fn channel(id: &str, kind: &str) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: id.into(),
        r#type: kind.into(),
        voice_bitrate_kbps: 64,
        retention_days_text: 0,
        retention_days_image: 0,
        retention_days_video: 0,
        retention_days_document: 0,
        retention_days_audio: 0,
        my_permissions: u64::MAX,
        slowmode_seconds: 0,
        encrypted: false,
        permissions_synced: false,
        category_sync: false,
        topic: String::new(),
    }
}

fn auth_ok() -> CoreEvent {
    CoreEvent::CommunityAuthResponded(CommunityAuthRespondedPayload {
        server_id: "1".into(),
        requested_server_id: "1".into(),
        host: "127.0.0.11".into(),
        port: 8082,
        success: true,
        message: String::new(),
        channels: vec![channel("general", "text"), channel("lounge", "voice"), channel("squad", "voice")],
        error_code: String::new(),
        server_name: "Daily Gaming".into(),
        server_description: String::new(),
        owner_username: "alice".into(),
        owner_uid: 1,
        attachment_port: 0,
        max_attachment_bytes: 0,
        public_listing: true,
    })
}

fn signed_in(cx: &mut TestAppContext) -> Model {
    let model = cx.update(|cx| Model::init(cx, None));
    deliver(
        &model,
        cx,
        [
            CoreEvent::LoginSucceeded(LoginSucceededPayload { username: "perf2".into(), uid: ME, renames: true, devices: true }),
            auth_ok(),
        ],
    );
    model
}

fn gaming() -> ServerId {
    ServerId::from_central(1)
}

fn key(channel: &str) -> (ServerId, ChannelId) {
    (gaming(), channel.into())
}

fn presence(channel: &str, uids: &[Uid]) -> CoreEvent {
    CoreEvent::VoicePresenceUpdated(VoicePresenceUpdatedPayload {
        server_id: "1".into(),
        channel_id: channel.into(),
        participants: uids.iter().map(|u| format!("u{u}")).collect(),
        participant_uids: uids.to_vec(),
        user_states: uids
            .iter()
            .map(|u| VoiceUserStatePayload {
                username: format!("u{u}"),
                uid: *u,
                is_muted: false,
                is_deafened: false,
                is_server_muted: false,
                is_server_deafened: false,
                is_suppressed: false,
            })
            .collect(),
        user_capabilities: vec![],
    })
}

fn roster(model: &Model, channel: &str, cx: &mut TestAppContext) -> Vec<Uid> {
    model.live.voice.rosters.read_with(cx, |r, _| r.roster(&key(channel)).to_vec())
}

fn participants(model: &Model, cx: &mut TestAppContext) -> Vec<Uid> {
    model.live.voice.participants.read_with(cx, |p, _| p.uids().collect())
}

fn stream(owner: Uid) -> StreamInfoPayload {
    StreamInfoPayload {
        stream_id: format!("{owner}_screen"),
        owner_username: format!("u{owner}"),
        owner_uid: owner,
        has_audio: false,
        resolution_width: 1920,
        resolution_height: 1080,
        fps: 60,
        current_codec: 1,
        enforced_codec: 0,
        watcher_count: 0,
    }
}

fn streams(channel: &str, owners: &[Uid]) -> CoreEvent {
    CoreEvent::StreamPresenceUpdated(StreamPresenceUpdatedPayload {
        server_id: "1".into(),
        channel_id: channel.into(),
        streams: owners.iter().map(|o| stream(*o)).collect(),
    })
}

// ── Voice ───────────────────────────────────────────────────────────────────

/// Our own join shows at once; others' presence arriving meanwhile is corrected; the
/// join / leave sounds play for others in the connected channel only; a moderator's move
/// rebuilds the stage from the destination's roster; a disconnect takes us out; a
/// community's re-authentication drops its rosters until its snapshot arrives.
#[gpui::test]
fn voice_rosters_under_event_sequences(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let cues = cues(&model, cx);
    deliver(&model, cx, [presence("lounge", &[2]), presence("squad", &[3, 4])]);
    cx.update(|cx| crate::actions::voice::enter_channel(&model, key("lounge"), true, false, cx));
    assert_eq!(roster(&model, "lounge", cx), vec![2, ME], "shown in at once");
    assert_eq!(participants(&model, cx), vec![2, ME]);
    // Someone joins before the server processed our JOIN: corrected, and a join sound.
    deliver(&model, cx, [presence("lounge", &[2, 5])]);
    assert_eq!(roster(&model, "lounge", cx), vec![2, 5, ME]);
    assert_eq!(*cues.borrow(), vec![Cue::Play(Sound::UserJoin)]);
    // Elsewhere: no sound.
    deliver(&model, cx, [presence("squad", &[3])]);
    assert_eq!(cues.borrow().len(), 1);
    // The server's confirmation; then 2 leaves.
    deliver(&model, cx, [presence("lounge", &[2, 5, ME]), presence("lounge", &[5, ME])]);
    assert_eq!(roster(&model, "lounge", cx), vec![5, ME]);
    assert_eq!(cues.borrow().last(), Some(&Cue::Play(Sound::UserLeave)));
    assert!(!model.live.voice.rosters.read_with(cx, |r, _| r.has_pending_moves()));

    // A moderator moves us to squad: its roster arrived first (with us in it).
    deliver(
        &model,
        cx,
        [
            presence("squad", &[3, ME]),
            presence("lounge", &[5]),
            CoreEvent::VoiceForceNotify(VoiceForceNotifyPayload {
                server_id: "1".into(),
                action: "moved".into(),
                channel_id: "squad".into(),
                actor: "carol".into(),
                actor_uid: 3,
            }),
        ],
    );
    model.live.voice.session.read_with(cx, |s, _| assert_eq!(s.connected(), Some(key("squad"))));
    assert_eq!(participants(&model, cx), vec![3, ME]);
    assert!(toasts(&model, cx).contains(&(Severity::Info, "Moved".into(), Some("carol moved you to squad.".into()))));

    // Then disconnects us.
    deliver(
        &model,
        cx,
        [CoreEvent::VoiceForceNotify(VoiceForceNotifyPayload {
            server_id: "1".into(),
            action: "disconnected".into(),
            channel_id: String::new(),
            actor: "carol".into(),
            actor_uid: 3,
        })],
    );
    model.live.voice.session.read_with(cx, |s, _| assert!(s.connected().is_none()));
    assert!(participants(&model, cx).is_empty());
    assert_eq!(roster(&model, "squad", cx), vec![3], "shown out at once");
    assert!(toasts(&model, cx).iter().any(|t| t.1 == "Disconnected"));
    // The server agrees; the queue empties.
    deliver(&model, cx, [presence("squad", &[3])]);
    assert!(!model.live.voice.rosters.read_with(cx, |r, _| r.has_pending_moves()));

    // A reconnect: the re-authentication drops the community's rosters (its snapshot
    // lists only non-empty channels), then the snapshot fills them again.
    deliver(&model, cx, [auth_ok()]);
    assert!(roster(&model, "squad", cx).is_empty() && roster(&model, "lounge", cx).is_empty());
    deliver(&model, cx, [presence("squad", &[3, 4])]);
    assert_eq!(roster(&model, "squad", cx), vec![3, 4]);
    // A channel list without squad drops it (hidden or deleted).
    deliver(
        &model,
        cx,
        [CoreEvent::ChannelListUpdated(ChannelListUpdatedPayload { server_id: "1".into(), channels: vec![channel("lounge", "voice")] })],
    );
    assert!(roster(&model, "squad", cx).is_empty());
}

/// A join nothing confirms (the server never answered) is shown as the server has it
/// after 10 s.
#[gpui::test]
fn an_unconfirmed_join_expires(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    cx.update(|cx| crate::actions::voice::enter_channel(&model, key("lounge"), true, false, cx));
    assert_eq!(roster(&model, "lounge", cx), vec![ME]);
    cx.executor().advance_clock(SELF_MOVE_TIMEOUT + Duration::from_millis(50));
    cx.run_until_parked();
    assert!(roster(&model, "lounge", cx).is_empty());
}

/// Speaking is its own entity: a speaking event notifies nothing else (the stage, the
/// session and the rosters stay put).
#[gpui::test]
fn speaking_is_apart_from_the_stage(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    deliver(&model, cx, [presence("lounge", &[2])]);
    cx.update(|cx| crate::actions::voice::enter_channel(&model, key("lounge"), true, false, cx));
    let counts: Rc<RefCell<[u32; 4]>> = Rc::default();
    cx.update(|cx| {
        let v = model.live.voice.clone();
        let c = counts.clone();
        cx.observe(&v.speaking, move |_, _| c.borrow_mut()[0] += 1).detach();
        let c = counts.clone();
        cx.observe(&v.session, move |_, _| c.borrow_mut()[1] += 1).detach();
        let c = counts.clone();
        cx.observe(&v.participants, move |_, _| c.borrow_mut()[2] += 1).detach();
        let c = counts.clone();
        cx.observe(&v.rosters, move |_, _| c.borrow_mut()[3] += 1).detach();
    });
    deliver(
        &model,
        cx,
        [
            CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid: 2, speaking: true }),
            CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid: 0, speaking: true }),
            CoreEvent::VoiceUserSpeaking(VoiceUserSpeakingPayload { uid: 2, speaking: true }),
        ],
    );
    assert_eq!(counts.borrow()[0], 1, "one batch, one notify");
    assert_eq!(&counts.borrow()[1..], &[0, 0, 0]);
    model.live.voice.speaking.read_with(cx, |s, _| assert!(s.is_speaking(ME), "uid 0 is ourselves"));
}

/// Streams in the connected channel: start / stop sounds for others, a watched stream
/// that ended drops its watch and focus.
#[gpui::test]
fn streams_in_the_connected_channel(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    cx.update(|cx| crate::actions::voice::enter_channel(&model, key("lounge"), true, false, cx));
    let cues = cues(&model, cx);
    deliver(&model, cx, [streams("lounge", &[2]), streams("squad", &[9])]);
    assert_eq!(*cues.borrow(), vec![Cue::Play(Sound::StreamStart)]);
    cx.update(|cx| crate::actions::voice::focus_stream(2, cx));
    model.live.voice.watching.update(cx, |w, cx| w.set_fullscreen(true, cx));
    deliver(&model, cx, [streams("lounge", &[])]);
    assert_eq!(cues.borrow().last(), Some(&Cue::Play(Sound::StreamStop)));
    model.live.voice.watching.read_with(cx, |w, _| {
        assert!(w.list().is_empty() && w.focused.is_none() && !w.fullscreen);
    });
    model.live.voice.streams.read_with(cx, |s, _| assert!(s.of_user(9).is_some(), "other channels keep theirs"));
}

/// Strict voice: without keys the passphrase prompt opens instead of a join.
#[gpui::test]
fn no_keys_no_voice(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    deliver(
        &model,
        cx,
        [CoreEvent::E2eeStatusChanged(E2eeStatusPayload { supported: true, status: "not_set_up".into(), key_id: 0, fingerprint: String::new() })],
    );
    cx.update(|cx| crate::actions::voice::click_voice_channel(gaming(), "lounge".into(), cx));
    cx.run_until_parked();
    model.live.e2ee.read_with(cx, |e, _| assert_eq!(e.passphrase.as_ref().map(|f| f.mode), Some(PassphraseMode::Setup)));
    assert!(toasts(&model, cx).iter().any(|t| t.1 == "Set up encryption to join voice"));
    model.live.voice.session.read_with(cx, |s, _| assert!(s.connected().is_none()));
    // Locked: the unlock prompt (it also opened on its own at the first report).
    deliver(
        &model,
        cx,
        [CoreEvent::E2eeStatusChanged(E2eeStatusPayload { supported: true, status: "locked".into(), key_id: 1, fingerprint: String::new() })],
    );
    model.live.e2ee.read_with(cx, |e, _| assert_eq!(e.status, E2eeStatus::Locked));
}

// ── Calls ───────────────────────────────────────────────────────────────────

#[derive(Default)]
struct FakeCalls {
    sent: RefCell<Vec<(&'static str, String, Uid)>>,
    connects: RefCell<Vec<String>>,
    ends: RefCell<u32>,
}

struct Fake(Rc<FakeCalls>);

impl CallTransport for Fake {
    fn config(&self) -> LocalBoxFuture<'static, Result<(bool, Vec<String>), UiError>> {
        Box::pin(async { Ok((true, vec![])) })
    }
    fn prepare(&self, _: SharedString, _: Uid, _: SharedString) -> LocalBoxFuture<'static, Result<Prepared, UiError>> {
        Box::pin(async { Ok(Prepared { pub_key: "our-key".into(), candidates: vec![] }) })
    }
    fn send(&self, s: OutSignal) -> LocalBoxFuture<'static, Result<(), UiError>> {
        self.0.sent.borrow_mut().push((s.kind, s.call_id.to_string(), s.to_uid));
        Box::pin(async { Ok(()) })
    }
    fn connect(&self, a: ConnectArgs) -> LocalBoxFuture<'static, Result<(), UiError>> {
        self.0.connects.borrow_mut().push(a.call_id.to_string());
        Box::pin(async { Ok(()) })
    }
    fn end(&self) -> LocalBoxFuture<'static, Result<(), UiError>> {
        *self.0.ends.borrow_mut() += 1;
        Box::pin(async { Ok(()) })
    }
    fn watch_stream(&self, _: bool) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async { Ok(()) })
    }
    fn new_call_id(&self) -> SharedString {
        "call-out".into()
    }
}

fn with_fake_calls(model: &Model, cx: &mut TestAppContext) -> Rc<FakeCalls> {
    let fake = Rc::new(FakeCalls::default());
    model.live.calls.update(cx, |c, cx| {
        c.set_transport(Rc::new(Fake(fake.clone())));
        c.set_config(true, vec![], cx);
    });
    fake
}

fn signal(kind: &str, call_id: &str, from: Uid) -> CoreEvent {
    CoreEvent::CallSignal(CallSignalPayload {
        kind: kind.into(),
        call_id: call_id.into(),
        from: format!("u{from}"),
        from_uid: from,
        to: "perf2".into(),
        pub_key: matches!(kind, "INVITE" | "ACCEPT").then(|| "their-key".into()),
        candidates: vec![],
        stream: None,
        timestamp: 0,
        pub_key_sig: None,
        key_id: 0,
    })
}

fn status(model: &Model, cx: &mut TestAppContext) -> CallStatus {
    model.live.calls.read_with(cx, |c, _| c.status)
}

/// Outgoing: INVITE after prepare, the ring loops, RINGING acks, and 45 s without an
/// answer cancels it ("No answer"); the timer exists only while ringing.
#[gpui::test]
fn an_unanswered_call_times_out(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let fake = with_fake_calls(&model, cx);
    let cues = cues(&model, cx);
    cx.update(|cx| crate::actions::calls::start_call(9, cx));
    cx.run_until_parked();
    assert_eq!(status(&model, cx), CallStatus::Outgoing);
    assert_eq!(fake.sent.borrow()[0], ("INVITE", "call-out".into(), 9));
    assert_eq!(*cues.borrow(), vec![Cue::StartRing]);
    assert!(model.live.calls.read_with(cx, |c, _| c.ring_armed()));
    deliver(&model, cx, [signal("RINGING", "call-out", 9)]);
    assert!(model.live.calls.read_with(cx, |c, _| c.ringing_acked));
    cx.executor().advance_clock(RING_TIMEOUT - Duration::from_millis(1));
    cx.run_until_parked();
    assert_eq!(status(&model, cx), CallStatus::Outgoing);
    cx.executor().advance_clock(Duration::from_millis(2));
    cx.run_until_parked();
    assert_eq!(status(&model, cx), CallStatus::Idle);
    model.live.calls.read_with(cx, |c, _| {
        assert_eq!(c.end_reason.as_deref(), Some("No answer"));
        assert!(!c.ring_armed());
    });
    assert_eq!(fake.sent.borrow().last().unwrap().0, "CANCEL");
    assert_eq!(cues.borrow().last(), Some(&Cue::StopRing));
    assert_eq!(*fake.ends.borrow(), 1);
}

/// Incoming on this device: RINGING back, the ring and a flash; another device answers →
/// the card shows "Answered on another device"; a second INVITE while in a call → BUSY.
#[gpui::test]
fn incoming_answered_elsewhere_and_busy(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let fake = with_fake_calls(&model, cx);
    let cues = cues(&model, cx);
    deliver(&model, cx, [signal("INVITE", "c1", 9)]);
    assert_eq!(status(&model, cx), CallStatus::Incoming);
    assert_eq!(fake.sent.borrow()[0], ("RINGING", "c1".into(), 9));
    assert_eq!(*cues.borrow(), vec![Cue::StartRing, Cue::Flash]);
    deliver(&model, cx, [signal("ANSWERED_ELSEWHERE", "c1", 9)]);
    model.live.calls.read_with(cx, |c, _| {
        assert_eq!(c.status, CallStatus::Idle);
        let ended = c.incoming_ended.as_ref().unwrap();
        assert!(ended.answered);
        assert_eq!(ended.reason.as_ref(), "Answered on another device");
        assert!(!c.ring_armed());
    });
    assert_eq!(cues.borrow().last(), Some(&Cue::StopRing));

    // Answer one here; another INVITE meanwhile is busy.
    deliver(&model, cx, [signal("INVITE", "c2", 9)]);
    cx.update(crate::actions::calls::accept_call);
    cx.run_until_parked();
    assert_eq!(status(&model, cx), CallStatus::Connecting);
    assert!(fake.sent.borrow().iter().any(|s| *s == ("ACCEPT", "c2".into(), 9)));
    assert_eq!(*fake.connects.borrow(), vec!["c2".to_string()]);
    model.nav.read_with(cx, |n, _| assert_eq!(n.active_view, ActiveView::Dm));
    deliver(&model, cx, [signal("INVITE", "c3", 4)]);
    assert_eq!(fake.sent.borrow().last().unwrap(), &("BUSY", "c3".into(), 4));
    // Connected: the voice session runs the call with no channel; hang up from the peer.
    deliver(&model, cx, [CoreEvent::CallConnected(CallConnectedPayload { call_id: "c2".into(), rtt_ms: 1.0, path: "host".into(), verified: true })]);
    assert_eq!(status(&model, cx), CallStatus::Active);
    model.live.voice.session.read_with(cx, |s, _| assert_eq!((s.call_peer, s.connected()), (Some(9), None)));
    assert_eq!(participants(&model, cx), vec![ME, 9]);
    deliver(&model, cx, [signal("HANGUP", "c2", 9)]);
    assert_eq!(status(&model, cx), CallStatus::Idle);
    model.live.voice.session.read_with(cx, |s, _| assert!(s.call_peer.is_none()));
    assert_eq!(cues.borrow().last(), Some(&Cue::Play(Sound::CallEnd)));
}

/// Do Not Disturb silences only the ring and the flash: the call still shows, and goes
/// to "Missed call" after 45 s.
#[gpui::test]
fn dnd_silences_only_the_ring(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let _fake = with_fake_calls(&model, cx);
    deliver(
        &model,
        cx,
        [CoreEvent::OwnPresence(OwnPresencePayload {
            supported: true,
            status: "dnd".into(),
            status_text: String::new(),
            status_emoji: String::new(),
            status_clear_in: 0.0,
            dm_friends_only: false,
            account_idle: false,
        })],
    );
    let cues = cues(&model, cx);
    deliver(&model, cx, [signal("INVITE", "c1", 9)]);
    assert_eq!(status(&model, cx), CallStatus::Incoming);
    assert!(cues.borrow().is_empty());
    cx.executor().advance_clock(RING_TIMEOUT);
    cx.run_until_parked();
    model.live.calls.read_with(cx, |c, _| assert_eq!((c.status, c.end_reason.as_deref()), (CallStatus::Idle, Some("Missed call"))));
    assert!(toasts(&model, cx).iter().any(|t| t.1 == "Missed call"));
}

/// Glare: both rang each other. The higher uid drops its own and answers theirs.
#[gpui::test]
fn glare_resolves_by_uid(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let fake = with_fake_calls(&model, cx);
    cx.update(|cx| crate::actions::calls::start_call(3, cx));
    cx.run_until_parked();
    deliver(&model, cx, [signal("INVITE", "theirs", 3)]);
    cx.run_until_parked();
    let sent = fake.sent.borrow();
    assert!(sent.contains(&("CANCEL", "call-out".into(), 3)), "{sent:?}");
    assert!(sent.contains(&("ACCEPT", "theirs".into(), 3)));
    drop(sent);
    assert_eq!(status(&model, cx), CallStatus::Connecting);
}

// ── Presence, friends, hotkeys, devices, sign-out ───────────────────────────

#[gpui::test]
fn presence_routing(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let user = |uid: Uid, status: &str, text: &str| UserPresencePayload {
        username: format!("u{uid}"),
        uid,
        avatar_version: if uid == 2 { "v2".into() } else { String::new() },
        status: status.into(),
        status_text: text.into(),
        status_emoji: String::new(),
    };
    deliver(
        &model,
        cx,
        [
            CoreEvent::FriendListReceived(FriendListReceivedPayload {
                friends: vec![
                    FriendInfoPayload { username: "u2".into(), uid: 2, status: "offline".into(), avatar_version: "v2".into() },
                    FriendInfoPayload { username: "u3".into(), uid: 3, status: "online".into(), avatar_version: String::new() },
                ],
            }),
            CoreEvent::UserListUpdated(UserListUpdatedPayload {
                users: vec![user(2, "dnd", "busy")],
                covered: vec![2, 3],
                avatars: vec![],
            }),
            CoreEvent::OwnPresence(OwnPresencePayload {
                supported: true,
                status: "online".into(),
                status_text: String::new(),
                status_emoji: String::new(),
                status_clear_in: 0.0,
                dm_friends_only: true,
                account_idle: true,
            }),
        ],
    );
    cx.read(|cx| {
        let p = &model.live.presence;
        assert_eq!(p.display(ME, 2, false, cx), DisplayStatus::Dnd);
        assert_eq!(p.display(ME, 3, true, cx), DisplayStatus::Offline);
        assert_eq!(p.display(ME, ME, false, cx), DisplayStatus::Away, "every device idle");
        assert_eq!(p.message(ME, 2, cx).map(|m| m.text.to_string()), Some("busy".into()));
        let friends = model.live.friends.read(cx);
        assert_eq!(friends.get(2).unwrap().status, crate::live::friends::FriendStatus::Online);
        assert_eq!(friends.get(3).unwrap().status, crate::live::friends::FriendStatus::Offline);
        assert!(model.settings.read(cx).get().friends_only_dms, "the account's value is adopted");
        assert_eq!(p.avatars.read(cx).get(2).unwrap().version.as_ref(), "v2");
    });
    // Invisible looks offline: central reports the user offline.
    deliver(
        &model,
        cx,
        [CoreEvent::PresenceDelta(PresenceDeltaPayload {
            username: "u2".into(),
            uid: 2,
            online: false,
            status: String::new(),
            avatar_version: String::new(),
            status_text: String::new(),
            status_emoji: String::new(),
        })],
    );
    cx.read(|cx| {
        assert_eq!(model.live.presence.display(ME, 2, true, cx), DisplayStatus::Offline);
        assert!(model.live.presence.message(ME, 2, cx).is_none());
        assert_eq!(model.live.friends.read(cx).get(2).unwrap().status, crate::live::friends::FriendStatus::Offline);
    });
    // Central's connection drops: statuses are stale, lists fall back to communities'.
    deliver(&model, cx, [CoreEvent::ConnectionLost(ConnectionEventPayload { server_type: "central".into(), server_id: None })]);
    cx.read(|cx| assert_eq!(model.live.presence.display(ME, 3, true, cx), DisplayStatus::Online));
}

/// The compositor's idle edges (`SystemIdleChanged`, which comes once `CoreIdleSource`
/// subscribed on Wayland) drive auto-away at once: Away on `idled`, back on `resumed`, our own
/// idle flag following, no timer either way; an edge for another threshold is only a reason to
/// read again.
#[gpui::test]
fn system_idle_edges_drive_auto_away(cx: &mut TestAppContext) {
    use crate::live::presence::{IdleReading, IdleSource};
    struct Pushing(Rc<std::cell::Cell<u32>>);
    impl IdleSource for Pushing {
        fn read(&self, _: Duration) -> LocalBoxFuture<'static, IdleReading> {
            self.0.set(self.0.get() + 1);
            Box::pin(async { IdleReading::Active { idle_for: None } })
        }
        fn pushes_resume(&self) -> bool {
            true
        }
    }
    let model = signed_in(cx);
    let reads = Rc::new(std::cell::Cell::new(0));
    model.live.presence.auto_away.update(cx, |a, cx| a.set_source(Rc::new(Pushing(reads.clone())), cx));
    deliver(
        &model,
        cx,
        [CoreEvent::OwnPresence(OwnPresencePayload {
            supported: true,
            status: "online".into(),
            status_text: String::new(),
            status_emoji: String::new(),
            status_clear_in: 0.0,
            dm_friends_only: false,
            account_idle: false,
        })],
    );
    let threshold_seconds = model.settings.read_with(cx, |s, _| s.get().away_after_minutes) * 60;
    assert!(threshold_seconds > 0);
    assert_eq!(reads.get(), 1, "configured: one read");
    let edge = |idle: bool, threshold_seconds: u32| {
        CoreEvent::SystemIdleChanged(SystemIdleChangedPayload { idle, threshold_seconds })
    };
    let state = |model: &Model, cx: &mut TestAppContext| {
        cx.read(|cx| {
            let away = model.live.presence.auto_away.read(cx);
            (away.idle(), model.live.presence.own.read(cx).idle, away.timer_armed())
        })
    };
    deliver(&model, cx, [edge(true, threshold_seconds)]);
    assert_eq!(state(&model, cx), (true, true, false), "Away at the edge, no timer");
    deliver(&model, cx, [edge(false, threshold_seconds)]);
    assert_eq!(state(&model, cx), (false, false, false), "back at the resume edge");
    assert_eq!(reads.get(), 1, "edges are readings: nothing read");
    deliver(&model, cx, [edge(true, threshold_seconds + 60)]);
    assert_eq!(state(&model, cx), (false, false, false), "another threshold's edge is not believed");
    assert_eq!(reads.get(), 2, "but read again");
}

/// Press actions route to the voice and call actions; holds and releases do nothing here.
#[gpui::test]
fn hotkey_actions_route(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let fake = with_fake_calls(&model, cx);
    let cues = cues(&model, cx);
    let hk = |action, pressed| CoreEvent::HotkeyAction(HotkeyActionPayload { action, pressed });
    deliver(&model, cx, [hk(Action::ToggleMute, true), hk(Action::ToggleMute, false), hk(Action::PushToTalk, true)]);
    assert_eq!(*cues.borrow(), vec![Cue::Play(Sound::Mute)]);
    deliver(&model, cx, [CoreEvent::VoiceStateChanged(VoiceStateChangedPayload { is_muted: true, is_deafened: false })]);
    deliver(&model, cx, [hk(Action::ToggleMute, true), hk(Action::ToggleDeafen, true)]);
    assert_eq!(&cues.borrow()[1..], &[Cue::Play(Sound::Unmute), Cue::Play(Sound::Deafen)]);
    // Answer only while ringing.
    deliver(&model, cx, [hk(Action::AnswerCall, true)]);
    assert!(fake.sent.borrow().is_empty());
    deliver(&model, cx, [signal("INVITE", "c1", 9), hk(Action::DeclineCall, true)]);
    cx.run_until_parked();
    assert_eq!(fake.sent.borrow().last().unwrap(), &("REJECT", "c1".into(), 9));
    assert_eq!(status(&model, cx), CallStatus::Idle);
}

#[gpui::test]
fn a_new_device_is_announced_and_sign_out_clears_people(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    deliver(
        &model,
        cx,
        [CoreEvent::DevicesChanged(DevicesChangedPayload {
            new_device: Some(DevicePayload {
                id: 5,
                name: String::new(),
                platform: "windows".into(),
                app_version: "0.9".into(),
                address: "203.0.113.4".into(),
                signed_in_at: 0,
                last_active_at: 0,
                online: true,
                current: false,
            }),
        })],
    );
    let toast = model.toasts.read_with(cx, |t, _| {
        let t = &t.toasts()[0];
        (t.title.to_string(), t.body.as_ref().map(|b| b.to_string()), t.duration, t.action.is_some())
    });
    assert_eq!(toast.0, "New sign-in to your account");
    assert_eq!(toast.1.as_deref(), Some("Windows · 203.0.113.4. If this wasn't you, sign that device out and change your password."));
    assert_eq!((toast.2, toast.3), (Duration::from_secs(20), true));
    model.live.devices.read_with(cx, |d, _| assert_eq!(d.revision, 1));

    deliver(&model, cx, [presence("lounge", &[2])]);
    cx.update(|cx| crate::actions::voice::enter_channel(&model, key("lounge"), true, false, cx));
    deliver(&model, cx, [CoreEvent::LoggedOut(LoggedOutPayload { reason: None })]);
    cx.read(|cx| {
        let v = &model.live.voice;
        assert!(v.session.read(cx).connected().is_none());
        assert!(v.rosters.read(cx).keys().next().is_none());
        assert!(model.live.friends.read(cx).list().is_empty());
        assert!(!model.live.presence.own.read(cx).supported);
    });
}

/// The membership revocation notice goes after 8 s.
#[gpui::test]
fn the_revocation_notice_lives_eight_seconds(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    deliver(
        &model,
        cx,
        [CoreEvent::MembershipRevoked(MembershipRevokedPayload {
            server_id: "1".into(),
            action: "kick".into(),
            reason: String::new(),
            actor: "alice".into(),
            actor_uid: 1,
            expires_at: 0,
        })],
    );
    assert!(model.overlays.read_with(cx, |o, _| o.membership_revocation.is_some()));
    cx.executor().advance_clock(crate::live::REVOCATION_NOTICE_LIFETIME);
    cx.run_until_parked();
    assert!(model.overlays.read_with(cx, |o, _| o.membership_revocation.is_none()));
    assert_eq!(crate::live::revocation_headline("kick", "Daily Gaming"), "You have been kicked from Daily Gaming");
}

/// The OS window title follows the view, and in the DM view the open conversation (M2's
/// DM list) under its current name.
#[gpui::test]
fn the_window_title(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    cx.read(|cx| assert_eq!(crate::live::window_title(&model, cx).as_ref(), "Decibell"));
    deliver(
        &model,
        cx,
        [CoreEvent::MembershipsReceived(MembershipsReceivedPayload {
            memberships: vec![ServerInfo {
                id: 1,
                name: "Daily Gaming".into(),
                description: String::new(),
                host_ip: "127.0.0.11".into(),
                port: 8082,
                member_count: 1,
                picture_version: String::new(),
            }],
        })],
    );
    cx.update(|cx| crate::actions::servers::open_server(gaming(), cx));
    cx.read(|cx| assert_eq!(crate::live::window_title(&model, cx).as_ref(), "Daily Gaming — Decibell"));
    // The DM view with no conversation open.
    model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Dm, cx));
    cx.read(|cx| assert_eq!(crate::live::window_title(&model, cx).as_ref(), "Decibell"));
    // A conversation opened (its row's name is the fallback), then renamed by central.
    cx.update(|cx| crate::actions::dm::open_dm(2, "bob".into(), cx));
    cx.read(|cx| assert_eq!(crate::live::window_title(&model, cx).as_ref(), "bob — Decibell"));
    deliver(&model, cx, [CoreEvent::UserRenamed(UserRenamedPayload { uid: 2, username: "robert".into(), is_self: false })]);
    cx.read(|cx| assert_eq!(crate::live::window_title(&model, cx).as_ref(), "robert — Decibell"));
    // Back in the community: its name again.
    model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Server, cx));
    cx.read(|cx| assert_eq!(crate::live::window_title(&model, cx).as_ref(), "Daily Gaming — Decibell"));
}
