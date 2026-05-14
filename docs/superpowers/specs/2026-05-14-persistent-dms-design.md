# Persistent DMs — Design

**Date:** 2026-05-14
**Author:** sunkhan (with Claude)
**Status:** Approved (pending implementation plan)

## Problem

DMs currently live only in the renderer's in-memory `dmStore`. Two consequences:

1. **No history persistence** — the message log vanishes on every app close, login, or even just a renderer reload.
2. **No offline delivery** — when a DM is sent to a recipient who isn't currently authenticated to the central server, `send_private()` returns false and the sender gets `"This user is currently offline. Your message could not be delivered."`. The message is then permanently lost; the recipient sees nothing when they come back online.

The central server has been pre-staged for this fix — there's already a `// Offline queuing to PostgreSQL will go here` comment in the existing `DIRECT_MSG` handler (`src/server/main.cpp:233`). This spec fills it in.

## Non-goals

- **End-to-end encryption.** DMs in the DB are plain text, just like community channel messages already in production. Wire is TLS; disk is the OS. E2E is a much bigger feature and out of scope.
- **Read receipts visible to the sender.** We track `last_read_id` per (reader, peer) for unread *counts* on the reader's side, not "Seen at hh:mm" for the sender's side.
- **Edit / delete / reactions.** Out of scope; could be added later as additive features.
- **DM attachments.** DMs remain text-only with emojis. Attachments are a community-channel feature.

## Scope

Two complementary capabilities, shared underlying primitive (every DM is persisted before any delivery decision):

1. **History persistence** — old DMs stay visible after restart/relogin.
2. **Offline delivery** — DMs to offline recipients are queued (just by virtue of being in the DB) and surfaced when they next log in.

Plus:

3. **Per-conversation unread counts** in the DM sidebar.

Userbase is small (a friend group, ~10-20 users). Storage estimate: ~9 MB/year at current usage, ~250 MB/year if it scales 25×.

## Architecture

```
┌──────────┐   DIRECT_MSG   ┌───────────────────────────┐   live route   ┌──────────┐
│ sender   │ ─────────────► │ central server             │ ─────────────► │ recipient│
└──────────┘                │  - identity / privacy chk  │  (if online)   └──────────┘
                            │  - insertDm() → id          │
                            │  - stamp id on routed pkt   │
                            │  - echo to sender           │
                            └──────────────┬─────────────┘
                                           │
                                           ▼
                                    ┌──────────────┐
                                    │ Postgres     │
                                    │  dm_messages │
                                    │  dm_read_state│
                                    └──────────────┘
                                           ▲
                                           │
                            DM_CONVERSATIONS_REQ (on login)
                            DM_HISTORY_REQ (on panel mount + paginate)
                            DM_MARK_READ_REQ (on view, debounced)
                                           │
                                    ┌──────────────┐
                                    │ recipient    │
                                    │ (next login) │
                                    └──────────────┘
```

## Section 1: Schema & data model

Two new Postgres tables. Both are added by an idempotent `CREATE TABLE IF NOT EXISTS` block in `AuthManager::initializeDatabase()`, alongside the existing `users` / `friends` / `community_invites` / `community_servers` schemas.

```sql
CREATE TABLE IF NOT EXISTS dm_messages (
  id BIGSERIAL PRIMARY KEY,
  sender VARCHAR(32) NOT NULL,
  recipient VARCHAR(32) NOT NULL,
  content TEXT NOT NULL,
  sent_at BIGINT NOT NULL
);

-- Two-direction lookup ("messages between A and B" hits the same B-tree
-- regardless of who sent which) — both pair components are normalised by
-- LEAST/GREATEST so a single index serves both query directions.
CREATE INDEX IF NOT EXISTS dm_messages_pair_idx
  ON dm_messages (LEAST(sender, recipient), GREATEST(sender, recipient), id DESC);

-- For per-recipient unread queries (`WHERE recipient = me AND id > last_read_id`).
CREATE INDEX IF NOT EXISTS dm_messages_recipient_idx
  ON dm_messages (recipient, id DESC);

CREATE TABLE IF NOT EXISTS dm_read_state (
  reader VARCHAR(32) NOT NULL,
  peer VARCHAR(32) NOT NULL,
  last_read_id BIGINT NOT NULL DEFAULT 0,
  PRIMARY KEY (reader, peer)
);
```

