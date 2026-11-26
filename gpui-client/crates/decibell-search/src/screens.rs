//! The search screens, drawn from [`crate::fixture`] (no network, no clock): the signed-in shell on
//! Daily Gaming with the header, the box and the results in each state.
//! `decibell-capture --screen search-results --all-themes`; `search-results-narrow` is captured at
//! `--size 900x700` (the panel floats over the chat).

use decibell_harness::Screen;
use decibell_shell::Shell;
use decibell_ui::AssumeClientDecorations;
use gpui::{AnyView, App, AppContext as _, Window};

use crate::fixture::{self, Stage};

fn build(stage: Stage, cx: &mut App) -> AnyView {
    // A capture renders on a clock that does not run: entrances show their end.
    if cx.has_global::<AssumeClientDecorations>() {
        cx.set_reduce_motion(true);
    }
    fixture::apply(stage, cx);
    cx.new(Shell::new).into()
}

macro_rules! screen {
    ($name:literal, $about:literal, $stage:expr) => {
        Screen { name: $name, about: $about, build: |_: &mut Window, cx: &mut App| build($stage, cx) }
    };
}

pub fn screens() -> Vec<Screen> {
    vec![
        screen!("header-text", "#general's header: name, topic, the members toggle, the search box over the members column", Stage::HeaderText),
        screen!("header-encrypted", "#vault's header: the E2EE badge", Stage::HeaderEncrypted),
        screen!("header-long-topic", "#general with a topic longer than the header: cut with an ellipsis", Stage::HeaderLongTopic),
        screen!("search-box-empty", "The search box focused, empty: the filters and \"Inside words\"", Stage::BoxEmpty),
        screen!("search-box-suggest", "\"gg\" typed: \"Search for gg\", then the filters", Stage::BoxSuggest),
        screen!("search-box-suggest-from", "from:perf: the members it names", Stage::BoxSuggestFrom),
        screen!("search-box-suggest-in", "in: the text channels, this one first", Stage::BoxSuggestIn),
        screen!("search-box-suggest-has", "has: the six kinds", Stage::BoxSuggestHas),
        screen!("search-box-chips", "Four chips and \"gg\" in the box (not focused)", Stage::BoxChips),
        screen!("date-picker", "before: Today / Yesterday and the month grid (today pinned to 2026-11-26)", Stage::DatePicker),
        screen!("search-results", "\"wayland\": 10,000+ results over two channels, #vault's text skipped, the words marked", Stage::Results),
        screen!("search-results-empty", "No results", Stage::ResultsEmpty),
        screen!("search-results-refused", "The server refused the search: the error and Try again", Stage::ResultsRefused),
        screen!("search-results-narrow", "search-results below 1024 px (capture at --size 900x700): the panel floats over the chat", Stage::Results),
    ]
}
