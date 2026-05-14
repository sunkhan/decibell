# Auto-Rejoin Community Servers — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Auto-populate the server bar with every community server the user is a member of on login, with central as the source of truth for membership.

**Architecture:** Central gets a new `user_communities` table. Community servers push membership changes to central via two new shared-secret packets (`MEMBERSHIP_REGISTER_REQ` on every successful auth, `MEMBERSHIP_REVOKE_REQ` consolidated into `force_disconnect`). The community needs to know its own `server_id` (central's `community_servers.id`) to populate those packets — so we extend the existing `SERVER_HEARTBEAT` into a request/response with a new `SERVER_HEARTBEAT_RES` carrying the assigned id. Community caches the id (in memory + persisted) and uses it in every subsequent membership packet. Central's `LoginResponse` carries the full membership list inline; the native client auto-fires `connect_to_community` for each entry. Stale memberships get cleaned up by a new JWT-authed client-side `request_drop_membership` command. The existing UI "Disconnect" affordances are rewired to `leave_server` (with confirmation) since transient-close is no longer meaningful under auto-rejoin.

**Tech Stack:** Protobuf 3, C++ + pqxx + Boost.Asio (central + community), Rust napi-rs (native), React + Zustand (renderer).

**Spec:** `docs/superpowers/specs/2026-05-14-auto-rejoin-communities-design.md` (note: the spec calls for `server_id` directly in the packets; this plan adds a small upstream amendment — a `SERVER_HEARTBEAT_RES` packet that plumbs central's assigned id back to the community at startup, since communities don't otherwise know their central-side `SERIAL` id).

---

## File map

**Modify:**
- `proto/messages.proto` — 3 new packet types (73, 74, 75), 3 new oneof entries, 3 new messages, +1 field on `LoginResponse` (tag 4)
- `src/server/auth_manager.hpp` — declarations for `registerMembership` / `revokeMembership` / `getUserCommunities`; signature change on `upsertCommunityServer` to return the assigned int id
- `src/server/auth_manager.cpp` — DDL + method bodies; `upsertCommunityServer` uses `RETURNING id`
- `src/server/main.cpp` — SERVER_HEARTBEAT handler now responds with SERVER_HEARTBEAT_RES; populate `memberships` on `LoginResponse`; new `MEMBERSHIP_REGISTER_REQ` (shared-secret) and `MEMBERSHIP_REVOKE_REQ` (dual-origin) handlers
- `src/community/main.cpp` — `send_to_central_blocking` reads one response; `SessionManager` gets `server_id_` field + DB persistence; `send_heartbeat` decodes response + caches id; `sync_membership_register` helper + fire-site at successful auth tail; `sync_membership_revoke` helper + fire-site inside `force_disconnect`
- `electron-client/native/src/net/central.rs` — parse new `memberships` field; emit `memberships_received` event; auto-connect fanout via `connect_with_invite`
- `electron-client/native/src/commands/servers.rs` — make `connect_with_invite` `pub(crate)`; new `request_drop_membership` napi command
- `electron-client/native/src/events.rs` — event name + payload struct
- `electron-client/src/stores/chatStore.ts` — `pendingMembershipServerIds` set + actions; `mergeServers` action; wire into `resetForLogout`
- `electron-client/src/features/servers/useServerEvents.ts` — `memberships_received` listener; auth-fail stale-cleanup branch
- `electron-client/src/features/servers/ServerBar.tsx` — placeholder tile state; rewire `×` to `leave_server` with confirmation
- `electron-client/src/features/channels/ServerChannelsSidebar.tsx` — dropdown "Disconnect" → "Leave Server" with confirmation

**Create:** none.

---

## Task 1: Protobuf additions

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Packet type enum entries**

In `Packet.Type` enum, append after `DM_MARK_READ_REQ = 72;`:

```proto
    // Auto-rejoin: community→central membership-tracking (shared
    // secret); client→central membership-drop (JWT) for stale rows;
    // central→community heartbeat response carrying the assigned
    // server_id. See docs/superpowers/specs/
    // 2026-05-14-auto-rejoin-communities-design.md
    MEMBERSHIP_REGISTER_REQ = 73;
    MEMBERSHIP_REVOKE_REQ   = 74;
    SERVER_HEARTBEAT_RES    = 75;
```

- [ ] **Step 2: oneof payload entries**

In `Packet.oneof payload`, after `DmMarkReadReq dm_mark_read_req = 74;`:

```proto
    // --- Auto-rejoin community memberships ---
    MembershipRegisterReq membership_register_req = 75;
    MembershipRevokeReq   membership_revoke_req   = 76;
    ServerHeartbeatRes    server_heartbeat_res    = 77;
```

- [ ] **Step 3: Three new messages, at the end of the file**

```proto
// --- Auto-rejoin community memberships ---

message MembershipRegisterReq {
  string username = 1;
  int64  server_id = 2;
}

message MembershipRevokeReq {
  string username = 1;
  int64  server_id = 2;
}

// Sent by central in response to a SERVER_HEARTBEAT, over the same
// one-shot TLS connection. Lets the community learn its
// central-assigned server_id (community_servers.id) so it can
// populate Membership{Register,Revoke}Req on future packets.
message ServerHeartbeatRes {
  int64 server_id = 1;
}
```

- [ ] **Step 4: Extend `LoginResponse` (next free tag = 4)**

Inside `message LoginResponse { ... }`, append:

```proto
  // Server-populated list of community servers the user is a member
  // of. Drives the auto-rejoin flow. Absent / empty on legacy
  // servers; client treats as empty (graceful degradation).
  repeated CommunityServerInfo memberships = 4;
```

- [ ] **Step 5: Regenerate Rust bindings; verify**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check
```

Expected: `Finished dev profile [unoptimized + debuginfo] target(s)`. If E0063 surfaces on any `LoginResponse { … }` initializer, add `memberships: Vec::new()`.

- [ ] **Step 6: Commit**

```bash
git add proto/messages.proto
git commit -m "proto(auto-rejoin): MEMBERSHIP_REGISTER/REVOKE + SERVER_HEARTBEAT_RES packets + LoginResponse.memberships"
```

---

## Task 2: Central — AuthManager DDL + methods + return-id on upsert

**Files:**
- Modify: `src/server/auth_manager.hpp`
- Modify: `src/server/auth_manager.cpp`

- [ ] **Step 1: Change `upsertCommunityServer` signature to return int**

In `src/server/auth_manager.hpp`, change:

```cpp
    void upsertCommunityServer(const std::string& name, const std::string& description, const std::string& host_ip, int port, int member_count);
```

to:

```cpp
    /// Returns the assigned id (community_servers.id, SERIAL). Used
    /// by the heartbeat handler to ack the community with its
    /// central-side id via SERVER_HEARTBEAT_RES.
    int upsertCommunityServer(const std::string& name, const std::string& description, const std::string& host_ip, int port, int member_count);
```

In `src/server/auth_manager.cpp` at line 221, replace the entire function body with:

```cpp
int AuthManager::upsertCommunityServer(const std::string& name, const std::string& description, const std::string& host_ip, int port, int member_count) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        txn.exec(
            "CREATE TABLE IF NOT EXISTS community_servers ("
            "  id SERIAL PRIMARY KEY,"
            "  name VARCHAR(64) NOT NULL,"
            "  description TEXT,"
            "  host_ip VARCHAR(45) NOT NULL,"
            "  port INTEGER NOT NULL,"
            "  member_count INTEGER DEFAULT 0,"
            "  last_heartbeat TIMESTAMP DEFAULT NOW(),"
            "  UNIQUE(host_ip, port)"
            ")"
        );
        pqxx::result rs = txn.exec_params(
            "INSERT INTO community_servers (name, description, host_ip, port, member_count, last_heartbeat) "
            "VALUES ($1, $2, $3, $4, $5, NOW()) "
            "ON CONFLICT (host_ip, port) DO UPDATE SET "
            "name = EXCLUDED.name, description = EXCLUDED.description, "
            "member_count = EXCLUDED.member_count, last_heartbeat = NOW() "
            "RETURNING id",
            name, description, host_ip, port, member_count
        );
        txn.commit();
        if (rs.empty()) return 0;
        return rs[0][0].as<int>();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] upsertCommunityServer: " << e.what() << "\n";
        return 0;
    }
}
```

- [ ] **Step 2: Add membership method declarations to the header**

In `src/server/auth_manager.hpp`, append:

```cpp
    // --- Auto-rejoin community memberships ---

    /// Idempotent insert. Called on every successful community auth
    /// (via MEMBERSHIP_REGISTER_REQ). ON CONFLICT DO NOTHING.
    void registerMembership(const std::string& username, int64_t server_id);

    /// Idempotent delete. Called by the community-side kick/ban/leave
    /// path (shared secret) and by the client-side stale-membership
    /// cleanup (JWT auth).
    void revokeMembership(const std::string& username, int64_t server_id);

    /// Returns every CommunityServerInfo the user is a member of.
    /// Orphan rows (server_id no longer in community_servers) filtered
    /// out via the JOIN.
    std::vector<chatproj::CommunityServerInfo> getUserCommunities(
        const std::string& username);
