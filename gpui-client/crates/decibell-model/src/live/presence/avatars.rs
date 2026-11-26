//! Profile pictures by uid (`stores/avatarStore.ts`; spec `2026-05-12-custom-profile-
//! pictures-design.md` §7). A rename keeps the cache.
//!
//! An entry is a version (sha256-hex of the picture, '' = none) and a state:
//!
//! - `idle`: version known, bytes not fetched (just announced, never fetched, evicted);
//! - `loading`: queued or `fetch_avatar` in flight;
//! - `loaded`: bytes here;
//! - `missing`: the user has no picture;
//! - `error`: the fetch failed; the letter shows, retried when central names the version
//!   again.
//!
//! Versions come from friend lists, subscription answers, presence deltas, avatar pushes,
//! and for ourselves a seed at sign-in (nothing tells us our own version). Fetches run
//! [`MAX_IN_FLIGHT`] at a time, newest request first, and skip users no longer on screen
//! (a fast scroll fetches what stays in view). Past [`MAX_LOADED`] loaded pictures, the
//! least recently loaded of users off screen drop their bytes (the version stays).
//!
//! **Bytes and decoding.** A loaded entry holds the JPEG as central sent it behind GPUI's
//! `Image` handle (bytes + format + content id); the view draws it with `img(image)` and
//! GPUI decodes it into its image cache. Evicting drops our handle, not GPUI's decoded
//! copy: a view that shows many pictures must draw them through a bounded `ImageCache`
//! (the UI layer's job, see docs/model.md M3). The renderer awaited `img.decode()` before
//! flipping to loaded to avoid a blank frame; GPUI's `img()` shows its fallback for the
//! frame it decodes in, which the avatar view covers with the letter.
//!
//! **Rhythm**: per batch of announced versions and per finished fetch. A view showing one
//! person subscribes and filters by uid ([`AvatarsChanged`]); a list observes.

use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;

use futures::future::LocalBoxFuture;
use gpui::{Context, EventEmitter, SharedString, WeakEntity};

use super::interest::PresenceInterest;
use crate::error::UiError;
use crate::ids::Uid;

/// Fetches in flight at once: enough to fill a list quickly without queueing a screenful
/// behind each other on the central connection (each times out after 5 s).
pub const MAX_IN_FLIGHT: usize = 6;
/// Loaded pictures kept; beyond this the oldest of users off screen go.
pub const MAX_LOADED: usize = 400;

#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
#[serde(rename_all = "lowercase")]
pub enum AvatarStatus {
    Idle,
    Loading,
    Loaded,
    Missing,
    Error,
}

#[derive(Clone)]
pub struct AvatarEntry {
    pub version: SharedString,
    pub image: Option<Arc<gpui::Image>>,
    pub status: AvatarStatus,
    /// Load order (for eviction: least recently loaded first).
    loaded_seq: u64,
}

/// `fetch_avatar`: the current version and the JPEG ('' / empty: no picture).
pub type AvatarFetcher = Rc<dyn Fn(Uid) -> LocalBoxFuture<'static, Result<(String, Vec<u8>), UiError>>>;

#[derive(Debug, Clone, PartialEq)]
pub struct AvatarsChanged(pub Vec<Uid>);

pub struct Avatars {
    entries: HashMap<Uid, AvatarEntry>,
    /// Waiting to be fetched; the newest request goes first.
    queue: Vec<Uid>,
    queued: HashSet<Uid>,
    in_flight: usize,
    /// Bumped by sign-out: fetches started before it drop their result.
    generation: u64,
    next_seq: u64,
    fetcher: AvatarFetcher,
    /// Who is on screen (the presence interest set).
    interest: WeakEntity<PresenceInterest>,
}

impl EventEmitter<AvatarsChanged> for Avatars {}

impl Avatars {
    pub fn new(fetcher: AvatarFetcher, interest: WeakEntity<PresenceInterest>) -> Self {
        Avatars {
            entries: HashMap::new(),
            queue: Vec::new(),
            queued: HashSet::new(),
            in_flight: 0,
            generation: 0,
            next_seq: 0,
            fetcher,
            interest,
        }
    }

    pub fn get(&self, uid: Uid) -> Option<&AvatarEntry> {
        self.entries.get(&uid)
    }

    /// The picture to draw, if loaded.
    pub fn image(&self, uid: Uid) -> Option<&Arc<gpui::Image>> {
        self.entries.get(&uid).and_then(|e| e.image.as_ref())
    }

