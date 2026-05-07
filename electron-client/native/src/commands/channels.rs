//! Channel-scoped commands: text-message send, history paging,
//! retention edit, and owner-only history wipe.
//!
//! Each command builds and queues a single TCP packet for the matching
//! community connection. Responses arrive asynchronously as bus events
//! (`message_received`, `channel_history_received`,
//! `channel_pruned`, `channel_updated`, `channel_wipe_responded`,
//! `channel_wiped`) — see net/community.rs::route_packets.

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state;

async fn send_for_server(
    server_id: &str,
    pkt_type: packet::Type,
    payload: packet::Payload,
) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let client = s.communities.get(server_id).ok_or_else(|| {
            napi::Error::from_reason(format!("Not connected to community {}", server_id))
        })?;
        let tx = client
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Community connection lost"))?;
        let pkt = build_packet(pkt_type, payload, Some(&client.jwt));
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct RequestChannelHistoryArgs {
    pub server_id: String,
    pub channel_id: String,
    /// 0 means most-recent; non-zero gets older messages for pagination.
    pub before_id: i64,
    pub limit: i32,
}

#[napi]
pub async fn request_channel_history(args: RequestChannelHistoryArgs) -> napi::Result<()> {
    let RequestChannelHistoryArgs {
        server_id,
        channel_id,
        before_id,
        limit,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelHistoryReq,
        packet::Payload::ChannelHistoryReq(ChannelHistoryRequest {
            channel_id,
            before_id,
            limit,
        }),
    )
    .await
}

#[napi(object)]
pub struct WipeChannelHistoryArgs {
    pub server_id: String,
    pub channel_id: String,
}

/// Owner-only: nuke every message and attachment in `channel_id`.
/// Server validates ownership, applies the wipe, replies with a
/// CHANNEL_WIPE_RES (deleted counts) and broadcasts a CHANNEL_WIPED to
/// every member so their local state drops the channel's history
/// without re-fetching. The IPC returns immediately after the packet
/// is queued — the result lands later as the `channel_wipe_responded`
/// bus event.
#[napi]
pub async fn wipe_channel_history(args: WipeChannelHistoryArgs) -> napi::Result<()> {
    let WipeChannelHistoryArgs {
        server_id,
        channel_id,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelWipeReq,
        packet::Payload::ChannelWipeReq(ChannelWipeRequest { channel_id }),
    )
    .await
}

#[napi(object)]
pub struct UpdateChannelRetentionArgs {
    pub server_id: String,
    pub channel_id: String,
    pub retention_days_text: i32,
    pub retention_days_image: i32,
    pub retention_days_video: i32,
    pub retention_days_document: i32,
    pub retention_days_audio: i32,
}

/// Owner-only retention edit. All five values are sent as a snapshot;
/// 0 means "keep forever".
#[napi]
pub async fn update_channel_retention(args: UpdateChannelRetentionArgs) -> napi::Result<()> {
    let UpdateChannelRetentionArgs {
        server_id,
        channel_id,
        retention_days_text,
        retention_days_image,
        retention_days_video,
        retention_days_document,
        retention_days_audio,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelUpdateReq,
        packet::Payload::ChannelUpdateReq(ChannelUpdateRequest {
            channel_id,
            retention_days_text,
            retention_days_image,
            retention_days_video,
            retention_days_document,
            retention_days_audio,
        }),
    )
    .await
}

#[napi(object)]
pub struct SendChannelMessageArgs {
    pub server_id: String,
    pub channel_id: String,
    pub message: String,
    /// Previously-uploaded attachment ids to bind to this message.
    /// Server verifies ownership, channel scope, and 'ready' status —
    /// anything that doesn't pass is silently dropped from the broadcast.
    /// PR4 only sends text; this stays in the contract so PR-attachments
    /// can light it up without an API change.
    pub attachment_ids: Option<Vec<i64>>,
    /// Client-generated UUID for optimistic-bubble dedup. Server echoes
    /// it in the broadcast so the sending client can match the real
    /// message back to its own optimistic placeholder.
    pub nonce: Option<String>,
}

#[napi]
pub async fn send_channel_message(args: SendChannelMessageArgs) -> napi::Result<()> {
    let state_arc = state::shared();

    let SendChannelMessageArgs {
        server_id,
        channel_id,
        message,
        attachment_ids,
        nonce,
    } = args;

    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let sender = s
            .username
            .clone()
            .ok_or_else(|| napi::Error::from_reason("Not authenticated"))?;
        let client = s.communities.get(&server_id).ok_or_else(|| {
            napi::Error::from_reason(format!("Not connected to community {}", server_id))
        })?;
        let tx = client
            .connection_write_tx()
            .ok_or_else(|| napi::Error::from_reason("Community connection lost"))?;
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Stub attachment rows by id only; server replaces them with
        // authoritative entries before broadcast, so every other field
        // is irrelevant here.
        let attachments: Vec<Attachment> = attachment_ids
            .unwrap_or_default()
            .into_iter()
            .map(|id| Attachment {
                id,
                message_id: 0,
                kind: 0,
                filename: String::new(),
                mime: String::new(),
                size_bytes: 0,
                url: String::new(),
                position: 0,
                created_at: 0,
                purged_at: 0,
                width: 0,
                height: 0,
                thumbnail_size_bytes: 0,
                thumbnail_sizes_mask: 0,
                duration_ms: 0,
            })
            .collect();

        let pkt = build_packet(
            packet::Type::ChannelMsg,
            packet::Payload::ChannelMsg(ChannelMessage {
                sender,
                channel_id,
                content: message,
                timestamp,
                id: 0, // server assigns on persist
                attachments,
                nonce: nonce.unwrap_or_default(),
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}
