//! One loaded message slice: a channel's (one entity per `(server, channel)`) or a DM
//! conversation's (one per peer uid). What `chatStore` kept across eight per-channel maps
//! (`messagesByChannel`, `hasMoreHistory`, `hasMoreAfter`, `historyLoading`,
//! `historyFetched`, `scrollPositionsByChannel`, `resyncing`, `pendingDeletions`, plus the
//! module-level `heldLive`) lives here as one entity, so a message in one channel notifies
//! that channel's list and nothing else.
//!
//! # The contract with the list view
//!
//! The slice owns an ordered window of rows ([`MessageSlice::rows`], `Arc<Message>`, stable
//! identity [`Message::key`]) and, parallel to it, each row's derived facts
//! ([`MessageSlice::facts`]: grouping with the previous row, day divider, own, edited,
//! can-edit / can-delete / can-reply). Every change to either is recorded as [`RowOp`]s
//! under a revision number:
//!
//! - `Splice { at, removed, inserted }`: at position `at`, `removed` rows went and
//!   `inserted` rows came (in the slice's order at that point of the sequence);
//! - `Update { at }`: the row at `at` kept its identity but its content, flags or facts
//!   changed (an edit, a send shown as sent, a neighbour that now groups with it).
//!
//! A list keeps the revision it last drew ([`MessageSlice::revision`]) and a per-row state
//! array aligned with the rows it drew. On notify it calls
//! [`MessageSlice::changes_since`]: [`Changes::Ops`] replays in order (splice its array,
//! mark updated entries dirty) and leaves it aligned 1:1 with `rows()` again, after which
//! the inserted and dirty entries are (re)built from `rows()[i]` / `facts()[i]`; every
//! other entry is guaranteed unchanged (same `Arc`, same facts). [`Changes::Reset`] (a list
//! older than the log, or a change that reordered rows) means rebuild, reusing whatever it
//! caches by row key. A live message appended is one `Splice` (+ one `Update` when the
//! bubble after it re-groups); a history page above is one `Splice` at 0; an echo
//! replacing its bubble is one `Splice` of 1 for 1.
//!
//! Optimistic sends sit at the tail (channels: always after every stored row). Message
//! text is never parsed here: the view caches layouts by row key and redoes the dirty ones.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use gpui::{Context, SharedString, Task};

use crate::chat::merge::{apply_held_rows, merge_stored_page, newest_stored_id};
use crate::chat::message::{Message, RowKey};
use crate::ids::{ChannelId, ServerId, Uid};

type Row = Arc<Message>;

/// A slice that no list is trimming keeps at most this many rows when live messages arrive
/// (`BACKGROUND_SLICE_CAP`, RealMessageList's `MAX_ROWS`).
pub const BACKGROUND_SLICE_CAP: usize = 150;
/// Live rows held for a re-syncing slice before giving up on them (`HELD_CAP`).
const HELD_CAP: usize = 200;
/// Ops kept for lists that are behind; a list further behind gets `Reset`.
const LOG_CAP: usize = 256;
/// A diff with more ops than this is sent as `Reset` (cheaper for the list to rebuild).
const MAX_DIFF_OPS: usize = 64;
/// Two messages of one sender this close (and on one day) share a header.
const GROUP_WINDOW_SECS: i64 = 7 * 60;

/// Whose slice it is.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum SliceKind {
    Channel { server: ServerId, channel: ChannelId },
    Dm { peer: Uid },
}

/// A channel's saved scroll position (`ScrollPosition`): `at_bottom` wins (land at the
/// newest message); otherwise the list restores to `anchor_id` at `offset` px from the top.
#[derive(Debug, Clone, Copy, PartialEq, Default, serde::Serialize)]
pub struct ScrollPosition {
    pub at_bottom: bool,
    pub anchor_id: i64,
    pub offset: f32,
}

/// One change to the rows (see the module doc).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(tag = "op", rename_all = "lowercase")]
pub enum RowOp {
    Splice { at: u32, removed: u32, inserted: u32 },
    Update { at: u32 },
}

/// What a list has to do to catch up.
#[derive(Debug, PartialEq, Eq)]
pub enum Changes<'a> {
    /// Replay these, in order. Empty: nothing changed.
    Ops(&'a [RowOp]),
    /// Rebuild from `rows()` (reuse caches by row key).
    Reset,
}

/// What the renderer computed per row in `renderBubble`, computed here once per change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
pub struct RowFacts {
    /// Shares the previous row's header (same sender, same day, under 7 min, not a reply).
    pub grouped: bool,
    /// A day divider opens this row (another local day than the previous row).
    pub new_day: bool,
    /// Sent by the signed-in account.
    pub own: bool,
    /// "(edited)".
    pub edited: bool,
    pub can_edit: bool,
    pub can_reply: bool,
    /// Our own stored message: always deletable by us.
    pub can_delete_own: bool,
    /// A stored channel message: deletable by whoever holds Manage Messages
    /// (`Model::can_delete_others`). Never for DMs.
    pub others_may_delete: bool,
    /// Someone else answering one of our messages (channels only: the reply rail).
    pub replies_to_me: bool,
}

