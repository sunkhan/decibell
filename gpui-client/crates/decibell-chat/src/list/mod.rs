//! The message list: [`MessageList`] (the view and its state), the element that paints it, and
//! the arithmetic under both ([`core`]).

pub mod core;
mod element;
mod view;

pub use element::BarGeometry;
pub use view::{Caret, Drawn, ListEvent, ListStats, MessageList};
