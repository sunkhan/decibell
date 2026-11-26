//! User statuses and custom status messages (`stores/presenceStore.ts`; CLAUDE.md "User
//! statuses are central's", specs `2026-10-11-user-status-design.md`,
//! `2026-10-12-status-messages-design.md`).
//!
//! Central is the source of truth, by subscription: views register interest
//! (`PresenceInterest`), central answers newly watched users with a `user_list_updated`
//! and then sends a `presence_delta` per change. A watched user central answered for and
//! absent from the statuses is offline (an invisible user is absent, never "invisible"
//! on the wire). Until central answers, a community roster's own online flag stands in.
//!
//! **Why one entity with keyed events, not an entity per uid.** The interest set is
//! hundreds of users (a member list's window, avatars, friends); a change is one user.
//! Per-uid entities would mean hundreds of entities created and dropped as lists scroll,
//! each with its own observer bookkeeping. With one entity, a dot or avatar view (one
//! person) subscribes and notifies itself only when its uid is in the batch
//! ([`PresenceChanged`]); a member list (many people) observes. A `user_list_updated` for
//! 200 users is one event and one notify. Messages are a separate entity, as the
//! renderer kept them apart so an edit doesn't regroup member lists.
//!
//! **Rhythm**: [`Presence`] / [`StatusMessages`]: a watched user's change (seconds to
//! minutes apart per user). [`OwnPresence`]: our own choices and other devices' (rare);
//! the clear-after timer exists only while our message has a deadline.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use decibell_core::events::UserPresencePayload;
use gpui::{Context, EventEmitter, SharedString, Task};

use crate::ids::Uid;

/// What others can be seen as.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PresenceStatus {
    Online,
    Away,
    Dnd,
}

impl PresenceStatus {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "online" => Some(Self::Online),
            "away" => Some(Self::Away),
            "dnd" => Some(Self::Dnd),
            _ => None,
        }
    }
}

/// The local user's choice.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum OwnStatus {
    #[default]
    Online,
    Away,
    Dnd,
    Invisible,
}

impl OwnStatus {
    pub fn parse(s: &str) -> Self {
        match s {
            "away" => Self::Away,
            "dnd" => Self::Dnd,
            "invisible" => Self::Invisible,
            _ => Self::Online,
        }
    }

    pub fn as_wire(self) -> &'static str {
        match self {
            Self::Online => "online",
            Self::Away => "away",
            Self::Dnd => "dnd",
            Self::Invisible => "invisible",
        }
    }
}

/// What a status dot shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum DisplayStatus {
    Online,
    Away,
    Dnd,
    Offline,
    Invisible,
}

impl DisplayStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Online => "Online",
            Self::Away => "Away",
            Self::Dnd => "Do Not Disturb",
            Self::Offline => "Offline",
            Self::Invisible => "Invisible",
        }
    }

    /// "Can be reached right now" (calls, grouping).
    pub fn is_online(self) -> bool {
        matches!(self, Self::Online | Self::Away | Self::Dnd)
    }
}

impl From<PresenceStatus> for DisplayStatus {
    fn from(s: PresenceStatus) -> Self {
        match s {
            PresenceStatus::Online => Self::Online,
            PresenceStatus::Away => Self::Away,
            PresenceStatus::Dnd => Self::Dnd,
        }
    }
}

/// One line of text and / or one emoji.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize)]
pub struct StatusMessage {
    pub text: SharedString,
    pub emoji: SharedString,
}

impl StatusMessage {
    /// None for "no message".
    pub fn of(text: &str, emoji: &str) -> Option<Self> {
        (!text.is_empty() || !emoji.is_empty())
            .then(|| StatusMessage { text: text.to_string().into(), emoji: emoji.to_string().into() })
    }
}

pub const STATUS_TEXT_MAX: usize = 128;

