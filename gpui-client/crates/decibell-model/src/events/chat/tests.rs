//! The message router against hand-built event sequences, through the real queue and
//! pump (`Model::init` without the core: every core call answers "not running").

use std::time::Duration;

use decibell_core::events::*;
use gpui::{Entity, TestAppContext};

use crate::Model;
use crate::chat::drafts::DraftKey;
use crate::chat::history::{HistoryRequest, WaitKey};
use crate::chat::slice::MessageSlice;
use crate::ids::ServerId;
use crate::pump::Incoming;
use crate::stores::navigation::ActiveView;
use crate::stores::permissions::perm;

const ME: i64 = 7;

fn gaming() -> ServerId {
    ServerId::from_central(1)
}

fn deliver(model: &Model, cx: &mut TestAppContext, events: impl IntoIterator<Item = CoreEvent>) {
    for event in events {
        model.sink().queue().push(Incoming::Core(event));
    }
    cx.run_until_parked();
}

fn channel(id: &str, slowmode: i32, perms: u64) -> ChannelInfoPayload {
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
        my_permissions: perms,
        slowmode_seconds: slowmode,
        encrypted: false,
        permissions_synced: false,
        category_sync: false,
        topic: String::new(),
    }
}

const SEND: u64 = perm::VIEW_CHANNEL | perm::SEND_MESSAGES | perm::READ_HISTORY;

fn auth_ok(channels: Vec<ChannelInfoPayload>) -> CoreEvent {
    CoreEvent::CommunityAuthResponded(CommunityAuthRespondedPayload {
        server_id: "1".into(),
        requested_server_id: "1".into(),
        host: "127.0.0.11".into(),
        port: 8082,
        success: true,
        message: String::new(),
        channels,
        error_code: String::new(),
        server_name: "Daily Gaming".into(),
        server_description: "d".into(),
        owner_username: "alice".into(),
        owner_uid: 1,
        attachment_port: 0,
        max_attachment_bytes: 0,
        public_listing: true,
    })
}

