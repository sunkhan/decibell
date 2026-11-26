//! Our voice session (`voiceStore`'s connection half): which channel (or DM call) the one
//! voice engine serves, our own mute / deafen, the channel's MLS state, the engine's last
//! error. And the session's participant list (`participants`), its own entity because it
//! changes with every roster change of the connected channel while the session changes
//! only on our own actions.
//!
//! **Rhythm**: [`VoiceSession`] on join / leave / switch / mute / deafen / MLS state
//! (user actions and rare pushes): the user panel, the dock, the call bar observe it.
//! [`Participants`] on every roster change in the connected channel and every
//! participant mute flip: the voice stage observes it.

use std::collections::HashSet;

use decibell_core::events::VoiceE2eeStatePayload;
use gpui::{Context, SharedString};

use super::rosters::{VoiceKey, VoiceUserState};
use crate::ids::{ChannelId, ServerId, Uid};

/// The MLS group behind the connected channel (`voice_e2ee_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum VoiceE2eePhase {
    Joining,
    Ready,
    Resyncing,
    /// A member's leaf didn't verify against central: nothing is sent until they leave.
    Quarantine,
    Failed,
    /// A state this build doesn't know.
    Other,
}

impl VoiceE2eePhase {
    pub fn parse(s: &str) -> Self {
        match s {
            "joining" => Self::Joining,
            "ready" => Self::Ready,
            "resyncing" => Self::Resyncing,
            "quarantine" => Self::Quarantine,
            "failed" => Self::Failed,
            _ => Self::Other,
        }
    }
}

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct VoiceE2ee {
    pub phase: VoiceE2eePhase,
    pub epoch: u64,
    pub members: u32,
    /// Members whose leaf identity did not verify (uids).
    pub unverified: Vec<Uid>,
}

impl From<&VoiceE2eeStatePayload> for VoiceE2ee {
    fn from(p: &VoiceE2eeStatePayload) -> Self {
        VoiceE2ee {
            phase: VoiceE2eePhase::parse(&p.state),
            epoch: p.epoch,
            members: p.members,
            unverified: p.unverified.iter().filter_map(|u| u.parse().ok()).collect(),
        }
    }
}

#[derive(Default)]
pub struct VoiceSession {
    pub server: Option<ServerId>,
    pub channel: Option<ChannelId>,
    /// A P2P DM call is the session (the same engine, no channel).
    pub call_peer: Option<Uid>,
    /// The connected channel's MLS state (none in a call or out of voice).
    pub e2ee: Option<VoiceE2ee>,
    pub muted: bool,
    pub deafened: bool,
    pub error: Option<SharedString>,
    /// Who the join / leave sounds compare against (`prevParticipants`), re-seeded
    /// whenever the connected channel changes.
    pub(crate) prev_participants: Option<HashSet<Uid>>,
    /// Same for the stream start / stop sounds (`prevStreamOwners`).
    pub(crate) prev_stream_owners: Option<HashSet<Uid>>,
}

impl VoiceSession {
    /// The connected community channel.
    pub fn connected(&self) -> Option<VoiceKey> {
        Some((self.server?, self.channel.clone()?))
    }

    pub fn is_connected_to(&self, server: ServerId, channel: &str) -> bool {
        self.server == Some(server) && self.channel.as_deref() == Some(channel)
    }

    /// A voice engine exists: a community channel or a DM call (`mediaSessionActive`).
    pub fn media_session_active(&self) -> bool {
        self.channel.is_some() || self.call_peer.is_some()
    }

    /// Our engine's mute / deafen (`voice_state_changed`). Deafening mutes too.
    pub fn set_self_state(&mut self, muted: bool, deafened: bool, cx: &mut Context<Self>) {
        let muted = muted || deafened;
        if (self.muted, self.deafened) != (muted, deafened) {
            self.muted = muted;
            self.deafened = deafened;
            cx.notify();
        }
    }

    pub fn set_error(&mut self, error: Option<SharedString>, cx: &mut Context<Self>) {
        if self.error != error {
            self.error = error;
            cx.notify();
        }
    }

    pub fn set_e2ee(&mut self, e2ee: Option<VoiceE2ee>, cx: &mut Context<Self>) {
        if self.e2ee != e2ee {
            self.e2ee = e2ee;
            cx.notify();
        }
    }

    /// Make `key` the connected channel (a join, a switch, a moderator's move). A real
    /// change drops the old channel's MLS state.
    pub(crate) fn enter(&mut self, key: &VoiceKey, cx: &mut Context<Self>) -> bool {
        let changed = self.connected().as_ref() != Some(key);
        self.server = Some(key.0);
        self.channel = Some(key.1.clone());
        if changed {
            self.e2ee = None;
        }
        cx.notify();
        changed
    }

    /// A DM call's media came up: the session runs with no channel.
    pub(crate) fn enter_call(&mut self, peer: Uid, cx: &mut Context<Self>) {
        self.server = None;
        self.channel = None;
        self.call_peer = Some(peer);
        self.e2ee = None;
        self.prev_participants = None;
        self.prev_stream_owners = None;
        cx.notify();
    }

    /// Leaving (`disconnect`): the session state goes; mute / deafen stay (the core keeps
    /// them for the next session).
    pub(crate) fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.server = None;
        self.channel = None;
        self.call_peer = None;
        self.e2ee = None;
        self.error = None;
        self.prev_participants = None;
        self.prev_stream_owners = None;
        cx.notify();
    }

    pub(crate) fn reset(&mut self, cx: &mut Context<Self>) {
        *self = VoiceSession::default();
        cx.notify();
    }
}

/// One row of the voice stage.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Participant {
    pub uid: Uid,
    /// The name the payload carried (the fallback; views draw `Model::name_of`).
    pub name: SharedString,
    pub state: VoiceUserState,
}

#[derive(Default)]
pub struct Participants {
    list: Vec<Participant>,
}

impl Participants {
    pub fn list(&self) -> &[Participant] {
        &self.list
    }

    pub fn uids(&self) -> impl Iterator<Item = Uid> + '_ {
        self.list.iter().map(|p| p.uid)
    }

    pub fn set(&mut self, list: Vec<Participant>, cx: &mut Context<Self>) {
        if self.list != list {
            self.list = list;
            cx.notify();
        }
    }

    /// A participant's own mute / deafen (`voice_user_state_changed`), until the next
    /// roster replaces it.
    pub fn patch(&mut self, uid: Uid, muted: bool, deafened: bool, cx: &mut Context<Self>) {
        if let Some(p) = self.list.iter_mut().find(|p| p.uid == uid)
            && (p.state.muted, p.state.deafened) != (muted, deafened)
        {
            p.state.muted = muted;
            p.state.deafened = deafened;
            cx.notify();
        }
    }

    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.list.is_empty() {
            self.list.clear();
            cx.notify();
        }
    }
}
