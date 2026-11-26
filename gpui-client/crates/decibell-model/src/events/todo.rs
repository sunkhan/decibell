//! What the features still to be ported will take: every event and hook the foundation
//! routes but does not handle yet, as an empty function naming the renderer code it
//! ports. `m2` is messages, DMs, drafts, attachments, link previews and search; `m3` is
//! voice, streams, calls, presence, friends, E2EE, hotkeys, devices and transfers
//! (docs/model.md §10).
//!
//! Filling one in: give the feature its store (a field on `Model`), replace the body
//! here with the store call (or move the function into the feature's own `events/*.rs`
//! and point the router at it). The router's match needs no other change. Payloads
//! taken by value are the large ones (message lists): move their strings into the
//! store instead of cloning.

/// Messages, DMs, drafts, attachments, link previews, search.
pub mod m2 {
    use decibell_core::events::*;
    use gpui::App;

    use crate::Model;
    use crate::ids::{ChannelId, ServerId};
    use crate::stores::community::Channel;

    /// Central renewed the session token: the attachment endpoints' tokens are read again (`electron/main/attachmentRegistry.ts` `refreshTargetsFromNative`).
    pub fn session_token_updated(model: &Model, cx: &mut App) {
        let _ = (model, cx);
    }

    /// A channel message or DM arrived, or our own echo (`features/chat/useChatEvents.ts`, `features/dm/useDmEvents.ts`).
    pub fn message_received(model: &Model, p: MessageReceivedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A page of channel history (`useChatEvents.ts`, `lib/historyMerge.ts`, `features/chat/historyRequests.ts`).
    pub fn channel_history_received(model: &Model, p: ChannelHistoryReceivedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// Retention pruned messages and attachments (`useChatEvents.ts` → `chatStore.applyChannelPruned`, attachment tombstones).
    pub fn channel_pruned(model: &Model, p: &ChannelPrunedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A channel's history was wiped (`useChatEvents.ts` → `chatStore.applyChannelWiped`).
    pub fn channel_wiped(model: &Model, p: &ChannelWipedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// The community refused one of our messages: withdraw that bubble, give the text back, correct slowmode (`features/chat/sendPacing.ts`, `slowmode.ts`).
    pub fn channel_message_rejected(model: &Model, p: &ChannelMessageRejectedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A page of search results (`features/search/useSearchEvents.ts`).
    pub fn message_search_received(model: &Model, p: MessageSearchReceivedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// The DM conversation list (`features/dm/useDmEvents.ts`).
    pub fn dm_conversations_received(model: &Model, p: DmConversationsReceivedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A page of DM history (`useDmEvents.ts`).
    pub fn dm_history_received(model: &Model, p: DmHistoryReceivedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// Another device of ours read a conversation (`useDmEvents.ts`).
    pub fn dm_read_synced(model: &Model, p: &DmReadSyncedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// Central answered our DM delete (`useDmEvents.ts`).
    pub fn dm_message_delete_responded(model: &Model, p: &DmMessageDeleteRespondedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A DM was deleted (`useDmEvents.ts`).
    pub fn dm_message_deleted(model: &Model, p: &DmMessageDeletedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// The community answered our message delete: restore the bubble and toast on refusal (`useServerEvents.ts`, `chatStore.restorePendingDeletion`).
    pub fn channel_message_delete_responded(model: &Model, p: &ChannelMessageDeleteRespondedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A channel message was deleted (`useServerEvents.ts` → `chatStore.removeMessage`).
    pub fn channel_message_deleted(model: &Model, p: &ChannelMessageDeletedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// Central answered our DM edit (`useDmEvents.ts`).
    pub fn dm_message_edit_responded(model: &Model, p: &DmMessageEditRespondedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A DM was edited (`useDmEvents.ts`).
    pub fn dm_message_edited(model: &Model, p: DmMessageEditedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// The community answered our edit: toast "Couldn't edit message" on refusal (`useServerEvents.ts`).
    pub fn channel_message_edit_responded(model: &Model, p: &ChannelMessageEditRespondedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    /// A channel message was edited (`useServerEvents.ts` → `chatStore.applyEdit`).
    pub fn channel_message_edited(model: &Model, p: ChannelMessageEditedPayload, cx: &mut App) {
        let _ = (model, p, cx);
    }

    // ── Hooks out of the handlers the foundation owns ──

    /// `login_succeeded` (`useAuthEvents.ts`): ask for the DM conversation previews
    /// (`request_dm_conversations`); when `again` (the same account signed back in after
    /// a dropped connection) also `abandonDmRequests()` and `dmStore.markHistoriesStale()`.
    pub fn signed_in(model: &Model, again: bool, cx: &mut App) {
        let _ = (model, again, cx);
    }

    /// `logged_out`: close the image viewer and the image context menu, clear the
    /// attachment endpoints and decryption keys, the DM store, the drafts, the retained
    /// images, the message caches (`chatStore.resetForLogout`'s message half).
    pub fn logged_out(model: &Model, cx: &mut App) {
        let _ = (model, cx);
    }

    /// A community authenticated (first time or again): its message bucket is full
    /// again (`resetSendPacing(serverId)`).
    pub fn reset_send_pacing(model: &Model, server: ServerId, cx: &mut App) {
        let _ = (model, server, cx);
    }

    /// A community authenticated, before its new channel list is stored (review B4):
    /// `abandonChannelRequests(serverId)` then `chatStore.markServerStale(serverId,
    /// channelIds)` (slices of channels gone from `channels` are dropped, caught-up ones
    /// refetch on the next visit, scrolled-up ones go windowed; bumps `resyncEpoch`).
    pub fn community_authenticated(model: &Model, server: ServerId, channels: &[Channel], cx: &mut App) {
        let _ = (model, server, channels, cx);
    }

    /// A community that serves attachments authenticated: read its endpoint and token
    /// (`api::servers::get_attachment_target`) into the attachment service's registry.
    pub fn attachment_target_changed(model: &Model, server: ServerId, cx: &mut App) {
        let _ = (model, server, cx);
    }

    /// A community's connection dropped: `abandonChannelRequests(serverId)` (their
    /// replies went down with it) and forget its attachment endpoint.
    pub fn community_connection_lost(model: &Model, server: ServerId, cx: &mut App) {
        let _ = (model, server, cx);
    }

    /// A channel no longer exists (`channel_list_updated`): `chatStore.purgeChannelState`.
    pub fn purge_channel_state(model: &Model, server: ServerId, channel: &ChannelId, cx: &mut App) {
        let _ = (model, server, channel, cx);
    }

    /// `legacyRejectionSuppressed()` (`sendPacing.ts`): a refused message was already
    /// shown through `channel_message_rejected`, so the legacy `mod_action_responded`
    /// echo of it stays silent. Until send pacing is ported this is false, which shows
    /// the community's refusal text as a toast.
    pub fn legacy_rejection_suppressed(model: &Model, cx: &mut App) -> bool {
        let _ = (model, cx);
        false
    }
}

/// Voice, streams, calls, presence, friends, E2EE, hotkeys, devices, transfers.
pub mod m3 {
    use decibell_core::events::*;
    use gpui::App;

    use crate::Model;
    use crate::ids::{ServerId, Uid};
    use crate::stores::community::Channel;

    /// A stream's JPEG thumbnail (the sink's own method in the core; an ordinary queued
    /// item here). `features/voice/useVoiceEvents.ts` (`stream_thumbnail` → a blob URL):
    /// decode at tile size into the image cache, keyed by `thumbnail.owner_uid`.
    pub fn stream_thumbnail(model: &Model, thumbnail: StreamThumbnail, cx: &mut App) {
        super::super::voice::stream_thumbnail(model, thumbnail, cx);
    }

    /// A user's picture changed (`useAuthEvents.ts` → `avatarStore.setVersion`).
    pub fn avatar_changed(model: &Model, p: &AvatarChangedPayload, cx: &mut App) {
        super::super::presence::avatar_changed(model, p, cx);
    }

    /// The account's device list changed: an open Devices tab reloads (`devicesStore.bump`), and a new device is announced with a 20 s warning toast "New sign-in to your account" whose action opens Settings → Devices (`useAuthEvents.ts`, `features/settings/deviceLabels.ts`).
    pub fn devices_changed(model: &Model, p: &DevicesChangedPayload, cx: &mut App) {
        super::super::misc::devices_changed(model, p, cx);
    }

    /// Presence of newly watched users (`features/presence/usePresenceEvents.ts`, `presenceStore`, `avatarStore`).
    pub fn user_list_updated(model: &Model, p: &UserListUpdatedPayload, cx: &mut App) {
        super::super::presence::user_list_updated(model, p, cx);
    }

    /// A watched user's presence changed (`usePresenceEvents.ts`).
    pub fn presence_delta(model: &Model, p: &PresenceDeltaPayload, cx: &mut App) {
        super::super::presence::presence_delta(model, p, cx);
    }

    /// Our own status, from the sign-in reply or any device's change (`usePresenceEvents.ts`; also adopts the account's friends-only-DMs value).
    pub fn own_presence(model: &Model, p: &OwnPresencePayload, cx: &mut App) {
        super::super::presence::own_presence(model, p, cx);
    }

    /// The friend list (`features/friends/useFriendsEvents.ts`).
    pub fn friend_list_received(model: &Model, p: &FriendListReceivedPayload, cx: &mut App) {
        super::super::friends::friend_list_received(model, p, cx);
    }

    /// Central answered a friend action (`useFriendsEvents.ts`).
    pub fn friend_action_responded(model: &Model, p: &FriendActionRespondedPayload, cx: &mut App) {
        super::super::friends::friend_action_responded(model, p, cx);
    }

    /// A moderator moved or disconnected us in voice (`useServerEvents.ts`: stop our stream, `enterChannel`, toasts "Moved" / "Disconnected").
    pub fn voice_force_notify(model: &Model, p: &VoiceForceNotifyPayload, cx: &mut App) {
        super::super::voice::voice_force_notify(model, p, cx);
    }

    /// Who is in a voice channel (`features/voice/useVoiceEvents.ts`).
    pub fn voice_presence_updated(model: &Model, p: &VoicePresenceUpdatedPayload, cx: &mut App) {
        super::super::voice::voice_presence_updated(model, p, cx);
    }

    /// Our own mute / deafen (`useVoiceEvents.ts`).
    pub fn voice_state_changed(model: &Model, p: &VoiceStateChangedPayload, cx: &mut App) {
        super::super::voice::voice_state_changed(model, p, cx);
    }

    /// A global hotkey fired (`features/hotkeys/hotkeyRuntime.ts`).
    pub fn hotkey_action(model: &Model, p: &HotkeyActionPayload, cx: &mut App) {
        super::super::misc::hotkey_action(model, p, cx);
    }

    /// The hotkey listener's status (`hotkeysStore`, Keybinds tab).
    pub fn hotkeys_status(model: &Model, p: &decibell_core::hotkeys::Status, cx: &mut App) {
        super::super::misc::hotkeys_status(model, p, cx);
    }

    /// A participant started or stopped speaking (`useVoiceEvents.ts`). High rate: its own small entity.
    pub fn voice_user_speaking(model: &Model, p: &VoiceUserSpeakingPayload, cx: &mut App) {
        super::super::voice::voice_user_speaking(model, p, cx);
    }

    /// A participant's self mute / deafen (`useVoiceEvents.ts`).
    pub fn voice_user_state_changed(model: &Model, p: &VoiceUserStateChangedPayload, cx: &mut App) {
        super::super::voice::voice_user_state_changed(model, p, cx);
    }

    /// Our mic level, ~16 a second while Settings → Audio's meter asked for it (`AudioTab.tsx`). Its own entity; never the voice store.
    pub fn voice_input_level(model: &Model, p: &VoiceInputLevelPayload, cx: &mut App) {
        super::super::voice::voice_input_level(model, p, cx);
    }

    /// A voice ping round trip (`voiceStatsStore`).
    pub fn voice_ping_updated(model: &Model, p: &VoicePingUpdatedPayload, cx: &mut App) {
        super::super::voice::voice_ping_updated(model, p, cx);
    }

    /// Periodic voice link stats (`voiceStatsStore`).
    pub fn voice_connection_stats(model: &Model, p: &VoiceConnectionStatsPayload, cx: &mut App) {
        super::super::voice::voice_connection_stats(model, p, cx);
    }

    /// The voice engine hit an error (`useVoiceEvents.ts`).
    pub fn voice_error(model: &Model, p: &VoiceErrorPayload, cx: &mut App) {
        super::super::voice::voice_error(model, p, cx);
    }

    /// The streams live in a voice channel (`useVoiceEvents.ts`).
    pub fn stream_presence_updated(model: &Model, p: &StreamPresenceUpdatedPayload, cx: &mut App) {
        super::super::voice::stream_presence_updated(model, p, cx);
    }

    /// A streamer switched codec, resolution or frame rate (`useVoiceEvents.ts`).
    pub fn stream_codec_changed(model: &Model, p: &StreamCodecChangedPayload, cx: &mut App) {
        super::super::voice::stream_codec_changed(model, p, cx);
    }

    /// Hardware encode fell back (no emitter in the core today; `useVoiceEvents.ts`).
    pub fn stream_gpu_fallback(model: &Model, p: &StreamGpuFallbackPayload, cx: &mut App) {
        super::super::voice::stream_gpu_fallback(model, p, cx);
    }

    /// Our capture source ended: the stream ends (`useVoiceEvents.ts`).
    pub fn stream_capture_ended(model: &Model, cx: &mut App) {
        super::super::voice::stream_capture_ended(model, cx);
    }

    /// Our stream needs a keyframe (the renderer-encode path; with native encode the core handles it).
    pub fn keyframe_requested(model: &Model, cx: &mut App) {
        super::super::voice::keyframe_requested(model, cx);
    }

    /// Our native encode loop died while streaming (`features/voice/streaming/StreamCapture.ts`).
    pub fn native_stream_failed(model: &Model, p: &NativeStreamFailedPayload, cx: &mut App) {
        super::super::voice::native_stream_failed(model, p, cx);
    }

    /// The local codec capabilities were re-probed (no emitter in the core today).
    pub fn caps_refreshed(model: &Model, cx: &mut App) {
        super::super::voice::caps_refreshed(model, cx);
    }

    /// Our E2EE status (`features/e2ee/`, `e2eeStore`).
    pub fn e2ee_status_changed(model: &Model, p: &E2eeStatusPayload, cx: &mut App) {
        super::super::e2ee::e2ee_status_changed(model, p, cx);
    }

    /// A pinned peer's identity changed (`e2eeStore`).
    pub fn e2ee_peer_changed(model: &Model, p: &E2eePeerChangedPayload, cx: &mut App) {
        super::super::e2ee::e2ee_peer_changed(model, p, cx);
    }

    /// The MLS group behind our voice channel changed state (`useVoiceEvents.ts`).
    pub fn voice_e2ee_state(model: &Model, p: &VoiceE2eeStatePayload, cx: &mut App) {
        super::super::voice::voice_e2ee_state(model, p, cx);
    }

    /// A call signal (`features/call/useCallEvents.ts`).
    pub fn call_signal(model: &Model, p: &CallSignalPayload, cx: &mut App) {
        super::super::calls::call_signal(model, p, cx);
    }

    /// The call's media is up (`useCallEvents.ts`).
    pub fn call_connected(model: &Model, p: &CallConnectedPayload, cx: &mut App) {
        super::super::calls::call_connected(model, p, cx);
    }

    /// The call could not connect (`useCallEvents.ts`).
    pub fn call_failed(model: &Model, p: &CallFailedPayload, cx: &mut App) {
        super::super::calls::call_failed(model, p, cx);
    }

    /// The call lost its peer (`useCallEvents.ts`).
    pub fn call_dropped(model: &Model, p: &CallDroppedPayload, cx: &mut App) {
        super::super::calls::call_dropped(model, p, cx);
    }

    // ── Hooks out of the handlers the foundation owns ──

    /// `login_succeeded`: `avatarStore.seed(uid)` (others' pictures come with their
    /// presence; nothing sends our own).
    pub fn seed_own_avatar(model: &Model, uid: Uid, cx: &mut App) {
        super::super::presence::seed_own_avatar(model, uid, cx);
    }

    /// `login_succeeded`: when `again`, `requestFriendList()`; and the presence
    /// subscriptions are re-sent by the core itself.
    pub fn signed_in(model: &Model, again: bool, cx: &mut App) {
        super::super::misc::signed_in(model, again, cx);
    }

    /// `logged_out`: `stopActiveStream()`, `stopRing()`, `callStore.reset()`,
    /// `voiceStore.disconnect()`, `presenceStore.reset()`, the friend list, the avatar
    /// cache.
    pub fn logged_out(model: &Model, cx: &mut App) {
        super::super::misc::logged_out(model, cx);
    }

    /// `voiceStore.clearServerPresence(serverId, keep)`: drop the voice and stream
    /// rosters of `server`'s channels. `keep` none: all of them (a (re)authentication's
    /// snapshot follows, a dropped connection, a revoked membership); `keep` some: only
    /// the channels not in the new list.
    pub fn clear_server_presence(model: &Model, server: ServerId, keep: Option<&[Channel]>, cx: &mut App) {
        super::super::voice::clear_server_presence(model, server, keep, cx);
    }

    /// `channel_updated`, before the channel is stored: when it is the voice channel we
    /// are connected to and `channel.voice_bitrate_kbps` differs from
    /// `previous_bitrate`, retune the running encoder (`api::voice::set_voice_bitrate`).
    pub fn channel_updated(model: &Model, server: ServerId, channel: &Channel, previous_bitrate: Option<i32>, cx: &mut App) {
        super::super::voice::channel_updated(model, server, channel, previous_bitrate, cx);
    }
}
