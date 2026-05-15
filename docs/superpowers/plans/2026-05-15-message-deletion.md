# Message Deletion — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add Discord-style per-message deletion — hover any message bubble (DM or channel) → red trash icon → styled confirm modal → hard delete from server DB + on-disk attachments → broadcast removes the bubble on every viewer's screen.

**Architecture:** Six new protobuf packets, three each for the two surfaces. DM path: client → central (`DM_DELETE_REQ`) → atomic SQL delete with sender in WHERE clause → response to requester + broadcast to peer. Channel path: client → community (`MESSAGE_DELETE_REQ`) → permission gate via new `can_delete_others` helper (forward-compat for roles) → DB delete that returns attachment unlink-paths → filesystem cleanup reusing CHANNEL_WIPE pattern → broadcast to all members. Renderer is optimistic — snapshots the deleted message into a per-store `pendingDeletions` map so a server-rejected delete can be restored with a toast.

**Tech Stack:** Protobuf 3, C++ + pqxx (central), C++ + sqlite (community), Rust napi-rs (native), React + Zustand (renderer).

**Spec:** `docs/superpowers/specs/2026-05-15-message-deletion-design.md`

---

## File map

**Modify:**
- `proto/messages.proto` — 6 new packet types (76-81), 6 new oneof entries, 6 new messages
- `src/server/auth_manager.hpp` — `deleteDmMessage` declaration
- `src/server/auth_manager.cpp` — impl
- `src/server/main.cpp` — `DM_DELETE_REQ` handler
- `src/community/db.hpp` — `DeleteMessageResult` struct + 3 method declarations
- `src/community/db.cpp` — impls
- `src/community/main.cpp` — `MESSAGE_DELETE_REQ` handler
- `electron-client/native/src/events.rs` — 4 event names + payload structs + emit fns
- `electron-client/native/src/commands/dm.rs` — `delete_dm_message` napi command
- `electron-client/native/src/commands/channels.rs` — `delete_channel_message` napi command
- `electron-client/native/src/net/central.rs` — DM_DELETE_RES + DM_MESSAGE_DELETED arms in `route_packets`
- `electron-client/native/src/net/community.rs` — MESSAGE_DELETE_RES + CHANNEL_MESSAGE_DELETED arms in `route_packets`
- `electron-client/src/stores/chatStore.ts` — `removeMessage` action + `pendingDeletions` field
- `electron-client/src/stores/dmStore.ts` — `removeDmMessage` action + `pendingDmDeletions` field
- `electron-client/src/features/chat/MessageBubble.tsx` — `canDelete`/`onDelete` props + trash icon
- `electron-client/src/features/chat/ChatPanel.tsx` — compute `canDelete` per row, wire `onDelete` → modal open
- `electron-client/src/features/dm/DmChatPanel.tsx` — compute `canDelete` per row, wire `onDelete` → modal open
- `electron-client/src/features/servers/useServerEvents.ts` — `channel_message_delete_responded` + `channel_message_deleted` listeners
- `electron-client/src/features/dm/useDmEvents.ts` — `dm_message_delete_responded` + `dm_message_deleted` listeners

**Create:**
- `electron-client/src/components/DeleteMessageConfirmModal.tsx`
- `electron-client/src/features/servers/useCanDeleteOthers.ts`

---

## Task 1: Protobuf additions

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Packet type enum entries**

In `Packet.Type` enum, after `SERVER_HEARTBEAT_RES = 75;`, append:

```proto
    // --- Message deletion ---
    // (see docs/superpowers/specs/2026-05-15-message-deletion-design.md)
    DM_DELETE_REQ           = 76;   // client→central; JWT-authed
    DM_DELETE_RES           = 77;   // central→requester
    DM_MESSAGE_DELETED      = 78;   // central→sender + recipient (success only)
    MESSAGE_DELETE_REQ      = 79;   // client→community
    MESSAGE_DELETE_RES      = 80;   // community→requester
    CHANNEL_MESSAGE_DELETED = 81;   // community→all members (success only)
```

- [ ] **Step 2: oneof payload entries**

In `Packet.oneof payload`, after `ServerHeartbeatRes server_heartbeat_res = 77;` (or whichever tag is the current last in the oneof — verify), append:

```proto
    // --- Message deletion ---
    DmDeleteReq           dm_delete_req           = 78;
    DmDeleteRes           dm_delete_res           = 79;
    DmMessageDeleted      dm_message_deleted      = 80;
    MessageDeleteReq      message_delete_req      = 81;
    MessageDeleteRes      message_delete_res      = 82;
    ChannelMessageDeleted channel_message_deleted = 83;
```

Tag numbers continue from the existing oneof max. If the auto-rejoin oneof field tags went 75/76/77, this picks up at 78.

- [ ] **Step 3: Append the six message bodies at the end of the file**

```proto
// --- Message deletion ---
// See docs/superpowers/specs/2026-05-15-message-deletion-design.md

message DmDeleteReq {
  string peer = 1;
  int64  message_id = 2;
}

message DmDeleteRes {
  bool   success = 1;
  string message = 2;
  string peer = 3;
  int64  message_id = 4;
}

// Peer field is rewritten by central per recipient — always "the
// other user" from the receiving session's perspective.
message DmMessageDeleted {
  string peer = 1;
  int64  message_id = 2;
  int64  deleted_at = 3;
}

message MessageDeleteReq {
  string channel_id = 1;
  int64  message_id = 2;
}

message MessageDeleteRes {
  bool   success = 1;
  string message = 2;
  string channel_id = 3;
  int64  message_id = 4;
}

message ChannelMessageDeleted {
  string channel_id = 1;
  int64  message_id = 2;
  int64  deleted_at = 3;
  // For future audit-log surfacing; not consumed by current UI.
  string deleted_by = 4;
}
```

- [ ] **Step 4: Verify cargo check passes**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -15
```

Expected: `Finished dev profile [unoptimized + debuginfo] target(s)`. If any E0063 missing-field errors surface, prost-build generated new fields and some construction site needs them filled in; address by adding the missing fields with defaults.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add proto/messages.proto
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "proto(message-delete): DM_DELETE + MESSAGE_DELETE packets (76-81) + bodies"
```

---

## Task 2: Central — `AuthManager::deleteDmMessage`

**Files:**
- Modify: `src/server/auth_manager.hpp`
- Modify: `src/server/auth_manager.cpp`

- [ ] **Step 1: Add method declaration to the header**

In `src/server/auth_manager.hpp`, near the existing DM-related declarations (`markDmRead`, `fetchDmConversations`, etc.), append:

```cpp
    // --- Message deletion (see docs/superpowers/specs/
    //     2026-05-15-message-deletion-design.md) ---

    /// Sender-enforced atomic delete. The WHERE clause is the
    /// authorization check — only the row's sender can delete it,
    /// and the recipient must match the requested peer. Returns
    /// true iff exactly one row was deleted.
    bool deleteDmMessage(const std::string& sender,
                         const std::string& peer,
                         int64_t message_id);
```

- [ ] **Step 2: Add method implementation**

Append to `src/server/auth_manager.cpp` (near the other DM methods like `markDmRead`):

