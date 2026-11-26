//! Merging a fetched page of stored messages into a loaded slice (`lib/historyMerge.ts`,
//! rule for rule). Channels and DMs share it; pure, no store access.
//!
//! A slice is its stored rows by id ascending, then the rows with no id yet (optimistic
//! sends, legacy rows) at the tail. A page is a contiguous run of stored rows, in any
//! order: everything the server holds between its oldest and newest row (the byte budget
//! only ever cuts the far end). Both facts let a page repair a slice that missed traffic
//! (review B4): a row the slice has inside the page's range but the page lacks was
//! deleted, a row the page has changed was edited, and a page entirely above the slice's
//! newest row may sit past a gap. Live rows that arrive while a slice re-syncs wait for its
//! newest page ([`apply_held_rows`]).
//!
//! Lists are `Arc<Message>`; "unchanged" results are `None`, so a caller keeps its own
//! vector (and every row's identity) when nothing moved.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::SharedString;

use crate::chat::message::Message;
use crate::ids::Uid;

type Row = Arc<Message>;

/// A row's server id, 0 for one that has none yet.
fn real_id(m: &Message) -> i64 {
    m.id.max(0)
}

/// The newest stored id in a slice (0 when it holds none).
pub fn newest_stored_id(slice: &[Row]) -> i64 {
    slice.iter().rev().map(|m| real_id(m)).find(|id| *id > 0).unwrap_or(0)
}

/// The optimistic sends (by nonce) a page of stored rows confirms (`settledNonces`). A row
/// carrying the bubble's nonce settles it. Rows without one (DM history, rows a restarted
/// community forgot) settle a text bubble whose content and reply target they repeat, if
/// they are ours, newer than the bubble's `sent_after_id` and not already in the slice;
/// bubbles take such rows oldest first, so two identical sends can't both claim one row.
/// An attachment-only bubble has nothing to compare and waits for its nonce.
pub fn settled_nonces(slice: &[Row], page: &[Row], own_uid: Uid) -> HashSet<SharedString> {
    let mut out = HashSet::new();
    let pending: Vec<&Row> = slice.iter().filter(|m| real_id(m) == 0 && m.nonce.is_some()).collect();
    if pending.is_empty() {
        return out;
    }
    let mut waiting: HashSet<SharedString> = pending.iter().filter_map(|m| m.nonce.clone()).collect();
    let known: HashSet<i64> = slice.iter().map(|m| real_id(m)).filter(|id| *id > 0).collect();
    let mut unclaimed: Vec<&Row> = Vec::new();
    for r in page {
        let id = real_id(r);
        if id == 0 {
            continue;
        }
        if let Some(nonce) = &r.nonce {
            // Another session's send carries its own nonce: never ours to claim.
            if waiting.remove(nonce) {
                out.insert(nonce.clone());
            }
            continue;
        }
        if own_uid > 0 && r.sender_uid == own_uid && !known.contains(&id) {
            unclaimed.push(r);
        }
    }
    if waiting.is_empty() || unclaimed.is_empty() {
        return out;
    }
    unclaimed.sort_by_key(|r| real_id(r));
    let mut claimed: HashSet<i64> = HashSet::new();
    for m in pending {
        let Some(nonce) = &m.nonce else { continue };
        if !waiting.contains(nonce) || m.content.trim().is_empty() {
            continue;
        }
        let row = unclaimed.iter().find(|r| {
            !claimed.contains(&real_id(r))
                && real_id(r) > m.sent_after_id
                && r.content == m.content
                && r.reply_to() == m.reply_to()
        });
        if let Some(row) = row {
            claimed.insert(real_id(row));
            out.insert(nonce.clone());
        }
    }
    out
}

/// What [`merge_stored_page`] did.
#[derive(Debug, Default)]
pub struct MergedPage {
    /// The new slice; `None` when it is the slice as it was.
    pub list: Option<Vec<Row>>,
    /// The page's rows went in. False for a page above the slice's newest row in `after`
    /// mode (or into a slice with no stored rows): it doesn't connect, so only the bubbles
    /// it confirms were settled.
    pub applied: bool,
    /// `before` mode: the page lay entirely above the slice's newest row and replaced the
    /// stored rows (they may sit past a gap).
    pub replaced: bool,
    /// The page reached below the slice's oldest row: its "older messages exist" flag now
    /// describes the slice's head.
    pub head_extended: bool,
    /// `before` mode: the page reached the slice's newest row or past it, i.e. it was the
    /// channel's newest page and the slice is now at the present.
    pub reached_newest: bool,
}

