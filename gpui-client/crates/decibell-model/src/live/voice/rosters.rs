//! Who is in every voice channel of every connected community, and our own moves shown
//! before the server confirms them (`voiceStore`'s `channelPresence`, `channelUserStates`,
//! `selfMoves`, `userCapabilities`).
//!
//! The community sends these to all members (a snapshot at authentication, then every
//! change), so they are community state, not voice-session state: leaving voice keeps
//! them, and the sidebar shows them whether or not we are in voice.
//!
//! **Our own joins and leaves** (`SelfMove`). The rosters are the server's, which lists
//! us in a channel only once the core has started the new engine and the server's
//! broadcast is back. Per channel we queue what we asked for and take an entry off the
//! front when a roster shows it; the channel shows the last one asked for meanwhile. A
//! roster that doesn't match the front was sent before the server got to our request
//! (someone else joined, a mute): it is corrected, not taken as an answer. An entry
//! nothing answers expires after [`SELF_MOVE_TIMEOUT`] and the server's roster is shown
//! as is. The expiry timer exists only while a move is pending.
//!
//! **Rhythm**: every join / leave / mute in any voice channel of any community. One
//! entity with a keyed event ([`VoiceRosterEvent::Changed`]): the channel list pane
//! subscribes and notifies only for its community's channels; the voice stage for the
//! connected channel.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use decibell_core::events::{ClientCapabilitiesPayload, VoiceUserStatePayload};
use gpui::{Context, EventEmitter, Task};

use crate::ids::{ChannelId, ServerId, Uid};

/// A voice channel: `(community, channel)`. Channel ids are slugs that two communities
/// routinely share, so every lookup goes through the community too.
pub type VoiceKey = (ServerId, ChannelId);

/// How long one of our own moves waits for the server's roster to show it.
pub const SELF_MOVE_TIMEOUT: Duration = Duration::from_millis(10_000);
/// Slack after the earliest deadline before the expiry timer fires (the renderer's 20 ms).
const EXPIRY_SLACK: Duration = Duration::from_millis(20);

/// One member's voice flags in a channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceUserState {
    pub muted: bool,
    pub deafened: bool,
    /// Moderator-applied (persisted on the member).
    pub server_muted: bool,
    pub server_deafened: bool,
    /// No SPEAK in this channel: the relay drops their voice (the "No speak" badge).
    pub suppressed: bool,
}

impl From<&VoiceUserStatePayload> for VoiceUserState {
    fn from(s: &VoiceUserStatePayload) -> Self {
        VoiceUserState {
            muted: s.is_muted,
            deafened: s.is_deafened,
            server_muted: s.is_server_muted,
            server_deafened: s.is_server_deafened,
            suppressed: s.is_suppressed,
        }
    }
}

/// A codec a client can encode or decode (`CodecCapability`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecLimit {
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

/// What a participant advertised (`ClientCapabilities`): drives watch gating and the
/// codec badge.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize)]
pub struct ClientCapabilities {
    pub encode: Vec<CodecLimit>,
    pub decode: Vec<CodecLimit>,
}

impl From<&ClientCapabilitiesPayload> for ClientCapabilities {
    fn from(c: &ClientCapabilitiesPayload) -> Self {
        let limit = |l: &decibell_core::events::CodecCapabilityPayload| CodecLimit {
            codec: l.codec,
            max_width: l.max_width,
            max_height: l.max_height,
            max_fps: l.max_fps,
        };
        ClientCapabilities { encode: c.encode.iter().map(limit).collect(), decode: c.decode.iter().map(limit).collect() }
    }
}

/// One channel's roster as shown (corrected for our own pending moves) and its members'
/// flags.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ChannelVoice {
    pub roster: Vec<Uid>,
    pub states: HashMap<Uid, VoiceUserState>,
}

