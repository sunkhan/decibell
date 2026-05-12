# Custom Profile Pictures Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Centrally-stored per-user profile pictures uploaded from Settings → Account, cropped client-side to a 256×256 JPEG, stored in Postgres as `BYTEA`, distributed to peers via in-band protobuf with sha256-hex cache versioning, displayed everywhere usernames currently render as letter avatars.

**Architecture:** Central server gains two columns (`avatar`, `avatar_version`) on `users` plus five new protobuf packet types. C++ server validates JPEG magic + 200 KB cap, stores bytes, broadcasts `AvatarChanged` to every active session. Rust napi addon exposes `upload_avatar` / `fetch_avatar` commands. Renderer adds a Zustand `avatarStore` keyed by username + version (with blob-URL backed `<img>` rendering), a shared `<UserAvatar>` component replacing inline letter markup at every call site, and an inline cropper modal in Account tab.

**Tech Stack:** C++ + OpenSSL + pqxx (server), prost / napi-rs (native), React + Zustand + WebCodecs/Canvas (renderer), Protobuf 3 (wire).

**Spec reference:** `docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md` — read first. Sections referenced below as §N.

---

## Task 1: Protobuf schema additions

**Files:**
- Modify: `proto/messages.proto`

- [ ] **Step 1: Append the five new messages and modify two existing ones**

At the end of `proto/messages.proto`, append the new `// --- Avatar messages ---` block:

```protobuf
// --- Avatar messages ---

message UpdateAvatarReq {
  bytes data = 1;       // empty = remove; otherwise JPEG bytes, max 200 KB
}

message UpdateAvatarRes {
  bool success = 1;
  string message = 2;
  string version = 3;   // sha256-hex; '' on removal
}

message FetchAvatarReq {
  string username = 1;
}

message FetchAvatarRes {
  string username = 1;
  string version = 2;   // '' when user has no avatar
  bytes data = 3;       // empty when version == ''
}

message AvatarChanged {
  string username = 1;
  string version = 2;   // '' on removal
}

message UserPresence {
  string username = 1;
  string avatar_version = 2;
}
```

In the existing `message FriendInfo` block, add a third field:

```protobuf
message FriendInfo {
  string username = 1;
  enum Status { ONLINE=0; OFFLINE=1; PENDING_INCOMING=2; PENDING_OUTGOING=3; BLOCKED=4; }
  Status status = 2;
  string avatar_version = 3;       // NEW
}
```

Replace the existing `message PresenceUpdate` block:

```protobuf
message PresenceUpdate {
  repeated UserPresence users = 1;   // was: repeated string online_users
}
```

- [ ] **Step 2: Add the five new `Packet.Type` enum entries**

Locate the `enum Type {` block inside `message Packet`. Append after the last existing variant (preserving its numeric value `N`):

```protobuf
  UPDATE_AVATAR_REQ = N+1;
  UPDATE_AVATAR_RES = N+2;
  FETCH_AVATAR_REQ = N+3;
  FETCH_AVATAR_RES = N+4;
  AVATAR_CHANGED   = N+5;
```

Replace `N` with whatever the last currently-used enum value is (read it from the file). Numeric values must be unique and monotonically increasing.

- [ ] **Step 3: Add the five new oneof payload variants in `Packet.payload`**

Locate the `oneof payload {` block. Append the matching message bindings using fresh field numbers (start at the next unused number after the last `_req` / `_res`):

```protobuf
    UpdateAvatarReq update_avatar_req = M+1;
    UpdateAvatarRes update_avatar_res = M+2;
    FetchAvatarReq  fetch_avatar_req  = M+3;
    FetchAvatarRes  fetch_avatar_res  = M+4;
    AvatarChanged   avatar_changed    = M+5;
```

Replace `M` with whatever the last currently-used field number is. Each `oneof` variant needs a unique field number.

- [ ] **Step 4: Commit**

```powershell
git add proto/messages.proto
git commit -m "feat(proto): avatar messages + version on FriendInfo/PresenceUpdate

Add UpdateAvatarReq/Res, FetchAvatarReq/Res, AvatarChanged message
types plus the UserPresence wrapper. FriendInfo gains avatar_version
(sha256-hex); PresenceUpdate.online_users replaced with repeated
UserPresence so non-friend peers also carry their version. Wire-
breaking change accepted per design spec §2 (all clients pinned to
latest).

See docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md
§4 for the exact wire format."
```

---

## Task 2: Verify protobuf compilation on both sides

**Files:** none (verification only)

- [ ] **Step 1: Build the server's protobuf to confirm the new types compile**

Run from the repo root:

```powershell
cmake --build build-servers --config Release --target generate_protos
```

Expected: build succeeds with new `chatproj::UpdateAvatarReq`, `chatproj::UpdateAvatarRes`, etc. headers regenerated in `build-servers/proto/`.

If the CMake target name differs in your build setup, locate it via `cmake --build build-servers --target help | findstr proto`.

- [ ] **Step 2: Rebuild the Rust client native addon to confirm prost regens the types**

Run:

```powershell
$env:VCPKG_ROOT = "C:\dev\vcpkg"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
cd electron-client/native
cargo build
```

Expected: build succeeds. New types `crate::net::proto::UpdateAvatarReq`, `UpdateAvatarRes`, `FetchAvatarReq`, `FetchAvatarRes`, `AvatarChanged`, `UserPresence` are now available; `packet::Type::UpdateAvatarReq` etc. enum variants exist.

If the build fails with "field number already used" or similar, the enum/oneof numbering in Task 1 collided — re-pick non-overlapping values.

- [ ] **Step 3: No commit**

Pure regeneration; nothing tracked changes.

---

## Task 3: Server schema migration

**Files:**
- Modify: `src/server/auth_manager.cpp`

- [ ] **Step 1: Add the avatar columns to `initializeDatabase`**

Locate the existing `CREATE TABLE IF NOT EXISTS users (...)` call at the top of `AuthManager::initializeDatabase()`. Immediately after the `txn.exec("CREATE TABLE IF NOT EXISTS users ...")` call, before the friends table, add:

```cpp
        // Avatar columns added 2026-05-12 (see
        // docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md
        // §5). ADD COLUMN IF NOT EXISTS makes this idempotent so the
        // migration runs cleanly on already-deployed servers.
        txn.exec(
            "ALTER TABLE users ADD COLUMN IF NOT EXISTS avatar BYTEA"
        );
        txn.exec(
            "ALTER TABLE users ADD COLUMN IF NOT EXISTS "
            "avatar_version VARCHAR(64) NOT NULL DEFAULT ''"
        );
```

- [ ] **Step 2: Compile to verify**

```powershell
cmake --build build-servers --config Release
```

Expected: server binary rebuilds cleanly.

- [ ] **Step 3: Commit**

```powershell
git add src/server/auth_manager.cpp
git commit -m "feat(server): add avatar + avatar_version columns to users

Idempotent ALTER TABLE additions in initializeDatabase. avatar is a
BYTEA (NULL for users with no picture); avatar_version is VARCHAR(64)
holding the sha256-hex of the bytes (or '' when none). Migration runs
at startup; safe to re-run on already-migrated databases."
```

---

## Task 4: Server sha256_hex helper

**Files:**
- Modify: `src/server/auth_utils.hpp`

- [ ] **Step 1: Add an inline `sha256_hex` helper using OpenSSL EVP**