/// `unchanged`: the row a page brings is the one the slice holds (its object is kept, so a
/// view keyed by identity doesn't redo it). Only these fields count, as in the renderer.
fn unchanged(a: &Message, b: &Message) -> bool {
    a.content == b.content && a.edited_at == b.edited_at && a.encrypted == b.encrypted && a.decrypt_error == b.decrypt_error
}

/// Merge a page into a slice (`mergeStoredPage`). `after_id` > 0: the page holds the rows
/// above it (downward paging, a re-sync); 0: a newest or older page (rows below a
/// `before_id`). `latest`: the server echoed no cursor at all, so the page is the channel's
/// newest and answers for everything from its oldest row up (a row the slice holds above
/// it was deleted, and an empty one means an empty channel). `skip` ids are never added
/// (deletes awaiting the server's answer). Pending bubbles the page confirms are dropped;
/// the rest stay at the tail.
pub fn merge_stored_page(
    slice: &[Row],
    page: &[Row],
    own_uid: Uid,
    after_id: i64,
    skip: Option<&HashSet<i64>>,
    latest: bool,
) -> MergedPage {
    let settled = settled_nonces(slice, page, own_uid);
    let mut stored: Vec<Row> = Vec::with_capacity(slice.len());
    let mut tail: Vec<Row> = Vec::new();
    for m in slice {
        if real_id(m) > 0 {
            stored.push(m.clone());
        } else if !m.nonce.as_ref().is_some_and(|n| settled.contains(n)) {
            tail.push(m.clone());
        }
    }
    // By id, the page's last word on a repeated id (a JS Map's `set`).
    let mut by_id: HashMap<i64, Row> = HashMap::with_capacity(page.len());
    for m in page {
        let id = real_id(m);
        if id > 0 && !skip.is_some_and(|s| s.contains(&id)) {
            by_id.insert(id, m.clone());
        }
    }
    let mut rows: Vec<Row> = by_id.into_values().collect();
    rows.sort_by_key(|m| real_id(m));
    let stored_oldest = stored.first().map_or(0, |m| real_id(m));
    let stored_newest = stored.last().map_or(0, |m| real_id(m));
    let keep_slice = |stored: Vec<Row>, tail: Vec<Row>, applied, head_extended, reached_newest| MergedPage {
        list: (!settled.is_empty()).then(|| [stored, tail].concat()),
        applied,
        replaced: false,
        head_extended,
        reached_newest,
    };

    if after_id > 0 {
        // Above the newest row there may be a gap; into nothing, a reply to a request from
        // before the slice was reset.
        if stored.is_empty() || after_id > stored_newest {
            return keep_slice(stored, tail, false, false, false);
        }
        // Rows below the slice's oldest would hang under it past a gap when the page
        // doesn't reach up to it; they stay reachable by scrolling up.
        rows.retain(|r| real_id(r) >= stored_oldest);
        let Some(page_newest) = rows.last().map(|r| real_id(r)) else {
            return keep_slice(stored, tail, true, false, false);
        };
        let lo = (after_id + 1).max(stored_oldest);
        return MergedPage {
            list: Some(refresh(&stored, rows, lo, page_newest, tail)),
            applied: true,
            replaced: false,
            head_extended: false,
            reached_newest: false,
        };
    }

    if rows.is_empty() {
        // The newest page of an empty channel: everything the slice held is gone.
        if latest {
            return MergedPage {
                replaced: !stored.is_empty(),
                list: Some(tail),
                applied: true,
                head_extended: true,
                reached_newest: true,
            };
        }
        // Nothing below the request's boundary.
        let empty = stored.is_empty();
        return keep_slice(stored, tail, true, true, empty);
    }
    let page_oldest = real_id(&rows[0]);
    let page_newest = real_id(&rows[rows.len() - 1]);
    if stored.is_empty() || page_oldest > stored_newest {
        return MergedPage {
            replaced: !stored.is_empty(),
            list: Some([rows, tail].concat()),
            applied: true,
            head_extended: true,
            reached_newest: true,
        };
    }
    MergedPage {
        list: Some(refresh(&stored, rows, page_oldest, if latest { i64::MAX } else { page_newest }, tail)),
        applied: true,
        replaced: false,
        head_extended: page_oldest < stored_oldest,
        reached_newest: latest || page_newest >= stored_newest,
    }
}