**Row size:** ~150 B per DM including indexes (~24 B Postgres tuple header + ~16 B per varchar + ~60 B avg content + ~8 B timestamp + index overhead). `dm_read_state` rows are ~50 B and capped by friend-group size.

**Storage projection:** ~5k DMs/month × 150 B ≈ 9 MB/year. At 25× scale: ~250 MB/year. Negligible.

**Retention:** none. DMs live forever. No periodic-sweep cron.

**Encryption at rest:** plain text, same as community channel messages.

## Section 2: Wire protocol

### Packet types

```proto
DM_CONVERSATIONS_REQ = 68;   // client → central; empty payload
DM_CONVERSATIONS_RES = 69;   // central → client
DM_HISTORY_REQ       = 70;   // client → central
DM_HISTORY_RES       = 71;   // central → client
DM_MARK_READ_REQ     = 72;   // client → central; fire-and-forget, no response
```

### Messages

```proto
message DmConversationsReq {}

message DmConversationPreview {
  string peer = 1;
  string last_message_content = 2;
  string last_message_sender = 3;  // either local user or peer
  int64  last_message_id = 4;
  int64  last_timestamp = 5;
  int64  unread_count = 6;
}
message DmConversationsRes {
  repeated DmConversationPreview conversations = 1;
}

message DmHistoryReq {
  string peer = 1;
  int64  before_id = 2;   // 0 = latest
  int32  limit = 3;       // server clamps to 200
}
message DmHistoryMessage {
  int64  id = 1;
  string sender = 2;
  string content = 3;
  int64  timestamp = 4;
}
message DmHistoryRes {
  string peer = 1;
  repeated DmHistoryMessage messages = 2;  // newest first (matches community channel history)
  bool   has_more = 3;
}

message DmMarkReadReq {
  string peer = 1;
  int64  up_to_id = 2;
}
```

### Existing `DirectMessage` gets one new field

```proto
message DirectMessage {
  string sender = 1;
  string recipient = 2;
  string content = 3;
  int64  timestamp = 4;
  int64  id = 5;          // NEW — set by central after insertDm
}
```

Backwards compatible — old clients ignore unknown fields. New clients use `id` to feed `DmMarkReadReq.up_to_id`.

## Section 3: Server flow

### `AuthManager` additions (`src/server/auth_manager.{hpp,cpp}`)

New methods, each owning its own pqxx connection (matching the avatar/getFriends pattern):

```cpp
int64_t insertDm(const std::string& sender,
                 const std::string& recipient,
                 const std::string& content,
                 int64_t sent_at);

struct DmHistoryRow {
  int64_t id;
  std::string sender;
  std::string content;
  int64_t timestamp;
};
std::vector<DmHistoryRow> fetchDmHistory(const std::string& user_a,
                                          const std::string& user_b,
                                          int64_t before_id,   // 0 = latest
                                          int32_t limit,
                                          bool& has_more);

struct DmConversationPreviewRow {
  std::string peer;
  std::string last_message_content;
  std::string last_message_sender;
  int64_t last_message_id;
  int64_t last_timestamp;
  int64_t unread_count;
};
std::vector<DmConversationPreviewRow> fetchDmConversations(const std::string& user);

void markDmRead(const std::string& reader,
                const std::string& peer,
                int64_t up_to_id);
```

`fetchDmConversations` is the only non-trivial query — uses a CTE / lateral join to find the latest message per `(LEAST(sender,recipient), GREATEST(sender,recipient))` pair where `user` is one side, then joins `dm_read_state` for the unread count via `COUNT(*) FILTER (WHERE id > coalesce(rs.last_read_id, 0))`.

### Modified `DIRECT_MSG` handler

Reordered:

1. Identity stamp: `dmsg->set_sender(username_)`, `dmsg->set_timestamp(now)` (unchanged).
2. Friends-only privacy check via `check_dm_allowed` (unchanged). Failure → send the existing "friends-only" error packet, **do not persist**.
3. **Persist** (new): `int64_t new_id = auth_manager_.insertDm(...)`. DB failure → log + send a generic "could not deliver" error packet to sender (rare path).
4. **Stamp id** (new): `dmsg->set_id(new_id)`.
5. **Live delivery**: `manager_.send_private(routed_packet, recipient)`. Return value no longer gates user-visible behavior.
6. **Always echo to sender** with the routed packet (carries the new `id`).
7. **Remove** the `"This user is currently offline..."` error packet entirely. The message IS persisted; that error text no longer matches reality.

### New handlers (modeled on `FETCH_AVATAR_REQ`)

- `DM_CONVERSATIONS_REQ`: `auth_manager_.fetchDmConversations(username_)` → build `DmConversationsRes` → `deliver(...)`.
- `DM_HISTORY_REQ`: validate `req.peer()` is non-empty; clamp `limit` to `[1, 200]`; `auth_manager_.fetchDmHistory(username_, req.peer(), req.before_id(), limit, has_more)` → build `DmHistoryRes`.
- `DM_MARK_READ_REQ`: `auth_manager_.markDmRead(username_, req.peer(), req.up_to_id())`. No reply.

### Self-DM guard

`send_private` already drops self-DMs implicitly, but the new persistence path could quietly write self-rows. Reject explicitly at the top of the `DIRECT_MSG` handler: `if (dmsg->recipient() == username_) return;`.

## Section 4: Client integration

### Native (Rust addon)

Three new napi commands in `electron-client/native/src/commands/dm.rs` (new file, or alongside existing DM code if any):

```rust
#[napi]
pub async fn request_dm_conversations() -> napi::Result<()>;

#[napi(object)]
pub struct RequestDmHistoryArgs {
    pub peer: String,
    pub before_id: i64,
    pub limit: i32,
}
#[napi]
pub async fn request_dm_history(args: RequestDmHistoryArgs) -> napi::Result<()>;

#[napi(object)]
pub struct MarkDmReadArgs {
    pub peer: String,
    pub up_to_id: i64,
}
#[napi]
pub async fn mark_dm_read(args: MarkDmReadArgs) -> napi::Result<()>;
```

Each is fire-and-send (no oneshot waiter) — responses arrive via the existing `route_packets` central client loop and are emitted as named events to the renderer. Pattern matches the existing `request_server_list` flow.

Three new event arms in `net/central.rs::route_packets`:

- `DmConversationsRes` → emit `dm_conversations_received { conversations: [...] }`
- `DmHistoryRes` → emit `dm_history_received { peer, messages: [...], has_more }`
- (No event for `DmMarkReadReq` — fire-and-forget; TCP delivery is the implicit ack)

### Renderer

**`stores/dmStore.ts`** — extend each conversation:

```ts
interface DmConversation {
  username: string;
  messages: DmMessage[];        // existing
  lastMessageTime: number;      // existing
  unreadCount: number;          // NEW
  lastReadId: number;           // NEW (used to compute optimistic mark-read)
  hasMoreHistory: boolean;      // NEW (drives scroll-up pagination)
  historyLoaded: boolean;       // NEW (false until first DmHistoryRes arrives)
}
```

New actions: `hydrateConversations(previews[])`, `appendHistory(peer, messages[], hasMore)`, `markRead(peer, upToId)` (optimistic, zeroes count).

