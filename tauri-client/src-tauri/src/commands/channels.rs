use tauri::State;

use crate::net::connection::build_packet;
use crate::net::proto::*;
use crate::state::SharedState;

async fn send_for_server(
    server_id: &str,
    state: &State<'_, SharedState>,
    pkt_type: packet::Type,
    payload: packet::Payload,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let client = s.communities
            .get(server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        let pkt = build_packet(pkt_type, payload, Some(&client.jwt));
        (tx, pkt)
    };
    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}

/// Request a page of persisted messages for `channel_id`. `before_id = 0`
/// means most-recent; non-zero gets older messages for pagination.
#[tauri::command]
pub async fn request_channel_history(
    server_id: String,
    channel_id: String,
    before_id: i64,
    limit: i32,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::ChannelHistoryReq,
        packet::Payload::ChannelHistoryReq(ChannelHistoryRequest {
            channel_id,
            before_id,
            limit,
        }),
    )
    .await
}

/// Owner-only: nuke every message and attachment in `channel_id`.
/// Server validates ownership, applies the wipe, replies with a
/// CHANNEL_WIPE_RES (deleted counts) and broadcasts a CHANNEL_WIPED
/// to every member so their local state drops the channel's history
/// without re-fetching. The IPC returns immediately after the packet
/// is queued — the result lands later as the `channel_wipe_responded`
/// Tauri event.
#[tauri::command]
pub async fn wipe_channel_history(
    server_id: String,
    channel_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
        packet::Type::ChannelWipeReq,
        packet::Payload::ChannelWipeReq(ChannelWipeRequest {
            channel_id,
        }),
    )
    .await
}

/// Update channel retention (owner-only, enforced server-side). All five
/// values are sent as a snapshot; 0 means "keep forever".
#[tauri::command]
pub async fn update_channel_retention(
    server_id: String,
    channel_id: String,
    retention_days_text: i32,
    retention_days_image: i32,
    retention_days_video: i32,
    retention_days_document: i32,
    retention_days_audio: i32,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    send_for_server(
        &server_id,
        &state,
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

#[tauri::command]
pub async fn send_channel_message(
    server_id: String,
    channel_id: String,
    message: String,
    // Previously-uploaded attachment ids to bind to this message. Server
    // verifies ownership, channel scope, and 'ready' status — anything
    // that doesn't pass is silently dropped from the broadcast.
    attachment_ids: Option<Vec<i64>>,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let (write_tx, data) = {
        let s = state.lock().await;
        let sender = s.username.clone().ok_or("Not authenticated")?;
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        let tx = client.connection_write_tx()
            .ok_or("Community connection lost")?;
        use std::time::{SystemTime, UNIX_EPOCH};
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs() as i64;

        // Populate attachment ids as stubs; server replaces with authoritative
        // rows before broadcast, so every other field is irrelevant here.
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
                sender: sender.into(),
                channel_id: channel_id.into(),
                content: message.into(),
                timestamp,
                id: 0, // server assigns on persist
                attachments,
            }),
            Some(&client.jwt),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err("Connection closed".to_string()),
        Err(_) => Err("Send timed out".to_string()),
    }
}
