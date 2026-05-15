//! Persistent DM commands. Three fire-and-emit napi calls:
//!
//!   - `request_dm_conversations()`: pulls conversation previews from
//!     central; response arrives as `dm_conversations_received`.
//!   - `request_dm_history(peer, before_id, limit)`: pulls one page
//!     of messages; response arrives as `dm_history_received`.
//!   - `mark_dm_read(peer, up_to_id)`: fire-and-forget read cursor
//!     update; no response.
//!
//! All three send through the central server's TCP/TLS connection.
//! Pattern mirrors the existing avatar request/response in
//! commands/auth.rs — the difference is that the response handling
//! is event-emit (not oneshot waiter) since the renderer is the
//! caller of last resort, not a pending future.

use crate::net::connection::build_packet;
use crate::net::proto::{packet, DmConversationsReq, DmDeleteReq, DmHistoryReq, DmMarkReadReq};
use crate::state;

#[napi]
pub async fn request_dm_conversations() -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmConversationsReq,
            packet::Payload::DmConversationsReq(DmConversationsReq {}),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct RequestDmHistoryArgs {
    pub peer: String,
    pub before_id: i64,
    pub limit: i32,
}

#[napi]
pub async fn request_dm_history(args: RequestDmHistoryArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmHistoryReq,
            packet::Payload::DmHistoryReq(DmHistoryReq {
                peer: args.peer,
                before_id: args.before_id,
                limit: args.limit,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct MarkDmReadArgs {
    pub peer: String,
    pub up_to_id: i64,
}

#[napi]
pub async fn mark_dm_read(args: MarkDmReadArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmMarkReadReq,
            packet::Payload::DmMarkReadReq(DmMarkReadReq {
                peer: args.peer,
                up_to_id: args.up_to_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await;
    Ok(())
}

#[napi(object)]
pub struct DeleteDmMessageArgs {
    pub peer: String,
    pub message_id: i64,
}

/// Sends DM_DELETE_REQ over the JWT-authed central session. The
/// ack arrives as the `dm_message_delete_responded` event; the
/// broadcast (if successful) arrives as `dm_message_deleted`. Both
/// land in useDmEvents on the renderer side.
#[napi]
pub async fn delete_dm_message(args: DeleteDmMessageArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmDeleteReq,
            packet::Payload::DmDeleteReq(DmDeleteReq {
                peer: args.peer,
                message_id: args.message_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}
