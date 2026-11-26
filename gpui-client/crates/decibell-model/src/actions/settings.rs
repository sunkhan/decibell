//! Saving the settings blob (`features/settings/saveSettings.ts`'s `doSave`).

use decibell_core::api;

/// [`crate::stores::settings::Settings`]' saver in the app: the whole blob to
/// `save_settings`, which preserves the saved sign-in and the codec toggles on disk.
/// Runs on the core's runtime, so a save flushed at quit doesn't need the UI thread.
pub fn save_blob(blob: serde_json::Value) {
    crate::core::spawn_detached(async move {
        if let Err(e) = api::settings::save_settings(blob).await {
            log::error!("[saveSettings] failed: {e}");
        }
    });
}
