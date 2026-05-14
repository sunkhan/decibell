# Persistent DMs — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Persist DMs on central, hydrate conversation previews + history + unread counts on login, drop the "user is offline" failure path so DMs are always delivered eventually.

**Architecture:** Two new Postgres tables on central (`dm_messages`, `dm_read_state`). The existing `DIRECT_MSG` handler is reordered to insert into the DB before any delivery decision and to stamp the new persisted `id` back onto the routed packet. Three new request/response packet pairs let the client pull conversation previews on login, page through per-conversation history on demand, and fire-and-forget mark-as-read. Three napi commands bridge them to the renderer's existing dmStore.

**Tech Stack:** Protobuf 3 (proto3), C++ with pqxx (PostgreSQL) + OpenSSL on the central server, Rust napi-rs in the native addon, React + Zustand in the renderer.

**Spec:** `docs/superpowers/specs/2026-05-14-persistent-dms-design.md`

---

## File map

**Modify:**
- `proto/messages.proto` — 5 new packet types, 5 new oneof entries, 4 new messages, +1 field on `DirectMessage`
- `src/server/auth_manager.hpp` — declarations for `insertDm`, `fetchDmHistory`, `fetchDmConversations`, `markDmRead` + supporting POD structs
- `src/server/auth_manager.cpp` — DDL in `initializeDatabase`; method bodies
- `src/server/main.cpp` — modified `DIRECT_MSG` handler (persist + stamp id + self-DM guard + drop offline error) + 3 new handlers
- `electron-client/native/src/events.rs` — 2 new event names + 2 payload structs
- `electron-client/native/src/net/central.rs` — 2 new `route_packets` arms
- `electron-client/native/src/lib.rs` (or wherever modules are declared) — register new `dm` command module
- `electron-client/src/stores/dmStore.ts` — extend `DmConversation`, add hydrate/append/markRead actions
- `electron-client/src/features/auth/useAuthEvents.ts` — request conversations on `login_succeeded`
- `electron-client/src/features/dm/DmChatPanel.tsx` — history fetch on mount, paginate on scroll-up, debounced mark-read
- `electron-client/src/layouts/DmSidebar.tsx` — unread-count pill on each conversation
- `electron-client/src/App.tsx` — install the new `useDmEvents` hook alongside the existing event hooks

**Create:**
- `electron-client/native/src/commands/dm.rs` — 3 napi commands (`request_dm_conversations`, `request_dm_history`, `mark_dm_read`)
- `electron-client/src/features/dm/useDmEvents.ts` — listener hook for `dm_conversations_received` + `dm_history_received`

---

## Task 1: Protobuf additions

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Add the new packet-type enum values**

In `proto/messages.proto`, inside the `Packet.Type` enum, after `FETCH_STREAM_THUMBNAIL_RES = 67;` (added by the live-stream-indicators feature), append:

```proto
    // Persistent DMs — see docs/superpowers/specs/
    // 2026-05-14-persistent-dms-design.md
    DM_CONVERSATIONS_REQ = 68;   // client→central; empty payload
    DM_CONVERSATIONS_RES = 69;   // central→client
    DM_HISTORY_REQ       = 70;   // client→central
    DM_HISTORY_RES       = 71;   // central→client
    DM_MARK_READ_REQ     = 72;   // client→central; fire-and-forget
```

- [ ] **Step 2: Add the oneof payload entries**

Inside `Packet.oneof payload`, after `FetchStreamThumbnailRes fetch_stream_thumbnail_res = 69;`, append:

```proto
    // --- Persistent DMs ---
    DmConversationsReq dm_conversations_req = 70;
    DmConversationsRes dm_conversations_res = 71;
    DmHistoryReq       dm_history_req       = 72;
    DmHistoryRes       dm_history_res       = 73;
    DmMarkReadReq      dm_mark_read_req     = 74;
```

- [ ] **Step 3: Add the `id` field to `DirectMessage`**

Find the existing `DirectMessage` definition (around line 241). Replace it with:

```proto
message DirectMessage {
  string sender = 1;
  string recipient = 2;
  string content = 3;
  int64 timestamp = 4;
  // Server-assigned persistence id, stamped by central after insertDm.
  // Old clients ignore the unknown field; new clients use it as the
  // `up_to_id` cursor in DmMarkReadReq.
  int64 id = 5;
}
```

- [ ] **Step 4: Add the four new message definitions**

Append at the end of `proto/messages.proto`:

```proto
// --- Persistent DMs ---
// (see docs/superpowers/specs/2026-05-14-persistent-dms-design.md)

message DmConversationsReq {}

message DmConversationPreview {
  string peer = 1;
  string last_message_content = 2;
  string last_message_sender = 3;  // either the local user or the peer
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
  repeated DmHistoryMessage messages = 2;  // newest first
  bool   has_more = 3;
}

message DmMarkReadReq {
  string peer = 1;
  int64  up_to_id = 2;
}
```

