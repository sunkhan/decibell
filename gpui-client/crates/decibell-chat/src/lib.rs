//! The chat views of the GPUI client (`gpui-client/CONVENTIONS.md`, "Chat"):
//!
//! - [`MessageList`]: the message list of one slice of the model: an exact-height custom element
//!   anchored on the row being read, selection across rows, Chromium's wheel and key scrolling.
//! - [`ChatPane`]: a server text channel's pane around it (loading, retry, the welcome banner,
//!   the composer slot, the delete confirmation).
//! - [`slots`] / [`RowSlot`] / [`EditorSlot`]: what other crates fill in a row (attachments,
//!   link embeds, the inline editor) and under the list (the composer).
//! - [`ListSource`]: "a slice and a few capabilities": what makes the same list a channel's or
//!   a DM's.

pub mod bench;
#[cfg(feature = "fixtures")]
pub mod fixture;
pub mod list;
mod live;
pub mod media;
mod pane;
pub mod row;
#[cfg(feature = "fixtures")]
mod screens;
#[cfg(feature = "fixtures")]
mod selftests;
pub mod source;
pub mod style;
#[cfg(test)]
pub(crate) mod testing;

pub use list::{ListEvent, ListStats, MessageList};
pub use media::{
    CancelEdit, ChatSlots, ComposerBuilder, ComposerParts, EditorSlot, RowSlot, SlotSpace, SubmitEdit, TextHidden, TypeToFocus, WelcomeBuilder,
    invalidate_slots, slots,
};
pub use pane::ChatPane;
pub use row::RowNotice;
pub use source::{ChannelSource, Jump, ListSource};

/// Once at start-up (the app's `registry::init_features`): the chat pane fills the shell's
/// `Slot::Chat` and follows the navigation's active text channel.
pub fn init(cx: &mut gpui::App) {
    use gpui::AppContext as _;
    decibell_shell::register_pane(cx, decibell_shell::Slot::Chat, |window, cx| cx.new(|cx| ChatPane::for_active_channel(window, cx)).into());
}

/// The message list of the chat pane that was shown last, while it lives (search's jump checks it
/// lands; the live benchmark drives it).
pub fn shown_list(cx: &gpui::App) -> Option<gpui::Entity<MessageList>> {
    cx.try_global::<pane::ShownList>().and_then(|shown| shown.0.upgrade())
}

/// The chat screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The checks `decibell --check <name>` runs inside the signed-in app (`chat-live`, `chat-copy`).
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    live::checks()
}

/// The chat self-tests (with the `fixtures` feature).
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::selftests();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}
