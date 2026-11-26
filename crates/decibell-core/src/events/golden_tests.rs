//! The wire shape of every named event, pinned: each emit helper is driven
//! with a representative payload, and the `{name, payload}` pairs
//! [`CoreEvent::to_json`] makes of what it emitted are compared, byte for byte
//! as pretty JSON, with `crates/decibell-core/tests/golden/events.json`. The
//! renderer's payload types (`electron-client/src/types/index.ts` and the
//! listeners) are hand-written against that JSON, so any drift fails here
//! first. The file was first written from the untyped `send(name, payload)`
//! path, before events were typed, and the typed path reproduces it.
//!
//! The samples pin the representations that matter to a JS reader: field
//! names (camelCase, `self`, `type`), `null` for `None`, integers above
//! 2^53 (they reach the renderer as BigInt), negative uids (community
//! placeholders), `u64` permission bits, `f32` widened to `f64`, and the
//! payload-less events' `null`. Optional payload fields also get a `None`
//! sample.
//!
//! A deliberate wire change: rerun with `DECIBELL_BLESS_GOLDEN=1` to rewrite
//! the file, and commit it with the renderer change it needs.

use super::*;
use crate::hotkeys::{Action, Status};

const GOLDEN: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/events.json");

/// A message id above 2^53: the renderer gets these as BigInt.
const BIG_ID: i64 = 9_007_199_254_740_993;
/// Permission bits with one far above 2^32.
const PERMS: u64 = (1 << 40) | 0b1011;

fn server_info(id: i32, name: &str) -> ServerInfo {
    ServerInfo {
        id,
        name: name.into(),
        description: "Games, mostly".into(),
        host_ip: "203.0.113.7".into(),
        port: 50052,
        member_count: 42,
        picture_version: "9f86d081884c7d65".into(),
    }
}

fn channel_info(id: &str, kind: &str) -> ChannelInfoPayload {
    ChannelInfoPayload {
        id: id.into(),
        name: "general".into(),
        r#type: kind.into(),
        voice_bitrate_kbps: 64,
        retention_days_text: 0,
        retention_days_image: 30,
        retention_days_video: 14,
        retention_days_document: 90,
        retention_days_audio: 7,
        my_permissions: PERMS,
        slowmode_seconds: 5,
        encrypted: true,
        permissions_synced: false,
        category_sync: true,
        topic: "Say hi".into(),
    }
}

fn attachment() -> AttachmentPayload {
    AttachmentPayload {
        id: 501,
        message_id: BIG_ID,
        kind: "image".into(),
        filename: "cat.png".into(),
        mime: "image/png".into(),
        size_bytes: 123_456,
        url: "/attachments/501".into(),
        position: 0,
        created_at: 1_764_000_000,
        purged_at: 0,
        width: 1920,
        height: 1080,
        thumbnail_size_bytes: 40_960,
        thumbnail_sizes_mask: 0b111,
        duration_ms: 0,
        placeholder: "3OcRJYB4d3h/iIeHeEh3eIhw+j3A".into(),
        encrypted: true,
    }
}

fn encrypted_attachment() -> EncryptedAttachmentMetaPayload {
    EncryptedAttachmentMetaPayload {
        id: 501,
        key_b64: "AAECAwQFBgcICQoLDA0ODxAREhMUFRYXGBkaGxwdHh8=".into(),
        filename: "secret.mp4".into(),
        mime: "video/mp4".into(),
        size_bytes: 9_876_543,
        width: 1280,
        height: 720,
        duration_ms: 61_000,
        placeholder: "".into(),
        chunk_bytes: 65_536,
        thumbnail_sizes_mask: 0b011,
    }
}

fn channel_message(id: i64) -> ChannelMessagePayload {
    ChannelMessagePayload {
        id,
        sender: "alice".into(),
        sender_uid: 1001,
        channel_id: "12".into(),
        content: "hello \"world\" — ünïcode ✓".into(),
        timestamp: 1_764_000_123,
        attachments: vec![attachment()],
        nonce: "n-1".into(),
        edited_at: 0,
        reply_to: 77,
        reply_to_sender: "bob".into(),
        reply_to_sender_uid: -3,
        reply_to_content: "earlier".into(),
        reply_to_attachment_kinds: vec![1, 3],
        encrypted: true,
        decrypt_error: "".into(),
        encrypted_attachments: vec![encrypted_attachment()],
        suppress_embeds: true,
    }
}

