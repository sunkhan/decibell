//! The conversation list: unread arithmetic, the echo of our sends, previews, pages,
//! read marks and read sync across devices, a re-sync after central signed us in again.

use gpui::TestAppContext;

use super::*;
use crate::chat::message::testing::{bubble, stored, with_nonce};

const ME: Uid = 7;
const PEER: Uid = 3;

fn list(cx: &mut TestAppContext) -> Entity<DmList> {
    let l = cx.new(|_| DmList::default());
    l.update(cx, |l, _| l.set_own_uid(ME));
    l
}

fn rows(l: &DmList, cx: &gpui::App) -> Vec<i64> {
    l.slice(PEER).map(|s| s.read(cx).rows().iter().map(|m| m.id).collect()).unwrap_or_default()
}

fn preview(id: i64, ts: i64, unread: i64) -> PreviewInput {
    PreviewInput {
        peer: PEER,
        username: "carol".into(),
        row: Arc::new(Message::dm_preview(id, "carol".into(), PEER, "last".into(), ts, false, None)),
        last_timestamp: ts,
        unread,
    }
}

#[gpui::test]
fn unread_counts_follow_who_sent_and_who_is_reading(cx: &mut TestAppContext) {
    let l = list(cx);
    l.update(cx, |l, cx| {
        l.add_message(PEER, stored(1, PEER, "hi", 100), false, "carol".into(), false, cx);
        l.add_message(PEER, stored(2, PEER, "there", 101), false, "carol".into(), false, cx);
        assert_eq!(l.get(PEER).unwrap().unread, 2);
        // Ours (from any device): no bump.
        l.add_message(PEER, stored(3, ME, "yo", 102), true, "carol".into(), false, cx);
        // Being read right now: no bump.
        l.add_message(PEER, stored(4, PEER, "!", 103), false, "carol".into(), true, cx);
        assert_eq!(l.get(PEER).unwrap().unread, 2);
        // A replay is never counted twice.
        assert!(matches!(l.add_message(PEER, stored(2, PEER, "there", 101), false, "carol".into(), false, cx), DmAdd::Ignored));
        assert_eq!(l.get(PEER).unwrap().unread, 2);
        assert_eq!(l.get(PEER).unwrap().last_message.as_ref().unwrap().id, 4);
        assert_eq!(l.total_unread(), 2);
        // Read up to 4: cleared; a later message at or below the marker isn't unread.
        assert!(l.mark_read(PEER, 4, cx));
        assert!(!l.mark_read(PEER, 4, cx), "nothing new to mark");
        assert_eq!(l.get(PEER).unwrap().unread, 0);
    });
}

#[gpui::test]
fn our_echo_replaces_its_bubble_and_the_preview(cx: &mut TestAppContext) {
    let l = list(cx);
    l.update(cx, |l, cx| {
        l.add_message(PEER, stored(1, PEER, "hi", 100), false, "carol".into(), false, cx);
        l.add_message(PEER, bubble("n1", ME, "reply", 101, 1), true, "carol".into(), true, cx);
        // A live DM lands after our pending bubble (DMs append where they land).
        l.add_message(PEER, stored(2, PEER, "and", 102), false, "carol".into(), false, cx);
        assert_eq!(rows(l, cx), vec![1, 0, 2]);
        let echo = with_nonce(&stored(3, ME, "reply", 101), "n1");
        assert!(matches!(l.add_message(PEER, echo, true, "carol".into(), false, cx), DmAdd::ReplacedEcho { .. }));
        assert_eq!(rows(l, cx), vec![1, 3, 2], "in place");
        assert_eq!(l.get(PEER).unwrap().unread, 2, "theirs; the echo isn't unread");
        // The next page sorts it out.
        let page = vec![stored(1, PEER, "hi", 100), stored(2, PEER, "and", 102), stored(3, ME, "reply", 101)];
        l.append_history(PEER, &page, false, true, cx);
        assert_eq!(rows(l, cx), vec![1, 2, 3]);
    });
}