    pub fn loaded(&self) -> usize {
        self.entries.values().filter(|e| e.status == AvatarStatus::Loaded).count()
    }

    pub fn in_flight(&self) -> usize {
        self.in_flight
    }

    fn changed(&self, uids: Vec<Uid>, cx: &mut Context<Self>) {
        if !uids.is_empty() {
            cx.emit(AvatarsChanged(uids));
            cx.notify();
        }
    }

    /// The known current versions of users (one notify for a whole friend / presence
    /// list). A version that differs from the entry's invalidates it (idle, or missing
    /// for ''); a failed entry is retried on the next mention.
    pub fn set_versions(&mut self, list: impl IntoIterator<Item = (Uid, SharedString)>, cx: &mut Context<Self>) {
        let mut changed = Vec::new();
        for (uid, version) in list {
            if uid <= 0 {
                continue;
            }
            if let Some(e) = self.entries.get(&uid)
                && e.version == version
                && e.status != AvatarStatus::Error
            {
                continue;
            }
            let status = if version.is_empty() { AvatarStatus::Missing } else { AvatarStatus::Idle };
            self.entries.insert(uid, AvatarEntry { version, image: None, status, loaded_seq: 0 });
            changed.push(uid);
        }
        self.changed(changed, cx);
    }

    /// Fetch `uid`'s picture without knowing its version (our own, at sign-in): an idle
    /// entry the fetch fills in. No-op when there is an entry already.
    pub fn seed(&mut self, uid: Uid, cx: &mut Context<Self>) {
        if uid <= 0 || self.entries.contains_key(&uid) {
            return;
        }
        self.entries.insert(uid, AvatarEntry { version: "".into(), image: None, status: AvatarStatus::Idle, loaded_seq: 0 });
        self.changed(vec![uid], cx);
    }

    /// Queue a fetch if the entry is idle (a view showing `uid` calls this where it
    /// builds its row, never from `render`).
    pub fn fetch_if_needed(&mut self, uid: Uid, cx: &mut Context<Self>) {
        let Some(e) = self.entries.get_mut(&uid) else { return };
        if e.status != AvatarStatus::Idle {
            return;
        }
        e.status = AvatarStatus::Loading;
        if self.queued.insert(uid) {
            self.queue.push(uid);
        }
        self.changed(vec![uid], cx);
        self.pump(cx);
    }

    fn on_screen(&self, uid: Uid, cx: &Context<Self>) -> bool {
        self.interest.upgrade().is_some_and(|i| i.read(cx).is_on_screen(uid))
    }

    fn pump(&mut self, cx: &mut Context<Self>) {
        let mut dropped = Vec::new();
        while self.in_flight < MAX_IN_FLIGHT {
            let Some(uid) = self.queue.pop() else { break };
            self.queued.remove(&uid);
            let Some(entry) = self.entries.get(&uid) else { continue };
            if entry.status != AvatarStatus::Loading {
                continue;
            }
            if !self.on_screen(uid, cx) {
                // Scrolled away while queued: fetched when it shows again.
                self.entries.get_mut(&uid).unwrap().status = AvatarStatus::Idle;
                dropped.push(uid);
                continue;
            }
            self.in_flight += 1;
            let asked = entry.version.clone();
            let generation = self.generation;
            let fetch = (self.fetcher)(uid);
            cx.spawn(async move |this, cx| {
                let result = fetch.await;
                let _ = this.update(cx, |this, cx| {
                    this.in_flight -= 1;
                    this.finish(uid, asked, generation, result, cx);
                    this.pump(cx);
                });
            })
            .detach();
        }
        self.changed(dropped, cx);
    }

    fn finish(
        &mut self,
        uid: Uid,
        asked: SharedString,
        generation: u64,
        result: Result<(String, Vec<u8>), UiError>,
        cx: &mut Context<Self>,
    ) {
        if generation != self.generation {
            return;
        }
        let Some(cur) = self.entries.get(&uid) else { return };
        match result {
            Ok((version, data)) => {
                // A newer version was announced while this was in flight: that one is
                // fetched next.
                if cur.version != asked {
                    return;
                }
                let has = !version.is_empty() && !data.is_empty();
                self.next_seq += 1;
                let entry = AvatarEntry {
                    version: version.into(),
                    image: has.then(|| Arc::new(gpui::Image::from_bytes(gpui::ImageFormat::Jpeg, data))),
                    status: if has { AvatarStatus::Loaded } else { AvatarStatus::Missing },
                    loaded_seq: self.next_seq,
                };
                self.entries.insert(uid, entry);
                let mut changed = vec![uid];
                if has {
                    changed.extend(self.evict_off_screen(cx));
                }
                self.changed(changed, cx);
            }
            Err(e) => {
                log::warn!("[avatars] fetch failed for uid {uid}: {e}");
                if let Some(cur) = self.entries.get_mut(&uid)
                    && cur.version == asked
                    && cur.status == AvatarStatus::Loading
                {
                    cur.status = AvatarStatus::Error;
                    self.changed(vec![uid], cx);
                }
            }
        }
    }

