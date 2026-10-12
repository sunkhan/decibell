//! User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md):
//! the status menu and auto-away. Central saves the chosen status and fans
//! out what others may see; communities only learn "report me offline"
//! while the user is invisible.

use napi_derive::napi;

use crate::net::connection::build_packet;
use crate::net::proto::{
    packet, PresenceVisibility, SetIdleRequest, SetStatusMessageRequest, SetStatusRequest,
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

#[napi(object)]
pub struct SetStatusArgs {
    /// "online" | "away" | "dnd" | "invisible"
    pub status: String,
}

#[napi]
pub async fn set_status(args: SetStatusArgs) -> napi::Result<()> {
    let status = crate::presence::parse_status(&args.status)
        .ok_or_else(|| napi::Error::from_reason(format!("Unknown status: {}", args.status)))?;
    let hidden = status == crate::net::proto::PresenceStatus::PresenceInvisible;
    let was_hidden = crate::presence::set_hidden(hidden);

    send_to_central(
        packet::Type::SetStatusReq,
        packet::Payload::SetStatusReq(SetStatusRequest { status: status as i32 }),
    )
    .await;

    // Entering or leaving Invisible: every connected community stops (or
    // resumes) reporting us online. New connections carry it in their auth.
    if hidden != was_hidden {
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
                            Some(&c.jwt),
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
    Ok(())
}

#[napi(object)]
pub struct SetStatusMessageArgs {
    pub text: String,
    pub emoji: String,
    /// Seconds until central clears it; 0 = keep until changed.
    pub clear_after: u32,
}

/// The custom status message (docs/superpowers/specs/2026-10-12-status-
/// messages-design.md). Central normalises and saves it, and everyone sees
/// it with our presence. Empty text and emoji clears it.
#[napi]
pub async fn set_status_message(args: SetStatusMessageArgs) -> napi::Result<()> {
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
}

#[napi(object)]
pub struct SystemIdleArgs {
    pub threshold_seconds: u32,
}

/// Auto-away's idle source where Electron's powerMonitor reads nothing
/// (Linux Wayland — see system_idle.rs): "idle" | "active" | "locked", or
/// "unsupported" and the renderer asks powerMonitor instead.
#[napi]
pub async fn system_idle_state(args: SystemIdleArgs) -> napi::Result<String> {
    let threshold = args.threshold_seconds.max(1);
    tokio::task::spawn_blocking(move || crate::system_idle::state(threshold).to_string())
        .await
        .map_err(|e| napi::Error::from_reason(e.to_string()))
}

#[napi(object)]
pub struct SetIdleArgs {
    pub idle: bool,
}

/// Auto-away: no input for the user's idle threshold, a locked screen or
/// sleep (renderer-detected). Central turns an ONLINE user AWAY meanwhile.
#[napi]
pub async fn set_idle(args: SetIdleArgs) -> napi::Result<()> {
    if crate::presence::set_idle(args.idle) == args.idle {
        return Ok(());
    }
    send_to_central(
        packet::Type::SetIdleReq,
        packet::Payload::SetIdleReq(SetIdleRequest { idle: args.idle }),
    )
    .await;
    Ok(())
}
