//! Direct messages (`stores/dmStore.ts`, `features/dm/useDmEvents.ts`,
//! `features/dm/readSync.ts`).
//!
//! [`DmList`] is the conversation list: per peer uid a [`Conversation`] with the sidebar's
//! facts (last message, its time, unread count, read cursor) and the conversation's own
//! [`MessageSlice`] entity (its rows: `dm::conversation`). A DM notifies that
//! conversation's slice, and the list only when the sidebar's facts moved
//! ([`DmEvent::Changed`] names the peer). Encrypted DMs arrive decided by the core:
//! `Message::encrypted` / `decrypt_error`; the E2EE *status* (locked, not set up) is M3's
//! store, read by the views beside this one.

pub mod conversation;
pub mod read_sync;

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use futures::channel::oneshot;
use futures::future::{LocalBoxFuture, Shared};
use gpui::{AppContext as _, Context, Entity, EventEmitter, SharedString};

use crate::chat::message::Message;
use crate::chat::slice::{MessageSlice, SliceKind};
use crate::ids::Uid;
use conversation::{DmAdd, newer_of, newest_of};
use read_sync::{already_read, unread_after_read_sync};

type Row = Arc<Message>;

/// The DM grouping exposed on `Model` (`model.dm`).
#[derive(Clone)]
pub struct DmStores {
    pub list: Entity<DmList>,
}

impl DmStores {
    pub fn new(cx: &mut gpui::App) -> DmStores {
        DmStores { list: cx.new(|_| DmList::default()) }
    }
}

/// One conversation.
pub struct Conversation {
    pub peer: Uid,
    /// Their name as last delivered: the fallback for the users store.
    pub username: SharedString,
    pub slice: Entity<MessageSlice>,
    /// The newest message seen (the sidebar's preview). Kept apart from the slice because
    /// the slice is a window: a jump replaces it, a trim cuts its tail.
    pub last_message: Option<Row>,
    /// Sort key, ms (`lastMessageTime`; 0 = no messages).
    pub last_message_time_ms: i64,
    /// Messages they sent that we haven't read.
    pub unread: i64,
    /// The highest id we marked read (`mark_dm_read`'s `up_to_id`).
    pub last_read_id: i64,
    /// When the entry appeared here (orders a conversation with no messages yet).
    pub created_at_ms: i64,
}

impl Conversation {
    /// `conversationActivityTime`: an empty conversation sorts by when it appeared.
    pub fn activity_time(&self) -> i64 {
        if self.last_message_time_ms != 0 { self.last_message_time_ms } else { self.created_at_ms }
    }
}

pub enum DmEvent {
    /// The sidebar facts of one conversation changed (preview, unread, appeared).
    Changed(Uid),
    /// The open conversation changed.
    ActiveChanged,
    /// Central signed us in again: the conversation on screen catches up
    /// (`actions::dm::resync_displayed`).
    Resync,
}

impl EventEmitter<DmEvent> for DmList {}

/// A preview from `DM_CONVERSATIONS_RES`.
pub struct PreviewInput {
    pub peer: Uid,
    pub username: SharedString,
    pub row: Row,
    pub last_timestamp: i64,
    pub unread: i64,
}

#[derive(Default)]
pub struct DmList {
    conversations: HashMap<Uid, Conversation>,
    /// The open conversation (sticky across view changes, like the active community).
    pub active_peer: Option<Uid>,
    /// The conversation whose panel is mounted (`displayedDmPeer`).
    displayed: Option<Uid>,
    /// The conversation **being read** (owner, 2026-11-26: the native client marks a DM read
    /// only then): it is the open view, the window is focused and its list rests at the live
    /// bottom. Set by the conversation's view as those change; a DM arriving for it is not
    /// unread. The renderer counted the open conversation as read whatever the focus or scroll.
    reading: Option<Uid>,
    /// Per conversation, its text sends whose call hasn't returned (DmChatPanel's `sending`:
    /// the send button is disabled meanwhile).
    sending: HashMap<Uid, u32>,
    resync_epoch: u64,
    own_uid: Uid,
    /// The sidebar's order, rebuilt on first use after a change.
    sorted: RefCell<Option<Rc<[Uid]>>>,
    /// The previous send's call: the next one waits for it (`sendChainRef`).
    pub(crate) send_chain: Option<Shared<LocalBoxFuture<'static, ()>>>,
    /// The composer's inline error of the last failed send (`sendError`), per peer.
    pub send_error: Option<(Uid, SharedString)>,
    /// Echo checks waiting for central to sign us back in.
    resync_waiters: Vec<oneshot::Sender<()>>,
}

