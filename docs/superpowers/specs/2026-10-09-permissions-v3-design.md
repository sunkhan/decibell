# Permissions v3: invites, nicknames, embeds, speak, split voice moderation, peer management, category sync

Status: implemented 2026-10-09 (community server + Electron client).
Builds on `2026-08-22-permissions-v2-design.md` (resolver + overwrites) and
`2026-08-22-server-management-moderation-design.md` (voice moderation).
Pre-1.0: client and server change together, no old-version fallbacks.

## Goals

The permission list after the 2026-10-09 inventory of every community
feature against Discord. This batch covers the gaps where the feature
already ships but nothing gates it, plus two holes found during the
inventory and the owner's additions:

1. New bits: Create Invite, Change Nickname, Embed Links, Speak.
2. Voice Moderate split into Mute / Deafen / Move Members, channel-scoped.
3. Moves no longer check the target: the mover only needs to see the
   destination; the moved member joins even without View/Connect there.
4. Per-role "members with this role can manage each other" flag.
5. Category permission sync.
6. Fix: stream thumbnails leaked out of hidden channels.

## Bits

Append-only (`Permission` enum, `perms::`, `PERM`). 1<<18 keeps its value
and is renamed.

| bit | name | scope | default (`everyone`) | gates |
|-----|------|-------|----------------------|-------|
| 1<<18 | MUTE_MEMBERS (was VOICE_MODERATE) | channel | off | server mute / unmute |
| 1<<19 | DEAFEN_MEMBERS | channel | off | server deafen / undeafen |
| 1<<20 | MOVE_MEMBERS | channel | off | move, disconnect |
| 1<<21 | CREATE_INVITE | server | **on** | create invites; list / revoke your own |
| 1<<22 | CHANGE_NICKNAME | server | **on** | set / clear your own nickname |
| 1<<23 | EMBED_LINKS | channel | **on** | link previews + GIF cards on your messages |
| 1<<24 | SPEAK | channel | **on** | your voice audio is relayed |

Migration v11 (one-shot, `perm_bits_v11` meta stamp):
- ORs CREATE_INVITE | CHANGE_NICKNAME | EMBED_LINKS | SPEAK into `everyone`;
- every role and every overwrite (allow and deny) carrying 1<<18 also gets
  1<<19 and 1<<20, so whoever could moderate voice still can, entirely.

MANAGE_INVITES narrows to "list and revoke everyone's invites" and also
implies create. Without it, CREATE_INVITE holders see and revoke only
the invites they made.

## Voice moderation

The actor's bit is resolved **in the target's current voice channel**
(overwrites apply, so a "room host" role can run one channel). Mute and
deafen are persisted member flags and also work on a member who isn't
in voice; then the bit is checked server-wide (base permissions).

| action | bit | where |
|--------|-----|-------|
| server mute / unmute | MUTE_MEMBERS | target's channel (base if not in voice) |
| server deafen / undeafen | DEAFEN_MEMBERS | target's channel (base if not in voice) |
| move | MOVE_MEMBERS | target's current channel; plus actor VIEW on the destination |
| disconnect | MOVE_MEMBERS | target's current channel |

All of them also need the light hierarchy check (below). The owner can't
be voice-moderated.

### Moves don't check the target: voice passes

Owner decision: if the mover can see the destination, the target goes
there even when they lack VIEW_CHANNEL / CONNECT on it. The server
grants the target a **voice pass** for that channel: while any of their
sessions is in it, the resolver treats them as holding VIEW_CHANNEL +
CONNECT there (added before the "no VIEW, no permissions" collapse, so
every other bit still follows the normal role/overwrite chain: a
listen-only room stays listen-only). The pass is runtime state in
`CommunityDb` (`voice_passes_`, never persisted) and ends when the
user's last session leaves that channel (leave, disconnect, move away,
kick/ban, session close). Granting or ending a pass re-sends that user's
channel list, so the channel appears before `VOICE_FORCE_NOTIFY{MOVED}`
arrives (same TCP stream, ordered) and disappears once they leave.

## Peer management: "members with this role can manage each other"

