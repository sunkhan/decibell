//! EventBus: a single ThreadsafeFunction shared across the entire addon.
//!
//! Mirrors Tauri's `app.emit(name, payload)` shape one-for-one so the
//! renderer-side `listen(name, handler)` shim works unchanged. The
//! handful of high-volume per-stream channels (encoded video frames)
//! get their own TSFN registered separately — they don't go through
//! this bus.
//!
//! Event name constants live alongside the helpers so call sites get
//! one canonical string per event (compile-time typo check).

use std::sync::OnceLock;

use napi::bindgen_prelude::*;
use napi::threadsafe_function::{
    ErrorStrategy, ThreadSafeCallContext, ThreadsafeFunction, ThreadsafeFunctionCallMode,
};
use napi::{JsFunction, JsUnknown};
use serde::Serialize;

type Bus = ThreadsafeFunction<serde_json::Value, ErrorStrategy::Fatal>;
static BUS: OnceLock<Bus> = OnceLock::new();

/// Convert the JS-side bus callback into a TSFN. Called once from
/// `init(opts, bus)` — the JS side passes a function `(env) => void`
/// that broadcasts to every BrowserWindow.
pub fn install(callback: JsFunction) -> Result<()> {
    let tsfn: Bus = callback.create_threadsafe_function(
        0,
        |ctx: ThreadSafeCallContext<serde_json::Value>| -> Result<Vec<JsUnknown>> {
            // serde-json feature on napi gives us Env::to_js_value for
            // any Serialize. Turns the envelope into a real JS object
            // instead of round-tripping through a JSON string.
            let v: JsUnknown = ctx.env.to_js_value(&ctx.value)?;
            Ok(vec![v])
        },
    )?;
    let _ = BUS.set(tsfn);
    Ok(())
}

// ── Stream frame bus ──────────────────────────────────────────────
// Encoded video frames don't go through the JSON bus — at 60 fps with
// 30 KB frames per watcher that's 1.8 MB/s of base64 + JSON.parse work
// per stream. This dedicated TSFN carries the raw bytes as Node Buffer
// (zero-copy view over a Vec) plus a small metadata bag, so the
// renderer's WebCodecs decoder gets the bitstream directly.

/// Owned binary payload that crosses the TSFN boundary. The Buffer
/// fields wrap Vec<u8> — napi-rs hands the underlying allocation to V8
/// as a real Uint8Array view (no copy on the way out).
pub struct StreamFrame {
    pub username: String,
    pub codec: u8,
    pub keyframe: bool,
    pub timestamp: i64,
    pub data: Vec<u8>,
    pub description: Option<Vec<u8>>,
}

type StreamBus = ThreadsafeFunction<StreamFrame, ErrorStrategy::Fatal>;
static STREAM_BUS: OnceLock<StreamBus> = OnceLock::new();

/// Install the stream-frame bus. Called from `init(...)` alongside the
/// main bus install. The JS callback receives a single object:
/// `{ username, codec, keyframe, timestamp, data: Uint8Array,
///    description: Uint8Array | null }`.
pub fn install_stream_bus(callback: JsFunction) -> Result<()> {
    let tsfn: StreamBus = callback.create_threadsafe_function(
        0,
        |ctx: ThreadSafeCallContext<StreamFrame>| -> Result<Vec<JsUnknown>> {
            let env = &ctx.env;
            let mut obj = env.create_object()?;
            obj.set_named_property("username", env.create_string(&ctx.value.username)?)?;
            obj.set_named_property("codec", env.create_uint32(ctx.value.codec as u32)?)?;
            obj.set_named_property("keyframe", env.get_boolean(ctx.value.keyframe)?)?;
            obj.set_named_property("timestamp", env.create_int64(ctx.value.timestamp)?)?;
            // Buffer::with_data hands V8 the Vec's backing allocation
            // directly — no copy.
            let data_buf = env.create_buffer_with_data(ctx.value.data)?;
            obj.set_named_property("data", data_buf.into_raw())?;
            match ctx.value.description {
                Some(d) => {
                    let desc_buf = env.create_buffer_with_data(d)?;
                    obj.set_named_property("description", desc_buf.into_raw())?;
                }
                None => {
                    obj.set_named_property("description", env.get_null()?)?;
                }
            }
            Ok(vec![obj.into_unknown()])
        },
    )?;
    let _ = STREAM_BUS.set(tsfn);
    Ok(())
}

