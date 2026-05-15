# Custom Server Pictures — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Owners can upload a custom image for their community server; ServerBar tiles render that image (fills the rectangle, dim overlay + name in white when inactive, just-image when active) instead of the default gradient-letter rectangle.

**Architecture:** Storage on central (`community_servers.picture` + `picture_version`). Owner uploads to their community server; community verifies ownership locally and forwards to central via shared-secret one-shot TLS (mirrors auto-rejoin's `MEMBERSHIP_REGISTER_REQ`). Central broadcasts `SERVER_PICTURE_CHANGED` to every online session whose username is in `user_communities` for that `server_id`. Renderer lazy-fetches bytes on first tile render with a version-guard against stale fetches.

**Tech Stack:** Protobuf 3, C++ + pqxx (central), C++ + sqlite (community), OpenSSL (sha256 + TLS), Rust napi-rs + base64 + prost (native), React + Zustand (renderer).

**Spec:** `docs/superpowers/specs/2026-05-15-custom-server-pictures-design.md`

---

## File map

**Modify:**
- `proto/messages.proto` — 6 new packet types (82-87), 6 new oneof entries, 6 new messages, +1 field on `CommunityServerInfo`
- `src/server/auth_manager.hpp` — 3 new method declarations
- `src/server/auth_manager.cpp` — DDL migrations inside `upsertCommunityServer`; `setServerPicture` / `getServerPicture` / `getServerMembers`; `picture_version` added to `getCommunityServers` and `getUserCommunities` SELECT
- `src/server/main.cpp` — JWT-gate whitelist entry for `SYNC_SERVER_PICTURE_REQ`; new handlers for `SYNC_SERVER_PICTURE_REQ` + `FETCH_SERVER_PICTURE_REQ`; `broadcast_to_users` helper on SessionManager (if no equivalent exists)
- `src/community/main.cpp` — `sha256_hex` helper; `UPDATE_SERVER_PICTURE_REQ` handler
- `electron-client/native/src/commands/servers.rs` — `update_server_picture` + `fetch_server_picture` napi commands
- `electron-client/native/src/net/central.rs` — `FETCH_SERVER_PICTURE_RES` + `SERVER_PICTURE_CHANGED` packet arms (base64-encodes bytes before emit)
- `electron-client/native/src/net/community.rs` — `UPDATE_SERVER_PICTURE_RES` packet arm
- `electron-client/native/src/events.rs` — 3 new event names + payload structs + emit helpers
- `electron-client/native/Cargo.toml` — add `base64` crate dependency if not already present
- `electron-client/src/stores/chatStore.ts` — `serverPictureVersions` + `serverPictures` maps + version-guarded setters; populate from existing `server_list_received` / `memberships_received` paths; reset on logout
- `electron-client/src/features/servers/useServerEvents.ts` — 3 new listeners + extend `server_list_received` / `memberships_received` to populate pictureVersion
- `electron-client/src/features/servers/ServerActionsDropdown.tsx` — new "Server Settings" entry, owner-only
- `electron-client/src/features/channels/ServerChannelsSidebar.tsx` — mount `ServerSettingsModal` conditional on `activeModal === "server-settings"`
- `electron-client/src/features/servers/ServerBar.tsx` — two render branches keyed off `pictureVersion`; `useFetchServerPictureIfMissing` hook; `PLACEHOLDER_DATA_URL` constant

**Create:**
- `electron-client/src/features/servers/useCanEditServerSettings.ts` — owner-only hook (forward-compat for roles)
- `electron-client/src/features/servers/ServerSettingsModal.tsx` — tabbed modal mirroring `SettingsModal`, single "Overview" tab with picture management

---

## Task 1: Protobuf additions

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Add packet type enum entries**

In `Packet.Type` enum, append after `CHANNEL_MESSAGE_DELETED = 81;`:

```proto
    // --- Custom server pictures ---
    // (see docs/superpowers/specs/2026-05-15-custom-server-pictures-design.md)
    UPDATE_SERVER_PICTURE_REQ = 82;  // client→community (JWT)
    UPDATE_SERVER_PICTURE_RES = 83;  // community→requester
    SYNC_SERVER_PICTURE_REQ   = 84;  // community→central (shared secret)
    FETCH_SERVER_PICTURE_REQ  = 85;  // client→central (JWT)
    FETCH_SERVER_PICTURE_RES  = 86;  // central→requester
    SERVER_PICTURE_CHANGED    = 87;  // central→every online member
```

- [ ] **Step 2: Add oneof payload entries**

In `Packet.oneof payload`, after `ChannelMessageDeleted channel_message_deleted = 83;` (or whichever tag is the current max), append:

```proto
    // --- Custom server pictures ---
    UpdateServerPictureReq update_server_picture_req = 84;
    UpdateServerPictureRes update_server_picture_res = 85;
    SyncServerPictureReq   sync_server_picture_req   = 86;
    FetchServerPictureReq  fetch_server_picture_req  = 87;
    FetchServerPictureRes  fetch_server_picture_res  = 88;
    ServerPictureChanged   server_picture_changed    = 89;
```

The exact tag numbers continue from the existing oneof max (verify by reading the file; the message-delete plan landed entries up through tag 83).

- [ ] **Step 3: Append the six message bodies at the end of the file**

```proto
// --- Custom server pictures ---
// See docs/superpowers/specs/2026-05-15-custom-server-pictures-design.md

message UpdateServerPictureReq {
  bytes data = 1;   // empty = remove
}

message UpdateServerPictureRes {
  bool   success = 1;
  string message = 2;
  string version = 3;   // sha256-hex; '' on removal or failure
}

message SyncServerPictureReq {
  string host = 1;
  int32  port = 2;
  bytes  data = 3;
  string version = 4;   // pre-computed by community
}

message FetchServerPictureReq {
  int32 server_id = 1;
}

message FetchServerPictureRes {
  int32  server_id = 1;
  string version = 2;
  bytes  data = 3;
}

message ServerPictureChanged {
  int32  server_id = 1;
  string version = 2;
}
```

- [ ] **Step 4: Extend `CommunityServerInfo`**

Find `message CommunityServerInfo { ... }` and add the new field:

```proto
message CommunityServerInfo {
  int32  id = 1;
  string name = 2;
  string description = 3;
  string host_ip = 4;
  int32  port = 5;
  int32  member_count = 6;
  // sha256-hex of the server picture; '' when no picture is set.
  // Propagates through LoginResponse.memberships and
  // ServerListResponse.servers so clients know up-front which tiles
  // have a picture.
  string picture_version = 7;
}
```

- [ ] **Step 5: Verify cargo check (regenerates Rust bindings)**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile [unoptimized + debuginfo] target(s)`. If any E0063 missing-field surfaces on `CommunityServerInfo { ... }` initializers, add `picture_version: String::new()` to those sites.

- [ ] **Step 6: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add proto/messages.proto
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "proto(server-pictures): UPDATE/SYNC/FETCH/CHANGED packets (82-87) + CommunityServerInfo.picture_version"
```

---

## Task 2: Central — AuthManager DDL + new methods + picture_version on existing queries

**Files:**
- Modify: `src/server/auth_manager.hpp`
- Modify: `src/server/auth_manager.cpp`

- [ ] **Step 1: Header — add method declarations**

In `src/server/auth_manager.hpp`, after the auto-rejoin methods (`registerMembership` / `revokeMembership` / `getUserCommunities`), append:

```cpp
    // --- Custom server pictures ---
    // (see docs/superpowers/specs/2026-05-15-custom-server-pictures-design.md)

    /// Atomic update of the picture bytes + sha256-hex version.
    /// Looks up the community_servers row by (host_ip, port) and
    /// writes the new picture + picture_version. Returns the
    /// assigned id, or 0 if no matching row exists yet (the
    /// community's first heartbeat hasn't landed). Empty `data`
    /// means removal: column is set NULL, version becomes ''.
    int setServerPicture(const std::string& host_ip, int port,
                          const std::string& data,
                          const std::string& version);

    /// Returns (version, data). Both empty when the server has no
    /// picture set, or when the id is unknown.
    std::pair<std::string, std::string> getServerPicture(int server_id);

    /// Every username in user_communities for this server_id.
    /// Drives central's SERVER_PICTURE_CHANGED broadcast.
    std::vector<std::string> getServerMembers(int server_id);
```

- [ ] **Step 2: Cpp — add the schema migration inside `upsertCommunityServer`**

Find `AuthManager::upsertCommunityServer` (which contains the existing `CREATE TABLE IF NOT EXISTS community_servers (...)` block). Immediately after the create-table call, before the upsert INSERT, add:

```cpp
        txn.exec(
            "ALTER TABLE community_servers "
            "ADD COLUMN IF NOT EXISTS picture BYTEA"
        );
        txn.exec(
            "ALTER TABLE community_servers "
            "ADD COLUMN IF NOT EXISTS picture_version VARCHAR(64) "
            "NOT NULL DEFAULT ''"
        );
```

Both idempotent — safe on already-deployed servers. Matches the auto-rejoin `user_communities` migration pattern.

- [ ] **Step 3: Cpp — implement `setServerPicture`**

Append to `src/server/auth_manager.cpp`:

```cpp
int AuthManager::setServerPicture(const std::string& host_ip, int port,
                                    const std::string& data,
                                    const std::string& version) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        // Empty data means removal: write NULL + empty version.
        // Otherwise write the bytes + sha256-hex version.
        pqxx::result rs;
        if (data.empty()) {
            rs = txn.exec_params(
                "UPDATE community_servers "
                "SET picture = NULL, picture_version = '' "
                "WHERE host_ip = $1 AND port = $2 "
                "RETURNING id",
                host_ip, port);
        } else {
            rs = txn.exec_params(
                "UPDATE community_servers "
                "SET picture = $1, picture_version = $2 "
                "WHERE host_ip = $3 AND port = $4 "
                "RETURNING id",
                pqxx::binarystring(data.data(), data.size()),
                version, host_ip, port);
        }
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] setServerPicture: " << e.what() << "\n";
        return 0;
    }
}
```

- [ ] **Step 4: Cpp — implement `getServerPicture`**

Append:

```cpp
std::pair<std::string, std::string>
AuthManager::getServerPicture(int server_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT picture_version, "
            "       COALESCE(picture::bytea, ''::bytea) "
            "FROM community_servers WHERE id = $1",
            server_id);
        txn.commit();
        if (rs.empty()) return {"", ""};
        std::string version = rs[0][0].as<std::string>();
        // pqxx::binarystring handles BYTEA → std::string conversion.
        pqxx::binarystring bs(rs[0][1]);
        std::string data(bs.data(), bs.size());
        return {std::move(version), std::move(data)};
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getServerPicture: " << e.what() << "\n";
        return {"", ""};
    }
}
```

If the existing user-avatar `getAvatar` uses a different pqxx idiom for BYTEA reads, prefer that pattern for consistency — engineer should grep `binarystring` in the existing auth_manager.cpp and align.

- [ ] **Step 5: Cpp — implement `getServerMembers`**

Append:

```cpp
std::vector<std::string> AuthManager::getServerMembers(int server_id) {
    std::vector<std::string> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT username FROM user_communities "
            "WHERE server_id = $1",
            server_id);
        txn.commit();
        out.reserve(rs.size());
        for (const auto& row : rs) {
            out.push_back(row[0].as<std::string>());
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getServerMembers: " << e.what() << "\n";
        return {};
    }
}
```

- [ ] **Step 6: Cpp — add `picture_version` to `getCommunityServers` SELECT**

Find `AuthManager::getCommunityServers`. Update the SELECT to include `picture_version`:

```cpp
pqxx::result res = txn.exec(
    "SELECT id, name, description, host_ip, port, member_count, "
    "       COALESCE(picture_version, '') "
    "FROM community_servers ORDER BY member_count DESC LIMIT 50"
);
```

And in the row-to-proto mapping loop, add:

```cpp
info.set_picture_version(row[6].as<std::string>());
```

right after the existing setters.

- [ ] **Step 7: Cpp — add `picture_version` to `getUserCommunities` SELECT**

Find `AuthManager::getUserCommunities`. Update the SELECT:

```cpp
pqxx::result rs = txn.exec_params(
    "SELECT cs.id, cs.name, cs.description, cs.host_ip, "
    "       cs.port, cs.member_count, "
    "       COALESCE(cs.picture_version, '') "
    "FROM user_communities uc "
    "JOIN community_servers cs ON cs.id = uc.server_id "
    "WHERE uc.username = $1 "
    "ORDER BY uc.joined_at",
    username);
```

And in the row-to-proto loop add `info.set_picture_version(row[6].as<std::string>());` after the existing setters.

- [ ] **Step 8: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/server/auth_manager.hpp src/server/auth_manager.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(server,server-pictures): community_servers schema + setServerPicture/getServerPicture/getServerMembers + picture_version on existing queries"
```

---

## Task 3: Central — JWT-gate whitelist + SYNC_SERVER_PICTURE_REQ handler with broadcast

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Add `SYNC_SERVER_PICTURE_REQ` to the JWT-gate whitelist**

At `src/server/main.cpp:152` find the existing if-not-shared-secret block (the one that exempts `LOGIN_REQ`, `SERVER_HEARTBEAT`, `INVITE_REGISTER_REQ`, `MEMBERSHIP_REGISTER_REQ` etc. from `validateToken`). Add the new exemption:

```cpp
        if (packet.type() != chatproj::Packet::REGISTER_REQ &&
            packet.type() != chatproj::Packet::LOGIN_REQ &&
            packet.type() != chatproj::Packet::HANDSHAKE &&
            packet.type() != chatproj::Packet::SERVER_HEARTBEAT &&
            packet.type() != chatproj::Packet::CLIENT_PING &&
            packet.type() != chatproj::Packet::INVITE_REGISTER_REQ &&
            packet.type() != chatproj::Packet::INVITE_UNREGISTER_REQ &&
            packet.type() != chatproj::Packet::MEMBERSHIP_REGISTER_REQ &&
            packet.type() != chatproj::Packet::MEMBERSHIP_REVOKE_REQ &&
            packet.type() != chatproj::Packet::SYNC_SERVER_PICTURE_REQ) {
            if (!auth_manager_.validateToken(packet.auth_token())) {
                ...
            }
        }
```

This is the bug that bit us on 0.6.4 auto-rejoin — without the whitelist entry, every `SYNC_SERVER_PICTURE_REQ` is dropped with "Missing or invalid JWT" before the handler runs.

- [ ] **Step 2: Add the `SYNC_SERVER_PICTURE_REQ` handler**

Find a stable insertion point — after the `MEMBERSHIP_REVOKE_REQ` handler is a natural neighbor (both are community-origin shared-secret syncs). Append:

```cpp
        // --- SYNC_SERVER_PICTURE_REQ ---
        // Community proxies an owner-uploaded picture to central.
        // Owner verification happened on the community side; central
        // verifies the shared secret and writes to community_servers.
        else if (packet.type() == chatproj::Packet::SYNC_SERVER_PICTURE_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped sync_server_picture - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.sync_server_picture_req();
            int server_id = auth_manager_.setServerPicture(
                req.host(), req.port(), req.data(), req.version());

            if (server_id == 0) {
                // No community_servers row yet (first heartbeat hasn't
                // landed). Silently drop — same edge case as
                // MEMBERSHIP_REGISTER_REQ before bootstrap.
                std::cout << "[Server] Dropped sync_server_picture - unknown community "
                          << req.host() << ":" << req.port() << "\n";
                return;
            }

            // Broadcast to every online session whose username is a
            // member of this server.
            auto members = auth_manager_.getServerMembers(server_id);
            chatproj::Packet bcast;
            bcast.set_type(chatproj::Packet::SERVER_PICTURE_CHANGED);
            auto* b = bcast.mutable_server_picture_changed();
            b->set_server_id(server_id);
            b->set_version(req.version());
            manager_.broadcast_to_users(bcast, members);

            std::cout << "[Server] Server picture updated for community " << server_id
                      << " (" << members.size() << " online members broadcast)\n";
        }
```

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/server/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(server,server-pictures): SYNC_SERVER_PICTURE_REQ handler + JWT-gate whitelist entry"
```

---

## Task 4: Central — `broadcast_to_users` helper on SessionManager

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Check if a similar helper already exists**

```bash
grep -nE "broadcast_to_friends|send_to_users|broadcast_to" C:/Users/sunkh/Desktop/decibell/decibell/src/server/main.cpp
```

If a multi-user broadcast helper already exists (e.g., `broadcast_to_friends`), reuse it directly in Task 3's handler instead of adding `broadcast_to_users`. Skip the rest of this task and continue to Task 5.

If no equivalent exists, continue with Step 2.

- [ ] **Step 2: Declare `broadcast_to_users` on `SessionManager`**

In `src/server/main.cpp`, find the `SessionManager` class (or the file's `Session` / `manager_` interface). Add the method declaration next to `send_private`:

```cpp
    /// Deliver `packet` to every online session whose username is in
    /// `usernames`. Used for SERVER_PICTURE_CHANGED broadcasts where
    /// the recipient set is the membership list of a specific community.
    void broadcast_to_users(const chatproj::Packet& packet,
                            const std::vector<std::string>& usernames);
```

- [ ] **Step 3: Implement `broadcast_to_users`**

Find an existing similar implementation (likely `send_private` or `broadcast_presence`) and mirror the pattern. The implementation:

```cpp
void SessionManager::broadcast_to_users(const chatproj::Packet& packet,
                                          const std::vector<std::string>& usernames) {
    if (usernames.empty()) return;
    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(
        chatproj::create_framed_packet(serialized));

    std::set<std::string> targets(usernames.begin(), usernames.end());
    std::lock_guard<std::mutex> lock(mutex_);
    for (const auto& session : sessions_) {
        if (!session) continue;
        if (targets.count(session->username()) == 0) continue;
        session->deliver(framed);
    }
}
```

`session->username()` and `session->deliver(...)` are existing Session methods (engineer should confirm exact names by grepping `Session::deliver` / `Session::username` — match whatever's there).

- [ ] **Step 4: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/server/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(server,server-pictures): SessionManager::broadcast_to_users helper for multi-user push"
```

(If Step 1 found an existing helper and you skipped Steps 2-3, skip this commit — no changes.)

---

## Task 5: Central — FETCH_SERVER_PICTURE_REQ handler

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Add the handler**

After the `SYNC_SERVER_PICTURE_REQ` handler from Task 3, append:

```cpp
        // --- FETCH_SERVER_PICTURE_REQ ---
        // Any authenticated user can fetch any server's picture by id.
        // Matches the public-fetch model of FETCH_AVATAR_REQ — server
        // pictures are visible to every member of a community anyway.
        else if (packet.type() == chatproj::Packet::FETCH_SERVER_PICTURE_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_server_picture_req();

            auto [version, data] = auth_manager_.getServerPicture(req.server_id());

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::FETCH_SERVER_PICTURE_RES);
            auto* res = rsp.mutable_fetch_server_picture_res();
            res->set_server_id(req.server_id());
            res->set_version(version);
            res->set_data(data);

            std::string serialized;
            rsp.SerializeToString(&serialized);
            auto framed = std::make_shared<std::vector<uint8_t>>(
                chatproj::create_framed_packet(serialized));
            deliver(framed);
        }
```

- [ ] **Step 2: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/server/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(server,server-pictures): FETCH_SERVER_PICTURE_REQ handler"
```

---

## Task 6: Community — sha256_hex helper

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Add the include if not already present**

Near the top of `src/community/main.cpp`, ensure `<openssl/sha.h>` is included. Grep:

```bash
grep -n "openssl/sha" C:/Users/sunkh/Desktop/decibell/decibell/src/community/main.cpp
```

If not present, add `#include <openssl/sha.h>` near the other OpenSSL/Boost includes at the top of the file.

- [ ] **Step 2: Add the helper**

In the anonymous namespace near other small helpers (e.g., near `send_to_central_blocking`), append:

```cpp
namespace {

/// Returns the lowercase sha256 hex digest of `data`.
std::string sha256_hex(const std::string& data) {
    unsigned char digest[SHA256_DIGEST_LENGTH];
    SHA256(reinterpret_cast<const unsigned char*>(data.data()),
           data.size(), digest);
    static const char kHex[] = "0123456789abcdef";
    std::string out;
    out.reserve(SHA256_DIGEST_LENGTH * 2);
    for (unsigned char b : digest) {
        out.push_back(kHex[b >> 4]);
        out.push_back(kHex[b & 0x0F]);
    }
    return out;
}

} // namespace
```

If the existing anonymous namespace at the top of `send_to_central_blocking` is the right place, append `sha256_hex` inside that namespace. Otherwise, create a small new anonymous namespace block near the top of the file.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/community/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(community,server-pictures): sha256_hex helper for picture version computation"
```

---

## Task 7: Community — UPDATE_SERVER_PICTURE_REQ handler

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Insert the handler near `CHANNEL_WIPE_REQ`**

Find the `CHANNEL_WIPE_REQ` handler block at around `src/community/main.cpp:818`. After its closing brace (around line 879), and before the `INVITE_CREATE_REQ` handler, add:

```cpp
        // --- UPDATE_SERVER_PICTURE_REQ ---
        // Owner-only. Verifies size + ownership locally, then
        // forwards to central via shared-secret one-shot TLS (same
        // pattern as sync_invite_register / sync_membership_register).
        else if (packet.type() == chatproj::Packet::UPDATE_SERVER_PICTURE_REQ) {
            auto* db = manager_.db();
            const auto& req = packet.update_server_picture_req();

            chatproj::Packet rsp;
            rsp.set_type(chatproj::Packet::UPDATE_SERVER_PICTURE_RES);
            auto* res = rsp.mutable_update_server_picture_res();

            if (!db) {
                res->set_success(false);
                res->set_message("Server misconfigured.");
                send_packet(rsp);
                return;
            }
            if (db->owner() != username_) {
                res->set_success(false);
                res->set_message("Only the server owner can change the server picture.");
                send_packet(rsp);
                return;
            }
            if (req.data().size() > 200 * 1024) {
                res->set_success(false);
                res->set_message("Image exceeds 200 KB.");
                send_packet(rsp);
                return;
            }

            std::string version = req.data().empty() ? "" : sha256_hex(req.data());
            res->set_success(true);
            res->set_message("");
            res->set_version(version);
            send_packet(rsp);

            // Forward to central — fire-and-forget over a one-shot TLS
            // connection (same pattern as sync_invite_register).
            chatproj::Packet pkt;
            pkt.set_type(chatproj::Packet::SYNC_SERVER_PICTURE_REQ);
            pkt.set_auth_token(central_jwt_secret_);
            auto* sync = pkt.mutable_sync_server_picture_req();
            sync->set_host(public_ip_);
            sync->set_port(community_port_);
            sync->set_data(req.data());
            sync->set_version(version);

            std::string serialized;
            pkt.SerializeToString(&serialized);
            auto framed = chatproj::create_framed_packet(serialized);

            std::string host = central_host_;
            int port = central_port_;
            std::thread([host, port, framed = std::move(framed)]() {
                send_to_central_blocking(host, port, framed);
            }).detach();

            std::cout << "[Community] server picture "
                      << (req.data().empty() ? "removed" : "updated")
                      << " by " << username_
                      << " (" << req.data().size() << " bytes)\n";
        }
```

If `central_jwt_secret_`, `public_ip_`, `community_port_`, `central_host_`, `central_port_` aren't accessible from this scope (they're member fields of `SessionManager` based on the auto-rejoin pattern), the handler runs inside a `Session` class method — it needs to access these through `manager_.<member>` or via a helper. Engineer should mirror exactly how `sync_invite_register` reaches these fields — grep `SessionManager::sync_invite_register` to see the pattern.

- [ ] **Step 2: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add src/community/main.cpp
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(community,server-pictures): UPDATE_SERVER_PICTURE_REQ handler with owner gate + size check + central forward"
```

---

**CHECKPOINT 1: Server side complete (Tasks 1-7).** Rebuild central + community on the Linux host. Restart both. Verify a baseline:

```sql
-- On central
\d community_servers
-- Expected: picture (bytea), picture_version (character varying)
```

---

## Task 8: Native — events.rs additions

**Files:**
- Modify: `electron-client/native/src/events.rs`

- [ ] **Step 1: Add three event-name constants**

Near the existing avatar-related constants (search for `AVATAR_CHANGED`), append:

```rust
// --- Custom server pictures ---
pub const SERVER_PICTURE_UPDATE_RESPONDED: &str = "server_picture_update_responded";
pub const SERVER_PICTURE_RECEIVED: &str = "server_picture_received";
pub const SERVER_PICTURE_CHANGED: &str = "server_picture_changed";
```

- [ ] **Step 2: Add the payload structs**

Near the existing avatar payload structs, append:

```rust
// --- Custom server pictures ---

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPictureUpdateRespondedPayload {
    pub success: bool,
    pub message: String,
    pub server_id: String,
    pub version: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPictureReceivedPayload {
    pub server_id: i32,
    pub version: String,
    /// Pre-encoded `data:image/...;base64,...` URL ready to drop into
    /// an <img src>. Empty string when the server has no picture set.
    pub data: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ServerPictureChangedPayload {
    pub server_id: i32,
    pub version: String,
}
```

- [ ] **Step 3: Add three emit helpers**

Near the existing emit helpers, append:

```rust
pub fn emit_server_picture_update_responded(payload: ServerPictureUpdateRespondedPayload) {
    send(SERVER_PICTURE_UPDATE_RESPONDED, payload);
}

pub fn emit_server_picture_received(payload: ServerPictureReceivedPayload) {
    send(SERVER_PICTURE_RECEIVED, payload);
}

pub fn emit_server_picture_changed(payload: ServerPictureChangedPayload) {
    send(SERVER_PICTURE_CHANGED, payload);
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
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,server-pictures): 3 new events + payload structs + emit helpers"
```

---

## Task 9: Native — base64 dependency + helper

**Files:**
- Modify: `electron-client/native/Cargo.toml`
- Modify: `electron-client/native/src/events.rs`

- [ ] **Step 1: Verify the `base64` crate is in Cargo.toml**

```bash
grep -n '^base64' C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native/Cargo.toml
```

If a line like `base64 = "0.21"` (or newer) is already present, skip Steps 2-3 and continue from Step 4.

- [ ] **Step 2: Add `base64` to `[dependencies]`**

In `electron-client/native/Cargo.toml`, under `[dependencies]`, add:

```toml
base64 = "0.22"
```

- [ ] **Step 3: Verify cargo check pulls the crate**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 4: Add a small `bytes_to_data_url` helper to events.rs**

At the bottom of `electron-client/native/src/events.rs`:

```rust
/// Convert raw image bytes into a data URL ready for an <img src>.
/// Sniffs JPEG/PNG magic bytes; falls back to image/jpeg.
/// Returns an empty string when `bytes` is empty.
pub fn bytes_to_data_url(bytes: &[u8]) -> String {
    use base64::engine::general_purpose::STANDARD;
    use base64::Engine as _;

    if bytes.is_empty() {
        return String::new();
    }
    let mime = if bytes.len() >= 4 && &bytes[..4] == [0x89, 0x50, 0x4E, 0x47] {
        "image/png"
    } else if bytes.len() >= 3 && &bytes[..3] == [0xFF, 0xD8, 0xFF] {
        "image/jpeg"
    } else {
        "image/jpeg" // best-effort fallback
    };
    let b64 = STANDARD.encode(bytes);
    format!("data:{};base64,{}", mime, b64)
}
```

The `&[0x89, 0x50, 0x4E, 0x47]` byte literal compares against the PNG magic bytes; `&[0xFF, 0xD8, 0xFF]` is the JPEG SOI marker.

- [ ] **Step 5: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 6: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/Cargo.toml electron-client/native/Cargo.lock electron-client/native/src/events.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,server-pictures): base64 crate + bytes_to_data_url helper"
```

(If Step 1 already found `base64` in Cargo.toml, omit it from the `git add`.)

---

## Task 10: Native — `update_server_picture` napi command

**Files:**
- Modify: `electron-client/native/src/commands/servers.rs`

- [ ] **Step 1: Append the command**

In `electron-client/native/src/commands/servers.rs`, after `request_drop_membership` (the auto-rejoin cleanup command), append:

```rust
#[napi(object)]
pub struct UpdateServerPictureArgs {
    pub server_id: String,
    pub data: napi::bindgen_prelude::Buffer,
}

/// Sends UPDATE_SERVER_PICTURE_REQ over the community session for
/// server_id. The ack arrives as the `server_picture_update_responded`
/// event; on success, central then broadcasts SERVER_PICTURE_CHANGED
/// to all online members (including the requester).
#[napi]
pub async fn update_server_picture(args: UpdateServerPictureArgs) -> napi::Result<()> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, UpdateServerPictureReq};

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
        let pkt = build_packet(
            packet::Type::UpdateServerPictureReq,
            packet::Payload::UpdateServerPictureReq(UpdateServerPictureReq {
                data: args.data.to_vec(),
            }),
            Some(&community.jwt),
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

If `community.jwt` field name differs, grep `CommunityClient` struct definition in `electron-client/native/src/net/community.rs` and use the actual field name (matches `delete_channel_message`'s pattern).

- [ ] **Step 2: Verify cargo check**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check 2>&1 | tail -10
```

Expected: `Finished dev profile`.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/commands/servers.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,server-pictures): update_server_picture napi command"
```

---

## Task 11: Native — `fetch_server_picture` napi command

**Files:**
- Modify: `electron-client/native/src/commands/servers.rs`

- [ ] **Step 1: Append the command**

After `update_server_picture` from Task 10, append:

```rust
#[napi(object)]
pub struct FetchServerPictureArgs {
    pub server_id: i32,
}

/// Sends FETCH_SERVER_PICTURE_REQ over the JWT-authed central
/// session. Response lands as the `server_picture_received` event,
/// with `data` already encoded as a data URL ready for <img src>.
#[napi]
pub async fn fetch_server_picture(args: FetchServerPictureArgs) -> napi::Result<()> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, FetchServerPictureReq};

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
            packet::Type::FetchServerPictureReq,
            packet::Payload::FetchServerPictureReq(FetchServerPictureReq {
                server_id: args.server_id,
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
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/commands/servers.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,server-pictures): fetch_server_picture napi command"
```

---

## Task 12: Native — route FETCH_SERVER_PICTURE_RES + SERVER_PICTURE_CHANGED in central.rs

**Files:**
- Modify: `electron-client/native/src/net/central.rs`

- [ ] **Step 1: Find the route_packets match block**

Grep for `Some(packet::Payload::DmDeleteRes` to find the recent message-delete arms — the new arms go alongside in the same `match packet.payload` body.

- [ ] **Step 2: Add the two new arms**

After the message-delete arms (or anywhere in the central match block, alongside other client-bound responses), insert:

```rust
                Some(packet::Payload::FetchServerPictureRes(resp)) => {
                    events::emit_server_picture_received(
                        events::ServerPictureReceivedPayload {
                            server_id: resp.server_id,
                            version: resp.version,
                            data: events::bytes_to_data_url(&resp.data),
                        },
                    );
                }
                Some(packet::Payload::ServerPictureChanged(b)) => {
                    events::emit_server_picture_changed(
                        events::ServerPictureChangedPayload {
                            server_id: b.server_id,
                            version: b.version,
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
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/src/net/central.rs
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,server-pictures): route FETCH_SERVER_PICTURE_RES + SERVER_PICTURE_CHANGED to renderer events"
```

---

## Task 13: Native — route UPDATE_SERVER_PICTURE_RES in community.rs

**Files:**
- Modify: `electron-client/native/src/net/community.rs`

- [ ] **Step 1: Find the route_packets match block**

In `electron-client/native/src/net/community.rs`, the recently-added arms include `MessageDeleteRes` / `ChannelMessageDeleted`. Server_id is captured into the closure as `server_id: String` — use it directly.

- [ ] **Step 2: Add the new arm**

After the message-delete arms (or alongside any community-bound response), insert:

```rust
                Some(packet::Payload::UpdateServerPictureRes(resp)) => {
                    events::emit_server_picture_update_responded(
                        events::ServerPictureUpdateRespondedPayload {
                            success: resp.success,
                            message: resp.message,
                            server_id: server_id.clone(),
                            version: resp.version,
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
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(native,server-pictures): route UPDATE_SERVER_PICTURE_RES to renderer event"
```

---

## Task 14: Native — addon rebuild + tsc verification

**Files:** none (verification only).

- [ ] **Step 1: Rebuild the addon**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; npm run build 2>&1 | tail -10
```

Expected: `Finished release profile`. `index.d.ts` regenerates with `updateServerPicture` and `fetchServerPicture` exports.

- [ ] **Step 2: Verify exports**

```bash
grep -E "(updateServerPicture|fetchServerPicture)" C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native/index.d.ts
```

Expected: 2 lines of `export declare function ...`.

- [ ] **Step 3: Verify renderer tsc still passes**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 4: Commit regenerated bindings**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/native/index.d.ts electron-client/native/index.js
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "build(native,server-pictures): regenerate index.d.ts with new commands"
```

---

**CHECKPOINT 2: Native chain complete (Tasks 8-14).**

---

## Task 15: chatStore — `serverPictureVersions` + `serverPictures` + actions

**Files:**
- Modify: `electron-client/src/stores/chatStore.ts`

- [ ] **Step 1: Add field declarations to the ChatState interface**

In `electron-client/src/stores/chatStore.ts`, near `pendingMembershipServerIds`, add:

```ts
  /// Per-server sha256-hex picture version. '' = no picture set.
  /// Populated from CommunityServerInfo payloads (server_list_received,
  /// memberships_received) and from server_picture_changed events.
  serverPictureVersions: Record<string, string>;
  /// Per-server cached image as a data URL. Populated lazily by the
  /// fetch effect when a tile sees a non-empty version with no
  /// cached bytes.
  serverPictures: Record<string, string>;
```

- [ ] **Step 2: Add action declarations**

In the actions section of `ChatState`, add:

```ts
  /// Set the picture version for a server. If the new version
  /// differs from the cached one, clears `serverPictures[serverId]`
  /// so the next tile render lazy-fetches fresh bytes. Idempotent.
  setServerPictureVersion: (serverId: string, version: string) => void;
  /// Cache fetched image bytes (data URL) for a server. Guarded:
  /// only writes if the fetch's version still matches the current
  /// serverPictureVersions[serverId] — a stale fetch landing after
  /// a newer version-changed event is dropped silently.
  setServerPictureData: (serverId: string, version: string, dataUrl: string) => void;
```

- [ ] **Step 3: Add initial values + reset on logout**

In the `create<...>()` factory body, near `pendingMembershipServerIds: new Set()`:

```ts
  serverPictureVersions: {},
  serverPictures: {},
```

In `resetForLogout`, add to the `set({...})`:

```ts
      serverPictureVersions: {},
      serverPictures: {},
```

- [ ] **Step 4: Add the action implementations**

Near the existing `setPendingMemberships` / `removePendingMembership` actions:

```ts
  setServerPictureVersion: (serverId, version) =>
    set((state) => {
      const current = state.serverPictureVersions[serverId] ?? "";
      if (current === version) return {};
      const nextVersions = {
        ...state.serverPictureVersions,
        [serverId]: version,
      };
      // Version changed → invalidate cached bytes for this server.
      const nextPictures = { ...state.serverPictures };
      delete nextPictures[serverId];
      return {
        serverPictureVersions: nextVersions,
        serverPictures: nextPictures,
      };
    }),

  setServerPictureData: (serverId, version, dataUrl) =>
    set((state) => {
      const current = state.serverPictureVersions[serverId] ?? "";
      // Drop fetches whose version is no longer current — a newer
      // server_picture_changed event invalidated this fetch
      // before it returned.
      if (current !== version) return {};
      return {
        serverPictures: { ...state.serverPictures, [serverId]: dataUrl },
      };
    }),
```

- [ ] **Step 5: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/stores/chatStore.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(chat-store,server-pictures): serverPictureVersions + serverPictures + version-guarded actions"
```

---

## Task 16: `useCanEditServerSettings` hook

**Files:**
- Create: `electron-client/src/features/servers/useCanEditServerSettings.ts`

- [ ] **Step 1: Write the hook**

```ts
import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";

/// Returns true if the local user is allowed to edit server-wide
/// settings for the given server. Today: owner-only. When roles
/// ship, this extends to:
///   || hasRolePermission(serverId, "EDIT_SERVER_SETTINGS").
///
/// Returns false if serverId is null (user is on the home or DM
/// view, not viewing a specific server).
export function useCanEditServerSettings(serverId: string | null): boolean {
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
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -5
```

Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/useCanEditServerSettings.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(perms,server-pictures): useCanEditServerSettings hook (owner-only today)"
```

---

## Task 17: `useServerEvents` — 3 new listeners + extend existing for picture_version

**Files:**
- Modify: `electron-client/src/features/servers/useServerEvents.ts`

- [ ] **Step 1: Extend the `memberships_received` listener**

Find the existing `memberships_received` listener (added during auto-rejoin). Inside the mapping function, propagate the new `pictureVersion` field into chatStore:

```ts
    const unlistenMemberships = listen<{
      memberships: Array<{
        id: number;
        name: string;
        description: string;
        hostIp: string;
        port: number;
        memberCount: number;
        pictureVersion: string;
      }>;
    }>("memberships_received", (event) => {
      const servers: CommunityServer[] = event.payload.memberships.map((s) => ({
        id: String(s.id),
        name: s.name,
        description: s.description,
        hostIp: s.hostIp,
        port: s.port,
        memberCount: s.memberCount,
      }));
      const chat = useChatStore.getState();
      chat.mergeServers(servers);
      chat.setPendingMemberships(servers.map((s) => s.id));
      // Propagate picture_version so the ServerBar tile knows up-front
      // whether to lazy-fetch a picture.
      for (const m of event.payload.memberships) {
        chat.setServerPictureVersion(String(m.id), m.pictureVersion ?? "");
      }
    });
```

- [ ] **Step 2: Extend the `server_list_received` listener**

Similarly, find the `server_list_received` listener (used for ServerBrowseView). Add the same propagation after the existing handling:

```ts
    const unlistenServerList = listen<{
      servers: Array<{
        id: number;
        name: string;
        description: string;
        hostIp: string;
        port: number;
        memberCount: number;
        pictureVersion: string;
      }>;
    }>("server_list_received", (event) => {
      const chat = useChatStore.getState();
      // ... existing population of servers list ...
      for (const s of event.payload.servers) {
        chat.setServerPictureVersion(String(s.id), s.pictureVersion ?? "");
      }
    });
```

The exact existing code around `server_list_received` differs — engineer should preserve any other state updates that listener was doing and just add the picture-version loop.

- [ ] **Step 3: Add 3 new listeners**

After the channel-delete listeners, append:

```ts
    const unlistenServerPictureUpdateRes = listen<{
      success: boolean;
      message: string;
      serverId: string;
      version: string;
    }>("server_picture_update_responded", (event) => {
      const p = event.payload;
      if (!p.success) {
        toast.error("Couldn't update server picture", p.message);
        return;
      }
      // Success: the broadcast (server_picture_changed) will update
      // the version + invalidate cached bytes, triggering a lazy
      // fetch on the next tile render. Modal closes implicitly.
    });

    const unlistenServerPictureChanged = listen<{
      serverId: number;
      version: string;
    }>("server_picture_changed", (event) => {
      const { serverId, version } = event.payload;
      useChatStore.getState().setServerPictureVersion(String(serverId), version);
    });

    const unlistenServerPictureReceived = listen<{
      serverId: number;
      version: string;
      data: string;
    }>("server_picture_received", (event) => {
      const { serverId, version, data } = event.payload;
      // Empty data means the server has no picture set (or unknown
      // server_id). setServerPictureData drops mismatched versions.
      if (!data) return;
      useChatStore
        .getState()
        .setServerPictureData(String(serverId), version, data);
    });
```

- [ ] **Step 4: Wire into the cleanup return**

In the existing `return () => { ... }` block at the bottom of the `useEffect`:

```ts
      unlistenServerPictureUpdateRes.then((fn) => fn());
      unlistenServerPictureChanged.then((fn) => fn());
      unlistenServerPictureReceived.then((fn) => fn());
```

- [ ] **Step 5: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 6: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/useServerEvents.ts
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(events,server-pictures): 3 listeners + picture_version propagation from existing CommunityServerInfo payloads"
```

---

## Task 18: `ServerSettingsModal` component

**Files:**
- Create: `electron-client/src/features/servers/ServerSettingsModal.tsx`

- [ ] **Step 1: Write the modal**

```tsx
import { createPortal } from "react-dom";
import { useEffect, useState, useCallback, useRef } from "react";
import { invoke } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { toast } from "../../stores/toastStore";
import { stringToGradient } from "../../utils/colors";

interface Props {
  serverId: string;
}

const TABS = [
  {
    id: "overview",
    label: "Overview",
    icon: (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
        <rect x="3" y="3" width="18" height="18" rx="2" />
        <path d="M3 9h18M9 3v18" />
      </svg>
    ),
  },
];

const MAX_BYTES = 200 * 1024;

/// Mirrors SettingsModal chrome 1:1 (820x560, tabbed sidebar, fade-in
/// scale-95->1, Esc closes, backdrop click closes, portal to body).
/// v1 has one Overview tab containing server-picture management.
export default function ServerSettingsModal({ serverId }: Props) {
  const isOpen = useUiStore((s) => s.activeModal === "server-settings");
  const closeModal = useUiStore((s) => s.closeModal);
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [activeTab, setActiveTab] = useState<string>("overview");

  const server = useChatStore((s) => s.servers.find((x) => x.id === serverId));
  const pictureVersion = useChatStore(
    (s) => s.serverPictureVersions[serverId] ?? "",
  );
  const pictureDataUrl = useChatStore((s) => s.serverPictures[serverId]);
  const hasPicture = pictureVersion !== "";

  useEffect(() => {
    if (isOpen) {
      setMounted(true);
      requestAnimationFrame(() => setVisible(true));
    } else {
      setVisible(false);
    }
  }, [isOpen]);

  const handleTransitionEnd = useCallback(() => {
    if (!visible) setMounted(false);
  }, [visible]);

  useEffect(() => {
    if (!mounted) return;
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") closeModal();
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
  }, [mounted, closeModal]);

  const fileInputRef = useRef<HTMLInputElement>(null);

  const onUploadClick = () => {
    fileInputRef.current?.click();
  };

  const onFileSelected = async (e: React.ChangeEvent<HTMLInputElement>) => {
    const file = e.target.files?.[0];
    e.target.value = "";  // allow re-select of the same file
    if (!file) return;
    if (file.size > MAX_BYTES) {
      toast.error("Image too large", "Maximum size is 200 KB.");
      return;
    }
    // Sniff JPEG/PNG magic bytes.
    const head = new Uint8Array(await file.slice(0, 4).arrayBuffer());
    const isPng = head[0] === 0x89 && head[1] === 0x50 && head[2] === 0x4E && head[3] === 0x47;
    const isJpeg = head[0] === 0xFF && head[1] === 0xD8 && head[2] === 0xFF;
    if (!isPng && !isJpeg) {
      toast.error("Unsupported format", "Only JPEG and PNG are supported.");
      return;
    }
    const buf = new Uint8Array(await file.arrayBuffer());
    invoke("update_server_picture", { serverId, data: Buffer.from(buf) }).catch(
      (err) => {
        console.error("update_server_picture:", err);
        toast.error("Failed to upload", "Please try again.");
      },
    );
  };

  const onRemove = () => {
    if (!window.confirm("Remove the server picture? The default gradient and letter will be used instead.")) return;
    invoke("update_server_picture", { serverId, data: Buffer.alloc(0) }).catch(
      (err) => {
        console.error("update_server_picture:", err);
        toast.error("Failed to remove", "Please try again.");
      },
    );
  };

  if (!mounted || !server) return null;

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={closeModal}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="flex h-[560px] w-[820px] overflow-hidden rounded-2xl border border-border bg-bg-dark shadow-[0_24px_80px_rgba(0,0,0,0.6),0_0_0_1px_rgba(255,255,255,0.02)] transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        {/* Sidebar */}
        <div className="flex w-[210px] shrink-0 flex-col gap-0.5 border-r border-border-divider bg-bg-darkest px-3 py-6">
          <div className="mb-2 px-3 text-[10.5px] font-semibold uppercase tracking-[0.08em] text-text-muted">
            Server settings
          </div>
          {TABS.map((tab) => (
            <button
              key={tab.id}
              onClick={() => setActiveTab(tab.id)}
              className={`flex items-center gap-2.5 rounded-[10px] px-3 py-[9px] text-[14px] transition-colors ${
                activeTab === tab.id
                  ? "bg-accent-soft font-medium text-text-primary"
                  : "font-normal text-text-secondary hover:bg-surface-hover hover:text-text-primary"
              }`}
            >
              <span className={activeTab === tab.id ? "text-accent-bright" : "text-text-muted"}>
                {tab.icon}
              </span>
              {tab.label}
            </button>
          ))}
        </div>

        {/* Content */}
        <div className="flex flex-1 flex-col overflow-y-auto scrollbar-thin">
          <div className="flex items-center justify-between px-8 pt-7 pb-5">
            <h2 className="font-display text-xl font-semibold text-text-primary">
              Overview
            </h2>
            <button
              onClick={closeModal}
              className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>
          <div className="flex flex-col gap-4 px-8 pb-7">
            <h3 className="text-[14px] font-semibold text-text-primary">
              Server picture
            </h3>
            <p className="text-[13px] text-text-secondary">
              Shown in the server bar in place of the default gradient and letter.
              Square images work best; JPEG or PNG, max 200 KB.
            </p>
            <div className="flex items-center gap-6">
              {hasPicture && pictureDataUrl ? (
                <img
                  src={pictureDataUrl}
                  alt={server.name}
                  className="h-[120px] w-[120px] rounded-xl object-cover"
                />
              ) : (
                <div
                  className="flex h-[120px] w-[120px] items-center justify-center rounded-xl text-[44px] font-bold text-white"
                  style={{ background: stringToGradient(server.name) }}
                >
                  {server.name.charAt(0).toUpperCase()}
                </div>
              )}
              <div className="flex flex-col gap-2">
                <button
                  onClick={onUploadClick}
                  className="rounded-lg bg-accent px-4 py-2 text-[13px] font-semibold text-white transition-colors hover:bg-accent-hover"
                >
                  Upload picture
                </button>
                {hasPicture && (
                  <button
                    onClick={onRemove}
                    className="rounded-lg border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
                  >
                    Remove picture
                  </button>
                )}
                <input
                  ref={fileInputRef}
                  type="file"
                  accept="image/jpeg,image/png"
                  className="hidden"
                  onChange={onFileSelected}
                />
              </div>
            </div>
          </div>
        </div>
      </div>
    </div>,
    document.body,
  );
}
```

- [ ] **Step 2: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 3: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/ServerSettingsModal.tsx
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(ui,server-pictures): ServerSettingsModal mirroring SettingsModal chrome (Overview tab with picture management)"
```

---

## Task 19: `ServerActionsDropdown` — add Server Settings entry + mount modal

**Files:**
- Modify: `electron-client/src/features/servers/ServerActionsDropdown.tsx`
- Modify: `electron-client/src/features/channels/ServerChannelsSidebar.tsx`

- [ ] **Step 1: Read the existing dropdown structure**

```bash
grep -n "onInvites\|onChannelSettings\|onDisconnect" C:/Users/sunkh/Desktop/decibell/decibell/electron-client/src/features/servers/ServerActionsDropdown.tsx
```

The dropdown takes callbacks like `onInvites`, `onChannelSettings`, `onDisconnect`. Add a new optional `onServerSettings?: () => void` prop with matching styling.

- [ ] **Step 2: Add the new prop + entry**

In `ServerActionsDropdown.tsx`, extend the props interface:

```tsx
interface ServerActionsDropdownProps {
  // ... existing props ...
  /// Only present when the local user can edit server settings.
  onServerSettings?: () => void;
}
```

And in the JSX, add a new button entry between Invites and the divider/Disconnect (or wherever feels natural):

```tsx
{onServerSettings && (
  <button
    onClick={onServerSettings}
    className="flex w-full items-center gap-2.5 rounded-md px-2.5 py-[9px] text-[13px] text-text-primary transition-colors hover:bg-surface-hover"
  >
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="shrink-0 text-text-muted">
      <circle cx="12" cy="12" r="3" />
      <path d="M19.4 15a1.65 1.65 0 0 0 .33 1.82l.06.06a2 2 0 0 1 0 2.83 2 2 0 0 1-2.83 0l-.06-.06a1.65 1.65 0 0 0-1.82-.33 1.65 1.65 0 0 0-1 1.51V21a2 2 0 0 1-2 2 2 2 0 0 1-2-2v-.09A1.65 1.65 0 0 0 9 19.4a1.65 1.65 0 0 0-1.82.33l-.06.06a2 2 0 0 1-2.83 0 2 2 0 0 1 0-2.83l.06-.06a1.65 1.65 0 0 0 .33-1.82 1.65 1.65 0 0 0-1.51-1H3a2 2 0 0 1-2-2 2 2 0 0 1 2-2h.09A1.65 1.65 0 0 0 4.6 9a1.65 1.65 0 0 0-.33-1.82l-.06-.06a2 2 0 0 1 0-2.83 2 2 0 0 1 2.83 0l.06.06a1.65 1.65 0 0 0 1.82.33H9a1.65 1.65 0 0 0 1-1.51V3a2 2 0 0 1 2-2 2 2 0 0 1 2 2v.09a1.65 1.65 0 0 0 1 1.51 1.65 1.65 0 0 0 1.82-.33l.06-.06a2 2 0 0 1 2.83 0 2 2 0 0 1 0 2.83l-.06.06a1.65 1.65 0 0 0-.33 1.82V9a1.65 1.65 0 0 0 1.51 1H21a2 2 0 0 1 2 2 2 2 0 0 1-2 2h-.09a1.65 1.65 0 0 0-1.51 1z" />
    </svg>
    Server Settings
  </button>
)}
```

- [ ] **Step 3: Wire the entry in `ServerChannelsSidebar.tsx`**

In `electron-client/src/features/channels/ServerChannelsSidebar.tsx`, where the dropdown is rendered, import the new hook and add the conditional prop:

```tsx
import { useCanEditServerSettings } from "../servers/useCanEditServerSettings";
import ServerSettingsModal from "../servers/ServerSettingsModal";
import { useUiStore } from "../../stores/uiStore";

// Inside the component body:
const canEditServerSettings = useCanEditServerSettings(activeServerId);
const openModal = useUiStore((s) => s.openModal);
const activeModal = useUiStore((s) => s.activeModal);
```

Pass `onServerSettings` to the dropdown only when allowed:

```tsx
<ServerActionsDropdown
  // ... existing callbacks ...
  onServerSettings={
    canEditServerSettings
      ? () => {
          setShowServerMenu(false);
          openModal("server-settings");
        }
      : undefined
  }
/>
```

And mount the modal at the bottom of the component's render (or wherever existing modals are mounted):

```tsx
{activeModal === "server-settings" && activeServerId && (
  <ServerSettingsModal serverId={activeServerId} />
)}
```

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/ServerActionsDropdown.tsx electron-client/src/features/channels/ServerChannelsSidebar.tsx
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(ui,server-pictures): ServerActionsDropdown Server Settings entry (owner-only) + mount modal"
```

---

## Task 20: `ServerBar` — tile rendering with picture branch + lazy fetch

**Files:**
- Modify: `electron-client/src/features/servers/ServerBar.tsx`

- [ ] **Step 1: Read the existing tile rendering to keep the no-picture branch intact**

The current code has a `{visible.map((server) => { ... })}` block rendering each tile. We're adding a branch keyed off `pictureVersion`.

- [ ] **Step 2: Add the placeholder + lazy-fetch infrastructure**

At the top of `electron-client/src/features/servers/ServerBar.tsx`, after imports, add:

```tsx
import { useEffect } from "react";

/// Tiny 1x1 transparent PNG. Used as a placeholder while the picture
/// bytes are in-flight so the <img> tag doesn't show a broken-image
/// icon.
const PLACEHOLDER_DATA_URL =
  "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAQAAAC1HAwCAAAAC0lEQVR42mNkYAAAAAYAAjCB0C8AAAAASUVORK5CYII=";

/// Module-level dedupe set for in-flight picture fetches.
/// Keyed by "<serverId>:<version>" so a new version triggers a fresh
/// fetch even if the previous one is still pending.
const inflightFetches = new Set<string>();

function useFetchServerPictureIfMissing(
  serverId: string,
  version: string,
  cachedDataUrl: string | undefined,
) {
  useEffect(() => {
    if (!version || cachedDataUrl) return;
    const key = `${serverId}:${version}`;
    if (inflightFetches.has(key)) return;
    inflightFetches.add(key);
    invoke("fetch_server_picture", { serverId: parseInt(serverId, 10) })
      .catch(console.error)
      .finally(() => inflightFetches.delete(key));
  }, [serverId, version, cachedDataUrl]);
}
```

`invoke` should already be imported at the top of the file from the existing handleDisconnect / handlers — if not, add `import { invoke } from "../../lib/ipc";`.

- [ ] **Step 3: Extract the tile rendering into a child component (avoids the hook-in-loop lint)**

Replace the existing `visible.map((server) => { ... return <button …> … </button>; })` with:

```tsx
{visible.map((server) => (
  <ServerTile
    key={server.id}
    server={server}
    isActive={activeServerId === server.id}
    isPending={pendingMembershipServerIds.has(server.id)}
    onClick={handleServerClick}
  />
))}
```

And add the `ServerTile` component below the main `ServerBar` export:

```tsx
interface ServerTileProps {
  server: CommunityServer;
  isActive: boolean;
  isPending: boolean;
  onClick: (serverId: string) => void;
}

function ServerTile({ server, isActive, isPending, onClick }: ServerTileProps) {
  const pictureVersion = useChatStore(
    (s) => s.serverPictureVersions[server.id] ?? "",
  );
  const pictureDataUrl = useChatStore((s) => s.serverPictures[server.id]);
  const hasPicture = pictureVersion !== "";

  useFetchServerPictureIfMissing(server.id, pictureVersion, pictureDataUrl);

  if (!hasPicture) {
    return (
      <button
        onClick={() => !isPending && onClick(server.id)}
        disabled={isPending}
        title={isPending ? "Connecting…" : undefined}
        className={`relative flex h-[38px] shrink-0 items-center gap-2 rounded-lg px-3.5 text-[13px] font-semibold transition-all duration-200 ${
          isPending
            ? "cursor-wait bg-surface-hover text-text-muted opacity-60"
            : isActive
              ? "bg-accent-mid text-accent-bright shadow-[0_2px_12px_rgba(56,143,255,0.10)]"
              : "text-text-secondary hover:bg-surface-hover hover:text-text-primary hover:-translate-y-px"
        }`}
      >
        {!isPending && isActive && (
          <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
        )}
        <div
          className="flex h-5 w-5 items-center justify-center rounded-[5px] text-[11px] font-semibold text-white"
          style={{ background: stringToGradient(server.name) }}
        >
          {server.name.charAt(0).toUpperCase()}
        </div>
        <span className="max-w-[100px] truncate">{server.name}</span>
      </button>
    );
  }

  // Picture branch.
  return (
    <button
      onClick={() => !isPending && onClick(server.id)}
      disabled={isPending}
      title={isPending ? "Connecting…" : server.name}
      className={`relative flex h-[38px] shrink-0 items-center justify-center overflow-hidden rounded-lg transition-all duration-200 ${
        isPending
          ? "cursor-wait opacity-60"
          : isActive
            ? "shadow-[0_2px_12px_rgba(56,143,255,0.10)]"
            : "hover:-translate-y-px"
      }`}
      style={{
        // Match the no-picture branch's variable width.
        width: "auto",
        minWidth: "60px",
        paddingLeft: "12px",
        paddingRight: "12px",
      }}
    >
      <img
        src={pictureDataUrl ?? PLACEHOLDER_DATA_URL}
        alt={server.name}
        className="absolute inset-0 h-full w-full object-cover"
      />
      {!isActive && (
        <>
          <div className="absolute inset-0 bg-black/45" />
          <span className="relative max-w-[100px] truncate text-[13px] font-semibold text-white">
            {server.name}
          </span>
        </>
      )}
      {!isPending && isActive && (
        <div className="absolute -bottom-[9px] left-1/2 h-[3px] w-5 -translate-x-1/2 rounded-t bg-accent" />
      )}
    </button>
  );
}
```

The component extraction keeps the hooks (`useChatStore`, `useFetchServerPictureIfMissing`) at the top of a stable function instead of inside the `.map(...)` callback, satisfying React's rules-of-hooks lint.

- [ ] **Step 4: Verify tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit 2>&1 | tail -10
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git -C C:/Users/sunkh/Desktop/decibell/decibell add electron-client/src/features/servers/ServerBar.tsx
git -C C:/Users/sunkh/Desktop/decibell/decibell commit -m "feat(ui,server-pictures): ServerBar tile renders image-with-overlay branch when pictureVersion is set"
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

Confirm the app starts without console errors. Hard-Ctrl-C to stop.

If you get "Could not resolve …/twemoji-data.json", run `npm run build:twemoji` first.

---

**CHECKPOINT 3: Renderer complete (Tasks 15-21).**

---

## Task 22: End-to-end manual test pass

**Files:** none — manual verification only.

Rebuild + redeploy central + community on Linux host. Restart both. Reload dev client.

- [ ] **Step 1: Schema migration verified**

On central:
```sql
\d community_servers
```
Expected output includes `picture | bytea` and `picture_version | character varying(64)`.

- [ ] **Step 2: Server Settings dropdown entry — owner only**

Connect to a community server as the owner. Click the server dropdown (ServerChannelsSidebar). Expected: a "Server Settings" entry appears between the other items. Connect as a non-owner: no "Server Settings" entry.

- [ ] **Step 3: Upload flow**

As owner: open Server Settings → Overview tab → click Upload → pick a JPEG ≤200 KB. Expected:
- File picker opens with image/jpeg + image/png filter
- After selection: native command fires, broadcast lands within ~500ms
- Tile in ServerBar transitions from gradient-letter to image-with-overlay
- Modal stays open (no toast)
- Confirm in central Postgres:
  ```sql
  SELECT id, host_ip, port, octet_length(picture) AS bytes, picture_version
  FROM community_servers WHERE id = <server_id>;
  ```
  Expected: bytes > 0, picture_version is a 64-char hex string.

- [ ] **Step 4: Cross-device broadcast**

User A is the owner; B is a member. Both connected. A uploads. B's ServerBar tile updates within ~500ms with no refresh.

- [ ] **Step 5: Permission denied — non-owner forged packet**

As non-owner, in devtools console:
```js
await window.decibell.updateServerPicture({
  serverId: "<server_id>",
  data: Buffer.from([0xFF, 0xD8, 0xFF, 0xE0]),
});
```
Expected: `toast.error("Couldn't update server picture", "Only the server owner...")`. The server-side handler rejects with the 403 message.

- [ ] **Step 6: Size limit**

As owner, try uploading a 300 KB JPEG. Expected: `toast.error("Image too large", "Maximum size is 200 KB.")` — caught client-side before invoke fires. (Defense-in-depth: also confirm the community rejects oversized data if you bypass the client check via devtools.)

- [ ] **Step 7: Wrong format**

As owner, try uploading a GIF or BMP. Expected: `toast.error("Unsupported format", "Only JPEG and PNG are supported.")` — caught client-side by magic-byte sniff.

- [ ] **Step 8: Removal**

As owner: Server Settings → Remove picture → confirm dialog → modal stays open after. Tile reverts to gradient-letter. Central DB: `picture IS NULL AND picture_version = ''`. B's tile also reverts via broadcast.

- [ ] **Step 9: Auto-rejoin tile pre-load**

Log out. Log back in (auto-rejoin path). Expected: ServerBar pending tiles immediately render the image-with-overlay (NOT just gradient-letter) because `LoginResponse.memberships` carries `picture_version` and the lazy-fetch fires on first tile render.

- [ ] **Step 10: Tile active/inactive treatment**

Click a server with a custom picture: tile shows just the image (no overlay, no name on top), active underline visible. Click another server: previous tile gains the dim overlay + name in white. Click home / DM view: same dim + name treatment. Click voice / stream within that server: still treated as active (the server is the active context).

- [ ] **Step 11: Stale-fetch race**

Open devtools network tab. As owner, upload a picture. Quickly upload a SECOND picture before the first FETCH_SERVER_PICTURE_RES lands (~1 sec window). Expected: the tile ends up showing the SECOND picture, not the first — `setServerPictureData`'s version guard discards the stale fetch. No flicker, no flash of the first picture.

- [ ] **Step 12: Regression sweep**

- ServerBrowseView (if it shows pictures, otherwise skip): tiles render correctly
- Auto-rejoin with no servers configured: empty ServerBar, no console errors
- DMs unaffected
- Channel sending + history + delete still work
- Login/logout/login still works; serverPictureVersions clears on logout
- Browser refresh: tiles re-fetch their pictures cleanly

If anything regresses, return to the relevant task. Otherwise the plan is complete — version bump (0.6.5 or whatever's next) is a separate ship-time step.

---

## Self-review notes

**Spec coverage:**
- §UX tile rendering → Task 20
- §UX modal → Task 18
- §UX dropdown entry → Task 19
- §Permissions community gate → Task 7
- §Permissions renderer hook → Task 16
- §Permissions JWT-gate whitelist → Task 3 Step 1
- §Storage schema → Task 2 Step 2
- §Wire protocol → Task 1
- §CommunityServerInfo.picture_version → Task 1 Step 4 + Task 2 Steps 6/7
- §Server-side processing (community handler) → Task 7
- §Server-side processing (central sync handler) → Task 3
- §Server-side processing (central fetch handler) → Task 5
- §Server-side processing (central broadcast helper) → Task 4
- §AuthManager new methods → Task 2 Steps 3-5
- §Native commands → Tasks 10, 11
- §Native events → Tasks 8, 12, 13
- §Native base64 helper → Task 9
- §Renderer stores → Task 15
- §Renderer event wire-up → Task 17
- §Renderer modal → Task 18
- §Renderer ServerBar → Task 20
- §Error handling matrix → exercised in Task 22 Steps 5-7, 11

**Placeholder scan:** every code step has actual code. The "engineer should grep for X to align" hints are intentional integration-detail callouts, not silent TODOs.

**Type consistency:**
- `UpdateServerPictureReq` / `Res`, `SyncServerPictureReq`, `FetchServerPictureReq` / `Res`, `ServerPictureChanged` proto names match across Task 1 → Tasks 3, 5, 7 (handlers) → Tasks 10, 11 (commands) → Tasks 12, 13 (routing).
- `setServerPicture` / `getServerPicture` / `getServerMembers` AuthManager methods match across Task 2 (decl + impl) → Tasks 3, 5 (callers).
- `serverPictureVersions` / `serverPictures` / `setServerPictureVersion` / `setServerPictureData` chatStore actions match across Task 15 (definition) → Tasks 17, 18, 20 (consumption).
- `useCanEditServerSettings` hook signature matches across Task 16 (definition) → Task 19 (caller).
- Event names (`server_picture_update_responded`, `server_picture_changed`, `server_picture_received`) match across Task 8 (constants) → Tasks 12, 13 (emit) → Task 17 (listen).
- `update_server_picture` / `fetch_server_picture` napi commands match across Tasks 10, 11 (definition) → Tasks 18, 20 (invoke sites).
- `bytes_to_data_url` helper matches between Task 9 (definition) and Task 12 (caller).
