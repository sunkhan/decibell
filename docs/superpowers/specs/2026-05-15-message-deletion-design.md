# Message Deletion — Design

**Date:** 2026-05-15
**Author:** sunkhan (with Claude)
**Status:** Approved (pending implementation plan)

## Problem

Decibell has no way to delete a sent message — once it's persisted, it's there forever. Discord and every comparable app let users delete their own messages; community-server owners (and eventually role-permission holders) need to be able to delete other people's messages too. The persistence layer needs to release the storage cleanly: dropping the row from the DB *and* unlinking the on-disk blobs for any tied attachments.

## Goal

Hover any message bubble — in a community-server channel or a DM — and a red trash icon appears in the top-right. Click it, confirm via a styled modal, and the message disappears from every viewer's chat view and is hard-deleted from the server-side store. No tombstone, no "[deleted]" placeholder, no time limit.

Cross-device consistent: a delete on machine A is immediately visible on machine B (assuming both are connected). Offline viewers see the message gone on their next history fetch, because the DB is authoritative.

## Non-goals

- **Soft delete / audit log.** Hard delete only. The `deleted_by` field on the channel broadcast is forward-compat for an audit-log surface but no UI consumes it today.
- **Edit messages.** Out of scope; separate feature.
- **Restore.** No undo after the modal confirm.
- **Per-channel permissions.** Today there's a single "delete others' messages" permission scoped to the whole community. When roles ship, granularity may expand; this design only commits to the server-wide permission.
- **Rate-limit / abuse protection.** No throttling. If it becomes a problem we'll add server-side rate limiting later.
- **Bulk delete.** One message at a time.

## Architecture

```
┌──────────┐  DM_DELETE_REQ          ┌──────────┐
│ client   │ ───────────────────────▶│ central  │
│ (sender) │                         │          │
│          │◀─── DM_DELETE_RES ──────│          │
│          │     {success}           │          │
└──────────┘                         └────┬─────┘
                                          │
                                          │ DM_MESSAGE_DELETED
                                          │ (to peer, if online)
                                          ▼
                                     ┌──────────┐
                                     │ client   │
                                     │ (peer)   │
                                     └──────────┘


┌──────────┐  MESSAGE_DELETE_REQ     ┌──────────┐
│ client   │ ───────────────────────▶│community │
│          │                         │          │
│          │◀── MESSAGE_DELETE_RES ──│          │
│          │     {success}           │          │
└──────────┘                         └────┬─────┘
                                          │
                                          │ CHANNEL_MESSAGE_DELETED
                                          │ (to all members)
                                          ▼
                                     ┌──────────┐
                                     │ every    │
                                     │ session  │
                                     └──────────┘
```

DMs live in central's `dm_messages` (PostgreSQL). Channel messages live in each community's `messages` (SQLite). Both surfaces share the same UI affordance (trash icon on hover, modal confirm, optimistic-drop + rollback-on-failure) but the wire/storage path is independent.

## Section 1: UX

The trash icon shows on hover for every text-message bubble that the local user is allowed to delete. Users who can't delete a given message never see the icon for it — no "disabled" affordance.

**Visibility rules:**
- **DM message:** show iff `message.sender === localUsername`.
- **Channel message:** show iff `message.sender === localUsername` OR `serverOwner[activeServerId] === localUsername`. (When roles ship: also if local user has `DELETE_MESSAGES` role permission.)

**Modal copy:**
- Title: *Delete message*
- Body: *Delete this message? This cannot be undone.*
- Buttons: **Delete** (destructive red, existing button-variant), **Cancel** (neutral).

**Optimistic flow:**
1. User confirms in the modal.
2. Renderer snapshots the message into a per-store `pendingDeletions` map and removes it from the visible message array.
3. Renderer invokes `delete_dm_message` / `delete_channel_message`.
4. On server success → broadcast arrives → already-gone bubble stays gone → snapshot cleared.
5. On server failure (`*_delete_responded` with `success: false`) → snapshot is re-inserted via the existing `mergeMessage` helper (sorts by id) and `toast.error(message)` is surfaced.