```

- [ ] **Step 3: Add DDL inside `initializeDatabase`**

In `src/server/auth_manager.cpp`, inside `initializeDatabase()`, after the `dm_read_state` block and before `txn.commit()`:

```cpp
        txn.exec(
            "CREATE TABLE IF NOT EXISTS user_communities ("
            "  username VARCHAR(32) NOT NULL,"
            "  server_id BIGINT NOT NULL,"
            "  joined_at BIGINT NOT NULL,"
            "  PRIMARY KEY (username, server_id)"
            ")"
        );
        txn.exec(
            "CREATE INDEX IF NOT EXISTS user_communities_user_idx "
            "ON user_communities (username)"
        );
```

- [ ] **Step 4: Implement the three new methods**

Append to `src/server/auth_manager.cpp`:

```cpp
void AuthManager::registerMembership(const std::string& username,
                                       int64_t server_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        auto now = std::chrono::system_clock::now();
        int64_t now_ts = std::chrono::system_clock::to_time_t(now);
        txn.exec_params(
            "INSERT INTO user_communities (username, server_id, joined_at) "
            "VALUES ($1, $2, $3) "
            "ON CONFLICT (username, server_id) DO NOTHING",
            username, server_id, now_ts);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] registerMembership: " << e.what() << "\n";
    }
}

