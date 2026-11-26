//! Where the Settings window is, for the self-tests: they read what its open tab holds. A weak
//! handle, set as the overlay is built.

use gpui::{App, Global, WeakEntity};

use crate::window::SettingsOverlay;

#[derive(Default)]
pub struct Handles {
    pub settings: Option<WeakEntity<SettingsOverlay>>,
}

impl Global for Handles {}

pub(crate) fn update(cx: &mut App, f: impl FnOnce(&mut Handles)) {
    f(cx.default_global::<Handles>());
}

pub fn settings(cx: &App) -> Option<gpui::Entity<SettingsOverlay>> {
    cx.try_global::<Handles>()?.settings.as_ref()?.upgrade()
}