fn now_ms() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_millis() as i64)
}

fn time_ms(m: &Message) -> Option<i64> {
    m.timestamp.map(|t| t * 1000)
}

impl DmList {
    pub fn get(&self, peer: Uid) -> Option<&Conversation> {
        self.conversations.get(&peer)
    }

    pub fn slice(&self, peer: Uid) -> Option<&Entity<MessageSlice>> {
        self.conversations.get(&peer).map(|c| &c.slice)
    }

    pub fn len(&self) -> usize {
        self.conversations.len()
    }

    pub fn is_empty(&self) -> bool {
        self.conversations.is_empty()
    }

    pub fn conversations(&self) -> impl Iterator<Item = &Conversation> {
        self.conversations.values()
    }

    /// Peers, most recent activity first (the sidebar).
    pub fn sorted(&self) -> Rc<[Uid]> {
        if let Some(sorted) = &*self.sorted.borrow() {
            return sorted.clone();
        }
        let mut peers: Vec<&Conversation> = self.conversations.values().collect();
        peers.sort_by(|a, b| b.activity_time().cmp(&a.activity_time()).then(a.peer.cmp(&b.peer)));
        let sorted: Rc<[Uid]> = peers.iter().map(|c| c.peer).collect();
        *self.sorted.borrow_mut() = Some(sorted.clone());
        sorted
    }

    /// Unread DMs over every conversation (the server bar's badge).
    pub fn total_unread(&self) -> i64 {
        self.conversations.values().map(|c| c.unread.max(0)).sum()
    }

    pub fn resync_epoch(&self) -> u64 {
        self.resync_epoch
    }

    pub fn displayed(&self) -> Option<Uid> {
        self.displayed
    }

    /// The conversation whose panel is mounted (`setDisplayedDmPeer`). No notify.
    pub fn set_displayed(&mut self, peer: Option<Uid>) {
        self.displayed = peer;
    }

    /// The conversation being read (open, the window focused, its list at the live bottom).
    pub fn reading(&self) -> Option<Uid> {
        self.reading
    }

    /// The conversation's view says whether it is being read now (`peer` stops being read
    /// only if it was the one). No notify: nothing draws it; a DM arriving reads it.
    pub fn set_reading(&mut self, peer: Uid, reading: bool) {
        if reading {
            self.reading = Some(peer);
        } else if self.reading == Some(peer) {
            self.reading = None;
        }
    }

    /// A text send to `peer` is in flight (the composer disables its send button).
    pub fn sending(&self, peer: Uid) -> bool {
        self.sending.get(&peer).is_some_and(|n| *n > 0)
    }

    pub(crate) fn send_started(&mut self, peer: Uid, cx: &mut Context<Self>) {
        let n = self.sending.entry(peer).or_insert(0);
        *n += 1;
        if *n == 1 {
            cx.notify();
        }
    }

    pub(crate) fn send_ended(&mut self, peer: Uid, cx: &mut Context<Self>) {
        if let Some(n) = self.sending.get_mut(&peer) {
            *n = n.saturating_sub(1);
            if *n == 0 {
                self.sending.remove(&peer);
                cx.notify();
            }
        }
    }

    pub fn set_own_uid(&mut self, uid: Uid) {
        self.own_uid = uid;
    }

    pub fn set_active(&mut self, peer: Option<Uid>, cx: &mut Context<Self>) {
        if self.active_peer != peer {
            self.active_peer = peer;
            cx.emit(DmEvent::ActiveChanged);
            cx.notify();
        }
    }

    fn changed(&mut self, peer: Uid, cx: &mut Context<Self>) {
        *self.sorted.borrow_mut() = None;
        cx.emit(DmEvent::Changed(peer));
        cx.notify();
    }

    /// The conversation with `peer`, created empty (`emptyConversation`).
    pub fn ensure(&mut self, peer: Uid, username: SharedString, cx: &mut Context<Self>) -> &mut Conversation {
        if !self.conversations.contains_key(&peer) {
            let own = self.own_uid;
            let slice = cx.new(|_| MessageSlice::new(SliceKind::Dm { peer }, own, false));
            self.conversations.insert(
                peer,
                Conversation {
                    peer,
                    username,
                    slice,
                    last_message: None,
                    last_message_time_ms: 0,
                    unread: 0,
                    last_read_id: 0,
                    created_at_ms: now_ms(),
                },
            );
            self.changed(peer, cx);
        }
        self.conversations.get_mut(&peer).expect("just inserted")
    }

