//! The DM side of [`MessageSlice`] (`dmStore`'s per-conversation `messages` and flags):
//! the same rows, change log and facts as a channel, with the DM store's own rules. A live
//! DM is appended where it lands (`addDmMessage` doesn't sort; a later page re-sorts), the
//! echo of our send replaces its bubble in place, and the conversation list's preview row
//! (`preview`) stands in until the first page.

use std::sync::Arc;

use gpui::Context;

use crate::chat::merge::merge_stored_page;
use crate::chat::message::Message;
use crate::chat::slice::MessageSlice;
use crate::ids::Uid;

type Row = Arc<Message>;

/// What [`MessageSlice::dm_add`] did.
#[derive(Debug, Clone)]
pub enum DmAdd {
    /// A replayed message already here, or one past the gap of a windowed slice.
    Ignored,
    /// Held for a re-syncing slice.
    Held,
    /// Central's echo of our send took its bubble's place; `was` is the bubble.
    ReplacedEcho { was: Row },
    Appended,
}

/// What a page did, for the conversation list's preview.
pub struct DmPage {
    /// The newest row the page (or the merged slice) brought.
    pub newest: Option<Row>,
    /// The newest timestamp in the slice after the merge (seconds, 0 none).
    pub slice_time: i64,
}

/// `newestOf`: the newer by id; ties (id-less rows) go to the later one.
pub fn newest_of<'a>(rows: impl IntoIterator<Item = &'a Row>) -> Option<Row> {
    let mut best: Option<&Row> = None;
    for m in rows {
        if best.is_none_or(|b| m.id >= b.id) {
            best = Some(m);
        }
    }
    best.cloned()
}

/// `newerOf`: the candidate when it is at least as new.
pub fn newer_of(current: Option<Row>, candidate: Option<Row>) -> Option<Row> {
    match (current, candidate) {
        (c, None) => c,
        (None, n) => n,
        (Some(c), Some(n)) => Some(if n.id >= c.id { n } else { c }),
    }
}

impl MessageSlice {
    fn stored_rows(&self) -> Vec<Row> {
        self.rows().iter().filter(|m| !m.preview).cloned().collect()
    }

    /// A live DM (`addDmMessage`'s slice part).
    pub fn dm_add(&mut self, message: Row, cx: &mut Context<Self>) -> DmAdd {
        // Replayed (reconnect, at-least-once delivery): never twice.
        if message.id > 0 && self.rows().iter().any(|m| m.id == message.id) {
            return DmAdd::Ignored;
        }
        // Windowed: a live DM belongs past the gap; held during a re-sync.
        if self.has_more_after && message.id > 0 {
            if self.resyncing {
                self.hold_live(message);
                cx.notify();
                return DmAdd::Held;
            }
            return DmAdd::Ignored;
        }
        if let Some(nonce) = &message.nonce
            && let Some(i) = self.rows().iter().position(|m| m.id == 0 && m.nonce.as_ref() == Some(nonce))
        {
            let was = self.rows()[i].clone();
            self.replace_row_for(i, message);
            cx.notify();
            return DmAdd::ReplacedEcho { was };
        }
        let at = self.rows().len();
        self.insert_row_for(at, message);
        cx.notify();
        DmAdd::Appended
    }

    /// The conversation list's preview stands in for an empty slice (`hydrateConversations`).
    pub fn dm_hydrate(&mut self, preview: Row, cx: &mut Context<Self>) {
        if self.rows().is_empty() {
            self.set_rows_for(vec![preview]);
            cx.notify();
        }
        self.has_more_before = true;
    }

    /// A newest or older page (`appendHistory`): preview rows give way; merged by id.
    pub fn dm_append_history(&mut self, page: &[Row], has_more: bool, latest: bool, own_uid: Uid, cx: &mut Context<Self>) -> DmPage {
        let existing = self.stored_rows();
        let skip = self.pending_deletion_ids();
        let r = merge_stored_page(&existing, page, own_uid, 0, skip.as_ref(), latest);
        let base = r.list.unwrap_or(existing);
        let (merged, released) = if self.resyncing && r.reached_newest {
            let before = base.len();
            let list = self.release_held(base);
            let changed = list.len() != before;
            (list, changed)
        } else {
            (base, false)
        };
        let slice_time = merged.iter().filter_map(|m| m.timestamp).max().unwrap_or(0);
        let newest = if released { newest_of(&merged) } else { newest_of(page) };
        self.set_rows_for(merged);
        if r.head_extended {
            self.has_more_before = has_more;
        }
        if r.reached_newest {
            self.has_more_after = false;
        }
        self.fetched = true;
        cx.notify();
        DmPage { newest, slice_time }
    }