/// Central's normalisation (`normalize_status_line`), so our own copy reads exactly like
/// everyone else's: control characters and line separators become spaces, bidi
/// controls are dropped, the end is trimmed, at most `max` code points.
pub fn normalize_status_text(raw: &str, max: usize) -> String {
    let mut out = String::new();
    let mut n = 0;
    for ch in raw.chars() {
        if n >= max {
            break;
        }
        let cp = ch as u32;
        if cp <= 0x20 || cp == 0x7f || (0x80..0xa0).contains(&cp) || cp == 0x2028 || cp == 0x2029 {
            if !out.is_empty() {
                out.push(' ');
                n += 1;
            }
        } else if !((0x202a..=0x202e).contains(&cp) || (0x2066..=0x2069).contains(&cp)) {
            out.push(ch);
            n += 1;
        }
    }
    out.trim_end_matches(' ').to_string()
}

/// The uids whose status (or "known") changed, in one batch.
#[derive(Debug, Clone, PartialEq)]
pub struct PresenceChanged(pub Vec<Uid>);

#[derive(Default)]
pub struct Presence {
    /// Visible online users → their effective status.
    statuses: HashMap<Uid, PresenceStatus>,
    /// Users central answered for since the last (re)connect.
    known: HashSet<Uid>,
}

impl EventEmitter<PresenceChanged> for Presence {}

impl Presence {
    pub fn status(&self, uid: Uid) -> Option<PresenceStatus> {
        self.statuses.get(&uid).copied()
    }

    pub fn is_known(&self, uid: Uid) -> bool {
        self.known.contains(&uid)
    }

    /// Another user's dot (`usePresence` for someone else): central's status, else the
    /// community's flag until central has answered, else offline.
    pub fn display(&self, uid: Uid, community_online: bool) -> DisplayStatus {
        match self.statuses.get(&uid) {
            Some(s) => (*s).into(),
            None if !self.known.contains(&uid) && community_online => DisplayStatus::Online,
            None => DisplayStatus::Offline,
        }
    }

    pub fn online_count(&self) -> usize {
        self.statuses.len()
    }

