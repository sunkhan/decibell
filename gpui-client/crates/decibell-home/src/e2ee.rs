//! A peer's encryption state as the core sees it (`usePeerE2ee`): keys published, their
//! fingerprint, the conversation's safety number, pin state and the last identity change.
//!
//! The renderer fetched it per mounted view and again whenever our status or the peer's change
//! marker moved (`[peer, status, changedAt]`), showing nothing meanwhile. Here one small store
//! keeps one answer per peer *for the key it was asked under* (our status, the marker), so the DM
//! header, the notice under it and the profile's Privacy tab share one request; an answer for an
//! older key is never shown (as the renderer's `setInfo(null)` on a dependency change).
//!
//! Views call [`PeerInfos::want`] from their observers (never from `render`) and subscribe to
//! [`PeerInfoChanged`] for their peer.

use std::collections::HashMap;

use decibell_core::api;
use decibell_model::call::call;
use decibell_model::live::e2ee::E2eeStatus;
use decibell_model::{Model, Uid};
use gpui::{App, AppContext as _, Context, Entity, EventEmitter, Global, SharedString};

/// `E2eePeerInfo`, kept.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PeerInfo {
    pub has_keys: bool,
    pub fingerprint: SharedString,
    /// Groups of digits separated by spaces; empty unless we're ready and they have keys.
    pub safety_number: SharedString,
    /// Unix seconds of the last identity change we saw; 0 = never.
    pub changed_at: i64,
    /// We've talked to this identity before (TOFU pin present).
    pub pinned: bool,
}

/// What an answer is valid for: our status and the peer's change marker when it was asked.
pub type InfoKey = (E2eeStatus, i64);

struct Entry {
    key: InfoKey,
    info: Option<PeerInfo>,
}

/// One peer's answer changed (arrived, or went stale).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PeerInfoChanged(pub Uid);

#[derive(Default)]
pub struct PeerInfos {
    entries: HashMap<Uid, Entry>,
}

impl EventEmitter<PeerInfoChanged> for PeerInfos {}

struct Store(Entity<PeerInfos>);

impl Global for Store {}

/// The key `uid`'s answer must have to be shown now.
pub fn current_key(uid: Uid, cx: &App) -> InfoKey {
    let e2ee = Model::global(cx).live.e2ee.read(cx);
    (e2ee.status, e2ee.changed_peers.get(&uid).copied().unwrap_or(0))
}

impl PeerInfos {
    /// The store (made on first use).
    pub fn entity(cx: &mut App) -> Entity<PeerInfos> {
        if let Some(store) = cx.try_global::<Store>() {
            return store.0.clone();
        }
        let entity = cx.new(|_| PeerInfos::default());
        cx.set_global(Store(entity.clone()));
        entity
    }

    /// The store if it was made already (for reads from `&App`).
    pub fn existing(cx: &App) -> Option<Entity<PeerInfos>> {
        cx.try_global::<Store>().map(|store| store.0.clone())
    }

    /// The answer for `uid` under `key`, if it arrived (`usePeerE2ee`'s `info`).
    pub fn get(&self, uid: Uid, key: InfoKey) -> Option<&PeerInfo> {
        self.entries.get(&uid).filter(|e| e.key == key).and_then(|e| e.info.as_ref())
    }

    /// A screen's fixture: the answer the core would give.
    pub fn seed(&mut self, uid: Uid, key: InfoKey, info: PeerInfo, cx: &mut Context<Self>) {
        self.entries.insert(uid, Entry { key, info: Some(info) });
        cx.emit(PeerInfoChanged(uid));
        cx.notify();
    }

    /// Ask the core about `uid` unless an answer for the current key is in or on its way. Our
    /// status "unavailable" asks nothing (the renderer's early return).
    pub fn want(uid: Uid, cx: &mut App) {
        if uid <= 0 {
            return;
        }
        let key = current_key(uid, cx);
        let store = PeerInfos::entity(cx);
        let fresh = store.update(cx, |s, cx| {
            if s.entries.get(&uid).is_some_and(|e| e.key == key) {
                return false;
            }
            let had = s.entries.insert(uid, Entry { key, info: None }).is_some_and(|e| e.info.is_some());
            if had {
                cx.emit(PeerInfoChanged(uid));
                cx.notify();
            }
            true
        });
        if !fresh || key.0 == E2eeStatus::Unavailable {
            return;
        }
        let name = Model::global(cx).name_of(uid, &SharedString::default(), cx).to_string();
        let request = api::e2ee::e2ee_peer_info(api::e2ee::PeerInfoArgs { username: name, uid: Some(uid) });
        call(cx, request, move |result, cx| {
            let Ok(info) = result else { return };
            let info = PeerInfo {
                has_keys: info.has_keys,
                fingerprint: info.fingerprint.into(),
                safety_number: info.safety_number.into(),
                changed_at: info.changed_at,
                pinned: info.pinned,
            };
            store.update(cx, |s, cx| {
                // Only the request for the key still wanted lands.
                if let Some(entry) = s.entries.get_mut(&uid).filter(|e| e.key == key) {
                    entry.info = Some(info);
                    cx.emit(PeerInfoChanged(uid));
                    cx.notify();
                }
            });
        });
    }

    /// A view of `uid` mounted (the renderer's hook fetched per mounted view: opening a
    /// conversation asks again, so a peer who set up or reset encryption since is seen): the kept
    /// answer goes (nothing shown until the new one, as `setInfo(null)`) and the core is asked.
    pub fn ask_again(uid: Uid, cx: &mut App) {
        if let Some(store) = PeerInfos::existing(cx) {
            store.update(cx, |s, cx| {
                if s.entries.remove(&uid).is_some() {
                    cx.emit(PeerInfoChanged(uid));
                    cx.notify();
                }
            });
        }
        PeerInfos::want(uid, cx);
    }

    /// Sign-out: nothing carries over to the next account.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        if !self.entries.is_empty() {
            self.entries.clear();
            cx.notify();
        }
    }
}

#[cfg(test)]
mod tests {
    use gpui::TestAppContext;

    use super::*;

    fn info(keys: bool) -> PeerInfo {
        PeerInfo { has_keys: keys, fingerprint: "ab".into(), safety_number: "1 2".into(), changed_at: 0, pinned: false }
    }

    #[gpui::test]
    fn an_answer_shows_only_under_the_key_it_was_asked_for(cx: &mut TestAppContext) {
        cx.update(|cx| {
            Model::init(cx, None);
            let store = PeerInfos::entity(cx);
            store.update(cx, |s, cx| s.seed(5, (E2eeStatus::Ready, 0), info(true), cx));
            assert!(store.read(cx).get(5, (E2eeStatus::Ready, 0)).is_some_and(|i| i.has_keys));
            // The peer's keys changed: the old answer is not shown under the new marker.
            assert!(store.read(cx).get(5, (E2eeStatus::Ready, 100)).is_none());
            assert!(store.read(cx).get(5, (E2eeStatus::Locked, 0)).is_none());
            // Asking again under a new key drops the stale answer (no core: nothing arrives).
            let model = Model::global(cx).clone();
            model.live.e2ee.update(cx, |e, cx| e.mark_peer_changed(5, 100, cx));
            PeerInfos::want(5, cx);
            assert!(store.read(cx).get(5, (E2eeStatus::Unavailable, 100)).is_none());
        });
    }
}