- [ ] **Step 5: Regenerate the Rust prost bindings**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check
```

Expected: `Finished dev profile` (with the usual 77 warnings from existing code).

- [ ] **Step 6: Commit**

```bash
git add proto/messages.proto
git commit -m "proto: persistent DM messages + DirectMessage.id field"
```

---

## Task 2: AuthManager DM methods

**Files:**
- Modify: `src/server/auth_manager.hpp`
- Modify: `src/server/auth_manager.cpp`

- [ ] **Step 1: Declare the DM methods + POD structs in the header**

Open `src/server/auth_manager.hpp`. After the avatar declarations (added in 0.6.3, look for `setAvatar` / `getAvatar`), add:

```cpp
    // --- Persistent DMs ---
    // (see docs/superpowers/specs/2026-05-14-persistent-dms-design.md)
    struct DmHistoryRow {
        int64_t id;
        std::string sender;
        std::string content;
        int64_t timestamp;
    };
    struct DmConversationPreviewRow {
        std::string peer;
        std::string last_message_content;
        std::string last_message_sender;
        int64_t last_message_id;
        int64_t last_timestamp;
        int64_t unread_count;
    };

    /// Insert a new DM, return its autoincrement id. Returns 0 on
    /// DB failure (caller surfaces a "could not deliver" error to
    /// the sender).
    int64_t insertDm(const std::string& sender,
                     const std::string& recipient,
                     const std::string& content,
                     int64_t sent_at);

    /// Fetch a page of messages between user_a and user_b, ordered
    /// newest first. before_id = 0 means "latest". limit is clamped
    /// to [1, 200] by the caller. Sets has_more to true if more
    /// messages exist older than the page.
    std::vector<DmHistoryRow> fetchDmHistory(const std::string& user_a,
                                              const std::string& user_b,
                                              int64_t before_id,
                                              int32_t limit,
                                              bool& has_more);

    /// One row per conversation the user is part of, with the most
    /// recent message preview + unread count (messages from peer
    /// with id > dm_read_state.last_read_id).
    std::vector<DmConversationPreviewRow> fetchDmConversations(
        const std::string& user);

    /// Upsert dm_read_state, setting last_read_id =
    /// GREATEST(existing, up_to_id). Idempotent and race-safe.
    void markDmRead(const std::string& reader,
                    const std::string& peer,
                    int64_t up_to_id);
```

- [ ] **Step 2: Add DDL to `initializeDatabase`**

Open `src/server/auth_manager.cpp`. Find `AuthManager::initializeDatabase()` (we extended it for avatars in 0.6.3 with idempotent `ADD COLUMN IF NOT EXISTS`). Inside the same try block, after the existing `CREATE TABLE` calls, before `txn.commit()`, add:

```cpp
        // --- Persistent DMs (see docs/superpowers/specs/
        //     2026-05-14-persistent-dms-design.md §1) ---
        txn.exec(
            "CREATE TABLE IF NOT EXISTS dm_messages ("
            "  id BIGSERIAL PRIMARY KEY,"
            "  sender VARCHAR(32) NOT NULL,"
            "  recipient VARCHAR(32) NOT NULL,"
            "  content TEXT NOT NULL,"
            "  sent_at BIGINT NOT NULL"
            ")"
        );
        // Two-direction lookup ("messages between A and B" hits the
        // same B-tree regardless of who sent which). The LEAST /
        // GREATEST normalisation is what makes a single index serve
        // both query directions.
        txn.exec(
            "CREATE INDEX IF NOT EXISTS dm_messages_pair_idx "
            "ON dm_messages "
            "(LEAST(sender, recipient), GREATEST(sender, recipient), id DESC)"
        );
        // Per-recipient unread queries — `WHERE recipient = me AND id > last_read_id`.
        txn.exec(
            "CREATE INDEX IF NOT EXISTS dm_messages_recipient_idx "
            "ON dm_messages (recipient, id DESC)"
        );
        txn.exec(
            "CREATE TABLE IF NOT EXISTS dm_read_state ("
            "  reader VARCHAR(32) NOT NULL,"
            "  peer VARCHAR(32) NOT NULL,"
            "  last_read_id BIGINT NOT NULL DEFAULT 0,"
            "  PRIMARY KEY (reader, peer)"
            ")"
        );