    /// Every user central answered for (the headless dump).
    pub fn known(&self) -> impl Iterator<Item = Uid> + '_ {
        self.known.iter().copied()
    }

    fn changed(&self, uids: Vec<Uid>, cx: &mut Context<Self>) {
        if !uids.is_empty() {
            cx.emit(PresenceChanged(uids));
            cx.notify();
        }
    }

    /// A subscription answer: `covered` users' current presence (those not in `users`
    /// are offline). Returns the changed uids.
    pub fn apply_covered(&mut self, covered: &[Uid], users: &[UserPresencePayload], cx: &mut Context<Self>) -> Vec<Uid> {
        let before: HashMap<Uid, Option<PresenceStatus>> = covered
            .iter()
            .chain(users.iter().map(|u| &u.uid))
            .map(|uid| (*uid, self.statuses.get(uid).copied()))
            .collect();
        let mut changed = HashSet::new();
        for uid in covered.iter().copied().filter(|u| *u > 0) {
            self.statuses.remove(&uid);
            if self.known.insert(uid) {
                changed.insert(uid);
            }
        }
        for u in users.iter().filter(|u| u.uid > 0) {
            let status = PresenceStatus::parse(&u.status).unwrap_or(PresenceStatus::Online);
            self.statuses.insert(u.uid, status);
        }
        for (uid, old) in before {
            if uid > 0 && self.statuses.get(&uid).copied() != old {
                changed.insert(uid);
            }
        }
        let mut changed: Vec<Uid> = changed.into_iter().collect();
        changed.sort_unstable();
        self.changed(changed.clone(), cx);
        changed
    }

    /// One user's full current presence (`presence_delta`). Returns whether anything
    /// changed.
    pub fn apply_delta(&mut self, uid: Uid, online: bool, status: &str, cx: &mut Context<Self>) -> bool {
        let next = online.then(|| PresenceStatus::parse(status).unwrap_or(PresenceStatus::Online));
        let status_same = self.statuses.get(&uid).copied() == next;
        let known_same = self.known.contains(&uid);
        if status_same && known_same {
            return false;
        }
        self.known.insert(uid);
        match next {
            Some(s) => self.statuses.insert(uid, s),
            None => self.statuses.remove(&uid),
        };
        self.changed(vec![uid], cx);
        true
    }

    /// Central's connection dropped: what we had is stale. Lists fall back to the
    /// communities' own flags until central answers again.
    pub fn mark_disconnected(&mut self, cx: &mut Context<Self>) {
        let mut uids: Vec<Uid> = self.known.iter().chain(self.statuses.keys()).copied().collect();
        uids.sort_unstable();
        uids.dedup();
        self.statuses.clear();
        self.known.clear();
        self.changed(uids, cx);
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MessagesChanged(pub Vec<Uid>);

/// Visible online users' custom status messages.
#[derive(Default)]
pub struct StatusMessages {
    messages: HashMap<Uid, StatusMessage>,
}

impl EventEmitter<MessagesChanged> for StatusMessages {}

impl StatusMessages {
    pub fn get(&self, uid: Uid) -> Option<&StatusMessage> {
        self.messages.get(&uid)
    }

    pub fn apply_covered(&mut self, covered: &[Uid], users: &[UserPresencePayload], cx: &mut Context<Self>) {
        let mut changed = Vec::new();
        let listed: HashMap<Uid, Option<StatusMessage>> =
            users.iter().map(|u| (u.uid, StatusMessage::of(&u.status_text, &u.status_emoji))).collect();
        for uid in covered.iter().copied().filter(|u| *u > 0) {
            let next = listed.get(&uid).cloned().flatten();
            if self.set(uid, next) {
                changed.push(uid);
            }
        }
        for u in users.iter().filter(|u| u.uid > 0 && !covered.contains(&u.uid)) {
            if self.set(u.uid, StatusMessage::of(&u.status_text, &u.status_emoji)) {
                changed.push(u.uid);
            }
        }
        if !changed.is_empty() {
            cx.emit(MessagesChanged(changed));
            cx.notify();
        }
    }

    pub fn apply_delta(&mut self, uid: Uid, online: bool, text: &str, emoji: &str, cx: &mut Context<Self>) {
        let next = if online { StatusMessage::of(text, emoji) } else { None };
        if self.set(uid, next) {
            cx.emit(MessagesChanged(vec![uid]));
            cx.notify();
        }
    }

    fn set(&mut self, uid: Uid, next: Option<StatusMessage>) -> bool {
        if self.messages.get(&uid) == next.as_ref() {
            return false;
        }
        match next {
            Some(m) => self.messages.insert(uid, m),
            None => self.messages.remove(&uid),
        };
        true
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.messages.is_empty() {
            let uids = self.messages.drain().map(|(u, _)| u).collect();
            cx.emit(MessagesChanged(uids));
            cx.notify();
        }
    }
}

/// Our own presence (the sign-in reply and `own_presence`, any device's change).
#[derive(Default)]
pub struct OwnPresence {
    /// This central understands statuses; false hides the status menu.
    pub supported: bool,
    pub status: OwnStatus,
    /// Auto-away reports no input on this device (what we tell central).
    pub idle: bool,
    /// Every device of the account is idle: others then see Online as Away, and so do we.
    pub account_idle: bool,
    pub message: Option<StatusMessage>,
    /// When central clears it; none = never.
    pub clears_at: Option<Instant>,
    /// Hides our message at `clears_at` (central clears it for everyone else on its own
    /// clock). Only while a deadline is set.
    clear_timer: Option<Task<()>>,
}

impl OwnPresence {
    /// The local user as others see them (Away while every device idles on Online),
    /// except that Invisible shows as itself rather than offline.
    pub fn display(&self) -> DisplayStatus {
        match self.status {
            OwnStatus::Online if self.account_idle => DisplayStatus::Away,
            OwnStatus::Online => DisplayStatus::Online,
            OwnStatus::Away => DisplayStatus::Away,
            OwnStatus::Dnd => DisplayStatus::Dnd,
            OwnStatus::Invisible => DisplayStatus::Invisible,
        }
    }

    /// The account's own state, from a sign-in or another device (`setOwnFromLogin`). The
    /// same message again (an echo of our own change, another device saving what we
    /// show) keeps the timer unless the deadline moved by 2 s or more.
    #[allow(clippy::too_many_arguments)] // the payload's fields, flat as the renderer's call
    pub fn apply_account(
        &mut self,
        supported: bool,
        status: &str,
        message: Option<StatusMessage>,
        clears_in: Duration,
        account_idle: bool,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let status = OwnStatus::parse(status);
        if (self.supported, self.status, self.account_idle) != (supported, status, account_idle) {
            self.supported = supported;
            self.status = status;
            self.account_idle = account_idle;
            cx.notify();
        }
        let clears_at = (message.is_some() && !clears_in.is_zero()).then(|| now + clears_in);
        let same_deadline = match (self.clears_at, clears_at) {
            (None, None) => true,
            (Some(a), Some(b)) => {
                let d = if a > b { a - b } else { b - a };
                d < Duration::from_secs(2)
            }
            _ => false,
        };
        if self.message != message || !same_deadline {
            self.set_message(message, clears_at, cx);
        }
    }

    pub fn set_status(&mut self, status: OwnStatus, cx: &mut Context<Self>) {
        if self.status != status {
            self.status = status;
            cx.notify();
        }
    }

    /// Our message and when it clears; the timer goes with the deadline.
    pub fn set_message(&mut self, message: Option<StatusMessage>, clears_at: Option<Instant>, cx: &mut Context<Self>) {
        let clears_at = if message.is_some() { clears_at } else { None };
        self.clear_timer = clears_at.map(|at| {
            let wait = at.saturating_duration_since(cx.background_executor().now());
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(wait).await;
                let _ = this.update(cx, |this, cx| {
                    this.clear_timer = None;
                    this.message = None;
                    this.clears_at = None;
                    cx.notify();
                });
            })
        });
        self.message = message;
        self.clears_at = clears_at;
        cx.notify();
    }

    pub fn clear_pending(&self) -> bool {
        self.clear_timer.is_some()
    }

    pub fn set_idle(&mut self, idle: bool, cx: &mut Context<Self>) -> bool {
        if self.idle == idle {
            return false;
        }
        self.idle = idle;
        cx.notify();
        true
    }

    /// Sign-out (a different account may sign in next).
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        *self = OwnPresence::default();
        cx.notify();
    }
}