void AuthManager::revokeMembership(const std::string& username,
                                     int64_t server_id) {
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        txn.exec_params(
            "DELETE FROM user_communities "
            "WHERE username = $1 AND server_id = $2",
            username, server_id);
        txn.commit();
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] revokeMembership: " << e.what() << "\n";
    }
}

std::vector<chatproj::CommunityServerInfo>
AuthManager::getUserCommunities(const std::string& username) {
    std::vector<chatproj::CommunityServerInfo> out;
    try {
        pqxx::connection conn(db_conn_str_);
        pqxx::work txn(conn);
        pqxx::result rs = txn.exec_params(
            "SELECT cs.id, cs.name, cs.description, cs.host_ip, "
            "       cs.port, cs.member_count "
            "FROM user_communities uc "
            "JOIN community_servers cs ON cs.id = uc.server_id "
            "WHERE uc.username = $1 "
            "ORDER BY uc.joined_at",
            username);
        txn.commit();

        out.reserve(rs.size());
        for (const auto& row : rs) {
            chatproj::CommunityServerInfo info;
            info.set_id(row[0].as<int>());
            info.set_name(row[1].as<std::string>());
            info.set_description(row[2].is_null() ? "" : row[2].as<std::string>());
            info.set_host_ip(row[3].as<std::string>());
            info.set_port(row[4].as<int>());
            info.set_member_count(row[5].as<int>());
            out.push_back(std::move(info));
        }
        return out;
    } catch (const std::exception& e) {
        std::cerr << "[DB Error] getUserCommunities: " << e.what() << "\n";
        return {};
    }
}
```

- [ ] **Step 5: Commit**

```bash
git add src/server/auth_manager.hpp src/server/auth_manager.cpp
git commit -m "feat(server,auto-rejoin): user_communities DDL + register/revoke/get + upsertCommunityServer returns id"
```

---

## Task 3: Central — heartbeat returns SERVER_HEARTBEAT_RES

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Extend the heartbeat handler**

At `src/server/main.cpp:558`, replace:

```cpp
        else if (packet.type() == chatproj::Packet::SERVER_HEARTBEAT) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped heartbeat - invalid shared secret.\n";
                return;
            }
            auto& hb = packet.server_heartbeat();
            std::cout << "[Server] Heartbeat from community server: " << hb.name() << " at " << hb.host_ip() << ":" << hb.port() << "\n";
            auth_manager_.upsertCommunityServer(hb.name(), hb.description(), hb.host_ip(), hb.port(), hb.member_count());
        }
```

with:

```cpp
        else if (packet.type() == chatproj::Packet::SERVER_HEARTBEAT) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped heartbeat - invalid shared secret.\n";
                return;
            }
            auto& hb = packet.server_heartbeat();
            std::cout << "[Server] Heartbeat from community server: " << hb.name() << " at " << hb.host_ip() << ":" << hb.port() << "\n";
            int server_id = auth_manager_.upsertCommunityServer(
                hb.name(), hb.description(), hb.host_ip(), hb.port(), hb.member_count());

            // Reply with the assigned server_id so the community can
            // populate MembershipRegisterReq / MembershipRevokeReq on
            // subsequent auth events. The community uses a one-shot
            // TLS connection here — this is its only chance to read.
            chatproj::Packet resp;
            resp.set_type(chatproj::Packet::SERVER_HEARTBEAT_RES);
            auto* body = resp.mutable_server_heartbeat_res();
            body->set_server_id(server_id);
            send_packet(resp);
        }
