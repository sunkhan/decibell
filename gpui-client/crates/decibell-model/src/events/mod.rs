//! The router: every event the core emits, matched once and handed to the stores.
//!
//! One module per event area, mirroring the renderer's hooks: `auth.rs`
//! (`useAuthEvents`), `servers.rs` (`useServerEvents`), `connection.rs`
//! (`useCentralConnectionStatus`), `users.rs` (`userDirectory.ts`); `todo.rs` lists what
//! the features still to be ported will take, as empty functions with the renderer file
//! each one ports. **Both matches (here and in `users.rs`) name all 86 variants with no
//! wildcard**: a new core event doesn't compile until someone decides where it goes.
//!
//! Called by the pump for each item of a batch, inside one `App::update`
//! (`crate::pump`). Handlers run on the UI thread and only touch stores; anything slow
//! goes through `crate::call`.

pub mod auth;
pub mod calls;
pub mod chat;
pub mod connection;
pub mod dm;
pub mod e2ee;
pub mod friends;
pub mod misc;
pub mod presence;
pub mod servers;
#[cfg(test)]
mod tests;
pub mod todo;
pub mod users;
pub mod voice;

use decibell_core::events::CoreEvent;
use gpui::App;

use crate::Model;
use crate::pump::Incoming;

/// Apply one queued item.
pub fn apply(model: &Model, item: Incoming, cx: &mut App) {
    match item {
        Incoming::Core(event) => route(model, event, cx),
        Incoming::Thumbnail(thumbnail) => todo::m3::stream_thumbnail(model, thumbnail, cx),
    }
}

/// Emit an answer no store keeps to the views that listen for it (`stores/bus.rs`).
fn answer(model: &Model, event: CoreEvent, cx: &mut App) {
    model.answers.update(cx, |_, cx| cx.emit(event));
}

