//! Message search and the text channel header's search box (spec
//! `docs/superpowers/specs/2026-10-14-message-search-design.md`; `gpui-client/CONVENTIONS.md`,
//! "Search and channel header"):
//!
//! - [`SearchBox`] fills `Slot::HeaderSearch`: the box with its filter chips, the suggestion menu
//!   (`from:`, `in:`, `has:`, dates with the picker, "Inside words") and Ctrl+F.
//! - [`RightPanel`] fills `Slot::RightPanel`: the results panel while a search of the active
//!   community is open ([`ResultsView`]: count, sort, notices, windowed results, Jump), else the
//!   member list; below 1024 px the results float over the chat.
//! - [`tokens`], [`menu`], [`calendar`], [`highlight`]: the renderer's grammar and rules, pure and
//!   unit-tested (and checked against the real TypeScript: `renderer_grammar.json`).
//!
//! The search itself (the request, supersede, refusals and retries, paging, the drafts) is the
//! model's `SearchStore` (`decibell_model::chat::search`). Nothing about a search is stored on
//! the device.

pub mod calendar;
#[cfg(feature = "fixtures")]
pub mod fixture;
pub mod format;
pub mod highlight;
pub mod menu;
mod menu_view;
mod panel;
pub mod result_row;
mod results;
pub mod runs;
mod search_box;
#[cfg(feature = "fixtures")]
mod screens;
#[cfg(feature = "fixtures")]
mod selftests;
pub mod tokens;

pub use panel::RightPanel;
pub use results::{FLOATING_WIDTH, PANEL_WIDTH, ResultsView, is_narrow};
pub use search_box::{FocusSearch, SearchBox};

/// Once at start-up (the app's `registry::init_features`): the box in the header, the results in
/// the right panel, Ctrl+F (⌘F) from anywhere.
pub fn init(cx: &mut gpui::App) {
    use gpui::AppContext as _;
    struct Initialized;
    impl gpui::Global for Initialized {}
    if cx.has_global::<Initialized>() {
        return;
    }
    cx.set_global(Initialized);
    decibell_shell::register_pane(cx, decibell_shell::Slot::HeaderSearch, |window, cx| cx.new(|cx| SearchBox::new(window, cx)).into());
    decibell_shell::register_pane(cx, decibell_shell::Slot::RightPanel, |_, cx| cx.new(RightPanel::new).into());
    cx.bind_keys([gpui::KeyBinding::new("ctrl-f", FocusSearch, None), gpui::KeyBinding::new("cmd-f", FocusSearch, None)]);
    // The store asks the box that is drawn (a box mounts without taking the focus).
    cx.on_action(|_: &FocusSearch, cx| {
        if let Some(model) = cx.try_global::<decibell_model::Model>().cloned() {
            model.chat.search.update(cx, |s, cx| s.request_focus(cx));
        }
    });
}

/// The search screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The search self-tests (with the `fixtures` feature): each starts the app signed in to the
/// local stack in a child process.
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::selftests();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The checks `decibell --check <name>` runs inside the signed-in app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::checks();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}