```cpp
bool AuthManager::deleteDmMessage(const std::string& sender,
                                    const std::string& peer,
                                    int64_t message_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Atomic auth: WHERE clause enforces sender-only + correct pair
        // + existence in one shot. Affected_rows == 1 means all three
        // conditions held.
        pqxx::result rs = txn.exec_params(
            "DELETE FROM dm_messages "
            "WHERE id = $1 AND sender = $2 AND recipient = $3",
            message_id, sender, peer);
        txn.commit();
        return rs.affected_rows() == 1;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] deleteDmMessage: " << e.what() << "\n";
        return false;
    }
}
```

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/server/auth_manager.hpp src/server/auth_manager.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(server,message-delete): AuthManager::deleteDmMessage with atomic sender-WHERE auth"
```

---

## Task 3: Central — `DM_DELETE_REQ` handler

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Locate the DM handler block**

Find the `DM_MARK_READ_REQ` handler (around `src/server/main.cpp:351`). After its closing brace, add the new handler.

- [ ] **Step 2: Add the DM_DELETE_REQ handler**

```cpp
        // --- DM_DELETE_REQ ---
        // Sender-only delete. The auth check happens inside the SQL
        // WHERE clause (sender = username_), so a forged packet with
        // someone else's message id is a no-op.
        else if (packet.type() == chatproj::Packet::DM_DELETE_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.dm_delete_req();
            if (req.peer().empty() || req.message_id() == 0) {
                chatproj::Packet rsp;
                rsp.set_type(chatproj::Packet::DM_DELETE_RES);
                auto* res = rsp.mutable_dm_delete_res();
                res->set_success(false);
                res->set_message("Invalid request.");
                res->set_peer(req.peer());
                res->set_message_id(req.message_id());
                std::string serialized;
                rsp.SerializeToString(&serialized);
                auto framed = std::make_shared<std::vector<uint8_t>>(
                    chatproj::create_framed_packet(serialized));
                deliver(framed);
                return;
            }

            bool ok = auth_manager_.deleteDmMessage(
                username_, req.peer(), req.message_id());

            // Always send a RES to the requester (success or failure).
            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::DM_DELETE_RES);
            auto* res = rsp.mutable_dm_delete_res();
            res->set_success(ok);
            res->set_message(ok ? "" : "Message not found or not deletable.");
            res->set_peer(req.peer());
            res->set_message_id(req.message_id());
            std::string serialized;
            rsp.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized));
            deliver(framed);

            if (!ok) return;

            // On success: broadcast DM_MESSAGE_DELETED to BOTH sessions.
            // peer field is rewritten per recipient so it's always "the
            // other user" from the receiving session's perspective.
            int64_t now_ts = static_cast<int64_t>(std::time(nullptr));

            // Echo to the sender (the requester themselves) — drives
            // the broadcast-handler dedupe with the optimistic remove.
            chatproj::Packet sender_bcast;
            sender_bcast.set_type(chatproj::Packet::DM_MESSAGE_DELETED);
            auto* sb = sender_bcast.mutable_dm_message_deleted();
            sb->set_peer(req.peer());     // from sender's POV, peer = recipient
            sb->set_message_id(req.message_id());
            sb->set_deleted_at(now_ts);
            std::string sender_ser;
            sender_bcast.SerializeToString(&sender_ser);
            auto sender_framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(sender_ser));
            deliver(sender_framed);

            // To the recipient (if online): peer = the sender.
            chatproj::Packet recv_bcast;
            recv_bcast.set_type(chatproj::Packet::DM_MESSAGE_DELETED);
            auto* rb = recv_bcast.mutable_dm_message_deleted();
            rb->set_peer(username_);       // from recipient's POV, peer = sender
            rb->set_message_id(req.message_id());
            rb->set_deleted_at(now_ts);
            manager_.send_private(recv_bcast, req.peer());
        }
```

The `manager_.send_private(packet, recipient_username)` helper already exists — it's what the DIRECT_MSG handler uses to forward messages to recipients (engineer should grep `send_private` to confirm; it's a fire-and-forget lookup that no-ops if the recipient isn't online).

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/server/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(server,message-delete): DM_DELETE_REQ handler with peer-rewrite broadcast"
```

---

## Task 4: Community — `CommunityDb` methods

**Files:**
- Modify: `src/community/db.hpp`
- Modify: `src/community/db.cpp`

- [ ] **Step 1: Add the `DeleteMessageResult` struct + method declarations**

In `src/community/db.hpp`, near the existing `WipeChannelResult` struct (search for `struct WipeChannelResult`), append a similar struct and three method declarations on `class CommunityDb`:

```cpp
    // --- Per-message delete (see docs/superpowers/specs/
    //     2026-05-15-message-deletion-design.md) ---
    struct DeleteMessageResult {
        bool ok = false;
        std::vector<std::string> unlink_paths;
    };

    /// Returns the sender username of the given message in this channel,
    /// or nullopt if no such row.
    std::optional<std::string> get_message_sender(
        const std::string& channel_id, int64_t message_id) const;

    /// Hard-deletes the message + its bound attachments in one
    /// transaction. Returns the storage_paths the caller should
    /// unlink from disk (matches WipeChannelResult.unlink_paths
    /// pattern).
    DeleteMessageResult delete_message(
        const std::string& channel_id, int64_t message_id);

    /// Forward-compat permission helper. owner() == username today;
    /// extends with an OR clause when roles ship.
    bool can_delete_others(const std::string& username) const;
```

- [ ] **Step 2: Implement `get_message_sender`**

In `src/community/db.cpp`, append (near the other message methods like `insert_message` / `fetch_messages`):

```cpp
std::optional<std::string> CommunityDb::get_message_sender(
    const std::string& channel_id, int64_t message_id) const {
    std::lock_guard<std::mutex> lock(mutex_);
    Stmt q(db_, "SELECT sender FROM messages WHERE id=? AND channel_id=?;");
    if (!q.s) return std::nullopt;
    q.bind_int64(1, message_id);
    q.bind_text(2, channel_id);
    if (q.step() == SQLITE_ROW) {
        return q.col_text(0);
    }
    return std::nullopt;
}
```

- [ ] **Step 3: Implement `delete_message`**

Append to `src/community/db.cpp`:

```cpp
CommunityDb::DeleteMessageResult CommunityDb::delete_message(
    const std::string& channel_id, int64_t message_id) {
    std::lock_guard<std::mutex> lock(mutex_);
    DeleteMessageResult result;

    exec_sql(db_, "BEGIN IMMEDIATE;");

    // Collect storage_paths of bound attachments before deletion.
    // Only 'ready' attachments with non-empty storage_path; 'uploading'
    // rows are abandoned partial uploads that don't have a final blob
    // on disk yet.
    {
        Stmt q(db_,
            "SELECT storage_path FROM attachments "
            "WHERE message_id=? AND storage_path != '';");
        if (q.s) {
            q.bind_int64(1, message_id);
            while (q.step() == SQLITE_ROW) {
                result.unlink_paths.push_back(q.col_text(0));
            }
        }
    }

    // Delete attachment rows.
    {
        Stmt q(db_, "DELETE FROM attachments WHERE message_id=?;");
        if (q.s) {
            q.bind_int64(1, message_id);
            q.step();
        }
    }

    // Delete the message row. FTS5 mirror trigger handles the index
    // sync — no manual FTS DELETE needed (same as in wipe_channel).
    bool deleted = false;
    {
        Stmt q(db_,
            "DELETE FROM messages WHERE id=? AND channel_id=?;");
        if (q.s) {
            q.bind_int64(1, message_id);
            q.bind_text(2, channel_id);
            deleted = (q.step() == SQLITE_DONE);
        }
    }

    if (deleted) {
        exec_sql(db_, "COMMIT;");
        result.ok = true;
    } else {
        exec_sql(db_, "ROLLBACK;");
        result.unlink_paths.clear();
    }
    return result;
}
```

- [ ] **Step 4: Implement `can_delete_others`**

Append to `src/community/db.cpp`:

```cpp
bool CommunityDb::can_delete_others(const std::string& username) const {
    // Today: owner-only. When roles ship, extend with:
    //   || role_has_permission(username, "DELETE_MESSAGES")
    return owner() == username;
}
```