    /// Past [`MAX_LOADED`], drop the bytes of the least recently loaded pictures of users
    /// off screen; they are fetched again if they come back.
    fn evict_off_screen(&mut self, cx: &Context<Self>) -> Vec<Uid> {
        let loaded = self.loaded();
        if loaded <= MAX_LOADED {
            return Vec::new();
        }
        let mut excess = loaded - MAX_LOADED;
        let mut candidates: Vec<(u64, Uid)> = self
            .entries
            .iter()
            .filter(|(_, e)| e.status == AvatarStatus::Loaded)
            .map(|(uid, e)| (e.loaded_seq, *uid))
            .collect();
        candidates.sort_unstable();
        let mut evicted = Vec::new();
        for (_, uid) in candidates {
            if excess == 0 {
                break;
            }
            if self.on_screen(uid, cx) {
                continue;
            }
            let e = self.entries.get_mut(&uid).unwrap();
            e.image = None;
            e.status = AvatarStatus::Idle;
            evicted.push(uid);
            excess -= 1;
        }
        evicted
    }

    /// Sign-out: every entry and queued fetch goes; fetches in flight drop their result.
    pub fn clear(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.queue.clear();
        self.queued.clear();
        let uids: Vec<Uid> = self.entries.drain().map(|(u, _)| u).collect();
        self.changed(uids, cx);
    }
}

#[cfg(test)]
mod tests {
    use std::cell::{Cell, RefCell};

    use gpui::{AppContext as _, Entity, TestAppContext};

    use super::*;

    struct Rig {
        avatars: Entity<Avatars>,
        interest: Entity<PresenceInterest>,
        /// uids fetched, in order.
        fetched: Rc<RefCell<Vec<Uid>>>,
        /// Fetches complete only when released (one per tick).
        gate: Rc<Cell<bool>>,
    }

    fn rig(cx: &mut TestAppContext) -> Rig {
        let interest = cx.new(|_| PresenceInterest::new(Rc::new(|_| Box::pin(async { Ok(()) }))));
        let fetched: Rc<RefCell<Vec<Uid>>> = Rc::default();
        let gate = Rc::new(Cell::new(true));
        let executor = cx.executor();
        let avatars = cx.new(|_| {
            let (fetched, gate, executor) = (fetched.clone(), gate.clone(), executor);
            Avatars::new(
                Rc::new(move |uid| {
                    fetched.borrow_mut().push(uid);
                    let (gate, executor) = (gate.clone(), executor.clone());
                    Box::pin(async move {
                        while !gate.get() {
                            executor.timer(std::time::Duration::from_millis(10)).await;
                        }
                        Ok((format!("v{uid}"), vec![0xff, 0xd8, 0xff, uid as u8]))
                    })
                }),
                interest.downgrade(),
            )
        });
        Rig { avatars, interest, fetched, gate }
    }

    #[gpui::test]
    fn six_at_a_time_newest_first_and_off_screen_skipped(cx: &mut TestAppContext) {
        let r = rig(cx);
        r.gate.set(false);
        let uids: Vec<Uid> = (1..=10).collect();
        r.interest.update(cx, |i, cx| i.retain(uids.iter().copied().filter(|u| *u != 3), cx));
        r.avatars.update(cx, |a, cx| {
            a.set_versions(uids.iter().map(|u| (*u, SharedString::from(format!("v{u}")))), cx);
            for u in &uids {
                a.fetch_if_needed(*u, cx);
            }
        });
        cx.run_until_parked();
        // The first six requests started as they came (the queue was empty each time);
        // 3 is not on screen and is skipped.
        assert_eq!(*r.fetched.borrow(), vec![1, 2, 4, 5, 6, 7]);
        r.avatars.read_with(cx, |a, _| {
            assert_eq!(a.in_flight(), MAX_IN_FLIGHT);
            assert_eq!(a.get(3).unwrap().status, AvatarStatus::Idle);
        });
        // 10 scrolls away before its turn.
        r.interest.update(cx, |i, cx| i.release([10], cx));
        r.gate.set(true);
        cx.executor().advance_clock(std::time::Duration::from_millis(20));
        cx.run_until_parked();
        assert_eq!(&r.fetched.borrow()[6..], &[9, 8], "newest first; 10 skipped");
        r.avatars.read_with(cx, |a, _| {
            assert_eq!(a.get(10).unwrap().status, AvatarStatus::Idle);
            assert_eq!(a.get(9).unwrap().status, AvatarStatus::Loaded);
            assert!(a.image(9).is_some());
            assert_eq!(a.in_flight(), 0);
        });
    }