Append to the end of `src/server/auth_utils.hpp` (before any closing namespace if there is one — match the existing file's style):

```cpp
#include <openssl/evp.h>
#include <sstream>
#include <iomanip>

namespace chatproj {

/// SHA-256 of the given bytes, formatted as a 64-character lowercase
/// hex string. Used to version-stamp avatar bytes so clients know when
/// to invalidate their cache. Empty input returns the well-known
/// SHA-256("") hash; callers that want '' for empty input should
/// branch before calling this.
inline std::string sha256_hex(const std::string& data) {
    unsigned char digest[EVP_MAX_MD_SIZE];
    unsigned int len = 0;
    EVP_MD_CTX* ctx = EVP_MD_CTX_new();
    EVP_DigestInit_ex(ctx, EVP_sha256(), nullptr);
    EVP_DigestUpdate(ctx, data.data(), data.size());
    EVP_DigestFinal_ex(ctx, digest, &len);
    EVP_MD_CTX_free(ctx);

    std::ostringstream oss;
    for (unsigned int i = 0; i < len; ++i) {
        oss << std::hex << std::setw(2) << std::setfill('0')
            << static_cast<int>(digest[i]);
    }
    return oss.str();
}

}  // namespace chatproj
```

If `auth_utils.hpp` doesn't use a `chatproj` namespace, drop the namespace wrapper and adjust accordingly to match the file's existing style.

- [ ] **Step 2: Compile**

```powershell
cmake --build build-servers --config Release
```

Expected: succeeds. OpenSSL is already linked for TLS.

- [ ] **Step 3: Commit**

```powershell
git add src/server/auth_utils.hpp
git commit -m "feat(server): sha256_hex helper for avatar versioning

OpenSSL EVP-based SHA-256, returns 64-char lowercase hex. Used to
content-version avatar bytes so clients can cache + invalidate
correctly. OpenSSL is already linked for TLS — no new deps."
```

---

## Task 5: Server session-manager broadcast helper

**Files:**
- Modify: `src/server/session_manager.hpp`

- [ ] **Step 1: Locate `broadcast_presence()` and add a sibling `broadcast_avatar_changed`**

Open `src/server/session_manager.hpp`. Find the existing `broadcast_presence()` method. Immediately below it, add:

```cpp
    /// Push AvatarChanged to every active authenticated session.
    /// Fires on every UPDATE_AVATAR_REQ that mutates a user's avatar
    /// (including removal — version is '' in that case). Iterates the
    /// session map under the same mutex broadcast_presence uses;
    /// per-session send is non-blocking.
    void broadcast_avatar_changed(const std::string& username,
                                  const std::string& version) {
        std::lock_guard<std::mutex> lock(sessions_mutex_);
        chatproj::Packet packet;
        packet.set_type(chatproj::Packet::AVATAR_CHANGED);
        auto* payload = packet.mutable_avatar_changed();
        payload->set_username(username);
        payload->set_version(version);
        std::string serialized;
        packet.SerializeToString(&serialized);
        for (auto& [name, session] : sessions_) {
            if (auto s = session.lock()) {
                s->send_framed(serialized);
            }
        }
    }
```

Adjust `session.lock()` / `send_framed` if the existing `broadcast_presence` uses different accessor names — copy the exact pattern from it.

- [ ] **Step 2: Compile**

```powershell
cmake --build build-servers --config Release
```

Expected: succeeds.

- [ ] **Step 3: Commit**

```powershell
git add src/server/session_manager.hpp
git commit -m "feat(server): broadcast_avatar_changed helper on session manager

Fans out an AVATAR_CHANGED packet to every currently-active session.
Called from the UPDATE_AVATAR_REQ handler after the DB UPDATE
commits. Mirrors broadcast_presence's locking + iteration shape."
```

---

## Task 6: Server UPDATE_AVATAR_REQ handler

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Add the handler in the packet-type dispatch chain**

Find the existing `else if (packet.type() == chatproj::Packet::FRIEND_LIST_REQ)` branch (or any other handler near the end of the chain). After it, before the catch-all/closing brace, add:

```cpp
        // --- AVATAR UPLOAD ---
        else if (packet.type() == chatproj::Packet::UPDATE_AVATAR_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.update_avatar_req();
            const std::string& data = req.data();

            chatproj::Packet response;
            response.set_type(chatproj::Packet::UPDATE_AVATAR_RES);
            auto* res = response.mutable_update_avatar_res();

            // Validate: empty = remove; otherwise must be a JPEG <=200 KB.
            if (!data.empty()) {
                if (data.size() < 2 ||
                    static_cast<unsigned char>(data[0]) != 0xFF ||
                    static_cast<unsigned char>(data[1]) != 0xD8) {
                    res->set_success(false);
                    res->set_message("Not a JPEG");
                    send_framed(response);
                    return;
                }
                if (data.size() > 200 * 1024) {
                    res->set_success(false);
                    res->set_message("Avatar too large");
                    send_framed(response);
                    return;
                }
            }

            const std::string version = data.empty()
                ? std::string()
                : chatproj::sha256_hex(data);

            try {
                pqxx::work txn(*db_conn_);
                if (data.empty()) {
                    txn.exec_params(
                        "UPDATE users SET avatar = NULL, avatar_version = '' "
                        "WHERE username = $1",
                        username_);
                } else {
                    txn.exec_params(
                        "UPDATE users SET avatar = $1, avatar_version = $2 "
                        "WHERE username = $3",
                        pqxx::binarystring(data.data(), data.size()),
                        version, username_);
                }
                txn.commit();
            } catch (const std::exception& e) {
                std::cerr << "[Server] avatar UPDATE failed: " << e.what() << "\n";
                res->set_success(false);
                res->set_message("Storage error");
                send_framed(response);
                return;
            }

            res->set_success(true);
            res->set_version(version);
            send_framed(response);

            manager_.broadcast_avatar_changed(username_, version);
        }
```

If `send_framed` is named differently in the surrounding code (e.g. `send_response` or `write_framed`), use whatever the rest of the file uses. The function should serialize the packet with length-prefix framing.

Same for `db_conn_` — copy whatever member or accessor the existing `FRIEND_ACTION_REQ` handler uses to reach the pqxx connection.

- [ ] **Step 2: Include the sha256_hex header**

Near the top of `main.cpp` confirm `#include "auth_utils.hpp"` is present (it likely already is — `validateToken` lives there). If not, add it.

- [ ] **Step 3: Compile**

```powershell
cmake --build build-servers --config Release
```

Expected: succeeds.

- [ ] **Step 4: Commit**

```powershell
git add src/server/main.cpp
git commit -m "feat(server): UPDATE_AVATAR_REQ handler

Validates JPEG magic + 200 KB cap, computes sha256-hex version,
single-transaction UPDATE on the users row, broadcasts AvatarChanged
to every active session, replies with the new version. Empty data
is a 'remove' (sets avatar = NULL, version = '')."
```

---

## Task 7: Server FETCH_AVATAR_REQ handler

**Files:**
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Add the fetch handler in the same dispatch chain**

After the `UPDATE_AVATAR_REQ` handler from Task 6, add:

```cpp
        // --- AVATAR FETCH ---
        else if (packet.type() == chatproj::Packet::FETCH_AVATAR_REQ) {
            if (!authenticated_) return;
            const auto& req = packet.fetch_avatar_req();
            const std::string& target = req.username();

            chatproj::Packet response;
            response.set_type(chatproj::Packet::FETCH_AVATAR_RES);
            auto* res = response.mutable_fetch_avatar_res();
            res->set_username(target);

            try {
                pqxx::work txn(*db_conn_);
                pqxx::result rs = txn.exec_params(
                    "SELECT avatar, avatar_version FROM users WHERE username = $1",
                    target);
                txn.commit();

                if (rs.empty()) {
                    res->set_version("");
                    // data left empty
                } else {
                    res->set_version(rs[0]["avatar_version"].as<std::string>());
                    if (!rs[0]["avatar"].is_null()) {
                        pqxx::binarystring blob(rs[0]["avatar"]);
                        res->set_data(blob.data(), blob.size());
                    }
                }
            } catch (const std::exception& e) {
                std::cerr << "[Server] avatar FETCH failed: " << e.what() << "\n";
                res->set_version("");
            }

            send_framed(response);
        }
```

- [ ] **Step 2: Compile**

```powershell
cmake --build build-servers --config Release
```

Expected: succeeds.

- [ ] **Step 3: Commit**

```powershell
git add src/server/main.cpp
git commit -m "feat(server): FETCH_AVATAR_REQ handler

SELECT (avatar, avatar_version) by username, pack into FetchAvatarRes.
No friend-gating — authenticated callers can fetch any user's
avatar (mirrors username/online-status visibility today). Missing
users or missing avatars both surface as empty version + empty data."
```

---

## Task 8: Server avatar_version on FriendInfo + PresenceUpdate

**Files:**
- Modify: `src/server/main.cpp` (handlers that build `FRIEND_LIST_RES` + `PRESENCE_UPDATE`)
- Modify: `src/server/session_manager.hpp` (`broadcast_presence` if it lives here)

- [ ] **Step 1: Find the FRIEND_LIST_RES builder**

Locate the `FRIEND_LIST_REQ` handler in `main.cpp`. It currently SELECTs friend rows and for each one populates a `FriendInfo` with `username` + `status`. Update the SELECT to also pull `users.avatar_version`:

```cpp
pqxx::result rs = txn.exec_params(
    "SELECT f.user1, f.user2, f.status, u.avatar_version "
    "FROM friends f JOIN users u ON "
    "  (CASE WHEN f.user1 = $1 THEN u.username = f.user2 "
    "        ELSE u.username = f.user1 END) "
    "WHERE f.user1 = $1 OR f.user2 = $1",
    username_);
```

(The exact SQL shape depends on the existing query — preserve it and add the `avatar_version` column.)

For each row, set the new field on the FriendInfo:

```cpp
info->set_avatar_version(rs[i]["avatar_version"].as<std::string>(""));
```

The `.as<std::string>("")` defaulting handles `NULL` (though the column has `NOT NULL DEFAULT ''`, defensive default is cheap).

- [ ] **Step 2: Find the PresenceUpdate builder**

Search `main.cpp` and `session_manager.hpp` for the existing `presence->add_online_users(uname)` calls (matching `online_users` field name). Replace each with the new shape:

```cpp
// OLD:
// presence->add_online_users(uname);

// NEW:
auto* user_entry = presence->add_users();
user_entry->set_username(uname);

// Look up avatar_version from the session_manager's user→session map,
// OR query the users table. The session map is cheaper.
user_entry->set_avatar_version(session_manager_avatar_version_for(uname));
```

`session_manager_avatar_version_for(...)` is a placeholder for whatever lookup makes sense in your code. The simplest implementation: each `Session` caches its user's `avatar_version` at login time and refreshes it on each `UPDATE_AVATAR_REQ`. The session_manager iterates sessions and pulls each one's cached version.

To support that:
1. In `Session`, add `std::string avatar_version_;` field, populated at login from `SELECT avatar_version FROM users WHERE username = $1`.
2. In `UPDATE_AVATAR_REQ` handler (Task 6), after the UPDATE commits and broadcast, also `this->avatar_version_ = version;`.
3. In `broadcast_presence`, iterate sessions and call `session->avatar_version()` getter.

Add the getter to `Session`:

```cpp
const std::string& avatar_version() const { return avatar_version_; }
```

And the load at login (in the `LOGIN_REQ` handler, after `authenticated_ = true; username_ = ...;`):

```cpp
try {
    pqxx::work txn(*db_conn_);
    auto rs = txn.exec_params(
        "SELECT avatar_version FROM users WHERE username = $1",
        username_);
    if (!rs.empty()) avatar_version_ = rs[0][0].as<std::string>("");
    txn.commit();
} catch (...) { /* leave avatar_version_ empty */ }
```

- [ ] **Step 3: Compile**

```powershell
cmake --build build-servers --config Release
```

Expected: succeeds.

- [ ] **Step 4: Commit**

```powershell
git add src/server/main.cpp src/server/session_manager.hpp
git commit -m "feat(server): populate avatar_version on FriendInfo + PresenceUpdate

FRIEND_LIST_RES SELECT now joins users.avatar_version and stamps each
FriendInfo with it. PresenceUpdate's wire shape changed from
repeated string to repeated UserPresence — the builder pulls each
session's cached avatar_version (loaded at login, refreshed on
UPDATE_AVATAR_REQ). Clients use this to detect cache misses
without polling."
```

---

## Task 9: Native AppState pending-avatar fields

**Files:**
- Modify: `electron-client/native/src/state.rs`

- [ ] **Step 1: Add two new fields to `AppState`**

In `state.rs`, locate the `AppState` struct. Add:

```rust
    /// In-flight UPDATE_AVATAR_REQ. Replaced when a new upload starts
    /// (only one upload-in-flight per session is the realistic UX —
    /// the AccountTab disables its buttons during the round-trip).
    pub pending_avatar_update: Option<tokio::sync::oneshot::Sender<
        crate::net::proto::UpdateAvatarResponse,
    >>,

    /// In-flight FETCH_AVATAR_REQ calls keyed by target username. The
    /// router resolves each one when the matching FETCH_AVATAR_RES
    /// arrives. The caller drops its half after a 5s timeout.
    pub pending_avatar_fetches: HashMap<String, tokio::sync::oneshot::Sender<
        crate::net::proto::FetchAvatarResponse,
    >>,
```

In the `Default` impl, initialise:

```rust
            pending_avatar_update: None,
            pending_avatar_fetches: HashMap::new(),
```

- [ ] **Step 2: Compile**

```powershell
$env:VCPKG_ROOT = "C:\dev\vcpkg"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
cd electron-client/native
cargo build
```

Expected: succeeds (the fields are unused — warnings are fine; routes that populate them land in Task 10).

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/state.rs
git commit -m "feat(native): AppState slots for in-flight avatar requests

pending_avatar_update is a single oneshot (only one upload at a
time per session). pending_avatar_fetches is a map keyed by target
username, populated by fetch_avatar napi command and drained by
the FETCH_AVATAR_RES router arm landing in Task 10."
```

---

## Task 10: Native route_packets new arms

**Files:**
- Modify: `electron-client/native/src/net/central.rs`

- [ ] **Step 1: Add the three new packet handlers**

In the `route_packets` `match packet.payload` block, before the catch-all `_ => {}` arm, add:

```rust
                Some(packet::Payload::UpdateAvatarRes(resp)) => {
                    let mut s = state.lock().await;
                    if let Some(tx) = s.pending_avatar_update.take() {
                        let _ = tx.send(resp);
                    }
                }
                Some(packet::Payload::FetchAvatarRes(resp)) => {
                    let username = resp.username.clone();
                    let mut s = state.lock().await;
                    if let Some(tx) = s.pending_avatar_fetches.remove(&username) {
                        let _ = tx.send(resp);
                    }
                }
                Some(packet::Payload::AvatarChanged(resp)) => {
                    events::send(
                        "avatar_changed",
                        serde_json::json!({
                            "username": resp.username,
                            "version": resp.version,
                        }),
                    );
                }
```

- [ ] **Step 2: Compile**

```powershell
cd electron-client/native
cargo build
```

Expected: succeeds.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/net/central.rs
git commit -m "feat(native): route avatar response + change packets

UpdateAvatarRes / FetchAvatarRes resolve their respective in-flight
oneshots from AppState (set up by upload_avatar / fetch_avatar in
Tasks 11–12). AvatarChanged broadcasts forward to the renderer via
the existing JSON event bus."
```

---

## Task 11: Native `upload_avatar` napi command

**Files:**
- Modify: `electron-client/native/src/commands/auth.rs`

- [ ] **Step 1: Add the command**

Open `electron-client/native/src/commands/auth.rs`. At the bottom of the file, add:

```rust
#[napi(object)]
pub struct UploadAvatarResult {
    pub success: bool,
    pub message: String,
    pub version: String,
}

/// Upload (or remove) the authenticated user's avatar. Empty `jpeg`
/// argument = remove. Returns the server-computed sha256-hex version
/// on success.
#[napi]
pub async fn upload_avatar(
    jpeg: napi::bindgen_prelude::Buffer,
) -> napi::Result<UploadAvatarResult> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, UpdateAvatarRequest};

    let state_arc = crate::state::shared();
    let (tx, rx) = tokio::sync::oneshot::channel();

    let (write_tx, data) = {
        let mut s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let token = s.token.clone().unwrap_or_default();
        let write_tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        s.pending_avatar_update = Some(tx);
        let pkt = build_packet(
            packet::Type::UpdateAvatarReq,
            packet::Payload::UpdateAvatarReq(UpdateAvatarRequest {
                data: jpeg.as_ref().to_vec(),
            }),
            Some(&token),
        );
        (write_tx, pkt)
    };

    write_tx
        .send(data)
        .await
        .map_err(|_| napi::Error::from_reason("Send failed"))?;

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => Ok(UploadAvatarResult {
            success: resp.success,
            message: resp.message,
            version: resp.version,
        }),
        Ok(Err(_)) => Err(napi::Error::from_reason("Response channel closed")),
        Err(_) => {
            // Timeout — clear our slot so a stale response doesn't
            // resolve a different upload later.
            state_arc.lock().await.pending_avatar_update = None;
            Err(napi::Error::from_reason("Upload timed out"))
        }
    }
}
```

If the prost-generated message names differ (e.g. `UpdateAvatarReq` vs `UpdateAvatarRequest`), adjust to match what Task 2 actually produced. Run `cargo doc --open` on the native crate if you need to confirm names.

- [ ] **Step 2: Compile**

```powershell
cd electron-client/native
cargo build
```

Expected: succeeds.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/commands/auth.rs
git commit -m "feat(native): upload_avatar napi command

Builds an UPDATE_AVATAR_REQ, stashes a oneshot Sender on AppState,
awaits the response with a 5s timeout. Clears the slot on timeout
so a late response doesn't resolve a subsequent upload's promise.
Empty jpeg argument is a 'remove' (server handles)."
```

---

## Task 12: Native `fetch_avatar` napi command

**Files:**
- Modify: `electron-client/native/src/commands/auth.rs`

- [ ] **Step 1: Add the command alongside upload_avatar**

Append:

```rust
#[napi(object)]
pub struct FetchAvatarResult {
    pub version: String,
    /// Empty when version == ''. Renderer treats that as "no avatar set".
    pub data: napi::bindgen_prelude::Buffer,
}

/// Fetch a specific user's avatar bytes + current version. Empty
/// version + empty data means the user has no avatar (or doesn't
/// exist).
#[napi]
pub async fn fetch_avatar(username: String) -> napi::Result<FetchAvatarResult> {
    use crate::net::connection::build_packet;
    use crate::net::proto::{packet, FetchAvatarRequest};

    let state_arc = crate::state::shared();
    let (tx, rx) = tokio::sync::oneshot::channel();

    let (write_tx, data) = {
        let mut s = state_arc.lock().await;
        let central = s.central.as_ref().ok_or_else(|| {
            napi::Error::from_reason("Not connected to central server")
        })?;
        let token = s.token.clone().unwrap_or_default();
        let write_tx = central.connection_write_tx().ok_or_else(|| {
            napi::Error::from_reason("Central connection lost")
        })?;
        // If another fetch for the same username is already in flight,
        // replace its oneshot — the most recent caller wins. Both
        // promises won't resolve, but the late one just times out.
        s.pending_avatar_fetches.insert(username.clone(), tx);
        let pkt = build_packet(
            packet::Type::FetchAvatarReq,
            packet::Payload::FetchAvatarReq(FetchAvatarRequest {
                username: username.clone(),
            }),
            Some(&token),
        );
        (write_tx, pkt)
    };

    write_tx
        .send(data)
        .await
        .map_err(|_| napi::Error::from_reason("Send failed"))?;

    match tokio::time::timeout(std::time::Duration::from_secs(5), rx).await {
        Ok(Ok(resp)) => Ok(FetchAvatarResult {
            version: resp.version,
            data: resp.data.into(),
        }),
        Ok(Err(_)) => Err(napi::Error::from_reason("Response channel closed")),
        Err(_) => {
            state_arc
                .lock()
                .await
                .pending_avatar_fetches
                .remove(&username);
            Err(napi::Error::from_reason("Fetch timed out"))
        }
    }
}
```

- [ ] **Step 2: Compile and rebuild the napi addon**

```powershell
cd electron-client
$env:VCPKG_ROOT = "C:\dev\vcpkg"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
npm run build:native:debug
```

Expected: build succeeds; `upload_avatar` and `fetch_avatar` exports appear in `native/index.d.ts`.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/native/src/commands/auth.rs electron-client/native/index.d.ts electron-client/native/index.js
git commit -m "feat(native): fetch_avatar napi command

Same in-flight pattern as upload_avatar — oneshot per username
keyed in AppState.pending_avatar_fetches, drained by the
FETCH_AVATAR_RES router arm. 5s timeout, slot-cleanup on timeout."
```

---

## Task 13: Native FriendInfo + PresenceUpdate version forwarding

**Files:**
- Modify: `electron-client/native/src/net/central.rs`
- Modify: `electron-client/native/src/events.rs`

- [ ] **Step 1: Locate the FriendListRes handler in route_packets**

Find the existing `Some(packet::Payload::FriendListRes(resp)) => { ... }` arm. It currently iterates `resp.friends` and emits a `FriendInfo[]` event payload. Add `avatar_version` to each entry:

```rust
                Some(packet::Payload::FriendListRes(resp)) => {
                    let friends: Vec<events::FriendInfo> = resp
                        .friends
                        .into_iter()
                        .map(|f| events::FriendInfo {
                            username: f.username,
                            status: status_to_str(f.status),
                            avatar_version: f.avatar_version,    // NEW
                        })
                        .collect();
                    events::send(
                        "friend_list_received",
                        serde_json::json!({ "friends": friends }),
                    );
                }
```

- [ ] **Step 2: Update `events::FriendInfo` to include the new field**

In `electron-client/native/src/events.rs`, find the existing `FriendInfo` struct (or whatever type the friend-list event uses). Add:

```rust
#[derive(Serialize)]
pub struct FriendInfo {
    pub username: String,
    pub status: String,
    pub avatar_version: String,   // NEW; '' when no avatar
}
```

- [ ] **Step 3: Find the PresenceUpdate handler**

Locate `Some(packet::Payload::PresenceUpdate(resp)) => { ... }` in `route_packets`. The shape just changed — `resp.users` is now `Vec<UserPresence>`. Update the event payload:

```rust
                Some(packet::Payload::PresenceUpdate(resp)) => {
                    let users: Vec<events::UserPresence> = resp
                        .users
                        .into_iter()
                        .map(|u| events::UserPresence {
                            username: u.username,
                            avatar_version: u.avatar_version,
                        })
                        .collect();
                    events::send(
                        "presence_update",
                        serde_json::json!({ "users": users }),
                    );
                }
```

Add the `UserPresence` event type to `events.rs`:

```rust
#[derive(Serialize)]
pub struct UserPresence {
    pub username: String,
    pub avatar_version: String,
}
```

- [ ] **Step 4: Compile**

```powershell
cd electron-client/native
cargo build
```

Expected: succeeds.

- [ ] **Step 5: Commit**

```powershell
git add electron-client/native/src/net/central.rs electron-client/native/src/events.rs
git commit -m "feat(native): forward avatar_version on FriendInfo + PresenceUpdate

Wire-side FriendInfo + UserPresence rows now carry the version.
Renderer-side avatarStore (Task 15) consumes these to invalidate
its cache when versions diverge."
```

---

## Task 14: Renderer type updates

**Files:**
- Modify: `electron-client/src/types/index.ts`

- [ ] **Step 1: Update `FriendInfo` to include `avatarVersion`**

Locate the existing `FriendInfo` interface in `electron-client/src/types/index.ts`. Add:

```typescript
export interface FriendInfo {
  username: string;
  status: "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked";
  avatarVersion: string;        // NEW; '' when no avatar set
}
```

- [ ] **Step 2: Add a `UserPresence` type**

Anywhere in the file:

```typescript
export interface UserPresence {
  username: string;
  avatarVersion: string;
}
```

- [ ] **Step 3: Typecheck**

```powershell
cd electron-client
npm run typecheck
```

Expected: existing call sites that read `friend.username` / `friend.status` keep working (new field is additive). Any code that consumed the old `presence_update` payload as `string[]` will error — those errors land in Task 19.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/src/types/index.ts
git commit -m "feat(types): FriendInfo.avatarVersion + UserPresence

Mirrors the protobuf shape change. Existing consumers see additive
fields; PresenceUpdate consumers that destructured online_users as
string[] will need updating (handled in Task 19)."
```

---

## Task 15: Renderer avatarStore (Zustand)

**Files:**
- Create: `electron-client/src/stores/avatarStore.ts`

- [ ] **Step 1: Write the store**

```typescript
import { create } from "zustand";
import { invoke } from "../lib/ipc";

type AvatarEntry = {
  version: string;
  data: Uint8Array | null;
  blobUrl: string | null;
  status: "idle" | "loading" | "loaded" | "missing" | "error";
};

interface AvatarStoreState {
  entries: Map<string, AvatarEntry>;
  /// Update the known current version for a user. If it differs from
  /// the cached entry's version, invalidate (revoke blobUrl, mark
  /// idle) so the next render fetches fresh.
  setVersion: (username: string, version: string) => void;
  /// Trigger a fetch if the entry is `idle` (just-invalidated or
  /// never-seen). No-op when already loading/loaded/missing/error.
  fetchIfNeeded: (username: string) => void;
  /// Drop the cached entry, revoke its blob URL, mark idle. Internal
  /// helper called by setVersion when versions differ.
  invalidate: (username: string) => void;
  /// Logout cleanup: revoke every blob URL, clear the map.
  clearAll: () => void;
}

export const useAvatarStore = create<AvatarStoreState>((set, get) => ({
  entries: new Map(),

  setVersion: (username, version) => {
    const existing = get().entries.get(username);
    if (existing && existing.version === version) return;
    get().invalidate(username);
    set((s) => {
      const next = new Map(s.entries);
      next.set(username, {
        version,
        data: null,
        blobUrl: null,
        status: "idle",
      });
      return { entries: next };
    });
  },

  fetchIfNeeded: (username) => {
    const entry = get().entries.get(username);
    if (entry && entry.status !== "idle") return;
    set((s) => {
      const next = new Map(s.entries);
      const cur = next.get(username) ?? {
        version: "",
        data: null,
        blobUrl: null,
        status: "idle" as const,
      };
      next.set(username, { ...cur, status: "loading" });
      return { entries: next };
    });
    void (async () => {
      try {
        const result = (await invoke("fetch_avatar", { username })) as {
          version: string;
          data: Uint8Array;
        };
        set((s) => {
          const next = new Map(s.entries);
          if (!result.version || result.data.byteLength === 0) {
            next.set(username, {
              version: result.version,
              data: null,
              blobUrl: null,
              status: "missing",
            });
          } else {
            const blob = new Blob([result.data as BlobPart], {
              type: "image/jpeg",
            });
            const blobUrl = URL.createObjectURL(blob);
            next.set(username, {
              version: result.version,
              data: result.data,
              blobUrl,
              status: "loaded",
            });
          }
          return { entries: next };
        });
      } catch (e) {
        console.warn(`[avatarStore] fetch failed for ${username}:`, e);
        set((s) => {
          const next = new Map(s.entries);
          const cur = next.get(username);
          if (cur) next.set(username, { ...cur, status: "error" });
          return { entries: next };
        });
      }
    })();
  },

  invalidate: (username) => {
    const entry = get().entries.get(username);
    if (entry?.blobUrl) URL.revokeObjectURL(entry.blobUrl);
    set((s) => {
      const next = new Map(s.entries);
      next.delete(username);
      return { entries: next };
    });
  },

  clearAll: () => {
    for (const e of get().entries.values()) {
      if (e.blobUrl) URL.revokeObjectURL(e.blobUrl);
    }
    set({ entries: new Map() });
  },
}));
```

- [ ] **Step 2: Typecheck**

```powershell
cd electron-client
npm run typecheck
```

Expected: clean.

- [ ] **Step 3: Commit**

```powershell
git add electron-client/src/stores/avatarStore.ts
git commit -m "feat(stores): avatarStore — Zustand cache keyed by username

Owns blob-URL lifecycle for avatar bytes. setVersion is the cache-
invalidation primitive (called by FriendListRes / PresenceUpdate /
AvatarChanged listeners in Task 18). fetchIfNeeded is reactive,
called by UserAvatar on mount when the entry is idle. clearAll
runs on logout."
```

---

## Task 16: Renderer LetterAvatar + UserAvatar

**Files:**
- Create: `electron-client/src/components/LetterAvatar.tsx`
- Create: `electron-client/src/components/UserAvatar.tsx`

- [ ] **Step 1: Write `LetterAvatar` (extract from existing inline markup)**

```tsx
// electron-client/src/components/LetterAvatar.tsx
interface Props {
  username: string;
  size: number;
  className?: string;
}

/// Letter-in-coloured-square fallback. Used standalone where no
/// avatar makes sense, and inside UserAvatar for the loading /
/// missing / error states. Background colour is deterministic on
/// the username so it stays stable across mounts.
export function LetterAvatar({ username, size, className }: Props) {
  const initial = username.charAt(0).toUpperCase() || "?";
  // Pick from the project's existing palette — match the look of the
  // current inline markup. The bg-* tokens are defined in
  // tailwind.config / styles/globals.css.
  const bg = "bg-bg-tertiary"; // adjust if the existing markup uses a different token
  return (
    <div
      className={`flex items-center justify-center rounded-md text-text-primary font-medium ${bg} ${className ?? ""}`}
      style={{
        width: size,
        height: size,
        fontSize: Math.max(10, Math.floor(size * 0.42)),
      }}
    >
      {initial}
    </div>
  );
}
```

If the existing call sites use a different rounding token (`rounded-lg` vs `rounded-md` vs `rounded-xl`), match the dominant pattern — Account-tab + the chat sender avatar are the easiest places to find the canonical style. Hardcode here, the user can tune in a follow-up.

- [ ] **Step 2: Write `UserAvatar`**

```tsx
// electron-client/src/components/UserAvatar.tsx
import { useEffect } from "react";
import { useAvatarStore } from "../stores/avatarStore";
import { LetterAvatar } from "./LetterAvatar";

interface Props {
  username: string;
  size: number;
  className?: string;
}

/// Self-fetching avatar. Reads from avatarStore; when the entry is
/// idle it triggers fetchIfNeeded(). Renders an <img> when loaded,
/// LetterAvatar fallback otherwise (loading / missing / error).
export function UserAvatar({ username, size, className }: Props) {
  const entry = useAvatarStore((s) => s.entries.get(username));
  const fetchIfNeeded = useAvatarStore((s) => s.fetchIfNeeded);

  useEffect(() => {
    fetchIfNeeded(username);
  }, [username, fetchIfNeeded, entry?.status]);

  if (entry?.status === "loaded" && entry.blobUrl) {
    return (
      <img
        src={entry.blobUrl}
        alt={username}
        className={`rounded-md object-cover ${className ?? ""}`}
        style={{ width: size, height: size }}
        draggable={false}
      />
    );
  }
  return <LetterAvatar username={username} size={size} className={className} />;
}
```

- [ ] **Step 3: Typecheck**

```powershell
cd electron-client
npm run typecheck
```

Expected: clean.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/src/components/LetterAvatar.tsx electron-client/src/components/UserAvatar.tsx
git commit -m "feat(components): UserAvatar + LetterAvatar

UserAvatar is the shared self-fetching component for every avatar
site. Reads from avatarStore; triggers fetchIfNeeded on mount when
idle; renders <img src={blobUrl}> when loaded, LetterAvatar fallback
otherwise. LetterAvatar lifts the existing letter-in-coloured-square
markup into a reusable component."
```

---

## Task 17: Renderer AvatarCropperModal

**Files:**
- Create: `electron-client/src/features/settings/AvatarCropperModal.tsx`

- [ ] **Step 1: Write the cropper**

```tsx
// electron-client/src/features/settings/AvatarCropperModal.tsx
import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";

interface Props {
  file: File;
  onSave: () => void;       // called on successful upload
  onCancel: () => void;
}

const VIEWPORT = 320;          // on-screen crop window edge (px)
const OUTPUT = 256;            // final JPEG edge (px)
const JPEG_QUALITY = 0.85;

/// Square-crop modal. Loads the picked file as an HTMLImageElement,
/// shows it inside a 320×320 viewport. Mouse wheel zooms, drag pans.
/// On Save: draws the visible viewport region to a 256×256 canvas,
/// JPEG-encodes at quality 0.85, ships bytes via upload_avatar.
export function AvatarCropperModal({ file, onSave, onCancel }: Props) {
  const imgRef = useRef<HTMLImageElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [imgLoaded, setImgLoaded] = useState(false);
  const [pos, setPos] = useState({ x: 0, y: 0 });
  const [scale, setScale] = useState(1);
  const [dragging, setDragging] = useState(false);
  const dragStart = useRef<{ x: number; y: number } | null>(null);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load the file as an HTMLImageElement.
  useEffect(() => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      imgRef.current = img;
      // Centre the image in the viewport, with the initial scale set
      // so the shorter edge fits exactly.
      const initial = Math.max(
        VIEWPORT / img.width,
        VIEWPORT / img.height,
      );
      setScale(initial);
      setPos({
        x: (VIEWPORT - img.width * initial) / 2,
        y: (VIEWPORT - img.height * initial) / 2,
      });
      setImgLoaded(true);
    };
    img.src = url;
    return () => URL.revokeObjectURL(url);
  }, [file]);

  // Repaint preview canvas on every state change.
  useEffect(() => {
    if (!imgLoaded || !imgRef.current || !canvasRef.current) return;
    const ctx = canvasRef.current.getContext("2d");
    if (!ctx) return;
    ctx.clearRect(0, 0, VIEWPORT, VIEWPORT);
    ctx.drawImage(
      imgRef.current,
      pos.x,
      pos.y,
      imgRef.current.width * scale,
      imgRef.current.height * scale,
    );
  }, [imgLoaded, pos, scale]);

  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    setScale((s) => Math.max(0.1, Math.min(10, s * factor)));
  };

  const onMouseDown = (e: React.MouseEvent) => {
    setDragging(true);
    dragStart.current = { x: e.clientX - pos.x, y: e.clientY - pos.y };
  };
  const onMouseMove = (e: React.MouseEvent) => {
    if (!dragging || !dragStart.current) return;
    setPos({
      x: e.clientX - dragStart.current.x,
      y: e.clientY - dragStart.current.y,
    });
  };
  const onMouseUp = () => {
    setDragging(false);
    dragStart.current = null;
  };

  const handleSave = async () => {
    if (!imgLoaded || !imgRef.current) return;
    setUploading(true);
    setError(null);
    try {
      const out = new OffscreenCanvas(OUTPUT, OUTPUT);
      const octx = out.getContext("2d");
      if (!octx) throw new Error("OffscreenCanvas 2d context unavailable");
      const ratio = OUTPUT / VIEWPORT;
      octx.drawImage(
        imgRef.current,
        pos.x * ratio,
        pos.y * ratio,
        imgRef.current.width * scale * ratio,
        imgRef.current.height * scale * ratio,
      );
      const blob = await out.convertToBlob({
        type: "image/jpeg",
        quality: JPEG_QUALITY,
      });
      const buf = await blob.arrayBuffer();
      const result = (await invoke("upload_avatar", {
        jpeg: new Uint8Array(buf),
      })) as { success: boolean; message: string; version: string };
      if (!result.success) throw new Error(result.message || "Upload failed");
      onSave();
    } catch (e) {
      setError(String(e));
    } finally {
      setUploading(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && onCancel()}
    >
      <div className="rounded-2xl border border-border bg-bg-dark p-6">
        <p className="mb-3 font-display text-[15px] font-semibold text-text-primary">
          Crop your picture
        </p>
        <canvas
          ref={canvasRef}
          width={VIEWPORT}
          height={VIEWPORT}
          onWheel={onWheel}
          onMouseDown={onMouseDown}
          onMouseMove={onMouseMove}
          onMouseUp={onMouseUp}
          onMouseLeave={onMouseUp}
          className="cursor-grab rounded-md bg-bg-darkest"
          style={{ width: VIEWPORT, height: VIEWPORT }}
        />
        {error && <p className="mt-3 text-[12px] text-error">{error}</p>}
        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={onCancel}
            disabled={uploading}
            className="rounded-md border border-border px-4 py-2 text-[13px] text-text-secondary hover:bg-bg-light"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={uploading || !imgLoaded}
            className="rounded-md bg-accent px-4 py-2 text-[13px] font-semibold text-white hover:bg-accent-hover disabled:opacity-50"
          >
            {uploading ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
```

- [ ] **Step 2: Typecheck**

```powershell
cd electron-client
npm run typecheck
```

Expected: clean. (Some `accent-hover` Tailwind tokens etc. might not exist in your palette — adjust to the project's actual tokens.)

- [ ] **Step 3: Commit**

```powershell
git add electron-client/src/features/settings/AvatarCropperModal.tsx
git commit -m "feat(settings): AvatarCropperModal — square pan/zoom crop

Loads picked File as HTMLImageElement, displays inside a 320×320
viewport. Wheel zooms, drag pans. On Save: draws viewport region to
an OffscreenCanvas at 256×256, JPEG-encodes at quality 0.85, ships
bytes via upload_avatar napi. Inline implementation — no external
crop library."
```

---

## Task 18: AccountTab upload UI

**Files:**
- Modify: `electron-client/src/features/settings/tabs/AccountTab.tsx`

- [ ] **Step 1: Read the current AccountTab structure**

Open the file and read the existing layout. Add the avatar UI at the top of the tab's main column (before the existing settings fields).

- [ ] **Step 2: Add the avatar section**

At the top of the existing JSX `return`, immediately inside the tab's root element, insert:

```tsx
import { useState, useRef } from "react";
import { useAuthStore } from "../../../stores/authStore";
import { useAvatarStore } from "../../../stores/avatarStore";
import { invoke } from "../../../lib/ipc";
import { UserAvatar } from "../../../components/UserAvatar";
import { AvatarCropperModal } from "../AvatarCropperModal";
// ... existing imports

export default function AccountTab() {
  const username = useAuthStore((s) => s.username);
  const fileInputRef = useRef<HTMLInputElement | null>(null);
  const [pickedFile, setPickedFile] = useState<File | null>(null);
  const entry = useAvatarStore((s) =>
    username ? s.entries.get(username) : undefined,
  );
  const hasAvatar = entry?.status === "loaded";

  const onFilePicked = (e: React.ChangeEvent<HTMLInputElement>) => {
    const f = e.target.files?.[0];
    if (f) setPickedFile(f);
    e.target.value = "";
  };
  const removeAvatar = async () => {
    if (!username) return;
    try {
      await invoke("upload_avatar", { jpeg: new Uint8Array(0) });
      // AvatarChanged event from the server will invalidate the cache.
    } catch (e) {
      console.error("[AccountTab] remove avatar failed:", e);
    }
  };

  return (
    <div className="...">  {/* existing root */}
      {username && (
        <div className="mb-6 flex items-center gap-4">
          <UserAvatar username={username} size={96} />
          <div className="flex flex-col gap-2">
            <button
              onClick={() => fileInputRef.current?.click()}
              className="rounded-md bg-accent px-4 py-2 text-[13px] font-semibold text-white hover:bg-accent-hover"
            >
              Change picture
            </button>
            {hasAvatar && (
              <button
                onClick={removeAvatar}
                className="rounded-md border border-border px-4 py-2 text-[13px] text-text-secondary hover:bg-bg-light"
              >
                Remove picture
              </button>
            )}
          </div>
          <input
            ref={fileInputRef}
            type="file"
            accept="image/jpeg,image/png,image/webp"
            hidden
            onChange={onFilePicked}
          />
        </div>
      )}
      {pickedFile && (
        <AvatarCropperModal
          file={pickedFile}
          onSave={() => setPickedFile(null)}
          onCancel={() => setPickedFile(null)}
        />
      )}
      {/* existing settings fields below */}
    </div>
  );
}
```

If the file already has a default export with a different shape, integrate the section into it preserving the existing fields. Adjust import paths if the relative `../../../` depth differs.

- [ ] **Step 3: Typecheck**

```powershell
cd electron-client
npm run typecheck
```

Expected: clean.

- [ ] **Step 4: Commit**

```powershell
git add electron-client/src/features/settings/tabs/AccountTab.tsx
git commit -m "feat(settings): account-tab avatar upload + remove

UserAvatar preview at 96 px next to two buttons. Change picture
opens the hidden <input type=file>, picked file drives the cropper
modal. Remove picture sends an empty upload — server interprets
as removal. AvatarChanged event from the server invalidates the
cache, UserAvatar re-renders with the result."
```

---

## Task 19: Wire events into avatarStore

**Files:**
- Modify: `electron-client/src/features/auth/useAuthEvents.ts`
- Modify: `electron-client/src/features/friends/useFriendsEvents.ts`
- Modify whichever file consumes the `presence_update` event (search for it)

- [ ] **Step 1: Listen for `avatar_changed` in useAuthEvents**

In `useAuthEvents.ts`, alongside the existing event listeners, add:

```typescript
import { useAvatarStore } from "../../stores/avatarStore";

// inside the listener-setup block:
promises.push(
  listen<{ username: string; version: string }>("avatar_changed", ({ payload }) => {
    useAvatarStore.getState().setVersion(payload.username, payload.version);
  }),
);
```

Also clear the cache on `logged_out`:

```typescript
promises.push(
  listen("logged_out", () => {
    useAvatarStore.getState().clearAll();
    // existing logged_out handling continues below
  }),
);
```

If a `logged_out` listener already exists, fold the `clearAll()` into its body.

- [ ] **Step 2: Listen for FriendListRes version updates**

In `useFriendsEvents.ts`, find the existing listener for `friend_list_received`. The payload now includes `avatarVersion` per entry. After the existing logic that stores friends in the state, add:

```typescript
import { useAvatarStore } from "../../stores/avatarStore";

// inside the friend_list_received handler, after the existing setFriends(...) call:
const setVersion = useAvatarStore.getState().setVersion;
for (const f of payload.friends) {
  setVersion(f.username, f.avatarVersion);
}
```

- [ ] **Step 3: Listen for PresenceUpdate version updates**

Search the codebase for where `presence_update` is consumed:

```powershell
cd electron-client
Select-String -Path "src/**/*.ts","src/**/*.tsx" -Pattern '"presence_update"'
```

In that file's listener, the payload shape changed from `{ online_users: string[] }` to `{ users: UserPresence[] }`. Update the destructuring and add the version forwarding:

```typescript
import { useAvatarStore } from "../../stores/avatarStore";
import type { UserPresence } from "../../types";

// inside the listener:
listen<{ users: UserPresence[] }>("presence_update", ({ payload }) => {
  const setVersion = useAvatarStore.getState().setVersion;
  for (const u of payload.users) {
    setVersion(u.username, u.avatarVersion);
  }
  // existing presence-related state updates with payload.users.map(u => u.username) etc.
});
```

Any existing code that read `online_users: string[]` needs to extract `payload.users.map(u => u.username)` to preserve old behaviour.

- [ ] **Step 4: Typecheck**

```powershell
cd electron-client
npm run typecheck
```

Expected: clean. If there are TS errors from `online_users` references, fix them — they were the wire-breaking surface and now read `users`.

- [ ] **Step 5: Commit**

```powershell
git add electron-client/src/features/auth/useAuthEvents.ts electron-client/src/features/friends/useFriendsEvents.ts # plus the presence_update listener file
git commit -m "feat(events): wire avatarStore to friend-list / presence / avatar-changed

avatar_changed → setVersion (immediate invalidation)
friend_list_received → setVersion for each FriendInfo
presence_update → setVersion for each UserPresence
logged_out → clearAll (revokes every cached blob URL).
Existing online_users[] consumers now derive their string array
from payload.users.map(u => u.username)."
```

---

## Task 20: Replace inline letter avatars at every call site

**Files (one commit per file, or batch as you prefer):**
- Modify: `electron-client/src/features/channels/UserPanel.tsx`
- Modify: `electron-client/src/features/channels/ConversationSidebar.tsx`
- Modify: `electron-client/src/features/friends/MembersList.tsx`
- Modify: `electron-client/src/features/dm/UserProfilePopup.tsx`
- Modify: `electron-client/src/features/servers/MembersAdminPanel.tsx`
- Modify: `electron-client/src/features/voice/VoicePanel.tsx`
- Modify: any chat message-sender avatar component (e.g. `electron-client/src/components/editor/MessageDelegate.tsx` — verify with grep below)

- [ ] **Step 1: Locate every inline letter-avatar markup**

Run:

```powershell
cd electron-client
Select-String -Path "src/**/*.tsx" -Pattern "username\.charAt\(0\)"
```

Each match is a call site. Expect the list from spec §7 — verify the actual files.

- [ ] **Step 2: For each call site, swap the inline markup for UserAvatar**

The general pattern: an existing `<div className="... rounded-... bg-... w-N h-N ...">{username.charAt(0).toUpperCase()}</div>` becomes `<UserAvatar username={...} size={N} />`. Preserve any wrapping container that adds an online-status dot or ring decoration — only the inner letter-square is replaced.

Example for `UserPanel.tsx` line 190:

```tsx
// OLD:
<div className="flex h-8 w-8 items-center justify-center rounded-md bg-bg-tertiary text-text-primary">
  {username.charAt(0).toUpperCase()}
</div>

// NEW:
<UserAvatar username={username} size={32} />
```

Add the import at the top of each file: `import { UserAvatar } from "../../../components/UserAvatar";` (adjust depth).

Use the sizes from spec §7:

| File | Size |
|------|------|
| `UserPanel.tsx` | 32 |
| `ConversationSidebar.tsx` | 32 |
| `MembersList.tsx` | 32 |
| `UserProfilePopup.tsx` | 64 |
| `MembersAdminPanel.tsx` | 32 |
| `VoicePanel.tsx` | 40 |
| Chat message-sender avatar | 40 |

- [ ] **Step 3: Typecheck and visual smoke (run dev briefly)**

```powershell
cd electron-client
npm run typecheck
```

Then run `npm run dev` once and visit each of the listed UI surfaces, confirming that avatars render (letter fallback at minimum; image once an avatar is uploaded). No errors in the renderer console.

- [ ] **Step 4: Commit (single commit covering every call site)**

```powershell
git add electron-client/src/features/channels/UserPanel.tsx electron-client/src/features/channels/ConversationSidebar.tsx electron-client/src/features/friends/MembersList.tsx electron-client/src/features/dm/UserProfilePopup.tsx electron-client/src/features/servers/MembersAdminPanel.tsx electron-client/src/features/voice/VoicePanel.tsx # plus the chat message-sender file
git commit -m "feat(ui): swap inline letter avatars for <UserAvatar /> everywhere

Every existing username.charAt(0).toUpperCase() markup replaced
with the shared component. Sizes preserved per design §7 (32 in
sidebars, 40 in voice tiles + chat messages, 64 in profile popup).
Online-status dots and ring decorations on wrapping containers
remain untouched — UserAvatar is just the inner letter-or-image
swap."
```

---

## Task 21: Manual integration test pass

**Files:** none (verification only)

- [ ] **Step 1: Rebuild server + run it**

```powershell
cmake --build build-servers --config Release
.\build-servers\Release\decibell-central.exe
```

Expected: server starts; `[Server] ... migration` logs (if any) appear cleanly; the new columns are present in Postgres (verify with `psql -c "\d users"` if convenient).

- [ ] **Step 2: Rebuild client + run dev**

```powershell
$env:VCPKG_ROOT = "C:\dev\vcpkg"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"
$env:PATH = "C:\dev\vcpkg\installed\x64-windows\bin;" + $env:PATH
cd electron-client
npm run dev
```

- [ ] **Step 3: Walk the test matrix from spec §9**

For each row, perform the action and confirm the expected outcome:

- [ ] **Upload + self-display:** Settings → Account → Change picture → pick a JPEG → drag/zoom in the cropper → Save. Your avatar appears in the AccountTab preview immediately. Open Friends panel — your own entry in the members list shows the picture.
- [ ] **Friend sees update mid-session:** With a second client logged in as a friend, repeat upload. Within ~1s the friend's UI shows your new avatar without a refresh.
- [ ] **Cold-launch with cached friend:** Restart your client (clears the in-memory cache). On login, friend list arrives with `avatarVersion`; UserAvatar fetches on first render; image swaps in within a few hundred ms (letter visible during the fetch).
- [ ] **Remove avatar:** Click Remove picture. Self-preview falls back to letter; friend's view of you also falls back.
- [ ] **Reject malformed:** Open DevTools, simulate via console: `await window.decibell.invoke("upload_avatar", { jpeg: new Uint8Array([1,2,3,4]) })`. Server replies `{ success: false, message: "Not a JPEG" }`.
- [ ] **Reject oversize:** Create a >200 KB buffer of `0xFF, 0xD8` + zeros: `await window.decibell.invoke("upload_avatar", { jpeg: new Uint8Array([0xFF, 0xD8, ...new Array(210*1024).fill(0)]) })`. Server replies `{ success: false, message: "Avatar too large" }`.
- [ ] **Two rapid updates:** Upload twice within ~1 second. Both `AvatarChanged` events arrive; the cache invalidates twice; the final image is the second upload's bytes (verified by re-fetching at the friend client).

- [ ] **Step 4: Resource-usage spot check**

While streaming idle, open Task Manager → Memory. Expect ~no measurable RAM bump from this feature vs the prior release (avatars are ~50 KB × N friends; e.g. 50 friends = ~2.5 MB of blobs).

- [ ] **Step 5: Version bump + commit**

Per the project's standard six-file checklist (per memory `feedback_version_bump.md`), bump:
- `electron-client/package.json` version → 0.6.3
- `electron-client/package-lock.json` (×2 occurrences) → 0.6.3
- `electron-client/src/features/auth/LoginPage.tsx` UI footer string
- `electron-client/src/features/settings/tabs/AboutTab.tsx` UI version label
- `aur/PKGBUILD` `pkgver`
- `aur/.SRCINFO` `pkgver` + `source` URL

```powershell
# After editing all six:
git add electron-client/package.json electron-client/package-lock.json electron-client/src/features/auth/LoginPage.tsx electron-client/src/features/settings/tabs/AboutTab.tsx aur/PKGBUILD aur/.SRCINFO
git commit -m "release: 0.6.3 — custom profile pictures"
```

- [ ] **Step 6: Tag + push**

```powershell
git tag -a ev0.6.3 -m "Decibell 0.6.3 — custom profile pictures"
git push origin main
git push origin ev0.6.3
```

CI builds + uploads release artifacts. Confirm by checking the Actions tab.

---

## Self-review pass

After writing the plan, checked against the spec:

**Spec coverage:**
- §1 (Goal): no implementation; informational. ✓
- §2 (Scope in/out): Tasks 1–20 cover scope; deferred items remain deferred. ✓
- §3 (Architecture): Tasks 4–13 implement the wire / native / server pipeline; Task 15 implements the renderer cache. ✓
- §4 (Protobuf): Task 1. ✓
- §5 (Server): Tasks 3 (migration) + 4 (sha256) + 5 (broadcast helper) + 6 (UPDATE) + 7 (FETCH) + 8 (FriendInfo/PresenceUpdate). ✓
- §6 (Native): Tasks 9 (state) + 10 (routes) + 11 (upload) + 12 (fetch) + 13 (event forwarding). ✓
- §7 (Renderer):
  - avatarStore — Task 15 ✓
  - UserAvatar / LetterAvatar — Task 16 ✓
  - AvatarCropperModal — Task 17 ✓
  - AccountTab — Task 18 ✓
  - Call-site swaps — Task 20 ✓
  - Event listeners — Task 19 ✓
- §8 (Failure handling): covered inline across server (Tasks 6–7), native (Tasks 11–12), renderer (Tasks 15–17). ✓
- §9 (Testing): Task 21. ✓
- §10 (Rollback): informational; no implementation. ✓
- §11 (Deferred): explicit out-of-scope; no tasks. ✓

**No spec requirement is unmatched to a task.**

**Placeholder scan:** No "TBD" / "implement later" / "fill in" markers. Several places say "adjust if the existing pattern differs" — those are direct instructions, not placeholders (each one is paired with the search command that resolves the ambiguity).

**Type consistency:**
- `avatar_version` (server) / `avatarVersion` (TS) consistent — Tasks 1, 8, 13, 14, 15, 19.
- `pending_avatar_update` / `pending_avatar_fetches` (Rust AppState field names) used identically in Tasks 9, 10, 11, 12.
- `UserAvatar` / `LetterAvatar` / `avatarStore` / `useAvatarStore` consistent in Tasks 15, 16, 18, 19, 20.
- Sizes (32, 40, 64, 96) consistent between spec §7 and Task 20.
- `UPDATE_AVATAR_REQ` / `UPDATE_AVATAR_RES` / `FETCH_AVATAR_REQ` / `FETCH_AVATAR_RES` / `AVATAR_CHANGED` enum identifiers consistent across Tasks 1, 6, 7, 10.

No type-consistency issues detected.
