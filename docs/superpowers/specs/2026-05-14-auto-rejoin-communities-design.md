# Auto-Rejoin Community Servers — Design

**Date:** 2026-05-14
**Author:** sunkhan (with Claude)
**Status:** Approved (pending implementation plan)

## Problem

When the Decibell client restarts (or the user signs in fresh), their server bar starts empty. To get back to community servers they're already a member of, they have to navigate to ServerBrowseView and manually click "Connect" on each one. Discord-style behavior — the server bar populating itself on login with every community the user belongs to — does not exist.

The architectural blocker: today, Decibell tracks community-server *membership* only on the community server itself (each one is independent and authoritative for its own member table). Central has a `community_servers` directory table — a catalogue of all known community servers populated by `SERVER_HEARTBEAT` packets — but no per-user data linking specific users to specific communities.

## Goal

After login completes, the client populates the server bar with every community server the user is a member of, automatically connecting to each in parallel without the user pressing anything. Stale memberships (kicks/bans/leaves that occurred while the user was offline) are cleaned up gracefully without spamming the UI.

Cross-device portable: the user can sign in on a different machine or after reinstalling, and the same server list appears — because the source of truth is central, not the local client.

## Non-goals

- **Cross-organization sync** — community servers stay independent; nothing in this design federates between community servers.
- **Membership-event push to the client** — if you're kicked while offline, you see the revocation when you next log in (toast + tile disappears). No live notification while offline.
- **Channel-level granularity** — this tracks "user is a member of community X", not "user joined channel Y". Channel state stays per-community-server like today.
- **Backfilling the central table from cold** — handled lazily: every successful community-server auth idempotently re-registers the user's membership with central, so existing memberships flow in naturally on the next reconnect.

## Architecture

```
┌──────────┐                                         ┌──────────┐
│ central  │                                         │community │
│          │ ◄────────── MEMBERSHIP_REGISTER_REQ ─── │ server   │
│ users    │              (on every successful auth)│          │
│ communi- │                                         │ members  │
│  ties    │ ◄────────── MEMBERSHIP_REVOKE_REQ ──── │ (DB)     │
│ user_    │              (kick/ban/leave path)     │          │
│  commu-  │                                         └──────────┘
│  nities  │
└────┬─────┘
     │   LoginResponse {
     │     ... existing fields ...
     │     memberships: [CommunityServerInfo, ...]
     │   }
     ▼
┌──────────┐    for each membership →    ┌──────────┐
│ client   │ ──── connect_to_community ──► │community │
│ (native) │                             │ servers  │
└──────────┘                             └──────────┘
     │
     │   emits memberships_received(list)
     ▼
┌──────────┐
│ renderer │ → pre-populate server-bar tiles as "connecting…"
│          │ → tiles flip to connected as each community_auth_responded lands
│          │ → on auth-fail = revoked: drop tile + toast + fire
│          │   request_drop_membership(server_id) to clean up central
└──────────┘
```

## Section 1: Schema + data model on central

One new table, added to `AuthManager::initializeDatabase()` via idempotent `CREATE TABLE IF NOT EXISTS` alongside the existing `users` / `friends` / `dm_*` blocks:

```sql
CREATE TABLE IF NOT EXISTS user_communities (
  username VARCHAR(32) NOT NULL,
  server_id BIGINT NOT NULL,
  joined_at BIGINT NOT NULL,
  PRIMARY KEY (username, server_id)
);

-- Per-user lookup on login (the only access pattern that isn't
-- already covered by the PRIMARY KEY).
CREATE INDEX IF NOT EXISTS user_communities_user_idx
  ON user_communities (username);
```

`server_id` is the same primary-key id used by the existing `community_servers` directory table. A simple JOIN on `server_id` gives us {username, server_id, name, host_ip, port, …} in one query — no separate directory lookup on the client.

**Three new `AuthManager` methods:**

```cpp
/// Idempotent insert. Called on every successful community auth (see
/// §2) so the row always exists. Race-safe: ON CONFLICT DO NOTHING.
void registerMembership(const std::string& username, int64_t server_id);

/// Idempotent delete. Called by the community-side kick/ban/leave
/// path (via MEMBERSHIP_REVOKE_REQ) and by the client-side
/// stale-membership cleanup (§3).
void revokeMembership(const std::string& username, int64_t server_id);

/// Returns CommunityServerInfo for every community the user belongs
/// to, with the directory info inline so the client can connect
/// without an extra round-trip. Filters out orphans (rows whose
/// server_id no longer matches a community_servers entry — server
/// was deleted from the directory).
std::vector<CommunityServerInfo> getUserCommunities(
    const std::string& username);
```

`getUserCommunities` SQL:
```sql
SELECT cs.id, cs.name, cs.description, cs.host_ip, cs.port, cs.member_count
FROM user_communities uc
JOIN community_servers cs ON cs.id = uc.server_id
WHERE uc.username = $1
ORDER BY uc.joined_at;
```

