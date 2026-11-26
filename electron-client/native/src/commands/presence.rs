//! `#[napi]` wrappers over `decibell_core::api::presence`, where the bodies
//! live. Doc comments stay here: napi copies them into index.d.ts.

use crate::api;
use crate::api::presence::{
    PresenceSubscribeArgs, SetIdleArgs, SetStatusArgs, SetStatusMessageArgs, SystemIdleArgs,
};

/// Presence by subscription (docs/superpowers/specs/2026-10-15-central-
/// scaling-design.md): central sends statuses and profile changes only for
/// these accounts. Kept here and re-sent after every central login.
#[napi]
pub async fn presence_subscribe(args: PresenceSubscribeArgs) -> napi::Result<()> {
    api::presence::presence_subscribe(args)
        .await
        .map_err(napi::Error::from)
}

#[napi]
pub async fn set_status(args: SetStatusArgs) -> napi::Result<()> {
    api::presence::set_status(args)
        .await
        .map_err(napi::Error::from)
}

/// The custom status message (docs/superpowers/specs/2026-10-12-status-
/// messages-design.md). Central normalises and saves it, and everyone sees
/// it with our presence. Empty text and emoji clears it.
#[napi]
pub async fn set_status_message(args: SetStatusMessageArgs) -> napi::Result<()> {
    api::presence::set_status_message(args)
        .await
        .map_err(napi::Error::from)
}

/// Auto-away's idle source where Electron's powerMonitor reads nothing
/// (Linux Wayland — see system_idle.rs): "idle" | "active" | "locked", or
/// "unsupported" and the renderer asks powerMonitor instead.
#[napi]
pub async fn system_idle_state(args: SystemIdleArgs) -> napi::Result<String> {
    api::presence::system_idle_state(args)
        .await
        .map_err(napi::Error::from)
}

/// Auto-away: no input for the user's idle threshold, a locked screen or
/// sleep (renderer-detected). Central turns an ONLINE user AWAY meanwhile.
#[napi]
pub async fn set_idle(args: SetIdleArgs) -> napi::Result<()> {
    api::presence::set_idle(args)
        .await
        .map_err(napi::Error::from)
}
