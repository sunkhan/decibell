# Permissions v2 — resolver, enforced bits, per-channel overwrites

Status: implemented 2026-08-22 (community server + Electron client).
Builds on `2026-08-17-roles-permissions-design.md` (roles v1).

## Goals

1. One place that answers "may `user` do `action` (in `channel`, to
   `target`)?" instead of 15 inline `has_permission` blocks — so adding a
   channel dimension touches one resolver, not every handler.
2. Enforce the bits roles v1 reserved but never checked:
   `SEND_MESSAGES`, `CONNECT_VOICE`, `STREAM`.
3. Per-channel permission overwrites (Discord's model) → private channels,
   read-only `#announcements`, mod-only channels.

## Model

### New bits (append-only; `Permission` enum, `perms::`, `PERM`)

| bit | name | gates |
|-----|------|-------|
| 1<<13 | VIEW_CHANNEL | channel appears in the list; history, messages, presence for it are delivered |
| 1<<14 | READ_HISTORY | `CHANNEL_HISTORY_REQ` |
| 1<<15 | ATTACH_FILES | attachments on `CHANNEL_MSG`, `POST /attachments/init` |

All three (plus SEND/CONNECT/STREAM) are default-on in the seeded
`everyone` role; migration v6 ORs them into an existing `everyone` once
(`perm_bits_v6` meta stamp), so upgraded servers keep behaving as before
until an operator changes something.

### Overwrites

Table `channel_overwrites(channel_id, target_type, target_id, allow, deny)`,
`target_type` 0 = role (`target_id` = role id as text), 1 = member
(`target_id` = username). A row with `allow = deny = 0` is deleted.
Cascades: channel delete, role delete, member remove/ban (manual, same
transaction). The `everyone` role is a valid role target.

### Resolution (`authz.cpp: channel_permissions(user, channel)`)

```
base = effective_permissions(user)            # roles v1: everyone | roles; owner/ADMIN → all
if owner or ADMINISTRATOR: return kAll        # bypass overwrites entirely (Discord)
p = base
p = (p & ~everyone_ow.deny) | everyone_ow.allow
role_allow = OR(allow of overwrites for user's roles); role_deny = OR(deny …)
p = (p & ~role_deny) | role_allow
p = (p & ~member_ow.deny) | member_ow.allow
if !(p & VIEW_CHANNEL): p = 0                 # can't see it → can do nothing in it
return p
```

Server-wide actions (roles, kick/ban, invites, server picture, nicknames)
use `base`; channel-scoped actions use `channel_permissions`. Because the
channel result starts from `base`, a server-wide MANAGE_CHANNELS holder
keeps it everywhere, and an overwrite can also grant MANAGE_CHANNELS /
MANAGE_MESSAGES for one channel only.

### Hierarchy (decided, documented, one function)

`member_level(user)` = highest *assigned* role position; 0 with none;
owner = ∞. `can_moderate(actor, target)` ⇔ `level(actor) > level(target)`.
Consequences, all Discord-accurate and now explicit:
- two members who only hold `everyone` cannot kick/ban/nick each other,
  even if `everyone` carries KICK_MEMBERS — grant a real role;
- ADMINISTRATOR expands permission bits but never bypasses hierarchy;
- role management / assignment uses the same level comparison against the
  role's position (`role.position < level(actor)`).

### Who may edit overwrites

`MANAGE_ROLES` in that channel (server-wide or via overwrite), plus:
- **escalation guard**: you may only set/clear bits you hold *in that
  channel* (`channel_permissions(actor, channel)`); foreign bits already
  in a row survive an edit untouched;
- role targets must sit strictly below your level (the `everyone` role,
  position 0, is editable by anyone with MANAGE_ROLES);
- you can't lock yourself out: an overwrite that would remove your own
  VIEW_CHANNEL is rejected (owner/admin bypass anyway).

## Wire protocol

New packet types (append-only):

| id | type | direction |
|----|------|-----------|
| 103 | `CHANNEL_OVERWRITE_SET_REQ` | client→community; `{channel_id, target_type, target_id, allow, deny}`; `allow=deny=0` deletes. Result: `CHANNEL_ACTION_RES{action="overwrite"}`. |
| 104 | `CHANNEL_OVERWRITES_REQ` | client→community `{channel_id}`; needs MANAGE_ROLES or MANAGE_CHANNELS in that channel |
| 105 | `CHANNEL_OVERWRITES_RES` | community→client `{channel_id, overwrites[]}`; also pushed to every session that can see the list after a change |

`ChannelInfo.my_permissions = 10` (uint64): the recipient's resolved
permissions for that channel. The channel list is now **per recipient** —
`COMMUNITY_AUTH_RES.channels` and `CHANNEL_LIST_UPDATE` only contain
channels the recipient can VIEW (categories are always included so the
sidebar keeps its structure). Clients gate their UI from `my_permissions`
(composer, attach button, voice join, history) — the server is
authoritative and answers denied actions with the existing failure
shapes (`MOD_ACTION_RES{action="message"}` for messages).

Channel-scoped fan-out is VIEW-filtered: `CHANNEL_MSG`,
`CHANNEL_MESSAGE_DELETED`, `CHANNEL_WIPED`, `CHANNEL_PRUNED`,
`CHANNEL_UPDATE_RES`, `VOICE_PRESENCE_UPDATE`, `STREAM_PRESENCE_UPDATE`,
stream thumbnails / codec notifies go only to members who can see the
channel. Roster (`MEMBER_LIST_RES`) and roles stay server-wide.

Any overwrite / role / membership change re-pushes the channel list to
everyone (cheap: channels × online), which is how a member learns a
channel appeared or vanished for them and how `my_permissions` refreshes.

## Server implementation notes

- `authz.{hpp,cpp}`: `Authorizer` over `CommunityDb` — `Action` enum,
  `AuthCtx{user, channel_id, target}`, `check(action, ctx) → {ok, reason}`,
  `channel_permissions`, `can_moderate`, `visible_channels(user)`.
  Results cached per (user, channel) in the DB layer's permission cache,
  invalidated with it plus on any overwrite change.
- `DbAttachment.channel_id` is now loaded so `GET /attachments/<id>` can
  check VIEW on the attachment's channel.
- `broadcast_to_channel(packet, channel_id)` replaces `broadcast_to_members`
  for channel-scoped packets; `broadcast_channels()` builds one filtered
  list per recipient.

## Client implementation notes

- `permissions.ts`: new bits; SEND/CONNECT/STREAM/VIEW/READ/ATTACH are now
  editable in the role editor; `useChannelPermission(serverId, channelId,
  bit)` reads `ChannelInfo.myPermissions` (owner → all).
- `ChannelSettingsModal` gains a **Permissions** tab: pick a role or
  member, tri-state (allow / inherit / deny) per bit, saved via
  `set_channel_overwrite`.
- Composer + attach button + voice join + stream button gate on the
  channel bits; hidden channels simply don't arrive.

## Deferred

Category-level overwrite sync (Discord "sync permissions"), per-channel
slowmode (buckets exist), channel-scoped audit entries, timeouts.
