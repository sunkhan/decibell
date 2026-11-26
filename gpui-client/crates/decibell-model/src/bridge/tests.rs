//! The seams between the chat half and the live half, through the real queue and pump
//! (`Model::init` without the core: every core call answers "not running" at once; calls,
//! history requests, uploads and downloads go through fakes where a test needs them).

use std::cell::RefCell;
use std::rc::Rc;

use decibell_core::events::*;
use futures::future::LocalBoxFuture;
use gpui::{SharedString, TestAppContext};

use crate::Model;
use crate::chat::history::{HistoryRequest, WaitKey};
use crate::error::UiError;
use crate::ids::{ServerId, Uid};
use crate::live::calls::{CallStatus, CallTransport, ConnectArgs, OutSignal, Prepared};
use crate::pump::Incoming;
use crate::stores::navigation::ActiveView;

const ME: Uid = 7;

fn deliver(model: &Model, cx: &mut TestAppContext, events: impl IntoIterator<Item = CoreEvent>) {
    for event in events {
        model.sink().queue().push(Incoming::Core(event));
    }
    cx.run_until_parked();
}

fn gaming() -> ServerId {
    ServerId::from_central(1)
}

fn channel(id: &str) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: id.into(),
        r#type: "text".into(),
        voice_bitrate_kbps: 0,
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

/// Signed in as uid 7, a member of Daily Gaming (id 1), which answered with #general and
/// #clips.
fn signed_in(cx: &mut TestAppContext) -> Model {
    let model = cx.update(|cx| Model::init(cx, None));
    deliver(
        &model,
        cx,
        [
            CoreEvent::LoginSucceeded(LoginSucceededPayload { username: "perf2".into(), uid: ME, renames: true, devices: true }),
            CoreEvent::MembershipsReceived(MembershipsReceivedPayload {
                memberships: vec![ServerInfo {
                    id: 1,
                    name: "Daily Gaming".into(),
                    description: String::new(),
                    host_ip: "127.0.0.11".into(),
                    port: 8082,
                    member_count: 1,
                    picture_version: String::new(),
                }],
            }),
            CoreEvent::CommunityAuthResponded(CommunityAuthRespondedPayload {
                server_id: "1".into(),
                requested_server_id: "1".into(),
                host: "127.0.0.11".into(),
                port: 8082,
                success: true,
                message: String::new(),
                channels: vec![channel("general"), channel("clips")],
                error_code: String::new(),
                server_name: "Daily Gaming".into(),
                server_description: String::new(),
                owner_username: "alice".into(),
                owner_uid: 1,
                attachment_port: 0,
                max_attachment_bytes: 0,
                public_listing: true,
            }),
        ],
    );
    model
}

/// History requests leave through a recorder that says "sent".
fn record_history(model: &Model, cx: &mut TestAppContext) -> Rc<RefCell<Vec<(WaitKey, HistoryRequest)>>> {
    let sent: Rc<RefCell<Vec<(WaitKey, HistoryRequest)>>> = Rc::default();
    let s = sent.clone();
    model.chat.history.update(cx, |h, _| {
        h.set_sender(Rc::new(move |key, request, cx, done| {
            s.borrow_mut().push((key, request));
            done(true, cx);
        }))
    });
    sent
}

fn attachment(id: i64, message_id: i64, filename: &str) -> AttachmentPayload {
    AttachmentPayload {
        id,
        message_id,
        kind: "video".into(),
        filename: filename.into(),
        mime: "video/mp4".into(),
        size_bytes: 4096,
        url: String::new(),
        position: 0,
        created_at: 0,
        purged_at: 0,
        width: 0,
        height: 0,
        thumbnail_size_bytes: 0,
        thumbnail_sizes_mask: 0,
        duration_ms: 0,
        placeholder: String::new(),
        encrypted: false,
    }
}

fn stored(id: i64, sender_uid: Uid, attachments: Vec<AttachmentPayload>) -> ChannelMessagePayload {
    ChannelMessagePayload {
        id,
        sender: format!("u{sender_uid}"),
        sender_uid,
        channel_id: "general".into(),
        content: format!("m{id}"),
        timestamp: 1_764_000_000 + id,
        attachments,
        nonce: String::new(),
        edited_at: 0,
        reply_to: 0,
        reply_to_sender: String::new(),
        reply_to_sender_uid: 0,
        reply_to_content: String::new(),
        reply_to_attachment_kinds: vec![],
        encrypted: false,
        decrypt_error: String::new(),
        encrypted_attachments: vec![],
        suppress_embeds: false,
    }
}