**No time limit, no tombstone.** Channel attachments tied to a deleted message are deleted with it — rows dropped + storage blobs + thumbnail variants unlinked from disk.

## Section 2: Permissions

**DMs (sender-only):** atomic at the SQL level — the `DELETE` includes the sender in its `WHERE` clause, so a bad actor sending a forged packet with someone else's message id can't drop rows that aren't theirs. Recipient sees the bubble disappear via the broadcast; can't initiate deletion.

**Channel messages (self or owner):**

New helper on `CommunityDb`:

```cpp
/// Forward-compat for roles. Returns owner() == username today;
/// extends to include role-derived perms once roles ship.
bool can_delete_others(const std::string& username) const;
```

Handler-level gate:

```cpp
auto sender = db->get_message_sender(channel_id, message_id);
if (!sender) { /* 404 */ }
if (*sender != username_ && !db->can_delete_others(username_)) {
    /* 403 */
}
```

No new schema today, no `members.permissions` column, no migrations. The function name documents intent; when roles ship, its body grows an OR clause.

**Renderer mirror:** a small hook `useCanDeleteOthers(serverId)` returns `serverOwner[serverId] === localUsername`. When roles ship later, the hook extends to consult role state too. Single source of truth for the visibility check.

## Section 3: Wire protocol

Six new packet types and six new message bodies. Tag numbers continue from the last used (`SERVER_HEARTBEAT_RES = 75`).

```proto
// --- Persistent-DMs delete (client ↔ central) ---
DM_DELETE_REQ      = 76;   // client→central; JWT-authed
DM_DELETE_RES      = 77;   // central→requester (success or failure)
DM_MESSAGE_DELETED = 78;   // central→sender + recipient (success only)

// --- Channel-message delete (client ↔ community) ---
MESSAGE_DELETE_REQ      = 79;   // client→community
MESSAGE_DELETE_RES      = 80;   // community→requester (success or failure)
CHANNEL_MESSAGE_DELETED = 81;   // community→every authenticated session (success only)
```

```proto
message DmDeleteReq      { string peer = 1; int64 message_id = 2; }
message DmDeleteRes      {
  bool success = 1;
  string message = 2;
  string peer = 3;
  int64 message_id = 4;
}
message DmMessageDeleted {
  // Peer-relative-to-receiver. Central rewrites this per recipient:
  // the same row has different "peer" values from sender's vs
  // recipient's perspective.
  string peer = 1;
  int64 message_id = 2;
  int64 deleted_at = 3;
}

message MessageDeleteReq {
  string channel_id = 1;
  int64 message_id = 2;
}
message MessageDeleteRes {
  bool success = 1;
  string message = 2;
  string channel_id = 3;
  int64 message_id = 4;
}
message ChannelMessageDeleted {
  string channel_id = 1;
  int64 message_id = 2;
  int64 deleted_at = 3;
  // For future audit-log surfacing; not consumed by current UI.
  string deleted_by = 4;
}
```

**Why Res + Broadcast, not one or the other:**
- The Res is the requester's private ack — rejection reasons (403, 404) can't be broadcast.
- The Broadcast is what makes the bubble disappear on every other viewer's screen.
- Requester also receives the broadcast; their optimistic delete already cleared the bubble, so the broadcast handler is a no-op for them.

**Authentication:** both REQs ride existing authenticated sessions (`DM_DELETE_REQ` via the JWT-authed central session, `MESSAGE_DELETE_REQ` via the community-auth session). No new exemption in the central JWT gate.

## Section 4: Server-side processing

### Central — DM delete