impl ChannelVoice {
    pub fn state(&self, uid: Uid) -> VoiceUserState {
        self.states.get(&uid).copied().unwrap_or_default()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Placement {
    In,
    Out,
}

#[derive(Debug, Clone)]
struct SelfMove {
    /// What we asked the server for in this channel, oldest first.
    pending: Vec<Placement>,
    /// The server's latest roster for the channel, uncorrected.
    raw: Vec<Uid>,
    /// Our row while we're shown in the channel ahead of the server.
    self_state: VoiceUserState,
    /// Shown as the server has it from then on.
    until: Instant,
}

/// The roster a channel shows while `m` is pending.
fn corrected(m: &SelfMove, me: Uid) -> Vec<Uid> {
    let want = *m.pending.last().expect("a move has a pending entry");
    let listed = m.raw.contains(&me);
    match want {
        Placement::In if listed => m.raw.clone(),
        Placement::In => {
            let mut roster = m.raw.clone();
            roster.push(me);
            roster
        }
        Placement::Out => m.raw.iter().copied().filter(|u| *u != me).collect(),
    }
}

/// The channels whose roster (or flags) changed, in one batch.
#[derive(Debug, Clone, PartialEq)]
pub enum VoiceRosterEvent {
    Changed(Vec<VoiceKey>),
}

#[derive(Default)]
pub struct VoiceRosters {
    channels: HashMap<VoiceKey, ChannelVoice>,
    self_moves: HashMap<VoiceKey, SelfMove>,
    /// uid → what they advertised (`userCapabilities`); refilled by every presence that
    /// carries them, cleared when we leave voice.
    capabilities: HashMap<Uid, ClientCapabilities>,
    /// Fires at the earliest pending move's deadline; only while one is pending.
    expiry: Option<Task<()>>,
}

impl EventEmitter<VoiceRosterEvent> for VoiceRosters {}

impl VoiceRosters {
    pub fn channel(&self, key: &VoiceKey) -> Option<&ChannelVoice> {
        self.channels.get(key)
    }

    /// The roster shown for a channel (empty when nobody is there).
    pub fn roster(&self, key: &VoiceKey) -> &[Uid] {
        self.channels.get(key).map_or(&[], |c| &c.roster)
    }

    pub fn capabilities(&self, uid: Uid) -> Option<&ClientCapabilities> {
        self.capabilities.get(&uid)
    }

    /// Every channel with a roster entry (the headless dump, tests).
    pub fn keys(&self) -> impl Iterator<Item = &VoiceKey> {
        self.channels.keys()
    }

    /// The voice channel of `server` where `uid` is shown, if any.
    pub fn channel_of(&self, server: ServerId, uid: Uid) -> Option<&ChannelId> {
        self.channels
            .iter()
            .find(|((s, _), c)| *s == server && c.roster.contains(&uid))
            .map(|((_, ch), _)| ch)
    }

    pub fn has_pending_moves(&self) -> bool {
        !self.self_moves.is_empty()
    }

    fn changed(&self, keys: Vec<VoiceKey>, cx: &mut Context<Self>) {
        if !keys.is_empty() {
            cx.emit(VoiceRosterEvent::Changed(keys));
            cx.notify();
        }
    }

    // ── The server's rosters ──

    /// A `voice_presence_updated`: the channel's full roster with each member's flags
    /// and (parallel to `uids`) their capabilities. Corrected for our own moves still
    /// pending there. Returns what the channel now shows.
    pub fn set_presence(
        &mut self,
        key: VoiceKey,
        uids: Vec<Uid>,
        states: &[VoiceUserStatePayload],
        capabilities: &[ClientCapabilitiesPayload],
        me: Uid,
        cx: &mut Context<Self>,
    ) -> &ChannelVoice {
        self.apply_presence(key.clone(), uids, states, capabilities, me);
        self.reschedule(cx);
        self.changed(vec![key.clone()], cx);
        &self.channels[&key]
    }

    fn apply_presence(
        &mut self,
        key: VoiceKey,
        uids: Vec<Uid>,
        states: &[VoiceUserStatePayload],
        capabilities: &[ClientCapabilitiesPayload],
        me: Uid,
    ) {
        let mut state_map: HashMap<Uid, VoiceUserState> =
            states.iter().map(|s| (s.uid, VoiceUserState::from(s))).collect();
        if capabilities.len() == uids.len() {
            for (uid, caps) in uids.iter().zip(capabilities) {
                self.capabilities.insert(*uid, ClientCapabilities::from(caps));
            }
        }
        let mut roster = uids.clone();
        if me > 0
            && let Some(prev) = self.self_moves.get_mut(&key)
        {
            let shows = if uids.contains(&me) { Placement::In } else { Placement::Out };
            if prev.pending.first() == Some(&shows) {
                prev.pending.remove(0);
            }
            if prev.pending.is_empty() {
                self.self_moves.remove(&key);
            } else {
                prev.raw = uids;
                roster = corrected(prev, me);
                if roster.contains(&me) {
                    state_map.entry(me).or_insert(prev.self_state);
                }
            }
        }
        self.channels.insert(key, ChannelVoice { roster, states: state_map });
    }

    // ── Our own moves ──

    /// Queue `placement` for `key`. Returns false when the server's roster already
    /// shows it and nothing is pending (leaving a channel a moderator already took us
    /// out of).
    fn queue(&mut self, key: &VoiceKey, placement: Placement, me: Uid, self_state: VoiceUserState, now: Instant) -> bool {
        let prev = self.self_moves.get(key);
        let raw = match prev {
            Some(m) => m.raw.clone(),
            None => self.channels.get(key).map(|c| c.roster.clone()).unwrap_or_default(),
        };
        if prev.is_none() && raw.contains(&me) == (placement == Placement::In) {
            return false;
        }
        let mut pending = prev.map(|m| m.pending.clone()).unwrap_or_default();
        pending.push(placement);
        let m = SelfMove { pending, raw, self_state, until: now + SELF_MOVE_TIMEOUT };
        let roster = corrected(&m, me);
        let channel = self.channels.entry(key.clone()).or_default();
        channel.roster = roster;
        if placement == Placement::In {
            channel.states.entry(me).or_insert(self_state);
        }
        self.self_moves.insert(key.clone(), m);
        true
    }

    /// Our own join or switch (`enterChannel` with `optimistic`): out of `from`, into
    /// `to`, before the server's broadcasts say so. `self_state` is our row meanwhile.
    pub fn queue_switch(
        &mut self,
        from: Option<&VoiceKey>,
        to: &VoiceKey,
        me: Uid,
        self_state: VoiceUserState,
        now: Instant,
        cx: &mut Context<Self>,
    ) {
        let mut keys = Vec::new();
        if let Some(from) = from
            && self.queue(from, Placement::Out, me, self_state, now)
        {
            keys.push(from.clone());
        }
        if self.queue(to, Placement::In, me, self_state, now) {
            keys.push(to.clone());
        }
        self.reschedule(cx);
        self.changed(keys, cx);
    }

    /// Our own leave (or a join that failed): out of `key` now. `self_state` is the row
    /// a pending move there already had, else ours.
    pub fn queue_leave(&mut self, key: &VoiceKey, me: Uid, self_state: VoiceUserState, now: Instant, cx: &mut Context<Self>) {
        let state = self.self_moves.get(key).map_or(self_state, |m| m.self_state);
        if self.queue(key, Placement::Out, me, state, now) {
            self.reschedule(cx);
            self.changed(vec![key.clone()], cx);
        }
    }

    /// The flags a pending move shows for us in `key`, else the server's.
    pub fn self_state(&self, key: &VoiceKey, me: Uid) -> Option<VoiceUserState> {
        self.self_moves
            .get(key)
            .map(|m| m.self_state)
            .or_else(|| self.channels.get(key).and_then(|c| c.states.get(&me).copied()))
    }

    /// Show the server's roster for every channel whose pending moves timed out.
    pub fn expire(&mut self, now: Instant, cx: &mut Context<Self>) {
        let expired: Vec<VoiceKey> =
            self.self_moves.iter().filter(|(_, m)| m.until <= now).map(|(k, _)| k.clone()).collect();
        for key in &expired {
            if let Some(m) = self.self_moves.remove(key) {
                self.channels.entry(key.clone()).or_default().roster = m.raw;
            }
        }
        self.reschedule(cx);
        self.changed(expired, cx);
    }

    /// Arm the expiry timer for the earliest pending deadline, or drop it.
    fn reschedule(&mut self, cx: &mut Context<Self>) {
        let Some(earliest) = self.self_moves.values().map(|m| m.until).min() else {
            self.expiry = None;
            return;
        };
        let wait = earliest.saturating_duration_since(cx.background_executor().now()) + EXPIRY_SLACK;
        self.expiry = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait).await;
            let _ = this.update(cx, |this, cx| {
                let now = cx.background_executor().now();
                this.expire(now, cx);
            });
        }));
    }

    // ── Clearing ──

    /// Drop one community's rosters and pending moves (`clearServerPresence`): all of
    /// them (`keep` none: a (re)authentication's snapshot follows, a dropped connection,
    /// a revoked membership), or only the channels not in `keep` (hidden or deleted).
    pub fn clear_server(&mut self, server: ServerId, keep: Option<&HashSet<ChannelId>>, cx: &mut Context<Self>) {
        let drop = |key: &VoiceKey| key.0 == server && !keep.is_some_and(|k| k.contains(&key.1));
        let keys: Vec<VoiceKey> = self.channels.keys().filter(|k| drop(k)).cloned().collect();
        for key in &keys {
            self.channels.remove(key);
        }
        let before = self.self_moves.len();
        self.self_moves.retain(|k, _| !drop(k));
        let moves_dropped = self.self_moves.len() != before;
        if moves_dropped {
            self.reschedule(cx);
        }
        if !keys.is_empty() || moves_dropped {
            self.changed(keys, cx);
        }
    }

    /// Leaving voice: the capabilities are refilled by the next join's presence.
    pub fn clear_capabilities(&mut self) {
        self.capabilities.clear();
    }

    /// Sign-out: nothing carries over.
    pub fn reset(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<VoiceKey> = self.channels.keys().cloned().collect();
        self.channels.clear();
        self.self_moves.clear();
        self.capabilities.clear();
        self.expiry = None;
        self.changed(keys, cx);
    }
}