/// Hand `event` to the stores. The users directory sees it first (so a handler that
/// reads a name finds it), then its own area.
pub fn route(model: &Model, event: CoreEvent, cx: &mut App) {
    users::directory(model, &event, cx);
    use CoreEvent as E;
    match event {
        // ── Connections, sign-in, account ──
        E::ConnectionLost(p) => {
            connection::lost(model, &p, cx);
            servers::connection_lost(model, &p, cx);
            dm::connection_lost(model, &p, cx);
        }
        E::ConnectionRestored(p) => {
            connection::restored(model, &p, cx);
            servers::connection_restored(model, &p, cx);
        }
        E::LoginSucceeded(p) => auth::login_succeeded(model, &p, cx),
        E::UserRenamed(p) => auth::user_renamed(model, &p, cx),
        E::AvatarChanged(p) => todo::m3::avatar_changed(model, &p, cx),
        E::LoginFailed(p) => auth::login_failed(model, &p, cx),
        E::RegisterResponded(p) => auth::register_responded(model, &p, cx),
        E::LoggedOut(p) => auth::logged_out(model, &p, cx),
        E::DevicesChanged(p) => todo::m3::devices_changed(model, &p, cx),
        E::SessionTokenUpdated => todo::m2::session_token_updated(model, cx),

        // ── Communities: list, membership, connection ──
        E::ServerListReceived(p) => servers::server_list_received(model, &p, cx),
        E::MembershipsReceived(p) => servers::memberships_received(model, &p, cx),
        E::MembershipAdded(p) => servers::membership_added(model, &p, cx),
        E::CommunityAuthResponded(p) => servers::community_auth_responded(model, &p, cx),

        // ── Messages and channels ──
        E::MessageReceived(p) => todo::m2::message_received(model, p, cx),
        E::ChannelHistoryReceived(p) => todo::m2::channel_history_received(model, p, cx),
        E::ChannelPruned(p) => todo::m2::channel_pruned(model, &p, cx),
        E::ChannelUpdated(p) => {
            servers::channel_updated(model, &p, cx);
            answer(model, E::ChannelUpdated(p), cx);
        }
        E::ChannelWipeResponded(p) => answer(model, E::ChannelWipeResponded(p), cx),
        E::ChannelWiped(p) => todo::m2::channel_wiped(model, &p, cx),

        // ── Friends + presence ──
        E::UserListUpdated(p) => todo::m3::user_list_updated(model, &p, cx),
        E::PresenceDelta(p) => todo::m3::presence_delta(model, &p, cx),
        E::OwnPresence(p) => todo::m3::own_presence(model, &p, cx),
        // Comes only after `api::presence::subscribe_system_idle`, which the
        // model doesn't call yet (auto-away still asks `system_idle_state`).
        E::SystemIdleChanged(_) => {}
        E::FriendListReceived(p) => todo::m3::friend_list_received(model, &p, cx),
        E::FriendActionResponded(p) => todo::m3::friend_action_responded(model, &p, cx),

        // ── Members + moderation ──
        E::MemberListReceived(p) => servers::member_list_received(model, &p, cx),
        E::ModActionResponded(p) => {
            servers::mod_action_responded(model, &p, cx);
            answer(model, E::ModActionResponded(p), cx);
        }
        E::ChannelMessageRejected(p) => todo::m2::channel_message_rejected(model, &p, cx),
        E::MembershipRevoked(p) => servers::membership_revoked(model, &p, cx),
        E::RoleListReceived(p) => servers::role_list_received(model, &p, cx),
        E::MemberUpsert(p) => servers::member_upsert(model, &p, cx),
        E::MemberRemove(p) => servers::member_remove(model, &p, cx),
        E::BanListReceived(p) => servers::ban_list_received(model, &p, cx),
        E::ServerMetaUpdated(p) => servers::server_meta_updated(model, &p, cx),
        E::ServerUpdateResponded(p) => answer(model, E::ServerUpdateResponded(p), cx),
        E::AuditLogReceived(p) => answer(model, E::AuditLogReceived(p), cx),
        E::MemberSearchReceived(p) => answer(model, E::MemberSearchReceived(p), cx),
        E::MessageSearchReceived(p) => todo::m2::message_search_received(model, p, cx),
        E::StorageInfoReceived(p) => answer(model, E::StorageInfoReceived(p), cx),
        E::VoiceForceNotify(p) => todo::m3::voice_force_notify(model, &p, cx),

        // ── Roles + permissions, channel management ──
        E::ChannelOverwritesReceived(p) => servers::channel_overwrites_received(model, &p, cx),
        E::RoleActionResponded(p) => {
            servers::role_action_responded(model, &p, cx);
            answer(model, E::RoleActionResponded(p), cx);
        }
        E::ChannelListUpdated(p) => servers::channel_list_updated(model, &p, cx),
        E::ChannelActionResponded(p) => {
            servers::channel_action_responded(model, &p, cx);
            answer(model, E::ChannelActionResponded(p), cx);
        }

        // ── Invites ──
        E::InviteListReceived(p) => servers::invite_list_received(model, &p, cx),
        E::InviteCreateResponded(p) => {
            servers::invite_create_responded(model, &p, cx);
            answer(model, E::InviteCreateResponded(p), cx);
        }
        E::InviteRevokeResponded(p) => servers::invite_revoke_responded(model, &p, cx),

        // ── Voice ──
        E::VoicePresenceUpdated(p) => todo::m3::voice_presence_updated(model, &p, cx),
        E::VoiceStateChanged(p) => todo::m3::voice_state_changed(model, &p, cx),
        E::HotkeyAction(p) => todo::m3::hotkey_action(model, &p, cx),
        E::HotkeysStatus(p) => todo::m3::hotkeys_status(model, &p, cx),
        E::VoiceUserSpeaking(p) => todo::m3::voice_user_speaking(model, &p, cx),
        E::VoiceUserStateChanged(p) => todo::m3::voice_user_state_changed(model, &p, cx),
        E::VoiceInputLevel(p) => todo::m3::voice_input_level(model, &p, cx),
        E::VoicePingUpdated(p) => todo::m3::voice_ping_updated(model, &p, cx),
        E::VoiceConnectionStats(p) => todo::m3::voice_connection_stats(model, &p, cx),
        E::VoiceError(p) => todo::m3::voice_error(model, &p, cx),

        // ── Streaming ──
        E::StreamPresenceUpdated(p) => todo::m3::stream_presence_updated(model, &p, cx),
        E::StreamCodecChanged(p) => todo::m3::stream_codec_changed(model, &p, cx),
        E::StreamGpuFallback(p) => todo::m3::stream_gpu_fallback(model, &p, cx),
        E::StreamCaptureEnded => todo::m3::stream_capture_ended(model, cx),
        E::KeyframeRequested => todo::m3::keyframe_requested(model, cx),
        E::NativeStreamFailed(p) => todo::m3::native_stream_failed(model, &p, cx),
        E::CapsRefreshed => todo::m3::caps_refreshed(model, cx),

        // ── DMs ──
        E::DmConversationsReceived(p) => todo::m2::dm_conversations_received(model, p, cx),
        E::DmHistoryReceived(p) => todo::m2::dm_history_received(model, p, cx),
        E::DmReadSynced(p) => todo::m2::dm_read_synced(model, &p, cx),
        E::DmMessageDeleteResponded(p) => todo::m2::dm_message_delete_responded(model, &p, cx),
        E::DmMessageDeleted(p) => todo::m2::dm_message_deleted(model, &p, cx),
        E::ChannelMessageDeleteResponded(p) => todo::m2::channel_message_delete_responded(model, &p, cx),
        E::ChannelMessageDeleted(p) => todo::m2::channel_message_deleted(model, &p, cx),
        E::DmMessageEditResponded(p) => todo::m2::dm_message_edit_responded(model, &p, cx),
        E::DmMessageEdited(p) => todo::m2::dm_message_edited(model, p, cx),

        // ── Encryption ──
        E::E2eeStatusChanged(p) => todo::m3::e2ee_status_changed(model, &p, cx),
        E::E2eePeerChanged(p) => todo::m3::e2ee_peer_changed(model, &p, cx),
        E::VoiceE2eeState(p) => todo::m3::voice_e2ee_state(model, &p, cx),
        E::ChannelMessageEditResponded(p) => todo::m2::channel_message_edit_responded(model, &p, cx),
        E::ChannelMessageEdited(p) => todo::m2::channel_message_edited(model, p, cx),

        // ── Server pictures ──
        E::ServerPictureUpdateResponded(p) => {
            servers::server_picture_update_responded(model, &p, cx);
            answer(model, E::ServerPictureUpdateResponded(p), cx);
        }
        E::ServerPictureReceived(p) => servers::server_picture_received(model, p, cx),
        E::ServerPictureChanged(p) => servers::server_picture_changed(model, &p, cx),

        // ── P2P DM calls ──
        E::CallSignal(p) => todo::m3::call_signal(model, &p, cx),
        E::CallConnected(p) => todo::m3::call_connected(model, &p, cx),
        E::CallFailed(p) => todo::m3::call_failed(model, &p, cx),
        E::CallDropped(p) => todo::m3::call_dropped(model, &p, cx),
    }
}