impl RowFacts {
    /// The trash icon (`canDelete`): `can_delete_others` is the viewer's Manage Messages.
    pub fn can_delete(&self, can_delete_others: bool) -> bool {
        self.can_delete_own || (self.others_may_delete && can_delete_others)
    }
}

/// `shouldGroup` + `startsNewDay`, for `row` after `prev`.
fn neighbour_facts(prev: Option<&Message>, row: &Message) -> (bool, bool) {
    let Some(prev) = prev else { return (false, false) };
    let new_day = prev.day != 0 && row.day != 0 && prev.day != row.day;
    let grouped = prev.sender_uid == row.sender_uid
        // No account on either side (system rows, deleted senders): the names decide.
        && !(row.sender_uid == 0 && prev.sender != row.sender)
        && match (prev.timestamp, row.timestamp) {
            (Some(a), Some(b)) => !new_day && (b - a).abs() < GROUP_WINDOW_SECS,
            _ => false,
        };
    (grouped && row.reply.is_none(), new_day)
}

/// Live rows held while the slice re-syncs (`heldLive`): off, holding, or given up.
#[derive(Debug, Default)]
enum Held {
    #[default]
    Off,
    Holding(Vec<Row>),
    Overflowed,
}

/// A message removed optimistically, waiting for the server's answer (`pendingDeletions`).
struct PendingDeletion {
    row: Row,
    /// The 5 s watchdog that puts it back when no answer comes: exists while it waits.
    _watchdog: Option<Task<()>>,
}

/// What [`MessageSlice::add_message`] did with a live message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddOutcome {
    /// Merged into the rows.
    Added,
    /// Past a gap (the slice is windowed): dropped, or held during a re-sync.
    Dropped,
    Held,
    /// A background slice over its cap, left scrolled up: it went windowed instead.
    WentWindowed,
}

pub struct MessageSlice {
    kind: SliceKind,
    own_uid: Uid,
    /// Channels: end-to-end encrypted (fixed for life). A sealed message in a channel that
    /// isn't can't be edited.
    channel_encrypted: bool,
    rows: Vec<Row>,
    facts: Vec<RowFacts>,
    /// The renderer's `messagesByChannel` holds this key (a page, a wipe, a prune or a live
    /// message put rows here). Background live messages only go into a slice that has
    /// rows or a fetch in flight.
    loaded_rows: bool,
    /// Older messages exist above the slice (`hasMoreHistory`).
    pub has_more_before: bool,
    /// Newer messages exist below the slice (`hasMoreAfter`): the slice is windowed (a
    /// jump, a trim, a reconnect). Live messages are dropped (or held) instead of appended.
    pub has_more_after: bool,
    /// A history request is in flight (`historyLoading`).
    pub loading: bool,
    /// The newest page (or a jump window) is in (`historyFetched`; DMs: `historyLoaded`).
    pub fetched: bool,
    /// `historyLoading` / `historyFetched` hold the key (what `markServerStale` looks at).
    history_touched: bool,
    /// The newest-page load after a reconnect is in flight (`resyncing`).
    pub resyncing: bool,
    held: Held,
    scroll: Option<ScrollPosition>,
    pending_deletions: HashMap<i64, PendingDeletion>,
    /// The 30 s echo watchdog of each optimistic send: exists while its bubble is pending.
    echo_watchdogs: HashMap<SharedString, Task<()>>,
    /// Paging guards (the panels' `lastRequestedBeforeIdRef` / `…AfterIdRef`): a boundary
    /// asked for once until its reply lands or the wait ends without one.
    pub(crate) last_before: i64,
    pub(crate) last_after: i64,
    /// A jump-to-message window being fetched (`pendingJumpIdRef`).
    pub(crate) pending_jump: Option<i64>,
    revision: u64,
    log: Vec<RowOp>,
    /// The revision of `log[0]` (`revision + 1` when the log is empty).
    first_logged: u64,
}

impl MessageSlice {
    pub fn new(kind: SliceKind, own_uid: Uid, channel_encrypted: bool) -> MessageSlice {
        MessageSlice {
            kind,
            own_uid,
            channel_encrypted,
            rows: Vec::new(),
            facts: Vec::new(),
            loaded_rows: false,
            has_more_before: false,
            has_more_after: false,
            loading: false,
            fetched: false,
            history_touched: false,
            resyncing: false,
            held: Held::Off,
            scroll: None,
            pending_deletions: HashMap::new(),
            echo_watchdogs: HashMap::new(),
            last_before: 0,
            last_after: 0,
            pending_jump: None,
            revision: 0,
            log: Vec::new(),
            first_logged: 1,
        }
    }

    // ── Reads ──

    pub fn kind(&self) -> &SliceKind {
        &self.kind
    }

