//! Where the shell's interactive parts are, for its self-tests: they click and scroll the real
//! window at the places these record. Off unless the process was started with
//! `DECIBELL_TEST_PROBES=1` (the shell's self-tests set it for the app they start): then each
//! probed element carries an invisible canvas that records its bounds when laid out, and a few
//! views record a flag when they render. Off, nothing is added to any element.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::OnceLock;

use gpui::{Bounds, IntoElement, Pixels, SharedString, Styled as _, canvas};

pub fn enabled() -> bool {
    static ENABLED: OnceLock<bool> = OnceLock::new();
    *ENABLED.get_or_init(|| std::env::var_os("DECIBELL_TEST_PROBES").is_some_and(|v| v == "1"))
}

thread_local! {
    static BOUNDS: RefCell<HashMap<SharedString, Bounds<Pixels>>> = RefCell::new(HashMap::new());
    static FLAGS: RefCell<HashMap<&'static str, bool>> = RefCell::new(HashMap::new());
    static TEXTS: RefCell<HashMap<&'static str, SharedString>> = RefCell::new(HashMap::new());
}

/// A child for an element (positioned `relative()`) whose bounds a test needs, when probes are on.
pub fn probe(key: impl Into<SharedString>) -> Option<impl IntoElement> {
    if !enabled() {
        return None;
    }
    let key = key.into();
    Some(
        canvas(
            move |bounds, _, _| {
                BOUNDS.with(|b| b.borrow_mut().insert(key.clone(), bounds));
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full(),
    )
}

/// Record a view's state as of its last render.
pub fn flag(key: &'static str, on: bool) {
    if enabled() {
        FLAGS.with(|f| f.borrow_mut().insert(key, on));
    }
}

/// The bounds `key` was last laid out at.
pub fn bounds(key: &str) -> Option<Bounds<Pixels>> {
    BOUNDS.with(|b| b.borrow().get(key).copied())
}

/// Record the text a view last drew under `key` (a label a test reads back).
pub fn text(key: &'static str, text: &SharedString) {
    if enabled() {
        TEXTS.with(|t| t.borrow_mut().insert(key, text.clone()));
    }
}

/// What [`text`] recorded under `key`.
pub fn text_of(key: &str) -> Option<SharedString> {
    TEXTS.with(|t| t.borrow().get(key).cloned())
}

pub fn flag_set(key: &str) -> bool {
    FLAGS.with(|f| f.borrow().get(key).copied().unwrap_or(false))
}