    /// A jump window (`setDmWindow`).
    pub fn dm_set_window(&mut self, page: &[Row], has_more_older: bool, has_more_newer: bool, cx: &mut Context<Self>) -> Option<Row> {
        let mut list: Vec<Row> = page.to_vec();
        list.sort_by_key(|m| m.id);
        let newest = newest_of(&list);
        self.set_rows_for(list);
        self.has_more_before = has_more_older;
        self.has_more_after = has_more_newer;
        self.fetched = true;
        cx.notify();
        newest
    }

    /// Downward paging (`appendNewerDm`).
    pub fn dm_append_newer(&mut self, page: &[Row], has_more_newer: bool, after_id: i64, own_uid: Uid, cx: &mut Context<Self>) -> Option<Row> {
        let existing = self.stored_rows();
        let skip = self.pending_deletion_ids();
        let r = merge_stored_page(&existing, page, own_uid, after_id, skip.as_ref(), false);
        let windowed = self.has_more_after;
        let mut list = r.list;
        let mut released = false;
        if r.applied && windowed && !has_more_newer && self.resyncing {
            let base = list.take().unwrap_or(existing);
            let before = base.len();
            let l = self.release_held(base);
            released = l.len() != before;
            list = Some(l);
        }
        let newest = match &list {
            Some(l) if released => newest_of(l),
            _ => newest_of(page),
        };
        // Unchanged: the rows stay as they were (preview rows included).
        if let Some(list) = list {
            self.set_rows_for(list);
        }
        if r.applied {
            self.has_more_after = windowed && has_more_newer;
        }
        cx.notify();
        newest
    }

    /// Jump to present (`resetDmForJump`): unconfirmed sends stay.
    pub fn dm_reset_for_jump(&mut self, cx: &mut Context<Self>) {
        let keep: Vec<Row> = self.rows().iter().filter(|m| m.id == 0 && m.nonce.is_some()).cloned().collect();
        self.set_rows_for(keep);
        self.has_more_before = true;
        self.has_more_after = false;
        self.fetched = false;
        self.last_before = 0;
        self.last_after = 0;
        self.pending_jump = None;
        cx.notify();
    }

    /// Central signed us in again (`markHistoriesStale`, one conversation not on screen):
    /// its rows drop and reload on open, unless it holds an unconfirmed send (it goes
    /// windowed and pages down).
    pub fn dm_go_stale(&mut self, cx: &mut Context<Self>) {
        let stored = self.rows().iter().any(|m| !m.preview && m.id > 0);
        if !stored {
            return;
        }
        if self.has_unconfirmed() {
            if !self.has_more_after {
                self.has_more_after = true;
                cx.notify();
            }
            return;
        }
        self.set_rows_for(Vec::new());
        self.fetched = false;
        self.has_more_before = true;
        self.has_more_after = false;
        self.last_before = 0;
        self.last_after = 0;
        cx.notify();
    }

    /// `trimDmTail`: never while a row lacks a real id.
    pub fn dm_trim_tail(&mut self, keep: usize, cx: &mut Context<Self>) {
        if self.rows().iter().any(|m| m.id <= 0) {
            return;
        }
        self.trim_tail(keep, cx);
    }

    /// Keys reload after an unlock (`invalidateAllHistory`, per conversation).
    pub fn dm_invalidate(&mut self, cx: &mut Context<Self>) {
        self.clear_held();
        self.set_rows_for(Vec::new());
        self.has_more_before = true;
        self.has_more_after = false;
        self.fetched = false;
        self.clear_pending_deletions();
        cx.notify();
    }
}