```

`send_packet(...)` should match whatever helper this session class uses elsewhere (grep for existing `send_packet(` calls in this file for response writes — e.g. INVITE_RESOLVE_RES has one). If the method is `write_packet` / `send_packet_external`, use that.

- [ ] **Step 2: Commit**

```bash
git add src/server/main.cpp
git commit -m "feat(server,auto-rejoin): SERVER_HEARTBEAT now responds with SERVER_HEARTBEAT_RES (assigned id)"
```

---

## Task 4: Central — MEMBERSHIP_REGISTER/REVOKE handlers

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Register handler**

After the `INVITE_UNREGISTER_REQ` handler:

```cpp
        // --- MEMBERSHIP_REGISTER_REQ ---
        // Community announces a user's membership. Idempotent on
        // central (INSERT … ON CONFLICT DO NOTHING), so re-firing on
        // every successful community auth is harmless — and serves as
        // the bootstrap mechanism for pre-feature memberships.
        else if (packet.type() == chatproj::Packet::MEMBERSHIP_REGISTER_REQ) {
            if (!auth_manager_.verifySharedSecret(packet.auth_token())) {
                std::cout << "[Security] Dropped membership_register - invalid shared secret.\n";
                return;
            }
            const auto& req = packet.membership_register_req();
            if (req.username().empty() || req.server_id() == 0) return;
            auth_manager_.registerMembership(req.username(), req.server_id());
        }
```

- [ ] **Step 2: Dual-origin revoke handler**

After the register handler:

```cpp
        // --- MEMBERSHIP_REVOKE_REQ ---
        // Dual-origin: community (shared-secret) on kick/ban/leave OR
        // client (JWT session) on stale-membership cleanup. Community
        // can revoke any user; client can only revoke self.
        else if (packet.type() == chatproj::Packet::MEMBERSHIP_REVOKE_REQ) {
            const auto& req = packet.membership_revoke_req();
            if (req.server_id() == 0) return;

            std::string target_username;
            if (auth_manager_.verifySharedSecret(packet.auth_token())) {
                if (req.username().empty()) return;
                target_username = req.username();
            } else if (authenticated_) {
                target_username = username_;
            } else {
                std::cout << "[Security] Dropped membership_revoke - no valid auth.\n";
                return;
            }

            auth_manager_.revokeMembership(target_username, req.server_id());
        }
```

- [ ] **Step 3: Commit**

```bash
git add src/server/main.cpp
git commit -m "feat(server,auto-rejoin): MEMBERSHIP_REGISTER_REQ + dual-origin MEMBERSHIP_REVOKE_REQ handlers"
```

---

## Task 5: Central — populate `LoginResponse.memberships`

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Populate memberships on success**

At `src/server/main.cpp:176`, find the LOGIN_REQ success branch. After `res->set_jwt_token(...)` (or whatever stamps the JWT on the response), before the response is sent:

```cpp
            // Auto-rejoin: ship the user's community memberships
            // inline so the client can start auto-connecting without
            // an extra round-trip.
            for (const auto& info : auth_manager_.getUserCommunities(username)) {
                *res->add_memberships() = info;
            }
```

Replace `username` with whatever the local variable is on this branch (likely `req.username()` or a derived `std::string username`).

- [ ] **Step 2: Commit**

```bash
git add src/server/main.cpp
git commit -m "feat(server,auto-rejoin): populate LoginResponse.memberships on successful login"
```

---

## Task 6: Community — `send_to_central_blocking` reads one response; `SessionManager.server_id_` + persistence

**Files:**
- Modify: `src/community/main.cpp`
- Possibly modify: community DB file (location of `manager.db()` impl)

- [ ] **Step 1: Refactor `send_to_central_blocking`**

At `src/community/main.cpp:2012`, replace with:

```cpp
namespace {
// Sends one framed packet to central over a one-shot TLS connection.
// If `read_response` is true, reads up to one framed response packet
// (with a short timeout) and parses it into `*out_response`. Returns
// true if (read_response == false) OR a response was successfully
// parsed.
bool send_to_central_blocking(const std::string& host, int port,
                               const std::vector<uint8_t>& framed,
                               bool read_response = false,
                               chatproj::Packet* out_response = nullptr) {
    try {
        boost::asio::io_context io;
        ssl::context ctx(ssl::context::tlsv12_client);
        ctx.set_verify_mode(ssl::verify_none);

        tcp::resolver resolver(io);
        auto endpoints = resolver.resolve(host, std::to_string(port));

        tcp::socket raw_socket(io);
        boost::asio::connect(raw_socket, endpoints);

        ssl::stream<tcp::socket> ssl_socket(std::move(raw_socket), ctx);
        ssl_socket.handshake(ssl::stream_base::client);

        boost::asio::write(ssl_socket, boost::asio::buffer(framed));

        if (!read_response || !out_response) {
            ssl_socket.lowest_layer().close();
            return true;
        }

        // Read 4-byte length prefix + payload, with a 2-second
        // deadline so we don't hang on a non-responsive central.
        boost::asio::steady_timer deadline(io);
        deadline.expires_after(std::chrono::seconds(2));
        bool timed_out = false;
        deadline.async_wait([&](const boost::system::error_code& ec) {
            if (!ec) {
                timed_out = true;
                boost::system::error_code ignore;
                ssl_socket.lowest_layer().cancel(ignore);
            }
        });

        uint32_t len_be = 0;
        boost::asio::read(ssl_socket, boost::asio::buffer(&len_be, 4));
        deadline.cancel();
        if (timed_out) {
            ssl_socket.lowest_layer().close();
            return false;
        }
        uint32_t len = ntohl(len_be);
        if (len == 0 || len > (1u << 20)) {
            ssl_socket.lowest_layer().close();
            return false;
        }

        std::vector<uint8_t> body(len);
        boost::asio::read(ssl_socket, boost::asio::buffer(body));
        ssl_socket.lowest_layer().close();

        return out_response->ParseFromArray(body.data(), static_cast<int>(body.size()));
    } catch (const std::exception& e) {
        std::cerr << "[CentralSync] Failed: " << e.what() << "\n";
        return false;
    }
}
} // namespace
```

If `ntohl` isn't in scope, add `#include <arpa/inet.h>` (Linux) or `#include <winsock2.h>` (Windows). Check whether `create_framed_packet` already pulls one in transitively.

- [ ] **Step 2: Add `server_id_` field on `SessionManager`**

In the `SessionManager` class definition (around the existing `central_host_` / `central_port_` / `central_jwt_secret_` / `public_ip_` / `community_port_` fields):

```cpp
    /// Central-assigned id for this community (community_servers.id).
    /// Learned via SERVER_HEARTBEAT_RES; persisted to the community DB
    /// for restart resilience. 0 = not yet known — membership sync
    /// packets skip while 0.
    std::atomic<int64_t> server_id_{0};

    int64_t server_id() const { return server_id_.load(); }
    void set_server_id(int64_t id) { server_id_.store(id); }
```

If `<atomic>` isn't already included in this file, add `#include <atomic>` at the top.

- [ ] **Step 3: Persist `server_id` in the community DB**

Locate the community DB file (the type returned by `manager.db()`). Grep for `class CommunityDb` or `DatabaseManager` in `src/community/`. Find the existing schema setup.

Add a `community_meta` key/value table if one doesn't already exist:

```cpp
    "CREATE TABLE IF NOT EXISTS community_meta ("
    "  key TEXT PRIMARY KEY,"
    "  value TEXT NOT NULL"
    ")"
```

Add two methods on the DB class (header + impl):

```cpp
    int64_t load_central_server_id();
    void save_central_server_id(int64_t id);
```

```cpp
int64_t CommunityDb::load_central_server_id() {
    // Use whatever sqlite3 prepared-statement pattern the rest of this
    // class uses. Example shape:
    //   SELECT value FROM community_meta WHERE key = 'central_server_id'
    // Return std::stoll(value) on hit, 0 on miss / parse error.
    // [implement per existing patterns in this file]
    return 0; // placeholder until engineer wires sqlite3 binding
}

void CommunityDb::save_central_server_id(int64_t id) {
    // INSERT OR REPLACE INTO community_meta (key, value)
    // VALUES ('central_server_id', $1)
    // [implement per existing patterns in this file]
}
```

(The exact sqlite3 calls depend on the existing DB layer's conventions; the engineer should mirror an existing simple key/value-style operation in the same file. If there isn't one, mirror the simplest existing INSERT/SELECT pattern.)

- [ ] **Step 4: Load cached server_id at community startup**

In `src/community/main.cpp`, find `main()`. After the `manager` is constructed and its DB is opened (i.e. after `manager.db()` becomes non-null), before the first heartbeat is fired:

```cpp
    if (auto* db = manager.db()) {
        int64_t cached = db->load_central_server_id();
        if (cached > 0) {
            manager.set_server_id(cached);
            std::cout << "[Community] Loaded cached central server_id = " << cached << "\n";
        }
    }
```

- [ ] **Step 5: Commit**

```bash
git add src/community/main.cpp
# plus the community DB file(s)
git commit -m "feat(community,auto-rejoin): one-shot TLS read-response + SessionManager.server_id_ + DB persistence"
```

---

## Task 7: Community — heartbeat caches server_id from response

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Adapt `send_heartbeat` to read the response**

At `src/community/main.cpp:2316`. The current body ends with:

```cpp
    std::string host = central_host;
    int port = central_port;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
```

(roughly — the engineer should verify by reading the current implementation). Replace that detached-send block with:

```cpp
    std::string host = central_host;
    int port = central_port;
    std::thread([host, port, framed = std::move(framed), &manager]() {
        chatproj::Packet resp;
        bool ok = send_to_central_blocking(host, port, framed, /*read_response=*/true, &resp);
        if (!ok) {
            std::cerr << "[Heartbeat] No response from central\n";
            return;
        }
        if (resp.type() == chatproj::Packet::SERVER_HEARTBEAT_RES) {
            int64_t id = resp.server_heartbeat_res().server_id();
            if (id > 0 && manager.server_id() != id) {
                manager.set_server_id(id);
                if (auto* db = manager.db()) {
                    db->save_central_server_id(id);
                }
                std::cout << "[Heartbeat] Cached central server_id = " << id << "\n";
            }
        }
    }).detach();
```

Make sure `manager` is captured by reference (lifetime is the whole process).

- [ ] **Step 2: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community,auto-rejoin): heartbeat reads SERVER_HEARTBEAT_RES and caches assigned server_id"
```

---

## Task 8: Community — `sync_membership_register` helper + fire on auth tail

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Declaration on `SessionManager`**

In the public section, near `sync_invite_register`:

```cpp
    /// Push membership-grant to central so the user auto-rejoins on
    /// future logins. Idempotent — fires on every successful auth.
    /// Silently skips if server_id_ isn't cached yet (recovered on
    /// next auth after the heartbeat round-trip completes).
    void sync_membership_register(const std::string& username);
