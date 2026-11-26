//! The slice: what the renderer's `chatStore` tests pinned, and property-style runs of
//! long random event sequences checking the invariants after every step (ids unique and
//! ordered, bubbles at the tail, facts as a fresh computation gives them, and the change
//! log replaying to exactly the rows a list would see).

use std::collections::HashSet;
use std::sync::Arc;

use gpui::{AppContext as _, Entity, TestAppContext};

use super::*;
use crate::chat::message::testing::{bubble, stored, with_nonce};

const ME: Uid = 7;

fn channel_slice(cx: &mut TestAppContext) -> Entity<MessageSlice> {
    cx.new(|_| MessageSlice::new(SliceKind::Channel { server: ServerId::from_central(1), channel: "general".into() }, ME, false))
}

fn ids(s: &MessageSlice) -> Vec<i64> {
    s.rows().iter().map(|m| m.id).collect()
}

/// What a list holds per row, rebuilt only where the log says.
#[derive(Clone, PartialEq, Debug)]
struct ViewRow {
    key: RowKey,
    ptr: usize,
    facts: RowFacts,
}

struct View {
    seen: u64,
    rows: Vec<Option<ViewRow>>,
    resets: usize,
}

impl View {
    fn new() -> View {
        View { seen: 0, rows: Vec::new(), resets: 0 }
    }

    /// Catch up as a list would, then check every untouched row is still right.
    fn sync(&mut self, s: &MessageSlice) {
        let fresh = |i: usize| ViewRow { key: s.rows()[i].key(), ptr: Arc::as_ptr(&s.rows()[i]) as usize, facts: s.facts()[i] };
        match s.changes_since(self.seen) {
            Changes::Reset => {
                self.resets += 1;
                self.rows = (0..s.rows().len()).map(|i| Some(fresh(i))).collect();
            }
            Changes::Ops(ops) => {
                for op in ops {
                    match *op {
                        RowOp::Splice { at, removed, inserted } => {
                            let at = at as usize;
                            assert!(at + removed as usize <= self.rows.len(), "splice past the end: {op:?} on {}", self.rows.len());
                            self.rows.splice(at..at + removed as usize, (0..inserted).map(|_| None));
                        }
                        RowOp::Update { at } => {
                            assert!((at as usize) < self.rows.len(), "update past the end");
                            self.rows[at as usize] = None;
                        }
                    }
                }
                assert_eq!(self.rows.len(), s.rows().len(), "replayed length");
                for i in 0..self.rows.len() {
                    match &self.rows[i] {
                        // Untouched: must be exactly what the slice holds.
                        Some(r) => assert_eq!(*r, fresh(i), "row {i} changed without an op"),
                        None => self.rows[i] = Some(fresh(i)),
                    }
                }
            }
        }
        self.seen = s.revision();
    }
}

fn check_invariants(s: &MessageSlice, channel: bool) {
    let stored: Vec<i64> = s.rows().iter().map(|m| m.id).filter(|id| *id > 0).collect();
    let mut sorted = stored.clone();
    sorted.dedup();
    assert_eq!(sorted.len(), stored.len(), "ids unique: {:?}", ids(s));
    if channel {
        assert!(stored.windows(2).all(|w| w[0] < w[1]), "ids ascending: {:?}", ids(s));
        // Bubbles after every stored row.
        if let Some(last_stored) = s.rows().iter().rposition(|m| m.id > 0) {
            assert!(s.rows()[..last_stored].iter().all(|m| m.id > 0), "a bubble above a stored row: {:?}", ids(s));
        }
    }
    // Facts are what a fresh computation gives.
    assert_eq!(s.facts().len(), s.rows().len());
    for i in 0..s.rows().len() {
        assert_eq!(s.facts()[i], s.fact_at(i), "stale facts at {i}");
    }
    if !s.resyncing {
        assert!(s.held_count().is_none(), "held rows outside a re-sync");
    }
}

