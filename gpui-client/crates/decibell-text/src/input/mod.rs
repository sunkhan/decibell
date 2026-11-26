//! Text input: the editing engine and the two fields built on it. See the README's "Inputs"
//! section for the API with examples.
//!
//! ```text
//! boundaries   grapheme and word stops, UTF-16 offsets          (pure)
//! history      undo / redo, grouped as Chromium groups them     (pure)
//! buffer       Buffer: text + selection + rules + atoms + IME   (pure)
//! keymap       keystroke → editing command                      (pure)
//! display      one Layout per hard line; positions, painting
//! state        InputState: the entity (focus, blink, IME, mouse, clipboard)
//! element      what it draws; TextInput / TextArea
//! ```

pub mod boundaries;
mod buffer;
mod display;
mod element;
mod history;
mod keymap;
mod state;
#[cfg(all(test, any(target_os = "linux", target_os = "freebsd")))]
mod tests;

pub use buffer::{AtomSpan, Buffer, Motion, Rules, numeric};
pub use display::{DisplayStats, InputStyle};
pub use element::{TextArea, TextInput};
pub use history::{EditKind, Selection};
pub use keymap::{Command, Move, command};
pub use state::{BLINK_INTERVAL, Enter, InputEvent, InputState, KeyHook, text_field_focused};
