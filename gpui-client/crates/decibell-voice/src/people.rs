//! The people part of a stage (the grid, the row under streams, the filmstrip): which
//! participants get a tile ([`Visible`]), their tile entities, the "+N more" tile, and the swap
//! timer that brings a hidden speaker into view. Shared by the voice stage and the filmstrip, as
//! the renderer shared `useVisibleParticipants`.
//!
//! Speaking flips reach [`People::speaking`] from the host's subscription; they change nothing on
//! screen (the tiles redraw themselves) unless a swap happens. The one timer exists only while a
//! hidden speaker waits out the 600 ms / 3 s rule.

use std::collections::{HashMap, HashSet};
use std::time::{Duration, Instant};

use decibell_model::live::voice::gain;
use decibell_model::live::voice::session::Participant;
use decibell_model::{Model, ServerId, Uid};
use gpui::{AppContext as _, Context, Entity, SharedString, Task};

use crate::layout::{Check, Visible};
use crate::tiles::{OverflowData, OverflowTile, ParticipantTile, PersonData};

/// What a host view implements to own a [`People`].
pub trait PeopleHost: Sized + 'static {
    fn people(&mut self) -> &mut People;
    /// Recompute and push the layout (called after a swap).
    fn relayout(&mut self, cx: &mut Context<Self>);
}

pub struct People {
    pub visible: Visible,
    pub tiles: HashMap<Uid, Entity<ParticipantTile>>,
    pub overflow: Option<Entity<OverflowTile>>,
    mini: bool,
    timer: Option<Task<()>>,
    /// The inputs of the last [`People::place`] (the swap check needs them).
    roster: Vec<Uid>,
    slots: usize,
    pinned: HashSet<Uid>,
}

/// One placement: who is shown (in order) and who is behind "+N more".
pub struct Placed {
    pub shown: Vec<Uid>,
    pub hidden: Vec<Uid>,
}

/// What the tiles need besides the roster.
pub struct TileContext<'a> {
    pub me: Uid,
    pub server: Option<ServerId>,
    pub channel_name: SharedString,
    pub participants: &'a [Participant],
    /// Our own engine's mute / deafen (the roster's copy of ours lags).
    pub self_muted: bool,
    pub self_deafened: bool,
    pub streamers: &'a HashSet<Uid>,
    pub width: f32,
}

impl People {
    pub fn new(mini: bool) -> Self {
        Self {
            visible: Visible::default(),
            tiles: HashMap::new(),
            overflow: None,
            mini,
            timer: None,
            roster: Vec::new(),
            slots: 0,
            pinned: HashSet::new(),
        }
    }

    /// Place `slots` people of the roster (fewer than the roster: "+N more"), pinning `pinned`,
    /// and bring the tiles in line: created, updated (each notifies only if it changed), dropped.
    pub fn place<V: PeopleHost>(&mut self, slots: usize, pinned: HashSet<Uid>, tcx: &TileContext, cx: &mut Context<V>) -> Placed {
        let roster: Vec<Uid> = tcx.participants.iter().map(|p| p.uid).collect();
        let shown = self.visible.reconcile(&roster, slots, &pinned).to_vec();
        let overflow = slots < roster.len();
        let hidden = if overflow { self.visible.hidden(&roster) } else { Vec::new() };
        let model = Model::global(cx).clone();
        let settings = model.settings.read(cx).get().clone();
        let by_uid: HashMap<Uid, &Participant> = tcx.participants.iter().map(|p| (p.uid, p)).collect();
        for uid in &shown {
            let p = by_uid.get(uid);
            let local = *uid == tcx.me;
            let state = p.map(|p| p.state).unwrap_or_default();
            let data = PersonData {
                uid: *uid,
                name: p.map(|p| p.name.clone()).unwrap_or_default(),
                server: tcx.server,
                local,
                muted: if local { tcx.self_muted } else { state.muted },
                deafened: if local { tcx.self_deafened } else { state.deafened },
                server_muted: state.server_muted,
                server_deafened: state.server_deafened,
                suppressed: state.suppressed,
                streaming: tcx.streamers.contains(uid),
                locally_muted: gain::locally_muted(&settings, *uid),
                width: tcx.width,
                mini: self.mini,
            };
            match self.tiles.get(uid) {
                Some(tile) => tile.update(cx, |t, cx| t.set(data, cx)),
                None => {
                    let tile = cx.new(|cx| ParticipantTile::new(data, cx));
                    self.tiles.insert(*uid, tile);
                }
            }
        }
        let keep: HashSet<Uid> = shown.iter().copied().collect();
        self.tiles.retain(|uid, _| keep.contains(uid));
        if overflow && !hidden.is_empty() {
            let data = OverflowData {
                names: hidden.iter().map(|u| by_uid.get(u).map(|p| p.name.clone()).unwrap_or_default()).collect(),
                hidden: hidden.clone(),
                width: tcx.width,
                mini: self.mini,
                server: tcx.server,
                channel_name: tcx.channel_name.clone(),
            };
            match &self.overflow {
                Some(tile) => tile.update(cx, |t, cx| t.set(data, cx)),
                None => self.overflow = Some(cx.new(|cx| OverflowTile::new(data, cx))),
            }
        } else {
            self.overflow = None;
        }
        self.roster = roster;
        self.slots = slots;
        self.pinned = pinned;
        Placed { shown, hidden }
    }

    /// A speaking flip: remember it, and swap a hidden speaker in when the rule allows.
    pub fn speaking<V: PeopleHost>(&mut self, uid: Uid, speaking: bool, cx: &mut Context<V>) {
        self.visible.speaking(uid, speaking, Instant::now());
        self.check(cx);
    }

    fn check<V: PeopleHost>(&mut self, cx: &mut Context<V>) {
        match self.visible.check(&self.roster, self.slots, &self.pinned, Instant::now()) {
            Check::Idle => self.timer = None,
            Check::Swapped => {
                // Another hidden speaker may be queued behind this one.
                self.schedule(crate::layout::SWAP_COOLDOWN, cx);
                cx.defer_in_self(|this: &mut V, cx| this.relayout(cx));
            }
            Check::Wait(wait) => self.schedule(wait, cx),
        }
    }

    /// `setTimeout(check, ms + 16)`.
    fn schedule<V: PeopleHost>(&mut self, wait: Duration, cx: &mut Context<V>) {
        self.timer = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(wait + Duration::from_millis(16)).await;
            let _ = this.update(cx, |host, cx| {
                let people = host.people();
                people.timer = None;
                people.check(cx);
            });
        }));
    }
}

/// `defer` for a host from inside `People` (no `Window` in reach).
trait DeferInSelf<V> {
    fn defer_in_self(&mut self, f: impl FnOnce(&mut V, &mut Context<V>) + 'static);
}

impl<V: 'static> DeferInSelf<V> for Context<'_, V> {
    fn defer_in_self(&mut self, f: impl FnOnce(&mut V, &mut Context<V>) + 'static) {
        let this = self.entity().downgrade();
        self.defer(move |cx| {
            let _ = this.update(cx, f);
        });
    }
}
