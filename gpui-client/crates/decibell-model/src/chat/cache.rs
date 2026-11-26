//! The registry of channel slices (`chatStore`'s per-channel maps, the message half): one
//! [`MessageSlice`] entity per `(server, channel)` that has state, the LRU of visited
//! channels (`channelAccessOrder`, 10 kept), which channel's list is mounted
//! (`displayedChannelKey`), the per-community re-sync epoch (`resyncEpoch`) and an outside
//! "go to message" (`jumpRequest`).
//!
//! It notifies only when a slice is created or dropped; a message notifies its slice alone.
//! Views that follow the re-sync or a jump request subscribe to [`CacheEvent`].

use std::collections::{HashMap, VecDeque};
use std::time::{Duration, Instant};

use futures::channel::oneshot;
use gpui::{AppContext as _, Context, Entity, EventEmitter};

use crate::chat::slice::{MessageSlice, SliceKind};
use crate::ids::{ChannelId, ServerId, Uid};

/// How many recently visited channels keep their slices (`CHANNEL_CACHE_SIZE`). Each is
/// bounded (150 rows in the background), so this costs little.
pub const CHANNEL_CACHE_SIZE: usize = 10;
/// A jump request nobody picked up within this long is dropped (the Transfers panel's
/// "Go to message" landing on a channel that never loaded).
pub const JUMP_REQUEST_TTL: Duration = Duration::from_secs(15);

pub type SliceKey = (ServerId, ChannelId);

/// "Go to message" from outside the chat panel.
#[derive(Debug, Clone, PartialEq)]
pub struct JumpRequest {
    pub server: ServerId,
    pub channel: ChannelId,
    pub message_id: i64,
    pub at: Instant,
}

pub enum CacheEvent {
    /// `server`'s connection authenticated again (review B4): the channel on screen, if it
    /// belongs to it, catches up (`actions::chat::resync_displayed`).
    Resync(ServerId),
    /// A jump was asked for; the panel showing that channel runs it once its first page
    /// is in (`take_jump_request`).
    JumpRequested,
}

impl EventEmitter<CacheEvent> for ChannelCache {}

#[derive(Default)]
pub struct ChannelCache {
    slices: HashMap<SliceKey, Entity<MessageSlice>>,
    /// Most recent first.
    access_order: VecDeque<SliceKey>,
    displayed: Option<SliceKey>,
    resync_epoch: HashMap<ServerId, u64>,
    jump_request: Option<JumpRequest>,
    /// Echo watchdogs waiting for a community to come back (`waitFor(resyncEpoch)`).
    reconnect_waiters: HashMap<ServerId, Vec<oneshot::Sender<()>>>,
}

impl ChannelCache {
    pub fn get(&self, server: ServerId, channel: &str) -> Option<&Entity<MessageSlice>> {
        self.slices.get(&(server, ChannelId::from(channel.to_string())))
    }

    pub fn len(&self) -> usize {
        self.slices.len()
    }

    pub fn is_empty(&self) -> bool {
        self.slices.is_empty()
    }

    pub fn iter(&self) -> impl Iterator<Item = (&SliceKey, &Entity<MessageSlice>)> {
        self.slices.iter()
    }

    /// The slice of a channel, created empty on first use (where the renderer created a
    /// map entry). `own_uid` and `encrypted` (the channel's flag, fixed for life) shape
    /// the rows' facts.
    pub fn ensure(&mut self, server: ServerId, channel: &ChannelId, own_uid: Uid, encrypted: bool, cx: &mut Context<Self>) -> Entity<MessageSlice> {
        let key = (server, channel.clone());
        if let Some(slice) = self.slices.get(&key) {
            return slice.clone();
        }
        let slice = cx.new(|_| MessageSlice::new(SliceKind::Channel { server, channel: channel.clone() }, own_uid, encrypted));
        self.slices.insert(key, slice.clone());
        cx.notify();
        slice
    }

    /// The channel whose list is mounted (`setDisplayedChannelKey`): it keeps its slice
    /// bounded itself; every other slice is capped by `add_message`. Not drawn: no notify.
    pub fn set_displayed(&mut self, key: Option<SliceKey>) {
        self.displayed = key;
    }

    pub fn displayed(&self) -> Option<&SliceKey> {
        self.displayed.as_ref()
    }

    pub fn is_displayed(&self, server: ServerId, channel: &str) -> bool {
        self.displayed.as_ref().is_some_and(|(s, c)| *s == server && &**c == channel)
    }

