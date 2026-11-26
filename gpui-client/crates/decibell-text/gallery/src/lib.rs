//! Screens for `decibell-text`: `text-roles` (labels in every theme role, size, weight and
//! spacing, with truncation and line clamp), `rich-text` (fixture messages at 320, 560
//! and 900 px, with a working selection across rows) and `inputs` (every field, editable).
//! Not part of the product.
//!
//! Capture them (`rich-text` needs a wide window to show all three columns):
//!
//! ```sh
//! cargo run -p decibell-app --features capture --bin decibell-capture -- \
//!     --screen text-roles --all-themes --out-dir target/captures
//! cargo run -p decibell-app --features capture --bin decibell-capture -- \
//!     --screen rich-text --size 1900x1500 --all-themes --out-dir target/captures
//! ```

mod input_tests;
mod inputs;
mod rich;
mod roles;

use decibell_harness::{Screen, SelfTest};
use gpui::AppContext as _;

pub fn screens() -> Vec<Screen> {
    vec![
        Screen {
            name: "text-roles",
            about: "decibell-text labels: every font role, sizes, weights, letter-spacing, truncation, clamp",
            build: |_, cx| cx.new(|_| roles::TextRoles).into(),
        },
        Screen {
            name: "rich-text",
            about: "decibell-text rich text: fixture messages at 320 / 560 / 900 px, selectable across rows",
            build: |_, cx| cx.new(|_| rich::RichGallery::default()).into(),
        },
        Screen {
            name: "inputs",
            about: "decibell-text inputs: single line, password, rules, text area, the composer's input with emoji atoms",
            build: |window, cx| {
                let view = cx.new(|cx| inputs::Inputs::new(window, cx));
                // The selection shows in a focused field only.
                let selected = view.read(cx).selected.clone();
                selected.update(cx, |state, cx| {
                    let selection = state.selection();
                    state.focus(window, cx);
                    state.select(selection, cx);
                });
                view.into()
            },
        },
    ]
}

/// The inputs' self-tests (`decibell --selftest input-typing`, …).
pub fn selftests() -> Vec<SelfTest> {
    input_tests::selftests()
}
