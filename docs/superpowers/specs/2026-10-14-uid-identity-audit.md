# User identity audit: where the backend still uses usernames

2026-10-14. Owner rule (CLAUDE.md): every backend keys users by the account uid, so a future username
change can't break anything. This audit lists every place central and the community server still identify
a user by name, what a rename would break, and the order to fix it in. Nothing here is implemented yet,
except the items marked as done (✅ or ✓).

Two scenarios are used throughout. **R**: alice (uid 7) renames to alice2. **Q**: afterwards, a different
person registers "alice". Under today's code, Q inherits whatever is keyed by the name. That is a security
problem, not just a lost setting.

## Where things stand

- **Central has a uid but doesn't use it.** `users.uid BIGSERIAL` has a unique index
  (`src/server/auth_manager.cpp:23-24`). Its only use is the JWT `uid` claim. `username` is the primary
  key, and **no other table references the uid**. There are no foreign keys at all.
- **The community server is partly migrated.**
  - `members.uid` and `bans.uid` exist. `messages.sender_uid` exists since 18467f5, and the sender filter,
    own-message edit/delete and the ban purge use it.
  - Renames are detected when the renamed account reconnects with a new JWT `sub`. `rename_member` then
    moves roles, member overwrites and created invites (`db.cpp:1191-1239`).
  - Everything else is still by name.
- **The wire carries no uid anywhere.** The proto has 74 username-typed fields and not one uid field, so the
  client knows people only by name: members, DMs, friends, presence, avatars and keys.
- **Encryption binds names.** All of these include the username, so a renamed account looks like a new
  identity:
  - bundle signatures and fingerprints / safety numbers (`native/src/e2ee/identity.rs:193-209`)
  - DM and channel envelopes, through AAD and key derivation (`envelope.rs:127`, `channel_envelope.rs:63-83`)
  - call authentication
  - MLS credentials (`group.rs:138`)
  - the client keystore file name
- **No rename feature exists.** Central has no username update and no account deletion, so names are
  never freed today. The problems below are latent, except two that matter now (next section).

## Problems that exist today, without any rename

Both fixed on 2026-10-15, ahead of the refactor (owner decision 3). Each item ends with what was done.

1. **Central accepts DMs, calls and read-state writes for names that don't exist.** ✅
   - DIRECT_MSG has no existence check (`src/server/main.cpp:404-482`). `check_dm_allowed` answers
     "allowed" for an unknown name, and `insertDm` stores the message.
   - Anyone can therefore pre-seed messages for a name nobody holds yet. Whoever registers it later finds
     them waiting.
   - The same applies to DM_MARK_READ (any string becomes a read-state row) and CALL_SIGNAL.
   - Fix: resolve the recipient to a uid at the request edge and answer "User not found."
   - **Done:** DIRECT_MSG resolves the recipient with `AuthManager::resolveUid` before the DM gate. An
     unknown name is refused with "This user doesn't exist." and the sender's nonce, so the client
     withdraws the bubble and shows that text in a toast. `markDmRead` and `registerMembership` check that
     the account exists before writing. CALL_SIGNAL stores nothing, and an INVITE to an unknown name was
     already answered PEER_OFFLINE, because no session holds that name.
2. **Central can mint a token without a uid.** ✅
   - `authenticateUser` reads the uid in a separate query and signs the JWT even when `getUserId` returns 0
     on a DB error (`auth_manager.cpp:283-318, 334-345`). A uid-0 token sends communities down the legacy
     name-only path.
   - Fix: read the uid and the hash in one query, and refuse to mint without a uid.
   - **Done:** `getCredentials` reads `uid, password_hash` in one query, and `authenticateUser` refuses to
     sign when the uid isn't positive. The timing equalization for unknown names is unchanged.

## Central (Postgres + sessions)