Storage cost: ~40 B per row. Even at 100 users × 20 servers each = 80 KB total. Negligible.

## Section 2: Wire protocol + community→central sync

### Two new packet types

```proto
MEMBERSHIP_REGISTER_REQ = 73;   // community→central (shared secret)
MEMBERSHIP_REVOKE_REQ   = 74;   // community→central (shared secret)
                                 //   AND client→central (JWT, for
                                 //   stale-membership cleanup §3)
```

### Two new messages

```proto
message MembershipRegisterReq {
  string username = 1;
  int64  server_id = 2;
}
message MembershipRevokeReq {
  string username = 1;
  int64  server_id = 2;
}
```

Both fire-and-forget — no response. Loss tolerance: a dropped `MEMBERSHIP_REGISTER_REQ` is recovered on the next successful community auth (idempotent re-fire). A dropped `MEMBERSHIP_REVOKE_REQ` results in central briefly thinking the user is still a member; on next login the client tries to auto-connect, gets rejected by the community, and fires the client-side stale-membership cleanup.

### Where community fires them

- `MEMBERSHIP_REGISTER_REQ` — at the tail of every successful `community_auth` handler, after the JWT-validated authentication succeeds. **This is also the bootstrap mechanism** for existing memberships: the first time any current member of any current community reconnects after this feature ships, their `user_communities` row gets created automatically. No migration script needed.
- `MEMBERSHIP_REVOKE_REQ` — in the existing kick/ban/leave paths (`force_disconnect` for moderation actions, the `LEAVE_SERVER_REQ` handler for user-initiated leaves), immediately after the community-side member row is deleted from its DB.

Authentication: both packets use the shared-secret channel community servers already use for `INVITE_REGISTER_REQ` / `INVITE_UNREGISTER_REQ`. Central rejects on bad secret. Same pattern, no new auth surface.

### `LoginResponse` extension

The existing message gets one new field:

```proto
message LoginResponse {
  // ... existing fields ...
  repeated CommunityServerInfo memberships = N;  // N = next free tag
}
```

Server populates via `getUserCommunities(username)` after a successful login. Backwards compatible: old clients ignore the unknown field; new clients use it.

Why on `LoginResponse` rather than a separate `GET_MEMBERSHIPS_REQ`:
- Zero added latency on the critical login path
- Payload is small (~50 B per server × ~10 servers = 500 B, well under any meaningful packet size)
- The auto-rejoin UI populates *as soon as* login completes, not after a follow-up RPC

## Section 3: Client auto-connect flow + error handling

### Native side

In `route_packets` for the central client's `LoginResponse` arm, after the existing success-path work that sets `state.username` / `state.token`:

1. Decode the new `memberships` repeated field into `Vec<CommunityServerInfo>`.
2. Emit a new `memberships_received` event to the renderer with the full list (server_id, name, host_ip, port, etc.). This is purely for the renderer to render placeholder tiles while connections are in flight.
3. For each membership, fire `connect_to_community(server_id, host, port)` internally — no renderer round-trip. Connections happen in parallel via the existing CommunityClient code path. Auth responses land as usual through the existing `community_auth_responded` event.

The renderer doesn't need new connection logic — the existing `community_auth_responded` listener already calls `addConnectedServer` and populates channels/attachment-config. The new `memberships_received` event only drives the placeholder tile UI before each auth response lands.

### New napi command: `request_drop_membership`

```rust
#[napi(object)]
pub struct DropMembershipArgs {
    pub server_id: String,
}

#[napi]
pub async fn request_drop_membership(args: DropMembershipArgs) -> napi::Result<()>;
```

Sends a `MEMBERSHIP_REVOKE_REQ` over the client's JWT-authenticated central connection. Used **only** in the stale-membership cleanup path (§3.3) — never for explicit user disconnects (those don't drop membership; they just close the connection).

### Renderer side

**One new event listener** (extends `useServerEvents`): `memberships_received` → for each entry, add a placeholder tile to the server bar with a "connecting…" indicator. Tiles get replaced as `community_auth_responded` events land for each server_id.

**`disconnect_from_community` is consolidated into Leave Server semantics.**

Today there are two semantically-distinct user actions that look similar in the UI:
- The `×` button on each server tab (ServerBar) and the dropdown "Disconnect" option (ServerChannelsSidebar) both fire `disconnect_from_community`, which closes the TLS connection but leaves the community-side `members` table untouched.
- The "Leave Server" button in MembersAdminPanel fires `LEAVE_SERVER_REQ`, which permanently removes the user from the community.

With auto-rejoin shipping, "close the connection but stay a member" becomes a meaningless action — next login auto-rejoins immediately, so `×` would effectively be a no-op visible only until app restart. Worse, it'd surprise users whose mental model (Discord-shaped) is "× = leave".

