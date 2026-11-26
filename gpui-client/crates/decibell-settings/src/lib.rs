//! Settings (`features/settings/`): the window (`SettingsModal.tsx` on the kit's
//! `SettingsWindow` frame), its nine tabs, the rename and change-password dialogs, and the E2EE
//! passphrase dialog (`features/e2ee/PassphraseModal.tsx`), which any feature opens through the
//! model (`model.live.e2ee.open(mode)`).
//!
//! - [`init`]: registers the three overlays with the shell. Call once, after `Model::init`.
//! - [`hooks`]: what the app supplies — its version, the updater's buttons, the out-of-voice mic
//!   test ([`mic_test::MicTest`]) — and the seams other features fill (the picture editor:
//!   [`hooks::PictureRequests`]).
//! - Each tab is its own cached view (`tabs/`), built when it shows and dropped when it goes,
//!   holding a [`decibell_common::Watch`] of exactly what it reads.
//!
//! The text fields draw their own shells (`fields.rs`) until `decibell-text`'s editor lands: the
//! values are the views' state, set by the self-tests through the views' API.

pub mod dialogs;
mod fields;
pub mod handles;
pub mod hooks;
pub mod mic_test;
mod recorder;
mod rows;
pub mod tabs;
mod window;

#[cfg(feature = "fixtures")]
pub mod fixture;
#[cfg(feature = "fixtures")]
mod screens;
mod selftests;

use gpui::{App, AppContext as _, Global};

struct Initialised;

impl Global for Initialised {}

pub use hooks::{PictureEvent, PictureRequests, SettingsHooks, UpdateHooks};
pub use mic_test::{LevelSink, MicTest, MicTestError, UnavailableMicTest};
pub use tabs::TabId;
pub use window::SettingsOverlay;

/// Once, after `Model::init`, before the main window opens: the Settings window, the
/// change-password dialog and the passphrase dialog become overlays of the main layout (in that
/// order, so the dialogs draw over the window).
pub fn init(cx: &mut App) {
    // Once: a screen built in the app (`--screen`) finds it done already.
    if cx.has_global::<Initialised>() {
        return;
    }
    cx.set_global(Initialised);
    hooks::ensure(cx);
    // The attachment speed caps follow the settings (loaded, or changed in Network) into the
    // attachment service. The model creates the service lazily, at the first upload or download,
    // without them (the model applies none at creation): so they are also handed over when the
    // composer or the transfers store first shows a transfer while a service runs that hasn't
    // had them. Event-driven: no timer.
    if let Some(model) = cx.try_global::<decibell_model::Model>().cloned() {
        use std::cell::Cell;
        use std::rc::Rc;
        let applied: Rc<Cell<Option<(usize, u64, u64)>>> = Rc::default();
        let sync = {
            let applied = applied.clone();
            move |cx: &mut App| {
                let Some(service) = decibell_model::chat::CoreAttachmentService::attachments() else { return };
                let s = decibell_model::Model::global(cx).settings.read(cx).get();
                let key = (std::sync::Arc::as_ptr(&service) as usize, s.upload_limit_bps, s.download_limit_bps);
                if applied.get() != Some(key) {
                    applied.set(Some(key));
                    tabs::network::apply_limits(key.1, key.2);
                }
            }
        };
        let sync = Rc::new(sync);
        let (a, b, c) = (sync.clone(), sync.clone(), sync);
        cx.observe(&model.settings, move |_, cx| a(cx)).detach();
        cx.observe(&model.chat.composer, move |_, cx| b(cx)).detach();
        cx.observe(&model.live.transfers, move |_, cx| c(cx)).detach();
    }
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| SettingsOverlay::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| cx.new(|cx| dialogs::password::PasswordOverlay::new(window, cx)).into());
    decibell_shell::register_overlay(cx, |window, cx| {
        cx.new(|cx| dialogs::passphrase::PassphraseOverlay::new(window, cx)).into()
    });
}

/// The screens of the Settings window, its tabs and dialogs, drawn from the fixture (feature
/// `fixtures`).
pub fn screens() -> Vec<decibell_harness::Screen> {
    #[cfg(feature = "fixtures")]
    return screens::screens();
    #[cfg(not(feature = "fixtures"))]
    Vec::new()
}

/// The self-tests: each starts the app again as a child (`--check`), signed in to the local stack.
pub fn selftests() -> Vec<decibell_harness::SelfTest> {
    selftests::selftests()
}

/// The checks `decibell --check <name>` runs inside the signed-in app.
pub fn checks() -> Vec<decibell_harness::SelfTest> {
    selftests::checks()
}
