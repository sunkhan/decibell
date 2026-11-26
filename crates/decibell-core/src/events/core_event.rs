//! [`CoreEvent`]: every named event the core emits, typed. One table below
//! declares each variant, its payload struct (the ones in `events.rs`) and its
//! wire name (the constants there); the enum, [`CoreEvent::NAMES`] and
//! [`CoreEvent::name`] are generated from it, so a variant can't lose its name.
//!
//! The Electron addon's sink turns a variant back into today's `{name,
//! payload}` JSON with [`CoreEvent::to_json`] (pinned byte for byte by
//! `tests/golden/events.json`); a native client matches on the variant and
//! reads the payload structs directly, with no JSON in between.
//!
//! Reading the payloads natively — what the renderer's hand-written types
//! (`electron-client/src/types/index.ts`) assume:
//! - **Users are uids.** Every user reference carries an `i64` `*_uid` (or
//!   `uid`) next to a name (`sender`, `username`, `actor`, `peer`, …). Key
//!   state on the uid; the name is the current name to show (a fallback for
//!   the client's own user directory). A negative uid is a community
//!   placeholder (a member row without a central account), `0` is "unknown /
//!   none" (`VoiceUserSpeakingPayload::uid` 0 = ourselves). Voice
//!   `participants`, `VoiceE2eeStatePayload::unverified` and
//!   `StreamFrame::key` are decimal uid strings (the media identity).
//! - **Server ids:** community payloads carry `server_id: String` (the
//!   central-assigned id in decimal, or `"host:port"` for a connection opened
//!   from an invite before it was re-keyed: see `CommunityAuthResponded`);
//!   central's `ServerInfo::id` and the server-picture payloads use `i32`.
//!   Channel ids are strings.
//! - **Empty means none** for most strings (`avatar_version`,
//!   `picture_version`, `decrypt_error`, `nickname`, `topic`, `message` on
//!   success); `Option` fields are `null` in JSON. Zero means none for most
//!   counters and times (`edited_at`, `reply_to`, `expires_at`, `purged_at`).
//! - **Large integers:** message ids, cursors and other `i64` / `u64` fields
//!   can exceed 2^53 (the renderer receives those as BigInt); `u64`
//!   permission bits and revisions are full 64-bit. Times are unix seconds
//!   unless the field says `_ms`; `MessageReceivedPayload::timestamp` is a
//!   decimal string, the history payloads' are `i64`.
//! - **Encrypted content arrives opened**: `encrypted` says it was sealed on
//!   the wire, `decrypt_error` ("locked" | "no_key" | "peer_key" | "bad") says
//!   it couldn't be opened and `content` is a placeholder.

use serde::Serialize;

use super::*;