```

- [ ] **Step 3: Implement `insertDm`**

In `src/server/auth_manager.cpp`, at the end of the file (or near the avatar methods), add:

```cpp
int64_t AuthManager::insertDm(const std::string& sender,
                               const std::string& recipient,
                               const std::string& content,
                               int64_t sent_at) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "INSERT INTO dm_messages (sender, recipient, content, sent_at) "
            "VALUES ($1, $2, $3, $4) RETURNING id",
            sender, recipient, content, sent_at);
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int64_t>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] insertDm: " << e.what() << "\n";
        return 0;
    }
}
```

- [ ] **Step 4: Implement `fetchDmHistory`**

Append to `src/server/auth_manager.cpp`:

```cpp
std::vector<AuthManager::DmHistoryRow> AuthManager::fetchDmHistory(
    const std::string& user_a, const std::string& user_b,
    int64_t before_id, int32_t limit, bool& has_more) {
    has_more = false;
    std::vector<DmHistoryRow> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Pull one extra row so we can detect has_more cheaply.
        const int32_t fetch_n = std::max(1, std::min(limit, 200)) + 1;
        // before_id=0 means "latest". For real cursoring we filter on
        // id < before_id. Either way the index covers the predicate.
        const char* sql =
            "SELECT id, sender, content, sent_at FROM dm_messages "
            "WHERE LEAST(sender, recipient) = LEAST($1, $2) "
            "  AND GREATEST(sender, recipient) = GREATEST($1, $2) "
            "  AND ($3 = 0 OR id < $3) "
            "ORDER BY id DESC LIMIT $4";
        pqxx::result rs = txn.exec_params(sql, user_a, user_b, before_id, fetch_n);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            DmHistoryRow r{
                row[0].as<int64_t>(),
                row[1].as<std::string>(),
                row[2].as<std::string>(),
                row[3].as<int64_t>(),
            };
            out.push_back(std::move(r));
        }
        if (static_cast<int32_t>(out.size()) > std::max(1, std::min(limit, 200))) {
            out.pop_back();
            has_more = true;
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmHistory: " << e.what() << "\n";
        return {};
    }
}
```

- [ ] **Step 5: Implement `fetchDmConversations`**

Append to `src/server/auth_manager.cpp`:

```cpp
std::vector<AuthManager::DmConversationPreviewRow>
AuthManager::fetchDmConversations(const std::string& user) {
    std::vector<DmConversationPreviewRow> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Picks the latest message id per conversation (pair grouped
        // by LEAST/GREATEST), then joins back to dm_messages for the
        // preview content, and to dm_read_state to derive unread
        // count for messages from peer with id > last_read_id.
        //
        // The unread COUNT is correlated by the conversation pair —
        // we only count messages WHERE the peer is the sender
        // (i.e. messages the local user received, not their own
        // outgoing messages).
        const char* sql =
            "WITH latest AS ( "
            "  SELECT LEAST(sender, recipient) AS a, "
            "         GREATEST(sender, recipient) AS b, "
            "         MAX(id) AS max_id "
            "  FROM dm_messages "
            "  WHERE sender = $1 OR recipient = $1 "
            "  GROUP BY 1, 2 "
            ") "
            "SELECT "
            "  CASE WHEN m.sender = $1 THEN m.recipient ELSE m.sender END AS peer, "
            "  m.content, m.sender, m.id, m.sent_at, "
            "  COALESCE(( "
            "    SELECT COUNT(*) FROM dm_messages d "
            "    WHERE d.recipient = $1 "
            "      AND d.sender = CASE WHEN m.sender = $1 THEN m.recipient ELSE m.sender END "
            "      AND d.id > COALESCE(( "
            "        SELECT last_read_id FROM dm_read_state rs "
            "        WHERE rs.reader = $1 AND rs.peer = "
            "          CASE WHEN m.sender = $1 THEN m.recipient ELSE m.sender END "
            "      ), 0) "
            "  ), 0) AS unread "
            "FROM latest l "
            "JOIN dm_messages m ON m.id = l.max_id "
            "ORDER BY m.id DESC";
        pqxx::result rs = txn.exec_params(sql, user);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            DmConversationPreviewRow p{
                row[0].as<std::string>(),       // peer
                row[1].as<std::string>(),       // last_message_content
                row[2].as<std::string>(),       // last_message_sender
                row[3].as<int64_t>(),           // last_message_id
                row[4].as<int64_t>(),           // last_timestamp
                row[5].as<int64_t>(),           // unread_count
            };
            out.push_back(std::move(p));
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] fetchDmConversations: " << e.what() << "\n";
        return {};
    }
}
```

- [ ] **Step 6: Implement `markDmRead`**

Append to `src/server/auth_manager.cpp`:

```cpp
void AuthManager::markDmRead(const std::string& reader,
                              const std::string& peer,
                              int64_t up_to_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Upsert with GREATEST so out-of-order or duplicate mark-read
        // calls never regress the read cursor.
        txn.exec_params(
            "INSERT INTO dm_read_state (reader, peer, last_read_id) "
            "VALUES ($1, $2, $3) "
            "ON CONFLICT (reader, peer) DO UPDATE "
            "SET last_read_id = GREATEST(dm_read_state.last_read_id, EXCLUDED.last_read_id)",
            reader, peer, up_to_id);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] markDmRead: " << e.what() << "\n";
    }
}
```

- [ ] **Step 7: Commit**

```bash
git add src/server/auth_manager.hpp src/server/auth_manager.cpp
git commit -m "feat(server,dms): AuthManager methods + DDL for dm_messages/dm_read_state"
```

---

## Task 3: Modified DIRECT_MSG handler

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Reorder + extend the handler**

In `src/server/main.cpp`, replace the existing `DIRECT_MSG` handler (the block starting around line 202, `else if (packet.type() == chatproj::Packet::DIRECT_MSG)`) with:

```cpp
        // --- DIRECT MESSAGE ---
        // Persistence-first flow: identity stamp → friends-only check
        // → insert into dm_messages → stamp the persisted id back on
        // the routed packet → live-deliver to recipient if online →
        // always echo to sender. The previous "user is offline" error
        // packet is gone — DMs are always persisted, so the recipient
        // will see them on their next login.
        else if (packet.type() == chatproj::Packet::DIRECT_MSG) {
            if (!authenticated_) return;

            auto now = std::chrono::system_clock::now();
            int64_t current_time = std::chrono::system_clock::to_time_t(now);

            chatproj::Packet routed_packet = packet;
            auto* dmsg = routed_packet.mutable_direct_msg();
            dmsg->set_sender(username_); // Enforce sender identity
            dmsg->set_timestamp(current_time);

            // Self-DM guard. The DB schema allows self-rows, but the
            // UX doesn't make sense; reject explicitly so persistence
            // doesn't silently accumulate them.
            if (dmsg->recipient() == username_) {
                return;
            }

            if (!manager_.check_dm_allowed(username_, dmsg->recipient(), auth_manager_)) {
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("This user only accepts direct messages from users in their friends list.");
                err_msg->set_timestamp(current_time);

                std::string serialized;
                error_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
                return;
            }

            // Persist before delivery. On DB failure, surface to
            // sender as a generic "couldn't deliver" — the message
            // is genuinely lost in that branch (rare).
            int64_t new_id = auth_manager_.insertDm(
                username_, dmsg->recipient(), dmsg->content(), current_time);
            if (new_id == 0) {
                chatproj::Packet error_packet;
                error_packet.set_type(chatproj::Packet::DIRECT_MSG);
                auto* err_msg = error_packet.mutable_direct_msg();
                err_msg->set_sender(username_);
                err_msg->set_recipient(dmsg->recipient());
                err_msg->set_content("The server couldn't deliver your message. Please try again.");
                err_msg->set_timestamp(current_time);

                std::string serialized;
                error_packet.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
                deliver(framed);
                return;
            }

            // Stamp the persisted id onto the routed packet so the
            // client can use it as `up_to_id` in DmMarkReadReq.
            dmsg->set_id(new_id);

            // Best-effort live delivery — return value is informational
            // only. The recipient gets it now if online, on next
            // login via DM_CONVERSATIONS_REQ / DM_HISTORY_REQ otherwise.
            manager_.send_private(routed_packet, dmsg->recipient());

            // Always echo to sender so their UI shows the DM as
            // delivered, carrying the new id field.
            std::string serialized;
            routed_packet.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
            deliver(framed);
        }