/// xorshift: deterministic, no dependency.
struct Rng(u64);
impl Rng {
    fn next(&mut self) -> u64 {
        self.0 ^= self.0 << 13;
        self.0 ^= self.0 >> 7;
        self.0 ^= self.0 << 17;
        self.0
    }
    fn below(&mut self, n: u64) -> u64 {
        if n == 0 { 0 } else { self.next() % n }
    }
    fn chance(&mut self, percent: u64) -> bool {
        self.below(100) < percent
    }
}

/// The server's channel: ids 1..=n, every 50 s, five senders (us among them).
fn server_row(id: i64) -> Arc<Message> {
    let sender = [1, 2, ME, 3, ME][(id % 5) as usize];
    stored(id, sender, &format!("m{id}"), 1_764_000_000 + id * 50)
}

fn page(lo: i64, hi: i64, deleted: &HashSet<i64>) -> Vec<Arc<Message>> {
    (lo.max(1)..=hi).filter(|id| !deleted.contains(id)).map(server_row).collect()
}

#[gpui::test]
fn random_sequences_keep_the_invariants_and_the_log_replays(cx: &mut TestAppContext) {
    for seed in 1..=40u64 {
        let slice = channel_slice(cx);
        let mut rng = Rng(seed.wrapping_mul(0x9E37_79B9_7F4A_7C15));
        let mut newest: i64 = 300;
        let mut deleted: HashSet<i64> = HashSet::new();
        let mut nonces: Vec<String> = Vec::new();
        let (mut eager, mut lazy) = (View::new(), View::new());
        let displayed = seed % 3 != 0;
        for step in 0..400 {
            let op = rng.below(22);
            slice.update(cx, |s, cx| {
                match op {
                    // Live: mostly the next id, sometimes a replay or an old one.
                    0..=5 => {
                        let id = if rng.chance(85) {
                            newest += 1;
                            newest
                        } else {
                            1 + rng.below(newest as u64) as i64
                        };
                        let mut m = server_row(id);
                        // The echo of one of our sends.
                        if rng.chance(30) && !nonces.is_empty() {
                            let n = nonces.remove(rng.below(nonces.len() as u64) as usize);
                            m = with_nonce(&m, &n);
                        }
                        s.add_message(m, displayed, cx);
                    }
                    6..=7 => {
                        let n = format!("n{seed}-{step}");
                        nonces.push(n.clone());
                        s.push_bubble(bubble(&n, ME, &format!("m{}", newest + 1), 1_764_000_000 + newest * 50, s.newest_stored_id()), cx);
                    }
                    // An older page below the oldest row (or the newest page).
                    8..=9 => {
                        let oldest = s.oldest_stored_id();
                        if oldest == 0 || rng.chance(20) {
                            let lo = newest - 49;
                            s.prepend_history(&page(lo, newest, &deleted), true, true, ME, cx);
                        } else {
                            s.prepend_history(&page(oldest - 50, oldest - 1, &deleted), oldest > 51, false, ME, cx);
                        }
                    }
                    10 => {
                        let after = s.newest_stored_id().saturating_sub(rng.below(30) as i64);
                        if after > 0 {
                            s.append_newer(&page(after + 1, (after + 50).min(newest), &deleted), after + 50 < newest, after, ME, cx);
                        }
                    }
                    11 => {
                        let around = 1 + rng.below(newest as u64) as i64;
                        s.set_window(&page(around - 12, (around + 12).min(newest), &deleted), true, around + 12 < newest, cx);
                    }
                    12 => {
                        let id = 1 + rng.below(newest as u64) as i64;
                        deleted.insert(id);
                        s.remove_message(id, cx);
                    }
                    13 => {
                        if let Some(m) = s.rows().get(rng.below(s.rows().len() as u64 + 1) as usize).cloned() {
                            let edit = crate::chat::message::Edit {
                                content: format!("edited {step}").into(),
                                edited_at: step,
                                encrypted: false,
                                decrypt_error: None,
                                suppress_embeds: Some(false),
                            };
                            s.apply_edit(m.id, &edit, cx);
                        }
                    }
                    14 => {
                        if !nonces.is_empty() {
                            let n = nonces.remove(rng.below(nonces.len() as u64) as usize);
                            s.remove_by_nonce(&n, cx);
                        }
                    }
                    15 => {
                        if let Some(m) = s.rows().iter().find(|m| m.id > 0 && rng.chance(10)).cloned() {
                            s.snapshot_and_remove(m.id, None, cx);
                        }
                    }
                    16 => {
                        let pending: Vec<i64> = s.pending_deletion_ids().map(|p| p.into_iter().collect()).unwrap_or_default();
                        if let Some(id) = pending.first() {
                            if rng.chance(50) {
                                s.restore_pending_deletion(*id, cx);
                            } else {
                                deleted.insert(*id);
                                s.clear_pending_deletion(*id);
                            }
                        }
                    }
                    17 => {
                        let len = s.rows().len();
                        if rng.chance(50) {
                            s.trim_tail(1 + rng.below(len as u64 + 1) as usize, cx);
                        } else {
                            s.trim_head(1 + rng.below(len as u64 + 1) as usize, cx);
                        }
                    }
                    18 => {
                        let gone: Vec<i64> = s.rows().iter().filter(|m| m.id > 0 && rng.chance(15)).map(|m| m.id).collect();
                        deleted.extend(&gone);
                        s.apply_pruned(&gone, cx);
                    }
                    19 => {
                        if rng.chance(30) {
                            s.reset_for_jump(cx);
                        } else {
                            s.set_scroll(ScrollPosition { at_bottom: rng.chance(50), anchor_id: s.oldest_stored_id(), offset: 3.0 });
                        }
                    }
                    20 => {
                        let on = !s.resyncing;
                        s.set_resyncing(on, cx);
                    }
                    _ => {
                        if rng.chance(5) {
                            s.apply_wiped(cx);
                        } else {
                            s.mark_windowed(cx);
                        }
                    }
                }
                check_invariants(s, true);
                eager.sync(s);
                if step % 7 == 0 {
                    lazy.sync(s);
                }
                if !displayed && op <= 5 {
                    assert!(s.rows().len() <= BACKGROUND_SLICE_CAP || s.rows().iter().any(|m| m.id == 0) || s.has_more_after, "background cap");
                }
            });
        }
        slice.read_with(cx, |s, _| lazy.sync(s));
        assert!(eager.resets <= 40, "seed {seed}: {} resets for 400 steps", eager.resets);
    }
}

