# Server management + moderation (batch 17)

Status: implemented 2026-08-22 (community server + Electron client).
Builds on permissions v2 (`2026-08-22-permissions-v2-design.md`) and the
roster-delta protocol (batch 16). Old-client compatibility is not kept.

## Features

| # | Feature | Packets | Permission |
|---|---------|---------|------------|
| 1 | **Server rename / description** in-app. DB is the source of truth; `DECIBELL_SERVER_NAME/DESC` only seed a fresh DB (or fill an empty name). Heartbeat re-reads the DB every tick. | `SERVER_UPDATE_REQ/RES` (110/111), `SERVER_META_UPDATE` (44, now live) to everyone | MANAGE_SERVER |
| 2 | **Audit log**: `audit_log(id, ts, actor, action, target, channel_id, details)`, written by every mod/management handler (kick, ban, unban, timeout, role CRUD, member roles, channel CRUD/update/wipe, others' message delete, overwrite set, nickname of others, server update, ownership transfer, voice mod, invite create/revoke). Pruned after 180 days. | `AUDIT_LOG_REQ/RES` (112/113), paged by id, ≤100/page | VIEW_AUDIT_LOG (1<<16) |
| 3 | **Timeouts**: `members.timed_out_until`; enforced in the `Authorizer` for SendMessage / AttachFiles / ConnectVoice / Stream (reason says until when). Clamped to 28 days; `until = 0` clears. `MemberInfo.timed_out_until` rides the roster deltas. | `TIMEOUT_MEMBER_REQ` (114) → `MOD_ACTION_RES{action="timeout"}` | MODERATE_MEMBERS (1<<17) + hierarchy; owner can't be timed out |
| 4 | **Ban improvements**: `expires_at` (expired bans count as lifted and are lazily deleted — auth, invite redemption and the ban list all agree), `delete_message_seconds` (≤ 7 days; every purged message is broadcast as `CHANNEL_MESSAGE_DELETED` to its channel's viewers), reason + expiry delivered to the target in `MEMBERSHIP_REVOKED`, `BAN_LIST_RES.entries` = `BanInfo{username, banned_by, reason, banned_at, expires_at}`. | `BanMemberRequest.expires_at / delete_message_seconds` | BAN_MEMBERS |
| 5 | **Slowmode**: `channels.slowmode_seconds` (≤ 6 h), per-session last-message time per channel; MANAGE_MESSAGES in the channel bypasses. | `ChannelInfo.slowmode_seconds`, `ChannelUpdateRequest.slowmode_seconds` | MANAGE_CHANNELS |
| 6 | **Voice moderation**: server mute / deafen (persisted on `members.server_muted/server_deafened`; the relay drops a server-muted user's AUDIO and skips server-deafened targets, so clients can't bypass it), move (target needs CONNECT in the destination), disconnect. Target gets `VOICE_FORCE_NOTIFY{MOVED|DISCONNECTED}`; `VoiceUserState.is_server_muted/deafened` in presence. | `VOICE_MOD_REQ` (115) → `MOD_ACTION_RES{action="voice_mod"}`, `VOICE_FORCE_NOTIFY` (116) | VOICE_MODERATE (1<<18) + hierarchy |
| 7 | **Ownership transfer**: owner only, target must be a member; updates `server_meta.owner` (all permission caches invalidate), broadcasts `SERVER_META_UPDATE`, `MEMBER_UPSERT` for both, re-pushes channel lists + ban list. Owner stays a `server_meta` key rather than a synthetic role — every owner check already goes through `Authorizer` / `db.owner()`, so there is one place to change if that ever needs to become a role. | `TRANSFER_OWNERSHIP_REQ` (117) → `MOD_ACTION_RES{action="transfer"}` | owner |

Schema v7 adds the columns above + `audit_log`. None of the new bits are
default-on.

## Client

- Overview tab: editable name/description (MANAGE_SERVER), ownership
  transfer card (owner, typed confirmation).
- Audit Log tab (VIEW_AUDIT_LOG), newest first, "Load older".
- Members tab: kick/ban/timeout dialogs with reason; ban duration + "delete
  messages from last …"; timeout duration; "End timeout"; Timed out chip.
- Bans tab shows reason / moderator / expiry; `MembershipRevokedToast`
  shows the expiry to the banned user.
- Channel settings: slowmode select (text channels). Composer shows a
  timeout countdown bar when timed out and a slowmode hint in the
  placeholder.
- Voice: user context menu gains Server mute/unmute, deafen/undeafen,
  Move to…, Disconnect (VOICE_MODERATE + hierarchy, target in voice);
  participant rows show a "Srv mute / Srv deaf" badge;
  `voice_force_notify` moves the local session or tears it down.