```

- [ ] **Step 2: Commit**

```bash
git add src/server/main.cpp
git commit -m "feat(server,dms): persist DIRECT_MSG before delivery; drop offline-error path"
```

---

## Task 4: Three new request handlers

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Add the conversations handler**

In `src/server/main.cpp`, after the modified `DIRECT_MSG` block from Task 3, add:

```cpp
        // --- DM CONVERSATIONS REQ ---
        // One-shot pull of all conversation previews + unread counts
        // for the local user. Fired on login from the renderer to
        // populate the DmSidebar cards.
        else if (packet.type() == chatproj::Packet::DM_CONVERSATIONS_REQ) {
            if (!authenticated_) return;

            auto convs = auth_manager_.fetchDmConversations(username_);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_CONVERSATIONS_RES);
            auto* res = response.mutable_dm_conversations_res();
            for (const auto& c : convs) {
                auto* preview = res->add_conversations();
                preview->set_peer(c.peer);
                preview->set_last_message_content(c.last_message_content);
                preview->set_last_message_sender(c.last_message_sender);
                preview->set_last_message_id(c.last_message_id);
                preview->set_last_timestamp(c.last_timestamp);
                preview->set_unread_count(c.unread_count);
            }

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }
```

- [ ] **Step 2: Add the history handler**

After the conversations handler, add:

```cpp
        // --- DM HISTORY REQ ---
        // Paginated fetch of messages between the local user and
        // `peer`. before_id=0 returns the latest page; client
        // paginates upward by passing the oldest seen id.
        else if (packet.type() == chatproj::Packet::DM_HISTORY_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_history_req();
            const std::string& peer = req.peer();
            if (peer.empty()) return;

            int32_t limit = req.limit();
            if (limit <= 0) limit = 50;
            if (limit > 200) limit = 200;

            bool has_more = false;
            auto rows = auth_manager_.fetchDmHistory(
                username_, peer, req.before_id(), limit, has_more);

            chatproj::Packet response;
            response.set_type(chatproj::Packet::DM_HISTORY_RES);
            auto* res = response.mutable_dm_history_res();
            res->set_peer(peer);
            res->set_has_more(has_more);
            for (const auto& r : rows) {
                auto* msg = res->add_messages();
                msg->set_id(r.id);
                msg->set_sender(r.sender);
                msg->set_content(r.content);
                msg->set_timestamp(r.timestamp);
            }

            std::string s;
            response.SerializeToString(&s);
            deliver(std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(s)));
        }
```

- [ ] **Step 3: Add the mark-read handler**

After the history handler, add:

```cpp
        // --- DM MARK READ REQ ---
        // Fire-and-forget: update dm_read_state.last_read_id so the
        // next DM_CONVERSATIONS_REQ surfaces the correct unread
        // count. No response — TCP delivery is the implicit ack.
        else if (packet.type() == chatproj::Packet::DM_MARK_READ_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_mark_read_req();
            if (req.peer().empty()) return;
            auth_manager_.markDmRead(username_, req.peer(), req.up_to_id());
        }