Note: `owner()` already takes the mutex internally; no extra locking needed here.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/community/db.hpp src/community/db.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(community,message-delete): get_message_sender + delete_message + can_delete_others"
```

---

## Task 5: Community — `MESSAGE_DELETE_REQ` handler

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Locate where to insert**

Find the `CHANNEL_WIPE_REQ` handler at `src/community/main.cpp:818`. Insert the new handler immediately after its closing brace (around line 879).

- [ ] **Step 2: Add the handler**

```cpp
        // --- MESSAGE_DELETE_REQ ---
        // Per-message delete. Self-or-can_delete_others gate. Reuses
        // the wipe filesystem-unlink pattern for attachment blobs.
        else if (packet.type() == chatproj::Packet::MESSAGE_DELETE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.message_delete_req();

            // Always echo a RES so the renderer can clear the pending
            // snapshot. Build it now; populate success/message below.
            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::MESSAGE_DELETE_RES);
            auto* res = rsp.mutable_message_delete_res();
            res->set_channel_id(req.channel_id());
            res->set_message_id(req.message_id());

            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(rsp);
                return;
            }
            if (req.channel_id().empty() || req.message_id() == 0) {
                res->set_success(false);
                res->set_message("Invalid request.");
                send_packet(rsp);
                return;
            }

            auto sender = db->get_message_sender(req.channel_id(), req.message_id());
            if (!sender) {
                res->set_success(false);
                res->set_message("Message not found.");
                send_packet(rsp);
                return;
            }

            if (*sender != username_ && !db->can_delete_others(username_)) {
                res->set_success(false);
                res->set_message("You don't have permission to delete this message.");
                send_packet(rsp);
                return;
            }

            auto del = db->delete_message(req.channel_id(), req.message_id());
            if (!del.ok) {
                res->set_success(false);
                res->set_message("Failed to delete message.");
                send_packet(rsp);
                return;
            }

            res->set_success(true);
            res->set_message("");
            send_packet(rsp);

            // Filesystem cleanup — mirror the CHANNEL_WIPE pattern.
            // Each storage_path may have sibling thumbnail variants;
            // remove them all, ignore errors (orphan files get swept
            // by retention or a future delete).
            for (const auto& path : del.unlink_paths) {
                std::error_code ec;
                std::filesystem::remove(path, ec);
                std::filesystem::remove(path + ".partial", ec);
                std::filesystem::remove(path + ".thumb.jpg", ec);
                std::filesystem::remove(path + ".thumb-320px.jpg", ec);
                std::filesystem::remove(path + ".thumb-640px.jpg", ec);
                std::filesystem::remove(path + ".thumb-1280px.jpg", ec);
            }

            // Broadcast deletion to every authenticated session.
            chatproj::Packet bcast;
            bcast.set_type(chatproj::Packet::CHANNEL_MESSAGE_DELETED);
            auto* bw = bcast.mutable_channel_message_deleted();
            bw->set_channel_id(req.channel_id());
            bw->set_message_id(req.message_id());
            bw->set_deleted_at(static_cast<int64_t>(std::time(nullptr)));
            bw->set_deleted_by(username_);
            manager_.broadcast_to_members(bcast);

            std::cout << "[Community] message " << req.message_id()
                      << " in #" << req.channel_id()
                      << " deleted by " << username_
                      << " (" << del.unlink_paths.size() << " attachments)\n";
        }
```

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/community/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(community,message-delete): MESSAGE_DELETE_REQ handler with perm gate + filesystem cleanup + broadcast"
```

---

**CHECKPOINT 1: Server side complete (Tasks 1-5).** Rebuild central + community on the Linux host. Restart both services. Smoke-test from a SQL console:

```sql
-- Insert a test DM, then watch the central handler delete it via packet flow.
SELECT * FROM dm_messages WHERE sender = '<test-user>';
```

For the community side, the smoke test happens via the renderer once Tasks 6-21 land.

---

## Task 6: Native — events + payload structs

**Files:**
- Modify: `electron-client/native/src/events.rs`

- [ ] **Step 1: Add four event-name constants**

In `electron-client/native/src/events.rs`, near the existing DM-related event constants (search for `DM_HISTORY_RECEIVED`), append:

```rust
// --- Message deletion ---
pub const DM_MESSAGE_DELETE_RESPONDED: &str = "dm_message_delete_responded";
pub const DM_MESSAGE_DELETED: &str = "dm_message_deleted";
pub const CHANNEL_MESSAGE_DELETE_RESPONDED: &str = "channel_message_delete_responded";
pub const CHANNEL_MESSAGE_DELETED: &str = "channel_message_deleted";
```

- [ ] **Step 2: Add four payload structs**

Near the existing persistent-DMs payload structs (`DmConversationPreviewPayload` etc.), append:

```rust
// --- Message deletion ---

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmMessageDeleteRespondedPayload {
    pub success: bool,
    pub message: String,
    pub peer: String,
    pub message_id: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DmMessageDeletedPayload {
    pub peer: String,
    pub message_id: i64,
    pub deleted_at: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMessageDeleteRespondedPayload {
    pub success: bool,
    pub message: String,
    pub server_id: String,
    pub channel_id: String,
    pub message_id: i64,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ChannelMessageDeletedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub message_id: i64,
    pub deleted_at: i64,
    pub deleted_by: String,
}
```

- [ ] **Step 3: Add four emit helpers**

Near the existing `emit_dm_conversations_received` / `emit_dm_history_received` helpers, append:

```rust
pub fn emit_dm_message_delete_responded(payload: DmMessageDeleteRespondedPayload) {
    send(DM_MESSAGE_DELETE_RESPONDED, payload);
}

pub fn emit_dm_message_deleted(payload: DmMessageDeletedPayload) {
    send(DM_MESSAGE_DELETED, payload);
}

pub fn emit_channel_message_delete_responded(payload: ChannelMessageDeleteRespondedPayload) {
    send(CHANNEL_MESSAGE_DELETE_RESPONDED, payload);
}

pub fn emit_channel_message_deleted(payload: ChannelMessageDeletedPayload) {
    send(CHANNEL_MESSAGE_DELETED, payload);
}
```

- [ ] **Step 4: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/events.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,message-delete): 4 new events + payload structs + emit fns"
```

---

## Task 7: Native — `delete_dm_message` command

**Files:**
- Modify: `electron-client/native/src/commands/dm.rs`

- [ ] **Step 1: Append the command**

In `electron-client/native/src/commands/dm.rs`, after `mark_dm_read`, append:

```rust
#[napi(object)]
pub struct DeleteDmMessageArgs {
    pub peer: String,
    pub message_id: i64,
}