```
DM_DELETE_REQ {peer, message_id} arrives on JWT-authed session
  ↓
ok = auth_manager_.deleteDmMessage(username_, peer, message_id)
   - Single SQL:
       DELETE FROM dm_messages
       WHERE id = $1 AND sender = $2 AND recipient = $3
   - Returns true iff affected_rows == 1
   - Atomic enforcement: "exists" + "sender-is-me" + "correct pair" all in
     the WHERE clause
  ↓
Send DM_DELETE_RES {success=ok, message=(ok ? "" : "Message not found or not deletable"),
                     peer, message_id} to requester
  ↓
If ok and recipient is online:
  Send DM_MESSAGE_DELETED to recipient with peer=requester's username
       (peer field is always "the other user" from the receiver's POV)
  Also send DM_MESSAGE_DELETED to sender with peer=peer
       (sender's own session, drives broadcast handler — idempotent
        with optimistic removal)
```

`dm_read_state` is intentionally untouched. The unread-count subquery joins against `dm_messages` — deleted rows simply stop counting. `last_read_id` doesn't regress.

**New `AuthManager` method:**

```cpp
/// Sender-enforced delete. Returns true iff a row was deleted (i.e.
/// the message existed AND the requester was its sender AND the
/// recipient matches). The WHERE clause is the authorization check.
bool deleteDmMessage(const std::string& sender,
                     const std::string& peer,
                     int64_t message_id);
```

### Community — channel message delete

```
MESSAGE_DELETE_REQ {channel_id, message_id} arrives on authenticated session
  ↓
auto sender = db->get_message_sender(channel_id, message_id)
   if (!sender) → MESSAGE_DELETE_RES {success=false, "Message not found."}; return
  ↓
Permission gate:
   if (*sender != username_ && !db->can_delete_others(username_)):
     MESSAGE_DELETE_RES {success=false, "You don't have permission to delete this message."}
     return
  ↓
DeleteMessageResult r = db->delete_message(channel_id, message_id)
   Transaction:
     - Fetch attachments WHERE message_id = $1, capture storage_paths into r.unlink_paths
     - DELETE FROM attachments WHERE message_id = $1
     - DELETE FROM messages WHERE id = $1
   Commit.
   FTS5 mirror trigger on messages auto-syncs the index.
  ↓
Send MESSAGE_DELETE_RES {success=true, ""} to requester
  ↓
For each path in r.unlink_paths (reuse exact pattern from CHANNEL_WIPE handler):
   std::filesystem::remove(path, ec)
   std::filesystem::remove(path + ".partial", ec)
   std::filesystem::remove(path + ".thumb.jpg", ec)
   std::filesystem::remove(path + ".thumb-320px.jpg", ec)
   std::filesystem::remove(path + ".thumb-640px.jpg", ec)
   std::filesystem::remove(path + ".thumb-1280px.jpg", ec)
  ↓
Broadcast CHANNEL_MESSAGE_DELETED {channel_id, message_id, deleted_at=now,
                                    deleted_by=username_}
   to every authenticated session via manager_.broadcast_to_members().
```

**New `CommunityDb` methods:**

```cpp
/// Returns the sender username, or nullopt if no such message in this channel.
std::optional<std::string> get_message_sender(
    const std::string& channel_id, int64_t message_id) const;

struct DeleteMessageResult {
    bool ok = false;
    std::vector<std::string> unlink_paths;
};

/// Hard-deletes the message + its bound attachments in one transaction.
/// Returns the storage paths the caller should unlink from disk.
DeleteMessageResult delete_message(
    const std::string& channel_id, int64_t message_id);

/// Forward-compat permission helper. owner() == u today; later also
/// owner OR role-with-DELETE_MESSAGES-perm.
bool can_delete_others(const std::string& username) const;
```

## Section 5: Native (napi-rs) + Renderer

### Native — commands

Two new `#[napi]` async functions:

```rust
// commands/dm.rs (appended)
#[napi(object)]
pub struct DeleteDmMessageArgs {
    pub peer: String,
    pub message_id: i64,
}
#[napi]
pub async fn delete_dm_message(args: DeleteDmMessageArgs) -> napi::Result<()>;

// commands/messaging.rs (or wherever channel sends live — confirm at impl time)
#[napi(object)]
pub struct DeleteChannelMessageArgs {
    pub server_id: String,
    pub channel_id: String,
    pub message_id: i64,
}
#[napi]
pub async fn delete_channel_message(args: DeleteChannelMessageArgs) -> napi::Result<()>;
```