fn stored(id: i64, sender_uid: i64, content: &str, nonce: &str) -> ChannelMessagePayload {
    ChannelMessagePayload {
        id,
        sender: format!("u{sender_uid}"),
        sender_uid,
        channel_id: "general".into(),
        content: content.into(),
        timestamp: 1_764_000_000 + id,
        attachments: vec![],
        nonce: nonce.into(),
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

fn live(channel: &str, id: i64, sender_uid: i64, content: &str, nonce: &str) -> CoreEvent {
    CoreEvent::MessageReceived(MessageReceivedPayload {
        context: channel.into(),
        server_id: "1".into(),
        sender: format!("u{sender_uid}"),
        sender_uid,
        recipient: String::new(),
        recipient_uid: 0,
        content: content.into(),
        timestamp: (1_764_000_000 + id).to_string(),
        id,
        attachments: vec![],
        nonce: nonce.into(),
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
    })
}

fn dm(id: i64, sender_uid: i64, recipient_uid: i64, content: &str, nonce: &str) -> CoreEvent {
    let mut e = live("dm", id, sender_uid, content, nonce);
    if let CoreEvent::MessageReceived(p) = &mut e {
        p.server_id = String::new();
        p.recipient = format!("u{recipient_uid}");
        p.recipient_uid = recipient_uid;
    }
    e
}

fn history(range: std::ops::RangeInclusive<i64>, before_id: i64, has_more: bool) -> CoreEvent {
    CoreEvent::ChannelHistoryReceived(ChannelHistoryReceivedPayload {
        server_id: "1".into(),
        channel_id: "general".into(),
        messages: range.rev().map(|id| stored(id, 2, &format!("m{id}"), "")).collect(),
        has_more,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id,
    })
}

fn signed_in(cx: &mut TestAppContext, channels: Vec<ChannelInfoPayload>) -> Model {
    let model = cx.update(|cx| Model::init(cx, None));
    deliver(
        &model,
        cx,
        [
            CoreEvent::LoginSucceeded(LoginSucceededPayload { username: "perf2".into(), uid: ME, renames: true, devices: true }),
            auth_ok(channels),
        ],
    );
    model
}

fn general(model: &Model, cx: &mut TestAppContext) -> Option<Entity<MessageSlice>> {
    cx.read(|cx| model.chat.channels.read(cx).get(gaming(), "general").cloned())
}

fn ids(slice: &Entity<MessageSlice>, cx: &mut TestAppContext) -> Vec<i64> {
    slice.read_with(cx, |s, _| s.rows().iter().map(|m| m.id).collect())
}

fn toasts(model: &Model, cx: &mut TestAppContext) -> Vec<(String, Option<String>)> {
    model.toasts.read_with(cx, |t, _| t.toasts().iter().map(|t| (t.title.to_string(), t.body.as_ref().map(|b| b.to_string()))).collect())
}

#[gpui::test]
fn live_messages_pages_and_our_echo(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![channel("general", 0, SEND)]);
    // Never loaded, nothing in flight: not cached.
    deliver(&model, cx, [live("general", 100, 2, "hi", "")]);
    assert!(general(&model, cx).is_none());
    // The newest page, then live traffic.
    deliver(&model, cx, [history(51..=100, 0, true), live("general", 101, 2, "x", "")]);
    let slice = general(&model, cx).unwrap();
    assert_eq!(ids(&slice, cx), (51..=101).collect::<Vec<_>>());
    slice.read_with(cx, |s, _| assert!(s.fetched && s.has_more_before && !s.loading));
    // An older page.
    deliver(&model, cx, [history(1..=50, 51, false)]);
    assert_eq!(ids(&slice, cx).len(), 101);
    slice.read_with(cx, |s, _| assert!(!s.has_more_before));
    // Our send: the bubble, then its echo (another device of ours would echo the same).
    model.nav.update(cx, |nav, cx| {
        nav.set_active_server(Some(gaming()), cx);
        nav.set_active_view(ActiveView::Server, cx);
        nav.set_active_channel(Some("general".into()), cx);
    });
    let outcome = cx.update(|cx| crate::actions::chat::send_message(gaming(), "general".into(), "  hello  ", 0, cx));
    let crate::actions::chat::SendOutcome::Sent { nonce } = outcome else { panic!("{outcome:?}") };
    slice.read_with(cx, |s, _| {
        let b = s.rows().last().unwrap();
        assert!(b.pending && &*b.content == "hello" && b.sent_after_id == 101);
        assert_eq!(b.nonce.as_ref(), Some(&nonce));
    });
    cx.run_until_parked();
    // The send failed ("not running"): the bubble went, the text is back, a toast says so.
    assert_eq!(ids(&slice, cx).last(), Some(&101));
    assert_eq!(&*model.chat.drafts.read_with(cx, |d, _| d.get(&DraftKey::Channel(gaming(), "general".into()))), "hello");
    assert!(toasts(&model, cx).iter().any(|(t, _)| t == "Failed to send message"));
}

#[gpui::test]
fn a_refusal_withdraws_the_bubble_and_corrects_slowmode(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![channel("general", 30, SEND)]);
    deliver(&model, cx, [history(1..=10, 0, false)]);
    let slice = general(&model, cx).unwrap();
    // Our own message (from another device) starts the window.
    deliver(&model, cx, [live("general", 11, ME, "mine", "")]);
    let key = (gaming(), crate::ids::ChannelId::from("general"));
    let until = model.chat.slowmode.read_with(cx, |s, _| s.until(&key));
    assert!(until.is_some());
    // A bubble the server refuses: withdrawn, its text back, the wait corrected.
    slice.update(cx, |s, cx| {
        s.push_bubble(std::sync::Arc::new(crate::chat::message::Message::optimistic("perf2".into(), ME, "again".into(), 0, "n9".into(), 0, 11)), cx)
    });
    deliver(
        &model,
        cx,
        [CoreEvent::ChannelMessageRejected(ChannelMessageRejectedPayload {
            server_id: "1".into(),
            channel_id: "general".into(),
            nonce: "n9".into(),
            reason: "Slow down.".into(),
            retry_after_seconds: 12,
        })],
    );
    assert_eq!(ids(&slice, cx).last(), Some(&11));
    assert_eq!(&*model.chat.drafts.read_with(cx, |d, _| d.get(&DraftKey::Channel(gaming(), "general".into()))), "again");
    assert!(toasts(&model, cx).contains(&("Message not sent".into(), Some("Slow down. Your text is back in the message box.".into()))));
    let now = cx.executor().now();
    assert_eq!(model.chat.slowmode.read_with(cx, |s, _| s.remaining(&key, 30, now)), 12);
    // The legacy echo of the same refusal stays silent for a beat.
    deliver(
        &model,
        cx,
        [CoreEvent::ModActionResponded(ModActionRespondedPayload {
            server_id: "1".into(),
            success: false,
            message: "legacy".into(),
            action: "message".into(),
            username: String::new(),
            uid: 0,
        })],
    );
    assert!(!toasts(&model, cx).iter().any(|(t, _)| t == "legacy"));
    // A send now is held (text kept), and the channel's timer shakes.
    let held = cx.update(|cx| crate::actions::chat::send_message(gaming(), "general".into(), "third", 0, cx));
    assert_eq!(held, crate::actions::chat::SendOutcome::HeldBySlowmode);
}