/// #general's newest page: messages 1..=5, message 5 from uid 2 carrying attachment 77.
fn general_page() -> CoreEvent {
    CoreEvent::ChannelHistoryReceived(ChannelHistoryReceivedPayload {
        server_id: "1".into(),
        channel_id: "general".into(),
        messages: (1..=5).rev().map(|id| stored(id, 2, if id == 5 { vec![attachment(77, 5, "clip.mp4")] } else { vec![] })).collect(),
        has_more: false,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 0,
    })
}

fn dm_page(peer: Uid, ids: std::ops::RangeInclusive<i64>, content: &str) -> CoreEvent {
    CoreEvent::DmHistoryReceived(DmHistoryReceivedPayload {
        peer: format!("u{peer}"),
        peer_uid: peer,
        messages: ids
            .rev()
            .map(|id| DmHistoryMessagePayload {
                id,
                sender: format!("u{peer}"),
                sender_uid: peer,
                content: content.into(),
                timestamp: 1_764_000_000 + id,
                edited_at: 0,
                reply_to: 0,
                reply_to_sender: String::new(),
                reply_to_sender_uid: 0,
                reply_to_content: String::new(),
                encrypted: true,
                decrypt_error: if content.is_empty() { "locked".into() } else { String::new() },
            })
            .collect(),
        has_more: false,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 0,
    })
}

fn e2ee(status: &str) -> CoreEvent {
    CoreEvent::E2eeStatusChanged(E2eeStatusPayload { supported: true, status: status.into(), key_id: 1, fingerprint: String::new() })
}

fn dm_rows(model: &Model, peer: Uid, cx: &mut TestAppContext) -> Vec<(i64, String)> {
    cx.read(|cx| {
        model.dm.list.read(cx).slice(peer).map_or_else(Vec::new, |s| {
            s.read(cx).rows().iter().map(|m| (m.id, m.content.to_string())).collect()
        })
    })
}

// ── Calls → DMs ─────────────────────────────────────────────────────────────

#[derive(Default)]
struct CallLog {
    sent: RefCell<Vec<&'static str>>,
}

struct FakeCalls(Rc<CallLog>);

impl CallTransport for FakeCalls {
    fn config(&self) -> LocalBoxFuture<'static, Result<(bool, Vec<String>), UiError>> {
        Box::pin(async { Ok((true, vec![])) })
    }
    fn prepare(&self, _: SharedString, _: Uid, _: SharedString) -> LocalBoxFuture<'static, Result<Prepared, UiError>> {
        Box::pin(async { Ok(Prepared { pub_key: "our-key".into(), candidates: vec![] }) })
    }
    fn send(&self, s: OutSignal) -> LocalBoxFuture<'static, Result<(), UiError>> {
        self.0.sent.borrow_mut().push(s.kind);
        Box::pin(async { Ok(()) })
    }
    fn connect(&self, _: ConnectArgs) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async { Ok(()) })
    }
    fn end(&self) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async { Ok(()) })
    }
    fn watch_stream(&self, _: bool) -> LocalBoxFuture<'static, Result<(), UiError>> {
        Box::pin(async { Ok(()) })
    }
    fn new_call_id(&self) -> SharedString {
        "call-out".into()
    }
}