#[gpui::test]
fn a_live_message_is_one_splice_and_an_echo_takes_its_bubbles_place(cx: &mut TestAppContext) {
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        s.prepend_history(&page(1, 10, &HashSet::new()), true, true, ME, cx);
        let r = s.revision();
        s.add_message(server_row(11), true, cx);
        assert_eq!(s.changes_since(r), Changes::Ops(&[RowOp::Splice { at: 10, removed: 0, inserted: 1 }]));
        // Our bubble, then its echo: one splice of 1 for 1 in the same place.
        s.push_bubble(bubble("n1", ME, "m12", 0, 11), cx);
        let r = s.revision();
        s.add_message(with_nonce(&server_row(12), "n1"), true, cx);
        assert_eq!(s.changes_since(r), Changes::Ops(&[RowOp::Splice { at: 11, removed: 1, inserted: 1 }]));
        assert_eq!(ids(s), (1..=12).collect::<Vec<_>>());
        // Someone else's message lands above our pending bubble.
        s.push_bubble(bubble("n2", ME, "x", 0, 12), cx);
        s.add_message(server_row(13), true, cx);
        assert_eq!(ids(s), [(1..=13).collect::<Vec<_>>(), vec![0]].concat());
        // A replay changes nothing.
        let r = s.revision();
        s.add_message(server_row(5), true, cx);
        assert!(matches!(s.changes_since(r), Changes::Ops(ops) if ops.iter().all(|o| matches!(o, RowOp::Update { .. }))));
        assert_eq!(ids(s), [(1..=13).collect::<Vec<_>>(), vec![0]].concat());
    });
}

