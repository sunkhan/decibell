//! Text input: the editing engine and the two fields built on it. See the README's "Inputs"
//! section for the API with examples.

pub mod boundaries;
mod buffer;
mod history;

pub use buffer::{AtomSpan, Buffer, Motion, Rules, numeric};
pub use history::{EditKind, Selection};