#[gpui::test]
fn a_refusal_withdraws_exactly_its_bubble(cx: &mut TestAppContext) {
    // Two sends pending in one channel (CLAUDE.md: "exactly that optimistic bubble is withdrawn").
    let model = signed_in(cx, vec![channel("general", 0, SEND)]);
    deliver(&model, cx, [history(1..=10, 0, false)]);
    let slice = general(&model, cx).unwrap();
    for (text, nonce) in [("first", "na"), ("second", "nb")] {
        slice.update(cx, |s, cx| {
            s.push_bubble(std::sync::Arc::new(crate::chat::message::Message::optimistic("perf2".into(), ME, text.into(), 0, nonce.into(), 0, 10)), cx)
        });
    }
    deliver(
        &model,
        cx,
        [CoreEvent::ChannelMessageRejected(ChannelMessageRejectedPayload {
            server_id: "1".into(),
            channel_id: "general".into(),
            nonce: "nb".into(),
            reason: "Too long.".into(),
            retry_after_seconds: 0,
        })],
    );
    let pending: Vec<String> = slice.read_with(cx, |s, _| s.rows().iter().filter(|m| m.pending).map(|m| m.content.to_string()).collect());
    assert_eq!(pending, vec!["first".to_string()], "only the refused bubble goes");
    assert_eq!(&*model.chat.drafts.read_with(cx, |d, _| d.get(&DraftKey::Channel(gaming(), "general".into()))), "second");
    // Its echo settles the other one.
    deliver(&model, cx, [live("general", 11, ME, "first", "na")]);
    assert!(slice.read_with(cx, |s, _| s.rows().iter().all(|m| !m.pending)));
    assert_eq!(ids(&slice, cx).last(), Some(&11));
}

#[gpui::test]
fn a_reconnect_drops_caught_up_slices_and_windows_scrolled_up_ones(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![channel("general", 0, SEND), channel("clips", 0, SEND), channel("old", 0, SEND)]);
    deliver(&model, cx, [history(1..=10, 0, false)]);
    let mut clips = history(1..=10, 0, false);
    let mut old = history(1..=10, 0, false);
    for (e, ch) in [(&mut clips, "clips"), (&mut old, "old")] {
        if let CoreEvent::ChannelHistoryReceived(p) = e {
            p.channel_id = ch.into();
        }
    }
    deliver(&model, cx, [clips, old]);
    let clips = cx.read(|cx| model.chat.channels.read(cx).get(gaming(), "clips").cloned()).unwrap();
    clips.update(cx, |s, _| s.set_scroll(crate::chat::slice::ScrollPosition { at_bottom: false, anchor_id: 3, offset: 0.0 }));
    // general is on screen: left to its panel.
    model.chat.channels.update(cx, |c, _| c.set_displayed(Some((gaming(), "general".into()))));
    let epoch = cx.read(|cx| model.chat.channels.read(cx).resync_epoch(gaming()));
    // Back with "old" deleted.
    deliver(&model, cx, [auth_ok(vec![channel("general", 0, SEND), channel("clips", 0, SEND)])]);
    cx.read(|cx| {
        let channels = model.chat.channels.read(cx);
        assert_eq!(channels.resync_epoch(gaming()), epoch + 1);
        assert!(channels.get(gaming(), "old").is_none(), "gone channel: dropped");
        assert!(channels.get(gaming(), "general").is_some(), "on screen: the panel's");
        assert!(channels.get(gaming(), "clips").unwrap().read(cx).has_more_after, "scrolled up: windowed");
    });
    // The panel re-syncs the displayed channel: caught up → windowed while the newest page loads.
    cx.update(|cx| crate::actions::chat::resync_displayed(gaming(), "general".into(), true, cx));
    let slice = general(&model, cx).unwrap();
    slice.read_with(cx, |s, _| assert!(s.resyncing && s.has_more_after && !s.list_windowed()));
    // ("not running": every attempt goes unanswered; after three the re-sync ends.)
    for _ in 0..3 {
        cx.executor().advance_clock(crate::chat::history::RETRY_DELAY);
        cx.run_until_parked();
    }
    slice.read_with(cx, |s, _| assert!(!s.resyncing));
}