/// Sends DM_DELETE_REQ over the JWT-authed central session. The
/// ack arrives as the `dm_message_delete_responded` event; the
/// broadcast (if successful) arrives as `dm_message_deleted`. Both
/// land in useDmEvents on the renderer side.
#[napi]
pub async fn delete_dm_message(args: DeleteDmMessageArgs) -> napi::Result<()> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, DmDeleteReq};

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
            packet::Type::DmDeleteReq,
            packet::Payload::DmDeleteReq(DmDeleteReq {
                peer: args.peer,
                message_id: args.message_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}
```

- [ ] **Step 2: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/commands/dm.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,message-delete): delete_dm_message napi command"
```

---

## Task 8: Native — `delete_channel_message` command

**Files:**
- Modify: `electron-client/native/src/commands/channels.rs`

- [ ] **Step 1: Append the command**

In `electron-client/native/src/commands/channels.rs`, after `send_channel_message` (or any other channel command), append:

```rust
#[napi(object)]
pub struct DeleteChannelMessageArgs {
    pub server_id: String,
    pub channel_id: String,
    pub message_id: i64,
}

/// Sends MESSAGE_DELETE_REQ over the community session for server_id.
/// The ack arrives as the `channel_message_delete_responded` event;
/// the broadcast (if successful) arrives as `channel_message_deleted`.
#[napi]
pub async fn delete_channel_message(args: DeleteChannelMessageArgs) -> napi::Result<()> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, MessageDeleteReq};

    let state_arc = state::shared();
    let (write_tx, data) = {
        let s = state_arc.lock().await;
        let community = s.communities.get(&args.server_id).ok_or_else(|| {
            napi::Error::from_reason(format!(
                "Not connected to community server {}",
                args.server_id
            ))
        })?;
        let tx = community.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Community connection lost")
        })?;
        let token = community.jwt_token().map(|t| t.to_string());
        let pkt = build_packet(
            packet::Type::MessageDeleteReq,
            packet::Payload::MessageDeleteReq(MessageDeleteReq {
                channel_id: args.channel_id,
                message_id: args.message_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    match tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await {
        Ok(Ok(())) => Ok(()),
        Ok(Err(_)) => Err(napi::Error::from_reason("Connection closed")),
        Err(_) => Err(napi::Error::from_reason("Send timed out")),
    }
}
```

If `community.jwt_token()` doesn't exist with that exact name, search `electron-client/native/src/commands/channels.rs` for how `send_channel_message` accesses the per-community JWT and mirror that — usually it's something like `community.jwt().clone()` or a stored token field. Engineer should mirror the existing channel-command pattern verbatim.

- [ ] **Step 2: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/commands/channels.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,message-delete): delete_channel_message napi command"
```

---

## Task 9: Native — DM delete-response/broadcast routing in `central.rs`

**Files:**
- Modify: `electron-client/native/src/net/central.rs`

- [ ] **Step 1: Find the route_packets DM block**

In `electron-client/native/src/net/central.rs`, find the existing `DmHistoryRes` arm (around line 455). The new arms go alongside it in the `match` body.

- [ ] **Step 2: Add the two new arms**

After the `DmHistoryRes` arm, append:

```rust
                Some(packet::Payload::DmDeleteRes(resp)) => {
                    events::emit_dm_message_delete_responded(
                        events::DmMessageDeleteRespondedPayload {
                            success: resp.success,
                            message: resp.message,
                            peer: resp.peer,
                            message_id: resp.message_id,
                        },
                    );
                }
                Some(packet::Payload::DmMessageDeleted(b)) => {
                    events::emit_dm_message_deleted(events::DmMessageDeletedPayload {
                        peer: b.peer,
                        message_id: b.message_id,
                        deleted_at: b.deleted_at,
                    });
                }
```

- [ ] **Step 3: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 4: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/net/central.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,message-delete): route DM_DELETE_RES + DM_MESSAGE_DELETED to renderer"
```

---

## Task 10: Native — channel delete-response/broadcast routing in `community.rs`

**Files:**
- Modify: `electron-client/native/src/net/community.rs`

- [ ] **Step 1: Find the route_packets channel block**

In `electron-client/native/src/net/community.rs`, find the existing `ChannelMsg` / `ChannelHistoryRes` arms (around line 412 / 428). The new arms go alongside them. Note: the route_packets function takes `sid` (server id) as a captured argument — use it directly in the emitted payload.

- [ ] **Step 2: Add the two new arms**

```rust
                Some(packet::Payload::MessageDeleteRes(resp)) => {
                    events::emit_channel_message_delete_responded(
                        events::ChannelMessageDeleteRespondedPayload {
                            success: resp.success,
                            message: resp.message,
                            server_id: sid.clone(),
                            channel_id: resp.channel_id,
                            message_id: resp.message_id,
                        },
                    );
                }
                Some(packet::Payload::ChannelMessageDeleted(b)) => {
                    events::emit_channel_message_deleted(
                        events::ChannelMessageDeletedPayload {
                            server_id: sid.clone(),
                            channel_id: b.channel_id,
                            message_id: b.message_id,
                            deleted_at: b.deleted_at,
                            deleted_by: b.deleted_by,
                        },
                    );
                }
```

- [ ] **Step 3: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 4: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/net/community.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,message-delete): route MESSAGE_DELETE_RES + CHANNEL_MESSAGE_DELETED to renderer"
```

---

## Task 11: Native — addon rebuild + tsc verification

**Files:** none (verification only).

- [ ] **Step 1: Rebuild the addon**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; npm run build 2>&1 | tail -10
```

Expected: `Finished release profile`. The build regenerates `index.d.ts` to expose `deleteDmMessage` and `deleteChannelMessage` to the renderer.

- [ ] **Step 2: Verify renderer tsc still passes**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0. (No renderer code uses the new commands yet, so this just confirms `index.d.ts` regeneration didn't break anything.)

- [ ] **Step 3: Verify the new exports are present**

```bash
grep -E "(deleteDmMessage|deleteChannelMessage)" C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native/index.d.ts
```

Expected: 2 lines of TypeScript export-declare statements.

- [ ] **Step 4: Commit (addon binary + index.d.ts)**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/index.d.ts electron-client/native/index.js
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "build(native,message-delete): regenerate index.d.ts with new delete commands"
```

---

**CHECKPOINT 2: Native chain complete (Tasks 6-11).**

---

## Task 12: `chatStore` — `removeMessage` + `pendingDeletions`

**Files:**
- Modify: `electron-client/src/stores/chatStore.ts`

- [ ] **Step 1: Add field + action declarations to the `ChatState` interface**

In `electron-client/src/stores/chatStore.ts`, near `messagesByChannel: Record<string, Message[]>;`, add:

```ts
  /// Per-channel snapshot of messages that have been optimistically
  /// removed but whose server delete-ack hasn't landed yet. On
  /// rejection (channel_message_delete_responded with success=false)
  /// or watchdog timeout, the snapshot is re-inserted via mergeMessage
  /// + a toast is surfaced. Keyed by channelId → (messageId → Message).
  pendingDeletions: Record<string, Map<number, Message>>;
```

In the actions section of the `ChatState` interface, add:

```ts
  /// Remove a message from a channel's visible message list. Idempotent.
  removeMessage: (channelId: string, messageId: number) => void;
  /// Snapshot a message into pendingDeletions, then remove it. Returns
  /// the snapshot so the caller can pass it to restorePendingDeletion
  /// on failure.
  snapshotAndRemove: (channelId: string, messageId: number) => Message | undefined;
  /// Re-insert a previously-snapshotted message back into the messages
  /// array (sorted by id via existing mergeMessage). Also clears the
  /// pending entry. No-op if no matching snapshot exists.
  restorePendingDeletion: (channelId: string, messageId: number) => void;
  /// Just drop the pending snapshot (called on success-ack or matching
  /// broadcast). No-op if no matching snapshot exists.
  clearPendingDeletion: (channelId: string, messageId: number) => void;
```

- [ ] **Step 2: Add initial value in the `create<...>()` factory**

In the factory body, near `messagesByChannel: {},`:

```ts
  pendingDeletions: {},
```

In `resetForLogout`, add to the `set({...})` argument:

```ts
      pendingDeletions: {},
```

- [ ] **Step 3: Add action implementations**

Near the existing `applyChannelPruned` action (the closest precedent — it also removes message ids from a channel), add:

```ts
  removeMessage: (channelId, messageId) =>
    set((state) => {
      const list = state.messagesByChannel[channelId];
      if (!list) return {};
      const next = list.filter((m) => m.id !== messageId);
      if (next.length === list.length) return {};
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [channelId]: next,
        },
      };
    }),

  snapshotAndRemove: (channelId, messageId) => {
    const state = useChatStore.getState();
    const list = state.messagesByChannel[channelId];
    if (!list) return undefined;
    const snap = list.find((m) => m.id === messageId);
    if (!snap) return undefined;
    useChatStore.setState((s) => {
      const bucket = s.pendingDeletions[channelId] ?? new Map();
      const next = new Map(bucket);
      next.set(messageId, snap);
      return {
        pendingDeletions: {
          ...s.pendingDeletions,
          [channelId]: next,
        },
        messagesByChannel: {
          ...s.messagesByChannel,
          [channelId]: list.filter((m) => m.id !== messageId),
        },
      };
    });
    return snap;
  },

  restorePendingDeletion: (channelId, messageId) =>
    set((state) => {
      const bucket = state.pendingDeletions[channelId];
      const snap = bucket?.get(messageId);
      if (!snap) return {};
      const existing = state.messagesByChannel[channelId] ?? [];
      const merged = mergeMessage(existing, snap);
      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[channelId];
      } else {
        nextPending[channelId] = nextBucket;
      }
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [channelId]: merged,
        },
        pendingDeletions: nextPending,
      };
    }),

  clearPendingDeletion: (channelId, messageId) =>
    set((state) => {
      const bucket = state.pendingDeletions[channelId];
      if (!bucket || !bucket.has(messageId)) return {};
      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[channelId];
      } else {
        nextPending[channelId] = nextBucket;
      }
      return { pendingDeletions: nextPending };
    }),