| Where | Keyed by | R | Q | Fix |
|---|---|---|---|---|
| `users` (13-63) | `username` PK; `uid` only UNIQUE | row-local columns move | — | `uid` PK, `UNIQUE(username)` (ideally case-insensitive), a reservation table for old names |
| `friends` (72-97) | `user1`, `user2`, `action_user` names; `CHECK user1 < user2` | every friendship and block is orphaned; **blocks stop applying** | new alice **inherits friendships and blocks** | `user1_uid`, `user2_uid`, `action_uid` + FK, CHECK on uids |
| `dm_messages` (110-151) | `sender`, `recipient` names; pair index on names | her DM list empties, threads split | new alice **reads all old DMs and can edit/delete old alice's sent messages** (`WHERE sender=$2`) | `sender_uid`, `recipient_uid` + FK, indexes on uids; keep the name columns as the sent-under names that encryption binds |
| `dm_read_state` (152-159) | `reader`, `peer` names (PK) | unread counts wrong | inherits read cursors | uid PK |
| `user_e2ee_keys` (166-176), `user_e2ee_backup` (179-186) | `username` | bundles are signed over "alice", so served as alice2 they fail verification | **new alice receives old alice's passphrase-wrapped private keys** (an offline guessing target) and her new key becomes "current" for old threads | key both by uid; store each bundle's `signed_name` (or move to a uid binding, see decisions) |
| `user_communities` (190-201) | `username` (PK) | auto-rejoin list empties | **new alice gets old alice's server list** (hosts, ports, cert pins, including invite-only servers) | `(uid, server_id)` PK |
| Session (`main.cpp:1413`) | `username_` from LOGIN_REQ; no `uid_`; every lookup scans by name | a live session keeps writing `WHERE username='alice'` | a stale session can overwrite new alice's avatar, status and DM-privacy, and `send_private("alice")` can deliver her DMs and calls to it | `Session::uid_`, sessions indexed by uid, the name for display only |
| JWT (`auth_manager.cpp:305-312`) | `sub` = name, `uid` claim; central never reads either back | old tokens assert the old name for 24 h | see community: a stale token can rename a member back and evict the new holder | reserve old names for at least the token lifetime; communities use `sub` for display only |
| MEMBERSHIP_REGISTER / REVOKE (`main.cpp:1206-1236`) | username only (the community already has the uid) | a kick may revoke the wrong name | a stale revoke deletes new alice's membership | add `uid` to both packets |
| Server-picture fan-out (`main.cpp:1244-1269`) | usernames from `user_communities` | missed | delivered to the wrong person | by uid |
| Self-checks (self-DM, self-call, self-friend) | name compare | — | — | compare uids |

Other per-user writes keyed `WHERE username=$1`: `setAvatar`, `getPresenceProfile`, `setPresenceStatus`,
`setStatusMessage`, `setDmFriendsOnly`, all the DM queries, the E2EE publish/fetch functions, and the
membership functions (`auth_manager.cpp:591-1542`).

## Community server (SQLite + sessions)

| Where | Keyed by | R / Q | Fix |
|---|---|---|---|
| **`server_meta.owner`** (`db.cpp:949`, owner check `db.cpp:1757`) | **name** | R: alice2 loses ownership. Q: **any account that later registers "alice" and joins becomes owner with every permission.** | `owner_uid` meta key; `DECIBELL_OWNER_UID` seed |
| `members` (PK `username`) | name; uid column | Q, or a newcomer joining under a stale name: `add_member` / `rename_member` **delete a different live uid's row** with its roles and overwrites (`db.cpp:1131-1167, 1202-1213`) | `uid` PK; drop the "evict by name" logic, since two uids can't collide |
| `member_roles`, `channel_overwrites` (member `target_id` = name) | name | moved by rename; lost to eviction | `(uid, role_id)`; overwrite target = uid |
| `bans` (PK `username`) | name OR uid | Q: an innocent new holder of a banned name is blocked. Re-banning by name upserts `uid=excluded.uid`, which **silently unbans the renamed uid** (`db.cpp:1437-1442`). | uid PK; name-only bans only for never-seen non-members |
| `invites.created_by`, `invites.for_user` | name | `for_user` isn't moved: Q can redeem a personal invite meant for someone else | `created_by_uid`, `for_uid` |
| `attachments.uploader` | name | pending uploads cross over to a new holder | `uploader_uid` |
| `audit_log.actor` / `target` | names | history attaches to a new holder | `actor_uid`, `target_uid` |
| `channel_key_blobs` (recipient, sender), `channel_key_epochs.created_by` | names | Q is served blobs sealed to uid 7, and the server thinks Q already holds the keys, so no refill happens | `recipient_uid` (keep the bound name); delete old-name blobs on rename |
| `messages.sender` | sent-under name + `sender_uid` ✓ | fine. Rows with `sender_uid = 0` (legacy) are still owned by name, and so is the name-based backfill on first auth. | keep `sender` as the immutable bound name; stop claiming uid-0 rows by name once migrated |
| Sessions (`sessions_by_user_`, per-packet `is_member(username_)` at `main.cpp:1202`) | name | CLIENT_PING returns before the membership check, so a stale-name session stays alive and can act with a new holder's permissions | sessions by uid; the per-packet check by uid; pings after it |
| In-memory maps: voice passes, slowmode, active streams / watchers / thumbnails, keyframe relay | name | lost or inherited | by uid |
| Attachment HTTP (`attachment_http.cpp:79-93`) | JWT `sub` only (uid ignored); membership and uploader by name | an old "alice" token acts as the new "alice" | read `uid`; membership and uploader by uid |
| Wire handlers that target a member (kick, ban, unban, timeout, voice mod, nickname, roles, overwrites, transfer ownership, personal invites, key publish, watch/thumbnail/keyframe) | name from the client | a stale roster entry can hit whoever holds the name now | requests carry the uid; resolve legacy names once, at the edge |
| Rename handling (`main.cpp:1045-1053`) | — | the return value is ignored; no MEMBER_REMOVE goes out for the old name (ghost roster rows); no central revoke for it; old sessions keep the old name | emit the removal / upsert pair; trust the newest `iat` |