/// How long a status message stays (the custom-status modal's "Clear after").
pub mod clear_after {
    /// Seconds; 0 = don't clear. "Today" is computed by the modal (until local midnight).
    pub const THIRTY_MINUTES: u32 = 30 * 60;
    pub const ONE_HOUR: u32 = 60 * 60;
    pub const FOUR_HOURS: u32 = 4 * 60 * 60;
    pub const NEVER: u32 = 0;
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    fn user(uid: Uid, status: &str, text: &str) -> UserPresencePayload {
        UserPresencePayload {
            username: format!("u{uid}"),
            uid,
            avatar_version: String::new(),
            status: status.into(),
            status_text: text.into(),
            status_emoji: String::new(),
        }
    }

    #[gpui::test]
    fn covered_users_and_deltas(cx: &mut TestAppContext) {
        let p = cx.new(|_| Presence::default());
        // Before central answers, the community's flag stands in.
        p.read_with(cx, |p, _| assert_eq!(p.display(2, true), DisplayStatus::Online));
        let changed = p.update(cx, |p, cx| p.apply_covered(&[2, 3, 4], &[user(2, "dnd", ""), user(3, "bogus", "")], cx));
        assert_eq!(changed, vec![2, 3, 4]);
        p.read_with(cx, |p, _| {
            assert_eq!(p.display(2, false), DisplayStatus::Dnd);
            assert_eq!(p.display(3, false), DisplayStatus::Online, "an unknown status reads as online");
            assert_eq!(p.display(4, true), DisplayStatus::Offline, "answered: absent is offline, whatever the community says");
        });
        // The same delta twice is one change.
        assert!(p.update(cx, |p, cx| p.apply_delta(4, true, "away", cx)));
        assert!(!p.update(cx, |p, cx| p.apply_delta(4, true, "away", cx)));
        // Invisible looks offline: central sends "offline", never "invisible".
        assert!(p.update(cx, |p, cx| p.apply_delta(4, false, "", cx)));
        p.read_with(cx, |p, _| assert_eq!(p.display(4, true), DisplayStatus::Offline));
        // Central dropped: back to the communities' flags.
        p.update(cx, |p, cx| p.mark_disconnected(cx));
        p.read_with(cx, |p, _| assert_eq!(p.display(2, true), DisplayStatus::Online));
    }