/// Answering a call lands on the caller's DM (`acceptCall`: `setActiveDmUser` + the DM
/// view): the conversation opens (created when new) under the caller's name, and the window
/// title follows it.
#[gpui::test]
fn answering_a_call_opens_the_callers_conversation(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let log = Rc::new(CallLog::default());
    model.live.calls.update(cx, |c, cx| {
        c.set_transport(Rc::new(FakeCalls(log.clone())));
        c.set_config(true, vec![], cx);
    });
    deliver(
        &model,
        cx,
        [CoreEvent::CallSignal(CallSignalPayload {
            kind: "INVITE".into(),
            call_id: "c1".into(),
            from: "u9".into(),
            from_uid: 9,
            to: "perf2".into(),
            pub_key: Some("their-key".into()),
            candidates: vec![],
            stream: None,
            timestamp: 0,
            pub_key_sig: None,
            key_id: 0,
        })],
    );
    assert_eq!(model.live.calls.read_with(cx, |c, _| c.status), CallStatus::Incoming);
    assert!(cx.read(|cx| model.dm.list.read(cx).get(9).is_none()), "ringing opens nothing");
    cx.update(crate::actions::calls::accept_call);
    cx.run_until_parked();
    assert_eq!(model.live.calls.read_with(cx, |c, _| c.status), CallStatus::Connecting);
    assert!(log.sent.borrow().contains(&"ACCEPT"));
    cx.read(|cx| {
        let list = model.dm.list.read(cx);
        assert_eq!(list.active_peer, Some(9));
        assert_eq!(list.get(9).map(|c| c.username.to_string()).as_deref(), Some("u9"));
        assert_eq!(model.nav.read(cx).active_view, ActiveView::Dm);
        assert_eq!(crate::live::window_title(&model, cx).as_ref(), "u9 — Decibell");
    });
}

// ── E2EE → DMs ──────────────────────────────────────────────────────────────

/// Becoming "ready" (unlock / setup) drops every loaded DM page — rows that arrived sealed
/// hold placeholders — and reloads the open conversation's newest page; other
/// conversations reload when opened; staying ready reloads nothing.
#[gpui::test]
fn unlocking_reloads_dm_history(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    deliver(&model, cx, [e2ee("locked"), dm_page(3, 1..=3, ""), dm_page(4, 1..=2, "")]);
    assert_eq!(dm_rows(&model, 3, cx).len(), 3);
    cx.update(|cx| crate::actions::dm::open_dm(3, "u3".into(), cx));
    let sent = record_history(&model, cx);

    deliver(&model, cx, [e2ee("ready")]);
    assert!(dm_rows(&model, 3, cx).is_empty(), "the placeholders went");
    assert!(dm_rows(&model, 4, cx).is_empty());
    cx.read(|cx| {
        let list = model.dm.list.read(cx);
        assert!(!list.slice(3).unwrap().read(cx).fetched);
        assert!(!list.slice(4).unwrap().read(cx).fetched, "reloads when opened");
        assert!(list.slice(4).unwrap().read(cx).has_more_before);
    });
    assert_eq!(*sent.borrow(), vec![(WaitKey::Dm(3), HistoryRequest::latest(crate::chat::history::PAGE_LIMIT))], "only the open one");
    // The fresh page lands, opened.
    deliver(&model, cx, [dm_page(3, 1..=3, "hello")]);
    assert_eq!(dm_rows(&model, 3, cx), vec![(1, "hello".into()), (2, "hello".into()), (3, "hello".into())]);
    // Another "ready" (any later status push) is no edge: nothing reloads.
    deliver(&model, cx, [e2ee("ready")]);
    assert_eq!(dm_rows(&model, 3, cx).len(), 3);
    assert_eq!(sent.borrow().len(), 1);
}

// ── The router's connection_lost → DM waits ─────────────────────────────────

/// Central's connection dropping ends the DM history waits at once (their replies went
/// down with it: `abandonDmRequests`), not at their 10 s timeout; a community dropping
/// leaves them alone.
#[gpui::test]
fn central_connection_lost_ends_dm_history_waits(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    record_history(&model, cx);
    let reply = model.chat.history.update(cx, |h, cx| h.request(WaitKey::Dm(3), HistoryRequest::latest(50), cx));
    let outcome: Rc<RefCell<Option<bool>>> = Rc::default();
    let o = outcome.clone();
    cx.spawn(async move |_| {
        let r = reply.await;
        *o.borrow_mut() = Some(r.is_some());
    })
    .detach();
    cx.run_until_parked();
    assert_eq!(model.chat.history.read_with(cx, |h, _| h.waiting()), 1);
    let lost = |server_type: &str, server_id: Option<&str>| {
        CoreEvent::ConnectionLost(ConnectionEventPayload { server_type: server_type.into(), server_id: server_id.map(Into::into) })
    };
    deliver(&model, cx, [lost("community", Some("1"))]);
    assert_eq!(model.chat.history.read_with(cx, |h, _| h.waiting()), 1, "a community's drop isn't central's");
    deliver(&model, cx, [lost("central", None)]);
    assert_eq!(model.chat.history.read_with(cx, |h, _| h.waiting()), 0);
    assert_eq!(*outcome.borrow(), Some(false), "ended without a reply, no clock advanced");
}