    /// A live DM (`addDmMessage`). `viewing`: the conversation is being read (open in the DM
    /// view, the window focused, its list at the live bottom: [`Self::reading`]); then it
    /// isn't unread.
    pub fn add_message(&mut self, peer: Uid, message: Row, from_self: bool, other_name: SharedString, viewing: bool, cx: &mut Context<Self>) -> DmAdd {
        let existed = self.conversations.contains_key(&peer);
        let conv = self.ensure(peer, other_name, cx);
        let slice = conv.slice.clone();
        let outcome = slice.update(cx, |s, cx| s.dm_add(message.clone(), cx));
        let time = time_ms(&message).unwrap_or_else(now_ms);
        let conv = self.conversations.get_mut(&peer).expect("ensured");
        match &outcome {
            DmAdd::Ignored | DmAdd::Held => {
                if !existed {
                    self.changed(peer, cx);
                }
            }
            DmAdd::ReplacedEcho { was } => {
                // Ours: unread and preview bookkeeping unaffected, except the preview row.
                if conv.last_message.as_ref().is_some_and(|l| Arc::ptr_eq(l, was)) {
                    conv.last_message = Some(message);
                }
                conv.last_message_time_ms = conv.last_message_time_ms.max(time);
                self.changed(peer, cx);
            }
            DmAdd::Appended => {
                // Not for our own (any device), the conversation being read, or one another
                // device of ours already read.
                if !(from_self || viewing || already_read(conv.last_read_id, message.id)) {
                    conv.unread += 1;
                }
                conv.last_message_time_ms = time;
                conv.last_message = Some(message);
                self.changed(peer, cx);
            }
        }
        outcome
    }

    /// Central's conversation previews (`hydrateConversations`): unread counts and times
    /// from the server; a conversation with no rows here gets the preview as its row.
    pub fn hydrate(&mut self, previews: Vec<PreviewInput>, cx: &mut Context<Self>) {
        for p in previews {
            let existed = self.conversations.contains_key(&p.peer);
            let conv = self.ensure(p.peer, p.username.clone(), cx);
            conv.username = p.username;
            conv.last_message_time_ms = p.last_timestamp * 1000;
            conv.unread = p.unread;
            if !existed {
                // Hydrated conversations always carry a real last message.
                conv.created_at_ms = 0;
            }
            conv.last_message = newer_of(conv.last_message.take(), Some(p.row.clone()));
            let slice = conv.slice.clone();
            slice.update(cx, |s, cx| {
                s.dm_hydrate(p.row, cx);
                // Hydrated: older messages may exist until the first page says.
                if !existed {
                    s.has_more_after = false;
                }
            });
            self.changed(p.peer, cx);
        }
    }

    fn slice_of(&self, peer: Uid) -> Option<Entity<MessageSlice>> {
        self.conversations.get(&peer).map(|c| c.slice.clone())
    }

    /// A newest or older page (`appendHistory`). `latest`: central echoed no cursor.
    pub fn append_history(&mut self, peer: Uid, page: &[Row], has_more: bool, latest: bool, cx: &mut Context<Self>) {
        let own = self.own_uid;
        let username = page.iter().find(|m| m.sender_uid == peer).map(|m| m.sender.clone()).unwrap_or_default();
        let conv = self.ensure(peer, username, cx);
        let slice = conv.slice.clone();
        let r = slice.update(cx, |s, cx| s.dm_append_history(page, has_more, latest, own, cx));
        let conv = self.conversations.get_mut(&peer).expect("ensured");
        // The newest page answers for the last message; any other page only adds older
        // rows to a slice a trim may have cut, so it never moves the time back.
        let slice_ms = r.slice_time * 1000;
        conv.last_message_time_ms = if latest {
            if slice_ms != 0 { slice_ms } else { conv.last_message_time_ms }
        } else {
            conv.last_message_time_ms.max(slice_ms)
        };
        conv.last_message = newer_of(conv.last_message.take(), r.newest);
        self.changed(peer, cx);
    }

    /// A jump window (`setDmWindow`): the sidebar's time is left as it is.
    pub fn set_window(&mut self, peer: Uid, page: &[Row], older: bool, newer: bool, cx: &mut Context<Self>) {
        let conv = self.ensure(peer, SharedString::default(), cx);
        let slice = conv.slice.clone();
        let newest = slice.update(cx, |s, cx| s.dm_set_window(page, older, newer, cx));
        let conv = self.conversations.get_mut(&peer).expect("ensured");
        conv.last_message = newer_of(conv.last_message.take(), newest);
        self.changed(peer, cx);
    }

