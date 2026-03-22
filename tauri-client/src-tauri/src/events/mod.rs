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
pub const JOIN_CHANNEL_RESPONDED: &str = "join_channel_responded";
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
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommunityAuthRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub channels: Vec<ChannelInfoPayload>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReceivedPayload {
    pub context: String,
    pub sender: String,
    pub content: String,
    pub timestamp: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserListUpdatedPayload {
    pub online_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct JoinChannelRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub channel_id: String,
    pub active_users: Vec<String>,
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
) {
    let _ = app.emit(
        COMMUNITY_AUTH_RESPONDED,
        CommunityAuthRespondedPayload {
            server_id,
            success,
            message,
            channels,
        },
    );
}

pub fn emit_message_received(
    app: &AppHandle,
    context: String,
    sender: String,
    content: String,
    timestamp: String,
) {
    let _ = app.emit(
        MESSAGE_RECEIVED,
        MessageReceivedPayload {
            context,
            sender,
            content,
            timestamp,
        },
    );
}

pub fn emit_user_list_updated(app: &AppHandle, online_users: Vec<String>) {
    let _ = app.emit(USER_LIST_UPDATED, UserListUpdatedPayload { online_users });
}

pub fn emit_join_channel_responded(
    app: &AppHandle,
    server_id: String,
    success: bool,
    channel_id: String,
    active_users: Vec<String>,
) {
    let _ = app.emit(
        JOIN_CHANNEL_RESPONDED,
        JoinChannelRespondedPayload {
            server_id,
            success,
            channel_id,
            active_users,
        },
    );
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