```

`mergeMessage` is the existing helper already in this file (sorts by id ascending, dedupes by id). `useChatStore.getState()` and `useChatStore.setState(...)` are the standard Zustand escape hatches for actions that need to both read existing state and produce a snapshot return value.

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/stores/chatStore.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(chat-store,message-delete): removeMessage + pendingDeletions snapshot/restore actions"
```

---

## Task 13: `dmStore` — `removeDmMessage` + `pendingDmDeletions`

**Files:**
- Modify: `electron-client/src/stores/dmStore.ts`

- [ ] **Step 1: Add field + action declarations**

In `DmState`, near `conversations: Record<string, DmConversation>;`:

```ts
  /// Per-peer snapshot of optimistically-removed DM messages awaiting
  /// the server ack. Same role as chatStore.pendingDeletions.
  pendingDmDeletions: Record<string, Map<number, DmMessage>>;
```

In the actions section:

```ts
  /// Remove a DM from a peer's visible message list. Idempotent.
  removeDmMessage: (peer: string, messageId: number) => void;
  /// Snapshot + remove for optimistic delete; returns the snapshot.
  snapshotAndRemoveDm: (peer: string, messageId: number) => DmMessage | undefined;
  /// Re-insert a snapshotted DM (rejection path). Sorted by id.
  restorePendingDmDeletion: (peer: string, messageId: number) => void;
  /// Just clear the snapshot (success path).
  clearPendingDmDeletion: (peer: string, messageId: number) => void;
```

- [ ] **Step 2: Add initial value**

In the `create<...>()` factory body, near `conversations: {},`:

```ts
  pendingDmDeletions: {},
```

- [ ] **Step 3: Add action implementations**

After the existing `markRead` action, append:

```ts
  removeDmMessage: (peer, messageId) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      const next = conv.messages.filter((m) => m.id !== messageId);
      if (next.length === conv.messages.length) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: { ...conv, messages: next },
        },
      };
    }),

  snapshotAndRemoveDm: (peer, messageId) => {
    const state = useDmStore.getState();
    const conv = state.conversations[peer];
    if (!conv) return undefined;
    const snap = conv.messages.find((m) => m.id === messageId);
    if (!snap) return undefined;
    useDmStore.setState((s) => {
      const bucket = s.pendingDmDeletions[peer] ?? new Map();
      const next = new Map(bucket);
      next.set(messageId, snap);
      const updatedConv = s.conversations[peer];
      if (!updatedConv) return {};
      return {
        pendingDmDeletions: {
          ...s.pendingDmDeletions,
          [peer]: next,
        },
        conversations: {
          ...s.conversations,
          [peer]: {
            ...updatedConv,
            messages: updatedConv.messages.filter((m) => m.id !== messageId),
          },
        },
      };
    });
    return snap;
  },

  restorePendingDmDeletion: (peer, messageId) =>
    set((state) => {
      const bucket = state.pendingDmDeletions[peer];
      const snap = bucket?.get(messageId);
      if (!snap) return {};
      const conv = state.conversations[peer];
      if (!conv) return {};
      // Re-insert by id ascending. messages are stored oldest-first;
      // find the right slot via binary-ish linear scan (typically
      // 50-200 messages — linear is fine).
      const existing = conv.messages;
      const restored: DmMessage[] = [];
      let inserted = false;
      const snapId = snap.id ?? 0;
      for (const m of existing) {
        const mid = typeof m.id === "number" ? m.id : 0;
        if (!inserted && mid > snapId) {
          restored.push(snap);
          inserted = true;
        }
        restored.push(m);
      }
      if (!inserted) restored.push(snap);

      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDmDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[peer];
      } else {
        nextPending[peer] = nextBucket;
      }
      return {
        conversations: {
          ...state.conversations,
          [peer]: { ...conv, messages: restored },
        },
        pendingDmDeletions: nextPending,
      };
    }),

  clearPendingDmDeletion: (peer, messageId) =>
    set((state) => {
      const bucket = state.pendingDmDeletions[peer];
      if (!bucket || !bucket.has(messageId)) return {};
      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDmDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[peer];
      } else {
        nextPending[peer] = nextBucket;
      }
      return { pendingDmDeletions: nextPending };
    }),
```

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/stores/dmStore.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(dm-store,message-delete): removeDmMessage + pendingDmDeletions snapshot/restore actions"
```

---

## Task 14: `useCanDeleteOthers` hook

**Files:**
- Create: `electron-client/src/features/servers/useCanDeleteOthers.ts`

- [ ] **Step 1: Write the hook**

```ts
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";

/// Returns true if the local user has the "delete others' messages"
/// permission in the given server. Today: owner-only. When roles
/// ship, extend with: || hasRolePermission(serverId, "DELETE_MESSAGES").
///
/// Returns false if serverId is null (e.g. user is on the home or DM
/// view, not viewing a server).
export function useCanDeleteOthers(serverId: string | null): boolean {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  if (!serverId || !localUsername || !owner) return false;
  return owner === localUsername;
}
```

- [ ] **Step 2: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/useCanDeleteOthers.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(perms,message-delete): useCanDeleteOthers hook (owner-only today)"
```

---

## Task 15: `DeleteMessageConfirmModal` component

**Files:**
- Create: `electron-client/src/components/DeleteMessageConfirmModal.tsx`

- [ ] **Step 1: Write the modal component**