    /// Downward paging (`appendNewerDm`); nothing for a conversation that isn't here.
    pub fn append_newer(&mut self, peer: Uid, page: &[Row], has_more_newer: bool, after_id: i64, cx: &mut Context<Self>) {
        let own = self.own_uid;
        let Some(slice) = self.slice_of(peer) else { return };
        let newest = slice.update(cx, |s, cx| s.dm_append_newer(page, has_more_newer, after_id, own, cx));
        let conv = self.conversations.get_mut(&peer).expect("present");
        let last = newer_of(conv.last_message.clone(), newest);
        if !opt_ptr_eq(&last, &conv.last_message) {
            conv.last_message = last;
            self.changed(peer, cx);
        }
    }

    /// Central signed us in again (`markHistoriesStale`): every loaded conversation but the
    /// one on screen reloads on open; the one on screen is its panel's
    /// ([`DmEvent::Resync`]).
    pub fn mark_histories_stale(&mut self, cx: &mut Context<Self>) {
        for (peer, conv) in &self.conversations {
            if Some(*peer) == self.displayed {
                continue;
            }
            conv.slice.update(cx, |s, cx| s.dm_go_stale(cx));
        }
        self.resync_epoch += 1;
        for waiter in self.resync_waiters.drain(..) {
            let _ = waiter.send(());
        }
        cx.emit(DmEvent::Resync);
    }

