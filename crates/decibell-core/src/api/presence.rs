//! User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md):
//! the status menu and auto-away. Central saves the chosen status and fans
//! out what others may see; communities only learn "report me offline"
//! while the user is invisible. Invisible is the account's (multi-device):
//! this device's community connections follow it (`apply_hidden`, which
//! central's router calls when another device changes it).

use crate::api::CoreError;
use crate::net::connection::build_packet;
use crate::net::proto::{
    packet, PresenceSubscribe, PresenceVisibility, SetIdleRequest, SetStatusMessageRequest,
    SetStatusRequest,
};
use crate::state;

/// Queue one packet on the central connection (no-op when disconnected —
/// central reads the saved status at the next login, and idle is re-sent).
async fn send_to_central(ty: packet::Type, payload: packet::Payload) {
    let state_arc = state::shared();
    let (tx, data) = {
        let s = state_arc.lock().await;
        let Some(tx) = s.central.as_ref().and_then(|c| c.connection_write_tx()) else {
            return;
        };
        (tx, build_packet(ty, payload, s.token.as_deref()))
    };
    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await;
}

/// Most accounts central watches for one connection (SessionManager::MAX_WATCH).
const MAX_WATCH: usize = 5000;

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct PresenceSubscribeArgs {
    /// Every account the renderer is showing; replaces the previous set.
    pub uids: Vec<i64>,
}

/// Presence by subscription (docs/superpowers/specs/2026-10-15-central-
/// scaling-design.md): central sends statuses and profile changes only for
/// these accounts. Kept here and re-sent after every central login.
pub async fn presence_subscribe(args: PresenceSubscribeArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let mut uids: Vec<i64> = args.uids.into_iter().filter(|u| *u > 0).collect();
        uids.truncate(MAX_WATCH);
        let state_arc = state::shared();
        let (tx, data) = {
            let mut s = state_arc.lock().await;
            if s.presence_watch == uids {
                return Ok(());
            }
            s.presence_watch = uids.clone();
            let (Some(tx), Some(token)) = (s.central.as_ref().and_then(|c| c.connection_write_tx()), s.token.as_deref())
            else {
                return Ok(());  // sent with the next login
            };
            (
                tx,
                build_packet(
                    packet::Type::PresenceSubscribe,
                    packet::Payload::PresenceSubscribe(PresenceSubscribe { uids }),
                    Some(token),
                ),
            )
        };
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await;
        Ok(())
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SetStatusArgs {
    /// "online" | "away" | "dnd" | "invisible"
    pub status: String,
}

pub async fn set_status(args: SetStatusArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        let status = crate::presence::parse_status(&args.status)
            .ok_or_else(|| CoreError::invalid(format!("Unknown status: {}", args.status)))?;
        let hidden = status == crate::net::proto::PresenceStatus::PresenceInvisible;
        let was_hidden = crate::presence::set_hidden(hidden);

        send_to_central(
            packet::Type::SetStatusReq,
            packet::Payload::SetStatusReq(SetStatusRequest { status: status as i32 }),
        )
        .await;

        if hidden != was_hidden {
            crate::api::presence::tell_communities_hidden(hidden).await;
        }
        Ok(())
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SetStatusMessageArgs {
    pub text: String,
    pub emoji: String,
    /// Seconds until central clears it; 0 = keep until changed.
    pub clear_after: u32,
}

/// The custom status message (docs/superpowers/specs/2026-10-12-status-
/// messages-design.md). Central normalises and saves it, and everyone sees
/// it with our presence. Empty text and emoji clears it.
pub async fn set_status_message(args: SetStatusMessageArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        send_to_central(
            packet::Type::SetStatusMessageReq,
            packet::Payload::SetStatusMessageReq(SetStatusMessageRequest {
                text: args.text,
                emoji: args.emoji,
                clear_after: i64::from(args.clear_after),
            }),
        )
        .await;
        Ok(())
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SystemIdleArgs {
    pub threshold_seconds: u32,
}

/// Auto-away's idle source where Electron's powerMonitor reads nothing
/// (Linux Wayland — see system_idle.rs): "idle" | "active" | "locked", or
/// "unsupported" and the renderer asks powerMonitor instead.
pub async fn system_idle_state(args: SystemIdleArgs) -> Result<String, CoreError> {
    crate::rt::run(async move {
        let threshold = args.threshold_seconds.max(1);
        crate::rt::spawn_blocking(move || crate::system_idle::state(threshold).to_string())
            .await
            .map_err(|e| CoreError::other(e.to_string()))
    })
    .await
}

#[cfg_attr(feature = "napi", napi_derive::napi(object))]
pub struct SetIdleArgs {
    pub idle: bool,
}

/// Auto-away: no input for the user's idle threshold, a locked screen or
/// sleep (renderer-detected). Central turns an ONLINE user AWAY meanwhile.
pub async fn set_idle(args: SetIdleArgs) -> Result<(), CoreError> {
    crate::rt::run(async move {
        if crate::presence::set_idle(args.idle) == args.idle {
            return Ok(());
        }
        send_to_central(
            packet::Type::SetIdleReq,
            packet::Payload::SetIdleReq(SetIdleRequest { idle: args.idle }),
        )
        .await;
        Ok(())
    })
    .await
}

/// Another device of this account changed its status (OWN_PRESENCE,
/// multi-device): Invisible is the account's, so this device's communities
/// follow it too.
pub(crate) async fn apply_hidden(hidden: bool) {
    if crate::presence::set_hidden(hidden) != hidden {
        tell_communities_hidden(hidden).await;
    }
}

/// Entering or leaving Invisible: every connected community stops (or
/// resumes) reporting us online. New connections carry it in their auth.
pub(crate) async fn tell_communities_hidden(hidden: bool) {
    let state_arc = state::shared();
    let sends: Vec<_> = {
        let s = state_arc.lock().await;
        s.communities
            .values()
            .filter_map(|c| {
                c.connection_write_tx().map(|tx| {
                    let data = build_packet(
                        packet::Type::PresenceVisibility,
                        packet::Payload::PresenceVisibility(PresenceVisibility { hidden }),
                        None,
                    );
                    (tx, data)
                })
            })
            .collect()
    };
    for (tx, data) in sends {
        let _ = tokio::time::timeout(std::time::Duration::from_secs(5), tx.send(data)).await;
    }
}