```tsx
import { useUiStore } from "../stores/uiStore";

interface DmContext {
  kind: "dm";
  peer: string;
  messageId: number;
}
interface ChannelContext {
  kind: "channel";
  serverId: string;
  channelId: string;
  messageId: number;
}
export type DeleteMessageContext = DmContext | ChannelContext;

interface Props {
  context: DeleteMessageContext;
  onConfirm: () => void;
}

/// Confirmation modal for per-message deletion. The destructive action
/// is fired by the caller (via `onConfirm`); this component only owns
/// the styled UI. Closing the modal happens via useUiStore.closeModal.
export default function DeleteMessageConfirmModal({ onConfirm }: Props) {
  const closeModal = useUiStore((s) => s.closeModal);

  const handleConfirm = () => {
    onConfirm();
    closeModal();
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={closeModal}
    >
      <div
        className="w-full max-w-[400px] rounded-2xl border border-border bg-bg-secondary p-6 shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-[16px] font-semibold text-text-primary">
          Delete message
        </h2>
        <p className="mt-2 text-[13px] text-text-secondary">
          Delete this message? This cannot be undone.
        </p>
        <div className="mt-5 flex justify-end gap-2">
          <button
            onClick={closeModal}
            className="rounded-lg border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
          >
            Cancel
          </button>
          <button
            onClick={handleConfirm}
            className="rounded-lg bg-error px-4 py-2 text-[13px] font-semibold text-white transition-colors hover:bg-error/90"
          >
            Delete
          </button>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Wire into the main modal router**

The renderer has a modal-routing component that reads `useUiStore.activeModal` and renders the matching modal. Grep for an existing modal that uses the same pattern:

```bash
grep -rn 'activeModal === "invite-manage"' C:/Users/sunkh/Desktop/decibell/decibell/electron-client/src
```

Open that file (likely `electron-client/src/AppShell.tsx` or `electron-client/src/layouts/Modals.tsx` — wherever modals are mounted), and add a new branch:

```tsx
{activeModal === "delete-message-confirm" && modalContext && (
  <DeleteMessageConfirmModal
    context={modalContext as DeleteMessageContext}
    onConfirm={() => {
      // The onConfirm wiring is per-callsite; ChatPanel / DmChatPanel
      // open the modal with their own onConfirm prop. If the existing
      // modal router doesn't support per-modal props, an alternative
      // pattern is to store `onConfirm` on uiStore alongside
      // modalContext (see Step 3 of Task 17).
    }}
  />
)}
```

Engineer should look at how the existing modal router passes per-modal data to confirm the pattern; the rest of the modal-route plumbing follows whatever the existing convention is. If the router doesn't natively pass an `onConfirm`, see Task 17's openModal call for the alternative: route through `useUiStore` state.

- [ ] **Step 3: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/components/DeleteMessageConfirmModal.tsx <modal-router-file>
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(ui,message-delete): DeleteMessageConfirmModal component + modal-router wiring"
```

---

## Task 16: `MessageBubble` — trash icon

**Files:**
- Modify: `electron-client/src/features/chat/MessageBubble.tsx`

- [ ] **Step 1: Extend the `Props` interface**

In `MessageBubble.tsx`, change the `Props` interface to include two new optional fields:

```ts
interface Props {
  message: Message;
  grouped: boolean;
  serverId?: string | null;
  isLast?: boolean;
  /// Override the bubble's left padding so the avatar aligns with the
  /// text-input field below. ChatPanel passes a value accounting for
  /// its attach button; DmChatPanel passes a smaller value matching
  /// its input-bar inner padding.
  paddingLeft?: number;
  /// True iff the local user is allowed to delete this message.
  /// Drives the hover-only trash icon visibility. Parents compute this
  /// (ChatPanel: sender-match OR owner; DmChatPanel: sender-match).
  canDelete?: boolean;
  /// Fired when the user clicks the trash icon. Parents open the
  /// DeleteMessageConfirmModal with the right context payload.
  onDelete?: (message: Message) => void;
}
```

- [ ] **Step 2: Destructure the new props**

Change the function signature:

```ts
function MessageBubble({
  message,
  grouped,
  serverId,
  isLast,
  paddingLeft = 8,
  canDelete = false,
  onDelete,
}: Props) {
```

- [ ] **Step 3: Add the trash button JSX**

The component has two render branches — grouped (compact, no header) and full (with avatar + sender + timestamp). Both need the trash icon. Add it as a positioned button inside the outer `<div className="group …">` wrapper of each branch.

For the **grouped branch**, modify:

```tsx
  if (grouped) {
    return (
      <div
        className="group relative flex gap-3 rounded-xl py-px pr-2 hover:bg-white/[0.015]"
        style={{ paddingLeft }}
      >
        {/* ... existing children ... */}
        {canDelete && onDelete && (
          <button
            onClick={() => onDelete(message)}
            title="Delete message"
            className="absolute right-2 top-0 hidden h-6 w-6 items-center justify-center rounded-md bg-bg-secondary text-error hover:bg-error/10 group-hover:flex"
          >
            <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="3 6 5 6 21 6" />
              <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
              <path d="M10 11v6M14 11v6" />
              <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
            </svg>
          </button>
        )}
      </div>
    );
  }
```

Note the addition of `relative` to the wrapper className (required for the absolute-positioned trash button to anchor correctly).

For the **non-grouped branch**, similarly add `relative` to the outer wrapper className and append the same trash button JSX just before the closing `</div>`:

```tsx
  return (
    <div
      className={`group relative flex gap-3 rounded-xl pr-2 pt-2.5 pb-0.5 hover:bg-white/[0.015]${
        isLast ? " animate-[fadeUp_0.3s_ease_both]" : ""
      }`}
      style={{ paddingLeft }}
    >
      {/* ... existing children ... */}
      {canDelete && onDelete && (
        <button
          onClick={() => onDelete(message)}
          title="Delete message"
          className="absolute right-2 top-1 hidden h-6 w-6 items-center justify-center rounded-md bg-bg-secondary text-error hover:bg-error/10 group-hover:flex"
        >
          <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="3 6 5 6 21 6" />
            <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
            <path d="M10 11v6M14 11v6" />
            <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
          </svg>
        </button>
      )}
    </div>
  );
```

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/chat/MessageBubble.tsx
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(ui,message-delete): MessageBubble canDelete/onDelete props + hover-only trash icon"
```

---

## Task 17: `ChatPanel` — wire `canDelete` + delete flow

**Files:**
- Modify: `electron-client/src/features/chat/ChatPanel.tsx`

- [ ] **Step 1: Imports**

Add at the top:

```ts
import { invoke } from "../../lib/ipc";
import { toast } from "../../stores/toastStore";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useCanDeleteOthers } from "../servers/useCanDeleteOthers";
import type { Message } from "../../types";
import type { DeleteMessageContext } from "../../components/DeleteMessageConfirmModal";
```

(Some of these may already be imported — dedupe rather than duplicating.)

- [ ] **Step 2: Add hooks at the top of the component body**

```tsx
  const localUsername = useAuthStore((s) => s.username);
  const canDeleteOthers = useCanDeleteOthers(activeServerId);
  const openModal = useUiStore((s) => s.openModal);
```

(`activeServerId` is already read from chatStore in this component.)

- [ ] **Step 3: Add the delete handler + watchdog**

Inside the component body (above the JSX return), add:

```tsx
  // Fire the delete flow for a channel message. Optimistic: snapshot
  // the message into pendingDeletions, remove it from the view, fire
  // the native command, and start a 5-second watchdog. The watchdog
  // fires only if no `channel_message_delete_responded` event arrives
  // by then — both success and failure responses are handled in
  // useServerEvents (Task 19), which clears the pendingDeletion entry.
  // If the watchdog fires while the entry is still pending, restore.
  const handleDeleteChannelMessage = (message: Message) => {
    if (!activeServerId || !activeChannelId || typeof message.id !== "number") return;
    const serverId = activeServerId;
    const channelId = activeChannelId;
    const messageId = message.id;

    useChatStore.getState().snapshotAndRemove(channelId, messageId);

    invoke("delete_channel_message", { serverId, channelId, messageId }).catch(
      (err) => {
        console.error("delete_channel_message:", err);
        useChatStore.getState().restorePendingDeletion(channelId, messageId);
        toast.error("Failed to delete message", "Please try again.");
      },
    );

    // 5-second watchdog: if no response/broadcast arrives by then,
    // assume the network is hung and restore the bubble.
    window.setTimeout(() => {
      const stillPending = useChatStore
        .getState()
        .pendingDeletions[channelId]?.has(messageId);
      if (stillPending) {
        useChatStore.getState().restorePendingDeletion(channelId, messageId);
        toast.error(
          "Delete timed out",
          "Couldn't reach the server. Please try again.",
        );
      }
    }, 5000);
  };

  const requestDeleteChannelMessage = (message: Message) => {
    if (!activeServerId || !activeChannelId || typeof message.id !== "number") return;
    const ctx: DeleteMessageContext = {
      kind: "channel",
      serverId: activeServerId,
      channelId: activeChannelId,
      messageId: message.id,
    };
    openModal("delete-message-confirm", ctx, () =>
      handleDeleteChannelMessage(message),
    );
  };