**`features/dm/useDmEvents.ts`** (new hook, mirrors `useAuthEvents`, `useFriendsEvents`): subscribes to:
- `dm_conversations_received` → `dmStore.hydrateConversations(payload.conversations)`
- `dm_history_received` → `dmStore.appendHistory(payload.peer, payload.messages, payload.hasMore)`
- (Existing `direct_message_received` listener — extend to also bump unread for the peer when the local user isn't currently viewing the conversation, and store the new `id` field on the appended message.)

**`features/auth/useAuthEvents.ts`** — on `login_succeeded`, alongside the existing `request_server_list` call:
```ts
invoke("request_dm_conversations").catch(() => {});
```

**`features/dm/DmChatPanel.tsx`** — on mount for a given peer:
```ts
if (!dmStore.conversations[peer]?.historyLoaded) {
  invoke("request_dm_history", { peer, beforeId: 0, limit: 50 });
}
```
On scroll-near-top with `hasMoreHistory`: invoke with `beforeId = oldestMessageId`.
On visible + focused: debounced (≤1/s) `invoke("mark_dm_read", { peer, upToId: latestId })`, and optimistically `dmStore.markRead(peer, latestId)`.

**`layouts/DmSidebar.tsx`** — render `unreadCount` badge per conversation (existing avatar gets a small pill overlay, similar in feel to the online-status dot already there). Click on a conversation auto-clears via the panel mount.

## Section 5: Edge cases

| Case | Behavior |
|------|----------|
| Sender + recipient both online | Persist, deliver live, echo to sender. Recipient's `unreadCount` for that peer increments by 1 (until they mark-read). |
| Recipient offline, sender online | Persist, no live delivery. Recipient's next `dm_conversations_received` (fired by their login) shows the new last-message + unread bump. Sender's UI shows DM delivered (no more "user offline" error). |
| Long-running conversation, 5000 historical messages | Initial fetch = 50. Scroll-up loads pages of 50 via `before_id` until `has_more=false`. |
| Friends-only rejection | Same as today: error packet to sender, no DB write, no unread increment. |
| Self-DM (sender == recipient) | Rejected explicitly in handler. No DB write. |
| Pre-existing in-memory DMs from this session | `dm_conversations_received` replaces preview state with server truth; per-peer `messages[]` array merges incoming history with already-live messages, deduplicated by `id`. |
| Sender's app crashes after sending but before recipient online | The DB row is there; recipient gets it on next login. No lost messages. |
| `mark_dm_read` arrives at server with `up_to_id` > latest existing id (race) | `markDmRead` is a simple UPSERT setting `last_read_id = GREATEST(existing, up_to_id)`; harmless. |
| DB transiently unavailable on insert | Surface as a generic "couldn't deliver" error packet to sender. Sender's UI shows the inline error block (same affordance as the existing friends-only error). User can retry the send. |

## Section 6: What's out of scope (deliberately)

| Item | Why |
|------|-----|
| End-to-end encryption | Big feature; needs key exchange, recovery, cross-device sync. Plain text in DB matches community channels. |
| Read receipts visible to sender | Asymmetric: this spec tracks the *reader's* unread count, but doesn't surface "seen at" to the sender. Adding it later = client-side opt-in + a server broadcast on mark-read. |
| Edit / delete | Additive; could land in v2. |
| Reactions | Same. |
| Pinned DMs | Same. |
| Typing indicators | Same; non-persistent live signal. |
| DM attachments | Community-channel feature; DMs stay text + emoji. |

## Section 7: File-level change list (preview for the implementation plan)

- `proto/messages.proto` — 5 new packet types + 5 new oneof entries + 4 new messages + 1 new field on `DirectMessage`
- `src/server/auth_manager.hpp` — declarations for `insertDm`, `fetchDmHistory`, `fetchDmConversations`, `markDmRead` + supporting POD structs
- `src/server/auth_manager.cpp` — DDL in `initializeDatabase`; method bodies
- `src/server/main.cpp` — modified `DIRECT_MSG` handler + 3 new handlers (`DM_CONVERSATIONS_REQ`, `DM_HISTORY_REQ`, `DM_MARK_READ_REQ`); self-DM guard
- `electron-client/native/src/commands/dm.rs` (new) — 3 napi commands
- `electron-client/native/src/net/central.rs` — 2 new `route_packets` arms
- `electron-client/native/src/events.rs` — 2 new event names + 2 payload structs
- `electron-client/src/stores/dmStore.ts` — extend conversation shape, new actions
- `electron-client/src/features/dm/useDmEvents.ts` (new) — listener hook
- `electron-client/src/features/auth/useAuthEvents.ts` — request conversations on login
- `electron-client/src/features/dm/DmChatPanel.tsx` — history fetch on mount, paginate on scroll, debounced mark-read
- `electron-client/src/layouts/DmSidebar.tsx` — unread-count pill
- `electron-client/src/App.tsx` (or wherever hooks are mounted) — install `useDmEvents`

Rough effort: ~500 LOC new, ~150 LOC modified.