    pub fn rows(&self) -> &[Row] {
        &self.rows
    }

    pub fn facts(&self) -> &[RowFacts] {
        &self.facts
    }

    pub fn revision(&self) -> u64 {
        self.revision
    }

    /// What changed after revision `seen` (see the module doc).
    pub fn changes_since(&self, seen: u64) -> Changes<'_> {
        if seen >= self.revision {
            return Changes::Ops(&[]);
        }
        if seen + 1 >= self.first_logged {
            let start = (seen + 1 - self.first_logged) as usize;
            return Changes::Ops(&self.log[start..]);
        }
        Changes::Reset
    }

    pub fn scroll(&self) -> Option<ScrollPosition> {
        self.scroll
    }

    /// The row with server id `id`.
    pub fn find(&self, id: i64) -> Option<&Row> {
        if id <= 0 {
            return None;
        }
        self.rows.iter().find(|m| m.id == id)
    }

    /// The pending bubble of a send.
    pub fn bubble(&self, nonce: &str) -> Option<&Row> {
        self.rows.iter().find(|m| m.id == 0 && m.nonce.as_deref() == Some(nonce))
    }

    pub fn newest_stored_id(&self) -> i64 {
        newest_stored_id(&self.rows)
    }

    /// The oldest stored row (the boundary an older page is asked for below).
    pub fn oldest_stored_id(&self) -> i64 {
        self.rows.iter().map(|m| m.id).find(|id| *id > 0).unwrap_or(0)
    }

    /// Optimistic sends not yet confirmed (`id == 0` with a nonce).
    pub fn has_unconfirmed(&self) -> bool {
        self.rows.iter().any(|m| m.id == 0 && m.nonce.is_some())
    }

    /// A delete of `id` waits for the server.
    pub fn is_deleting(&self, id: i64) -> bool {
        self.pending_deletions.contains_key(&id)
    }

    /// What the list shows as windowed: the store's flag, except during a re-sync (shown as
    /// caught up: no jump-to-present pill, bottom-follow on, sends go through).
    pub fn list_windowed(&self) -> bool {
        self.has_more_after && !self.resyncing
    }

    /// The renderer's `messagesByChannel` holds this slice.
    pub fn has_rows_entry(&self) -> bool {
        self.loaded_rows
    }

    pub(crate) fn history_touched(&self) -> bool {
        self.history_touched
    }

    pub fn holds_echo_watchdog(&self, nonce: &str) -> bool {
        self.echo_watchdogs.contains_key(nonce)
    }

    pub fn pending_deletion_count(&self) -> usize {
        self.pending_deletions.len()
    }

    // ── The change log and the row primitives ──

    fn push_op(&mut self, op: RowOp) {
        self.revision += 1;
        if self.log.is_empty() {
            self.first_logged = self.revision;
        }
        self.log.push(op);
        if self.log.len() > 2 * LOG_CAP {
            let cut = self.log.len() - LOG_CAP;
            self.log.drain(..cut);
            self.first_logged += cut as u64;
        }
    }

    fn push_reset(&mut self) {
        self.revision += 1;
        self.log.clear();
        self.first_logged = self.revision + 1;
    }

    fn facts_for(&self, prev: Option<&Message>, row: &Message) -> RowFacts {
        let (grouped, new_day) = neighbour_facts(prev, row);
        let own = self.own_uid != 0 && row.sender_uid == self.own_uid;
        let stored = row.id > 0;
        match self.kind {
            SliceKind::Channel { .. } => RowFacts {
                grouped,
                new_day,
                own,
                edited: row.edited_at > 0,
                // A sealed message in a channel whose encryption was switched off (before it
                // became fixed at creation): the server refuses to turn it into plaintext.
                can_edit: stored && own && (self.channel_encrypted || !row.encrypted),
                can_reply: stored,
                can_delete_own: stored && own,
                others_may_delete: stored,
                replies_to_me: row.reply.as_ref().is_some_and(|r| r.sender_uid != 0 && r.sender_uid == self.own_uid)
                    && !own,
            },
            SliceKind::Dm { .. } => RowFacts {
                grouped,
                new_day,
                own,
                edited: row.edited_at > 0,
                // Can't edit a body this device couldn't open.
                can_edit: stored && own && row.decrypt_error.is_none(),
                can_reply: stored,
                can_delete_own: stored && own,
                others_may_delete: false,
                // In a DM nearly every reply is to you.
                replies_to_me: false,
            },
        }
    }

    fn fact_at(&self, i: usize) -> RowFacts {
        let prev = if i > 0 { Some(&*self.rows[i - 1]) } else { None };
        self.facts_for(prev, &self.rows[i])
    }

    /// Recompute the facts at `i` (after a neighbour changed); log an update if they moved.
    fn refresh_fact(&mut self, i: usize) {
        if i >= self.rows.len() {
            return;
        }
        let f = self.fact_at(i);
        if self.facts[i] != f {
            self.facts[i] = f;
            self.push_op(RowOp::Update { at: i as u32 });
        }
    }

    fn insert_row(&mut self, at: usize, row: Row) {
        self.rows.insert(at, row);
        let f = self.fact_at(at);
        self.facts.insert(at, f);
        self.push_op(RowOp::Splice { at: at as u32, removed: 0, inserted: 1 });
        self.refresh_fact(at + 1);
        self.loaded_rows = true;
    }

    fn remove_row(&mut self, at: usize) -> Row {
        let row = self.rows.remove(at);
        self.facts.remove(at);
        self.push_op(RowOp::Splice { at: at as u32, removed: 1, inserted: 0 });
        self.refresh_fact(at);
        row
    }

    fn replace_row(&mut self, at: usize, row: Row) {
        let same_key = self.rows[at].key() == row.key();
        self.rows[at] = row;
        self.facts[at] = self.fact_at(at);
        self.push_op(if same_key {
            RowOp::Update { at: at as u32 }
        } else {
            RowOp::Splice { at: at as u32, removed: 1, inserted: 1 }
        });
        self.refresh_fact(at + 1);
    }

    /// Replace the rows wholesale, logging the difference: one pass over both lists keyed
    /// by row identity (kept rows in the same order → splices for the gaps, updates for
    /// kept rows whose `Arc` or facts changed; anything else → `Reset`).
    fn set_rows(&mut self, new: Vec<Row>) {
        self.loaded_rows = true;
        let mut facts = Vec::with_capacity(new.len());
        for i in 0..new.len() {
            let prev = if i > 0 { Some(&*new[i - 1]) } else { None };
            facts.push(self.facts_for(prev, &new[i]));
        }
        let ops = diff(&self.rows, &self.facts, &new, &facts);
        self.rows = new;
        self.facts = facts;
        match ops {
            Some(ops) => {
                for op in ops {
                    self.push_op(op);
                }
            }
            None => self.push_reset(),
        }
        self.prune_watchdogs();
    }

    /// An echo watchdog whose bubble is no longer pending has nothing left to do.
    fn prune_watchdogs(&mut self) {
        if self.echo_watchdogs.is_empty() {
            return;
        }
        let rows = &self.rows;
        self.echo_watchdogs
            .retain(|nonce, _| rows.iter().any(|m| m.id == 0 && m.pending && m.nonce.as_ref() == Some(nonce)));
    }

    // ── Live messages (channels) ──

    /// `mergeMessage`: sorted by id, deduped by id; optimistic rows at the tail; a stored
    /// message carrying a bubble's nonce reaps the bubble. In place, with direct ops for the
    /// common case (a message newer than every stored row).
    fn merge_message(&mut self, incoming: Row) {
        if incoming.id == 0 {
            let at = self.rows.len();
            self.insert_row(at, incoming);
            return;
        }
        let len = self.rows.len();
        // One tail scan: the last stored row, and a bubble in the trailing block echoing us.
        let mut last_real: Option<usize> = None;
        let mut nonce_match: Option<usize> = None;
        for i in (0..len).rev() {
            let m = &self.rows[i];
            if m.id == 0 {
                if nonce_match.is_none() && incoming.nonce.is_some() && m.nonce == incoming.nonce {
                    nonce_match = Some(i);
                }
            } else {
                last_real = Some(i);
                break;
            }
        }
        if last_real.is_none_or(|i| self.rows[i].id < incoming.id) {
            let at = last_real.map_or(0, |i| i + 1);
            match nonce_match {
                // The echo of our send takes its bubble's place when it would land there.
                Some(n) if n == at => self.replace_row(at, incoming),
                Some(n) => {
                    self.remove_row(n);
                    self.insert_row(at, incoming);
                }
                None => self.insert_row(at, incoming),
            }
            self.prune_watchdogs();
            return;
        }
        // Back-fill, out of order, or a duplicate: after the last stored row with a smaller
        // id, optimistic rows ignored for the position.
        let mut filtered: Vec<Row> = Vec::with_capacity(len + 1);
        for m in &self.rows {
            if m.id == incoming.id {
                continue;
            }
            if m.id == 0 && incoming.nonce.is_some() && m.nonce == incoming.nonce {
                continue;
            }
            filtered.push(m.clone());
        }
        let mut at = filtered.len();
        for i in (0..filtered.len()).rev() {
            if filtered[i].id != 0 && filtered[i].id < incoming.id {
                at = i + 1;
                break;
            }
            if i == 0 {
                at = 0;
            }
        }
        filtered.insert(at, incoming);
        self.set_rows(filtered);
    }

    /// A live channel message (`addMessage`). `displayed`: the channel's list is mounted
    /// (it keeps its slice bounded itself); any other slice is capped at
    /// [`BACKGROUND_SLICE_CAP`]. The caller creates the slice only when one exists or a
    /// fetch is in flight (a never-loaded channel's first visit fetches anyway).
    pub fn add_message(&mut self, message: Row, displayed: bool, cx: &mut Context<Self>) -> AddOutcome {
        // Windowed (not at the live bottom): a live message belongs past a hidden gap.
        // Optimistic sends never get here in that state (the composer jumps to present
        // first). During a re-sync it waits for the newest page instead.
        if self.has_more_after && message.id != 0 {
            if self.resyncing {
                self.hold_live(message);
                cx.notify();
                return AddOutcome::Held;
            }
            return AddOutcome::Dropped;
        }
        if !displayed && message.id != 0 {
            if !self.loaded_rows && !self.loading {
                return AddOutcome::Dropped;
            }
            let mut merged = merge_message_vec(&self.rows, message);
            if merged.len() > BACKGROUND_SLICE_CAP {
                // Left scrolled up with no send in flight → windowed, like a tail trim:
                // keep the slice (and its anchor) intact; the return visit shows the
                // jump-to-present pill. Never with a send pending: its echo would be dropped
                // too and the watchdog would withdraw a delivered message.
                if self.scroll.is_some_and(|p| !p.at_bottom) && !merged.iter().any(|m| m.id == 0) {
                    self.has_more_after = true;
                    cx.notify();
                    return AddOutcome::WentWindowed;
                }
                // Caught up (or a send in flight) → the newest rows; bubbles ride along.
                merged.drain(..merged.len() - BACKGROUND_SLICE_CAP);
                self.has_more_before = true;
            }
            self.set_rows(merged);
            cx.notify();
            return AddOutcome::Added;
        }
        self.merge_message(message);
        cx.notify();
        AddOutcome::Added
    }

    fn drop_head(&mut self, cut: usize) {
        if cut == 0 {
            return;
        }
        self.rows.drain(..cut);
        self.facts.drain(..cut);
        self.push_op(RowOp::Splice { at: 0, removed: cut as u32, inserted: 0 });
        self.refresh_fact(0);
    }

    /// A live row for a re-syncing slice waits (`holdLive`). If it is the echo of a send of
    /// ours the server has stored it: the bubble shows as sent now.
    pub(crate) fn hold_live(&mut self, message: Row) {
        let Held::Holding(held) = &mut self.held else { return };
        if held.len() >= HELD_CAP {
            self.held = Held::Overflowed;
            return;
        }
        held.push(message.clone());
        let Some(nonce) = &message.nonce else { return };
        if let Some(i) = self.rows.iter().position(|m| m.id == 0 && m.pending && m.nonce.as_ref() == Some(nonce)) {
            let settled = Arc::new(self.rows[i].settled());
            self.replace_row(i, settled);
            self.prune_watchdogs();
        }
    }

    /// The re-syncing slice reached the present: put in what waited (`releaseHeld`).
    pub(crate) fn release_held(&mut self, list: Vec<Row>) -> Vec<Row> {
        match std::mem::replace(&mut self.held, Held::Off) {
            Held::Off => list,
            Held::Holding(held) => {
                self.held = Held::Holding(Vec::new());
                apply_held_rows(&list, &held).unwrap_or(list)
            }
            Held::Overflowed => {
                self.held = Held::Holding(Vec::new());
                list
            }
        }
    }

    /// Keep held rows in step with deletes and edits that arrive meanwhile.
    pub(crate) fn update_held(&mut self, f: impl FnOnce(&mut Vec<Row>)) {
        if let Held::Holding(held) = &mut self.held
            && !held.is_empty()
        {
            f(held);
        }
    }

    pub fn held_count(&self) -> Option<usize> {
        match &self.held {
            Held::Holding(h) => Some(h.len()),
            _ => None,
        }
    }

    pub(crate) fn pending_deletion_ids(&self) -> Option<HashSet<i64>> {
        (!self.pending_deletions.is_empty()).then(|| self.pending_deletions.keys().copied().collect())
    }

    // ── History pages (channels; DMs have their twins in `dm::conversation`) ──

    /// A newest or older page (`prependHistory`): merged by id, settling sends whose echo
    /// was lost, or replacing the slice when it lies entirely above it. `latest`: the
    /// server echoed no cursor (the channel's newest page).
    pub fn prepend_history(&mut self, page: &[Row], has_more: bool, latest: bool, own_uid: Uid, cx: &mut Context<Self>) {
        let skip = self.pending_deletion_ids();
        let r = merge_stored_page(&self.rows, page, own_uid, 0, skip.as_ref(), latest);
        let mut list = r.list;
        // A re-sync's newest page: what arrived meanwhile goes in with it.
        if self.resyncing && r.reached_newest {
            let base = list.take().unwrap_or_else(|| self.rows.clone());
            list = Some(self.release_held(base));
        }
        match list {
            Some(list) => self.set_rows(list),
            None => self.loaded_rows = true,
        }
        if r.head_extended {
            self.has_more_before = has_more;
        }
        if r.reached_newest {
            self.has_more_after = false;
        }
        cx.notify();
    }

    /// A jump window (`setChannelWindow`): replaces the slice; optimistic rows go.
    pub fn set_window(&mut self, page: &[Row], has_more_older: bool, has_more_newer: bool, cx: &mut Context<Self>) {
        let mut list: Vec<Row> = page.iter().filter(|m| m.id != 0).cloned().collect();
        list.sort_by_key(|m| m.id);
        self.set_rows(list);
        self.has_more_before = has_more_older;
        self.has_more_after = has_more_newer;
        self.fetched = true;
        self.history_touched = true;
        cx.notify();
    }

    /// Downward paging, a re-sync or an echo check (`appendNewer`): the rows above
    /// `after_id`. Only a windowed slice can reach the present.
    pub fn append_newer(&mut self, page: &[Row], has_more_newer: bool, after_id: i64, own_uid: Uid, cx: &mut Context<Self>) {
        let skip = self.pending_deletion_ids();
        let r = merge_stored_page(&self.rows, page, own_uid, after_id, skip.as_ref(), false);
        if r.list.is_none() && !r.applied {
            return;
        }
        let windowed = self.has_more_after;
        let mut list = r.list;
        if r.applied && windowed && !has_more_newer && self.resyncing {
            let base = list.take().unwrap_or_else(|| self.rows.clone());
            list = Some(self.release_held(base));
        }
        if let Some(list) = list {
            self.set_rows(list);
        }
        if r.applied {
            self.has_more_after = windowed && has_more_newer;
        }
        cx.notify();
    }

    /// Jump to present (`resetChannelForJump`): only unconfirmed sends stay; the newest page
    /// reloads.
    pub fn reset_for_jump(&mut self, cx: &mut Context<Self>) {
        let keep: Vec<Row> = self.rows.iter().filter(|m| m.id == 0 && m.nonce.is_some()).cloned().collect();
        self.set_rows(keep);
        self.has_more_before = true;
        self.has_more_after = false;
        self.fetched = false;
        self.history_touched = true;
        self.last_before = 0;
        self.last_after = 0;
        self.pending_jump = None;
        cx.notify();
    }

    /// Newer messages may exist below: live arrivals wait, downward paging fills the gap.
    pub fn mark_windowed(&mut self, cx: &mut Context<Self>) {
        if !self.has_more_after {
            self.has_more_after = true;
            cx.notify();
        }
    }

    /// Start (windowed in the same update) or end a re-sync (`setResyncing`).
    pub fn set_resyncing(&mut self, on: bool, cx: &mut Context<Self>) {
        if on {
            self.held = Held::Holding(Vec::new());
            self.resyncing = true;
            self.has_more_after = true;
            cx.notify();
            return;
        }
        // Anything still held belongs past whatever gap the slice kept (the newest page
        // never came): downward paging and the echo watchdog take it from here.
        self.held = Held::Off;
        if self.resyncing {
            self.resyncing = false;
            cx.notify();
        }
    }

    /// Sliding-window trims (the list keeps itself bounded): keep the oldest `keep` rows;
    /// the slice goes windowed. Never while a send is in flight.
    pub fn trim_tail(&mut self, keep: usize, cx: &mut Context<Self>) {
        if !self.loaded_rows || keep == 0 || keep >= self.rows.len() {
            return;
        }
        if self.rows.iter().any(|m| m.id <= 0) {
            return;
        }
        let removed = self.rows.len() - keep;
        self.rows.truncate(keep);
        self.facts.truncate(keep);
        self.push_op(RowOp::Splice { at: keep as u32, removed: removed as u32, inserted: 0 });
        self.has_more_after = true;
        cx.notify();
    }

    /// Keep the newest `keep` rows (bubbles ride along at the tail); older pages reload.
    pub fn trim_head(&mut self, keep: usize, cx: &mut Context<Self>) {
        if !self.loaded_rows || keep == 0 || keep >= self.rows.len() {
            return;
        }
        let cut = self.rows.len() - keep;
        self.drop_head(cut);
        self.has_more_before = true;
        cx.notify();
    }

    pub fn set_loading(&mut self, loading: bool, cx: &mut Context<Self>) {
        self.history_touched = true;
        if self.loading != loading {
            self.loading = loading;
            cx.notify();
        }
    }

    pub fn mark_fetched(&mut self, cx: &mut Context<Self>) {
        self.history_touched = true;
        if !self.fetched {
            self.fetched = true;
            cx.notify();
        }
    }

    /// The server's connection authenticated again while this slice was not displayed
    /// (`markServerStale`'s per-channel part). Returns false when the slice should be
    /// dropped (the next visit loads the newest page); true when it went windowed (left
    /// scrolled up, or holding an unconfirmed send) and keeps its place.
    pub(crate) fn go_stale(&mut self, cx: &mut Context<Self>) -> bool {
        let unconfirmed = self.has_unconfirmed();
        let stored = self.rows.iter().any(|m| m.id > 0);
        let scrolled_up = self.scroll.is_some_and(|p| !p.at_bottom);
        if !(unconfirmed || (stored && scrolled_up)) {
            return false;
        }
        self.has_more_after = true;
        // A request made on the dropped connection is never answered.
        self.loading = false;
        self.last_before = 0;
        self.last_after = 0;
        self.pending_jump = None;
        cx.notify();
        true
    }

    /// Retention removed messages (`applyChannelPruned`); bubbles stay. Attachment
    /// tombstones aren't applied, as in the renderer.
    pub fn apply_pruned(&mut self, ids: &[i64], cx: &mut Context<Self>) {
        let gone: HashSet<i64> = ids.iter().copied().collect();
        self.update_held(|held| held.retain(|m| !gone.contains(&m.id)));
        let keep: Vec<Row> = self.rows.iter().filter(|m| m.id == 0 || !gone.contains(&m.id)).cloned().collect();
        if keep.len() != self.rows.len() || !self.loaded_rows {
            self.set_rows(keep);
            cx.notify();
        }
    }

    /// The channel's history was wiped (`applyChannelWiped`).
    pub fn apply_wiped(&mut self, cx: &mut Context<Self>) {
        self.update_held(|held| held.clear());
        self.set_rows(Vec::new());
        self.has_more_before = false;
        self.has_more_after = false;
        self.fetched = true;
        self.history_touched = true;
        cx.notify();
    }

    /// A delete, ours or anyone's (`removeMessage`). Idempotent.
    pub fn remove_message(&mut self, id: i64, cx: &mut Context<Self>) -> bool {
        self.update_held(|held| held.retain(|m| m.id != id));
        if !self.loaded_rows {
            return false;
        }
        let Some(i) = self.rows.iter().position(|m| m.id == id) else { return false };
        self.remove_row(i);
        cx.notify();
        true
    }

    /// An edit broadcast (`applyEdit` / `applyDmEdit`). `suppress_embeds` is `None` for DMs.
    /// Returns the edited row, if the slice holds it.
    pub fn apply_edit(&mut self, id: i64, edit: &crate::chat::message::Edit, cx: &mut Context<Self>) -> Option<Row> {
        self.update_held(|held| {
            for m in held.iter_mut().filter(|m| m.id == id) {
                *m = Arc::new(m.edited(edit));
            }
        });
        let i = self.rows.iter().position(|m| m.id == id && id > 0)?;
        let row = Arc::new(self.rows[i].edited(edit));
        self.replace_row(i, row.clone());
        cx.notify();
        Some(row)
    }

    /// Withdraw an optimistic send by its nonce (refused, abandoned, never confirmed).
    /// Returns the bubble, so its text can go back in the message box.
    pub fn remove_by_nonce(&mut self, nonce: &str, cx: &mut Context<Self>) -> Option<Row> {
        let mut removed = None;
        while let Some(i) = self.rows.iter().position(|m| m.id == 0 && m.nonce.as_deref() == Some(nonce)) {
            let row = self.remove_row(i);
            removed.get_or_insert(row);
        }
        if removed.is_some() {
            self.echo_watchdogs.remove(nonce);
            cx.notify();
        }
        removed
    }

    /// Optimistic delete: the row goes now and waits in `pending_deletions` for the answer
    /// (`snapshotAndRemove`). `watchdog` puts it back after 5 s without one.
    pub fn snapshot_and_remove(&mut self, id: i64, watchdog: Option<Task<()>>, cx: &mut Context<Self>) -> Option<Row> {
        if !self.loaded_rows {
            return None;
        }
        let i = self.rows.iter().position(|m| m.id == id && id > 0)?;
        let row = self.remove_row(i);
        self.pending_deletions.insert(id, PendingDeletion { row: row.clone(), _watchdog: watchdog });
        cx.notify();
        Some(row)
    }

    /// The server refused the delete (or never answered): the row comes back
    /// (`restorePendingDeletion`, by `mergeMessage`; DMs by id order).
    pub fn restore_pending_deletion(&mut self, id: i64, cx: &mut Context<Self>) -> bool {
        let Some(pending) = self.pending_deletions.remove(&id) else { return false };
        match self.kind {
            SliceKind::Channel { .. } => self.merge_message(pending.row),
            SliceKind::Dm { .. } => {
                let at = self.rows.iter().position(|m| m.id > pending.row.id).unwrap_or(self.rows.len());
                self.insert_row(at, pending.row);
            }
        }
        cx.notify();
        true
    }

    /// The delete was confirmed (answer or broadcast).
    pub fn clear_pending_deletion(&mut self, id: i64) -> bool {
        self.pending_deletions.remove(&id).is_some()
    }

    pub fn set_scroll(&mut self, position: ScrollPosition) {
        // Not drawn by anyone: no notify.
        self.scroll = Some(position);
    }

    /// Hold the echo watchdog of a send (dropped when its bubble settles or goes).
    pub fn set_echo_watchdog(&mut self, nonce: SharedString, task: Task<()>) {
        if self.bubble(&nonce).is_some_and(|m| m.pending) {
            self.echo_watchdogs.insert(nonce, task);
        }
    }

    /// Append an optimistic bubble (our send).
    pub fn push_bubble(&mut self, bubble: Row, cx: &mut Context<Self>) {
        let at = self.rows.len();
        self.insert_row(at, bubble);
        cx.notify();
    }

    // ── Test and DM-module access ──

    pub(crate) fn set_rows_for(&mut self, rows: Vec<Row>) {
        self.set_rows(rows);
    }

    pub(crate) fn insert_row_for(&mut self, at: usize, row: Row) {
        self.insert_row(at, row);
    }

    pub(crate) fn replace_row_for(&mut self, at: usize, row: Row) {
        self.replace_row(at, row);
    }

    pub(crate) fn clear_pending_deletions(&mut self) {
        self.pending_deletions.clear();
    }

    pub(crate) fn clear_held(&mut self) {
        if let Held::Holding(h) = &mut self.held {
            h.clear();
        }
    }
}