```

- [ ] **Step 2: Implementation**

After `sync_invite_unregister` (around `src/community/main.cpp:2078`):

```cpp
void SessionManager::sync_membership_register(const std::string& username) {
    if (central_host_.empty() || central_port_ == 0) return;
    int64_t sid = server_id();
    if (sid <= 0) {
        // server_id not yet learned — skip; will re-fire on next auth
        // after the first heartbeat response lands.
        return;
    }
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::MEMBERSHIP_REGISTER_REQ);
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_membership_register_req();
    req->set_username(username);
    req->set_server_id(sid);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
}
```

- [ ] **Step 3: Wire into successful-auth tail**

Find the community auth handler in `src/community/main.cpp` (search for `authenticated_ = true` in the `COMMUNITY_AUTH_REQ` branch). After that assignment, add:

```cpp
                manager_.sync_membership_register(username_);
```

(Match indentation to the surrounding block.)

- [ ] **Step 4: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community,auto-rejoin): sync_membership_register helper + fire on every successful auth"
```

---

## Task 9: Community — `sync_membership_revoke` helper + fire in `force_disconnect`

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Declaration on `SessionManager`**

Next to `sync_membership_register`:

```cpp
    /// Symmetric counterpart. Fired from force_disconnect (the
    /// chokepoint for kick/ban/leave) so central drops the
    /// user_communities row and auto-rejoin doesn't surface a stale
    /// tile.
    void sync_membership_revoke(const std::string& username);
```

- [ ] **Step 2: Implementation**

