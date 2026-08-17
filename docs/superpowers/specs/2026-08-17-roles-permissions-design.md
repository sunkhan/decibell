# Roles + Permissions (v1) + Channel Management + Nicknames

Status: implemented 2026-08-17 (community server + Electron client).

## Model

Discord-shaped, deliberately minimal:

- **Role** = name, color (0xRRGGBB), dense hierarchy `position`, and a
  `permissions` bitfield. Stored per community server in SQLite
  (`roles`, `member_roles`; migration v5 in `src/community/db.cpp`).
- **`everyone`** is a real seeded role: `is_default=1`, position 0,
  undeletable/unmovable, implicit on every member (never listed in
  `member_roles` or `MemberInfo.role_ids`). Only its permission bits are
  editable. It seeds with the reserved default-on bits so future
  enforcement of send/connect/stream needs no migration.
- **Effective permissions** = `everyone.permissions | OR(member's roles)`.
  The owner implicitly holds every permission. `ADMINISTRATOR` expands
  to everything but does NOT bypass hierarchy.
- **Hierarchy**: a member's level is their highest role position (owner =
  ∞). Rules, enforced server-side and mirrored in the UI:
  - manage/edit/delete/assign only roles **strictly below** your level;
  - kick/ban only members **strictly below** your level;
  - the escalation guard: you can only toggle permission bits **you
    yourself hold** (checked on the changed bits, so foreign bits a role
    already carries survive an edit untouched).

## Permission bits (wire contract — append-only, never renumber)

Defined in `proto/messages.proto` (`Permission` enum), mirrored in
`src/community/db.hpp` (`chatproj::perms`) and
`electron-client/src/features/servers/permissions.ts` (`PERM`).

| bit | name | gates |
|-----|------|-------|
| 1<<0 | ADMINISTRATOR | everything below |
| 1<<1 | MANAGE_SERVER | server picture (name/desc when editable) |
| 1<<2 | MANAGE_CHANNELS | retention update, channel wipe; channel CRUD later |
| 1<<3 | MANAGE_ROLES | role CRUD + assignment (hierarchy-gated) |
| 1<<4 | KICK_MEMBERS | kick |
| 1<<5 | BAN_MEMBERS | ban, unban, ban-list visibility |
| 1<<6 | MANAGE_MESSAGES | delete others' messages |
| 1<<7 | MANAGE_INVITES | invite create/list/revoke |
| 1<<8 | MANAGE_NICKNAMES | change lower-ranked members' nicknames |
| 1<<10..12 | SEND_MESSAGES / CONNECT_VOICE / STREAM | reserved, default-on, unenforced |

### Capacity + growth

The field is a `uint64` end-to-end (proto / C++ / SQLite int64), so the
protocol has 64 bits of append-only runway (Discord uses ~50 today).
Practical ceilings by layer:

- **JS/JSON**: numbers are exact up to 2^53 → bits 0..52 usable as-is.
  The renderer's bit math goes through BigInt-backed helpers
  (`hasBits`/`toggleBit` in `permissions.ts`) because native JS bitwise
  operators truncate to 32-bit signed ints — never use `|`/`&`/`~` on a
  permission mask directly.
- **Past bit 52** (if ever needed): switch the event/command layer to
  carrying the bitfield as a decimal string (the servers are already
  64-bit clean); mechanical change, old clients degrade to UI-gating
  only, which is safe because the server is authoritative.
- The server masks client-supplied bitfields with `perms::kKnownMask`
  on role create/update, so undefined bits never reach the DB — a new
  bit becomes storable exactly when a server build that defines it
  ships.

## Wire protocol