/// `mergeMessage` on a copy (the background path decides on the result before keeping it).
pub(crate) fn merge_message_vec(rows: &[Row], incoming: Row) -> Vec<Row> {
    let mut out: Vec<Row> = Vec::with_capacity(rows.len() + 1);
    if incoming.id == 0 {
        out.extend(rows.iter().cloned());
        out.push(incoming);
        return out;
    }
    let mut last_real: Option<usize> = None;
    let mut nonce_match: Option<usize> = None;
    for i in (0..rows.len()).rev() {
        let m = &rows[i];
        if m.id == 0 {
            if nonce_match.is_none() && incoming.nonce.is_some() && m.nonce == incoming.nonce {
                nonce_match = Some(i);
            }
        } else {
            last_real = Some(i);
            break;
        }
    }
    if last_real.is_none_or(|i| rows[i].id < incoming.id) {
        let at = last_real.map_or(0, |i| i + 1);
        for (i, m) in rows.iter().enumerate() {
            if i == at {
                out.push(incoming.clone());
            }
            if Some(i) != nonce_match {
                out.push(m.clone());
            }
        }
        if at >= rows.len() {
            out.push(incoming);
        }
        return out;
    }
    for m in rows {
        if m.id == incoming.id || (m.id == 0 && incoming.nonce.is_some() && m.nonce == incoming.nonce) {
            continue;
        }
        out.push(m.clone());
    }
    let mut at = out.len();
    for i in (0..out.len()).rev() {
        if out[i].id != 0 && out[i].id < incoming.id {
            at = i + 1;
            break;
        }
        if i == 0 {
            at = 0;
        }
    }
    out.insert(at, incoming);
    out
}

