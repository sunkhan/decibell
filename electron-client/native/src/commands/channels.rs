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
    /// Jump-to-message: fetch a window centered on this id (replaces the view).
    pub around_id: Option<i64>,
    /// Downward pagination: fetch messages newer than this id.
    pub after_id: Option<i64>,
}

#[napi]
pub async fn request_channel_history(args: RequestChannelHistoryArgs) -> napi::Result<()> {
    let RequestChannelHistoryArgs {
        server_id,
        channel_id,
        before_id,
        limit,
        around_id,
        after_id,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelHistoryReq,
        packet::Payload::ChannelHistoryReq(ChannelHistoryRequest {
            channel_id,
            before_id,
            limit,
            around_id: around_id.unwrap_or(0),
            after_id: after_id.unwrap_or(0),
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
    /// Voice channels only: Opus bitrate in kbps, 0 = client default.
    /// None/undefined = leave unchanged (the wire field has explicit
    /// presence, so retention-only updates can't reset it).
    pub voice_bitrate_kbps: Option<i32>,
    /// Text channels: slowmode seconds (0 = off). None = leave unchanged.
    pub slowmode_seconds: Option<i32>,
}

/// MANAGE_CHANNELS edit. All five retention values are sent as a
/// snapshot; 0 means "keep forever". Voice bitrate rides along when
/// provided.
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
        voice_bitrate_kbps,
        slowmode_seconds,
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
            voice_bitrate_kbps,
            slowmode_seconds,
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
    /// Id of the message being replied to (0/absent = not a reply). Server
    /// validates it points at a message in this channel, else stores 0.
    pub reply_to: Option<i64>,
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
        reply_to,
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
                placeholder: String::new(),
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
                edited_at: 0,
                reply_to: reply_to.unwrap_or(0),
                // Server-resolved on broadcast; never set by the client.
                reply_to_sender: String::new(),
                reply_to_content: String::new(),
                reply_to_attachment_kinds: Vec::new(),
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

#[napi(object)]
pub struct DeleteChannelMessageArgs {
    pub server_id: String,
    pub channel_id: String,
    pub message_id: i64,
}

/// Sends MESSAGE_DELETE_REQ over the community session for server_id.
/// The ack arrives as the `channel_message_delete_responded` event;
/// the broadcast (if successful) arrives as `channel_message_deleted`.
/// Server-side handler enforces self-or-can_delete_others.
#[napi]
pub async fn delete_channel_message(args: DeleteChannelMessageArgs) -> napi::Result<()> {
    let DeleteChannelMessageArgs {
        server_id,
        channel_id,
        message_id,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::MessageDeleteReq,
        packet::Payload::MessageDeleteReq(MessageDeleteReq {
            channel_id,
            message_id,
        }),
    )
    .await
}

#[napi(object)]
pub struct EditChannelMessageArgs {
    pub server_id: String,
    pub channel_id: String,
    pub message_id: i64,
    pub content: String,
}

/// Sends MESSAGE_EDIT_REQ over the community session. The ack arrives as
/// `channel_message_edit_responded`; on success the broadcast arrives as
/// `channel_message_edited`. Server enforces own-message-only.
#[napi]
pub async fn edit_channel_message(args: EditChannelMessageArgs) -> napi::Result<()> {
    let EditChannelMessageArgs {
        server_id,
        channel_id,
        message_id,
        content,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::MessageEditReq,
        packet::Payload::MessageEditReq(MessageEditReq {
            channel_id,
            message_id,
            content,
        }),
    )
    .await
}

// ── Channel management (MANAGE_CHANNELS) ─────────────────────────
//
// Results land as `channel_action_responded`; the authoritative list
// follows as a pushed `channel_list_updated` broadcast.

#[napi(object)]
pub struct CreateChannelArgs {
    pub server_id: String,
    pub name: String,
    /// "text" | "voice" | "category"
    pub channel_type: String,
    /// Voice channels only. 0 = client default.
    pub voice_bitrate_kbps: i32,
    /// Category to create the channel inside (end of its block).
    /// None/empty = end of the uncategorized area. Ignored for
    /// categories, which always append at the very end.
    pub category_id: Option<String>,
}

#[napi]
pub async fn create_channel(args: CreateChannelArgs) -> napi::Result<()> {
    let CreateChannelArgs {
        server_id,
        name,
        channel_type,
        voice_bitrate_kbps,
        category_id,
    } = args;
    let r#type = match channel_type.as_str() {
        "voice" => channel_info::Type::Voice,
        "category" => channel_info::Type::Category,
        _ => channel_info::Type::Text,
    };
    send_for_server(
        &server_id,
        packet::Type::ChannelCreateReq,
        packet::Payload::ChannelCreateReq(ChannelCreateRequest {
            name,
            r#type: r#type as i32,
            voice_bitrate_kbps,
            category_id: category_id.unwrap_or_default(),
        }),
    )
    .await
}

#[napi(object)]
pub struct ReorderChannelsArgs {
    pub server_id: String,
    /// The complete new sidebar order (channels + categories). Must
    /// exactly match the server's current channel set or the reorder
    /// is rejected wholesale.
    pub channel_ids: Vec<String>,
}

#[napi]
pub async fn reorder_channels(args: ReorderChannelsArgs) -> napi::Result<()> {
    let ReorderChannelsArgs {
        server_id,
        channel_ids,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelReorderReq,
        packet::Payload::ChannelReorderReq(ChannelReorderRequest { channel_ids }),
    )
    .await
}

#[napi(object)]
pub struct RenameChannelArgs {
    pub server_id: String,
    pub channel_id: String,
    pub name: String,
}

#[napi]
pub async fn rename_channel(args: RenameChannelArgs) -> napi::Result<()> {
    let RenameChannelArgs {
        server_id,
        channel_id,
        name,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelRenameReq,
        packet::Payload::ChannelRenameReq(ChannelRenameRequest { channel_id, name }),
    )
    .await
}

#[napi(object)]
pub struct SetChannelOverwriteArgs {
    pub server_id: String,
    pub channel_id: String,
    /// "role" | "member"
    pub target_type: String,
    /// Role id (decimal string) or username.
    pub target_id: String,
    pub allow: i64,
    pub deny: i64,
}

/// Permissions v2: set (or, with allow == deny == 0, clear) one
/// per-channel overwrite. Result arrives as channel_action_responded with
/// action "overwrite"; the server then re-pushes channel_list_updated
/// (with refreshed myPermissions) and channel_overwrites_received.
#[napi]
pub async fn set_channel_overwrite(args: SetChannelOverwriteArgs) -> napi::Result<()> {
    let target_type = if args.target_type == "member" {
        channel_overwrite::TargetType::Member
    } else {
        channel_overwrite::TargetType::Role
    };
    send_for_server(
        &args.server_id,
        packet::Type::ChannelOverwriteSetReq,
        packet::Payload::ChannelOverwriteSetReq(ChannelOverwriteSetRequest {
            overwrite: Some(ChannelOverwrite {
                channel_id: args.channel_id,
                target_type: target_type as i32,
                target_id: args.target_id,
                allow: args.allow.max(0) as u64,
                deny: args.deny.max(0) as u64,
            }),
        }),
    )
    .await
}

#[napi(object)]
pub struct ListChannelOverwritesArgs {
    pub server_id: String,
    pub channel_id: String,
}

#[napi]
pub async fn list_channel_overwrites(args: ListChannelOverwritesArgs) -> napi::Result<()> {
    send_for_server(
        &args.server_id,
        packet::Type::ChannelOverwritesReq,
        packet::Payload::ChannelOverwritesReq(ChannelOverwritesRequest {
            channel_id: args.channel_id,
        }),
    )
    .await
}

#[napi(object)]
pub struct DeleteChannelArgs {
    pub server_id: String,
    pub channel_id: String,
}

#[napi]
pub async fn delete_channel(args: DeleteChannelArgs) -> napi::Result<()> {
    let DeleteChannelArgs {
        server_id,
        channel_id,
    } = args;
    send_for_server(
        &server_id,
        packet::Type::ChannelDeleteReq,
        packet::Payload::ChannelDeleteReq(ChannelDeleteRequest { channel_id }),
    )
    .await
}