New packet types 88–95: `ROLE_LIST_REQ/RES`, `ROLE_CREATE/UPDATE/DELETE_REQ`,
`ROLE_ACTION_RES` (shared result, `action` ∈ create/update/delete/assign),
`MEMBER_ROLES_UPDATE_REQ` (replaces a member's full role set),
`UNBAN_MEMBER_REQ` (answered via `MOD_ACTION_RES` with `action="unban"`).
`MemberInfo` gains `repeated int64 role_ids = 6`.

Flow: the server pushes `ROLE_LIST_RES` right after a successful
`COMMUNITY_AUTH_RES` and re-broadcasts it on every role change; member
role assignments ride the existing `MEMBER_LIST_RES` broadcasts. Clients
resolve `role_ids` against the role list for names/colors/permissions.

**Compatibility**: legacy clients ignore the new packets/fields; legacy
servers never send a role list, and the client's permission hooks fall
back to owner-only gating when `rolesByServer` is empty — identical to
pre-roles behavior in both directions.

## Server implementation notes

- `CommunityDb` API: `list_roles`, `get_role`, `create_role` (inserts at
  position 1, shifts up), `update_role` (list-move keeps positions
  dense), `delete_role` (cascades `member_roles`, closes the gap),
  `set_member_roles`, `effective_permissions`, `has_permission`,
  `member_level`. Kick/ban/leave clear the target's `member_roles`
  (roles do not survive rejoin).
- Every previously owner-gated handler now goes through
  `has_permission()`: retention, wipe, server picture, invites,
  kick/ban, ban-list visibility, message deletion
  (`can_delete_others` → MANAGE_MESSAGES).
- e2e-covered by the scratchpad test client (26 scenarios incl. the
  full role lifecycle, hierarchy rejections, escalation guard, unban).

## Client implementation notes

- Native: commands `list_roles`, `create_role`, `update_role`,
  `delete_role`, `set_member_roles`, `unban_member`; events
  `role_list_received`, `role_action_responded`.
- Renderer: `chatStore.rolesByServer`;
  `features/servers/permissions.ts` (`PERM`, `usePermission`,
  `useHierarchy`, `computeEffectivePermissions`, `memberLevel`).
  `useCanDeleteOthers` / `useCanEditServerSettings` now delegate to it.
  `MembersAdminPanel` gains a Roles tab (create/edit/delete/reorder,
  permission checkboxes, color presets), per-member role assignment,
  hierarchy-aware kick/ban buttons, and Unban on the bans tab. The
  profile popup shows real role chips; member rows tint the display
  name with the top role's color.

## Channel management (same 2026-08-17 pass)

Packet types 96–100: `CHANNEL_CREATE_REQ` / `CHANNEL_DELETE_REQ` /
`CHANNEL_RENAME_REQ` → `CHANNEL_ACTION_RES` (shared result), plus
`CHANNEL_LIST_UPDATE` — a full ordered channel-list push broadcast after
any structural change (clients replace their list wholesale and purge
cached message state for channels that disappeared, since a recreated
channel can reuse the slug). All gated by MANAGE_CHANNELS.

- **Ids are slugs**: server-generated from the name (lowercase
  `[a-z0-9-]`, ≤32 chars, `-2`/`-3` suffix on collision), immutable —
  they double as the attachment directory name, so renames touch only
  the display name. New channels append at position max+1.
- **Delete** wipes messages + attachments (blob unlink included) and
  removes the row in one transaction. Guards: the last text channel is
  undeletable (it's the post-auth landing channel), and an occupied
  voice channel must empty out first.
- Client: "+" buttons on the sidebar section headers →
  `CreateChannelModal`; rename field + Delete danger-zone card in
  `ChannelSettingsModal` (whole modal now MANAGE_CHANNELS-gated instead
  of owner-gated).

## Categories + drag-reorder (same 2026-08-17 pass)

Discord's flat sidebar model: `ChannelInfo.Type` gains `CATEGORY = 2` —
categories are rows in the same ordered `channels` table/list. A
channel's category is **implicit**: the nearest CATEGORY row above it
in position order; channels before the first category are
uncategorized (the header-less area pinned at the top). Categories
can't nest, carry no messages (CHANNEL_MSG + attachment-init reject
them), and reuse the whole channel CRUD surface (create/rename/delete;
deleting one lets its channels reflow to the block above).

- `ChannelCreateRequest.category_id`: "" = end of the uncategorized
  area; a category id = end of that category's block. Creation
  renumbers positions densely (position gaps from deletes otherwise
  corrupt index-based placement — caught by e2e).
- `CHANNEL_REORDER_REQ` (type 102): the client sends the complete new
  flat order after a drag; the server validates the id set matches
  exactly (concurrent create/delete ⇒ clean rejection + resync push),
  rewrites positions 0..N-1 transactionally, broadcasts
  `CHANNEL_LIST_UPDATE`. MANAGE_CHANNELS.
- Client: sidebar renders the flat grouped list with collapsible
  category headers (hover "+" creates inside the category); HTML5
  drag-reorder with an insertion indicator — categories move with
  their whole child block and snap to block boundaries so they never
  nest or capture the top uncategorized area; right-click context menu
  (MANAGE_CHANNELS) on rows (settings/delete) and on empty space
  (create text/voice/category). Optimistic reorder, server broadcast
  confirms.

## Nicknames (same 2026-08-17 pass)

`SET_NICKNAME_REQ` (type 101) → `MOD_ACTION_RES` with
`action="nickname"`; roster broadcast follows. Self-changes are always
allowed; changing another member requires MANAGE_NICKNAMES **and** a
strictly higher role. 32-char cap, empty clears. Client: inline "Nick"
editor on member rows in `MembersAdminPanel`.

## Deferred (v2+)

Per-channel permission overwrites + private channels, channel
drag-reorder (the protocol's position field + list push already support
it), ownership transfer, enforcement of the reserved
send/connect/stream bits, audit log, colored names in the members
sidebar / chat, role mentions. Note: per-channel overwrites require
attachment-download checks to become channel-aware
(`attachment_http.cpp` currently checks membership only).