fn member(uid: i64, name: &str) -> MemberInfoPayload {
    MemberInfoPayload {
        username: name.into(),
        uid,
        joined_at: 1_700_000_000,
        nickname: "Al".into(),
        is_owner: false,
        is_online: true,
        role_ids: vec![2, 5],
        timed_out_until: 0,
    }
}

fn role(id: i64) -> RoleInfoPayload {
    RoleInfoPayload {
        id,
        name: "Moderator".into(),
        color: 0x00ff_8800,
        position: 3,
        permissions: PERMS,
        is_default: false,
        manage_each_other: true,
        member_count: 4,
    }
}

fn invite() -> InviteInfoPayload {
    InviteInfoPayload {
        code: "AbC123".into(),
        created_by: "alice".into(),
        created_by_uid: 1001,
        created_at: 1_764_000_000,
        expires_at: 0,
        max_uses: 0,
        uses: 2,
        for_user: "bob".into(),
        for_uid: 1002,
    }
}

fn device(id: i64, current: bool) -> DevicePayload {
    DevicePayload {
        id,
        name: "Linux".into(),
        platform: "linux".into(),
        app_version: "0.9.4".into(),
        address: "198.51.100.4".into(),
        signed_in_at: 1_763_000_000,
        last_active_at: 1_764_000_000,
        online: true,
        current,
    }
}

fn caps() -> ClientCapabilitiesPayload {
    ClientCapabilitiesPayload {
        encode: vec![CodecCapabilityPayload { codec: 3, max_width: 3840, max_height: 2160, max_fps: 60 }],
        decode: vec![
            CodecCapabilityPayload { codec: 1, max_width: 1920, max_height: 1080, max_fps: 60 },
            CodecCapabilityPayload { codec: 4, max_width: 7680, max_height: 4320, max_fps: 30 },
        ],
    }
}

fn hotkeys_status() -> Status {
    let mut triggers = std::collections::HashMap::new();
    triggers.insert("ptt".to_string(), "Mouse4".to_string());
    triggers.insert("mute".to_string(), "<Control>m".to_string());
    Status {
        backend: "portal",
        state: "active",
        detail: None,
        mouse: true,
        focused_fallback: false,
        can_configure: true,
        triggers,
        failed: vec!["deafen".into()],
        registered: vec![],
    }
}

fn call_signal(full: bool) -> CallSignalPayload {
    CallSignalPayload {
        kind: if full { "INVITE" } else { "HANGUP" }.into(),
        call_id: "c-9".into(),
        from: "bob".into(),
        from_uid: 1002,
        to: "alice".into(),
        pub_key: full.then(|| "cHVia2V5".to_string()),
        candidates: if full {
            vec![CallCandidatePayload { socket: "VOICE".into(), kind: "SRFLX".into(), ip: "198.51.100.9".into(), port: 40_123 }]
        } else {
            vec![]
        },
        stream: full.then_some(CallStreamMetaPayload { codec: 3, width: 1920, height: 1080, fps: 60, has_audio: true }),
        timestamp: 1_764_000_456,
        pub_key_sig: full.then(|| "c2ln".to_string()),
        key_id: if full { 7 } else { 0 },
    }
}