#[cfg(test)]
mod tests {
    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    const ME: Uid = 7;

    fn key(channel: &str) -> VoiceKey {
        (ServerId::from_central(1), channel.into())
    }

    fn st(uid: Uid, muted: bool) -> VoiceUserStatePayload {
        VoiceUserStatePayload {
            username: format!("u{uid}"),
            uid,
            is_muted: muted,
            is_deafened: false,
            is_server_muted: false,
            is_server_deafened: false,
            is_suppressed: false,
        }
    }

    fn presence(r: &gpui::Entity<VoiceRosters>, channel: &str, uids: &[Uid], cx: &mut TestAppContext) {
        let states: Vec<_> = uids.iter().map(|u| st(*u, false)).collect();
        r.update(cx, |r, cx| {
            r.set_presence(key(channel), uids.to_vec(), &states, &[], ME, cx);
        });
    }

    fn shown(r: &gpui::Entity<VoiceRosters>, channel: &str, cx: &mut TestAppContext) -> Vec<Uid> {
        r.read_with(cx, |r, _| r.roster(&key(channel)).to_vec())
    }

    fn now(cx: &TestAppContext) -> Instant {
        cx.executor().now()
    }

    /// A switch shows us in the new channel and out of the old one at once; each
    /// channel's confirming roster takes its entry off, an unrelated roster sent before
    /// the server got to us is corrected, not taken as the answer.
    #[gpui::test]
    fn a_switch_is_shown_at_once_and_confirmed_per_channel(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        presence(&r, "lounge", &[1, ME], cx);
        presence(&r, "squad", &[2], cx);
        let t = now(cx);
        r.update(cx, |r, cx| r.queue_switch(Some(&key("lounge")), &key("squad"), ME, VoiceUserState::default(), t, cx));
        assert_eq!(shown(&r, "lounge", cx), vec![1]);
        assert_eq!(shown(&r, "squad", cx), vec![2, ME]);
        assert!(r.read_with(cx, |r, _| r.has_pending_moves()));

        // Someone mutes in squad before the server handled our JOIN: still corrected.
        presence(&r, "squad", &[2], cx);
        assert_eq!(shown(&r, "squad", cx), vec![2, ME]);
        // The server's answers, per channel.
        presence(&r, "lounge", &[1], cx);
        presence(&r, "squad", &[2, ME], cx);
        assert_eq!(shown(&r, "lounge", cx), vec![1]);
        assert_eq!(shown(&r, "squad", cx), vec![2, ME]);
        assert!(!r.read_with(cx, |r, _| r.has_pending_moves()), "both confirmed");
        // From here on the server's rosters are shown as they are.
        presence(&r, "squad", &[2], cx);
        assert_eq!(shown(&r, "squad", cx), vec![2]);
    }