```

- [ ] **Step 4: Commit**

```bash
git add src/server/main.cpp
git commit -m "feat(server,dms): DM_CONVERSATIONS_REQ + DM_HISTORY_REQ + DM_MARK_READ_REQ handlers"
```

---

## Task 5: Native — events + payload structs

**Files:**
- Modify: `electron-client/native/src/events.rs`

- [ ] **Step 1: Add event-name constants**

In `electron-client/native/src/events.rs`, near the other event-name constants (look for `pub const STREAM_PRESENCE_UPDATED`), add:

```rust
pub const DM_CONVERSATIONS_RECEIVED: &str = "dm_conversations_received";
pub const DM_HISTORY_RECEIVED: &str = "dm_history_received";
```

- [ ] **Step 2: Add the payload structs**

Near the other payload struct definitions in `events.rs` (the file has a `StreamPresenceUpdatedPayload` etc.), add:

```rust
#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmConversationPreviewPayload {
    pub peer: String,
    pub last_message_content: String,
    pub last_message_sender: String,
    pub last_message_id: i64,
    pub last_timestamp: i64,
    pub unread_count: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmConversationsReceivedPayload {
    pub conversations: Vec<DmConversationPreviewPayload>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmHistoryMessagePayload {
    pub id: i64,
    pub sender: String,
    pub content: String,
    pub timestamp: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmHistoryReceivedPayload {
    pub peer: String,
    pub messages: Vec<DmHistoryMessagePayload>,
    pub has_more: bool,
}
```

- [ ] **Step 3: Add the emit helpers**

Near the other `pub fn emit_*` functions in `events.rs`, add:

```rust
pub fn emit_dm_conversations_received(payload: DmConversationsReceivedPayload) {
    send(EventEnvelope {
        name: DM_CONVERSATIONS_RECEIVED.to_string(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    });
}

pub fn emit_dm_history_received(payload: DmHistoryReceivedPayload) {
    send(EventEnvelope {
        name: DM_HISTORY_RECEIVED.to_string(),
        payload: serde_json::to_value(payload).unwrap_or(serde_json::Value::Null),
    });
}
```

(Use the same envelope/send pattern as the other emit helpers in this file — match by reading `emit_stream_presence_updated` and mirror its shape; the snippet above assumes `send(EventEnvelope { ... })`, but if the existing helpers use a different invocation, use whatever they do.)

- [ ] **Step 4: Verify it compiles**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check
```

Expected: `Finished dev profile`.

- [ ] **Step 5: Commit**

```bash
git add electron-client/native/src/events.rs
git commit -m "feat(native,dms): event names + payload structs for conversations/history"
```

---

## Task 6: Native — route_packets arms for the two response packets

**Files:**
- Modify: `electron-client/native/src/net/central.rs`

- [ ] **Step 1: Find the central client's route_packets match**

The central client (not community — central is where DMs live) has a `route_packets` function with a `match packet.payload` block. Find the existing arm for `FetchAvatarRes` (added in 0.6.3 — search the file for `FetchAvatarRes` or `fetch_avatar_res`).

- [ ] **Step 2: Add the conversations arm**

After the existing avatar arms, add:

```rust
                Some(packet::Payload::DmConversationsRes(res)) => {
                    let conversations = res
                        .conversations
                        .into_iter()
                        .map(|c| events::DmConversationPreviewPayload {
                            peer: c.peer,
                            last_message_content: c.last_message_content,
                            last_message_sender: c.last_message_sender,
                            last_message_id: c.last_message_id,
                            last_timestamp: c.last_timestamp,
                            unread_count: c.unread_count,
                        })
                        .collect();
                    events::emit_dm_conversations_received(
                        events::DmConversationsReceivedPayload { conversations },
                    );
                }
```

- [ ] **Step 3: Add the history arm**

Next to the conversations arm, add:

```rust
                Some(packet::Payload::DmHistoryRes(res)) => {
                    let messages = res
                        .messages
                        .into_iter()
                        .map(|m| events::DmHistoryMessagePayload {
                            id: m.id,
                            sender: m.sender,
                            content: m.content,
                            timestamp: m.timestamp,
                        })
                        .collect();
                    events::emit_dm_history_received(events::DmHistoryReceivedPayload {
                        peer: res.peer,
                        messages,
                        has_more: res.has_more,
                    });
                }
```

- [ ] **Step 4: Verify it compiles**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check
```

Expected: `Finished dev profile`.

- [ ] **Step 5: Commit**

```bash
git add electron-client/native/src/net/central.rs
git commit -m "feat(native,dms): route DmConversationsRes + DmHistoryRes to events"
```

---

## Task 7: Native — three napi commands

**Files:**
- Create: `electron-client/native/src/commands/dm.rs`
- Modify: `electron-client/native/src/commands/mod.rs` (or wherever command modules are declared)

- [ ] **Step 1: Locate the command module declaration**

Open `electron-client/native/src/commands/mod.rs`. There's a list of `pub mod ...` declarations (auth, streaming, voice, etc.). Note this file's exact contents — we'll add a `pub mod dm;` line.

- [ ] **Step 2: Register the new module**

In `electron-client/native/src/commands/mod.rs`, append:

```rust
pub mod dm;
```

- [ ] **Step 3: Create the dm command file**

Create `electron-client/native/src/commands/dm.rs`:

```rust
//! Persistent DM commands. Three fire-and-emit napi calls:
//!
//!   - `request_dm_conversations()`: pulls conversation previews from
//!     central; response arrives as `dm_conversations_received`.
//!   - `request_dm_history(peer, before_id, limit)`: pulls one page
//!     of messages; response arrives as `dm_history_received`.
//!   - `mark_dm_read(peer, up_to_id)`: fire-and-forget read cursor
//!     update; no response.
//!
//! All three send through the central server's TCP/TLS connection.
//! Pattern matches the existing `request_server_list` napi command
//! (also fire-and-emit on the central client).

use crate::net::connection::build_packet;
use crate::net::proto::{packet, DmConversationsReq, DmHistoryReq, DmMarkReadReq};
use crate::state;

#[napi]
pub async fn request_dm_conversations() -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmConversationsReq,
            packet::Payload::DmConversationsReq(DmConversationsReq {}),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct RequestDmHistoryArgs {
    pub peer: String,
    pub before_id: i64,
    pub limit: i32,
}

#[napi]
pub async fn request_dm_history(args: RequestDmHistoryArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmHistoryReq,
            packet::Payload::DmHistoryReq(DmHistoryReq {
                peer: args.peer,
                before_id: args.before_id,
                limit: args.limit,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data))
        .await
    {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}

#[napi(object)]
pub struct MarkDmReadArgs {
    pub peer: String,
    pub up_to_id: i64,
}

#[napi]
pub async fn mark_dm_read(args: MarkDmReadArgs) -> napi::Result<()> {
    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        let token = s.token.clone();
        let pkt = build_packet(
            packet::Type::DmMarkReadReq,
            packet::Payload::DmMarkReadReq(DmMarkReadReq {
                peer: args.peer,
                up_to_id: args.up_to_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await;
    Ok(())
}
```

(If `central.connection_write_tx()` or `s.token` field/method names differ in the actual code, use the names from `auth.rs::register` — that command is the closest matching shape and was added in the same release.)

- [ ] **Step 4: Rebuild the native addon**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; npm run build
```

Expected: `Finished release profile target(s)` and `[copy-dlls] copied 5 FFmpeg DLLs`. The build will regenerate `index.d.ts` and `index.js` with the new exports `requestDmConversations`, `requestDmHistory`, `markDmRead`.

- [ ] **Step 5: Verify renderer can see the new exports**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git add electron-client/native/src/commands/mod.rs electron-client/native/src/commands/dm.rs electron-client/native/index.d.ts electron-client/native/index.js
git commit -m "feat(native,dms): request_dm_conversations + request_dm_history + mark_dm_read"
```

---

## Task 8: Renderer dmStore extension

**Files:**
- Modify: `electron-client/src/stores/dmStore.ts`

- [ ] **Step 1: Extend the conversation shape**

Open `electron-client/src/stores/dmStore.ts`. Find the conversation type (or interface) — it currently has fields like `username`, `messages`, `lastMessageTime`. Add four new fields and update the type:

```ts
interface DmConversation {
  username: string;
  messages: DmMessage[];
  lastMessageTime: number;
  /// Unread DM count for this peer (messages they sent that the
  /// local user hasn't acked yet). Set from the server's preview on
  /// hydrate; bumped on incoming DIRECT_MSG when the user isn't
  /// actively viewing this conversation; cleared optimistically
  /// when the panel mounts (and again when the next preview
  /// arrives).
  unreadCount: number;
  /// Highest message id the user has marked-read up through.
  /// Drives the `up_to_id` argument of mark_dm_read.
  lastReadId: number;
  /// Server says there are older messages available before the
  /// oldest currently-loaded one. Drives the scroll-up paginator.
  hasMoreHistory: boolean;
  /// Set to true once we've received a DmHistoryRes for this peer.
  /// `false` means "messages[] is purely from live events; no
  /// server hydration yet". Drives the on-mount fetch decision.
  historyLoaded: boolean;
}
```

(If the existing interface has different field names — e.g. `peerUsername` instead of `username` — match the existing names. The four new fields are additive.)

- [ ] **Step 2: Update the message shape to include id**

Find the existing `DmMessage` interface. Add an optional `id` field — optional because in-memory messages from before this feature don't have ids:

```ts
interface DmMessage {
  sender: string;
  content: string;
  timestamp: number;
  /// Server-assigned id from DirectMessage.id. Present on persisted
  /// messages (everything that comes via DIRECT_MSG after this
  /// feature ships, and everything in DmHistoryRes). 0 / undefined
  /// for legacy in-memory messages.
  id?: number;
}
```

- [ ] **Step 3: Initialise new fields in any conversation-creation paths**

The store creates new conversation entries lazily when the first DM arrives. Find every place that initialises a conversation (typically a `setConversation` action or similar — search for `lastMessageTime: 0` or where conversations enter the map). Add the new defaults:

```ts
unreadCount: 0,
lastReadId: 0,
hasMoreHistory: false,
historyLoaded: false,
```

- [ ] **Step 4: Add the three new actions**

In the store's `create<DmState>(...)` factory, add three new actions:

```ts
  /// Replace conversation previews with the server-truth list from
  /// DmConversationsRes. Per peer: keep the existing `messages` array
  /// (live DMs from this session may already be there), reset
  /// unreadCount + lastReadId from the server, set hasMoreHistory
  /// based on whether there are messages older than `last_message_id`,
  /// and leave historyLoaded alone (still false until the user opens
  /// the conversation and request_dm_history responds).
  hydrateConversations: (
    previews: {
      peer: string;
      lastMessageContent: string;
      lastMessageSender: string;
      lastMessageId: number;
      lastTimestamp: number;
      unreadCount: number;
    }[],
  ) =>
    set((state) => {
      const next = { ...state.conversations };
      for (const p of previews) {
        const existing = next[p.peer];
        next[p.peer] = {
          username: p.peer,
          messages: existing?.messages ?? [],
          lastMessageTime: p.lastTimestamp,
          unreadCount: p.unreadCount,
          lastReadId: existing?.lastReadId ?? 0,
          hasMoreHistory: true,  // server may have more older than what we have; ask on open
          historyLoaded: existing?.historyLoaded ?? false,
        };
      }
      return { conversations: next };
    }),

  /// Merge a DmHistoryRes page into a peer's conversation, ordered
  /// oldest→newest in memory. Dedupes by id against existing
  /// messages (live DMs may already have been delivered while
  /// history was being fetched). Sets hasMoreHistory from the
  /// server's flag and flips historyLoaded to true.
  appendHistory: (
    peer: string,
    messages: { id: number; sender: string; content: string; timestamp: number }[],
    hasMore: boolean,
  ) =>
    set((state) => {
      const conv = state.conversations[peer];
      const existing = conv?.messages ?? [];
      const existingIds = new Set(existing.map((m) => m.id).filter((id): id is number => typeof id === "number"));
      // History page is newest-first; flip to oldest-first to match
      // in-memory ordering, then dedupe.
      const incoming = [...messages].reverse().filter((m) => !existingIds.has(m.id));
      const merged = [...incoming, ...existing];
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            username: peer,
            messages: merged,
            lastMessageTime: conv?.lastMessageTime ?? (merged.at(-1)?.timestamp ?? 0),
            unreadCount: conv?.unreadCount ?? 0,
            lastReadId: conv?.lastReadId ?? 0,
            hasMoreHistory: hasMore,
            historyLoaded: true,
          },
        },
      };
    }),

  /// Optimistically zero the unread count for a peer and bump
  /// lastReadId. Called from DmChatPanel when the conversation
  /// becomes visible. Server-side mark_dm_read is fire-and-forget;
  /// the next hydrateConversations will re-confirm the count.
  markRead: (peer: string, upToId: number) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      if (upToId <= conv.lastReadId) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            unreadCount: 0,
            lastReadId: upToId,
          },
        },
      };
    }),