/// Every named event, in `CoreEvent` order (extra samples follow the first
/// for payloads with optional parts).
fn emit_samples() {
    emit_connection_lost("community", Some("7".into()));
    emit_connection_restored("central", None);
    emit_login_succeeded("alice".into(), 1001, true, false);
    emit_user_renamed(UserRenamedPayload { uid: 1002, username: "bobby".into(), is_self: false });
    emit_avatar_changed(1002, "bobby".into(), "c0ffee".into());
    emit_login_failed("Invalid username or password".into());
    emit_register_responded(false, "Username taken".into());
    emit_logged_out(Some("signed_out"));
    emit_logged_out(None);
    emit_devices_changed(Some(device(31, false)));
    emit_devices_changed(None);
    emit_session_token_updated();
    emit_server_list_received(vec![server_info(7, "Daily Gaming"), server_info(8, "Rustaceans")]);
    emit_memberships_received(vec![server_info(7, "Daily Gaming")]);
    emit_membership_added(server_info(9, "Design Lab"));
    emit_community_auth_responded(CommunityAuthRespondedPayload {
        server_id: "7".into(),
        requested_server_id: "203.0.113.7:50052".into(),
        host: "203.0.113.7".into(),
        port: 50052,
        success: true,
        message: "".into(),
        channels: vec![channel_info("12", "text"), channel_info("13", "voice")],
        error_code: "".into(),
        server_name: "Daily Gaming".into(),
        server_description: "Games, mostly".into(),
        owner_username: "alice".into(),
        owner_uid: 1001,
        attachment_port: 50053,
        max_attachment_bytes: 5_368_709_120,
        public_listing: true,
    });
    emit_message_received(MessageReceivedPayload {
        context: "12".into(),
        server_id: "7".into(),
        sender: "alice".into(),
        sender_uid: 1001,
        recipient: "".into(),
        recipient_uid: 0,
        content: "hi".into(),
        timestamp: "1764000123".into(),
        id: BIG_ID,
        attachments: vec![attachment()],
        nonce: "n-2".into(),
        edited_at: 1_764_000_200,
        reply_to: 0,
        reply_to_sender: "".into(),
        reply_to_sender_uid: 0,
        reply_to_content: "".into(),
        reply_to_attachment_kinds: vec![],
        encrypted: false,
        decrypt_error: "locked".into(),
        encrypted_attachments: vec![],
        suppress_embeds: false,
    });
    emit_channel_history_received(ChannelHistoryReceivedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        messages: vec![channel_message(BIG_ID)],
        has_more: true,
        has_more_after: false,
        around_id: 0,
        after_id: 0,
        before_id: 600,
    });
    emit_channel_pruned(ChannelPrunedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        deleted_message_ids: vec![1, BIG_ID],
        purged_attachments: vec![AttachmentTombstonePayload { attachment_id: 501, purged_at: 1_764_000_999 }],
    });
    emit_channel_updated(ChannelUpdatedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        channel: Some(channel_info("12", "text")),
    });
    emit_channel_updated(ChannelUpdatedPayload {
        server_id: "7".into(),
        success: false,
        message: "Missing permission".into(),
        channel: None,
    });
    emit_channel_wipe_responded(ChannelWipeRespondedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        success: true,
        message: "".into(),
        deleted_message_count: 10_000,
        deleted_attachment_count: 12,
    });
    emit_channel_wiped(ChannelWipedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        wiped_at: 1_764_001_000,
        wiped_by: "alice".into(),
        wiped_by_uid: 1001,
    });
    emit_user_list_updated(
        vec![UserPresencePayload {
            username: "bob".into(),
            uid: 1002,
            avatar_version: "".into(),
            status: "dnd".into(),
            status_text: "busy".into(),
            status_emoji: "🎮".into(),
        }],
        vec![1002, 1003],
        vec![AvatarVersionPayload { uid: 1003, version: "abc".into() }],
    );
    emit_presence_delta(PresenceDeltaPayload {
        username: "bob".into(),
        uid: 1002,
        online: true,
        status: "away".into(),
        avatar_version: "abc".into(),
        status_text: "".into(),
        status_emoji: "".into(),
    });
    emit_own_presence(OwnPresencePayload {
        supported: true,
        status: "invisible".into(),
        status_text: "brb".into(),
        status_emoji: "☕".into(),
        status_clear_in: 3599.5,
        dm_friends_only: true,
        account_idle: false,
    });
    emit_friend_list_received(vec![FriendInfoPayload {
        username: "bob".into(),
        uid: 1002,
        status: "pending_incoming".into(),
        avatar_version: "".into(),
    }]);
    emit_friend_action_responded(true, "Friend request sent".into());
    emit_member_list_received(MemberListReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        members: vec![member(1001, "alice")],
        revision: u64::MAX,
        total_members: 3,
        has_more: false,
        next_after: "".into(),
        first_page: true,
        online_count: 1,
        online_complete: true,
        extra: vec![member(-3, "legacy")],
        lookup_uids: vec![],
    });
    emit_mod_action_responded(ModActionRespondedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        username: "mallory".into(),
        uid: 1666,
        action: "ban".into(),
    });
    emit_channel_message_rejected(ChannelMessageRejectedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        nonce: "n-3".into(),
        reason: "slowmode".into(),
        retry_after_seconds: 4,
    });
    emit_membership_revoked(MembershipRevokedPayload {
        server_id: "7".into(),
        action: "ban".into(),
        reason: "spam".into(),
        actor: "alice".into(),
        actor_uid: 1001,
        expires_at: 0,
    });
    emit_role_list_received(RoleListReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        roles: vec![role(5), role(1)],
    });
    emit_member_upsert(MemberUpsertPayload {
        server_id: "7".into(),
        member: member(1002, "bob"),
        revision: 18,
        online_count: 2,
        total_members: 3,
    });
    emit_member_remove(MemberRemovePayload {
        server_id: "7".into(),
        username: "bob".into(),
        uid: 1002,
        revision: 19,
        online_count: 1,
        total_members: 2,
    });
    emit_ban_list_received(BanListReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        entries: vec![BanInfoPayload {
            username: "mallory".into(),
            uid: 1666,
            banned_by: "alice".into(),
            banned_by_uid: 1001,
            reason: "spam".into(),
            banned_at: 1_764_000_000,
            expires_at: 1_764_600_000,
        }],
        revision: 3,
        truncated: false,
    });
    emit_server_meta_updated(ServerMetaUpdatedPayload {
        server_id: "7".into(),
        server_name: "Daily Gaming".into(),
        server_description: "Games, mostly".into(),
        owner_username: "alice".into(),
        owner_uid: 1001,
        public_listing: false,
    });
    emit_server_update_responded(ServerUpdateRespondedPayload {
        server_id: "7".into(),
        success: false,
        message: "Name too long".into(),
    });
    emit_audit_log_received(AuditLogReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        entries: vec![AuditEntryPayload {
            id: 88,
            timestamp: 1_764_000_000,
            actor: "alice".into(),
            actor_uid: 1001,
            action: "role_update".into(),
            target: "bob".into(),
            target_uid: 1002,
            channel_id: "".into(),
            details: "{\"color\":1}".into(),
        }],
        has_more: true,
        seq: 4,
    });
    emit_member_search_received(MemberSearchReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        members: vec![member(1002, "bob")],
        total_matches: 1,
        has_more: false,
        next_after: "bob".into(),
        seq: 9,
        first_page: true,
    });
    emit_message_search_received(MessageSearchReceivedPayload {
        server_id: "7".into(),
        seq: 2,
        success: true,
        message: "".into(),
        messages: vec![channel_message(42)],
        has_more: true,
        next_cursor: BIG_ID,
        next_offset: 25,
        total: -1,
        total_capped: false,
        text_skipped_channel_ids: vec!["14".into()],
        partial: true,
        retry_after_ms: 0,
        first_page: true,
    });
    emit_storage_info_received(StorageInfoReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        volume_total_bytes: 2_000_398_934_016,
        volume_available_bytes: 1_000_000_000_000,
        attachments_bytes: 5_000_000,
        thumbnails_bytes: 400_000,
        database_bytes: 1_048_576,
        attachment_count: 12,
        min_free_bytes: 1_073_741_824,
        by_kind: vec![StorageKindUsagePayload { kind: 1, bytes: 4_000_000, count: 10 }],
        by_channel: vec![StorageChannelUsagePayload { channel_id: "12".into(), bytes: 5_000_000, count: 12 }],
        largest: vec![StorageLargestItemPayload {
            attachment_id: 501,
            filename: "cat.png".into(),
            size_bytes: 123_456,
            channel_id: "12".into(),
            kind: 1,
        }],
    });
    emit_voice_force_notify(VoiceForceNotifyPayload {
        server_id: "7".into(),
        action: "moved".into(),
        channel_id: "13".into(),
        actor: "alice".into(),
        actor_uid: 1001,
    });
    emit_channel_overwrites_received(ChannelOverwritesReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        channel_id: "12".into(),
        overwrites: vec![ChannelOverwritePayload {
            channel_id: "12".into(),
            target_type: "member".into(),
            target_id: "bob".into(),
            target_uid: 1002,
            allow: PERMS,
            deny: 1 << 63,
        }],
    });
    emit_role_action_responded(RoleActionRespondedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        action: "update".into(),
        role: Some(role(5)),
    });
    emit_role_action_responded(RoleActionRespondedPayload {
        server_id: "7".into(),
        success: false,
        message: "Role is above yours".into(),
        action: "delete".into(),
        role: None,
    });
    emit_channel_list_updated(ChannelListUpdatedPayload {
        server_id: "7".into(),
        channels: vec![channel_info("11", "category"), channel_info("12", "text")],
    });
    emit_channel_action_responded(ChannelActionRespondedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        action: "create".into(),
        channel: Some(channel_info("15", "text")),
    });
    emit_channel_action_responded(ChannelActionRespondedPayload {
        server_id: "7".into(),
        success: false,
        message: "Too many channels".into(),
        action: "create".into(),
        channel: None,
    });
    emit_invite_list_received(InviteListReceivedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        invites: vec![invite()],
        truncated: true,
    });
    emit_invite_create_responded(InviteCreateRespondedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        invite: Some(invite()),
        request_id: 3,
        already_member: false,
    });
    emit_invite_create_responded(InviteCreateRespondedPayload {
        server_id: "7".into(),
        success: false,
        message: "Already a member".into(),
        invite: None,
        request_id: 4,
        already_member: true,
    });
    emit_invite_revoke_responded(InviteRevokeRespondedPayload {
        server_id: "7".into(),
        success: true,
        message: "".into(),
        code: "AbC123".into(),
    });
    emit_voice_presence_updated(VoicePresenceUpdatedPayload {
        server_id: "7".into(),
        channel_id: "13".into(),
        participants: vec!["alice".into(), "bob".into()],
        participant_uids: vec![1001, 1002],
        user_states: vec![VoiceUserStatePayload {
            username: "alice".into(),
            uid: 1001,
            is_muted: true,
            is_deafened: false,
            is_server_muted: false,
            is_server_deafened: false,
            is_suppressed: true,
        }],
        user_capabilities: vec![caps(), ClientCapabilitiesPayload::default()],
    });
    emit_voice_state_changed(true, false);
    emit_hotkey_action(Action::PushToTalk, true);
    emit_hotkeys_status(&hotkeys_status());
    emit_voice_user_speaking(0, true);
    emit_voice_user_state_changed(1002, false, true);
    emit_voice_input_level(-42.7);
    emit_system_idle_changed(true, 600);
    emit_voice_ping_updated(23);
    emit_voice_connection_stats(Some(23), 0.1);
    emit_voice_connection_stats(None, 0.0);
    emit_voice_error("No input device".into());
    emit_stream_presence_updated(
        "7".into(),
        "13".into(),
        vec![StreamInfoPayload {
            stream_id: "1001_screen".into(),
            owner_username: "alice".into(),
            owner_uid: 1001,
            has_audio: true,
            resolution_width: 2560,
            resolution_height: 1440,
            fps: 60,
            current_codec: 3,
            enforced_codec: 0,
            watcher_count: 2,
        }],
    );
    emit_stream_codec_changed(StreamCodecChangedPayload {
        channel_id: "13".into(),
        streamer_username: "alice".into(),
        streamer_uid: 1001,
        new_codec: 1,
        new_width: 1920,
        new_height: 1080,
        new_fps: 30,
        reason: 2,
    });
    emit_stream_gpu_fallback("NVENC unavailable".into());
    emit_stream_capture_ended();
    emit_keyframe_requested();
    emit_native_stream_failed("encoder panicked: boom".into());
    emit_caps_refreshed();
    emit_dm_conversations_received(DmConversationsReceivedPayload {
        conversations: vec![DmConversationPreviewPayload {
            peer: "bob".into(),
            peer_uid: 1002,
            last_message_content: "see you".into(),
            last_message_sender: "bob".into(),
            last_message_sender_uid: 1002,
            last_message_id: BIG_ID,
            last_timestamp: 1_764_000_300,
            unread_count: 3,
            encrypted: true,
            decrypt_error: "no_key".into(),
        }],
    });
    emit_dm_history_received(DmHistoryReceivedPayload {
        peer: "bob".into(),
        peer_uid: 1002,
        messages: vec![DmHistoryMessagePayload {
            id: 900,
            sender: "bob".into(),
            sender_uid: 1002,
            content: "yo".into(),
            timestamp: 1_764_000_301,
            edited_at: 0,
            reply_to: 899,
            reply_to_sender: "alice".into(),
            reply_to_sender_uid: 1001,
            reply_to_content: "hey".into(),
            encrypted: true,
            decrypt_error: "".into(),
        }],
        has_more: false,
        has_more_after: true,
        around_id: 900,
        after_id: 0,
        before_id: 0,
    });
    emit_dm_read_synced(1002, BIG_ID);
    emit_dm_message_delete_responded(DmMessageDeleteRespondedPayload {
        success: true,
        message: "".into(),
        peer: "bob".into(),
        peer_uid: 1002,
        message_id: 900,
    });
    emit_dm_message_deleted(DmMessageDeletedPayload {
        peer: "bob".into(),
        peer_uid: 1002,
        message_id: 900,
        deleted_at: 1_764_000_400,
    });
    emit_channel_message_delete_responded(ChannelMessageDeleteRespondedPayload {
        success: false,
        message: "Not yours".into(),
        server_id: "7".into(),
        channel_id: "12".into(),
        message_id: BIG_ID,
    });
    emit_channel_message_deleted(ChannelMessageDeletedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        message_id: BIG_ID,
        deleted_at: 1_764_000_401,
        deleted_by: "alice".into(),
        deleted_by_uid: 1001,
    });
    emit_dm_message_edit_responded(DmMessageEditRespondedPayload {
        success: true,
        message: "".into(),
        peer: "bob".into(),
        peer_uid: 1002,
        message_id: 900,
    });
    emit_dm_message_edited(DmMessageEditedPayload {
        peer: "bob".into(),
        peer_uid: 1002,
        message_id: 900,
        content: "yo!".into(),
        edited_at: 1_764_000_402,
        encrypted: true,
        decrypt_error: "".into(),
    });
    emit_e2ee_status_changed(E2eeStatusPayload {
        supported: true,
        status: "ready".into(),
        key_id: 3,
        fingerprint: "05 12 99 41".into(),
    });
    emit_e2ee_peer_changed(E2eePeerChangedPayload {
        username: "bob".into(),
        uid: 1002,
        key_id: 5,
        fingerprint: "77 12 03 88".into(),
    });
    emit_voice_e2ee_state(VoiceE2eeStatePayload {
        server_id: "7".into(),
        channel_id: "13".into(),
        state: "ready".into(),
        epoch: u64::MAX - 1,
        members: 2,
        unverified: vec!["1002".into()],
    });
    emit_channel_message_edit_responded(ChannelMessageEditRespondedPayload {
        success: true,
        message: "".into(),
        server_id: "7".into(),
        channel_id: "12".into(),
        message_id: BIG_ID,
    });
    emit_channel_message_edited(ChannelMessageEditedPayload {
        server_id: "7".into(),
        channel_id: "12".into(),
        message_id: BIG_ID,
        content: "edited".into(),
        edited_at: 1_764_000_403,
        editor: "alice".into(),
        editor_uid: 1001,
        encrypted: true,
        decrypt_error: "".into(),
        encrypted_attachments: vec![encrypted_attachment()],
        suppress_embeds: false,
    });
    emit_server_picture_update_responded(ServerPictureUpdateRespondedPayload {
        success: true,
        message: "".into(),
        server_id: "7".into(),
        version: "d1e8".into(),
    });
    emit_server_picture_received(ServerPictureReceivedPayload {
        server_id: 7,
        version: "d1e8".into(),
        data: bytes_to_data_url(&[0x89, 0x50, 0x4E, 0x47, 0x0D, 0x0A]),
    });
    emit_server_picture_changed(ServerPictureChangedPayload { server_id: 7, version: "".into() });
    emit_call_signal(call_signal(true));
    emit_call_signal(call_signal(false));
    emit_call_connected(CallConnectedPayload {
        call_id: "c-9".into(),
        rtt_ms: 12.345,
        path: "srflx".into(),
        verified: true,
    });
    emit_call_failed(CallFailedPayload {
        call_id: "c-9".into(),
        reason: "no_path".into(),
        detail: "punch timed out".into(),
    });
    emit_call_dropped(CallDroppedPayload { call_id: "c-9".into(), reason: "timeout".into() });
}