`RoleInfo.manage_each_other` (DB `roles.manage_each_other`). Two members
are **peers** when their highest roles are the same role (same level;
positions are unique) and that role has the flag on. Level 0 (`everyone`
only) counts too when `everyone` has the flag. Peers still need the
permission bit for the action.

| action | strictly above | peer |
|--------|----------------|------|
| change another member's nickname | yes | **yes** |
| server mute / deafen, move, disconnect | yes | **yes** |
| timeout, kick, ban | yes | no |
| add / remove roles | yes (role below your level) | no: the shared role is at your level, so it is never assignable by a peer |

`Authorizer::can_moderate` (strict) is unchanged; the light actions use
`can_manage(actor, target) = can_moderate || peers(actor, target)`.
The flag is edited like any other role field (MANAGE_ROLES + role below
your level) and is allowed on `everyone`.

## Speak

The UDP relay drops AUDIO (and its sealed twin) from a session whose
user lacks SPEAK in their voice channel, on the same path as server
mute, so a modified client can't get around it. Stream audio is
untouched (that's STREAM, as with server mute). Per session
`can_speak_` is computed on join / move and refreshed for everyone in
voice whenever permissions may have changed (the `broadcast_channels()`
hook every role / overwrite / membership change already calls). A change
re-broadcasts that channel's voice presence.

`VoiceUserState.is_suppressed = 6`: the client renders it like server
mute ("Can't speak here"), and a suppressed local user's mic shows
blocked.

## Embed Links

Link previews are unfurled by the viewer's client, so the server can't
stop the fetch. It marks the message instead:
`ChannelMessage.suppress_embeds = 15` / `ChannelMessageEdited.suppress_embeds = 7`
= the sender lacked EMBED_LINKS in that channel when sending (or
editing). Persisted (`messages.suppress_embeds`), echoed on broadcast
and history. Receivers skip link-preview cards and GIF media for that
message and show the URL text (no `loneLink` hiding). Invite cards still
render (they resolve against our central, not the link). Works in
encrypted channels: it depends on the sender, not the content.

## Category sync

Revised the same day (owner decision, after a review note that following
the new category on a drag could quietly make a private channel public).
The rule: **nothing but an explicit sync changes what a channel allows.**

- A category is a **plain group** unless its switch "Channels follow this
  category's permissions" is on (`channels.category_sync`,
  `ChannelInfo.category_sync = 14`, default **off**, which is how every
  server behaved before v3). Its rows then still decide whether its own
  header shows, but never touch its channels.
- A channel **follows** its category (`channels.perm_synced`,
  `ChannelInfo.permissions_synced = 13`) only under a syncing category.
  Invariants: a synced channel has no rows of its own, and its category
  syncs (`enforce_sync_invariant_unlocked_` at open and after layout
  changes).
- Resolution reads the rows of the channel's **source**: its category when
  it follows one, else itself. `overwrite_source_` is cached and cleared on
  create / delete / reorder / sync changes.