After `sync_membership_register`:

```cpp
void SessionManager::sync_membership_revoke(const std::string& username) {
    if (central_host_.empty() || central_port_ == 0) return;
    int64_t sid = server_id();
    if (sid <= 0) return;
    chatproj::Packet packet;
    packet.set_type(chatproj::Packet::MEMBERSHIP_REVOKE_REQ);
    packet.set_auth_token(central_jwt_secret_);
    auto* req = packet.mutable_membership_revoke_req();
    req->set_username(username);
    req->set_server_id(sid);

    std::string serialized;
    packet.SerializeToString(&serialized);
    auto framed = chatproj::create_framed_packet(serialized);

    std::string host = central_host_;
    int port = central_port_;
    std::thread([host, port, framed = std::move(framed)]() {
        send_to_central_blocking(host, port, framed);
    }).detach();
}
```

- [ ] **Step 3: Fire inside `force_disconnect`**

At `src/community/main.cpp:1978`, replace `force_disconnect` body with:

```cpp
void SessionManager::force_disconnect(const std::string& username,
                                      const std::string& action,
                                      const std::string& reason,
                                      const std::string& actor) {
    auto session = find_session_by_username(username);
    if (!session) return;

    // Tell central this user is no longer a member so auto-rejoin
    // doesn't surface a stale tile on their next login. Fires for
    // all three force_disconnect call sites: kick, ban, leave.
    sync_membership_revoke(username);

    // Best-effort notification before we close the socket. If the write is
    // already queued behind a slow client, the close below cancels it — the
    // target just won't see the reason, which is fine.
    chatproj::Packet p;
    p.set_type(chatproj::Packet::MEMBERSHIP_REVOKED);
    auto* rev = p.mutable_membership_revoked();
    rev->set_action(action);
    rev->set_reason(reason);
    rev->set_actor(actor);
    session->send_packet_external(p);

    session->close_connection();
}
```