    /// Join A, then B before A's confirmation: A queues in → out, B in.
    #[gpui::test]
    fn moves_queue_in_order(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        let t = now(cx);
        r.update(cx, |r, cx| r.queue_switch(None, &key("a"), ME, VoiceUserState::default(), t, cx));
        r.update(cx, |r, cx| r.queue_switch(Some(&key("a")), &key("b"), ME, VoiceUserState::default(), t, cx));
        assert_eq!(shown(&r, "a", cx), Vec::<Uid>::new(), "the last asked for (out) shows");
        assert_eq!(shown(&r, "b", cx), vec![ME]);
        // The server lists us in A (answer to the first entry): still shown out.
        presence(&r, "a", &[ME], cx);
        assert_eq!(shown(&r, "a", cx), Vec::<Uid>::new());
        // Then out of A: the queue empties.
        presence(&r, "a", &[], cx);
        presence(&r, "b", &[ME], cx);
        assert!(!r.read_with(cx, |r, _| r.has_pending_moves()));
    }

    /// A move nothing answers expires: the server's roster is shown as is after 10 s,
    /// and the timer goes with the last pending move.
    #[gpui::test]
    fn an_unanswered_move_expires(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        presence(&r, "lounge", &[1], cx);
        let t = now(cx);
        r.update(cx, |r, cx| r.queue_switch(None, &key("lounge"), ME, VoiceUserState { muted: true, ..Default::default() }, t, cx));
        assert_eq!(shown(&r, "lounge", cx), vec![1, ME]);
        // Our own row carries the flags we joined with.
        assert!(r.read_with(cx, |r, _| r.channel(&key("lounge")).unwrap().state(ME).muted));
        cx.executor().advance_clock(SELF_MOVE_TIMEOUT - Duration::from_millis(1));
        cx.run_until_parked();
        assert_eq!(shown(&r, "lounge", cx), vec![1, ME]);
        cx.executor().advance_clock(Duration::from_millis(50));
        cx.run_until_parked();
        assert_eq!(shown(&r, "lounge", cx), vec![1], "the server's roster");
        r.read_with(cx, |r, _| assert!(!r.has_pending_moves() && r.expiry.is_none(), "no timer at rest"));
    }