macro_rules! core_events {
    ($(
        $(#[$doc:meta])*
        $variant:ident $(($payload:ty))? => $name:ident;
    )*) => {
        /// A named UI event, as the core hands it to [`EventSink::event`].
        ///
        /// Serialises (untagged) as its payload alone, `null` for the
        /// payload-less variants; [`CoreEvent::to_json`] pairs that with the
        /// name. Stream frames and thumbnails are not events: they have their
        /// own binary sink methods.
        #[derive(Debug, Clone, Serialize)]
        #[serde(untagged)]
        pub enum CoreEvent {
            $( $(#[$doc])* $variant $(($payload))?, )*
        }

        impl CoreEvent {
            /// Every event name, in declaration order.
            pub const NAMES: &'static [&'static str] = &[$($name),*];

            /// The wire name: the event constant the renderer's
            /// `listen(name, …)` matches.
            pub fn name(&self) -> &'static str {
                match self {
                    $( Self::$variant { .. } => $name, )*
                }
            }
        }
    };
}

impl CoreEvent {
    /// `(name, payload)` exactly as the untyped bus sent it before events
    /// were typed: the payload struct serialised (camelCase), `null` for the
    /// payload-less events.
    pub fn to_json(&self) -> (&'static str, serde_json::Value) {
        (self.name(), serde_json::to_value(self).unwrap_or(serde_json::Value::Null))
    }
}

core_events! {
    // ── Connections, sign-in, account ──
    /// A central or community connection dropped.
    ConnectionLost(ConnectionEventPayload) => CONNECTION_LOST;
    /// A dropped central or community connection is back.
    ConnectionRestored(ConnectionEventPayload) => CONNECTION_RESTORED;
    /// Central accepted our sign-in.
    LoginSucceeded(LoginSucceededPayload) => LOGIN_SUCCEEDED;
    /// A user changed their username: ours after `rename_account` (`is_self`), anyone else's from USER_RENAMED.
    UserRenamed(UserRenamedPayload) => USER_RENAMED;
    /// A user's picture changed: central's AVATAR_CHANGED push, or our own upload succeeding (we don't watch ourselves).
    AvatarChanged(AvatarChangedPayload) => AVATAR_CHANGED;
    /// Central refused a sign-in.
    LoginFailed(LoginFailedPayload) => LOGIN_FAILED;
    /// Central answered a registration.
    RegisterResponded(RegisterRespondedPayload) => REGISTER_RESPONDED;
    /// This device is signed out: our own Log out (`reason` null), another device signing it out, or a password change.
    LoggedOut(LoggedOutPayload) => LOGGED_OUT;
    /// The account's device list changed on central; `new_device` is set for a typed sign-in on a new device.
    DevicesChanged(DevicesChangedPayload) => DEVICES_CHANGED;
    /// Central renewed our session token (no payload: the token never rides an event).
    SessionTokenUpdated => SESSION_TOKEN_UPDATED;

    // ── Communities: list, membership, connection ──
    /// Central's public community list.
    ServerListReceived(ServerListReceivedPayload) => SERVER_LIST_RECEIVED;
    /// The communities the account belongs to, from the sign-in reply (auto-rejoin tiles).
    MembershipsReceived(MembershipsReceivedPayload) => MEMBERSHIPS_RECEIVED;
    /// Another device of the account joined a community we aren't connected to (MEMBERSHIP_ADDED): show its tile; core then connects.
    MembershipAdded(ServerInfo) => MEMBERSHIP_ADDED;
    /// A community answered our authentication (channels, server meta, limits), or refused it.
    CommunityAuthResponded(CommunityAuthRespondedPayload) => COMMUNITY_AUTH_RESPONDED;

    // ── Messages and channels ──
    /// A channel message or DM arrived (or our own echo), decrypted when it was sealed.
    MessageReceived(MessageReceivedPayload) => MESSAGE_RECEIVED;
    /// A page of channel history (decrypted), echoing the request's mode.
    ChannelHistoryReceived(ChannelHistoryReceivedPayload) => CHANNEL_HISTORY_RECEIVED;
    /// Retention pruned messages and attachments of a channel.
    ChannelPruned(ChannelPrunedPayload) => CHANNEL_PRUNED;
    /// The community answered a channel settings update.
    ChannelUpdated(ChannelUpdatedPayload) => CHANNEL_UPDATED;
    /// The community answered our request to wipe a channel's history.
    ChannelWipeResponded(ChannelWipeRespondedPayload) => CHANNEL_WIPE_RESPONDED;
    /// A channel's history was wiped (by anyone).
    ChannelWiped(ChannelWipedPayload) => CHANNEL_WIPED;

    // ── Friends + presence ──
    /// Central answered newly watched users with their presence (`covered`: the ones it answered for).
    UserListUpdated(UserListUpdatedPayload) => USER_LIST_UPDATED;
    /// A watched user's presence changed (the full current state).
    PresenceDelta(PresenceDeltaPayload) => PRESENCE_DELTA;
    /// Our own status, from the sign-in reply or OWN_PRESENCE (any device's change).
    OwnPresence(OwnPresencePayload) => OWN_PRESENCE;
    /// The system went idle past the subscribed threshold / input resumed (`system_idle::subscribe`; only after a client subscribed: Electron never does).
    SystemIdleChanged(SystemIdleChangedPayload) => SYSTEM_IDLE_CHANGED;
    /// An audio device appeared or went, or a default device changed (after `api::audio::watch_devices`).
    AudioDevicesChanged => AUDIO_DEVICES_CHANGED;
    /// The account's friend list (incl. pending and blocked).
    FriendListReceived(FriendListReceivedPayload) => FRIEND_LIST_RECEIVED;
    /// Central answered a friend action.
    FriendActionResponded(FriendActionRespondedPayload) => FRIEND_ACTION_RESPONDED;

    // ── Members + moderation ──
    /// One page of a community's roster (or a lookup's reply).
    MemberListReceived(MemberListReceivedPayload) => MEMBER_LIST_RECEIVED;
    /// The community answered a kick / ban / leave.
    ModActionResponded(ModActionRespondedPayload) => MOD_ACTION_RESPONDED;
    /// The community refused one of our channel messages (CHANNEL_MSG_REJECTED; `nonce` names the optimistic bubble).
    ChannelMessageRejected(ChannelMessageRejectedPayload) => CHANNEL_MESSAGE_REJECTED;
    /// We were kicked or banned from a community.
    MembershipRevoked(MembershipRevokedPayload) => MEMBERSHIP_REVOKED;
    /// A community's roles (most senior first).
    RoleListReceived(RoleListReceivedPayload) => ROLE_LIST_RECEIVED;
    /// A roster delta: a member joined or changed.
    MemberUpsert(MemberUpsertPayload) => MEMBER_UPSERT;
    /// A roster delta: a member left.
    MemberRemove(MemberRemovePayload) => MEMBER_REMOVE;
    /// A community's ban list (newest first).
    BanListReceived(BanListReceivedPayload) => BAN_LIST_RECEIVED;
    /// A community's name / description / owner / listing changed.
    ServerMetaUpdated(ServerMetaUpdatedPayload) => SERVER_META_UPDATED;
    /// The community answered a server settings update.
    ServerUpdateResponded(ServerUpdateRespondedPayload) => SERVER_UPDATE_RESPONDED;
    /// A page of a community's audit log (`seq` echoes the request).
    AuditLogReceived(AuditLogReceivedPayload) => AUDIT_LOG_RECEIVED;
    /// A page of a server-side member search (`seq` echoes the request).
    MemberSearchReceived(MemberSearchReceivedPayload) => MEMBER_SEARCH_RECEIVED;
    /// A page of message-search results, opened like history (`seq` echoes the request).
    MessageSearchReceived(MessageSearchReceivedPayload) => MESSAGE_SEARCH_RECEIVED;
    /// A community's storage usage (Server Settings → Storage).
    StorageInfoReceived(StorageInfoReceivedPayload) => STORAGE_INFO_RECEIVED;
    /// A moderator moved or disconnected us in voice.
    VoiceForceNotify(VoiceForceNotifyPayload) => VOICE_FORCE_NOTIFY;

    // ── Roles + permissions, channel management ──
    /// A channel's permission overwrites.
    ChannelOverwritesReceived(ChannelOverwritesReceivedPayload) => CHANNEL_OVERWRITES_RECEIVED;
    /// The community answered a role create / update / delete / assign.
    RoleActionResponded(RoleActionRespondedPayload) => ROLE_ACTION_RESPONDED;
    /// The community's full ordered channel list, after any channel create / rename / delete.
    ChannelListUpdated(ChannelListUpdatedPayload) => CHANNEL_LIST_UPDATED;
    /// The community answered a channel create / rename / delete.
    ChannelActionResponded(ChannelActionRespondedPayload) => CHANNEL_ACTION_RESPONDED;

    // ── Invites ──
    /// A community's invites (newest first).
    InviteListReceived(InviteListReceivedPayload) => INVITE_LIST_RECEIVED;
    /// The community answered an invite creation (`request_id` echoes ours).
    InviteCreateResponded(InviteCreateRespondedPayload) => INVITE_CREATE_RESPONDED;
    /// The community answered an invite revocation.
    InviteRevokeResponded(InviteRevokeRespondedPayload) => INVITE_REVOKE_RESPONDED;

    // ── Voice ──
    /// Who is in a voice channel, with their mute states and codec capabilities.
    VoicePresenceUpdated(VoicePresenceUpdatedPayload) => VOICE_PRESENCE_UPDATED;
    /// Our own mute / deafen after joining, leaving or toggling (voice channel or call).
    VoiceStateChanged(VoiceStateChangedPayload) => VOICE_STATE_CHANGED;
    /// A global hotkey fired (hold actions also report their release).
    HotkeyAction(HotkeyActionPayload) => HOTKEY_ACTION;
    /// The global-hotkey listener's status changed (Keybinds tab).
    HotkeysStatus(crate::hotkeys::Status) => HOTKEYS_STATUS;
    /// A voice participant started or stopped speaking.
    VoiceUserSpeaking(VoiceUserSpeakingPayload) => VOICE_USER_SPEAKING;
    /// A voice participant's self mute / deafen, from the media plane.
    VoiceUserStateChanged(VoiceUserStateChangedPayload) => VOICE_USER_STATE_CHANGED;
    /// Our mic level, ~16 a second, only while input-level reporting is on (Settings → Audio's meter).
    VoiceInputLevel(VoiceInputLevelPayload) => VOICE_INPUT_LEVEL;
    /// A voice ping round trip was measured.
    VoicePingUpdated(VoicePingUpdatedPayload) => VOICE_PING_UPDATED;
    /// Periodic voice link stats (latency, packet loss).
    VoiceConnectionStats(VoiceConnectionStatsPayload) => VOICE_CONNECTION_STATS;
    /// The voice engine hit an error.
    VoiceError(VoiceErrorPayload) => VOICE_ERROR;

    // ── Streaming ──
    /// The streams live in a voice channel.
    StreamPresenceUpdated(StreamPresenceUpdatedPayload) => STREAM_PRESENCE_UPDATED;
    /// A streamer in the channel switched codec, resolution or frame rate.
    StreamCodecChanged(StreamCodecChangedPayload) => STREAM_CODEC_CHANGED;
    /// Hardware encode failed and the stream fell back (no emitter today).
    StreamGpuFallback(StreamGpuFallbackPayload) => STREAM_GPU_FALLBACK;
    /// Our native capture source ended (window closed, portal session gone): the stream ends.
    StreamCaptureEnded => STREAM_CAPTURE_ENDED;
    /// Our stream needs a keyframe: a watcher's PLI, or the renderer-encode send queue shed frames.
    KeyframeRequested => KEYFRAME_REQUESTED;
    /// Our native encode loop died (encoder or capture failure, or a panic) while streaming; not sent for a requested stop.
    NativeStreamFailed(NativeStreamFailedPayload) => NATIVE_STREAM_FAILED;
    /// The local codec capabilities were re-probed (no emitter today).
    CapsRefreshed => CAPS_REFRESHED;

    // ── DMs ──
    /// The DM conversation list with last-message previews (decrypted).
    DmConversationsReceived(DmConversationsReceivedPayload) => DM_CONVERSATIONS_RECEIVED;
    /// A page of DM history with one peer (decrypted), echoing the request's mode.
    DmHistoryReceived(DmHistoryReceivedPayload) => DM_HISTORY_RECEIVED;
    /// Another device of ours read a DM conversation up to a message (DM_READ_SYNC).
    DmReadSynced(DmReadSyncedPayload) => DM_READ_SYNCED;
    /// Central answered our DM delete.
    DmMessageDeleteResponded(DmMessageDeleteRespondedPayload) => DM_MESSAGE_DELETE_RESPONDED;
    /// A DM was deleted (by either side, on any device).
    DmMessageDeleted(DmMessageDeletedPayload) => DM_MESSAGE_DELETED;
    /// The community answered our channel message delete.
    ChannelMessageDeleteResponded(ChannelMessageDeleteRespondedPayload) => CHANNEL_MESSAGE_DELETE_RESPONDED;
    /// A channel message was deleted.
    ChannelMessageDeleted(ChannelMessageDeletedPayload) => CHANNEL_MESSAGE_DELETED;
    /// Central answered our DM edit.
    DmMessageEditResponded(DmMessageEditRespondedPayload) => DM_MESSAGE_EDIT_RESPONDED;
    /// A DM was edited (decrypted).
    DmMessageEdited(DmMessageEditedPayload) => DM_MESSAGE_EDITED;

    // ── Encryption ──
    /// Our E2EE status: after sign-in bootstrap, setup, unlock, reset, a rotation of our keys elsewhere, and logout.
    E2eeStatusChanged(E2eeStatusPayload) => E2EE_STATUS_CHANGED;
    /// A pinned peer's identity changed (they reset their keys).
    E2eePeerChanged(E2eePeerChangedPayload) => E2EE_PEER_CHANGED;
    /// The MLS group behind our voice channel changed state ("joining" | "ready" | "resyncing" | "quarantine" | "failed").
    VoiceE2eeState(VoiceE2eeStatePayload) => VOICE_E2EE_STATE;
    /// The community answered our channel message edit.
    ChannelMessageEditResponded(ChannelMessageEditRespondedPayload) => CHANNEL_MESSAGE_EDIT_RESPONDED;
    /// A channel message was edited (decrypted).
    ChannelMessageEdited(ChannelMessageEditedPayload) => CHANNEL_MESSAGE_EDITED;

    // ── Server pictures ──
    /// The community answered our server picture upload.
    ServerPictureUpdateResponded(ServerPictureUpdateRespondedPayload) => SERVER_PICTURE_UPDATE_RESPONDED;
    /// Central sent a community's picture (as a `data:` URL; empty = none).
    ServerPictureReceived(ServerPictureReceivedPayload) => SERVER_PICTURE_RECEIVED;
    /// Central announced that a community's picture changed.
    ServerPictureChanged(ServerPictureChangedPayload) => SERVER_PICTURE_CHANGED;

    // ── P2P DM calls ──
    /// A call signal relayed by central, or generated by it (PEER_OFFLINE / NOT_ALLOWED); `from` is the peer.
    CallSignal(CallSignalPayload) => CALL_SIGNAL;
    /// Hole punch succeeded on both sockets and the call's voice engine is up.
    CallConnected(CallConnectedPayload) => CALL_CONNECTED;
    /// STUN / bind / punch failed: no media session was started.
    CallFailed(CallFailedPayload) => CALL_FAILED;
    /// An active call lost its peer (no authenticated datagram for 15 s).
    CallDropped(CallDroppedPayload) => CALL_DROPPED;
}