- [ ] **Step 4: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community,auto-rejoin): sync_membership_revoke + fire inside force_disconnect (kick/ban/leave)"
```

---

**CHECKPOINT 1: Server-side complete (Tasks 1-9).** Pause here for review/build before native+renderer work.

```bash
# On the Linux build host
cmake --build build-servers
cmake --build build-community
# Restart both. First community heartbeat should log:
#   [Heartbeat] Cached central server_id = N
```

---

## Task 10: Native — parse `memberships` + auto-connect fanout

**Files:**
- Modify: `electron-client/native/src/commands/servers.rs` (make `connect_with_invite` `pub(crate)`)
- Modify: `electron-client/native/src/events.rs`
- Modify: `electron-client/native/src/net/central.rs`

- [ ] **Step 1: Expose `connect_with_invite`**

In `electron-client/native/src/commands/servers.rs:319`, change:

```rust
async fn connect_with_invite(
```

to:

```rust
pub(crate) async fn connect_with_invite(
```

- [ ] **Step 2: Event constant + payload**

In `electron-client/native/src/events.rs`, add the new event name and payload:

```rust
pub const MEMBERSHIPS_RECEIVED: &str = "memberships_received";

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct MembershipsReceivedPayload {
    pub memberships: Vec<ServerInfoPayload>,
}

pub fn emit_memberships_received(memberships: Vec<ServerInfoPayload>) {
    send(MEMBERSHIPS_RECEIVED, MembershipsReceivedPayload { memberships });
}
```

If `ServerInfoPayload` doesn't exist, grep `events.rs` for the type used by `server_list_received` (likely `ServerInfo` / `ServerEntry` / similar) and use that exact name throughout.

- [ ] **Step 3: Parse + auto-connect in `net/central.rs`**

In the `LoginRes` arm of `route_packets`, after the existing success-path lines that set `state.token` and emit `login_succeeded`:

```rust
                    // Auto-rejoin: ship memberships to renderer for
                    // placeholder tiles, then auto-connect in parallel.
                    let memberships: Vec<events::ServerInfoPayload> = res
                        .memberships
                        .iter()
                        .map(|info| events::ServerInfoPayload {
                            id: info.id,
                            name: info.name.clone(),
                            description: info.description.clone(),
                            host_ip: info.host_ip.clone(),
                            port: info.port,
                            member_count: info.member_count,
                        })
                        .collect();
                    events::emit_memberships_received(memberships.clone());

                    for info in memberships {
                        let server_id = info.id.to_string();
                        let host = info.host_ip.clone();
                        let port = info.port as u16;
                        tokio::spawn(async move {
                            if let Err(e) = crate::commands::servers::connect_with_invite(
                                server_id, host, port, None,
                            )
                            .await
                            {
                                eprintln!("[auto-rejoin] connect failed: {e}");
                            }
                        });
                    }
```

If `ServerInfoPayload` field names differ (e.g. `hostIp` for camelCase), match them.

- [ ] **Step 4: Verify cargo check**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; cargo check
```

Expected: `Finished dev profile`.

- [ ] **Step 5: Commit**

```bash
git add electron-client/native/src/events.rs electron-client/native/src/net/central.rs electron-client/native/src/commands/servers.rs
git commit -m "feat(native,auto-rejoin): parse LoginResponse.memberships + auto-connect fanout"
```

---

## Task 11: Native — `request_drop_membership` napi command

**Files:**
- Modify: `electron-client/native/src/commands/servers.rs`

- [ ] **Step 1: Add the command**

Append to `electron-client/native/src/commands/servers.rs`:

```rust
#[napi(object)]
pub struct DropMembershipArgs {
    pub server_id: String,
}

/// Stale-membership cleanup: tells central to drop the user's
/// user_communities row for this server. Used when an auto-rejoin
/// auth comes back with success=false (kicked/banned while offline).
/// Authenticated via the user's JWT on the central session —
/// central's MEMBERSHIP_REVOKE_REQ handler enforces self-revoke when
/// JWT-authed (ignores the username field).
#[napi]
pub async fn request_drop_membership(args: DropMembershipArgs) -> napi::Result<()> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, MembershipRevokeReq};

    let server_id: i64 = args.server_id.parse().map_err(|_| {
        napi::Error::from_reason(format!("Invalid server_id: {}", args.server_id))
    })?;

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
            packet::Type::MembershipRevokeReq,
            packet::Payload::MembershipRevokeReq(MembershipRevokeReq {
                username: String::new(),
                server_id,
            }),
            token.as_deref(),
        );
        (tx, pkt)
    };

    let _ = tokio::time::timeout(std::time::Duration::from_secs(5), write_tx.send(data)).await;
    Ok(())
}
```

Match the exact module path / helper names with what an existing JWT-authed client→central command in the same file uses (e.g. similar to how `request_friend_action` or another simple command is built — engineer should grep one).

- [ ] **Step 2: Rebuild addon**

```bash
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"; $env:VCPKG_ROOT = "C:\dev\vcpkg"; cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client/native; npm run build
```

Expected: `Finished release profile`.

- [ ] **Step 3: tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add electron-client/native/src/commands/servers.rs electron-client/native/index.d.ts electron-client/native/index.js
git commit -m "feat(native,auto-rejoin): request_drop_membership napi command"
```

---

**CHECKPOINT 2: Native chain complete (Tasks 10-11).**

---

## Task 12: Renderer — memberships listener + stale-membership cleanup + toast

**Files:**
- Modify: `electron-client/src/stores/chatStore.ts`
- Modify: `electron-client/src/features/servers/useServerEvents.ts`

- [ ] **Step 1: chatStore additions**

In `electron-client/src/stores/chatStore.ts`, near the `connectedServers` declarations, add:

```ts
  pendingMembershipServerIds: Set<string>;
  setPendingMemberships: (ids: string[]) => void;
  removePendingMembership: (id: string) => void;
  mergeServers: (entries: CommunityServer[]) => void;
```

In the `create<...>()` factory body:

```ts
  pendingMembershipServerIds: new Set(),
  setPendingMemberships: (ids) =>
    set({ pendingMembershipServerIds: new Set(ids) }),
  removePendingMembership: (id) =>
    set((state) => {
      if (!state.pendingMembershipServerIds.has(id)) return {};
      const next = new Set(state.pendingMembershipServerIds);
      next.delete(id);
      return { pendingMembershipServerIds: next };
    }),
  mergeServers: (entries) =>
    set((state) => {
      const byId = new Map<string, CommunityServer>();
      for (const s of state.servers) byId.set(s.id, s);
      for (const s of entries) {
        if (!byId.has(s.id)) byId.set(s.id, s);
      }
      return { servers: Array.from(byId.values()) };
    }),
```

In the existing `resetForLogout` action (grep the file for it), append `pendingMembershipServerIds: new Set()` to the `set({...})` argument.

- [ ] **Step 2: `memberships_received` listener + auth-fail branch**

In `electron-client/src/features/servers/useServerEvents.ts`, near the existing `server_list_received` listener, add:

```ts
    const unlistenMemberships = listen<{
      memberships: Array<{
        id: number;
        name: string;
        description: string;
        hostIp: string;
        port: number;
        memberCount: number;
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
    });
```

Append cleanup in the effect's return:

```ts
      unlistenMemberships.then((fn) => fn());
```

Find the `community_auth_responded` listener in the same file. Inside its callback, after destructuring the payload (`const p = event.payload;` or similar):

```ts
      if (!p.success) {
        const wasPending = useChatStore
          .getState()
          .pendingMembershipServerIds.has(p.serverId);
        if (wasPending) {
          useChatStore.getState().removePendingMembership(p.serverId);
          invoke("request_drop_membership", { serverId: p.serverId }).catch(
            console.error,
          );
          const serverName =
            useChatStore.getState().servers.find((s) => s.id === p.serverId)
              ?.name ?? "a community server";
          toast.error(
            "Membership revoked",
            `You're no longer a member of ${serverName}.`,
          );
        }
        return;
      }
      // Success path continues below: existing addConnectedServer etc.
      useChatStore.getState().removePendingMembership(p.serverId);
```

Replace `toast.error(...)` with whatever the existing toast API is — search other features for the toast call shape and use the matching pattern.

- [ ] **Step 3: tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 4: Commit**

```bash
git add electron-client/src/stores/chatStore.ts electron-client/src/features/servers/useServerEvents.ts
git commit -m "feat(auto-rejoin): renderer memberships listener + stale-membership cleanup + toast"
```

---

## Task 13: Renderer — placeholder tiles + Disconnect → Leave Server

**Files:**
- Modify: `electron-client/src/features/servers/ServerBar.tsx`
- Modify: `electron-client/src/features/channels/ServerChannelsSidebar.tsx`

- [ ] **Step 1: Placeholder tiles in ServerBar**

Read the new field:

```tsx
  const pendingMembershipServerIds = useChatStore(
    (s) => s.pendingMembershipServerIds,
  );
```

Replace the existing visible-server filter (currently `servers.filter((s) => connectedServers.has(s.id))`) with:

```tsx
  const visible = useMemo(
    () =>
      servers.filter(
        (s) =>
          connectedServers.has(s.id) ||
          pendingMembershipServerIds.has(s.id),
      ),
    [servers, connectedServers, pendingMembershipServerIds],
  );
```

In the iteration, derive `isPending` per tile and apply a "connecting…" styling when set. The exact JSX should preserve existing active-server styling and the existing × button — engineer should diff carefully.

- [ ] **Step 2: Rewire `×` to leave_server with confirmation**

Find `handleDisconnect`:

```tsx
  const handleDisconnect = (e: React.MouseEvent, serverId: string) => {
    e.stopPropagation();
    const server = servers.find((s) => s.id === serverId);
    const name = server?.name ?? "this server";
    if (!window.confirm(`Leave ${name}? You will need a new invite to rejoin.`)) {
      return;
    }
    invoke("leave_server", { serverId }).catch(console.error);
    useChatStore.getState().removeConnectedServer(serverId);
    if (activeServerId === serverId) {
      setActiveServer(null);
      setActiveChannel(null);
      setActiveView("home");
    }
  };
```

Verify the napi command name is `leave_server` (grep `electron-client/native/index.d.ts`); use the actual name.

- [ ] **Step 3: Rewire ServerChannelsSidebar dropdown**

In `electron-client/src/features/channels/ServerChannelsSidebar.tsx`, find the `onDisconnect` callback in the dropdown. Replace with the same confirm-then-leave pattern. Also change any user-visible "Disconnect" label to "Leave Server" — grep the surrounding component (or `ServerActionsDropdown.tsx`) for the label.

- [ ] **Step 4: tsc**

```bash
cd C:/Users/sunkh/Desktop/decibell/decibell/electron-client; npx tsc --noEmit
```

Expected: exit 0.

- [ ] **Step 5: Commit**

```bash
git add electron-client/src/features/servers/ServerBar.tsx electron-client/src/features/channels/ServerChannelsSidebar.tsx
# add ServerActionsDropdown.tsx (or whichever) if label changed there
git commit -m "feat(auto-rejoin): placeholder tiles + Disconnect → Leave Server with confirmation"
```

---

**CHECKPOINT 3: Renderer complete (Tasks 12-13).**

---

## Task 14: Manual end-to-end test pass

**Files:** none — manual verification only.

- [ ] **Step 1: First-heartbeat handshake**

Restart a community server. Community logs should contain:
```
[Heartbeat] Cached central server_id = N
```

- [ ] **Step 2: Bootstrap path**

Login as user A. Server bar empty. Connect to community X via ServerBrowseView → auth succeeds → tile appears. Confirm in central's Postgres:
```sql
SELECT username, server_id FROM user_communities WHERE username = '<userA>';
```
One row expected.

- [ ] **Step 3: Auto-rejoin on next login**

Close & reopen client, log in. Tile for X appears immediately with `connecting…` styling, flips to connected after auth.

- [ ] **Step 4: Kick path cleans up central**

A in X. X-owner kicks A. Tile disappears. `user_communities` (A, X) row gone. Next login: no X tile.

- [ ] **Step 5: Stale-membership cleanup**

A in Y. Take central offline. Kick A from Y (community→central revoke is silently dropped). Bring central up. A logs in:
- Auto-rejoin tries Y, gets rejected
- Toast: "Membership revoked: You're no longer a member of Y."
- Tile gone
- `user_communities` (A, Y) row gone
- Next login: no Y tile

- [ ] **Step 6: Disconnect → Leave consolidation**

A in Z. Click × on Z tile → confirm dialog. Confirm → tile gone, Z's members table doesn't have A, central's `user_communities` (A, Z) row gone.

Repeat with ServerChannelsSidebar dropdown.

- [ ] **Step 7: Unreachable-server tolerance**

A in Q. Kill Q's process. A logs in. Auto-rejoin attempts Q, fails, Q tile renders with connection-lost indicator, no toast. Restart Q → manual reconnect works.

- [ ] **Step 8: Regression sweep**

- ServerBrowseView Connect/RedeemInvite still works
- Channels + history load on auto-rejoined servers
- DMs unaffected
- Voice + streaming work
- MembersAdminPanel "Leave Server" still works
- Logout fully clears auto-rejoin state

---

## Self-review notes

**Spec coverage:**
- §1 schema → Task 2
- §2 wire protocol → Tasks 1/3/4 (central) + Tasks 6/7/8/9 (community)
- §3 client auto-connect → Task 10
- §3 napi command → Task 11
- §3 renderer placeholder + cleanup → Task 12
- §3 Disconnect → Leave → Task 13
- §4 bootstrap → Task 8 fires register on every auth (idempotent INSERT)
- Plan amendment (server_id plumbing) → Tasks 1/3/6/7

**Type consistency check:** all proto names, AuthManager method names, SessionManager API, event names, and chatStore actions are referenced consistently across the tasks they appear in.
