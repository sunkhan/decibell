//! Keeping a cached view in step with what it reads (docs/model.md §4).
//!
//! GPUI re-renders a cached view (`entity.cached(…)`, one per pane) only when that view itself is
//! notified: reading a store in `render` is not enough, the pane would replay its last paint. So a
//! pane holds a [`Watch`] and lists, once, every store its `render` reads:
//!
//! ```ignore
//! struct ChannelList { watch: Watch, … }
//! fn new(cx: &mut Context<Self>) -> Self {
//!     let model = Model::global(cx).clone();
//!     let mut watch = Watch::new();
//!     watch
//!         .store(&model.nav, cx)                                   // every change
//!         .store_if(&model.servers, cx, |this, servers, _| this.name_changed(servers))
//!         .event(&model.live.voice.rosters, cx, |this, e: &VoiceRosterEvent| this.shows(e))
//!         .people(cx, |this, uid| this.shows_user(uid));           // names, avatars, statuses
//!     Self { watch, … }
//! }
//! ```
//!
//! The subscriptions live as long as the `Watch` (the view): dropping the view drops them.

use std::rc::Rc;

use decibell_model::live::presence::avatars::AvatarsChanged;
use decibell_model::live::presence::statuses::{MessagesChanged, PresenceChanged};
use decibell_model::stores::users::NamesChanged;
use decibell_model::{Model, Uid};
use gpui::{App, Context, Entity, EventEmitter, Subscription};

/// Which uids a view shows.
type Shows<V> = Rc<dyn Fn(&V, Uid) -> bool>;

/// The subscriptions of one view. See the module doc.
#[derive(Default)]
pub struct Watch {
    subscriptions: Vec<Subscription>,
}

impl Watch {
    pub fn new() -> Self {
        Self::default()
    }

    /// Re-render whenever `store` notifies.
    pub fn store<V: 'static, T: 'static>(&mut self, store: &Entity<T>, cx: &mut Context<V>) -> &mut Self {
        self.subscriptions.push(cx.observe(store, |_, _, cx| cx.notify()));
        self
    }

    /// Re-render when `store` notifies and `changed` says the part this view shows moved (it may
    /// update the view's own copy of that part as it compares).
    pub fn store_if<V: 'static, T: 'static>(
        &mut self,
        store: &Entity<T>,
        cx: &mut Context<V>,
        mut changed: impl FnMut(&mut V, &T, &App) -> bool + 'static,
    ) -> &mut Self {
        self.subscriptions.push(cx.observe(store, move |this, store, cx| {
            if changed(this, store.read(cx), cx) {
                cx.notify();
            }
        }));
        self
    }

    /// Re-render when `store` emits an event `wants` accepts (a keyed event: `NamesChanged`,
    /// `VoiceRosterEvent::Changed`, …).
    pub fn event<V: 'static, T: EventEmitter<E> + 'static, E: 'static>(
        &mut self,
        store: &Entity<T>,
        cx: &mut Context<V>,
        mut wants: impl FnMut(&mut V, &E) -> bool + 'static,
    ) -> &mut Self {
        self.subscriptions.push(cx.subscribe(store, move |this, _, event: &E, cx| {
            if wants(this, event) {
                cx.notify();
            }
        }));
        self
    }

    /// Re-render when a person the view shows changes: their name (renames), picture, status or
    /// status message, and our own presence (our dot and message come from there). `shows` is
    /// asked per uid of each change; a list that shows everyone passes `|_, _| true`.
    pub fn people<V: 'static>(&mut self, cx: &mut Context<V>, shows: impl Fn(&V, Uid) -> bool + 'static) -> &mut Self {
        let model = Model::global(cx).clone();
        let shows: Shows<V> = Rc::new(shows);
        let any = |shows: &Shows<V>| {
            let shows = shows.clone();
            move |this: &mut V, uids: &[Uid]| uids.iter().any(|uid| shows(this, *uid))
        };
        let names = any(&shows);
        let avatars = any(&shows);
        let statuses = any(&shows);
        let messages = any(&shows);
        self.event(&model.users, cx, move |this, e: &NamesChanged| names(this, &e.0))
            .event(&model.live.presence.avatars, cx, move |this, e: &AvatarsChanged| avatars(this, &e.0))
            .event(&model.live.presence.statuses, cx, move |this, e: &PresenceChanged| statuses(this, &e.0))
            .event(&model.live.presence.messages, cx, move |this, e: &MessagesChanged| messages(this, &e.0))
            .store(&model.live.presence.own, cx)
    }

    /// How many subscriptions the view holds (tests).
    pub fn len(&self) -> usize {
        self.subscriptions.len()
    }

    pub fn is_empty(&self) -> bool {
        self.subscriptions.is_empty()
    }
}

#[cfg(test)]
mod tests {
    use std::cell::Cell;
    use std::rc::Rc;

    use gpui::{AppContext as _, TestAppContext};

    use super::*;

    struct Shows {
        uid: Uid,
        notified: Rc<Cell<u32>>,
        _watch: Watch,
    }

    #[gpui::test]
    fn people_redraws_only_for_the_uids_shown(cx: &mut TestAppContext) {
        let model = cx.update(|cx| Model::init(cx, None));
        let notified = Rc::new(Cell::new(0));
        let view = cx.new(|cx| {
            let mut watch = Watch::new();
            watch.people(cx, |this: &Shows, uid| uid == this.uid);
            Shows { uid: 5, notified: notified.clone(), _watch: watch }
        });
        let _count = cx.update(|cx| {
            let notified = notified.clone();
            cx.observe(&view, move |_, _| notified.set(notified.get() + 1))
        });
        model.users.update(cx, |u, cx| u.remember([(6, "someone")], true, cx));
        cx.run_until_parked();
        assert_eq!(notified.get(), 0, "a rename of someone not shown");
        model.users.update(cx, |u, cx| u.remember([(5, "shown")], true, cx));
        cx.run_until_parked();
        assert_eq!(notified.get(), 1, "a rename of the person shown");
        view.read_with(cx, |v, _| assert_eq!(v.notified.get(), 1));
    }
}