## Client and crypto (outside the backend, but in the way of a rename)

- **The client keys everything on usernames:** roster, display names, DM threads, presence, avatars cache
  (`username, version`), E2EE peer pins, and the keystore file name (`<hex(username)>.json`). After a rename
  every cache splits until the client learns the uid.
- **Encryption binds the name.** A rename changes the person's fingerprint and safety numbers, so peers see
  "keys changed" although no key changed. Old envelopes stay readable only if the server keeps returning
  the name each was sealed under: never rewrite `messages.sender`, `dm_messages.sender`, or blob sender and
  recipient names.

## Plan

A multi-commit refactor, so on a branch (CLAUDE.md). Pre-1.0, client and server change together, with no
compatibility shims (memory: pre-1.0 has no back-compat constraint).

1. **Central storage and sessions.**
   - uid PK; `*_uid` columns, backfilled with FKs, on friends, DMs, read state, E2EE keys and backup, and
     memberships.
   - `Session::uid_` and uid-indexed sessions.
   - Resolve typed names at the request edge (fixes today's issue 1).
   - Refuse uid-less tokens (fixes today's issue 2).
2. **Community storage and sessions.**
   - `owner_uid` and uid PKs on members, roles, overwrites, bans and invites; `uploader_uid`; audit uids;
     key-escrow recipient uid.
   - Sessions and maps by uid; the HTTP side by uid.
   - Remove name eviction and name-based claiming.
   - Membership sync carries the uid.
3. **Wire.**
   - Every user reference carries the uid: MemberInfo, ChannelMessage `sender_uid`, FriendInfo,
     DirectMessage, presence, CallSignal, key bundles, ban entries, overwrites and audit entries.
   - Requests target by uid; LoginResponse returns the user's own uid.
4. **Client.** Stores keyed by uid; names for display. Avatar cache, DM threads, presence, pins and the
   keystore by uid.
5. **Encryption.** Either keep names bound (store the signed name per bundle and the sent-under name per
   message, and accept "keys changed" warnings on rename), or a v2 binding to the uid, with v1 kept for
   history (see the first decision).
6. **The rename feature.**
   - Password-confirmed; the old name is reserved.
   - USER_RENAMED {uid, name} goes to friends, DM peers and communities.
   - Live sessions are updated and tokens refreshed.

## Owner decisions (2026-10-14)

1. **Encryption moves to a uid binding (v2).** Bundles, fingerprints / safety numbers, DM and channel
   envelopes, call auth and MLS credentials will bind the uid, so a rename is invisible to encryption. The
   one-time "keys changed" warning the switch causes is accepted ("we only have like 10 users"). Old
   messages stay v1 and keep their sent-under names.
2. **Old names can be taken by someone else after 14 days.** A renamed account's old name is reserved for
   14 days (longer than the 24 h token lifetime), then freed.
3. **The two present-day central issues are fixed first,** on their own, before the refactor.