    #[gpui::test]
    fn versions_invalidate_and_sign_out_drops_late_results(cx: &mut TestAppContext) {
        let r = rig(cx);
        r.interest.update(cx, |i, cx| i.retain([2], cx));
        r.avatars.update(cx, |a, cx| {
            a.set_versions([(2, "v2".into()), (3, "".into())], cx);
            a.fetch_if_needed(2, cx);
            a.fetch_if_needed(3, cx);
        });
        cx.run_until_parked();
        r.avatars.read_with(cx, |a, _| {
            assert_eq!(a.get(2).unwrap().status, AvatarStatus::Loaded);
            assert_eq!(a.get(3).unwrap().status, AvatarStatus::Missing, "no picture: nothing fetched");
        });
        assert_eq!(*r.fetched.borrow(), vec![2]);
        // The same version again changes nothing; a new one invalidates.
        r.avatars.update(cx, |a, cx| a.set_versions([(2, "v2".into())], cx));
        r.avatars.read_with(cx, |a, _| assert_eq!(a.get(2).unwrap().status, AvatarStatus::Loaded));
        r.avatars.update(cx, |a, cx| a.set_versions([(2, "v2b".into())], cx));
        r.avatars.read_with(cx, |a, _| assert!(a.get(2).unwrap().image.is_none()));
        // A fetch in flight across sign-out drops its result.
        r.gate.set(false);
        r.avatars.update(cx, |a, cx| a.fetch_if_needed(2, cx));
        r.avatars.update(cx, |a, cx| a.clear(cx));
        r.gate.set(true);
        cx.executor().advance_clock(std::time::Duration::from_millis(20));
        cx.run_until_parked();
        r.avatars.read_with(cx, |a, _| assert!(a.get(2).is_none()));
    }

    /// Past 400 loaded pictures, the least recently loaded off-screen ones drop their bytes.
    #[gpui::test]
    fn retention_is_bounded(cx: &mut TestAppContext) {
        let r = rig(cx);
        let total = MAX_LOADED as Uid + 20;
        r.interest.update(cx, |i, cx| i.retain(1..=total, cx));
        for uid in 1..=total {
            r.avatars.update(cx, |a, cx| {
                a.set_versions([(uid, SharedString::from(format!("v{uid}")))], cx);
                a.fetch_if_needed(uid, cx);
            });
            cx.run_until_parked();
        }
        r.avatars.read_with(cx, |a, _| assert_eq!(a.loaded(), total as usize, "all on screen: none evicted"));
        // The first 30 scroll away; the next load trims to the cap from the oldest.
        r.interest.update(cx, |i, cx| i.release(1..=30, cx));
        r.avatars.update(cx, |a, cx| {
            a.set_versions([(total + 1, "x".into())], cx);
        });
        r.interest.update(cx, |i, cx| i.retain([total + 1], cx));
        r.avatars.update(cx, |a, cx| a.fetch_if_needed(total + 1, cx));
        cx.run_until_parked();
        r.avatars.read_with(cx, |a, _| {
            assert_eq!(a.loaded(), MAX_LOADED);
            assert_eq!(a.get(1).unwrap().status, AvatarStatus::Idle, "the oldest off-screen went first");
            assert_eq!(a.get(21).unwrap().status, AvatarStatus::Idle);
            assert_eq!(a.get(22).unwrap().status, AvatarStatus::Loaded);
            assert_eq!(a.get(31).unwrap().status, AvatarStatus::Loaded, "on screen: kept");
            assert_eq!(a.get(1).unwrap().version.as_ref(), "v1", "the version stays");
        });
    }
}