    #[gpui::test]
    fn messages_ride_presence(cx: &mut TestAppContext) {
        let m = cx.new(|_| StatusMessages::default());
        m.update(cx, |m, cx| m.apply_covered(&[2, 3], &[user(2, "online", "gaming")], cx));
        m.read_with(cx, |m, _| {
            assert_eq!(m.get(2).unwrap().text.as_ref(), "gaming");
            assert!(m.get(3).is_none());
        });
        m.update(cx, |m, cx| m.apply_delta(2, false, "gaming", "", cx));
        m.read_with(cx, |m, _| assert!(m.get(2).is_none(), "an offline user shows no message"));
    }

    /// Our own message clears itself at its deadline; an echo of the same message keeps
    /// the timer, a moved deadline re-arms it.
    #[gpui::test]
    fn own_message_clear_after(cx: &mut TestAppContext) {
        let own = cx.new(|_| OwnPresence::default());
        let msg = StatusMessage::of("brb", "☕");
        let t0 = cx.executor().now();
        own.update(cx, |o, cx| o.apply_account(true, "online", msg.clone(), Duration::from_secs(1800), false, t0, cx));
        own.read_with(cx, |o, _| {
            assert!(o.clear_pending());
            assert_eq!(o.display(), DisplayStatus::Online);
        });
        cx.executor().advance_clock(Duration::from_secs(600));
        // The echo, 600 s later, says "1200 s left": the same deadline.
        let t1 = cx.executor().now();
        own.update(cx, |o, cx| o.apply_account(true, "online", msg.clone(), Duration::from_secs(1200), true, t1, cx));
        own.read_with(cx, |o, _| {
            assert_eq!(o.clears_at, Some(t0 + Duration::from_secs(1800)));
            assert_eq!(o.display(), DisplayStatus::Away, "every device idle: Away");
        });
        cx.executor().advance_clock(Duration::from_secs(1199));
        cx.run_until_parked();
        own.read_with(cx, |o, _| assert!(o.message.is_some()));
        cx.executor().advance_clock(Duration::from_secs(2));
        cx.run_until_parked();
        own.read_with(cx, |o, _| {
            assert!(o.message.is_none() && o.clears_at.is_none());
            assert!(!o.clear_pending(), "no timer at rest");
        });
        // A message with no deadline holds no timer.
        own.update(cx, |o, cx| o.set_message(StatusMessage::of("x", ""), None, cx));
        own.read_with(cx, |o, _| assert!(!o.clear_pending()));
        own.update(cx, |o, cx| o.apply_account(true, "invisible", None, Duration::ZERO, false, t1, cx));
        own.read_with(cx, |o, _| {
            assert_eq!(o.display(), DisplayStatus::Invisible);
            assert!(o.message.is_none());
        });
    }

    #[test]
    fn status_text_is_normalised_like_central() {
        assert_eq!(normalize_status_text("  hi\nthere\u{202e}!  ", 128), "hi there!");
        assert_eq!(normalize_status_text("\u{2028}x", 128), "x");
        assert_eq!(normalize_status_text("abcdef", 3), "abc");
        assert_eq!(normalize_status_text("éé😀", 2), "éé");
    }
}