// ── friends_only_dms: one copy, two writers ─────────────────────────────────

/// The account's "only friends may DM me" lives in `Settings` alone: the sign-in adopts
/// central's value (a change only when it differs, so the same value again writes
/// nothing), and the Privacy toggle sets it. Nothing in the DM half writes it.
#[gpui::test]
fn friends_only_dms_has_one_copy(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    let notifies = Rc::new(RefCell::new(0));
    let n = notifies.clone();
    cx.update(|cx| cx.observe(&model.settings, move |_, _| *n.borrow_mut() += 1).detach());
    let own = |friends_only: bool| {
        CoreEvent::OwnPresence(OwnPresencePayload {
            supported: true,
            status: "online".into(),
            status_text: String::new(),
            status_emoji: String::new(),
            status_clear_in: 0.0,
            dm_friends_only: friends_only,
            account_idle: false,
        })
    };
    deliver(&model, cx, [own(true)]);
    assert!(model.settings.read_with(cx, |s, _| s.get().friends_only_dms));
    assert_eq!(*notifies.borrow(), 1);
    assert!(model.settings.read_with(cx, |s, _| s.save_pending()), "the local copy is saved");
    deliver(&model, cx, [own(true)]);
    assert_eq!(*notifies.borrow(), 1, "the same value writes nothing");
    cx.update(|cx| crate::actions::dm::set_friends_only_dms(false, cx));
    cx.run_until_parked();
    assert!(!model.settings.read_with(cx, |s, _| s.get().friends_only_dms));
    assert_eq!(*notifies.borrow(), 2);
}

// ── Sign-out across both halves ─────────────────────────────────────────────

/// Sign-out leaves nothing of the account in either half: messages, DMs, drafts, history
/// waits and searches (M2), voice / stream rosters, presence, friends, calls (M3).
#[gpui::test]
fn sign_out_resets_both_halves(cx: &mut TestAppContext) {
    let model = signed_in(cx);
    record_history(&model, cx);
    deliver(&model, cx, [general_page(), dm_page(3, 1..=2, "hi")]);
    cx.update(|cx| crate::actions::dm::open_dm(3, "u3".into(), cx));
    model.chat.drafts.update(cx, |d, cx| d.set(crate::chat::drafts::DraftKey::Dm(3), "draft".into(), cx));
    let _wait = model.chat.history.update(cx, |h, cx| h.request(WaitKey::Dm(3), HistoryRequest::before(1, 50), cx));
    deliver(
        &model,
        cx,
        [CoreEvent::VoicePresenceUpdated(VoicePresenceUpdatedPayload {
            server_id: "1".into(),
            channel_id: "lounge".into(),
            participants: vec!["u2".into()],
            participant_uids: vec![2],
            user_states: vec![],
            user_capabilities: vec![],
        })],
    );
    cx.run_until_parked();
    assert!(cx.read(|cx| model.live.voice.rosters.read(cx).roster(&(gaming(), "lounge".into())).len()) == 1);

    deliver(&model, cx, [CoreEvent::LoggedOut(LoggedOutPayload { reason: None })]);
    cx.read(|cx| {
        assert!(model.chat.channels.read(cx).is_empty());
        assert!(model.dm.list.read(cx).is_empty());
        assert_eq!(model.dm.list.read(cx).active_peer, None);
        assert!(model.chat.drafts.read(cx).is_empty());
        assert_eq!(model.chat.history.read(cx).waiting(), 0);
        assert!(model.live.voice.rosters.read(cx).roster(&(gaming(), "lounge".into())).is_empty());
        assert!(model.live.friends.read(cx).list().is_empty());
        assert_eq!(model.live.calls.read(cx).status, CallStatus::Idle);
        assert_eq!(model.nav.read(cx).active_view, ActiveView::Home);
        assert_eq!(crate::live::window_title(&model, cx).as_ref(), "Decibell");
    });
}