    /// `setActiveChannel`'s LRU half (`NavEvent::ChannelActivated`): the channel moves to
    /// the front; past [`CHANNEL_CACHE_SIZE`] the tail is forgotten and every slice not in
    /// the kept order is dropped.
    pub fn activate(&mut self, server: ServerId, channel: ChannelId, cx: &mut Context<Self>) {
        let key = (server, channel);
        if let Some(i) = self.access_order.iter().position(|k| *k == key) {
            self.access_order.remove(i);
        }
        self.access_order.push_front(key);
        if self.access_order.len() <= CHANNEL_CACHE_SIZE {
            return;
        }
        self.access_order.truncate(CHANNEL_CACHE_SIZE);
        let before = self.slices.len();
        let order = &self.access_order;
        self.slices.retain(|k, _| order.contains(k));
        if self.slices.len() != before {
            cx.notify();
        }
    }

    pub fn access_order(&self) -> impl Iterator<Item = &SliceKey> {
        self.access_order.iter()
    }

    pub fn resync_epoch(&self, server: ServerId) -> u64 {
        self.resync_epoch.get(&server).copied().unwrap_or(0)
    }

    /// The server's connection authenticated again (`markServerStale`): slices of channels
    /// gone from `live` are dropped; a slice the user left caught up is dropped too (the
    /// next visit loads the newest page); one left scrolled up, or holding an unconfirmed
    /// send, goes windowed. The displayed channel is left to the panel (it knows whether
    /// the user is at the bottom): [`CacheEvent::Resync`]. Echo checks waiting for the
    /// reconnect go on.
    pub fn mark_server_stale(&mut self, server: ServerId, live: &[ChannelId], cx: &mut Context<Self>) {
        let mut drop: Vec<SliceKey> = Vec::new();
        for (key, slice) in &self.slices {
            if key.0 != server {
                continue;
            }
            let keep = slice.update(cx, |s, cx| {
                if !s.has_rows_entry() && !s.history_touched() {
                    // Only a scroll position or a flag: nothing to go stale.
                    return true;
                }
                if !live.contains(&key.1) {
                    return false;
                }
                if self.displayed.as_ref() == Some(key) {
                    return true;
                }
                s.go_stale(cx)
            });
            if !keep {
                drop.push(key.clone());
            }
        }
        for key in &drop {
            self.slices.remove(key);
        }
        if !drop.is_empty() {
            cx.notify();
        }
        *self.resync_epoch.entry(server).or_insert(0) += 1;
        for waiter in self.reconnect_waiters.remove(&server).unwrap_or_default() {
            let _ = waiter.send(());
        }
        cx.emit(CacheEvent::Resync(server));
    }

    /// A channel no longer exists (`purgeChannelState`): a future channel reusing the slug
    /// refetches instead of inheriting this one's history.
    pub fn purge(&mut self, server: ServerId, channel: &ChannelId, cx: &mut Context<Self>) {
        if self.slices.remove(&(server, channel.clone())).is_some() {
            cx.notify();
        }
    }

    /// Resolves at `server`'s next authentication (an echo check waiting to ask it).
    pub fn wait_for_reconnect(&mut self, server: ServerId) -> oneshot::Receiver<()> {
        let (tx, rx) = oneshot::channel();
        self.reconnect_waiters.entry(server).or_default().push(tx);
        rx
    }

    // ── Jump requests (`goToMessage`) ──

    pub fn request_jump(&mut self, server: ServerId, channel: ChannelId, message_id: i64, now: Instant, cx: &mut Context<Self>) {
        self.jump_request = Some(JumpRequest { server, channel, message_id, at: now });
        cx.emit(CacheEvent::JumpRequested);
    }

    pub fn jump_request(&self) -> Option<&JumpRequest> {
        self.jump_request.as_ref()
    }

    /// The panel showing `(server, channel)` with its first page in takes the request; a
    /// stale one (over [`JUMP_REQUEST_TTL`]) is dropped instead.
    pub fn take_jump_request(&mut self, server: ServerId, channel: &str, now: Instant) -> Option<i64> {
        let request = self.jump_request.as_ref()?;
        if now.saturating_duration_since(request.at) > JUMP_REQUEST_TTL {
            self.jump_request = None;
            return None;
        }
        if request.server != server || &*request.channel != channel {
            return None;
        }
        self.jump_request.take().map(|r| r.message_id)
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        self.slices.clear();
        self.access_order.clear();
        self.displayed = None;
        self.resync_epoch.clear();
        self.jump_request = None;
        self.reconnect_waiters.clear();
        cx.notify();
    }
}