#[gpui::test]
fn previews_stand_in_until_the_first_page(cx: &mut TestAppContext) {
    let l = list(cx);
    l.update(cx, |l, cx| {
        l.hydrate(vec![preview(40, 1_700_000_000, 2)], cx);
        let c = l.get(PEER).unwrap();
        assert_eq!((c.unread, c.last_message_time_ms, c.created_at_ms), (2, 1_700_000_000_000, 0));
        assert_eq!(rows(l, cx), vec![40]);
        assert!(l.slice(PEER).unwrap().read(cx).rows()[0].preview);
        // The first page replaces the preview row.
        let page: Vec<_> = (31..=40).map(|id| stored(id, PEER, "x", 1_700_000_000 - (40 - id))).collect();
        l.append_history(PEER, &page, true, true, cx);
        assert_eq!(rows(l, cx), (31..=40).collect::<Vec<_>>());
        assert!(!l.slice(PEER).unwrap().read(cx).rows()[9].preview);
        let s = l.slice(PEER).unwrap().read(cx);
        assert!(s.fetched && s.has_more_before);
        // A second sign-in's previews keep the loaded rows.
        l.hydrate(vec![preview(41, 1_700_000_100, 1)], cx);
        assert_eq!(rows(l, cx), (31..=40).collect::<Vec<_>>());
        assert_eq!(l.get(PEER).unwrap().last_message.as_ref().unwrap().id, 41);
    });
}

#[gpui::test]
fn read_sync_from_another_device(cx: &mut TestAppContext) {
    let l = list(cx);
    l.update(cx, |l, cx| {
        for id in 1..=4 {
            l.add_message(PEER, stored(id, PEER, "x", 100 + id), false, "carol".into(), false, cx);
        }
        assert_eq!(l.get(PEER).unwrap().unread, 4);
        // Read up to 2 elsewhere: the two above it stay unread.
        l.apply_read_sync(PEER, 2, cx);
        assert_eq!((l.get(PEER).unwrap().unread, l.get(PEER).unwrap().last_read_id), (2, 2));
        // A live message at or below the marker another device set isn't unread.
        l.apply_read_sync(PEER, 4, cx);
        assert_eq!(l.get(PEER).unwrap().unread, 0);
        l.add_message(PEER, stored(5, PEER, "x", 106), false, "carol".into(), false, cx);
        assert_eq!(l.get(PEER).unwrap().unread, 1);
        // Ignored for an unknown conversation or a zero cursor.
        l.apply_read_sync(99, 4, cx);
        l.apply_read_sync(PEER, 0, cx);
        assert_eq!(l.get(PEER).unwrap().unread, 1);
    });
}

#[gpui::test]
fn a_new_sign_in_drops_loaded_conversations_but_keeps_unconfirmed_sends(cx: &mut TestAppContext) {
    let l = list(cx);
    l.update(cx, |l, cx| {
        let page: Vec<_> = (1..=5).map(|id| stored(id, PEER, "x", 100 + id)).collect();
        l.append_history(PEER, &page, false, true, cx);
        l.append_history(9, &[stored(10, 9, "y", 200)], false, true, cx);
        l.add_message(9, bubble("n", ME, "pending", 201, 10), true, "z".into(), true, cx);
        l.set_displayed(Some(4));
        l.mark_histories_stale(cx);
        assert_eq!(l.resync_epoch(), 1);
        assert!(rows(l, cx).is_empty(), "reloads on open");
        assert!(!l.slice(PEER).unwrap().read(cx).fetched);
        let nine = l.slice(9).unwrap().read(cx);
        assert_eq!(nine.rows().len(), 2, "a conversation with a send in flight keeps its rows");
        assert!(nine.has_more_after);
    });
}

#[gpui::test]
fn deletes_and_edits_move_the_preview(cx: &mut TestAppContext) {
    let l = list(cx);
    l.update(cx, |l, cx| {
        for id in 1..=3 {
            l.add_message(PEER, stored(id, PEER, "x", 100 + id), false, "carol".into(), false, cx);
        }
        let edit = crate::chat::message::Edit {
            content: "edited".into(),
            edited_at: 500,
            encrypted: false,
            decrypt_error: None,
            suppress_embeds: None,
        };
        l.apply_edit(PEER, 3, &edit, cx);
        assert_eq!(&*l.get(PEER).unwrap().last_message.as_ref().unwrap().content, "edited");
        l.remove_message(PEER, 3, cx);
        assert_eq!(l.get(PEER).unwrap().last_message.as_ref().unwrap().id, 2);
        assert!(l.snapshot_and_remove(PEER, 2, None, cx).is_some());
        assert_eq!(l.get(PEER).unwrap().last_message.as_ref().unwrap().id, 1);
        assert!(l.restore_pending_deletion(PEER, 2, cx));
        assert_eq!(rows(l, cx), vec![1, 2]);
        assert_eq!(l.get(PEER).unwrap().last_message.as_ref().unwrap().id, 2);
        // The sidebar order: most recent activity first.
        l.add_message(5, stored(9, 5, "new", 900), false, "e".into(), false, cx);
        assert_eq!(&*l.sorted(), &[5, PEER]);
    });
}