```

`openModal(name, context, onConfirm)` is a small extension to the existing `useUiStore.openModal(name, context)` to accept an optional confirm callback. If the existing signature is just `(name, context)`, extend it to `(name, context, onConfirm?)` and have the store stash `onConfirm` alongside `activeModal`/`modalContext` so the modal router (Task 15 Step 2) can invoke it.

- [ ] **Step 4: Pass `canDelete` and `onDelete` props to MessageBubble**

Find the `<MessageBubble>` render site in this file (in the Virtuoso `itemContent` or message map). Add the two new props:

```tsx
<MessageBubble
  message={message}
  grouped={grouped}
  serverId={activeServerId}
  isLast={isLast}
  paddingLeft={/* existing value */}
  canDelete={
    typeof message.id === "number" &&
    message.id > 0 &&
    (message.sender === localUsername || canDeleteOthers)
  }
  onDelete={requestDeleteChannelMessage}
/>
```

The `message.id > 0` guard skips optimistic-bubble placeholders (no server id yet — nothing to delete).

- [ ] **Step 5: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/chat/ChatPanel.tsx electron-client/src/stores/uiStore.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(chat,message-delete): ChatPanel canDelete + optimistic delete flow + 5s watchdog"
```

(Include `uiStore.ts` in the commit if the `openModal` signature change was needed.)

---

## Task 18: `DmChatPanel` — wire `canDelete` + delete flow

**Files:**
- Modify: `electron-client/src/features/dm/DmChatPanel.tsx`

- [ ] **Step 1: Imports**

```ts
import { invoke } from "../../lib/ipc";
import { toast } from "../../stores/toastStore";
import { useDmStore } from "../../stores/dmStore";
import type { DmMessage } from "../../types";
import type { DeleteMessageContext } from "../../components/DeleteMessageConfirmModal";
```

(Dedupe with existing imports.)

- [ ] **Step 2: Add the delete handler + watchdog**

Inside `DmChatPanel`, near the existing `useEffect` blocks, add (place above the JSX return):

```tsx
  const openModal = useUiStore((s) => s.openModal);

  const handleDeleteDmMessage = (message: DmMessage) => {
    if (!activeDmUser || typeof message.id !== "number") return;
    const peer = activeDmUser;
    const messageId = message.id;

    useDmStore.getState().snapshotAndRemoveDm(peer, messageId);

    invoke("delete_dm_message", { peer, messageId }).catch((err) => {
      console.error("delete_dm_message:", err);
      useDmStore.getState().restorePendingDmDeletion(peer, messageId);
      toast.error("Failed to delete message", "Please try again.");
    });

    window.setTimeout(() => {
      const stillPending = useDmStore
        .getState()
        .pendingDmDeletions[peer]?.has(messageId);
      if (stillPending) {
        useDmStore.getState().restorePendingDmDeletion(peer, messageId);
        toast.error(
          "Delete timed out",
          "Couldn't reach the server. Please try again.",
        );
      }
    }, 5000);
  };

  const requestDeleteDmMessage = (message: DmMessage) => {
    if (!activeDmUser || typeof message.id !== "number") return;
    const ctx: DeleteMessageContext = {
      kind: "dm",
      peer: activeDmUser,
      messageId: message.id,
    };
    openModal("delete-message-confirm", ctx, () =>
      handleDeleteDmMessage(message),
    );
  };
```

- [ ] **Step 3: Pass `canDelete` and `onDelete` props to MessageBubble**

Find the `<MessageBubble>` site in this file and extend it:

```tsx
<MessageBubble
  message={msg as Message}
  grouped={grouped}
  serverId={null}
  isLast={isLast}
  paddingLeft={/* existing value */}
  canDelete={
    typeof msg.id === "number" &&
    msg.id > 0 &&
    msg.sender === localUsername
  }
  onDelete={(m) => requestDeleteDmMessage(m as DmMessage)}
/>
```

The `as Message` / `as DmMessage` casts are because the two shapes differ slightly (Message has `id?: number`, DmMessage has `id?: number` — they're already structurally compatible for what MessageBubble reads; the cast is just to satisfy strict-typing of the prop interface, which uses `Message` from the chat type).

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/dm/DmChatPanel.tsx
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(dm,message-delete): DmChatPanel canDelete + optimistic delete flow + 5s watchdog"
```

---

## Task 19: `useServerEvents` — channel delete listeners

**Files:**
- Modify: `electron-client/src/features/servers/useServerEvents.ts`

- [ ] **Step 1: Add the response listener**

In the `useEffect` body, after existing `unlistenAuth` / `unlistenMembers` / etc., add:

```ts
    const unlistenChannelDeleteRes = listen<{
      success: boolean;
      message: string;
      serverId: string;
      channelId: string;
      messageId: number;
    }>("channel_message_delete_responded", (event) => {
      const p = event.payload;
      const chat = useChatStore.getState();
      if (!p.success) {
        // Server rejected (403/404). Restore the bubble + surface
        // the server's reason as a toast.
        chat.restorePendingDeletion(p.channelId, p.messageId);
        toast.error(
          "Couldn't delete message",
          p.message || "Server rejected the request.",
        );
        return;
      }
      // Success: clear the pending entry. The broadcast (or already-
      // optimistic-remove) keeps the bubble gone.
      chat.clearPendingDeletion(p.channelId, p.messageId);
    });
```

- [ ] **Step 2: Add the broadcast listener**

```ts
    const unlistenChannelDeleted = listen<{
      serverId: string;
      channelId: string;
      messageId: number;
      deletedAt: number;
      deletedBy: string;
    }>("channel_message_deleted", (event) => {
      const { channelId, messageId } = event.payload;
      const chat = useChatStore.getState();
      // Idempotent: removeMessage on an already-gone id is a no-op.
      // Same handler for "my delete succeeded" (already removed
      // optimistically) and "someone else deleted this message".
      chat.removeMessage(channelId, messageId);
      chat.clearPendingDeletion(channelId, messageId);
    });
```

- [ ] **Step 3: Wire both into the cleanup return**

In the `return () => { ... }` block at the bottom of the `useEffect`:

```ts
      unlistenChannelDeleteRes.then((fn) => fn());
      unlistenChannelDeleted.then((fn) => fn());
```

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/useServerEvents.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(events,message-delete): channel delete response + broadcast listeners"
```

---

## Task 20: `useDmEvents` — DM delete listeners

**Files:**
- Modify: `electron-client/src/features/dm/useDmEvents.ts`

- [ ] **Step 1: Add the response listener**

In the `useEffect` body, after the existing `unlistenConv` / `unlistenHist` listeners:

```ts
    const unlistenDmDeleteRes = listen<{
      success: boolean;
      message: string;
      peer: string;
      messageId: number;
    }>("dm_message_delete_responded", (event) => {
      const p = event.payload;
      const dm = useDmStore.getState();
      if (!p.success) {
        dm.restorePendingDmDeletion(p.peer, p.messageId);
        toast.error(
          "Couldn't delete message",
          p.message || "Server rejected the request.",
        );
        return;
      }
      dm.clearPendingDmDeletion(p.peer, p.messageId);
    });
```

(`toast` should already be imported in this file; if not, `import { toast } from "../../stores/toastStore";`.)

- [ ] **Step 2: Add the broadcast listener**

```ts
    const unlistenDmDeleted = listen<{
      peer: string;
      messageId: number;
      deletedAt: number;
    }>("dm_message_deleted", (event) => {
      const { peer, messageId } = event.payload;
      const dm = useDmStore.getState();
      dm.removeDmMessage(peer, messageId);
      dm.clearPendingDmDeletion(peer, messageId);
    });
```

- [ ] **Step 3: Wire into the cleanup return**

```ts
      unlistenDmDeleteRes.then((fn) => fn());
      unlistenDmDeleted.then((fn) => fn());
```

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/dm/useDmEvents.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(events,message-delete): DM delete response + broadcast listeners"
```

---

## Task 21: Renderer build verification

**Files:** none (verification only).

- [ ] **Step 1: Final tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0, no warnings.

- [ ] **Step 2: Dev start (smoke-only)**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npm run dev
```

Confirm the app starts without console errors. Hard-Ctrl-C to stop; we're not testing functionality yet, just that the renderer builds.

If you get any "Could not resolve …/twemoji-data.json" errors at this stage, run `npm run build:twemoji` first (the gitignored JSON map needs regenerating on a fresh worktree).

---

**CHECKPOINT 3: Renderer complete (Tasks 12-21).**

---

## Task 22: End-to-end manual test pass

**Files:** none — manual verification only.

Run the dev client. Rebuild + redeploy central & community on the Linux host so the server-side handlers are live.

- [ ] **Step 1: DM self-delete (basic path)**

In a DM with another user, send a message ("DEL_TEST_1"). Hover the bubble → red trash icon appears top-right. Click → confirm modal appears with the right copy ("Delete this message? This cannot be undone."). Click Delete. Expected:
- Bubble disappears immediately (optimistic)
- No error toast surfaces
- Reload the conversation (close & reopen the panel) → message stays gone

Confirm in central DB:
```sql
SELECT id FROM dm_messages WHERE content = 'DEL_TEST_1';
```
Expected: 0 rows.

- [ ] **Step 2: DM cross-device broadcast**

Two users in a DM: user A and user B. A deletes one of A's own messages. On B's client (kept open), the bubble disappears within ~50–500ms without B having to refresh.

- [ ] **Step 3: DM permission enforcement (recipient can't delete sender's message)**

The trash icon should not appear on bubbles you didn't send (DM context). Verify by looking at incoming messages — no icon.

Negative test (defense-in-depth): in devtools, manually call `await window.decibell.deleteDmMessage({ peer: "<sender>", messageId: <their id> })`. Expected: a "Couldn't delete message" toast appears with "Message not found or not deletable" — the atomic SQL refused. Original bubble (if optimistically removed by the manual call) is restored within a couple seconds.

- [ ] **Step 4: Channel self-delete (basic path)**

In a community-server text channel, send a message ("DEL_TEST_2"). Hover → trash icon → modal → Delete. Expected:
- Bubble disappears
- No error toast
- Other connected viewers' bubble also disappears
- Reloading the channel → message stays gone

- [ ] **Step 5: Channel owner-delete of someone else's message**

User A is the server owner. User B (regular member) sends "DEL_TEST_3". A sees the trash icon on B's message (per the owner permission). A clicks → modal → Delete. Expected:
- B's message disappears for both A and B
- DB confirmation: `SELECT id FROM messages WHERE content = 'DEL_TEST_3';` returns 0 rows on the community SQLite

- [ ] **Step 6: Channel non-owner permission denied**

User B (regular member) tries to delete user A's message via devtools console (the trash icon won't show — confirm that first):
```js
await window.decibell.deleteChannelMessage({
  serverId: "<...>",
  channelId: "<...>",
  messageId: <A's message id>,
});
```
Expected: toast.error "Couldn't delete message — You don't have permission to delete this message."