/// The ops turning (`old`, `old_facts`) into (`new`, `new_facts`), or `None` for `Reset`.
fn diff(old: &[Row], old_facts: &[RowFacts], new: &[Row], new_facts: &[RowFacts]) -> Option<Vec<RowOp>> {
    if old.is_empty() && new.is_empty() {
        return Some(Vec::new());
    }
    if old.is_empty() {
        return Some(vec![RowOp::Splice { at: 0, removed: 0, inserted: new.len() as u32 }]);
    }
    let mut index: HashMap<RowKey, usize> = HashMap::with_capacity(new.len());
    for (j, m) in new.iter().enumerate() {
        if index.insert(m.key(), j).is_some() {
            return None;
        }
    }
    // Rows of `old` that stay, with their place in `new`; they must keep their order.
    let mut pairs: Vec<(usize, usize)> = Vec::with_capacity(old.len().min(new.len()));
    let mut seen: HashSet<RowKey> = HashSet::with_capacity(old.len());
    for (i, m) in old.iter().enumerate() {
        let key = m.key();
        if !seen.insert(key.clone()) {
            return None;
        }
        if let Some(&j) = index.get(&key) {
            if pairs.last().is_some_and(|&(_, pj)| pj >= j) {
                return None;
            }
            pairs.push((i, j));
        }
    }
    let mut ops = Vec::new();
    let (mut oi, mut nj, mut pos) = (0usize, 0usize, 0usize);
    for &(i, j) in pairs.iter().chain(std::iter::once(&(old.len(), new.len()))) {
        let (removed, inserted) = (i - oi, j - nj);
        if removed > 0 || inserted > 0 {
            ops.push(RowOp::Splice { at: pos as u32, removed: removed as u32, inserted: inserted as u32 });
        }
        pos += inserted;
        if i == old.len() {
            break;
        }
        if !Arc::ptr_eq(&old[i], &new[j]) || old_facts[i] != new_facts[j] {
            ops.push(RowOp::Update { at: pos as u32 });
        }
        pos += 1;
        oi = i + 1;
        nj = j + 1;
        if ops.len() > MAX_DIFF_OPS {
            return None;
        }
    }
    Some(ops)
}

#[cfg(test)]
pub(crate) mod tests;