/// What this thread emitted, as the `{name, payload}` envelopes the napi sink
/// builds, pretty-printed.
fn render(events: Vec<(&'static str, serde_json::Value)>) -> String {
    let list: Vec<serde_json::Value> = events
        .into_iter()
        .map(|(name, payload)| {
            let mut env = serde_json::Map::new();
            env.insert("name".into(), serde_json::Value::String(name.into()));
            env.insert("payload".into(), payload);
            serde_json::Value::Object(env)
        })
        .collect();
    serde_json::to_string_pretty(&list).expect("events serialise") + "\n"
}

fn recorded_samples() -> Vec<(&'static str, serde_json::Value)> {
    testing::install_recording_sink();
    let _ = testing::take_own_events();
    emit_samples();
    testing::take_own_events().iter().map(CoreEvent::to_json).collect()
}

#[test]
fn every_event_matches_the_golden_wire_shape() {
    let got = render(recorded_samples());
    if std::env::var_os("DECIBELL_BLESS_GOLDEN").is_some() {
        std::fs::create_dir_all(std::path::Path::new(GOLDEN).parent().unwrap()).unwrap();
        std::fs::write(GOLDEN, &got).unwrap();
        return;
    }
    let want = std::fs::read_to_string(GOLDEN)
        .unwrap_or_else(|e| panic!("{GOLDEN}: {e} (DECIBELL_BLESS_GOLDEN=1 writes it)"));
    if got != want {
        let line = got.lines().zip(want.lines()).position(|(g, w)| g != w);
        let at = line.unwrap_or_else(|| got.lines().count().min(want.lines().count()));
        panic!(
            "event JSON drifted from {GOLDEN} at line {}:\n  golden: {:?}\n  now:    {:?}\n\
             (a deliberate wire change: DECIBELL_BLESS_GOLDEN=1, then update the renderer)",
            at + 1,
            want.lines().nth(at).unwrap_or("<end>"),
            got.lines().nth(at).unwrap_or("<end>"),
        );
    }
}

/// Every `CoreEvent` variant has a sample above (so the golden file pins its
/// JSON), every name is declared once, and `to_json` names each sample
/// as `name()` does.
#[test]
fn every_variant_has_a_golden_sample() {
    use std::collections::BTreeSet;
    let names: BTreeSet<&str> = CoreEvent::NAMES.iter().copied().collect();
    assert_eq!(names.len(), CoreEvent::NAMES.len(), "an event name is declared twice");
    testing::install_recording_sink();
    let _ = testing::take_own_events();
    emit_samples();
    let mut sampled = BTreeSet::new();
    for event in testing::take_own_events() {
        assert_eq!(event.to_json().0, event.name());
        sampled.insert(event.name());
    }
    let missing: Vec<_> = names.difference(&sampled).collect();
    assert!(missing.is_empty(), "no golden sample for {missing:?}: add one to emit_samples");
    assert_eq!(sampled, names);
}

const RENDERER_LISTENERS: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/tests/golden/renderer-listeners.txt");

/// Events the renderer listens for that the core never emits: Electron main
/// sends them on the same `decibell:event` channel.
const RENDERER_ONLY: &[&str] = &[
    "deep_link_received", // electron/main/index.ts (decibell:// links)
    "downloads_changed",  // electron/main/downloads.ts
    "downloads_removed",  // electron/main/downloads.ts
    "update_status",      // electron/main/update.ts
];

/// Core events no renderer listener reads.
const NOT_LISTENED: &[&str] = &[
    // No emitter either: emit_caps_refreshed has no caller.
    "caps_refreshed",
    // Electron main reads it (electron/main/addon.ts refreshes the
    // attachment targets); it reaches the renderer too, unheard.
    "session_token_updated",
    // Only after `system_idle::subscribe`, which the native client calls;
    // Electron polls `system_idle_state` and never subscribes.
    "system_idle_changed",
];

/// The renderer's listeners (the committed list) against the core's event
/// names: a listener for a name the core doesn't emit, or an event nobody
/// listens for, must be one of the known ones above.
#[test]
fn renderer_listeners_match_core_events() {
    use std::collections::BTreeSet;
    let text = std::fs::read_to_string(RENDERER_LISTENERS)
        .unwrap_or_else(|e| panic!("{RENDERER_LISTENERS}: {e}"));
    let listened: BTreeSet<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty() && !l.starts_with('#'))
        .collect();
    let names: BTreeSet<&str> = CoreEvent::NAMES.iter().copied().collect();
    let renderer_only: BTreeSet<&str> = listened.difference(&names).copied().collect();
    let not_listened: BTreeSet<&str> = names.difference(&listened).copied().collect();
    assert_eq!(
        renderer_only,
        RENDERER_ONLY.iter().copied().collect(),
        "renderer listeners with no core event"
    );
    assert_eq!(
        not_listened,
        NOT_LISTENED.iter().copied().collect(),
        "core events with no renderer listener"
    );
}
