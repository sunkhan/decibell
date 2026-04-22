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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReceivedPayload {
    pub context: String,
    pub sender: String,
    pub recipient: String,  // populated for DMs, empty for channel messages
    pub content: String,
    pub timestamp: String,
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
        },
    );
}

pub fn emit_message_received(
    app: &AppHandle,
    context: String,
    sender: String,
    recipient: String,  // NEW
    content: String,
    timestamp: String,
) {
    let _ = app.emit(
        MESSAGE_RECEIVED,
        MessageReceivedPayload {
            context,
            sender,
            recipient,  // NEW
            content,
            timestamp,
        },
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
pub struct VoicePresenceUpdatedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub participants: Vec<String>,
    pub user_states: Vec<VoiceUserStatePayload>,
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
) {
    let _ = app.emit(
        VOICE_PRESENCE_UPDATED,
        VoicePresenceUpdatedPayload { server_id, channel_id, participants, user_states },
    );
}

pub fn emit_voice_state_changed(app: &AppHandle, is_muted: bool, is_deafened: bool) {
    let _ = app.emit(
        VOICE_STATE_CHANGED,
        VoiceStateChangedPayload { is_muted, is_deafened },
    );
}

pub const STREAM_PRESENCE_UPDATED: &str = "stream_presence_updated";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfoPayload {
    pub stream_id: String,
    pub owner_username: String,
    pub has_audio: bool,
    pub resolution_width: u32,
    pub resolution_height: u32,
    pub fps: u32,
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
