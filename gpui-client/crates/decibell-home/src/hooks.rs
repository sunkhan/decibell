//! What the self-tests reach from inside the app: the views this crate's panes and overlays built
//! last (weak handles, kept as globals), and — only with `DECIBELL_TEST_PROBES=1`, the shell's
//! probe switch — what some views drew in their last render. Off, nothing is recorded.

use std::cell::RefCell;
use std::collections::HashMap;

use gpui::{App, Entity, Global, WeakEntity};

/// The last view of type `V` this crate built for a place (one conversation, one Friends page…).
pub struct Shown<V: 'static>(pub WeakEntity<V>);

impl<V: 'static> Global for Shown<V> {}

pub fn remember<V: 'static>(entity: &Entity<V>, cx: &mut App) {
    cx.set_global(Shown(entity.downgrade()));
}

/// The view, while it lives.
pub fn shown<V: 'static>(cx: &App) -> Option<Entity<V>> {
    cx.try_global::<Shown<V>>().and_then(|s| s.0.upgrade())
}

thread_local! {
    static DRAWN: RefCell<HashMap<&'static str, Vec<String>>> = RefCell::new(HashMap::new());
}

/// Record what a view drew under `key` (test runs only).
pub fn record(key: &'static str, items: impl FnOnce() -> Vec<String>) {
    if decibell_shell::probes::enabled() {
        let items = items();
        DRAWN.with(|d| d.borrow_mut().insert(key, items));
    }
}

/// What was last recorded under `key`.
pub fn drawn(key: &str) -> Vec<String> {
    DRAWN.with(|d| d.borrow().get(key).cloned().unwrap_or_default())
}
