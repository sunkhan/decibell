//! `#[napi]` wrappers over `decibell_core::api::hotkeys`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::hotkeys::{
    HotkeysConfigureArgs, HotkeysInjectArgs, HotkeysSetPausedArgs, SetInputModeArgs,
};

/// Replace the bindings and (re)start this platform's listener. Unknown
/// actions are skipped so a config from a newer build can't break it.
/// Status changes arrive as `hotkeys_status` events only — a snapshot
/// returned here could land after a newer event and roll it back.
#[napi]
pub async fn hotkeys_configure(args: HotkeysConfigureArgs) -> napi::Result<()> {
    api::hotkeys::hotkeys_configure(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub fn hotkeys_get_status() -> napi::Result<serde_json::Value> {
    api::hotkeys::hotkeys_get_status().map_err(napi::Error::from)
}

/// Swallow presses while the Keybinds tab records a combo, so recording
/// Ctrl+M doesn't also toggle mute.
#[napi]
pub fn hotkeys_set_paused(args: HotkeysSetPausedArgs) {
    api::hotkeys::hotkeys_set_paused(args)
}

/// A press / release from outside the native listeners: the renderer's
/// focused fallback, macOS accelerators, `decibell --hotkey=…`.
#[napi]
pub fn hotkeys_inject(args: HotkeysInjectArgs) -> napi::Result<()> {
    api::hotkeys::hotkeys_inject(args).map_err(napi::Error::from)
}

/// Open the desktop's own shortcut editor (GlobalShortcuts portal v2).
#[napi]
pub async fn hotkeys_open_system_settings() -> napi::Result<()> {
    api::hotkeys::hotkeys_open_system_settings()
        .await
        .map_err(napi::Error::from)
}

/// Voice activity vs push-to-talk. Process-wide: applies to the current
/// and every later voice session / DM call.
#[napi]
pub fn set_input_mode(args: SetInputModeArgs) {
    api::hotkeys::set_input_mode(args)
}