/// Push an encoded video frame to JS. Safe from any thread.
/// Drops silently if the stream bus hasn't been installed yet (the
/// renderer's video receive thread starts before the bus is installed
/// during init() in rare ordering — fine, no frames are flowing yet).
pub fn send_stream_frame(frame: StreamFrame) {
    let Some(bus) = STREAM_BUS.get() else {
        return;
    };
    bus.call(frame, ThreadsafeFunctionCallMode::NonBlocking);
}

/// Send an event. Safe to call from any thread (including std::thread
/// workers — capture, encode, network receivers). NonBlocking: if the
/// renderer-side queue saturates the call is dropped silently. For the
/// ~30 named events this is fine; encoded-video frames bypass this bus
/// entirely.
pub fn send<P: Serialize>(name: &'static str, payload: P) {
    let Some(bus) = BUS.get() else {
        // init(opts, bus) hasn't run yet — happens during very early
        // startup. Drop the event silently.
        return;
    };
    let value = serde_json::json!({
        "name": name,
        "payload": serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    });
    bus.call(value, ThreadsafeFunctionCallMode::NonBlocking);
}

// ── Event name constants ──────────────────────────────────────────
// Ported as needed from tauri-client/src-tauri/src/events/mod.rs.
// New names land here when their emitting commands port over.

pub const CONNECTION_LOST: &str = "connection_lost";
pub const CONNECTION_RESTORED: &str = "connection_restored";
pub const LOGIN_SUCCEEDED: &str = "login_succeeded";
pub const LOGIN_FAILED: &str = "login_failed";
pub const REGISTER_RESPONDED: &str = "register_responded";
pub const LOGGED_OUT: &str = "logged_out";
pub const SERVER_LIST_RECEIVED: &str = "server_list_received";
pub const COMMUNITY_AUTH_RESPONDED: &str = "community_auth_responded";
pub const MESSAGE_RECEIVED: &str = "message_received";
pub const CHANNEL_HISTORY_RECEIVED: &str = "channel_history_received";
pub const CHANNEL_PRUNED: &str = "channel_pruned";
pub const CHANNEL_UPDATED: &str = "channel_updated";
pub const CHANNEL_WIPE_RESPONDED: &str = "channel_wipe_responded";
pub const CHANNEL_WIPED: &str = "channel_wiped";
pub const USER_LIST_UPDATED: &str = "user_list_updated";
pub const FRIEND_LIST_RECEIVED: &str = "friend_list_received";
pub const FRIEND_ACTION_RESPONDED: &str = "friend_action_responded";
pub const MEMBER_LIST_RECEIVED: &str = "member_list_received";
pub const MOD_ACTION_RESPONDED: &str = "mod_action_responded";
pub const MEMBERSHIP_REVOKED: &str = "membership_revoked";
pub const VOICE_PRESENCE_UPDATED: &str = "voice_presence_updated";
pub const VOICE_STATE_CHANGED: &str = "voice_state_changed";
pub const VOICE_USER_SPEAKING: &str = "voice_user_speaking";
pub const VOICE_USER_STATE_CHANGED: &str = "voice_user_state_changed";
pub const VOICE_INPUT_LEVEL: &str = "voice_input_level";
pub const VOICE_PING_UPDATED: &str = "voice_ping_updated";
pub const VOICE_CONNECTION_STATS: &str = "voice_connection_stats";
pub const VOICE_ERROR: &str = "voice_error";
pub const STREAM_PRESENCE_UPDATED: &str = "stream_presence_updated";
pub const STREAM_THUMBNAIL_UPDATED: &str = "stream_thumbnail_updated";
pub const STREAM_CODEC_CHANGED: &str = "stream_codec_changed";
pub const STREAM_GPU_FALLBACK: &str = "stream_gpu_fallback";
pub const STREAM_CAPTURE_ENDED: &str = "stream_capture_ended";
pub const STREAM_FRAME: &str = "stream_frame";
pub const CAPS_REFRESHED: &str = "caps_refreshed";