- [ ] **Step 7: Channel attachment cleanup**

Send a message with an image attachment in a channel. Note the attachment id from devtools (or check the community's data dir). Delete the message via the trash icon. Expected:
- Message + attachment row gone from community DB
- Attachment blob + `.thumb-*.jpg` variants gone from disk

Confirm on community host:
```bash
ls /path/to/community-data/<server-id>/attachments/<attachment-id>.*
```
Expected: no matches.

- [ ] **Step 8: Watchdog timeout (network-error path)**

Kill the central server (for DM) or community server (for channel) while client is connected — easiest via dev tools is just disable network in devtools' Network tab. Try to delete a message. Expected:
- Bubble disappears optimistically
- After ~5 seconds, "Delete timed out — Couldn't reach the server" toast surfaces
- Bubble is restored to the chat view in correct id-sorted position

- [ ] **Step 9: Modal cancel path**

Hover a message → trash → modal → click "Cancel". Expected: modal closes, message stays. No bubble flicker. No RPC fires (check devtools network tab — no packet sent).

- [ ] **Step 10: Regression sweep**

- DM sending still works; new bubbles appear correctly
- Channel sending still works
- Channel history pagination still works (scroll up loads older messages, no stale "deleted" ghosts re-appear)
- DM unread badges still update correctly when peer sends after a delete
- CHANNEL_WIPE (full-channel wipe) still works for the owner

If anything regresses, return to the relevant task. Otherwise the plan is complete.

---

## Self-review notes

**Spec coverage:**
- §1 UX (trash hover, modal copy, optimistic + restore) → Tasks 15, 16, 17, 18, 19, 20
- §2 Permissions (DM sender-only via SQL WHERE, channel self-or-owner via `can_delete_others`) → Tasks 2, 3, 4, 5, 14
- §3 Wire protocol (6 packets) → Task 1
- §4 Server-side processing (central deleteDmMessage + DM_DELETE_REQ handler, community delete_message + MESSAGE_DELETE_REQ handler + filesystem unlink) → Tasks 2, 3, 4, 5
- §5 Native + Renderer (4 events, 2 commands, route_packets arms, stores, listeners, modal, bubble, panels) → Tasks 6-20
- Error handling matrix (perm-denied, 404, network error/watchdog, recipient offline) → Tasks 5 (server response branches), 17, 18 (watchdog), 19, 20 (toast on rejection)

**Placeholder scan:** every code-bearing step has actual code. Spots where the engineer needs to verify existing patterns (e.g. `community.jwt_token()` exact name in Task 8, modal-router location in Task 15) are flagged with grep guidance, not silent "TODO" placeholders.

**Type consistency:**
- `DmDeleteReq` / `DmDeleteRes` / `DmMessageDeleted` / `MessageDeleteReq` / `MessageDeleteRes` / `ChannelMessageDeleted` proto names match across Task 1 (definition) → Tasks 3, 5 (server handlers) → Tasks 7, 8 (native commands) → Tasks 9, 10 (native routing).
- `deleteDmMessage` AuthManager method matches across Task 2 (decl + impl) → Task 3 (caller).
- `get_message_sender` / `delete_message` / `can_delete_others` / `DeleteMessageResult` CommunityDb names match across Task 4 (decl + impl) → Task 5 (caller).
- Event names (`dm_message_delete_responded`, `dm_message_deleted`, `channel_message_delete_responded`, `channel_message_deleted`) consistent across Task 6 (constants) → Tasks 9, 10 (emit sites) → Tasks 19, 20 (listen sites).
- chatStore actions (`removeMessage`, `snapshotAndRemove`, `restorePendingDeletion`, `clearPendingDeletion`) consistent across Task 12 (definition) → Tasks 17, 19 (callers).
- dmStore actions (`removeDmMessage`, `snapshotAndRemoveDm`, `restorePendingDmDeletion`, `clearPendingDmDeletion`) consistent across Task 13 (definition) → Tasks 18, 20 (callers).
- `useCanDeleteOthers` hook signature consistent across Task 14 (definition) → Task 17 (caller).
- `DeleteMessageContext` discriminated union (`{kind: "dm"|"channel", …}`) consistent across Task 15 (definition) → Tasks 17, 18 (callers).
- `canDelete` / `onDelete` MessageBubble props consistent across Task 16 (definition) → Tasks 17, 18 (callers).
