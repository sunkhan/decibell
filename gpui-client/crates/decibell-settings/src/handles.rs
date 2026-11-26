//! Where the overlays are, for the self-tests: they set the forms' fields (until the text editor
//! lands) and read what the views hold. Weak handles, set as each overlay is built.

use gpui::{App, Global, WeakEntity};

use crate::dialogs::passphrase::PassphraseOverlay;
use crate::dialogs::password::PasswordOverlay;
use crate::window::SettingsOverlay;

#[derive(Default)]
pub struct Handles {
    pub settings: Option<WeakEntity<SettingsOverlay>>,
    pub password: Option<WeakEntity<PasswordOverlay>>,
    pub passphrase: Option<WeakEntity<PassphraseOverlay>>,
}

impl Global for Handles {}

pub(crate) fn update(cx: &mut App, f: impl FnOnce(&mut Handles)) {
    f(cx.default_global::<Handles>());
}

pub fn settings(cx: &App) -> Option<gpui::Entity<SettingsOverlay>> {
    cx.try_global::<Handles>()?.settings.as_ref()?.upgrade()
}

pub fn password(cx: &App) -> Option<gpui::Entity<PasswordOverlay>> {
    cx.try_global::<Handles>()?.password.as_ref()?.upgrade()
}

pub fn passphrase(cx: &App) -> Option<gpui::Entity<PassphraseOverlay>> {
    cx.try_global::<Handles>()?.passphrase.as_ref()?.upgrade()
}