/// Live rows held while a slice re-synced (one landing before the newest page could have
/// sat past a gap), put in once that page has merged and the slice is at the present
/// (`applyHeldRows`). The page answers for every row up to its newest: it holds them, or
/// they were deleted since. Held rows above it go in at the top, in id order. A held row
/// carrying a bubble's nonce settles that bubble whichever way it went: the server stored
/// it. `None`: nothing to change.
pub fn apply_held_rows(slice: &[Row], held: &[Row]) -> Option<Vec<Row>> {
    if held.is_empty() {
        return None;
    }
    let newest = newest_stored_id(slice);
    let mut nonces: HashSet<&SharedString> = HashSet::new();
    let mut above: HashMap<i64, &Row> = HashMap::new();
    for r in held {
        let id = real_id(r);
        if id == 0 {
            continue;
        }
        if let Some(n) = &r.nonce {
            nonces.insert(n);
        }
        if id > newest {
            above.insert(id, r);
        }
    }
    let mut stored: Vec<Row> = Vec::with_capacity(slice.len() + above.len());
    let mut tail: Vec<Row> = Vec::new();
    for m in slice {
        if real_id(m) > 0 {
            stored.push(m.clone());
        } else if !m.nonce.as_ref().is_some_and(|n| nonces.contains(n)) {
            tail.push(m.clone());
        }
    }
    if above.is_empty() && stored.len() + tail.len() == slice.len() {
        return None;
    }
    let mut rows: Vec<Row> = above.into_values().cloned().collect();
    rows.sort_by_key(|m| real_id(m));
    stored.extend(rows);
    stored.extend(tail);
    Some(stored)
}