**Resolution:** the existing `×` button and the dropdown "Disconnect" option are rewired to invoke the existing `leave_server` napi command (which routes to `LEAVE_SERVER_REQ` on the community side; community then fires `MEMBERSHIP_REVOKE_REQ` to central). The native `disconnect_from_community` command can stay as a low-level primitive for internal use (logout cleanup, app shutdown) but is no longer wired to any visible UI affordance.

Because the action is now destructive, both the `×` button and the dropdown option get a confirmation dialog matching the one MembersAdminPanel's "Leave Server" already uses: `"Leave <server_name>? You will need a new invite to rejoin."`. The MembersAdminPanel's Leave Server button keeps working unchanged — it's redundant with the new × behavior but harmless and provides symmetry from the members-management context.

### Error handling matrix

| Failure mode | Behavior |
|---|---|
| Community server unreachable (TCP timeout) | Server-bar tile renders with the existing connection-lost indicator (`useCentralConnectionStatus`-style). One reconnect attempt per login — no retry storm. User can manually reconnect from the browse view if the server comes back. |
| `community_auth_responded` returns `success=false` (kicked/banned/membership revoked while offline) | Client fires `request_drop_membership(server_id)` to central; tile disappears from server bar; one-time toast: `"You're no longer a member of <server_name>"`. Other auto-rejoin connections continue independently. |
| Server-directory entry deleted (community server was removed from `community_servers` table but the `user_communities` row lingers) | `getUserCommunities`'s JOIN drops orphan rows on the fly, so the user never sees this case. Background cleanup can happen lazily or never; harmless either way. |
| Toast spam | Only the "no longer a member" toast surfaces, and only on auth-rejection responses. Unreachable / network failures show inline tile indicators, no toast. |
| Login response missing `memberships` field (server hasn't been upgraded) | Field treated as empty; client behaves exactly like today (empty server bar). Graceful degradation. |
| Connection-rate spike at login (10+ servers, all auto-connecting in parallel) | Existing CommunityClient code handles independent connections in parallel; no shared bottleneck. If a flood becomes a real problem, we can chunk this later (out of scope). |

## Section 4: Bootstrap behavior

The first time a user (with existing community memberships) signs in to a Decibell client running this feature:

1. Their `user_communities` table on central is **empty** (the feature didn't exist before).
2. `LoginResponse.memberships = []`, so the server bar starts empty (same as today).
3. User manually connects to one of their existing community servers via ServerBrowseView.
4. The community server authenticates them (existing flow), then fires `MEMBERSHIP_REGISTER_REQ` to central → row inserted.
5. On the user's **next login**, their `user_communities` row exists and they auto-rejoin.

So the migration is naturally amortized over each user's normal browsing behavior. No explicit migration script. Users who have many servers will need to manually reconnect to each one **once** after the feature ships — same effort as today, just one last time.

## File-level change list (preview for the implementation plan)

- `proto/messages.proto` — 2 new packet types, 2 new oneof entries, 2 new messages, 1 new field on `LoginResponse`
- `src/server/auth_manager.{hpp,cpp}` — DDL for `user_communities`; `registerMembership` / `revokeMembership` / `getUserCommunities` methods
- `src/server/main.cpp` — populate `memberships` on `LoginResponse`; new handler for `MEMBERSHIP_REGISTER_REQ` (shared-secret) and `MEMBERSHIP_REVOKE_REQ` (both shared-secret community origin and JWT-authed client origin)
- `src/community/main.cpp` — fire `MEMBERSHIP_REGISTER_REQ` at successful auth tail; fire `MEMBERSHIP_REVOKE_REQ` in `force_disconnect` + `LEAVE_SERVER_REQ` handlers
- `electron-client/native/src/net/central.rs` — parse new `memberships` field on `LoginResponse`; emit `memberships_received` event; auto-connect-fanout loop
- `electron-client/native/src/commands/servers.rs` — new `request_drop_membership` napi command
- `electron-client/native/src/events.rs` — new event name + payload struct
- `electron-client/src/features/servers/useServerEvents.ts` — new `memberships_received` listener for placeholder tile rendering
- `electron-client/src/features/servers/ServerBar.tsx` — render "connecting…" state for placeholder tiles; rewire `×` button to invoke `leave_server` with confirmation dialog (replacing `disconnect_from_community`)
- `electron-client/src/features/channels/ServerChannelsSidebar.tsx` — dropdown "Disconnect" → "Leave Server", invoke `leave_server` with confirmation dialog (replacing `disconnect_from_community`)
- `electron-client/src/features/servers/useServerEvents.ts` (same hook, second concern) — on `community_auth_responded` with `success=false` and a placeholder still present, fire `request_drop_membership` + toast

Rough effort: ~400 LOC new, ~150 LOC modified.
