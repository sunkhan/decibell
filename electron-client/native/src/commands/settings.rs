//! `#[napi]` wrappers over `decibell_core::api::settings`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::settings::{AudioDeviceList, SetDmPrivacyArgs};

/// Read the persisted config blob from disk. Returns
/// `{ saved_username, settings }` as a JSON value so the renderer can
/// hydrate its zustand stores without us having to re-declare the
/// whole AppSettings shape as a #[napi(object)] mirror (it has a
/// HashMap field which napi(object) doesn't support natively).
/// The saved password never leaves native: `saved_username` set means
/// "call `auto_login`".
#[napi]
pub async fn load_config() -> napi::Result<serde_json::Value> {
    api::settings::load_config()
        .await
        .map_err(napi::Error::from)
}

/// Persist a complete AppSettings blob to disk, preserving any
/// existing credentials. Renderer is the source of truth for the
/// settings shape; native just deserializes via serde and writes.
/// Mirrors tauri-client's save_settings: settings come from the
/// renderer as an object, we round-trip through serde_json::Value
/// into AppSettings.
#[napi]
pub async fn save_settings(settings: serde_json::Value) -> napi::Result<()> {
    api::settings::save_settings(settings)
        .await
        .map_err(napi::Error::from)
}

/// Tell the central server whether to deliver DMs from non-friends.
/// Mirrors the tauri command of the same name (defined there in
/// commands/messaging.rs). Lives in settings.rs here because the DM
/// command surface hasn't ported yet — when it does, this can move.
#[napi]
pub async fn set_dm_privacy(args: SetDmPrivacyArgs) -> napi::Result<()> {
    api::settings::set_dm_privacy(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn list_audio_devices() -> napi::Result<AudioDeviceList> {
    api::settings::list_audio_devices()
        .await
        .map_err(napi::Error::from)
}