#[gpui::test]
fn grouping_and_day_dividers_follow_the_previous_row(cx: &mut TestAppContext) {
    let slice = channel_slice(cx);
    let at = |id: i64, sender: Uid, ts: i64| stored(id, sender, "x", ts);
    slice.update(cx, |s, cx| {
        let t = 1_764_000_000;
        s.set_window(&[at(1, 2, t), at(2, 2, t + 60), at(3, 2, t + 60 + 7 * 60), at(4, 3, t + 500), at(5, 3, t + 86_400 * 2)], false, false, cx);
        let f = s.facts();
        assert!(!f[0].grouped && f[1].grouped, "same sender within 7 minutes");
        assert!(!f[2].grouped, "7 minutes apart: a new header");
        assert!(!f[3].grouped, "another sender");
        assert!(f[4].new_day && !f[4].grouped, "another day opens a divider");
        // Deleting a row re-groups the next one: an update for it.
        let r = s.revision();
        s.remove_message(2, cx);
        let Changes::Ops(ops) = s.changes_since(r) else { panic!() };
        assert_eq!(ops, &[RowOp::Splice { at: 1, removed: 1, inserted: 0 }]);
        assert!(!s.facts()[1].grouped, "8 minutes after 1");
        // Own / edit / delete facts.
        s.add_message(stored(6, ME, "mine", t + 86_400 * 2 + 10), true, cx);
        let mine = s.facts()[4];
        assert!(mine.own && mine.can_edit && mine.can_delete_own && mine.can_delete(false));
        let theirs = s.facts()[0];
        assert!(!theirs.can_delete(false) && theirs.can_delete(true) && !theirs.can_edit);
    });
}

#[gpui::test]
fn background_slices_are_capped_or_go_windowed(cx: &mut TestAppContext) {
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        // Never loaded, nothing in flight: a live message isn't cached.
        assert_eq!(s.add_message(server_row(1), false, cx), AddOutcome::Dropped);
        assert!(s.rows().is_empty());
        s.prepend_history(&page(1, 150, &HashSet::new()), true, true, ME, cx);
        assert_eq!(s.add_message(server_row(151), false, cx), AddOutcome::Added);
        assert_eq!(s.rows().len(), BACKGROUND_SLICE_CAP);
        assert_eq!(s.oldest_stored_id(), 2);
        assert!(s.has_more_before);
        // Left scrolled up: the slice keeps its place and goes windowed.
        s.set_scroll(ScrollPosition { at_bottom: false, anchor_id: 50, offset: 0.0 });
        assert_eq!(s.add_message(server_row(152), false, cx), AddOutcome::WentWindowed);
        assert!(s.has_more_after);
        assert_eq!(s.newest_stored_id(), 151);
        // Windowed: later live messages are dropped (past the gap).
        assert_eq!(s.add_message(server_row(153), true, cx), AddOutcome::Dropped);
    });
    // A send in flight keeps the slice at the present (its echo must land).
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        s.prepend_history(&page(1, 149, &HashSet::new()), true, true, ME, cx);
        s.push_bubble(bubble("n", ME, "hi", 0, 149), cx);
        s.set_scroll(ScrollPosition { at_bottom: false, anchor_id: 1, offset: 0.0 });
        assert_eq!(s.add_message(server_row(150), false, cx), AddOutcome::Added);
        assert_eq!(s.rows().len(), BACKGROUND_SLICE_CAP);
        assert_eq!(s.rows().last().unwrap().id, 0, "the bubble rides along");
        assert!(!s.has_more_after);
    });
}