#[gpui::test]
fn an_older_page_without_its_echo_never_reads_as_the_newest(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![channel("general", 0, SEND)]);
    deliver(&model, cx, [history(51..=100, 0, true)]);
    let slice = general(&model, cx).unwrap();
    // An older-page request waits (a sender that says "sent").
    model.chat.history.update(cx, |h, _| h.set_sender(std::rc::Rc::new(|_, _, cx, done| done(true, cx))));
    let key = WaitKey::Channel(gaming(), "general".into());
    let _wait = model.chat.history.update(cx, |h, cx| h.request(key.clone(), HistoryRequest::before(51, 50), cx));
    assert_eq!(model.chat.history.read_with(cx, |h, _| h.waiting_before_cursor(&key)), Some(51));
    // An older server answers with no cursor echoed: merged as an older page.
    deliver(&model, cx, [history(1..=50, 0, false)]);
    assert_eq!(ids(&slice, cx), (1..=100).collect::<Vec<_>>(), "nothing above the page was dropped");
}

#[gpui::test]
fn deletes_edits_and_their_answers(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![channel("general", 0, SEND)]);
    deliver(&model, cx, [history(1..=5, 0, false)]);
    let slice = general(&model, cx).unwrap();
    cx.update(|cx| crate::actions::chat::delete_message(gaming(), "general".into(), 3, cx));
    cx.run_until_parked();
    // The call failed ("not running"): the row came back with a toast.
    assert_eq!(ids(&slice, cx), vec![1, 2, 3, 4, 5]);
    assert!(toasts(&model, cx).iter().any(|(t, _)| t == "Failed to delete message"));
    // A refused delete: the row comes back; a broadcast removes it for good.
    slice.update(cx, |s, cx| s.snapshot_and_remove(4, None, cx));
    deliver(
        &model,
        cx,
        [CoreEvent::ChannelMessageDeleteResponded(ChannelMessageDeleteRespondedPayload {
            success: false,
            message: String::new(),
            server_id: "1".into(),
            channel_id: "general".into(),
            message_id: 4,
        })],
    );
    assert_eq!(ids(&slice, cx), vec![1, 2, 3, 4, 5]);
    deliver(
        &model,
        cx,
        [
            CoreEvent::ChannelMessageDeleted(ChannelMessageDeletedPayload {
                server_id: "1".into(),
                channel_id: "general".into(),
                message_id: 2,
                deleted_at: 1,
                deleted_by: "x".into(),
                deleted_by_uid: 2,
            }),
            CoreEvent::ChannelMessageEdited(ChannelMessageEditedPayload {
                server_id: "1".into(),
                channel_id: "general".into(),
                message_id: 5,
                content: "five!".into(),
                edited_at: 99,
                editor: "u2".into(),
                editor_uid: 2,
                encrypted: false,
                decrypt_error: String::new(),
                encrypted_attachments: vec![],
                suppress_embeds: false,
            }),
        ],
    );
    slice.read_with(cx, |s, _| {
        assert_eq!(s.rows().iter().map(|m| m.id).collect::<Vec<_>>(), vec![1, 3, 4, 5]);
        assert_eq!(&*s.rows()[3].content, "five!");
        assert!(s.facts()[3].edited);
    });
    // The 5 s delete watchdog of a delete nobody answers.
    slice.update(cx, |s, cx| s.snapshot_and_remove(1, None, cx));
    cx.executor().advance_clock(Duration::from_secs(6));
    cx.run_until_parked();
}

