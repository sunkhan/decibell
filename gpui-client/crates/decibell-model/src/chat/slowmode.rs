//! Slowmode, as the composer sees it (`features/chat/slowmode.ts`). The community allows
//! one message per `slowmode_seconds` per member in a channel, unless the member may
//! Manage Messages there (or owns the community). It keeps no state the client can ask
//! for, so the wait is tracked here:
//!  - every message of ours the server accepts (the echo, from this device or another)
//!    starts a full window;
//!  - a refusal carries the server's remaining wait (`retry_after_seconds`), which
//!    replaces ours (after an app restart inside a window, say).
//!
//! No timer here: whether a send is held is decided at send time against the clock, and
//! the composer's countdown pill runs its own once-a-second timer while it is on screen
//! ([`Slowmode::remaining`]).

use std::collections::HashMap;
use std::time::{Duration, Instant};

use gpui::{Context, EventEmitter};

use crate::chat::cache::SliceKey;
use crate::chat::message::Message;
use crate::ids::{ServerId, Uid};
use crate::stores::community::Channel;
use crate::stores::permissions::{has_bits, perm};

/// How far back from the tail the composer looks for a send of ours still on its way.
const PENDING_LOOKBACK: usize = 20;

pub enum SlowmodeEvent {
    /// A channel's window started, moved or ended early (a refusal).
    Changed(SliceKey),
    /// A send was held back: that channel's timer shakes (`nudge`).
    Nudged(SliceKey),
}

impl EventEmitter<SlowmodeEvent> for Slowmode {}

#[derive(Default)]
pub struct Slowmode {
    /// When the next message may go out, per channel.
    until: HashMap<SliceKey, Instant>,
}

/// The interval that applies to the local user in `channel`, in seconds (`intervalFor`):
/// 0 when it's off or bypassed (the owner, or Manage Messages there).
pub fn interval_for(channel: Option<&Channel>, is_owner: bool) -> u32 {
    let Some(ch) = channel else { return 0 };
    if ch.slowmode_seconds <= 0 || is_owner || has_bits(ch.my_permissions, perm::MANAGE_MESSAGES) {
        return 0;
    }
    ch.slowmode_seconds as u32
}

impl Slowmode {
    /// A message of ours was accepted in this channel: a new window starts (never shorter
    /// than one already running).
    pub fn note_own_message(&mut self, key: SliceKey, interval: u32, now: Instant, cx: &mut Context<Self>) {
        if interval == 0 {
            return;
        }
        let until = now + Duration::from_secs(interval as u64);
        if self.until.get(&key).is_some_and(|u| *u >= until) {
            return;
        }
        self.until.insert(key.clone(), until);
        cx.emit(SlowmodeEvent::Changed(key));
        cx.notify();
    }

    /// The server refused a message for slowmode with `seconds` still to go.
    pub fn note_refusal(&mut self, key: SliceKey, seconds: i32, now: Instant, cx: &mut Context<Self>) {
        if seconds <= 0 {
            return;
        }
        self.until.insert(key.clone(), now + Duration::from_secs(seconds as u64));
        cx.emit(SlowmodeEvent::Changed(key));
        cx.notify();
    }

    /// Whether a send in this channel has to wait (`slowmodeHolds`): inside a window, or
    /// while a message of ours is still on its way (it may yet start one). `rows`: the
    /// channel's slice.
    pub fn holds(&self, key: &SliceKey, interval: u32, rows: &[std::sync::Arc<Message>], own_uid: Uid, now: Instant) -> bool {
        if interval == 0 {
            return false;
        }
        if self.until.get(key).is_some_and(|u| *u > now) {
            return true;
        }
        rows.iter().rev().take(PENDING_LOOKBACK).any(|m| m.pending && m.sender_uid == own_uid)
    }

    /// A send was held back: shake that channel's timer.
    pub fn nudge(&mut self, key: SliceKey, cx: &mut Context<Self>) {
        cx.emit(SlowmodeEvent::Nudged(key));
    }

    /// Whole seconds left before the next message may go out (0 = none), with the interval
    /// that applies (`useSlowmode`).
    pub fn remaining(&self, key: &SliceKey, interval: u32, now: Instant) -> u32 {
        if interval == 0 {
            return 0;
        }
        match self.until.get(key) {
            Some(until) if *until > now => until.saturating_duration_since(now).as_millis().div_ceil(1000) as u32,
            _ => 0,
        }
    }

    pub fn until(&self, key: &SliceKey) -> Option<Instant> {
        self.until.get(key).copied()
    }

    pub fn forget_server(&mut self, server: ServerId) {
        self.until.retain(|k, _| k.0 != server);
    }

    pub fn reset_for_logout(&mut self, cx: &mut Context<Self>) {
        if !self.until.is_empty() {
            self.until.clear();
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;
    use crate::chat::message::testing::{bubble, stored};
    use crate::stores::community::ChannelKind;

    fn channel(slowmode: i32, perms: u64) -> Channel {
        Channel {
            id: "slow-lane".into(),
            name: "slow-lane".into(),
            kind: ChannelKind::Text,
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
            topic: "".into(),
        }
    }

    #[test]
    fn the_owner_and_manage_messages_bypass() {
        let send = perm::VIEW_CHANNEL | perm::SEND_MESSAGES;
        assert_eq!(interval_for(Some(&channel(30, send)), false), 30);
        assert_eq!(interval_for(Some(&channel(30, send)), true), 0);
        assert_eq!(interval_for(Some(&channel(30, send | perm::MANAGE_MESSAGES)), false), 0);
        assert_eq!(interval_for(Some(&channel(0, send)), false), 0);
        assert_eq!(interval_for(None, false), 0);
    }

    #[gpui::test]
    fn our_echo_starts_the_window_and_a_refusal_corrects_it(cx: &mut TestAppContext) {
        let slow = cx.new(|_| Slowmode::default());
        let key: SliceKey = (ServerId::from_central(1), "slow-lane".into());
        let t0 = cx.executor().now();
        slow.update(cx, |s, cx| {
            assert!(!s.holds(&key, 30, &[], 7, t0));
            s.note_own_message(key.clone(), 30, t0, cx);
            assert!(s.holds(&key, 30, &[], 7, t0 + Duration::from_secs(29)));
            assert_eq!(s.remaining(&key, 30, t0 + Duration::from_millis(500)), 30);
            assert_eq!(s.remaining(&key, 30, t0 + Duration::from_millis(29_001)), 1);
            assert!(!s.holds(&key, 30, &[], 7, t0 + Duration::from_secs(30)));
            // A shorter window never replaces a longer one; a refusal does.
            s.note_own_message(key.clone(), 10, t0, cx);
            assert_eq!(s.until(&key), Some(t0 + Duration::from_secs(30)));
            s.note_refusal(key.clone(), 5, t0, cx);
            assert_eq!(s.remaining(&key, 30, t0), 5);
            // Bypassed: never held.
            assert!(!s.holds(&key, 0, &[], 7, t0));
            // A send of ours still on its way holds the next one.
            let rows = vec![stored(1, 7, "a", 0), bubble("n", 7, "b", 0, 1)];
            assert!(s.holds(&key, 30, &rows, 7, t0 + Duration::from_secs(60)));
            assert!(!s.holds(&key, 30, &rows[..1], 7, t0 + Duration::from_secs(60)));
        });
    }
}