Both build a packet with the existing JWT/community-auth tokens and send via the existing `connection_write_tx` plumbing. Fire-and-forget; the ack arrives as an event.

### Native — events

Four new emitted events from `net/central.rs` and `net/community.rs` packet routing:

| Event | Payload (camelCase) | Source packet |
|---|---|---|
| `dm_message_delete_responded` | `{success, message, peer, messageId}` | `DM_DELETE_RES` |
| `dm_message_deleted` | `{peer, messageId, deletedAt}` | `DM_MESSAGE_DELETED` |
| `channel_message_delete_responded` | `{success, message, serverId, channelId, messageId}` | `MESSAGE_DELETE_RES` |
| `channel_message_deleted` | `{serverId, channelId, messageId, deletedAt, deletedBy}` | `CHANNEL_MESSAGE_DELETED` |

### Renderer — store changes

Two new actions plus per-store rollback maps:

```ts
// chatStore.ts
removeMessage: (channelId: string, messageId: number) => void;
pendingDeletions: Record<string /*channelId*/, Map<number /*id*/, Message>>;

// dmStore.ts
removeDmMessage: (peer: string, messageId: number) => void;
pendingDmDeletions: Record<string /*peer*/, Map<number /*id*/, DmMessage>>;
```

Both `removeMessage` / `removeDmMessage` filter `conv.messages` by `m.id !== messageId`. Idempotent.

`pendingDeletions` / `pendingDmDeletions` hold the snapshot during the round-trip so a server-rejected delete can be re-inserted via the existing `mergeMessage` (which sorts by id ascending).

### Renderer — event handlers

`useServerEvents` adds two new listeners:
- `channel_message_delete_responded` → on `success=false`, restore the snapshot from `pendingDeletions[channelId]` via `mergeMessage` + `toast.error(message)`. On `success=true`, clear the snapshot.
- `channel_message_deleted` → unconditionally `removeMessage(channelId, messageId)` + clear any pending snapshot (covers the case where another viewer deleted the message; also a no-op for our own success-echo broadcast).

`useDmEvents` adds the two DM equivalents (`dm_message_delete_responded`, `dm_message_deleted`).

### Renderer — MessageBubble

Trash icon is a new absolutely-positioned button inside the existing `group` wrapper:

```tsx
{canDelete && (
  <button
    onClick={handleDeleteClick}
    title="Delete message"
    className="absolute right-2 top-1 hidden h-6 w-6 items-center justify-center
               rounded-md bg-bg-secondary text-error hover:bg-error/10
               group-hover:flex"
  >
    <TrashIcon className="h-3.5 w-3.5" />
  </button>
)}
```

`canDelete` and `onDelete` are new optional props on `MessageBubble`. Parent components compute them:
- `ChatPanel`: `canDelete = message.sender === localUsername || serverOwner[activeServerId] === localUsername`; `onDelete` invokes the channel delete flow.
- `DmChatPanel`: `canDelete = message.sender === localUsername`; `onDelete` invokes the DM delete flow.

`handleDeleteClick` opens a new modal (id `"delete-message-confirm"`) via `useUiStore.openModal`, with the context payload {kind: "dm"|"channel", …ids…} so the modal knows which native command to fire on confirm.

### Renderer — `DeleteMessageConfirmModal`

New component at `electron-client/src/components/DeleteMessageConfirmModal.tsx`. Standard modal shell (matches existing modal infrastructure — e.g. `ChannelSettingsModal`, `InviteManageModal`). Body:

> Delete this message?
> This cannot be undone.

Buttons: red **Delete** (destructive variant), neutral **Cancel**. On Delete: snapshot the message, call `removeMessage` / `removeDmMessage`, invoke the appropriate native command, close the modal.