```

- [ ] **Step 5: Update the live-DM append path to bump unread + store id**

Find the action that appends a DM to a conversation when a `direct_message_received` event arrives (likely `addMessage` or similar; the `messages.push` site in the store). Extend it so that:

1. The new `id` field on the incoming DM is stored on the `DmMessage` entry.
2. If the message is FROM the peer (i.e., sender !== local user) AND the user isn't currently viewing this conversation (compare `state.activeDmUser` to the peer), increment `unreadCount` by 1.

Concrete code shape — replace whatever the existing append-message reducer looks like with this template:

```ts
appendMessage: (peer: string, m: DmMessage, isFromSelf: boolean) =>
  set((state) => {
    const conv = state.conversations[peer];
    const messages = [...(conv?.messages ?? []), m];
    const isViewing = state.activeDmUser === peer;
    const newUnread = (() => {
      // Self-sent: don't increment my own unread.
      if (isFromSelf) return conv?.unreadCount ?? 0;
      // Currently viewing: stay at 0 (mark-read will fire from panel).
      if (isViewing) return 0;
      return (conv?.unreadCount ?? 0) + 1;
    })();
    return {
      conversations: {
        ...state.conversations,
        [peer]: {
          username: peer,
          messages,
          lastMessageTime: m.timestamp,
          unreadCount: newUnread,
          lastReadId: conv?.lastReadId ?? 0,
          hasMoreHistory: conv?.hasMoreHistory ?? false,
          historyLoaded: conv?.historyLoaded ?? false,
        },
      },
    };
  }),
```

If the store currently exposes `activeDmUser` from a separate store (e.g. `useDmStore.activeDmUser` already exists for DmChatPanel routing), reuse that. The exact integration depends on the existing shape; the key invariant is "don't bump unread for the conversation you're actively reading."

- [ ] **Step 6: Verify typecheck**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 7: Commit**

```bash
git add electron-client/src/stores/dmStore.ts
git commit -m "feat(dms): dmStore — unreadCount + history + hydrate/append/markRead actions"
```

---

## Task 9: useDmEvents listener hook + login wire-up + App.tsx install

**Files:**
- Create: `electron-client/src/features/dm/useDmEvents.ts`
- Modify: `electron-client/src/features/auth/useAuthEvents.ts`
- Modify: `electron-client/src/App.tsx`

- [ ] **Step 1: Create the listener hook**

Create `electron-client/src/features/dm/useDmEvents.ts`:

```ts
// Listener hook for the two persistent-DM server-push events:
// `dm_conversations_received` (server-truth previews + unread counts)
// and `dm_history_received` (one page of messages between local user
// and peer). Mounted once in App.tsx alongside the other event hooks.

