use serde::{Deserialize, Serialize};
use tauri::{AppHandle, Emitter};

// Event name constants
pub const LOGIN_SUCCEEDED: &str = "login_succeeded";
pub const LOGIN_FAILED: &str = "login_failed";
pub const REGISTER_RESPONDED: &str = "register_responded";
pub const LOGGED_OUT: &str = "logged_out";
pub const CONNECTION_LOST: &str = "connection_lost";
pub const CONNECTION_RESTORED: &str = "connection_restored";
pub const SERVER_LIST_RECEIVED: &str = "server_list_received";
pub const COMMUNITY_AUTH_RESPONDED: &str = "community_auth_responded";
pub const MESSAGE_RECEIVED: &str = "message_received";
pub const USER_LIST_UPDATED: &str = "user_list_updated";
pub const FRIEND_LIST_RECEIVED: &str = "friend_list_received";
pub const FRIEND_ACTION_RESPONDED: &str = "friend_action_responded";

// --- Payload structs ---

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginSucceededPayload {
    pub username: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct LoginFailedPayload {
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RegisterRespondedPayload {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEventPayload {
    pub server_type: String,
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub id: i32,
    pub name: String,
    pub description: String,
    pub host_ip: String,
    pub port: i32,
    pub member_count: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ServerListReceivedPayload {
    pub servers: Vec<ServerInfo>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelInfoPayload {
    pub id: String,
    pub name: String,
    pub r#type: String,
    pub voice_bitrate_kbps: i32,
    pub retention_days_text: i32,
    pub retention_days_image: i32,
    pub retention_days_video: i32,
    pub retention_days_document: i32,
    pub retention_days_audio: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPayload {
    pub id: i64,
    pub message_id: i64,
    pub kind: String, // "image" | "video" | "document" | "audio"
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    pub url: String,
    pub position: i32,
    pub created_at: i64,
    pub purged_at: i64, // 0 = present, nonzero = tombstone timestamp
    // Intrinsic image dimensions, 0 when unknown (non-image or legacy row).
    pub width: u32,
    pub height: u32,
    // Total bytes across all server-stored thumbnail sizes for this
    // attachment (0 = none). Used as a "fetch makes sense" flag.
    pub thumbnail_size_bytes: u32,
    // Bitmask of pre-generated thumbnail sizes available on the server.
    // bit 0 = 320 px long-edge, bit 1 = 640 px, bit 2 = 1280 px. 0 with
    // thumbnail_size_bytes > 0 means a legacy single-size upload (320,
    // served from the legacy path without &size=).
    pub thumbnail_sizes_mask: u32,
    // Audio + video duration in ms; 0 unknown. Read at upload time
    // client-side and shipped through /attachments/init.
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMessagePayload {
    pub id: i64,
    pub sender: String,
    pub channel_id: String,
    pub content: String,
    pub timestamp: i64,
    pub attachments: Vec<AttachmentPayload>,
    // Client-generated UUID echoed back by the server. Lets the
    // sender's client dedup the optimistic bubble against the real
    // broadcast. Empty for messages from history (originating client
    // long gone, no optimistic to dedup) and for incoming messages
    // from other users (no optimistic on this client to dedup against).
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityAuthRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub channels: Vec<ChannelInfoPayload>,
    pub error_code: String,
    pub server_name: String,
    pub server_description: String,
    pub owner_username: String,
    pub attachment_port: i32,
    pub max_attachment_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReceivedPayload {
    pub context: String,
    pub sender: String,
    pub recipient: String,  // populated for DMs, empty for channel messages
    pub content: String,
    pub timestamp: String,
    // Server-assigned message id for channel messages. 0 for DMs (not persisted).
    pub id: i64,
    pub attachments: Vec<AttachmentPayload>,
    // Client-generated UUID echoed by the server. Empty for incoming
    // messages from other users (their nonce is irrelevant to us).
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserListUpdatedPayload {
    pub online_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendInfoPayload {
    pub username: String,
    pub status: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendListReceivedPayload {
    pub friends: Vec<FriendInfoPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FriendActionRespondedPayload {
    pub success: bool,
    pub message: String,
}

// --- Emit helper functions ---

pub fn emit_login_succeeded(app: &AppHandle, username: String) {
    let _ = app.emit(LOGIN_SUCCEEDED, LoginSucceededPayload { username });
}

pub fn emit_login_failed(app: &AppHandle, message: String) {
    let _ = app.emit(LOGIN_FAILED, LoginFailedPayload { message });
}

pub fn emit_register_responded(app: &AppHandle, success: bool, message: String) {
    let _ = app.emit(REGISTER_RESPONDED, RegisterRespondedPayload { success, message });
}

pub fn emit_logged_out(app: &AppHandle) {
    let _ = app.emit(LOGGED_OUT, ());
}

pub fn emit_connection_lost(app: &AppHandle, server_type: &str, server_id: Option<String>) {
    let _ = app.emit(
        CONNECTION_LOST,
        ConnectionEventPayload {
            server_type: server_type.to_string(),
            server_id,
        },
    );
}

pub fn emit_connection_restored(app: &AppHandle, server_type: &str, server_id: Option<String>) {
    let _ = app.emit(
        CONNECTION_RESTORED,
        ConnectionEventPayload {
            server_type: server_type.to_string(),
            server_id,
        },
    );
}

pub fn emit_server_list_received(app: &AppHandle, servers: Vec<ServerInfo>) {
    let _ = app.emit(SERVER_LIST_RECEIVED, ServerListReceivedPayload { servers });
}

pub fn emit_community_auth_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    channels: Vec<ChannelInfoPayload>,
    error_code: String,
    server_name: String,
    server_description: String,
    owner_username: String,
    attachment_port: i32,
    max_attachment_bytes: i64,
) {
    let _ = app.emit(
        COMMUNITY_AUTH_RESPONDED,
        CommunityAuthRespondedPayload {
            server_id,
            success,
            message,
            channels,
            error_code,
            server_name,
            server_description,
            owner_username,
            attachment_port,
            max_attachment_bytes,
        },
    );
}

pub fn emit_message_received(
    app: &AppHandle,
    context: String,
    sender: String,
    recipient: String,
    content: String,
    timestamp: String,
    id: i64,
    attachments: Vec<AttachmentPayload>,
    nonce: String,
) {
    let _ = app.emit(
        MESSAGE_RECEIVED,
        MessageReceivedPayload {
            context,
            sender,
            recipient,
            content,
            timestamp,
            id,
            attachments,
            nonce,
        },
    );
}

// --- Channel history + pruning ---
pub const CHANNEL_HISTORY_RECEIVED: &str = "channel_history_received";
pub const CHANNEL_PRUNED: &str = "channel_pruned";
pub const CHANNEL_UPDATED: &str = "channel_updated";
pub const CHANNEL_WIPE_RESPONDED: &str = "channel_wipe_responded";
pub const CHANNEL_WIPED: &str = "channel_wiped";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelHistoryReceivedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub messages: Vec<ChannelMessagePayload>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentTombstonePayload {
    pub attachment_id: i64,
    pub purged_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPrunedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub deleted_message_ids: Vec<i64>,
    pub purged_attachments: Vec<AttachmentTombstonePayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelUpdatedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub channel: Option<ChannelInfoPayload>,
}

pub fn emit_channel_history_received(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    messages: Vec<ChannelMessagePayload>,
    has_more: bool,
) {
    let _ = app.emit(
        CHANNEL_HISTORY_RECEIVED,
        ChannelHistoryReceivedPayload { server_id, channel_id, messages, has_more },
    );
}

pub fn emit_channel_pruned(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    deleted_message_ids: Vec<i64>,
    purged_attachments: Vec<AttachmentTombstonePayload>,
) {
    let _ = app.emit(
        CHANNEL_PRUNED,
        ChannelPrunedPayload {
            server_id, channel_id, deleted_message_ids, purged_attachments,
        },
    );
}

pub fn emit_channel_updated(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    channel: Option<ChannelInfoPayload>,
) {
    let _ = app.emit(
        CHANNEL_UPDATED,
        ChannelUpdatedPayload { server_id, success, message, channel },
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelWipeRespondedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub success: bool,
    pub message: String,
    pub deleted_message_count: i64,
    pub deleted_attachment_count: i64,
}

pub fn emit_channel_wipe_responded(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    success: bool,
    message: String,
    deleted_message_count: i64,
    deleted_attachment_count: i64,
) {
    let _ = app.emit(
        CHANNEL_WIPE_RESPONDED,
        ChannelWipeRespondedPayload {
            server_id, channel_id, success, message,
            deleted_message_count, deleted_attachment_count,
        },
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelWipedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub wiped_at: i64,
    pub wiped_by: String,
}

pub fn emit_channel_wiped(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    wiped_at: i64,
    wiped_by: String,
) {
    let _ = app.emit(
        CHANNEL_WIPED,
        ChannelWipedPayload { server_id, channel_id, wiped_at, wiped_by },
    );
}

pub fn emit_user_list_updated(app: &AppHandle, online_users: Vec<String>) {
    let _ = app.emit(USER_LIST_UPDATED, UserListUpdatedPayload { online_users });
}

pub fn emit_friend_list_received(app: &AppHandle, friends: Vec<FriendInfoPayload>) {
    let _ = app.emit(FRIEND_LIST_RECEIVED, FriendListReceivedPayload { friends });
}

pub fn emit_friend_action_responded(app: &AppHandle, success: bool, message: String) {
    let _ = app.emit(
        FRIEND_ACTION_RESPONDED,
        FriendActionRespondedPayload { success, message },
    );
}

pub const VOICE_PRESENCE_UPDATED: &str = "voice_presence_updated";
pub const VOICE_STATE_CHANGED: &str = "voice_state_changed";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceUserStatePayload {
    pub username: String,
    pub is_muted: bool,
    pub is_deafened: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecCapabilityPayload {
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilitiesPayload {
    pub encode: Vec<CodecCapabilityPayload>,
    pub decode: Vec<CodecCapabilityPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePresenceUpdatedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub participants: Vec<String>,
    pub user_states: Vec<VoiceUserStatePayload>,
    /// Parallel to participants — user_capabilities[i] belongs to
    /// participants[i]. Drives JS-side LCD evaluation, watch-button
    /// gating, and codec badge rendering.
    pub user_capabilities: Vec<ClientCapabilitiesPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStateChangedPayload {
    pub is_muted: bool,
    pub is_deafened: bool,
}

pub fn emit_voice_presence_updated(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    participants: Vec<String>,
    user_states: Vec<VoiceUserStatePayload>,
    user_capabilities: Vec<ClientCapabilitiesPayload>,
) {
    let _ = app.emit(
        VOICE_PRESENCE_UPDATED,
        VoicePresenceUpdatedPayload {
            server_id, channel_id, participants, user_states, user_capabilities,
        },
    );
}

pub fn emit_voice_state_changed(app: &AppHandle, is_muted: bool, is_deafened: bool) {
    let _ = app.emit(
        VOICE_STATE_CHANGED,
        VoiceStateChangedPayload { is_muted, is_deafened },
    );
}

pub const STREAM_PRESENCE_UPDATED: &str = "stream_presence_updated";

// Plan C: codec swap event forwarded from server. Drives the toast
// notification in React.
pub const STREAM_CODEC_CHANGED: &str = "stream_codec_changed";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamCodecChangedPayload {
    pub channel_id: String,
    pub streamer_username: String,
    pub new_codec: i32,
    pub new_width: u32,
    pub new_height: u32,
    pub new_fps: u32,
    pub reason: i32,
}

pub fn emit_stream_codec_changed(
    app: &AppHandle,
    payload: StreamCodecChangedPayload,
) {
    let _ = app.emit(STREAM_CODEC_CHANGED, payload);
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfoPayload {
    pub stream_id: String,
    pub owner_username: String,
    pub has_audio: bool,
    pub resolution_width: u32,
    pub resolution_height: u32,
    pub fps: u32,
    /// VideoCodec enum value — drives codec badge + per-packet decoder
    /// reconfiguration awareness on the viewer side.
    pub current_codec: i32,
    /// VideoCodec.UNKNOWN (0) when not enforced. Drives lock icon on
    /// the badge and grayed-out watch button when local client lacks
    /// the codec in its decode caps.
    pub enforced_codec: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamPresenceUpdatedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub streams: Vec<StreamInfoPayload>,
}

pub fn emit_stream_presence_updated(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    streams: Vec<StreamInfoPayload>,
) {
    let _ = app.emit(
        STREAM_PRESENCE_UPDATED,
        StreamPresenceUpdatedPayload {
            server_id,
            channel_id,
            streams,
        },
    );
}

pub const STREAM_THUMBNAIL_UPDATED: &str = "stream_thumbnail_updated";

// --- Attachment upload / download progress ---
pub const ATTACHMENT_UPLOAD_PROGRESS: &str = "attachment_upload_progress";
pub const ATTACHMENT_UPLOAD_COMPLETE: &str = "attachment_upload_complete";
pub const ATTACHMENT_UPLOAD_FAILED:   &str = "attachment_upload_failed";
pub const ATTACHMENT_DOWNLOAD_PROGRESS: &str = "attachment_download_progress";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadProgressPayload {
    pub pending_id: String,
    pub server_id: String,
    pub channel_id: String,
    pub attachment_id: i64,
    pub filename: String,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadCompletePayload {
    pub pending_id: String,
    pub server_id: String,
    pub channel_id: String,
    pub attachment_id: i64,
    pub filename: String,
    pub mime: String,
    pub kind: String,
    pub size_bytes: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentUploadFailedPayload {
    pub pending_id: String,
    pub server_id: String,
    pub channel_id: String,
    pub attachment_id: i64,
    pub filename: String,
    pub message: String,
    pub cancelled: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentDownloadProgressPayload {
    pub attachment_id: i64,
    pub transferred_bytes: u64,
    pub total_bytes: u64,
}

pub fn emit_attachment_upload_progress(app: &AppHandle, p: AttachmentUploadProgressPayload) {
    let _ = app.emit(ATTACHMENT_UPLOAD_PROGRESS, p);
}
pub fn emit_attachment_upload_complete(app: &AppHandle, p: AttachmentUploadCompletePayload) {
    let _ = app.emit(ATTACHMENT_UPLOAD_COMPLETE, p);
}
pub fn emit_attachment_upload_failed(app: &AppHandle, p: AttachmentUploadFailedPayload) {
    let _ = app.emit(ATTACHMENT_UPLOAD_FAILED, p);
}
pub fn emit_attachment_download_progress(app: &AppHandle, p: AttachmentDownloadProgressPayload) {
    let _ = app.emit(ATTACHMENT_DOWNLOAD_PROGRESS, p);
}

// --- Deep link ---
pub const DEEP_LINK_RECEIVED: &str = "deep_link_received";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DeepLinkReceivedPayload {
    pub url: String,
}

pub fn emit_deep_link_received(app: &AppHandle, url: String) {
    let _ = app.emit(DEEP_LINK_RECEIVED, DeepLinkReceivedPayload { url });
}

// --- Invites + membership ---
pub const INVITE_CREATE_RESPONDED: &str = "invite_create_responded";
pub const INVITE_LIST_RECEIVED: &str = "invite_list_received";
pub const INVITE_REVOKE_RESPONDED: &str = "invite_revoke_responded";
pub const MEMBER_LIST_RECEIVED: &str = "member_list_received";
pub const MOD_ACTION_RESPONDED: &str = "mod_action_responded";
pub const MEMBERSHIP_REVOKED: &str = "membership_revoked";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteInfoPayload {
    pub code: String,
    pub created_by: String,
    pub created_at: i64,
    pub expires_at: i64,
    pub max_uses: i32,
    pub uses: i32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteCreateRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub invite: Option<InviteInfoPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteListReceivedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub invites: Vec<InviteInfoPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct InviteRevokeRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInfoPayload {
    pub username: String,
    pub joined_at: i64,
    pub nickname: String,
    pub is_owner: bool,
    pub is_online: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberListReceivedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub members: Vec<MemberInfoPayload>,
    pub bans: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModActionRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub username: String,
    pub action: String, // "kick" | "ban" | "leave"
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MembershipRevokedPayload {
    pub server_id: String,
    pub action: String,
    pub reason: String,
    pub actor: String,
}

pub fn emit_invite_create_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    invite: Option<InviteInfoPayload>,
) {
    let _ = app.emit(
        INVITE_CREATE_RESPONDED,
        InviteCreateRespondedPayload { server_id, success, message, invite },
    );
}

pub fn emit_invite_list_received(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    invites: Vec<InviteInfoPayload>,
) {
    let _ = app.emit(
        INVITE_LIST_RECEIVED,
        InviteListReceivedPayload { server_id, success, message, invites },
    );
}

pub fn emit_invite_revoke_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    code: String,
) {
    let _ = app.emit(
        INVITE_REVOKE_RESPONDED,
        InviteRevokeRespondedPayload { server_id, success, message, code },
    );
}

pub fn emit_member_list_received(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    members: Vec<MemberInfoPayload>,
    bans: Vec<String>,
) {
    let _ = app.emit(
        MEMBER_LIST_RECEIVED,
        MemberListReceivedPayload { server_id, success, message, members, bans },
    );
}

pub fn emit_mod_action_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    message: String,
    username: String,
    action: String,
) {
    let _ = app.emit(
        MOD_ACTION_RESPONDED,
        ModActionRespondedPayload { server_id, success, message, username, action },
    );
}

pub fn emit_membership_revoked(
    app: &AppHandle,
    server_id: String,
    action: String,
    reason: String,
    actor: String,
) {
    let _ = app.emit(
        MEMBERSHIP_REVOKED,
        MembershipRevokedPayload { server_id, action, reason, actor },
    );
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamThumbnailUpdatedPayload {
    pub owner_username: String,
    pub thumbnail_base64: String,
}

pub fn emit_stream_thumbnail_updated(
    app: &AppHandle,
    owner_username: String,
    thumbnail_data: Vec<u8>,
) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&thumbnail_data);
    let data_url = format!("data:image/jpeg;base64,{}", b64);
    let _ = app.emit(
        STREAM_THUMBNAIL_UPDATED,
        StreamThumbnailUpdatedPayload {
            owner_username,
            thumbnail_base64: data_url,
        },
    );
}