/// Stored rows outside `[lo, hi]` as they are; inside it, the page's rows, keeping the
/// slice's object for a row that didn't change; then the tail.
fn refresh(stored: &[Row], rows: Vec<Row>, lo: i64, hi: i64, tail: Vec<Row>) -> Vec<Row> {
    let old: HashMap<i64, &Row> = stored.iter().map(|m| (real_id(m), m)).collect();
    let mut out: Vec<Row> = stored.iter().filter(|m| real_id(m) < lo || real_id(m) > hi).cloned().collect();
    for r in rows {
        match old.get(&real_id(&r)) {
            Some(prev) if unchanged(prev, &r) => out.push((*prev).clone()),
            _ => out.push(r),
        }
    }
    out.sort_by_key(|m| real_id(m));
    out.extend(tail);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chat::message::testing::{bubble, stored, with_nonce};

    const ME: Uid = 7;

    fn ids(list: &[Row]) -> Vec<i64> {
        list.iter().map(|m| m.id).collect()
    }

    fn rows(range: std::ops::RangeInclusive<i64>) -> Vec<Row> {
        range.map(|id| stored(id, 1, &format!("m{id}"), 1000 + id)).collect()
    }

    #[test]
    fn an_older_page_extends_the_head_and_dedupes() {
        let slice = rows(10..=20);
        let page = rows(5..=12);
        let r = merge_stored_page(&slice, &page, ME, 0, None, false);
        let list = r.list.unwrap();
        assert_eq!(ids(&list), (5..=20).collect::<Vec<_>>());
        assert!(r.applied && r.head_extended && !r.replaced && !r.reached_newest);
        // Unchanged rows keep their objects.
        assert!(Arc::ptr_eq(&list[5], &slice[0]));
    }

    #[test]
    fn a_page_inside_the_range_deletes_and_edits() {
        let slice = rows(1..=10);
        // 4 and 6 gone; 5 edited.
        let mut page: Vec<Row> = rows(3..=8).into_iter().filter(|m| m.id != 4 && m.id != 6).collect();
        let mut five = (*page[1]).clone();
        five.content = "edited".into();
        five.edited_at = 99;
        page[1] = Arc::new(five);
        let r = merge_stored_page(&slice, &page, ME, 0, None, false);
        let list = r.list.unwrap();
        assert_eq!(ids(&list), vec![1, 2, 3, 5, 7, 8, 9, 10]);
        assert_eq!(&*list[3].content, "edited");
        assert!(!r.head_extended && !r.reached_newest);
    }

    #[test]
    fn the_newest_page_answers_for_everything_above_its_oldest_row() {
        let slice = rows(1..=10);
        // latest: 9 and 10 were deleted since.
        let r = merge_stored_page(&slice, &rows(5..=8), ME, 0, None, true);
        assert_eq!(ids(&r.list.unwrap()), (1..=8).collect::<Vec<_>>());
        assert!(r.reached_newest);
        // An empty newest page: an empty channel, bubbles stay.
        let mut with_bubble = rows(1..=3);
        with_bubble.push(bubble("n", ME, "hi", 0, 3));
        let r = merge_stored_page(&with_bubble, &[], ME, 0, None, true);
        assert_eq!(ids(&r.list.unwrap()), vec![0]);
        assert!(r.replaced && r.reached_newest && r.head_extended);
        // An empty older page: nothing below, the slice stays.
        let r = merge_stored_page(&slice, &[], ME, 0, None, false);
        assert!(r.list.is_none() && r.head_extended && !r.reached_newest);
    }

    #[test]
    fn a_page_above_the_slice_replaces_it_and_skip_ids_stay_out() {
        let slice = rows(1..=5);
        let skip: HashSet<i64> = [21].into();
        let r = merge_stored_page(&slice, &rows(20..=25), ME, 0, Some(&skip), false);
        assert_eq!(ids(&r.list.unwrap()), vec![20, 22, 23, 24, 25]);
        assert!(r.replaced && r.reached_newest);
    }

    #[test]
    fn after_pages_connect_or_only_settle() {
        let mut slice = rows(10..=20);
        slice.push(bubble("n1", ME, "hello", 0, 20));
        // A gap above the newest: not applied, the bubble it confirms settles.
        let echo = with_nonce(&stored(40, ME, "hello", 0), "n1");
        let r = merge_stored_page(&slice, &[echo], ME, 30, None, false);
        assert!(!r.applied);
        assert_eq!(ids(&r.list.unwrap()), (10..=20).collect::<Vec<_>>());
        // Connecting: the rows above 15.
        let r = merge_stored_page(&slice, &rows(16..=24), ME, 15, None, false);
        assert!(r.applied);
        assert_eq!(ids(&r.list.unwrap()), [(10..=24).collect::<Vec<_>>(), vec![0]].concat());
        // Nothing new: applied, the slice as it was.
        let r = merge_stored_page(&slice, &[], ME, 20, None, false);
        assert!(r.applied && r.list.is_none());
    }

    #[test]
    fn bubbles_settle_by_nonce_or_by_their_text() {
        let slice = vec![
            stored(1, 2, "x", 0),
            bubble("a", ME, "same", 0, 1),
            bubble("b", ME, "same", 0, 1),
            bubble("c", ME, "   ", 0, 1),
        ];
        // One row without a nonce: claims the first identical bubble only.
        let page = vec![stored(2, ME, "same", 0)];
        let s = settled_nonces(&slice, &page, ME);
        assert_eq!(s, ["a".into()].into());
        // Two rows: both identical bubbles, oldest first; the blank one never.
        let page = vec![stored(3, ME, "same", 0), stored(2, ME, "same", 0), stored(4, ME, "   ", 0)];
        assert_eq!(settled_nonces(&slice, &page, ME), ["a".into(), "b".into()].into());
        // Someone else's row, or one at / below sent_after_id, settles nothing.
        assert!(settled_nonces(&slice, &[stored(5, 3, "same", 0)], ME).is_empty());
        assert!(settled_nonces(&slice, &[stored(1, ME, "same", 0)], ME).is_empty());
        // A nonce row from another session never claims by text.
        let other = with_nonce(&stored(6, ME, "same", 0), "zzz");
        assert!(settled_nonces(&slice, &[other], ME).is_empty());
        // Signed out (uid 0): only nonces.
        assert!(settled_nonces(&slice, &[stored(2, ME, "same", 0)], 0).is_empty());
    }

    #[test]
    fn held_rows_go_in_above_the_page() {
        let mut slice = rows(1..=5);
        slice.push(bubble("n", ME, "hi", 0, 5));
        let held = vec![stored(7, 1, "b", 0), with_nonce(&stored(6, ME, "hi", 0), "n"), stored(3, 1, "old", 0)];
        let list = apply_held_rows(&slice, &held).unwrap();
        assert_eq!(ids(&list), vec![1, 2, 3, 4, 5, 6, 7]);
        assert!(apply_held_rows(&rows(1..=5), &rows(2..=4)).is_none());
        assert!(apply_held_rows(&slice, &[]).is_none());
    }
}
