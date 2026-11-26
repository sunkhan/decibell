//! The live / people half of the state model ("M3", docs/model.md): voice, streams, DM
//! calls, presence and statuses, friends, E2EE, hotkeys, devices, transfers, and the
//! sound / attention cues they raise. One grouping struct on `Model` ([`LiveStores`],
//! `model.live`), the entities inside it split by how often they change and who shows
//! them (each module's doc has its table).
//!
//! The router code for these events is in `events/{voice,calls,presence,friends,e2ee,
//! misc}.rs` (called from `events/todo.rs`' `m3` functions), the actions in
//! `actions/{voice,calls,presence,friends,e2ee,devices}.rs`.

pub mod calls;
pub mod devices;
pub mod e2ee;
pub mod friends;
pub mod hotkeys;
pub mod presence;
#[cfg(test)]
mod tests;
pub mod transfers;
pub mod voice;

use std::rc::Rc;
use std::time::Duration;

use gpui::{App, AppContext as _, Context, Entity, EventEmitter, SharedString, Task};

use crate::Model;
use crate::stores::navigation::ActiveView;
use crate::stores::overlays::MembershipRevocationNotice;

/// A UI sound (`utils/sounds.ts`); the files are `gpui-client/assets/sounds/<name>.wav`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Sound {
    Mute,
    Unmute,
    Deafen,
    Undeafen,
    UserJoin,
    UserLeave,
    StreamStart,
    StreamStop,
    Connect,
    Disconnect,
    CallRing,
    CallEnd,
}

impl Sound {
    pub fn asset(self) -> &'static str {
        match self {
            Sound::Mute => "sounds/mute.wav",
            Sound::Unmute => "sounds/unmute.wav",
            Sound::Deafen => "sounds/deafen.wav",
            Sound::Undeafen => "sounds/undeafen.wav",
            Sound::UserJoin => "sounds/user_join.wav",
            Sound::UserLeave => "sounds/user_leave.wav",
            Sound::StreamStart => "sounds/stream_start.wav",
            Sound::StreamStop => "sounds/stream_stop.wav",
            Sound::Connect => "sounds/connect.wav",
            Sound::Disconnect => "sounds/disconnect.wav",
            Sound::CallRing => "sounds/call_ring.wav",
            Sound::CallEnd => "sounds/call_end.wav",
        }
    }
}

/// What the app's audio layer and window do in answer to the model. The model plays
/// nothing itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Cue {
    /// Play once (concurrent plays mix).
    Play(Sound),
    /// Loop the ringtone / ringback until [`Cue::StopRing`] (`loopSound("call_ring")`).
    StartRing,
    StopRing,
    /// Ask for attention without stealing focus (taskbar flash / dock bounce): an incoming
    /// call, unless Do Not Disturb.
    Flash,
}

/// Emits [`Cue`]s; never notifies (nothing draws it).
#[derive(Default)]
pub struct Cues {
    ringing: bool,
}

impl EventEmitter<Cue> for Cues {}

impl Cues {
    pub fn play(&mut self, sound: Sound, cx: &mut Context<Self>) {
        cx.emit(Cue::Play(sound));
    }

    pub fn start_ring(&mut self, cx: &mut Context<Self>) {
        // `startRing` stops the old loop first.
        if self.ringing {
            cx.emit(Cue::StopRing);
        }
        self.ringing = true;
        cx.emit(Cue::StartRing);
    }

    pub fn stop_ring(&mut self, cx: &mut Context<Self>) {
        if self.ringing {
            self.ringing = false;
            cx.emit(Cue::StopRing);
        }
    }

    pub fn flash(&mut self, cx: &mut Context<Self>) {
        cx.emit(Cue::Flash);
    }

    pub fn ringing(&self) -> bool {
        self.ringing
    }
}

/// How long the "you were kicked / banned" notice stays (`MembershipRevokedToast`).
pub const REVOCATION_NOTICE_LIFETIME: Duration = Duration::from_millis(8000);

/// Small timers that belong to notices other stores hold: the membership revocation
/// notice's 8 s (the renderer's toast component cleared it).
#[derive(Default)]
pub struct Notices {
    revocation: Option<MembershipRevocationNotice>,
    revocation_timer: Option<Task<()>>,
}

impl Notices {
    fn follow_revocation(&mut self, notice: Option<MembershipRevocationNotice>, model: &Model, cx: &mut Context<Self>) {
        if self.revocation == notice {
            return;
        }
        self.revocation = notice.clone();
        self.revocation_timer = notice.map(|notice| {
            let overlays = model.overlays.clone();
            cx.spawn(async move |this, cx| {
                cx.background_executor().timer(REVOCATION_NOTICE_LIFETIME).await;
                let _ = this.update(cx, |this, _| this.revocation_timer = None);
                overlays.update(cx, |o, cx| {
                    if o.membership_revocation.as_ref() == Some(&notice) {
                        o.set_membership_revocation(None, cx);
                    }
                });
            })
        });
    }
}

/// The headline of the revocation notice.
pub fn revocation_headline(action: &str, server_name: &str) -> String {
    match action {
        "ban" => format!("You have been banned from {server_name}"),
        "kick" => format!("You have been kicked from {server_name}"),
        _ => format!("You have been removed from {server_name}"),
    }
}