    /// A leave the server already made (a moderator took us out) queues nothing.
    #[gpui::test]
    fn leaving_where_the_server_already_has_us_out_is_a_no_op(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        presence(&r, "lounge", &[1], cx);
        let t = now(cx);
        r.update(cx, |r, cx| r.queue_leave(&key("lounge"), ME, VoiceUserState::default(), t, cx));
        r.read_with(cx, |r, _| assert!(!r.has_pending_moves() && r.expiry.is_none()));
    }

    /// A community's (re)authentication drops its rosters and pending moves; a channel
    /// list keeps the channels still listed. Other communities are untouched.
    #[gpui::test]
    fn clearing_a_community(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        presence(&r, "lounge", &[1], cx);
        presence(&r, "squad", &[2], cx);
        let other = (ServerId::from_central(2), ChannelId::from("lounge"));
        r.update(cx, |r, cx| {
            r.set_presence(other.clone(), vec![3], &[st(3, false)], &[], ME, cx);
        });
        let keep: HashSet<ChannelId> = ["squad".into()].into();
        r.update(cx, |r, cx| r.clear_server(ServerId::from_central(1), Some(&keep), cx));
        assert_eq!(shown(&r, "lounge", cx), Vec::<Uid>::new());
        assert_eq!(shown(&r, "squad", cx), vec![2]);
        r.update(cx, |r, cx| r.clear_server(ServerId::from_central(1), None, cx));
        assert_eq!(shown(&r, "squad", cx), Vec::<Uid>::new());
        r.read_with(cx, |r, _| assert_eq!(r.roster(&other), &[3]));
    }

    /// Capabilities are taken only when they line up with the roster.
    #[gpui::test]
    fn capabilities_follow_the_roster(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        let caps = |codec| ClientCapabilitiesPayload {
            encode: vec![],
            decode: vec![decibell_core::events::CodecCapabilityPayload { codec, max_width: 1, max_height: 1, max_fps: 1 }],
        };
        r.update(cx, |r, cx| {
            r.set_presence(key("lounge"), vec![1, 2], &[], &[caps(4), caps(1)], ME, cx);
            r.set_presence(key("lounge"), vec![1, 2], &[], &[caps(3)], ME, cx);
        });
        r.read_with(cx, |r, _| {
            assert_eq!(r.capabilities(1).unwrap().decode[0].codec, 4, "a mismatched list is ignored");
            assert_eq!(r.capabilities(2).unwrap().decode[0].codec, 1);
        });
    }

    /// Every change is one keyed event naming the channels it touched.
    #[gpui::test]
    fn events_name_the_channels(cx: &mut TestAppContext) {
        let r = cx.new(|_| VoiceRosters::default());
        let seen = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        let _sub = cx.update(|cx| {
            let seen = seen.clone();
            cx.subscribe(&r, move |_, e: &VoiceRosterEvent, _| seen.borrow_mut().push(e.clone()))
        });
        presence(&r, "lounge", &[ME], cx);
        let t = now(cx);
        r.update(cx, |r, cx| r.queue_switch(Some(&key("lounge")), &key("squad"), ME, VoiceUserState::default(), t, cx));
        assert_eq!(
            *seen.borrow(),
            vec![
                VoiceRosterEvent::Changed(vec![key("lounge")]),
                VoiceRosterEvent::Changed(vec![key("lounge"), key("squad")]),
            ]
        );
    }
}