- How a channel starts following: it's created inside a syncing category,
  or someone syncs it: "Sync now" in its settings, "Sync all" on the
  category, or "Sync permissions" in the drop prompt.
  `CHANNEL_PERMISSIONS_SYNC_REQ = 147` `{channel_id, category_id}` →
  `CHANNEL_ACTION_RES{action="sync"}`. Needs MANAGE_ROLES in the channel,
  a syncing category, the escalation guard (every bit that differs
  between the channel's rows and the category's, per target, must be one
  the actor holds in the channel) and the lock-out guard (actor loses VIEW
  → restore). `category_id`, when set, must be the channel's category at
  that moment (the drop prompt sends it right after the reorder; a failed
  or raced reorder can't sync the channel with its old category). A
  category id as `channel_id` syncs every channel under it that doesn't
  follow yet, each guarded on its own, and reports "Synced N of M".
- Everything else keeps what the channel allows, by copying the rows it
  followed onto it and unsyncing it:
  - **moves** (`reorder_channels`): any channel that followed a category
    and ends up under another one (or none), including channels shifted
    by a moved category header;
  - **switching a category off** (`CATEGORY_SYNC_SET_REQ = 148`
    `{channel_id, enabled}` → `CHANNEL_ACTION_RES{action="category_sync"}`,
    MANAGE_ROLES in the category). Switching it on changes nothing until
    channels sync;
  - **deleting a category**.
- Editing a following channel's overwrites copies the category's rows onto
  it first (same effective result), then applies the edit; it no longer
  follows.
- The client asks on drop. A channel dropped into a different category
  whose switch is on gets "Move #x into Category?" with **Sync
  permissions** (needs MANAGE_ROLES in the channel), **Keep permissions**,
  and **Cancel**. Sync = reorder + sync request; Keep = reorder only;
  Cancel = nothing. No prompt anywhere else, since the server keeps
  permissions on every other move anyway.
- `CHANNEL_OVERWRITES_RES` for a following channel returns the category's
  rows (what applies); the client shows "Synced with <category>" or "Not
  synced … Sync now" under syncing categories only.
- Migration v12: adds `category_sync` (off). A DB that ran v11 live keeps
  syncing exactly the categories that carry rows (their channels were
  following them); a v10 → v12 upgrade leaves every category plain, even
  one with formerly inert rows.
- Visibility: a category is listed when the user can VIEW it **or** any
  channel under it is visible (previously categories were always listed).

## Thumbnail leak

`FETCH_STREAM_THUMBNAIL_REQ` served any streamer's latest thumbnail by
username. The roster is server-wide, so anyone could pull the live
thumbnail of a stream in a voice channel hidden from them. Now the
server looks up the channel the target is streaming in and requires
VIEW_CHANNEL there; no active stream means no thumbnail. (Thumbnails stay
unencrypted on purpose, for people outside the call, not for hidden
channels.)

## Wire summary

- `Permission`: `PERM_MUTE_MEMBERS` (renamed 1<<18), `PERM_DEAFEN_MEMBERS`,
  `PERM_MOVE_MEMBERS`, `PERM_CREATE_INVITE`, `PERM_CHANGE_NICKNAME`,
  `PERM_EMBED_LINKS`, `PERM_SPEAK`.
- `RoleInfo.manage_each_other = 7`, `RoleCreateRequest.manage_each_other = 4`,
  `RoleUpdateRequest.manage_each_other = 6`.
- `ChannelInfo.permissions_synced = 13`, `ChannelInfo.category_sync = 14`.
- `ChannelMessage.suppress_embeds = 15`, `ChannelMessageEdited.suppress_embeds = 7`.
- `VoiceUserState.is_suppressed = 6`.
- `Packet.CHANNEL_PERMISSIONS_SYNC_REQ = 147` +
  `ChannelPermissionsSyncRequest channel_permissions_sync_req = 149`
  `{channel_id, category_id}`; `Packet.CATEGORY_SYNC_SET_REQ = 148` +
  `CategorySyncSetRequest category_sync_set_req = 150` `{channel_id, enabled}`.

## Client

- `permissions.ts`: new bits and labels; channel-overwrite subset gains
  Embed Links, Speak, Mute / Deafen / Move Members.
- Role editor: "Members with this role can manage each other" toggle.
- `useHierarchy` gains `canManage(username)` (strict or peer) for the
  nickname and voice menus; kick / ban / timeout / roles keep the strict
  check.
- Voice context menu: mute / deafen / move / disconnect gated per bit
  in the target's channel (`myPermissions` of that channel); "Move to"
  lists every voice channel the local user can see.
- Invites: create button on CREATE_INVITE; the list shows everyone's
  invites with MANAGE_INVITES, only your own otherwise.
- Own nickname editing gated on CHANGE_NICKNAME.
- Messages with `suppressEmbeds` render no link-preview / GIF media.
- Voice: `isSuppressed` badge; the local mic shows blocked when the
  user can't speak in the connected channel.
- Category settings: the "Channels follow this category's permissions"
  switch; when on, "N of M channels follow these permissions" + "Sync
  all" and the overwrite editor (hidden for a plain group). A channel under
  a syncing category shows "Synced with <category>" (editing un-syncs) or
  "Not synced … Sync now".
- Sidebar drop into a syncing category → `CategorySyncPrompt` (Sync / Keep
  / Cancel); `ConfirmModal` gained an optional middle choice and an
  accent tone.
