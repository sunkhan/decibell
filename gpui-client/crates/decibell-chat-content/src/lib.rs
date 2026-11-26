//! What messages contain in the GPUI client (`gpui-client/CONVENTIONS.md`, "Chat content"):
//!
//! - [`pictures`]: the picture pipeline every picture of a message goes through (fetch, decode at
//!   the drawn size off the UI thread, byte-bounded caches, animations only while visible).
//! - [`attachments`], [`inflight`], [`embeds`]: the three row slots of `decibell-chat`.
//! - [`menu`], [`viewer`], [`transfers`]: the image right-click menu, the image viewer and the
//!   Transfers panel (overlays / the title bar).
//! - [`player`]: the seam the persistent audio / video players (another crate) plug into.

pub mod actions;
pub mod attachments;
pub mod downloads;
pub mod embeds;
pub mod inflight;
pub mod menu;
pub mod pictures;
pub mod player;
pub mod sizing;
pub mod style;
pub mod tile;
pub mod transfers;
pub mod viewer;

use std::rc::Rc;

use decibell_text::Engine;
use gpui::{App, AppContext as _, Global};

use crate::pictures::{Fetcher, Pictures};

struct Shared {
    engine: Rc<Engine>,
    fixture: bool,
}

impl Global for Shared {}

/// The text engine (taken once at `init`, so code with `&App` measures with it).
pub fn engine(cx: &App) -> Rc<Engine> {
    cx.global::<Shared>().engine.clone()
}

/// Screens and their fixtures (no attachment endpoints to wait for).
pub fn fixture_mode(cx: &App) -> bool {
    cx.try_global::<Shared>().is_some_and(|s| s.fixture)
}

/// Once at start-up (the app's `registry::init_features`): the picture pipeline on the core's
/// fetcher, the row slots, the image menu.
pub fn init(cx: &mut App) {
    install(decibell_unfurl_fetcher(), false, cx);
}

fn decibell_unfurl_fetcher() -> Rc<dyn Fetcher> {
    pictures::fetch::CoreFetcher::new(env!("CARGO_PKG_VERSION"))
}

/// Installs everything with `fetcher` (screens pass a memory one). Idempotent per fetcher.
pub fn install(fetcher: Rc<dyn Fetcher>, fixture: bool, cx: &mut App) {
    let engine = Engine::global(cx);
    cx.set_global(Shared { engine, fixture });
    Pictures::install(fetcher, cx);
    let inflight = Rc::new(inflight::InflightSlot::new(cx));
    let embeds = Rc::new(embeds::EmbedsSlot::new(cx));
    let slots = decibell_chat::slots(cx);
    slots.attachments = Some(Rc::new(attachments::AttachmentSlot::default()));
    slots.inflight = Some(inflight);
    slots.embeds = Some(embeds.clone());
    slots.text_hidden = Some(Rc::new(move |message, _, cx| embeds.text_hidden(message, cx)));
    decibell_shell::register_overlay(cx, |_, cx| cx.new(menu::MediaMenu::new).into());
    viewer::init(cx);
    transfers::init(cx);
}