#[gpui::test]
fn a_resync_holds_live_rows_and_the_newest_page_releases_them(cx: &mut TestAppContext) {
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        s.prepend_history(&page(1, 20, &HashSet::new()), true, true, ME, cx);
        s.push_bubble(bubble("n", ME, "m30", 0, 20), cx);
        s.set_resyncing(true, cx);
        assert!(s.has_more_after && !s.list_windowed(), "windowed in the store, caught up on screen");
        // Our echo arrives first: held, and the bubble shows as sent at once.
        assert_eq!(s.add_message(with_nonce(&server_row(30), "n"), true, cx), AddOutcome::Held);
        assert!(!s.bubble("n").unwrap().pending);
        assert_eq!(s.add_message(server_row(31), true, cx), AddOutcome::Held);
        // The newest page (up to 25: the outage swallowed 21..=29, 30 and 31 came live).
        s.prepend_history(&page(6, 25, &HashSet::new()), true, true, ME, cx);
        assert_eq!(ids(s), [(1..=25).collect::<Vec<_>>(), vec![30, 31]].concat(), "held rows above the page, the bubble settled");
        assert!(!s.has_more_after);
        s.set_resyncing(false, cx);
        assert_eq!(s.held_count(), None);
    });
}

#[gpui::test]
fn an_older_page_never_answers_for_rows_above_it(cx: &mut TestAppContext) {
    // The `before_id == 0` incident: an older page read as the newest wiped the rows above.
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        s.prepend_history(&page(51, 100, &HashSet::new()), true, true, ME, cx);
        s.prepend_history(&page(1, 50, &HashSet::new()), false, false, ME, cx);
        assert_eq!(ids(s), (1..=100).collect::<Vec<_>>());
        // The same page taken for the newest would have dropped 51..=100.
        s.prepend_history(&page(1, 50, &HashSet::new()), false, true, ME, cx);
        assert_eq!(ids(s), (1..=50).collect::<Vec<_>>());
    });
}

#[gpui::test]
fn deletes_wait_for_the_answer_and_come_back_on_refusal(cx: &mut TestAppContext) {
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        s.prepend_history(&page(1, 10, &HashSet::new()), true, true, ME, cx);
        assert!(s.snapshot_and_remove(5, None, cx).is_some());
        assert!(s.is_deleting(5) && s.find(5).is_none());
        // A page that still holds it doesn't put it back.
        s.prepend_history(&page(1, 10, &HashSet::new()), true, true, ME, cx);
        assert!(s.find(5).is_none());
        assert!(s.restore_pending_deletion(5, cx));
        assert_eq!(ids(s), (1..=10).collect::<Vec<_>>());
        assert!(s.snapshot_and_remove(6, None, cx).is_some());
        assert!(s.clear_pending_deletion(6));
        assert!(!s.restore_pending_deletion(6, cx));
    });
}

#[gpui::test]
fn trims_and_a_jump_to_present(cx: &mut TestAppContext) {
    let slice = channel_slice(cx);
    slice.update(cx, |s, cx| {
        s.prepend_history(&page(1, 100, &HashSet::new()), true, true, ME, cx);
        s.trim_tail(60, cx);
        assert!(s.has_more_after && s.newest_stored_id() == 60);
        s.trim_head(40, cx);
        assert_eq!((s.oldest_stored_id(), s.rows().len()), (21, 40));
        // Never trims a send in flight off the tail.
        s.push_bubble(bubble("n", ME, "x", 0, 60), cx);
        s.trim_tail(10, cx);
        assert_eq!(s.rows().len(), 41);
        s.reset_for_jump(cx);
        assert_eq!(ids(s), vec![0], "only the unconfirmed send stays");
        assert!(!s.fetched && !s.has_more_after && s.has_more_before);
    });
}