## Error handling matrix

| Failure mode | Behavior |
|---|---|
| Permission denied (channel; non-owner trying to delete someone else's message) | Server sends `MESSAGE_DELETE_RES {success=false, message="You don't have permission to delete this message."}`. Renderer restores snapshot + toast.error. |
| Message not found (already deleted by someone else seconds ago) | Server sends `*_DELETE_RES {success=false, message="Message not found."}`. Renderer restores snapshot, but `mergeMessage` finds the same id already in the array (the other deleter's broadcast might still be in flight), so behavior degrades gracefully to "no double bubble". Toast info: harmless. |
| Network error mid-request | Renderer arms a 5-second watchdog `setTimeout` after the optimistic remove. If no matching `*_delete_responded` event lands by then, restore the snapshot via `mergeMessage` + `toast.error("Failed to delete message; please try again.")` + clear the pending entry. If a late response lands after the watchdog fires, both sides of the snapshot-clearing logic are guarded by a "was this id still pending?" check, so the late response is a harmless no-op. |
| Recipient offline (DM) | Broadcast is dropped on the floor on central; recipient sees the message gone on next `DM_HISTORY_REQ` or `DM_CONVERSATIONS_REQ` (DB is the source of truth). |
| Server crashes between DELETE and broadcast | DB is committed; broadcast is missed. Other viewers see the message gone on next `CHANNEL_HISTORY_REQ`. Acceptable — DB is authoritative. |
| Concurrent delete of same message (rare race) | Second delete returns `success=false, "Message not found."` — same handling as above. |

## File-level change list (preview for the implementation plan)

- `proto/messages.proto` — 6 new packet types, 6 new oneof entries, 6 new messages
- `src/server/auth_manager.{hpp,cpp}` — `deleteDmMessage` method
- `src/server/main.cpp` — `DM_DELETE_REQ` handler (Res + broadcast to peer)
- `src/community/db.{hpp,cpp}` — `get_message_sender`, `delete_message`, `can_delete_others`, `DeleteMessageResult` struct
- `src/community/main.cpp` — `MESSAGE_DELETE_REQ` handler (perm gate, DB call, filesystem unlink, broadcast)
- `electron-client/native/src/commands/dm.rs` — `delete_dm_message`
- `electron-client/native/src/commands/messaging.rs` (or location of channel commands) — `delete_channel_message`
- `electron-client/native/src/net/central.rs` — `DM_DELETE_RES` + `DM_MESSAGE_DELETED` packet arms → emit events
- `electron-client/native/src/net/community.rs` — `MESSAGE_DELETE_RES` + `CHANNEL_MESSAGE_DELETED` packet arms → emit events
- `electron-client/native/src/events.rs` — 4 new event names + payload structs + emit fns
- `electron-client/src/stores/chatStore.ts` — `removeMessage` + `pendingDeletions`
- `electron-client/src/stores/dmStore.ts` — `removeDmMessage` + `pendingDmDeletions`
- `electron-client/src/features/servers/useServerEvents.ts` — `channel_message_delete_responded` + `channel_message_deleted` listeners
- `electron-client/src/features/dm/useDmEvents.ts` — `dm_message_delete_responded` + `dm_message_deleted` listeners
- `electron-client/src/features/chat/MessageBubble.tsx` — `canDelete`/`onDelete` props + trash button
- `electron-client/src/features/chat/ChatPanel.tsx` — compute `canDelete` per row, wire `onDelete` → channel flow
- `electron-client/src/features/dm/DmChatPanel.tsx` — compute `canDelete` per row, wire `onDelete` → DM flow
- `electron-client/src/components/DeleteMessageConfirmModal.tsx` (new) — confirm modal
- `electron-client/src/features/servers/useCanDeleteOthers.ts` (new) — hook for owner-or-role check

Rough effort: ~500 LOC new, ~150 LOC modified.
