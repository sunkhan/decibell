//! Global hotkeys + the push-to-talk input mode. The listeners live in
//! `crate::hotkeys`; the gate the hold actions drive is
//! `media::voice_gate`.

use crate::api::CoreError;
use crate::hotkeys::{self, Action, Binding};

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct HotkeyBindingArg {
    pub id: String,
    /// `toggle_mute` | `toggle_deafen` | `push_to_talk` | `push_to_mute`
    /// | `leave_voice` | `answer_call` | `decline_call`
    pub action: String,
    /// DOM `KeyboardEvent.code`s, generic `Control`/`Shift`/`Alt`/`Meta`,
    /// `Mouse3`/`Mouse4`/`Mouse5`.
    pub keys: Vec<String>,
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct HotkeysConfigureArgs {
    pub bindings: Vec<HotkeyBindingArg>,
}

fn status_json() -> Result<serde_json::Value, CoreError> {
    serde_json::to_value(hotkeys::status())
        .map_err(|e| CoreError::other(format!("hotkeys status: {}", e)))
}

/// Replace the bindings and (re)start this platform's listener. Unknown
/// actions are skipped so a config from a newer build can't break it.
/// Status changes arrive as `hotkeys_status` events only — a snapshot
/// returned here could land after a newer event and roll it back.
pub async fn hotkeys_configure(args: HotkeysConfigureArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let bindings: Vec<Binding> = args
            .bindings
            .into_iter()
            .filter_map(|b| {
                Some(Binding {
                    id: b.id,
                    action: Action::parse(&b.action)?,
                    keys: b.keys,
                })
            })
            .collect();
        // Starting a listener can block briefly (X11 connect, Windows
        // window class registration).
        crate::rt::spawn_blocking(move || hotkeys::configure(bindings))
            .await
            .map_err(|e| CoreError::other(format!("hotkeys configure: {}", e)))
    })
    .await
}

pub fn hotkeys_get_status() -> Result<serde_json::Value, CoreError> {
    status_json()
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct HotkeysSetPausedArgs {
    pub paused: bool,
}

/// Swallow presses while the Keybinds tab records a combo, so recording
/// Ctrl+M doesn't also toggle mute.
pub fn hotkeys_set_paused(args: HotkeysSetPausedArgs) {
    hotkeys::set_paused(args.paused);
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct HotkeysInjectArgs {
    pub action: String,
    pub pressed: bool,
    /// What is held: a binding id (renderer focused fallback, macOS
    /// accelerators) or `cli`.
    pub source: String,
}

/// A press / release from outside the native listeners: the renderer's
/// focused fallback, macOS accelerators, `decibell --hotkey=…`.
pub fn hotkeys_inject(args: HotkeysInjectArgs) -> Result<(), CoreError> {
    let action = Action::parse(&args.action)
        .ok_or_else(|| CoreError::invalid(format!("Unknown hotkey action: {}", args.action)))?;
    hotkeys::dispatch(&args.source, action, args.pressed);
    Ok(())
}

/// Open the desktop's own shortcut editor (GlobalShortcuts portal v2).
pub async fn hotkeys_open_system_settings() -> Result<(), CoreError> {
    crate::rt::run(async move {
        hotkeys::open_system_settings().map_err(CoreError::other)
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SetInputModeArgs {
    pub push_to_talk: bool,
    pub release_delay_ms: u32,
}

/// Voice activity vs push-to-talk. Process-wide: applies to the current
/// and every later voice session / DM call.
pub fn set_input_mode(args: SetInputModeArgs) {
    crate::media::voice_gate::set_mode(args.push_to_talk, args.release_delay_ms);
}
