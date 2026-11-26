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
//! [`init`] registers the chat crate's `composer` and `editor` slots; [`dm_composer`] is what the
//! app hands the DM pane (`decibell_home::dm::set_dm_composer`: the crates don't depend on each
//! other).

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

/// The DM conversation's composer (`DmChatPanel`'s input bar): a [`Composer`] on a [`DmTarget`]
/// under the conversation's list, and its "type anywhere" hook. The DM pane draws the view
/// uncached (the card sizes itself).
pub fn dm_composer(peer: decibell_model::Uid, list: gpui::Entity<decibell_chat::MessageList>, window: &mut gpui::Window, cx: &mut App) -> decibell_chat::ComposerParts {
    let target: Rc<dyn ComposerTarget> = Rc::new(DmTarget { peer });
    let composer = cx.new(|cx| Composer::new(target, Some(list), window, cx));
    decibell_chat::ComposerParts { view: composer.clone().into(), type_to_focus: Composer::type_to_focus_hook(&composer) }
}

/// "Type anywhere to compose" (`useTypeToFocusComposer`), for the window's root view: call it from
/// the root element's key-down listener in the bubble phase (the renderer's `keydown` listener on
/// `window`, which runs after every element on the focused path: one that handles the key and
/// stops it keeps it). It hands the key to the composer on screen (the one mounted last, while it
/// lives and its host draws it) when no text field has the focus (`text_field_focused`: the
/// renderer's `INPUT` / `TEXTAREA` / contentEditable): a printable character (its input takes the
/// focus and the character) or ArrowUp (edit our newest message). Not with Ctrl, Alt or Super
/// held, a modal open, the composer replaced by a notice or hidden (the DM call's theater), nor
/// for a key the focused hotkey fallback takes (`Hotkeys::would_take`: that key is the hotkey's,
/// `docs/electron-quirks.md` #17). Returns whether it took the key (then stop the event).
///
/// The app's root holds the focus whenever nothing else does (`decibell-app`'s `Root`), so every
/// key reaches its listeners; keys that reach a chat pane from its list go through the pane's
/// own listener first (the same rule, `Composer::type_to_focus_hook`).
pub fn type_to_focus(event: &gpui::KeyDownEvent, window: &mut gpui::Window, cx: &mut App) -> bool {
    let Some(composer) = Composer::active(cx) else { return false };
    composer.update(cx, |composer, cx| composer.key_from_pane(event, window, cx))
}

/// The status dialog's pick: the chosen emoji.
pub type StatusPick = Rc<dyn Fn(gpui::SharedString, &mut gpui::Window, &mut App)>;

/// The custom status dialog's emoji picker (`CustomStatusModal`'s `EmojiPicker` without
/// `onSendGif`: the emoji tab alone): what the app hands `decibell_home::status::set_emoji_picker`.
/// `anchor` is the dialog's emoji button (a press on it toggles, not closes). The dialog places
/// the view (352 × 420) and drops it when it closes the picker; its images go with it.
pub fn status_emoji_picker(
    on_pick: StatusPick,
    on_close: picker::OnPickerClose,
    anchor: Rc<std::cell::Cell<gpui::Bounds<gpui::Pixels>>>,
    window: &mut gpui::Window,
    cx: &mut App,
) -> gpui::AnyView {
    let pick: picker::OnEmoji = Rc::new(move |native, window, cx| on_pick(native.into(), window, cx));
    cx.new(|cx| picker::Picker::emoji_only(pick, on_close, anchor, window, cx)).into()
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