/// `DmChatPanel`'s `sending`: set while a text send's call runs (the button is disabled), not
/// for a GIF; a failed call (here the core isn't running) ends it with the inline error.
#[gpui::test]
fn a_dm_text_send_holds_the_send_button_until_its_call_returns(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![]);
    deliver(&model, cx, [dm(1, 3, ME, "hi", "")]);
    cx.update(|cx| {
        crate::actions::dm::send_dm(3, "hello", 0, cx);
    });
    cx.read(|cx| assert!(model.dm.list.read(cx).sending(3)));
    cx.run_until_parked();
    cx.read(|cx| {
        let list = model.dm.list.read(cx);
        assert!(!list.sending(3));
        assert!(list.send_error.as_ref().is_some_and(|(peer, _)| *peer == 3), "the failure shows inline");
    });
    cx.update(|cx| {
        crate::actions::dm::send_gif(3, "https://example.test/a.gif", 0, cx);
    });
    cx.read(|cx| assert!(!model.dm.list.read(cx).sending(3), "a GIF doesn't hold the button"));
    cx.run_until_parked();
}

#[gpui::test]
fn dms_route_by_the_other_side_and_read_sync(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![]);
    deliver(&model, cx, [dm(1, 3, ME, "hi", ""), dm(2, 3, ME, "there", ""), dm(3, ME, 3, "yo", "")]);
    cx.read(|cx| {
        let list = model.dm.list.read(cx);
        let c = list.get(3).unwrap();
        assert_eq!((c.unread, c.last_message.as_ref().unwrap().id), (2, 3), "our own (to 3) files under 3");
        assert_eq!(list.slice(3).unwrap().read(cx).rows().len(), 3);
    });
    // Another device of ours read it.
    deliver(&model, cx, [CoreEvent::DmReadSynced(DmReadSyncedPayload { peer_uid: 3, up_to_id: 3 })]);
    cx.read(|cx| assert_eq!(model.dm.list.read(cx).get(3).unwrap().unread, 0));
    // Open on screen but not being read (the window unfocused, or the list scrolled up: the
    // view says): unread, the native client's rule (owner, 2026-11-26).
    model.dm.list.update(cx, |l, cx| l.set_active(Some(3), cx));
    model.nav.update(cx, |n, cx| n.set_active_view(ActiveView::Dm, cx));
    deliver(&model, cx, [dm(4, 3, ME, "unseen", "")]);
    cx.read(|cx| assert_eq!(model.dm.list.read(cx).get(3).unwrap().unread, 1));
    model.dm.list.update(cx, |l, cx| l.mark_read(3, 4, cx));
    // Being read: new ones aren't unread.
    model.dm.list.update(cx, |l, _| l.set_reading(3, true));
    deliver(&model, cx, [dm(5, 3, ME, "seen", "")]);
    cx.read(|cx| assert_eq!(model.dm.list.read(cx).get(3).unwrap().unread, 0));
    // Another conversation stops nothing; this one's view stopping does.
    model.dm.list.update(cx, |l, _| l.set_reading(7, false));
    cx.read(|cx| assert_eq!(model.dm.list.read(cx).reading(), Some(3)));
    model.dm.list.update(cx, |l, _| l.set_reading(3, false));
    deliver(&model, cx, [dm(6, 3, ME, "later", "")]);
    cx.read(|cx| assert_eq!(model.dm.list.read(cx).get(3).unwrap().unread, 1));
    // Central's error reply to a send of ours withdraws its bubble with the reason.
    let bubble = std::sync::Arc::new(crate::chat::message::Message::optimistic("perf2".into(), ME, "nope".into(), 0, "nz".into(), 0, 6));
    model.dm.list.update(cx, |l, cx| l.add_message(3, bubble, true, "u3".into(), true, cx));
    deliver(&model, cx, [dm(0, ME, 3, "They only accept DMs from friends.", "nz")]);
    cx.read(|cx| assert!(model.dm.list.read(cx).slice(3).unwrap().read(cx).bubble("nz").is_none()));
    assert!(toasts(&model, cx).contains(&("Message not sent".into(), Some("They only accept DMs from friends.".into()))));
}

#[gpui::test]
fn sign_out_leaves_nothing(cx: &mut TestAppContext) {
    let model = signed_in(cx, vec![channel("general", 0, SEND)]);
    deliver(&model, cx, [history(1..=5, 0, false), dm(1, 3, ME, "hi", "")]);
    model.chat.drafts.update(cx, |d, cx| d.set(DraftKey::Dm(3), "draft".into(), cx));
    deliver(&model, cx, [CoreEvent::LoggedOut(LoggedOutPayload { reason: None })]);
    cx.read(|cx| {
        assert!(model.chat.channels.read(cx).is_empty());
        assert!(model.dm.list.read(cx).is_empty());
        assert!(model.chat.drafts.read(cx).is_empty());
    });
}