import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useDmStore } from "../../stores/dmStore";

interface ConversationPreviewPayload {
  peer: string;
  lastMessageContent: string;
  lastMessageSender: string;
  lastMessageId: number;
  lastTimestamp: number;
  unreadCount: number;
}

interface DmHistoryMessagePayload {
  id: number;
  sender: string;
  content: string;
  timestamp: number;
}

export function useDmEvents() {
  useEffect(() => {
    const unlistenConv = listen<{ conversations: ConversationPreviewPayload[] }>(
      "dm_conversations_received",
      (event) => {
        useDmStore.getState().hydrateConversations(event.payload.conversations);
      },
    );

    const unlistenHist = listen<{
      peer: string;
      messages: DmHistoryMessagePayload[];
      hasMore: boolean;
    }>("dm_history_received", (event) => {
      const { peer, messages, hasMore } = event.payload;
      useDmStore.getState().appendHistory(peer, messages, hasMore);
    });

    return () => {
      unlistenConv.then((fn) => fn());
      unlistenHist.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 2: Wire login → request_dm_conversations**

Open `electron-client/src/features/auth/useAuthEvents.ts`. Find the `login_succeeded` handler — it currently calls `invoke("request_server_list").catch(() => {});`. Right after that, add:

```ts
        invoke("request_dm_conversations").catch(() => {});
```

- [ ] **Step 3: Install the hook in App.tsx**

Open `electron-client/src/App.tsx`. Find where existing event hooks are installed (look for `useAuthEvents()`, `useFriendsEvents()`, etc.). Add the import at the top:

```ts
import { useDmEvents } from "./features/dm/useDmEvents";
```

Then add a call inside the same component:

```ts
  useDmEvents();
```

(Place it next to the other `use*Events()` calls so it's obvious it's part of the same event-hook block.)

- [ ] **Step 4: Verify typecheck**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add electron-client/src/features/dm/useDmEvents.ts electron-client/src/features/auth/useAuthEvents.ts electron-client/src/App.tsx
git commit -m "feat(dms): useDmEvents hook + request conversations on login"
```

---

## Task 10: DmChatPanel — history fetch on mount, paginate, debounced mark-read

**Files:**
- Modify: `electron-client/src/features/dm/DmChatPanel.tsx`

- [ ] **Step 1: Add the on-mount history fetch**

Near the top of the `DmChatPanel` component, after the existing store reads, add a `useEffect` that runs whenever `activeDmUser` changes:

```tsx
  // On switching to a peer, pull the latest page of history IF we
  // haven't already loaded server history for this conversation in
  // this session. Live in-memory messages aren't enough to know we've
  // "seen" the full history; the server's view is authoritative.
  useEffect(() => {
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (conv?.historyLoaded) return;
    invoke("request_dm_history", {
      peer: activeDmUser,
      beforeId: 0,
      limit: 50,
    }).catch(console.error);
  }, [activeDmUser]);
```

(Adapt the imports — make sure `invoke` from `../../lib/ipc` and `useDmStore` are imported.)

- [ ] **Step 2: Add the scroll-up paginator**

Find the existing message-list container (the scrollable div that holds the bubble list). Attach an `onScroll` handler (or augment the existing one) that checks for scroll-near-top and triggers a paginate when `hasMoreHistory` is true:

```tsx
  const onScrollLoadMore = (e: React.UIEvent<HTMLDivElement>) => {
    const el = e.currentTarget;
    if (el.scrollTop > 80) return;  // not near top yet
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (!conv?.hasMoreHistory) return;
    // Stamp a sentinel so we don't fire multiple parallel pages on
    // rapid scroll. The state.set below clears it once the page lands.
    if (loadMoreInFlightRef.current) return;
    loadMoreInFlightRef.current = true;
    const oldest = conv.messages.find((m) => typeof m.id === "number");
    const beforeId = oldest?.id ?? 0;
    invoke("request_dm_history", {
      peer: activeDmUser,
      beforeId,
      limit: 50,
    })
      .catch(console.error)
      .finally(() => {
        loadMoreInFlightRef.current = false;
      });
  };
```

And `const loadMoreInFlightRef = useRef(false);` at the top of the component.

Attach `onScroll={onScrollLoadMore}` to the scrollable div.

- [ ] **Step 3: Add the debounced mark-read**

Below the existing effects, add:

```tsx
  // Debounced mark-read. Fires when the panel becomes visible/active
  // for a peer and the latest message has changed; at most once per
  // second. Sends to server fire-and-forget; also optimistically
  // zeroes unreadCount in the store so the badge clears immediately.
  useEffect(() => {
    if (!activeDmUser) return;
    const conv = useDmStore.getState().conversations[activeDmUser];
    if (!conv) return;
    // Latest message id is the highest id across the in-memory messages.
    const latestId = conv.messages.reduce<number>((acc, m) => {
      if (typeof m.id === "number" && m.id > acc) return m.id;
      return acc;
    }, 0);
    if (latestId === 0 || latestId <= conv.lastReadId) return;
    // Optimistic local clear; server sync follows.
    useDmStore.getState().markRead(activeDmUser, latestId);
    const handle = window.setTimeout(() => {
      invoke("mark_dm_read", { peer: activeDmUser, upToId: latestId }).catch(
        console.error,
      );
    }, 250);  // small coalesce window for rapid message arrivals
    return () => window.clearTimeout(handle);
  }, [
    activeDmUser,
    // Re-run when a new message arrives in the active conversation.
    // Reading conv.messages.length via a selector keeps this stable.
    useDmStore((s) =>
      activeDmUser ? s.conversations[activeDmUser]?.messages.length : 0,
    ),
  ]);
```

Note: that last dependency relies on a Zustand-selector-inside-deps idiom. If the file already uses a different pattern for this (e.g. a separate `useDmStore` selector hook), follow whatever's already established.

- [ ] **Step 4: Verify typecheck**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add electron-client/src/features/dm/DmChatPanel.tsx
git commit -m "feat(dms): DmChatPanel — history fetch + scroll-up paginate + debounced mark-read"
```

---

## Task 11: DmSidebar — unread-count pill

**Files:**
- Modify: `electron-client/src/layouts/DmSidebar.tsx`

- [ ] **Step 1: Read unreadCount per conversation**

Open `electron-client/src/layouts/DmSidebar.tsx`. The component currently iterates conversations and renders a button per peer with a UserAvatar inside. Inside the loop, add a read of `conv.unreadCount` (the loop is already iterating `dmStore.conversations` — the new field is available on each conv).

- [ ] **Step 2: Render the unread pill**

Inside the conversation button JSX, after the `<UserAvatar />` and before the status dot, add:

```tsx
              {conv.unreadCount > 0 && (
                <div
                  className="absolute -top-px -right-px flex h-[16px] min-w-[16px] items-center justify-center rounded-full border-[2px] border-bg-darkest bg-error px-1 text-[9px] font-bold text-white"
                  title={`${conv.unreadCount} unread`}
                >
                  {conv.unreadCount > 99 ? "99+" : conv.unreadCount}
                </div>
              )}
```

The pill sits at the top-right of the avatar, mirroring the status-dot's bottom-right placement. `99+` caps the label so the pill stays compact.

- [ ] **Step 3: Verify typecheck**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add electron-client/src/layouts/DmSidebar.tsx
git commit -m "feat(dms): unread-count pill on DmSidebar avatars"
```

---

## Task 12: End-to-end test pass

**Files:** none — manual verification.

- [ ] **Step 1: Server rebuild + redeploy**

On the Linux build host, pull main and rebuild the central server. The new tables get created automatically by the idempotent DDL in `initializeDatabase` on first start.

- [ ] **Step 2: Start dev client with at least two accounts**

```bash
cd electron-client && npm run dev
```

Log in as user A on one machine/window, user B on another.

- [ ] **Step 3: Verify live persistence (both online)**

User A sends a DM to user B. Both see the message. Now BOTH restart their clients and log back in. Confirm:
- DmSidebar populates with the conversation card showing the last message + timestamp
- Opening the conversation pulls history (≤50 messages) and renders them
- Console log shows `dm_conversations_received` and `dm_history_received` events firing in DevTools

- [ ] **Step 4: Verify offline delivery**

User B closes their app entirely. User A sends 3 DMs to B. User A's UI should show all 3 as delivered (no "user offline" inline error). User B reopens the app + logs in. Confirm:
- DmSidebar shows the conversation with unread badge "3"
- Opening the conversation: history pulls the 3 messages
- Within a second of opening, the unread badge clears (mark-read fired)

- [ ] **Step 5: Verify unread accounting**

User A and B are both online, A is viewing the conversation with B. A sends a DM to B. B's badge increments to 1 (B is online but hasn't opened the conversation). B clicks the conversation → badge clears.

User B sends a DM back. A is currently viewing the conversation → badge stays at 0 (auto-marked as read).

- [ ] **Step 6: Verify pagination**

If you have an account with >50 historical messages with a peer (or you can spam to get there), open the conversation. Scroll up to the top of the message list. Confirm:
- Another page of 50 older messages loads in
- `hasMoreHistory` flips to false once the oldest message is reached; no further pages load

- [ ] **Step 7: Verify friends-only rejection still works**

User C has `dm_friends_only` enabled. User D (not C's friend) sends a DM to C. Confirm:
- D sees the existing inline error "This user only accepts direct messages from users in their friends list."
- No DB row created (verify with `SELECT count(*) FROM dm_messages WHERE recipient = 'C' AND sender = 'D';` returning 0)

- [ ] **Step 8: Verify self-DM guard**

In DevTools console, manually fire `window.decibell.invoke('send_private_message', { recipient: '<own-username>', message: 'self' })`. Confirm: no DB row created, no echo back, no error toast — just silently dropped server-side.

- [ ] **Step 9: Verify pre-existing behaviour**

- Sending DMs between two users in real-time still works (no regression on the live path)
- DM panel switching between peers works without resetting state weirdly
- Friend list panel + status dots still work

- [ ] **Step 10: No commit — testing only**

If anything is broken, return to the relevant task and fix. Otherwise this plan is complete. Version bump to 0.6.5 and tag is a separate ship-time step (per [[feedback_version_bump]]), not part of this plan.

---

## Self-review checklist (already run)

**Spec coverage:**
- ✅ §1 (schema) → Task 2
- ✅ §2 (wire protocol) → Task 1
- ✅ §3 (server flow) → Tasks 2 (methods) + 3 (modified DIRECT_MSG) + 4 (new handlers)
- ✅ §4 (client integration) → Tasks 5–11
- ✅ §5 (edge cases) → Task 12 test matrix covers them
- ✅ §6 (out of scope) — nothing to implement

**Placeholder scan:** no TBDs; every code-bearing step has actual code; verification commands have expected output.

**Type consistency:**
- `DmConversationPreview` proto fields: `peer / last_message_content / last_message_sender / last_message_id / last_timestamp / unread_count` (Task 1) → `DmConversationPreviewRow` C++ struct same names (Task 2) → `DmConversationPreviewPayload` Rust struct same names (Task 5) → renderer `ConversationPreviewPayload` in useDmEvents.ts uses camelCase via serde rename (Task 9) → matches `hydrateConversations` arg shape in dmStore (Task 8). End-to-end consistent.
- `DmHistoryMessage` chain: same pattern, end-to-end aligned.
- `DmMarkReadReq.up_to_id` matches `MarkDmReadArgs.upToId` (Task 7) matches `mark_dm_read` invoke shape (Task 10) matches server `req.up_to_id()` (Task 4).