    /// Resolves at central's next sign-in (an echo check waiting to ask it).
    pub fn wait_for_resync(&mut self) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.resync_waiters.push(tx);
        rx
    }

    /// The composer's inline send error (`setSendError`): set by a failed send, cleared by
    /// the composer on the next send or a conversation switch.
    pub fn set_send_error(&mut self, error: Option<(Uid, SharedString)>, cx: &mut Context<Self>) {
        if self.send_error != error {
            self.send_error = error;
            cx.notify();
        }
    }

    /// Optimistically read up to `up_to_id` (`markRead`); false when nothing changed.
    pub fn mark_read(&mut self, peer: Uid, up_to_id: i64, cx: &mut Context<Self>) -> bool {
        let Some(conv) = self.conversations.get_mut(&peer) else { return false };
        // A count from central can outlive our own marker (a mark-read lost to a drop).
        if up_to_id <= conv.last_read_id && conv.unread == 0 {
            return false;
        }
        conv.unread = 0;
        conv.last_read_id = conv.last_read_id.max(up_to_id);
        self.changed(peer, cx);
        true
    }

    /// Another device of ours read the conversation up to `up_to_id` (`applyReadSync`).
    pub fn apply_read_sync(&mut self, peer: Uid, up_to_id: i64, cx: &mut Context<Self>) {
        if up_to_id <= 0 {
            return;
        }
        let me = self.own_uid;
        let Some(conv) = self.conversations.get(&peer) else { return };
        let slice = conv.slice.read(cx);
        let unread = unread_after_read_sync(conv.unread, conv.last_message.as_deref(), slice.rows(), slice.has_more_after, up_to_id, me);
        let last_read = conv.last_read_id.max(up_to_id);
        if unread == conv.unread && last_read == conv.last_read_id {
            return;
        }
        let conv = self.conversations.get_mut(&peer).expect("present");
        conv.unread = unread;
        conv.last_read_id = last_read;
        self.changed(peer, cx);
    }

    /// A DM deleted (`removeDmMessage`): the preview falls back to the loaded tail.
    pub fn remove_message(&mut self, peer: Uid, id: i64, cx: &mut Context<Self>) {
        let Some(slice) = self.slice_of(peer) else { return };
        let removed = slice.update(cx, |s, cx| s.remove_message(id, cx));
        if !removed {
            return;
        }
        self.fall_back_if_last(peer, id, cx);
    }

    fn fall_back_if_last(&mut self, peer: Uid, id: i64, cx: &mut Context<Self>) {
        let Some(conv) = self.conversations.get(&peer) else { return };
        if conv.last_message.as_ref().is_none_or(|l| l.id != id) {
            return;
        }
        let tail = conv.slice.read(cx).rows().last().cloned();
        let conv = self.conversations.get_mut(&peer).expect("present");
        conv.last_message = tail;
        self.changed(peer, cx);
    }

    /// Withdraw an optimistic DM (`removeDmMessageByNonce`); returns its bubble.
    pub fn remove_by_nonce(&mut self, peer: Uid, nonce: &str, cx: &mut Context<Self>) -> Option<Row> {
        let slice = self.slice_of(peer)?;
        let bubble = slice.update(cx, |s, cx| s.remove_by_nonce(nonce, cx))?;
        let conv = self.conversations.get(&peer)?;
        if conv.last_message.as_ref().is_some_and(|l| l.id == 0 && l.nonce.as_deref() == Some(nonce)) {
            let tail = conv.slice.read(cx).rows().last().cloned();
            self.conversations.get_mut(&peer).expect("present").last_message = tail;
            self.changed(peer, cx);
        }
        Some(bubble)
    }

    /// A DM edited (`applyDmEdit`).
    pub fn apply_edit(&mut self, peer: Uid, id: i64, edit: &crate::chat::message::Edit, cx: &mut Context<Self>) {
        let Some(slice) = self.slice_of(peer) else { return };
        let edited = slice.update(cx, |s, cx| s.apply_edit(id, edit, cx));
        if edited.is_none() {
            return;
        }
        let conv = self.conversations.get_mut(&peer).expect("present");
        if let Some(last) = conv.last_message.as_ref().filter(|l| l.id == id) {
            conv.last_message = Some(Arc::new(last.edited(edit)));
            self.changed(peer, cx);
        }
    }

    /// Optimistic delete (`snapshotAndRemoveDm`).
    pub fn snapshot_and_remove(&mut self, peer: Uid, id: i64, watchdog: Option<gpui::Task<()>>, cx: &mut Context<Self>) -> Option<Row> {
        let slice = self.slice_of(peer)?;
        let row = slice.update(cx, |s, cx| s.snapshot_and_remove(id, watchdog, cx))?;
        self.fall_back_if_last(peer, id, cx);
        Some(row)
    }

    /// The delete was refused (or timed out): the row comes back (`restorePendingDmDeletion`).
    pub fn restore_pending_deletion(&mut self, peer: Uid, id: i64, cx: &mut Context<Self>) -> bool {
        let Some(slice) = self.slice_of(peer) else { return false };
        let restored = slice.update(cx, |s, cx| s.restore_pending_deletion(id, cx));
        if restored {
            let row = slice.read(cx).find(id).cloned();
            let conv = self.conversations.get_mut(&peer).expect("present");
            conv.last_message = newer_of(conv.last_message.take(), row);
            self.changed(peer, cx);
        }
        restored
    }

    pub fn clear_pending_deletion(&mut self, peer: Uid, id: i64, cx: &mut Context<Self>) {
        if let Some(slice) = self.slice_of(peer) {
            slice.update(cx, |s, _| s.clear_pending_deletion(id));
        }
    }

    /// Keys unlocked: every row that arrived sealed holds a placeholder; all loaded history
    /// goes (`invalidateAllHistory`) and reloads.
    pub fn invalidate_all_history(&mut self, cx: &mut Context<Self>) {
        for conv in self.conversations.values() {
            conv.slice.update(cx, |s, cx| s.dm_invalidate(cx));
        }
    }

    /// The newest row of a conversation's slice, for mark-read (`latestId`).
    pub fn newest_loaded_id(&self, peer: Uid, cx: &gpui::App) -> i64 {
        self.conversations.get(&peer).map_or(0, |c| c.slice.read(cx).rows().iter().map(|m| m.id).max().unwrap_or(0))
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        self.conversations.clear();
        self.active_peer = None;
        self.displayed = None;
        self.reading = None;
        self.sending.clear();
        self.resync_epoch = 0;
        self.own_uid = 0;
        *self.sorted.borrow_mut() = None;
        self.send_chain = None;
        self.send_error = None;
        self.resync_waiters.clear();
        cx.notify();
    }

    /// `newestOf` over a conversation's loaded rows (tests, dumps).
    pub fn newest_row(&self, peer: Uid, cx: &gpui::App) -> Option<Row> {
        self.conversations.get(&peer).and_then(|c| newest_of(c.slice.read(cx).rows()))
    }
}

fn opt_ptr_eq(a: &Option<Row>, b: &Option<Row>) -> bool {
    match (a, b) {
        (Some(a), Some(b)) => Arc::ptr_eq(a, b),
        (None, None) => true,
        _ => false,
    }
}

#[cfg(test)]
mod tests;
