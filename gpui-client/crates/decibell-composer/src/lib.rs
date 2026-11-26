//! The message composer of the GPUI client (`gpui-client/CONVENTIONS.md`, "Composer"): what
//! `ChatPanel.tsx` / `DmChatPanel.tsx` draw under the message list, and the inline editor of a sent
//! message.
//!
//! - [`Composer`]: the card (input, live preview strip, reply bar, attachment chips, slowmode pill,
//!   the code / math trigger, the emoji / GIF picker, send), or the notice that replaces it. One per
//!   chat pane, generic over where it sends through [`ComposerTarget`] ([`ChannelTarget`],
//!   [`DmTarget`]).
//! - [`picker`]: the emoji picker (Twemoji grid, categories, search, recent) and its GIFs tab.
//! - [`gifs`]: GIF search on the core's runtime (KLIPY / GIPHY through `decibell-unfurl`), the key's
//!   whereabouts, and the GIF previews' image fetches.
//! - [`panel`]: the code block and math panels (`RichComposer.tsx`).
//! - [`editor`]: the inline editor the message list shows in place of an edited message.
//!
//! [`init`] registers the chat crate's `composer` and `editor` slots.

pub mod drop;
pub mod editor;
pub mod emoji_data;
pub mod gifs;
pub mod panel;
pub mod picker;
pub mod preview;
pub mod target;

mod chrome;
mod composer;
mod ink;
#[cfg(feature = "fixtures")]
mod screens;
#[cfg(feature = "fixtures")]
mod selftests;

pub use composer::{Composer, ComposerStats};
pub use target::{ChannelTarget, ComposerTarget, DmTarget, Variant};

use std::rc::Rc;

use gpui::{App, AppContext as _};

/// Once at start-up (the app's `registry::init_features`, after the chat crate's): the chat pane's
/// composer and the list's inline editor.
pub fn init(cx: &mut App) {
    let build: decibell_chat::ComposerBuilder = Rc::new(|server, channel, list, window, cx| {
        let target: Rc<dyn ComposerTarget> = Rc::new(ChannelTarget { server, channel });
        let list = list.clone();
        let composer = cx.new(|cx| Composer::new(target, Some(list), window, cx));
        decibell_chat::ComposerParts { view: composer.clone().into(), type_to_focus: Composer::type_to_focus_hook(&composer) }
    });
    decibell_chat::slots(cx).composer = Some(build);
    let engine = decibell_text::Engine::global(cx);
    let slot = Rc::new(editor::InlineEditorSlot::new(engine));
    cx.set_global(editor::EditorRegistry(slot.clone()));
    decibell_chat::slots(cx).editor = Some(slot);
}

/// The composer screens (with the `fixtures` feature).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The composer self-tests (with the `fixtures` feature): each starts the app again signed in to
/// the local stack and runs the check of the same name inside it.
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::selftests();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The checks `decibell --check composer-…` runs inside the signed-in app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    #[cfg(feature = "fixtures")]
    return selftests::checks();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}