// ── Payload structs ───────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ConnectionEventPayload {
    pub server_type: String,
    pub server_id: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginSucceededPayload {
    pub username: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct LoginFailedPayload {
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct RegisterRespondedPayload {
    pub success: bool,
    pub message: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerInfo {
    pub id: i32,
    pub name: String,
    pub description: String,
    pub host_ip: String,
    pub port: i32,
    pub member_count: i32,
}

#[derive(Debug, Clone, Serialize)]
pub struct ServerListReceivedPayload {
    pub servers: Vec<ServerInfo>,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentPayload {
    pub id: i64,
    pub message_id: i64,
    /// "image" | "video" | "document" | "audio"
    pub kind: String,
    pub filename: String,
    pub mime: String,
    pub size_bytes: i64,
    pub url: String,
    pub position: i32,
    pub created_at: i64,
    /// 0 = present, nonzero = tombstone timestamp
    pub purged_at: i64,
    pub width: u32,
    pub height: u32,
    /// Total bytes across all server-stored thumbnail sizes (0 = none).
    pub thumbnail_size_bytes: u32,
    /// Bitmask of pre-generated thumbnail sizes (bit 0 = 320 px, 1 = 640, 2 = 1280).
    pub thumbnail_sizes_mask: u32,
    /// Audio + video duration in ms; 0 unknown.
    pub duration_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AttachmentTombstonePayload {
    pub attachment_id: i64,
    pub purged_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMessagePayload {
    pub id: i64,
    pub sender: String,
    pub channel_id: String,
    pub content: String,
    pub timestamp: i64,
    pub attachments: Vec<AttachmentPayload>,
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageReceivedPayload {
    /// Channel id for channel messages, "dm" for direct messages.
    pub context: String,
    pub sender: String,
    /// Populated for DMs, empty for channel messages.
    pub recipient: String,
    pub content: String,
    pub timestamp: String,
    pub id: i64,
    pub attachments: Vec<AttachmentPayload>,
    pub nonce: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelHistoryReceivedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub messages: Vec<ChannelMessagePayload>,
    pub has_more: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelPrunedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub deleted_message_ids: Vec<i64>,
    pub purged_attachments: Vec<AttachmentTombstonePayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelUpdatedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub channel: Option<ChannelInfoPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelWipeRespondedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub success: bool,
    pub message: String,
    pub deleted_message_count: i64,
    pub deleted_attachment_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelWipedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub wiped_at: i64,
    pub wiped_by: String,
}

#[derive(Debug, Clone, Serialize)]
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

// ── Emit helpers ──────────────────────────────────────────────────

pub fn emit_connection_lost(server_type: &str, server_id: Option<String>) {
    send(
        CONNECTION_LOST,
        ConnectionEventPayload {
            server_type: server_type.to_string(),
            server_id,
        },
    );
}

pub fn emit_connection_restored(server_type: &str, server_id: Option<String>) {
    send(
        CONNECTION_RESTORED,
        ConnectionEventPayload {
            server_type: server_type.to_string(),
            server_id,
        },
    );
}

pub fn emit_login_succeeded(username: String) {
    send(LOGIN_SUCCEEDED, LoginSucceededPayload { username });
}

pub fn emit_login_failed(message: String) {
    send(LOGIN_FAILED, LoginFailedPayload { message });
}

pub fn emit_register_responded(success: bool, message: String) {
    send(
        REGISTER_RESPONDED,
        RegisterRespondedPayload { success, message },
    );
}

pub fn emit_logged_out() {
    send(LOGGED_OUT, serde_json::Value::Null);
}

pub fn emit_server_list_received(servers: Vec<ServerInfo>) {
    send(SERVER_LIST_RECEIVED, ServerListReceivedPayload { servers });
}

pub fn emit_community_auth_responded(payload: CommunityAuthRespondedPayload) {
    send(COMMUNITY_AUTH_RESPONDED, payload);
}

pub fn emit_message_received(payload: MessageReceivedPayload) {
    send(MESSAGE_RECEIVED, payload);
}

pub fn emit_channel_history_received(payload: ChannelHistoryReceivedPayload) {
    send(CHANNEL_HISTORY_RECEIVED, payload);
}

pub fn emit_channel_pruned(payload: ChannelPrunedPayload) {
    send(CHANNEL_PRUNED, payload);
}

pub fn emit_channel_updated(payload: ChannelUpdatedPayload) {
    send(CHANNEL_UPDATED, payload);
}

pub fn emit_channel_wipe_responded(payload: ChannelWipeRespondedPayload) {
    send(CHANNEL_WIPE_RESPONDED, payload);
}

pub fn emit_channel_wiped(payload: ChannelWipedPayload) {
    send(CHANNEL_WIPED, payload);
}

// ── Friends + presence ────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct UserListUpdatedPayload {
    pub online_users: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FriendInfoPayload {
    pub username: String,
    /// "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked"
    pub status: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct FriendListReceivedPayload {
    pub friends: Vec<FriendInfoPayload>,
}

#[derive(Debug, Clone, Serialize)]
pub struct FriendActionRespondedPayload {
    pub success: bool,
    pub message: String,
}

pub fn emit_user_list_updated(online_users: Vec<String>) {
    send(USER_LIST_UPDATED, UserListUpdatedPayload { online_users });
}

pub fn emit_friend_list_received(friends: Vec<FriendInfoPayload>) {
    send(FRIEND_LIST_RECEIVED, FriendListReceivedPayload { friends });
}

pub fn emit_friend_action_responded(success: bool, message: String) {
    send(
        FRIEND_ACTION_RESPONDED,
        FriendActionRespondedPayload { success, message },
    );
}

// ── Members + moderation ──────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberInfoPayload {
    pub username: String,
    pub joined_at: i64,
    pub nickname: String,
    pub is_owner: bool,
    pub is_online: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MemberListReceivedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub members: Vec<MemberInfoPayload>,
    pub bans: Vec<String>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ModActionRespondedPayload {
    pub server_id: String,
    pub success: bool,
    pub message: String,
    pub username: String,
    /// "kick" | "ban" | "leave"
    pub action: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MembershipRevokedPayload {
    pub server_id: String,
    pub action: String,
    pub reason: String,
    pub actor: String,
}

pub fn emit_member_list_received(payload: MemberListReceivedPayload) {
    send(MEMBER_LIST_RECEIVED, payload);
}

pub fn emit_mod_action_responded(payload: ModActionRespondedPayload) {
    send(MOD_ACTION_RESPONDED, payload);
}

pub fn emit_membership_revoked(payload: MembershipRevokedPayload) {
    send(MEMBERSHIP_REVOKED, payload);
}

// ── Voice ─────────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceUserStatePayload {
    pub username: String,
    pub is_muted: bool,
    pub is_deafened: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodecCapabilityPayload {
    pub codec: i32,
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
}

#[derive(Debug, Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClientCapabilitiesPayload {
    pub encode: Vec<CodecCapabilityPayload>,
    pub decode: Vec<CodecCapabilityPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePresenceUpdatedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub participants: Vec<String>,
    pub user_states: Vec<VoiceUserStatePayload>,
    /// Parallel to `participants` — user_capabilities[i] belongs to
    /// participants[i]. Drives JS-side LCD evaluation, watch-button
    /// gating, and codec badge rendering.
    pub user_capabilities: Vec<ClientCapabilitiesPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStateChangedPayload {
    pub is_muted: bool,
    pub is_deafened: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceUserSpeakingPayload {
    pub username: String,
    pub speaking: bool,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceUserStateChangedPayload {
    pub username: String,
    pub is_muted: bool,
    pub is_deafened: bool,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceInputLevelPayload {
    pub db: f32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePingUpdatedPayload {
    pub latency_ms: u32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceConnectionStatsPayload {
    pub latency_ms: Option<u32>,
    pub packet_loss_pct: f32,
}

#[derive(Debug, Clone, Serialize)]
pub struct VoiceErrorPayload {
    pub message: String,
}

pub fn emit_voice_presence_updated(payload: VoicePresenceUpdatedPayload) {
    send(VOICE_PRESENCE_UPDATED, payload);
}

pub fn emit_voice_state_changed(is_muted: bool, is_deafened: bool) {
    send(
        VOICE_STATE_CHANGED,
        VoiceStateChangedPayload {
            is_muted,
            is_deafened,
        },
    );
}

pub fn emit_voice_user_speaking(username: String, speaking: bool) {
    send(
        VOICE_USER_SPEAKING,
        VoiceUserSpeakingPayload { username, speaking },
    );
}

pub fn emit_voice_user_state_changed(username: String, is_muted: bool, is_deafened: bool) {
    send(
        VOICE_USER_STATE_CHANGED,
        VoiceUserStateChangedPayload {
            username,
            is_muted,
            is_deafened,
        },
    );
}

pub fn emit_voice_input_level(db: f32) {
    send(VOICE_INPUT_LEVEL, VoiceInputLevelPayload { db });
}

pub fn emit_voice_ping_updated(latency_ms: u32) {
    send(VOICE_PING_UPDATED, VoicePingUpdatedPayload { latency_ms });
}

pub fn emit_voice_connection_stats(latency_ms: Option<u32>, packet_loss_pct: f32) {
    send(
        VOICE_CONNECTION_STATS,
        VoiceConnectionStatsPayload {
            latency_ms,
            packet_loss_pct,
        },
    );
}

pub fn emit_voice_error(message: String) {
    send(VOICE_ERROR, VoiceErrorPayload { message });
}

// ── Streaming ─────────────────────────────────────────────────────

#[derive(Debug, Clone, Serialize)]
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
    /// VideoCodec.UNKNOWN (0) when not enforced. Drives lock icon on the
    /// badge and grayed-out watch button when local client lacks the
    /// codec in its decode caps.
    pub enforced_codec: i32,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamPresenceUpdatedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub streams: Vec<StreamInfoPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamThumbnailUpdatedPayload {
    pub owner_username: String,
    /// Already-prefixed `data:image/jpeg;base64,…` URL ready for `<img src>`.
    pub thumbnail_base64: String,
}

#[derive(Debug, Clone, Serialize)]
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

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamGpuFallbackPayload {
    pub error: String,
}

/// Pushed for every reassembled remote video frame and every locally
/// encoded self-preview frame. Renderer's StreamVideoPlayer filters by
/// `username`, base64-decodes `data` + `description`, and feeds them
/// into a WebCodecs VideoDecoder. PR7c will replace this with a
/// dedicated per-stream Buffer TSFN to avoid base64 + JSON cost.
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamFramePayload {
    pub username: String,
    /// Always "h264" historically — the actual codec is in `codec`.
    /// Kept for back-compat with the tauri-client renderer.
    pub format: String,
    /// Base64-encoded encoded bitstream bytes.
    pub data: String,
    /// Synthetic timestamp = frame_id * 33_333 µs (good enough for
    /// WebCodecs's monotonic timestamp requirement).
    pub timestamp: u64,
    pub keyframe: bool,
    /// Base64-encoded avcC/hvcC/av1C decoder configuration record.
    /// Present on keyframes only.
    pub description: Option<String>,
    /// CodecKind as u8 (1=H264Hw, 2=H264Sw, 3=H265, 4=AV1).
    pub codec: u8,
}

pub fn emit_stream_presence_updated(
    server_id: String,
    channel_id: String,
    streams: Vec<StreamInfoPayload>,
) {
    send(
        STREAM_PRESENCE_UPDATED,
        StreamPresenceUpdatedPayload {
            server_id,
            channel_id,
            streams,
        },
    );
}

pub fn emit_stream_thumbnail_updated(owner_username: String, thumbnail_data: Vec<u8>) {
    use base64::Engine;
    let b64 = base64::engine::general_purpose::STANDARD.encode(&thumbnail_data);
    let data_url = format!("data:image/jpeg;base64,{}", b64);
    send(
        STREAM_THUMBNAIL_UPDATED,
        StreamThumbnailUpdatedPayload {
            owner_username,
            thumbnail_base64: data_url,
        },
    );
}

pub fn emit_stream_codec_changed(payload: StreamCodecChangedPayload) {
    send(STREAM_CODEC_CHANGED, payload);
}

pub fn emit_stream_gpu_fallback(error: String) {
    send(STREAM_GPU_FALLBACK, StreamGpuFallbackPayload { error });
}

pub fn emit_stream_capture_ended() {
    send(STREAM_CAPTURE_ENDED, serde_json::Value::Null);
}

pub fn emit_stream_frame(payload: StreamFramePayload) {
    send(STREAM_FRAME, payload);
}

pub fn emit_caps_refreshed() {
    send(CAPS_REFRESHED, serde_json::Value::Null);
}
