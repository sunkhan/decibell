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

/// Write a pending save and wait until it is on disk. For what the core reads from
/// `config.json` itself (the devices of a voice session or a call, when their engine
/// starts): [`save_blob`] only queues the write, so a device chosen inside the save
/// debounce would lose the race against the join that follows.
pub(crate) async fn flush_saved(model: &crate::Model, cx: &mut gpui::AsyncApp) {
    if !crate::core::is_running() {
        cx.update(|cx| model.settings.update(cx, |s, _| s.flush()));
        return;
    }
    let pending = cx.update(|cx| model.settings.update(cx, |s, _| s.take_pending_save()));
    if let Some(blob) = pending
        && let Err(e) = api::settings::save_settings(blob).await
    {
        log::error!("[saveSettings] failed: {e}");
    }
}