/// The OS window title (`hooks/useWindowTitle.ts`): "<community> — Decibell" in a
/// community or voice view, "<their current name> — Decibell" in a DM with an open
/// thread (the open conversation of `dm.list`, its row's name as the fallback), else
/// "Decibell". The app recomputes it when `nav`, `servers`, `dm.list` or the users store's
/// `NamesChanged` fire, and sets it only when it differs.
pub fn window_title(model: &Model, cx: &App) -> SharedString {
    let nav = model.nav.read(cx);
    match nav.active_view {
        ActiveView::Dm => {
            let list = model.dm.list.read(cx);
            if let Some(peer) = list.active_peer {
                let fallback = list.get(peer).map(|c| c.username.clone()).unwrap_or_default();
                let name = model.name_of(peer, &fallback, cx);
                if !name.is_empty() {
                    return format!("{name} — Decibell").into();
                }
            }
        }
        ActiveView::Server | ActiveView::Voice => {
            if let Some(server) = nav.active_server.and_then(|id| model.servers.read(cx).get(id)) {
                return format!("{} — Decibell", server.name).into();
            }
        }
        _ => {}
    }
    SharedString::new_static("Decibell")
}

/// Every M3 entity (handles; cheap to clone).
#[derive(Clone)]
pub struct LiveStores {
    pub voice: voice::Voice,
    pub calls: Entity<calls::Calls>,
    pub presence: presence::PresenceStores,
    pub friends: Entity<friends::Friends>,
    pub e2ee: Entity<e2ee::E2ee>,
    pub hotkeys: Entity<hotkeys::Hotkeys>,
    pub devices: Entity<devices::Devices>,
    pub transfers: Entity<transfers::Transfers>,
    pub cues: Entity<Cues>,
    pub notices: Entity<Notices>,
}

impl LiveStores {
    /// The entities, each wired to the core through `crate::actions` (tests build the
    /// stores alone with fakes instead).
    pub fn new(cx: &mut App) -> LiveStores {
        LiveStores {
            voice: voice::Voice::new(cx),
            calls: cx.new(|_| calls::Calls::new(Rc::new(crate::actions::calls::CoreCalls))),
            presence: presence::PresenceStores::new(cx),
            friends: cx.new(|_| friends::Friends::new(Rc::new(crate::actions::friends::send_list_request))),
            e2ee: cx.new(|_| e2ee::E2ee::default()),
            hotkeys: cx.new(|_| hotkeys::Hotkeys::new(crate::actions::hotkeys::backend())),
            devices: cx.new(|_| devices::Devices::default()),
            transfers: cx.new(|_| transfers::Transfers::default()),
            cues: cx.new(|_| Cues::default()),
            notices: cx.new(|_| Notices::default()),
        }
    }
}

/// What follows other stores (installed once by `Model::init`, after every store exists):
/// the hotkeys and auto-away follow `Settings`, auto-away follows our speaking flag and
/// central's support for statuses, the watch set follows who is signed in, the
/// revocation notice gets its lifetime.
pub fn install(model: &Model, cx: &mut App) {
    let live = model.live.clone();
    let mut subs = Vec::new();

    let sync_settings = {
        let model = model.clone();
        move |cx: &mut App| {
            let settings = model.settings.read(cx).get().clone();
            model.live.hotkeys.update(cx, |h, cx| h.sync(&settings, cx));
            let supported = model.live.presence.own.read(cx).supported;
            let minutes = settings.away_after_minutes;
            model.live.presence.auto_away.update(cx, |a, cx| a.configure(minutes, supported, cx));
        }
    };
    subs.push(cx.observe(&model.settings, {
        let sync = sync_settings.clone();
        move |_, cx| sync(cx)
    }));
    subs.push(cx.observe(&live.presence.own, {
        let sync = sync_settings.clone();
        move |_, cx| sync(cx)
    }));
    subs.push(cx.subscribe(&live.voice.speaking, {
        let model = model.clone();
        move |_, e: &voice::speaking::SpeakingChanged, cx| {
            if model.auth.read(cx).is_me(e.uid) {
                let now = cx.background_executor().now();
                model.live.presence.auto_away.update(cx, |a, cx| a.set_speaking(e.speaking, now, cx));
            }
        }
    }));
    subs.push(cx.observe(&model.auth, {
        let model = model.clone();
        move |auth, cx| {
            let me = auth.read(cx).uid;
            model.live.presence.interest.update(cx, |i, cx| i.set_me(me, cx));
        }
    }));
    // Central's connection dropped: the statuses we had are stale; lists fall back to the
    // communities' own flags until central answers again (`usePresenceEvents`).
    subs.push(cx.observe(&model.connections, {
        let model = model.clone();
        move |connections, cx| {
            if connections.read(cx).central == crate::stores::connections::CentralStatus::Reconnecting {
                model.live.presence.statuses.update(cx, |s, cx| s.mark_disconnected(cx));
                model.live.presence.messages.update(cx, |m, cx| m.clear(cx));
            }
        }
    }));
    subs.push(cx.observe(&model.overlays, {
        let model = model.clone();
        move |overlays, cx| {
            let notice = overlays.read(cx).membership_revocation.clone();
            let m = model.clone();
            model.live.notices.update(cx, |n, cx| n.follow_revocation(notice, &m, cx));
        }
    }));
    // The subscriptions live as long as the model (the app's lifetime).
    for sub in subs {
        sub.detach();
    }
}
