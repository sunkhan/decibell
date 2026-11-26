//! Which views re-render when a store notifies, at the pinned GPUI (docs/model.md §4).
//! The rules every store and view in this client are shaped by, checked against the real
//! thing rather than remembered:
//!
//! 1. A window is redrawn when an entity it read during its last frame notifies
//!    (`App::notify` → `window_invalidators_by_entity` / `tracked_entities`,
//!    gpui/src/app.rs; `Window::record_entities_accessed`, gpui/src/window.rs).
//! 2. A redraw calls `render` on the root view and on every view that is **not cached**.
//! 3. A **cached** view (`entity.cached(style)`) is rendered again only when that view
//!    itself was notified (or a view inside it was): `prepaint_view` checks
//!    `window.dirty_views.contains(view_id)` (gpui/src/view.rs), and a store's id in
//!    `dirty_views` matches no view (`Window::mark_view_dirty` walks the dispatch tree's
//!    *view* path). Reading a store in `render` is therefore **not enough** for a
//!    cached view: it must `cx.observe(&store, |_, _, cx| cx.notify())`.
//! 4. A notify on an entity no window read costs no frame.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, StyleRefinement, Styled as _,
    Subscription, TestAppContext, Window, div,
};

struct Store {
    value: u32,
}

/// A pane that reads `store` in `render` and counts its renders.
struct Pane {
    store: Entity<Store>,
    renders: Rc<Cell<u32>>,
    _observe: Option<Subscription>,
}

impl Pane {
    fn new(store: &Entity<Store>, observe: bool, cx: &mut Context<Self>) -> (Self, Rc<Cell<u32>>) {
        let renders = Rc::new(Cell::new(0));
        let pane = Pane {
            store: store.clone(),
            renders: renders.clone(),
            _observe: observe.then(|| cx.observe(store, |_, _, cx| cx.notify())),
        };
        (pane, renders)
    }
}

impl Render for Pane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        div().child(self.store.read(cx).value.to_string())
    }
}

struct Root {
    cached_reader: Entity<Pane>,
    cached_observer: Entity<Pane>,
    plain_reader: Entity<Pane>,
    cached_bystander: Entity<Pane>,
    renders: Rc<Cell<u32>>,
}

impl Render for Root {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        self.renders.set(self.renders.get() + 1);
        let cached = || StyleRefinement::default().w_full().h_10();
        div()
            .size_full()
            .child(self.cached_reader.clone().cached(cached()))
            .child(self.cached_observer.clone().cached(cached()))
            .child(self.plain_reader.clone())
            .child(self.cached_bystander.clone().cached(cached()))
    }
}

#[gpui::test]
fn a_cached_view_re_renders_only_when_it_observes_what_it_reads(cx: &mut TestAppContext) {
    let store = cx.new(|_| Store { value: 0 });
    let other_store = cx.new(|_| Store { value: 100 });
    let unread_store = cx.new(|_| Store { value: 1000 });

    let mut counters = None;
    let _window = cx.add_window(|_, cx| {
        let mut pane = |store: &Entity<Store>, observe: bool| {
            let mut renders = None;
            let entity = cx.new(|cx| {
                let (pane, count) = Pane::new(store, observe, cx);
                renders = Some(count);
                pane
            });
            (entity, renders.unwrap())
        };
        let (cached_reader, reader_renders) = pane(&store, false);
        let (cached_observer, observer_renders) = pane(&store, true);
        let (plain_reader, plain_renders) = pane(&store, false);
        let (cached_bystander, bystander_renders) = pane(&other_store, true);
        let root_renders = Rc::new(Cell::new(0));
        counters = Some((root_renders.clone(), reader_renders, observer_renders, plain_renders, bystander_renders));
        Root { cached_reader, cached_observer, plain_reader, cached_bystander, renders: root_renders }
    });
    cx.run_until_parked();
    let (root, reader, observer, plain, bystander) = counters.unwrap();
    let counts = || (root.get(), reader.get(), observer.get(), plain.get(), bystander.get());
    assert_eq!(counts(), (1, 1, 1, 1, 1), "the first frame renders everything once");

    // Nothing changed: no frame.
    cx.run_until_parked();
    assert_eq!(counts(), (1, 1, 1, 1, 1));

    // The store notifies. The window read it, so it redraws (rule 1): the root and the
    // uncached pane render again (rule 2); the cached pane that observes re-renders; the
    // cached pane that only *reads* replays its old paint and is now stale (rule 3); the
    // cached pane of another store is untouched.
    store.update(cx, |s, cx| {
        s.value += 1;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(counts(), (2, 1, 2, 2, 1));

    // A store only one cached observer reads: that pane alone re-renders (and the root).
    other_store.update(cx, |s, cx| {
        s.value += 1;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(counts(), (3, 1, 2, 3, 2));

    // A store no view read: no frame at all (rule 4).
    unread_store.update(cx, |s, cx| {
        s.value += 1;
        cx.notify();
    });
    cx.run_until_parked();
    assert_eq!(counts(), (3, 1, 2, 3, 2));

    // Changing state without `notify` draws nothing, whoever reads it.
    store.update(cx, |s, _| s.value += 1);
    cx.run_until_parked();
    assert_eq!(counts(), (3, 1, 2, 3, 2));

    // Several notifies in one update are one frame.
    cx.update(|cx| {
        for _ in 0..5 {
            store.update(cx, |s, cx| {
                s.value += 1;
                cx.notify();
            });
        }
    });
    cx.run_until_parked();
    assert_eq!(counts(), (4, 1, 3, 4, 2));
}
