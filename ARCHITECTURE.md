# Decibell Architecture

The system reference: what runs where, how the pieces talk, and what a client has to do to be a
Decibell client. Written to be read by someone building a **new** client (e.g. a native GPUI one) as
much as by someone changing the existing ones.

> **Status: current as of 0.9.4 (2026-11-07).** Rewritten from the 2026-07 version, whose sections
> on the retired Qt/QML client and the Tauri rebuild plan are gone (read them with
> `git show 871a232:ARCHITECTURE.md`). Companions:
> - `docs/FEATURES.md`: every user-visible feature, where it lives, and its design record.
> - `CLAUDE.md`: working rules and the owner's standing decisions.
> - `electron-client/HANDOFF.md`: the shipping client's internals, gotchas and open items.
> - `proto/messages.proto`: the wire contract; its comments are authoritative for field semantics.
> - `docs/superpowers/specs/`: one design record per feature (index: `docs/README.md`).

---

## 1. System overview

```
                         ┌────────────────────────────┐
                         │ Central  (src/server/)      │  accounts, sign-in, devices,
                         │ C++20 · Boost.Asio · TLS    │  DMs, friends, presence, avatars,
                         │ PostgreSQL (libpqxx 7)      │  community directory + invites,
                         │ :8080                       │  E2EE key directory, call signalling
                         └──────▲──────────────▲───────┘
                     TLS/TCP    │              │  TLS/TCP (heartbeats, memberships,
              (session token)   │              │  invites; shared community secret)
┌───────────────────────────┐   │   ┌──────────┴─────────────────┐
│ Client (electron-client/) │───┘   │ Community (src/community/)  │  one process per hosted
│ Electron + React renderer │──────▶│ C++20 · single io thread    │  community: channels,
│ Rust napi-rs native       │ TLS   │ SQLite (WAL, FTS5)          │  messages, roles, voice
│                           │ :8082 │ :8082 chat  :8085 HTTPS     │  relay, streams, search,
│                           │◀─────▶│ :8083 voice UDP :8084 media │  attachments
└──────────┬────────────────┘  UDP  └─────────────────────────────┘
           │ UDP (sealed, STUN + hole punch) — DM calls only
           ▼
        another client
```

| Tier | Path | Build |
|---|---|---|
| Central server | `src/server/` | Production: root `CMakeLists.txt` target `decibell_server`, built on the owner's Hetzner box. Tests: `src/server/tests/` (own deps, bundled PostgreSQL 16). |
| Community server | `src/community/` | Production: root `CMakeLists.txt` target `decibell_community` (pinned SQLite 3.53.4 via `cmake/DecibellSQLite.cmake`). Tests: `src/community/tests/`. |
| Shared C++ | `src/common/` | `net_utils.hpp` (framing), `udp_packet.hpp` (UDP layouts), `ed25519_keys.hpp`. `src/common/proto/messages.pb.*` are stale; protoc regenerates them at build time. |
| Wire contract | `proto/messages.proto` | protoc (C++), prost via `electron-client/native/build.rs` (Rust). |
| Client | `electron-client/` | Electron (castlabs fork, v33) + React 18 / zustand / Tailwind v4 renderer + napi-rs native addon (`native/`, tokio, prost). See §7. |
| Retired | `src/client/` (Qt/QML), `tauri-client/`, `.github/workflows/release.yml`, `CMakeLists-{linux,windows}.txt` | Nothing builds against them. Don't build on them. |

Deployment today: central runs on the owner's server; the owner hosts the only community server.
Pre-1.0, so client and servers change together, but releases ship the client first and rebuild the
servers days later: a new client must never act destructively on a field an older server doesn't send
(proto3 reads a missing field as 0).

### 1.1 Central server (`src/server/`)

- **Port 8080**, TLS 1.2+ (`server.crt` / `server.key` in the working directory), hard-coded.
- **Threads** (`docs/superpowers/specs/2026-10-15-central-scaling-design.md`): `DECIBELL_IO_THREADS`
  io threads (default = cores) with a strand per connection; handlers that touch PostgreSQL run on a
  database pool (`DECIBELL_DB_THREADS`, default 64, capped to fit Postgres's `max_connections`; commits
  share disk flushes, which is what bounds the DM rate); bcrypt on a CPU pool (`DECIBELL_CPU_THREADS`,
  default cores / 2). A session stops reading while one of its handlers is away, so its packets stay in
  order. `PgPool` hands out a connection per operation.
- **Directory**: `SessionManager` (`session_manager.hpp`) owns everything shared (connections, signed-in
  accounts with their devices and presence, the presence-watch index, the call registry) behind one
  mutex. `deliver_to_user` is the only way to reach an account (every device of it).
- **Database**: PostgreSQL; schema version in `central_schema` (currently **6**; migrations in
  `auth_manager.cpp`). Main tables: `users`, `devices`, `revoked_devices`, `name_changes`, `friendships`,
  `dm_messages`, `dm_conversations` (kept by triggers), `dm_read_cursors`, `community_servers`,
  `community_memberships`, `community_invites`, `e2ee_keys`, `e2ee_backups`. A schema change bumps
  `central_schema` and gets a migration check in the e2e harness.
- **Abuse limits** (`abuse_limits.hpp`, `docs/superpowers/specs/2026-10-15-central-hardening-design.md`):
  per address: 256 open connections, 60 new connections then 10/s, 30 password sign-ins then 1 per 2 s,
  10 sign-ups then 1 per 2 min; failed sign-ins per account **and** address (burst 10, then 1/min), with
  an account-wide cap of 100; a server-wide connection cap from the open-file limit. Per connection,
  DMs / reads / writes / sign-in attempts / status and presence changes / presence subscriptions / call
  signals / key requests are **paced** (an over-rate request waits; nothing is dropped); renames and token
  renewals are refused with an answer. Limits key on the TCP peer address, so anything put in front of
  central must pass the real address (PROXY protocol) first.
- **Response caps** (`wire_limits.hpp`): every response is budgeted to fit the client's 2 MiB frame cap
  (DM conversations are paged, strings are capped).

| Central env | Meaning |
|---|---|
| `DECIBELL_DB_CONN` | **Required.** libpq connection string. |
| `DECIBELL_COMMUNITY_SECRET` | **Required.** Shared with community servers; authenticates heartbeats / membership / invite sync. Not a signing key. |
| `DECIBELL_JWT_KEY_FILE` | Ed25519 signing key (default `jwt_ed25519.pem`, generated on first boot; `.pub` written beside it for communities). |
| `DECIBELL_STUN_SERVERS` | Comma-separated `host:port` list handed to clients for DM calls (unset: clients use a built-in list). |
| `DECIBELL_IO_THREADS` / `DECIBELL_DB_THREADS` / `DECIBELL_CPU_THREADS` | Thread pools (above). |
| `DECIBELL_MAX_CONNECTIONS` | Server-wide connection cap (clamped to the open-file limit). |
| `DECIBELL_JWT_SECRET` | Retired (HS256 era); ignored with a warning. |

### 1.2 Community server (`src/community/`)

- **Ports**, all derived from 8082 (hard-coded): **8082** TLS chat, **8083** voice UDP (port + 1),
  **8084** media UDP (port + 2), **8085** attachment HTTPS (port + 3). TLS cert / key: `server.crt` /
  `server.key` in the working directory; the cert's sha256 fingerprint is reported to central so clients
  can pin it.
- **Threads**: one `io_context` thread runs everything on the hot path (sessions, UDP relay, attachment
  HTTP); a **search worker** (own read-only SQLite connection, `search.cpp`) and a **central-sync worker**
  (`central_sync.hpp`) keep blocking work off it.
- **Database**: SQLite in WAL mode, `schema_version` in `server_meta` (currently **17**; migrations in
  `db.cpp`). Tables: `server_meta`, `members`, `roles`, `member_roles`, `bans`, `channels`,
  `channel_overwrites`, `messages` (+ an FTS5 contentless-delete index kept by triggers), `attachments`,
  `invites`, `audit_log`, `channel_key_epochs`, `channel_key_blobs`, `channel_key_rotations`,
  `channel_key_refusals`. Users are keyed by **uid** everywhere (`members.uid`, `messages.sender_uid`);
  a name from before uids reached the server (a member who hasn't connected since, a name-only ban)
  holds a negative placeholder uid in `legacy_claims` until a token with that name claims it (legacy
  claims end 2026-10-29).
- **First boot** seeds the owner from `DECIBELL_OWNER_USERNAME` and the name / description from
  `DECIBELL_SERVER_NAME` / `DECIBELL_SERVER_DESC`; after that they're edited in-app.
- **Central sync**: a heartbeat every 60 s (`SERVER_HEARTBEAT`: name, description, public host, port,
  member count, cert fingerprint, public listing) answered by `SERVER_HEARTBEAT_RES {server_id,
  revoked}` (central's id for this community, and device sessions central signed out, which the
  community closes and refuses for 25 h). Also `MEMBERSHIP_REGISTER/REVOKE_REQ`,
  `INVITE_REGISTER/UNREGISTER_REQ`, `SYNC_SERVER_PICTURE_REQ`, all authenticated with the community
  secret in `Packet.auth_token`. Central's TLS cert is pinned (`DECIBELL_CENTRAL_CERT_FINGERPRINT` or
  trust-on-first-use stored in `server_meta`).
- **Sessions**: TLS + `COMMUNITY_AUTH_REQ` within `DECIBELL_AUTH_TIMEOUT_SECONDS` (10); an
  authenticated session with no frames for `DECIBELL_IDLE_TIMEOUT_SECONDS` (90) is closed (clients ping
  every 30 s); at most 8 sessions per account; a session whose token expires without a
  `SESSION_TOKEN_UPDATE` ends.
- **Rate buckets** per session (`main.cpp`, `rate_limit.hpp`; review B13): messages + edits 10 burst /
  3 per s, voice / stream signalling 10 / 2, history and list queries 20 / 4, search 6 / 1, management
  and moderation 20 / 2, MLS relay + channel key escrow 20 / 5, stream thumbnails 3 / 0.2. Over a bucket
  the request **waits** and the session stops reading, so order holds and nothing is dropped. Two
  exceptions: a search is refused with `retry_after_ms`; thumbnails keep only the newest in a one-packet
  slot. A refused `CHANNEL_MSG` (slowmode, permission, attachment cap, persist failure) is answered with
  `CHANNEL_MSG_REJECTED {channel_id, nonce, reason}`. The client mirrors the message bucket
  (`sendPacing.ts`: 10 / 2.7 per s), so people never hit it.
- **Retention sweep** every `DECIBELL_RETENTION_INTERVAL_SECONDS` (600): per-channel retention for text
  and per attachment kind; purged attachments keep a tombstone row (`purged_at`).

| Community env | Meaning |
|---|---|
| `DECIBELL_JWT_PUBLIC_KEY_FILE` | **Required.** Central's `jwt_ed25519.pem.pub`. Tokens are verified, never minted. |
| `DECIBELL_COMMUNITY_SECRET` | **Required.** Same value as central's. |
| `DECIBELL_OWNER_USERNAME` | **Required on first boot.** Seeds the owner. |
| `DECIBELL_CENTRAL_HOST` / `DECIBELL_CENTRAL_PORT` | Central's address (127.0.0.1 / 8080). |
| `DECIBELL_CENTRAL_CERT_FINGERPRINT` | Pins central's TLS cert (else trust-on-first-use). |
| `DECIBELL_PUBLIC_IP` | The host clients should dial (reported in heartbeats). |
| `DECIBELL_SERVER_NAME` / `DECIBELL_SERVER_DESC` | Seed a fresh database only. |
| `DECIBELL_DB_PATH` / `DECIBELL_ATTACHMENTS_ROOT` | Storage locations. |
| `DECIBELL_MAX_ATTACHMENT_BYTES` | Per-upload cap (100 MiB). |
| `DECIBELL_MIN_FREE_BYTES` | Free-space headroom kept before accepting uploads (also set in-app, Storage tab). No per-user quota by decision. |
| `DECIBELL_MAX_CONNECTIONS` | Chat connection cap (attachments get half), clamped to the open-file limit. |
| `DECIBELL_AUTH_TIMEOUT_SECONDS` / `DECIBELL_IDLE_TIMEOUT_SECONDS` / `DECIBELL_RETENTION_INTERVAL_SECONDS` / `DECIBELL_HEARTBEAT_INTERVAL_SECONDS` | Deadlines and periods (10 / 90 / 600 / 60). |
| `DECIBELL_JWT_SECRET` | Retired; ignored with a warning. |

---

## 2. Identity, tokens and trust

**The uid is the identity; the username is a label.** Every backend keys stored rows, filters, ownership
checks and purges on the account uid (int64), so a rename never breaks a feature. Every wire user
reference carries `*_uid` next to the name; requests to a community target by uid only; central accepts a
typed *name* only on `DIRECT_MSG`, friend `ADD`, call `INVITE` and E2EE key fetch. Payload names are the
current names. Clients never show uids (fallback: the payload's name, else "Unknown user"). Design:
`docs/superpowers/specs/2026-10-15-uid-identity-design.md`.

### 2.1 Tokens

| Token | Format | Lifetime | Used for |
|---|---|---|---|
| **Session token** | Ed25519 (EdDSA) JWT, `iss = decibell_central_auth`, `sub` = username, `uid`, `sid` = the device row, no audience | 24 h, renewed every 6 h (`TOKEN_REFRESH_REQ`) | `Packet.auth_token` on every central request; `CommunityAuthRequest.jwt_token`; the attachment HTTPS `Authorization` header; its last 31 characters are the session's **UDP key** |
| **Resume token** | Same key, `aud = decibell_central_resume`, `uid`, `did` = device id, `iat` = the password sign-in | 30 days from the password sign-in, never extended | `LoginRequest.resume_token` after a dropped connection (no password, no bcrypt storm after a restart). Central-only: central refuses any token with an audience on a packet, communities refuse any token with an audience. |

Central signs with its private key; communities hold only the public key. `users.sessions_valid_after`
cancels an account's older resume tokens (a password change sets it); signing a device out deletes its
`devices` row, which every resume token of it names.

### 2.2 Signing in to central

1. TLS to central:8080 (pinned, §2.4), then `LOGIN_REQ` (`LoginRequest`): `username` + `password`
   (typed: `automatic = false`; saved password: `automatic = true`, allowed only on a device the account
   still has) **or** `resume_token`. Always `device_id` (random per install), `device_name` (the OS name,
   never the hostname), `device_platform`, `app_version`. A password sign-in without a device id is
   refused.
2. `LOGIN_RES` (`LoginResponse`): session token, resume token, `uid`, `username`, `memberships` (up to
   200 communities: id, name, host, port, picture version, cert fingerprint), `stun_servers`, own status /
   status message / clear-in, `dm_friends_only`, and refusal flags (`device_limit` above 8 devices,
   `signed_out` for a device that was signed out). The capability booleans (`call_signaling`, `e2ee_keys`,
   `presence_statuses`, `e2ee_uid_binding`, `renames`, `devices`) are always true on current centrals.
3. The client then connects to every membership's community (§2.3), sends its presence subscription
   (`PRESENCE_SUBSCRIBE`, §4.8), bootstraps E2EE (§5) and pings (`CLIENT_PING`) every 30 s.

Several devices are online at once (up to 8 per account). The same `device_id` connecting again replaces
its own older connection (`SESSION_CLOSED {REPLACED}`; a live client receiving it is a copied install and
renews its id). `SESSION_CLOSED {SIGNED_OUT | PASSWORD_CHANGED}` means sign out fully and forget the saved
sign-in. Design: `2026-10-18-multi-device-design.md`, `2026-10-19-devices-and-password-design.md`.

### 2.3 Signing in to a community

TLS to the community's host:8082 (pinned), then `COMMUNITY_AUTH_REQ` (`jwt_token`, optional
`invite_code` to join, `presence_hidden` when invisible). `COMMUNITY_AUTH_RES` carries the channel list
**built for this recipient** (only channels they can view, each with `my_permissions` already resolved),
server name / description, `owner_uid`, `max_attachment_bytes`, `attachment_port`, central's `server_id`
for this community (re-key an invite-joined connection onto it), `public_listing`, or an `error_code`
(`not_member`, `invalid_invite`, `banned`, `auth`). The role list, voice presence for every visible
voice channel and (for BAN_MEMBERS holders) the ban list follow as pushes; the client pages the roster
itself (§4.3). Token renewals reach each community as `SESSION_TOKEN_UPDATE` (no reply; a
rename updates the live session in place; `members.name_iat` stops an older token renaming a member
back). `PRESENCE_VISIBILITY` toggles "show me offline" mid-session.

### 2.4 TLS trust

Servers use self-signed certificates, so clients pin by sha256 fingerprint: central is trust-on-first-use;
each community is pinned to the `cert_fingerprint` central reports for it (directory rows, memberships,
invite resolution), falling back to trust-on-first-use. A mismatch is surfaced to the user with a
deliberate re-trust (the client's `CERT_MISMATCH:` contract), never silently accepted. The attachment
HTTPS host is pinned to the same fingerprint.

---

## 3. TCP wire protocol

- **Framing**: `[u32 big-endian length][protobuf Packet]` (`src/common/net_utils.hpp`,
  `native/src/net/framing.rs`). Frames are capped at **2 MiB**; servers budget every response to fit, and
  the client reads and skips (rather than disconnects on) a slightly larger frame.
- **Envelope**: `Packet { Type type = 1; int64 timestamp = 2; string auth_token = 16; oneof payload }`.
  Central checks `auth_token` (the session token) on every post-login packet; community sessions are
  authenticated once at `COMMUNITY_AUTH_REQ` and the client sends no per-packet token there.
  Community↔central packets carry the community secret in `auth_token`.
- **Replies**: most requests have a typed `*_RES`. Shared result packets carry an `action` string:
  `MOD_ACTION_RES` (kick, ban, unban, leave, nickname, timeout, voice_mod, transfer, message),
  `CHANNEL_ACTION_RES` (create, rename, delete, reorder, overwrite, sync, category_sync),
  `ROLE_ACTION_RES`. Where several requests can be in flight, the request carries an id the reply echoes
  (`request_id`, `seq`, `nonce`).
- **Optimistic sends**: `ChannelMessage.nonce` / `DirectMessage.nonce` are client-minted; the server
  echoes them on the broadcast (never persisted), and a channel history page carries the requester's own
  nonces from the last 30 minutes so a send whose echo a reconnect swallowed still settles.
- **Packet types**: 178, numbered in `Packet.Type`. By area:

| Area | Types (number) | Tier |
|---|---|---|
| Accounts, sign-in | `REGISTER_*` (2–3), `LOGIN_*` (4–5), `CLIENT_PING` (31), `RENAME_REQ/RES` (158–159), `USER_RENAMED` (160), `TOKEN_REFRESH_REQ/RES` (161–162), `SESSION_CLOSED` (171), `DEVICE_LIST_*` (172–173), `DEVICE_SIGN_OUT_*` (174–175), `DEVICES_CHANGED` (176), `PASSWORD_CHANGE_*` (177–178) | central |
| Presence, profile | `PRESENCE_UPDATE` (7), `PRESENCE_DELTA` (149), `SET_STATUS_REQ` (150), `SET_IDLE_REQ` (151), `SET_STATUS_MESSAGE_REQ` (153), `PRESENCE_SUBSCRIBE` (164), `OWN_PRESENCE` (169), `UPDATE/FETCH_AVATAR_*` (61–64), `AVATAR_CHANGED` (65), `DM_PRIVACY` (28) | central |
| Friends | `FRIEND_ACTION_*` (15–16), `FRIEND_LIST_*` (17–18), `FRIENDS_IN_COMMUNITY_*` (166–167) | central |
| DMs | `DIRECT_MSG` (6), `DM_CONVERSATIONS_*` (68–69), `DM_HISTORY_*` (70–71), `DM_MARK_READ_REQ` (72), `DM_DELETE_*` (76–77), `DM_MESSAGE_DELETED` (78), `DM_EDIT_*` (121–122), `DM_MESSAGE_EDITED` (123), `DM_READ_SYNC` (168) | central |
| Calls | `CALL_SIGNAL` (128) | central relay |
| E2EE keys | `E2EE_PUBLISH_KEYS_*` (129–130), `E2EE_FETCH_KEYS_*` (131–132), `E2EE_FETCH_BACKUP_*` (133–134), `E2EE_KEYS_CHANGED` (135) | central |
| Directory, memberships, invites | `SERVER_LIST_*` (8–9), `INVITE_RESOLVE_*` (48–49), `MEMBERSHIP_REVOKE_REQ` (74, also client→central for stale cleanup), `MEMBERSHIP_ADDED` (170), `FETCH_SERVER_PICTURE_*` (85–86), `SERVER_PICTURE_CHANGED` (87) | central |
| Community↔central | `SERVER_HEARTBEAT` (30), `SERVER_HEARTBEAT_RES` (75), `INVITE_REGISTER/UNREGISTER_REQ` (46–47), `MEMBERSHIP_REGISTER/REVOKE_REQ` (73–74), `SYNC_SERVER_PICTURE_REQ` (84) | both servers |
| Community session | `COMMUNITY_AUTH_*` (10–11), `SERVER_META_UPDATE` (44), `MEMBERSHIP_REVOKED` (45), `LEAVE_SERVER_REQ` (43), `SESSION_TOKEN_UPDATE` (163), `PRESENCE_VISIBILITY` (152) | community |
| Messages | `CHANNEL_MSG` (12), `CHANNEL_MSG_REJECTED` (127), `CHANNEL_HISTORY_*` (50–51), `MESSAGE_EDIT_*` (124–125), `CHANNEL_MESSAGE_EDITED` (126), `MESSAGE_DELETE_*` (79–80), `CHANNEL_MESSAGE_DELETED` (81), `CHANNEL_PRUNED` (54), `CHANNEL_WIPE_*` (55–56), `CHANNEL_WIPED` (57), `MESSAGE_SEARCH_*` (156–157) | community |
| Channels | `CHANNEL_UPDATE_*` (52–53), `CHANNEL_CREATE/DELETE/RENAME_REQ` (96–98), `CHANNEL_ACTION_RES` (99), `CHANNEL_LIST_UPDATE` (100), `CHANNEL_REORDER_REQ` (102), `CHANNEL_OVERWRITE_SET_REQ` (103), `CHANNEL_OVERWRITES_*` (104–105), `CHANNEL_PERMISSIONS_SYNC_REQ` (147), `CATEGORY_SYNC_SET_REQ` (148) | community |
| Members, roles, moderation | `MEMBER_LIST_*` (38–39), `MEMBER_UPSERT` (106), `MEMBER_REMOVE` (107), `MEMBER_SEARCH_*` (154–155), `ROLE_LIST_*` (88–89), `ROLE_CREATE/UPDATE/DELETE_REQ` (90–92), `ROLE_ACTION_RES` (93), `MEMBER_ROLES_UPDATE_REQ` (94), `SET_NICKNAME_REQ` (101), `KICK/BAN_MEMBER_REQ` (40–41), `UNBAN_MEMBER_REQ` (95), `MOD_ACTION_RES` (42), `BAN_LIST_*` (108–109), `TIMEOUT_MEMBER_REQ` (114), `VOICE_MOD_REQ` (115), `VOICE_FORCE_NOTIFY` (116), `TRANSFER_OWNERSHIP_REQ` (117), `AUDIT_LOG_*` (112–113) | community |
| Server settings | `SERVER_UPDATE_*` (110–111), `UPDATE_SERVER_PICTURE_*` (82–83), `STORAGE_INFO_*` (118–119), `STORAGE_CONFIG_SET_REQ` (120), `INVITE_CREATE/LIST/REVOKE_*` (32–37) | community |
| Voice | `JOIN_VOICE_REQ` (19), `LEAVE_VOICE_REQ` (20), `VOICE_PRESENCE_UPDATE` (21), `VOICE_STATE_NOTIFY` (27) | community |
| Streams | `STREAM_PRESENCE_UPDATE` (22), `START/STOP_STREAM_REQ` (23–24), `WATCH_STREAM_REQ` (25), `STOP_WATCHING_REQ` (26), `STREAM_THUMBNAIL_UPDATE` (29), `FETCH_STREAM_THUMBNAIL_*` (66–67), `STREAM_CODEC_CHANGED_NOTIFY` (58), `UPDATE_CAPABILITIES_REQ` (59), `STREAM_WATCHER_NOTIFY` (60) | community |
| Voice encryption (MLS) | `MLS_GROUP_INFO_*` (136–137), `MLS_GROUP_CREATE_REQ` (138), `MLS_COMMIT_*` (139–140), `MLS_COMMIT_BROADCAST` (141) | community |
| Channel encryption | `CHANNEL_KEYS_*` (142–143), `CHANNEL_KEYS_PUBLISH_*` (144–145), `CHANNEL_KEYS_CHANGED` (146), `CHANNEL_KEYS_REJECT` (165) | community |
| Retired / unused | `HANDSHAKE` (1, never enforced); 13–14 reserved; 58–60 exist but no client sends or acts on them | — |

---

## 4. Feature protocols

What a client must do per feature. Field-level detail is in the proto comments; rationale in the
named design records.

### 4.1 Channels and messages (community)

- **Channel list**: one flat ordered list of `ChannelInfo` (id, name, type TEXT / VOICE / CATEGORY; a
  channel's category is the nearest CATEGORY row above it, categories don't nest), `voice_bitrate_kbps`, retention per kind, `my_permissions`, `slowmode_seconds`, `encrypted`,
  `permissions_synced`, `category_sync`, `topic`). Pushed whole as `CHANNEL_LIST_UPDATE` after any
  change, again built per recipient. Gate composer / attach / join / history UI on `my_permissions`; the
  server stays authoritative.
- **History**: `CHANNEL_HISTORY_REQ {channel_id, before_id | around_id | after_id, limit}`; the response
  echoes which page it is (`before_id`, `around_id`, `after_id`) with `has_more` / `has_more_after`.
  `before_id = 0` means "newest page" — and since an older server omitted the echo, a client must not
  treat a missing echo as "newest" destructively. After a reconnect the client re-syncs from history and
  holds live messages until it reaches the present.
- **Send**: `CHANNEL_MSG {channel_id, content, nonce, reply_to, attachments (completed upload ids), envelope}` →
  broadcast to the channel's viewers with `id`, `sender_uid`, `timestamp` and the echoed `nonce`, or
  `CHANNEL_MSG_REJECTED {nonce, reason, retry_after_seconds}` to the sender. Message ids are 64-bit and
  increase over time (no other promise).
- **Edit / delete**: `MESSAGE_EDIT_REQ` (own only) → `CHANNEL_MESSAGE_EDITED`; `MESSAGE_DELETE_REQ`
  (own, or MANAGE_MESSAGES) → `CHANNEL_MESSAGE_DELETED`. Replies carry `reply_to` plus a denormalised
  preview (`reply_to_sender_uid`, `reply_to_content`, attachment kinds, envelope).
- **Slowmode**: per channel; the sender's own echo starts the window, a refusal's `retry_after_seconds`
  corrects it; owner and MANAGE_MESSAGES bypass.
- **Retention / wipe**: `CHANNEL_PRUNED` (rows removed by the sweep or a ban purge), `CHANNEL_WIPED`
  (drop all local state for the channel).

### 4.2 Attachments (community HTTPS, port 8085)

Bytes never travel over the TCP protocol. HTTP/1.1 over TLS with keep-alive; every request carries
`Authorization: Bearer <session token>`.

| Method + path | Purpose |
|---|---|
| `POST /attachments/init` | JSON: channel, kind, filename, mime, size, `width` / `height` / `duration_ms` / `placeholder` (computed by the uploader), `encrypted`. Returns the attachment id. Refused over the per-file cap or the free-space headroom. |
| `PATCH /attachments/<id>` | Append a chunk at `Upload-Offset` (tus.io semantics, **not** `Content-Range`); resumable. |
| `HEAD /attachments/<id>` | Upload status (current offset). |
| `POST /attachments/<id>/complete` | Finish; the id can now be bound to a `CHANNEL_MSG`. |
| `POST /attachments/<id>/thumbnail?size=N` | Uploader-made thumbnail (320 / 640 / 1280 px → `thumbnail_sizes_mask` bits 0–2). |
| `GET /attachments/<id>` | Download (Range supported; `?variant=thumb&size=N` for a thumbnail). VIEW_CHANNEL + READ_HISTORY. |
| `DELETE /attachments/<id>` | Abandon an upload. |

`Attachment` rows on messages: id, kind (IMAGE / VIDEO / DOCUMENT / AUDIO), filename, mime, size, url,
position, `purged_at` (retention removed the blob), `width` / `height`, thumbnail size + mask,
`duration_ms`, `placeholder` (base64 ThumbHash, decoded only by the renderer), `encrypted`. In encrypted
channels the server stores stand-ins; the real metadata and the per-file key ride the message envelope
(§5.3). Abandoned uploads are swept by idle time (`last_activity_at`, schema v10).

### 4.3 Roster, roles and moderation (community)

- **Roster**: `MEMBER_LIST_REQ {after, limit}` pages members by username (online members page too);
  `{uids}` looks specific members up. Live changes arrive as `MEMBER_UPSERT` / `MEMBER_REMOVE`; every
  roster packet carries online / total counts. Online vs offline follows the community's own view
  (connected and not `presence_hidden`). `MEMBER_SEARCH_REQ` (name and / or role, paged) serves server
  settings and pickers; its results never feed the roster. Spec `2026-10-18-roster-paging-design.md`.
- **Roles**: `ROLE_LIST_RES` is pushed after auth and to everyone on any role change (it is the delta).
  Role CRUD, `MEMBER_ROLES_UPDATE_REQ` (replace a member's set), with hierarchy checks; `RoleInfo` has
  colour, position, permission bits, `is_default` (@everyone), `manage_each_other`, `member_count`.
- **Permissions**: `PERM_*` bits in the proto; resolution = server-wide role bits, then the channel's
  overwrites (role, then member); owner and ADMINISTRATOR get everything (`src/community/authz.hpp`).
  Permissions v3 decisions in CLAUDE.md (voice moves check only the mover; the peer flag covers
  nicknames + voice moderation only; category sync is opt-in and only an explicit sync changes a
  channel). Specs `2026-08-22-permissions-v2-design.md`, `2026-10-09-permissions-v3-design.md`.
- **Moderation**: kick, ban (`reason`, `expires_at`, `delete_message_seconds`), unban, timeout,
  `VOICE_MOD_REQ` (server mute / deafen, move, disconnect; the target gets `VOICE_FORCE_NOTIFY`), slowmode,
  ownership transfer; all audited (`AUDIT_LOG_REQ` with `query` / `actions` filters, searched on the
  server). A kicked / banned member gets `MEMBERSHIP_REVOKED`. Server-mute stops talking but not stream
  audio (decision).

### 4.4 Directory, invites, memberships

- Public directory: `SERVER_LIST_REQ` (central) lists communities with `public_listing` on.
- Invites are created on the community (`INVITE_CREATE_REQ {expires_at, max_uses, for_uid,
  request_id}`; `for_uid` = a personal, single-use invite only that user can redeem) and registered with
  central. Links are code-only, `decibell://invite/<CODE>`; `INVITE_RESOLVE_REQ` (central) turns a code
  into host:port + cert fingerprint + a preview (name, description, member count, picture version). The
  client joins by `COMMUNITY_AUTH_REQ` with `invite_code`.
- Communities register memberships with central (`MEMBERSHIP_REGISTER_REQ`), which is how sign-in
  returns them and how `MEMBERSHIP_ADDED` reaches the account's other devices. `FRIENDS_IN_COMMUNITY_REQ`
  asks central which friends are already members (the friend list is never sent to a community).

### 4.5 Message search (community)

`MESSAGE_SEARCH_REQ {seq, query, channel_ids, author_uids, before, after, has (bitmask), sort, cursor /
offset, limit, match_within_words}` → `MESSAGE_SEARCH_RES` (results, approximate total "N+",
skipped encrypted channels). One search per session; a new one supersedes; over the bucket it is refused
with `retry_after_ms`. Runs on the search worker over the FTS5 index (plaintext channels only;
encrypted channels get filters only). Spec `2026-10-14-message-search-design.md`.

### 4.6 Direct messages (central)

- `DIRECT_MSG {recipient_uid (or a typed name), content, nonce, reply_to, envelope}` → persisted, echoed
  to every device of both people with `id`, uids and the `nonce`. A refusal comes back as a `DIRECT_MSG`
  with no id, the refusal text as content, and the request's nonce.
- `DM_CONVERSATIONS_REQ` (paged list with last-message preview + `unread_count`), `DM_HISTORY_REQ`,
  `DM_MARK_READ_REQ` (→ `DM_READ_SYNC` to the reader's other devices), edit / delete (own only).
- The DM gate (`check_dm_allowed`) applies blocks and friends-only DMs (saved on the account).
- Encrypted DMs put the body in `envelope` and a fixed placeholder in `content` (§5.1).

### 4.7 Friends (central)

`FRIEND_ACTION_REQ {action: ADD (by name) | REMOVE (also unblocks) | BLOCK | ACCEPT | REJECT, target}` →
`FRIEND_ACTION_RES`; `FRIEND_LIST_REQ/RES` with statuses (online status only for visible users). Friend
list changes reach all the account's devices.

### 4.8 Presence, statuses, profiles (central)

- **By subscription**: the client sends `PRESENCE_SUBSCRIBE {uids}` with every user it is showing (and
  re-sends the set after each sign-in); central answers newly watched users with a `PRESENCE_UPDATE`
  (`covered_uids` + `avatars` for those not visible) and then pushes `PRESENCE_DELTA` and profile pushes
  (`AVATAR_CHANGED`, `USER_RENAMED`, `E2EE_KEYS_CHANGED`) to watchers only. There is no login snapshot.
  A client never watches itself: its own state comes from `LoginResponse` and `OWN_PRESENCE` (any of its
  devices' change, echoed).
- **Status**: `SET_STATUS_REQ` (online / away / dnd / invisible, saved), `SET_IDLE_REQ` (per-device idle
  flag; the account is Away only while every device idles), `SET_STATUS_MESSAGE_REQ` (emoji + 128 chars +
  clear-in). Invisible users are *absent* on the wire, exactly like offline ones; profile pushes are the
  one exception (announced even while invisible, by decision).
- **Avatars**: `UPDATE_AVATAR_REQ` (bytes to central), `FETCH_AVATAR_REQ` by uid; each presence entry
  carries `avatar_version`, and a subscription answer includes the pictures of covered users who aren't
  visible. **Server pictures**: uploaded to the community, synced to central, fetched from central.
- Communities learn only "hide me" (`presence_hidden` / `PRESENCE_VISIBILITY`). Specs
  `2026-10-11-user-status-design.md`, `2026-10-12-status-messages-design.md`.

### 4.9 Voice channels (community)

1. `JOIN_VOICE_REQ {channel_id, capabilities}` (CONNECT_VOICE; a moved member gets a runtime voice pass).
   The server broadcasts `VOICE_PRESENCE_UPDATE {channel_id, active_uids, user_states, capabilities}`
   for every voice channel the recipient can see (sidebar rosters), and `STREAM_PRESENCE_UPDATE`.
2. The client binds its two UDP sockets (§6.2), then joins the channel's MLS group (§5.4); no keys →
   no voice.
3. `VOICE_STATE_NOTIFY {is_muted, is_deafened}` on self mute / deafen (the relay skips deafened
   listeners). `LEAVE_VOICE_REQ` to leave; switching sends LEAVE then JOIN.
4. A moderator move / disconnect arrives as `VOICE_FORCE_NOTIFY {MOVED | DISCONNECTED}`; the client
   rebinds its MLS group to the new channel.

### 4.10 Streams (community)

- `START_STREAM_REQ {channel_id, target_fps, target_bitrate_kbps, has_audio, resolution, chosen_codec,
  enforced_codec}` (STREAM permission) → everyone gets `STREAM_PRESENCE_UPDATE` (`VideoStreamInfo`:
  stream id `<uid>_screen`, owner uid, audio flag, resolution, fps, codec, watcher count).
- `WATCH_STREAM_REQ {target_uid}` / `STOP_WATCHING_REQ`; the relay forwards the streamer's media only to
  watchers. Watchers ask for keyframes / retransmits over UDP (§6).
- Thumbnails: the streamer uploads `STREAM_THUMBNAIL_UPDATE` (960 px JPEG ≤ 256 KB at start, +2 s, then
  every 15 s; plaintext by decision); late joiners fetch `FETCH_STREAM_THUMBNAIL_REQ`.
- `VideoCodec` values: 0 unknown (retired VP9 slot), 1 H.264 (hardware), 2 H.264 (software), 3 H.265 /
  HEVC, 4 AV1. Capabilities ride `JOIN_VOICE_REQ`; mid-stream renegotiation (58–60) is not wired.

### 4.11 DM calls (central signalling, P2P media)

`CALL_SIGNAL {kind, call_id, to_uid, pub_key, pub_key_sig, key_id, candidates, stream}`, relayed by
central (never persisted, `from` / `from_uid` stamped by central, INVITE gated by the DM rules and by
invisibility → `PEER_OFFLINE`). Kinds: INVITE, RINGING, ACCEPT, REJECT, BUSY, CANCEL, HANGUP,
PEER_OFFLINE, NOT_ALLOWED, STREAM_START, STREAM_STOP, ANSWERED_ELSEWHERE, DECLINED_ELSEWHERE. An INVITE
rings every device of the callee; the first ACCEPT takes the call; later signals go only between the two
connections in it. Media: STUN (server list from `LoginResponse.stun_servers`) + simultaneous hole punch,
IPv4, no relay; the sealed UDP transport of §6.4. Ephemeral X25519 keys are signed by the E2EE identity
and verified before use (fail closed when the peer has keys). Specs `2026-08-28-p2p-dm-calls-design.md`,
`2026-09-03-authenticated-p2p-calls-design.md`.

### 4.12 Several devices (central)

What one device does reaches the account's others: `OWN_PRESENCE`, DM echoes / edits / deletes,
`DM_READ_SYNC`, friend list changes, profile pushes, `MEMBERSHIP_ADDED`, `DEVICES_CHANGED`,
`E2EE_KEYS_CHANGED` (another device made a new identity: lock), calls (§4.11).

---

## 5. End-to-end encryption

All client crypto lives in the native crate (`electron-client/native/src/e2ee/`); servers store and
relay opaque bytes and never hold a private key.

### 5.1 DMs (`2026-09-03-e2ee-dms-design.md`)

- **Identity**: static X25519 (agreement) + Ed25519 (`sign_pub`, identity) per account, published to
  central as a bundle bound to the uid (`binding` 2). No forward secrecy, deliberately: history is
  server-fetched and there's no local message store. Every old private key is kept (locally and in the
  backup) so old history opens; envelopes name `key_id`s.
- **Envelope**: per-message HKDF + AES-256-GCM. Version byte `0x03` for new DMs (binds uids); `0x01`
  (v1) still opens with the `sealed_*` names. Content tags inside: `0x02` raw bytes (key-escrow blobs),
  `0x03` message body with attachments (`EncryptedMessageBody`).
- **Backup**: the private keys wrapped under an Argon2id passphrase key (64 MiB), stored on central
  (`E2EE_PUBLISH_KEYS_REQ` / `E2EE_FETCH_BACKUP_REQ`). Another device gets the identity only from the
  backup (it reports *locked* until the passphrase is entered; setup never publishes over a backup).
- **Trust**: trust-on-first-use pins keyed by uid, safety numbers / fingerprints over `sign_pub`; a
  pinned peer is never downgraded to plaintext; a changed `sign_pub` is a "keys changed" notice + re-pin.
- **Send policy**: ready and peer has keys → sealed; peer never had keys and was never pinned →
  plaintext (shown as such); peer pinned but keys missing / we're locked → refuse.
- **Local store**: `e2ee/u<uid>.json`, encrypted at rest under a key from the OS keychain.

### 5.2 Calls

The ephemeral call key is signed with the identity (call auth v2); see §4.11 and §6.4.

### 5.3 Encrypted text channels (`2026-09-04-encrypted-text-channels-design.md`)

Chosen at channel creation, fixed for life, default off. Per-channel **epoch keys**, escrowed to each
member as an identity-sealed blob (a DM-style envelope, content tag `0x02`) on the community server; the
server sequences epochs and names a **filler** (a member holding the key) when someone lacks it
(`CHANNEL_KEYS_CHANGED`); a member drops blobs it can't open (`CHANNEL_KEYS_REJECT`) so a holder seals
them again. Messages are channel envelopes `0x04` (v2; binds uids). The server refuses plaintext in an
encrypted channel and envelopes elsewhere. Attachments are sealed client-side: random key per file,
64 KiB AES-256-GCM chunks, sealed thumbnails; the real metadata + key ride the message envelope (tag
`0x03`); the server learns only kind, ciphertext size and thumbnail sizes. Names and topics stay in the
clear (decision). Search sees filters only.

### 5.4 Voice channels and streams (`2026-09-03-mls-voice-channel-encryption-design.md`)

One MLS group (OpenMLS, ciphersuite of `e2ee/group.rs`) per voice-channel session; the community server
is the delivery service: it keeps the current GroupInfo for external joins (`MLS_GROUP_INFO_REQ`),
sequences commits (`MLS_COMMIT_REQ` must be epoch + 1; `MLS_GROUP_CREATE_REQ` for epoch 0) and forwards
them (`MLS_COMMIT_BROADCAST`). Credentials are `"<uid>"`, checked against central's identity keys (a
mismatch quarantines the epoch). After every merge each member derives per-sender media keys; UDP
payloads are sealed per sender per epoch (`media/frame_crypto.rs`) and sent as the sealed packet types
(7–9); video is sealed per frame before chunking, so FEC / NACK work unchanged. Strict: no keys → no
voice; plaintext media in a channel session is ignored. Stream thumbnails stay plaintext (decision).

---

## 6. UDP media plane

### 6.1 Sockets and identity

Each voice session uses **two UDP sockets** to the community: **voice** (port + 1 = 8083: `AUDIO`,
`STREAM_AUDIO` and their sealed twins, `PING`, `BIND_ECHO`) and **media** (port + 2 = 8084: `VIDEO`,
`VIDEO_SEALED`, `FEC`, `NACK`, `KEYFRAME_REQUEST`, `PING`, `BIND_ECHO`). Upstream, `sender_id` is the
session's **UDP key** (the last 31 characters of its session token); the relay identifies the session by
it and, before forwarding, rewrites `sender_id` to the sender's **uid in decimal** (NUL-padded).
Keyframe requests and NACKs name the streamer in `target_id` (decimal uid) and have their `sender_id`
zeroed when relayed. Streams are named `<uid>_screen`. Native media state keys on the uid string.

### 6.2 Endpoint binding (return routability, review S11)

The key names the sender; it never decides where the relay sends. Each session has one bound endpoint
per socket, set only by a challenge round trip (`src/community/udp_path.hpp`): a keyed datagram from an
address that isn't the bound one gets a `BIND_CHALLENGE` (41 B: type 10, 32 zero bytes, an 8-byte random
cookie; at most one per session per socket per 250 ms, only in reply to a datagram ≥ 41 B; a cookie
lives 10 s and is single-use), and only a `BIND_ECHO` (type 11, the UDP key in bytes 1–32, the cookie)
from that address binds it. Until then the relay sends that session nothing on that socket. PINGs are
echoed only to the bound endpoint. The client answers challenges inside its socket wrapper and pings at
once on joining, then every 500 ms until the first echo; a NAT rebinding re-binds on the next datagram.

### 6.3 Packet layouts

Packed (`#pragma pack(1)`), little-endian host order as written by both sides (`src/common/udp_packet.hpp`
and `native/src/media/{packet,video_packet}.rs` are the source of truth and agree).
`SENDER_ID_SIZE` 32, `UDP_MAX_PAYLOAD` 1200 (MTU-safe incl. PPPoE / VPN), `NACK_MAX_ENTRIES` 64,
`FEC_GROUP_SIZE` 5. Clients send header + actual payload (not the full struct); the relay forwards the
received length and drops audio / FEC / NACK datagrams larger than their structs.

| Type | Value | Layout (offset: field) |
|---|---|---|
| AUDIO / STREAM_AUDIO | 0 / 6 | 0 type · 1 sender_id[32] · 33 sequence u16 · 35 payload_size u16 · 37 payload (flags byte + Opus) |
| VIDEO | 1 | 0 type · 1 sender_id[32] · 33 frame_id u32 · 37 packet_index u16 · 39 total_packets u16 · 41 payload_size u16 · 43 is_keyframe u8 · 44 codec u8 · 45 payload |
| KEYFRAME_REQUEST | 2 | 0 type · 1 sender_id[32] · 33 target_id[32] |
| NACK | 3 | 0 type · 1 sender_id[32] · 33 target_id[32] · 65 frame_id u32 · 69 nack_count u16 · 71 missing_indices u16[64] |
| FEC | 4 | 0 type · 1 sender_id[32] · 33 frame_id u32 · 37 group_start u16 · 39 group_count u16 · 41 payload_size_xor u16 · 43 XOR of the group's payloads |
| PING | 5 | RTT probe / keepalive; echoed by the relay to the bound endpoint |
| AUDIO_SEALED / STREAM_AUDIO_SEALED / VIDEO_SEALED | 7 / 8 / 9 | Same headers as 0 / 6 / 1; the payload is sealed under the sender's MLS media key for the epoch (video: the whole frame is sealed, then chunked) |
| BIND_CHALLENGE / BIND_ECHO | 10 / 11 | §6.2 |

**Audio payload flags** (first payload byte, client↔client; the relay doesn't read it): `0x01` muted,
`0x02` deafened, `0x04` silence (a gate-closed tail frame or a keepalive: receivers treat the following
gap as a pause, no concealment, and keep keepalives out of the jitter estimate). While the voice gate is
closed the client sends one keepalive per 500 ms instead of 50 packets/s, preceded by 3 frames of encoded
silence so the listener's decoder renders the fade. Voice is 48 kHz Opus, 20 ms frames, bitrate per
channel clamped to 320 kbps on both ends.

**Video**: an encoded frame is split into ≤ 1200-byte chunks with one XOR FEC packet per 5 data packets;
the sender keeps recent frames for NACK retransmission; a receiver that can't rebuild a frame asks for a
keyframe (throttled). Keyframes of codecs that need out-of-band configuration carry the decoder
description in-band after a `WIRE_DESCRIPTION_MAGIC` prefix (`video_packet.rs`); H.264 travels as Annex B
on every path.

### 6.4 P2P DM calls: the sealed transport

DM calls carry the **same datagrams** peer to peer with no server in the media path. Both sockets are
wrapped in `MediaSocket::Sealed` (the community path uses `MediaSocket::Plain`, a byte-identical
passthrough):

| Outer field | Size | Notes |
|---|---|---|
| type | 1 | `0xE5` (top bits set; never collides with STUN) |
| counter | 8 | u64 LE per direction; nonce = 4-byte salt ‖ counter (BE); replay window |
| ciphertext | n | AES-256-GCM of the unchanged inner datagram |
| tag | 16 | AAD = the 9-byte outer header |

Keys: ephemeral X25519 per call (signed, §4.11), HKDF-SHA256 with `salt = call_id` → voice / media ×
a→b / b→a. `sender_id` is the own uid (no relay to rewrite it). A sealed socket never `connect()`s: the
peer address is learned from the last authenticated datagram; peer PINGs are reflected re-sealed.

---

## 7. The client

The shipping client is `electron-client/`. Its internals, gotchas and open items are in
`electron-client/HANDOFF.md`; this section is the overview plus what a different client would reuse.

### 7.1 Layers

| Layer | Path | Owns |
|---|---|---|
| Renderer | `src/` (React 18, zustand, Tailwind v4) | All UI and view state; video encode (WebCodecs fallback) and **all video decode** (WebCodecs); ThumbHash + thumbnails for uploads; attachment encryption before upload; voice-view / stream UI; message list. Stores keyed by uid (`stores/usersStore.ts` is the only name source). |
| Electron main | `electron/main/` | Window, deep links, auto-update, Sentry, the attachment protocol (`decibell-attachment://`, decrypting encrypted attachments) and a loopback media server for Range playback, downloads + pacing, link-preview unfurling, GIF search, clipboard, global-hotkey glue, idle, the E2EE local key from `safeStorage`, IPC routing to the addon. |
| Native addon | `native/` (Rust, napi-rs 2, tokio, prost) | Every server connection (TLS, framing, pins, reconnect, resume), the protocol state machines, voice capture / processing / playback, UDP send / receive / relay protocol, native screen capture + hardware encode, stream-audio capture, STUN / punch / sealed transport, all E2EE (identities, envelopes, backup, MLS, channel keys), global hotkeys, system idle on Wayland, settings (`config.json`). |

### 7.2 Media pipelines in the client

- **Voice** (`media/pipeline.rs`, own threads): cpal capture (device's native format) → resample to 48 kHz → AEC3 echo cancellation,
  RNNoise noise suppression, AGC2 → voice gate (activity threshold or push-to-talk, `voice_gate.rs`) →
  Opus → UDP. Receive: per-peer adaptive jitter buffer (`jitter.rs`, concealment only for real loss) →
  Opus decode → per-peer gain → soft-clip mixer → cpal output paced by the device clock. "Default" devices
  follow the OS default (`default_device_watch.rs`).
- **Screen capture + encode** (sender). Sources are listed by Electron's `desktopCapturer` (Chromium
  ids `screen:N:0` / `window:HWND:0`, parsed natively by `source_id.rs`).
  - **Windows** (native by default; `localStorage["decibell.win_native_encode"] = "0"` opts out): WGC
    (Win11, borderless) or DXGI Desktop Duplication with GPU cursor compositing (Win10 monitors) →
    D3D11 video processor (BGRA → NV12, sized from the input) → FFmpeg NVENC or AMF (vendor order by
    adapter; QSV is left out of the probe), dynamically linked LGPL FFmpeg from vcpkg. No CPU↔GPU sync in
    the capture loop.
  - **Linux** (native when a hardware encoder probes; `decibell.linux_native_encode = "0"` opts out):
    xdg-desktop-portal ScreenCast + PipeWire SHM → static LGPL FFmpeg NVENC (BGRA in, converted on the
    GPU). VAAPI's probe is unreliable, so AMD / Intel normally take the fallback.
  - **Fallback, and always on macOS**: `getDisplayMedia` → `MediaStreamTrackProcessor` → WebCodecs
    `VideoEncoder` (the castlabs Electron build adds H.264 / HEVC encoders) → chunks to native for
    packetising.
  - Codecs offered = encode caps (native probe or WebCodecs probe) ∩ local decode caps.
- **Stream audio**: Linux PipeWire tap of the selected apps (never Decibell's own nodes); Windows one
  WASAPI process-loopback client per allowed app, summed (`ProcessLoopbackMixer`); none on macOS. Sent as
  STREAM_AUDIO on the voice socket.
- **Watching**: native receive thread reassembles frames (FEC, NACK, keyframe requests) and decrypts
  them, then hands whole encoded frames to the renderer, which decodes with WebCodecs `VideoDecoder` into
  a canvas (self-preview too).
- **Thumbnails**: native paths encode JPEGs on a worker (`thumb_encode.rs`); the WebCodecs path sends a
  renderer-made JPEG (`send_stream_thumbnail`).
- **Renderer-side audio**: the mic test outside a call and UI sounds (`src/utils/sounds.ts`) use Web
  Audio; auto-away is a renderer timer over Electron `powerMonitor` (native `system_idle.rs` covers only
  Wayland).

### 7.3 Reusing the native crate in another client

The native crate (`electron-client/native/`) is where the protocol knowledge lives: connections, every
packet router, voice, UDP media, native capture / encode, and all E2EE. A native (e.g. GPUI) client
should reuse it rather than re-implement it. What stands in the way today (mapped 2026-11-07):

- **It is not a library.** `crate-type = ["cdylib"]` only and every module in `lib.rs` is a private
  `mod`. Step one is a workspace split: a plain `decibell-core` lib crate (net, media, e2ee, hotkeys,
  config, presence, system_idle, state) and a thin napi crate on top.
- **napi lives only in `lib.rs`, `events.rs` and `commands/`.** `net/`, `media/`, `e2ee/`, `hotkeys/`,
  `config.rs`, `presence.rs`, `system_idle.rs` and `state.rs` have no napi types. Exceptions to move out
  of `commands/` first: `net/central.rs` calls `commands::…::connect_with_invite` (returns
  `napi::Result`), `signed_out_elsewhere` and `apply_hidden`; `net/community.rs` calls
  `rebind_voice_group`.
- **Events have one seam, but no trait.** Everything reaches the UI through `events.rs`: `send(name,
  payload)` (a `{name, payload}` JSON envelope; ~77 named events, payload structs `Serialize` in
  camelCase), `send_stream_frame` (encoded video frames, a queue capped at 8 with a discontinuity flag)
  and `emit_stream_thumbnail_updated`. They sit on three `OnceLock` ThreadsafeFunctions installed by
  `init`. Replacing them with a `dyn Fn(&'static str, serde_json::Value)` plus two binary callbacks is
  the whole UI coupling. (`STREAM_FRAME_SINKS` and input-level reporting state live there too.) Tests
  can't reach an emit today because the TSFN symbols only link inside Node, hence the `cfg(not(test))`
  wrappers (`hotkeys/mod.rs`, `e2ee/session.rs`, `video_pipeline.rs`, `encoder_thread_linux.rs`).
- **Commands**: 132 `#[napi]` functions in `commands/*.rs` (auth 7, call 6, channels 15, community 23,
  devices 3, dm 5, e2ee 6, friends 3, hotkeys 6, messaging 1, presence 5, servers 11, settings 4,
  stream_audio 2, streaming 18, voice 17), each taking one args struct; plus `init` / `shutdown` / `ping`
  in `lib.rs`. They are thin: lock `AppState`, build a packet or call into `media` / `e2ee`. A native
  client calls the same functions directly.
- **Runtime**: the crate never builds a tokio runtime; it relies on napi's `tokio_rt`. A native client
  must own a multi-thread runtime and call in from inside it (e.g. `media/mod.rs` `tokio::spawn`s from
  stream start). State is one global `AppState` (`state.rs`, `OnceLock<Arc<tokio::Mutex<…>>>`) plus a few
  globals (`WATCHED_STREAMS`, the voice gate, the hotkeys hub).
- **`InitOptions`**: `user_data_dir`, `app_version`, `e2ee_local_key` (32 bytes, base64; from the OS
  keychain), `local_key_protected`.
- **Dead code to leave behind**: `capture_wlr_screencopy.rs` (unreachable), `gpu_interop.rs` and the
  DMA-BUF / CUDA / VAAPI encode branches (never fed), `codec_selection.rs` (tests only),
  `emit_stream_gpu_fallback`, `emit_caps_refreshed`, `watcher_event_tx` (no subscriber). Several file
  headers still claim "FFmpeg removed entirely" (`lib.rs`, `media/mod.rs`, `video_pipeline.rs`) — false.

What the Electron side does today that a native client must replace:

| Responsibility | Today | Native-client note |
|---|---|---|
| Video **decode** + render (remote and self-preview) | Renderer, WebCodecs `VideoDecoder` (`StreamVideoPlayer.tsx`) | Needs a decoder: Media Foundation / D3D11VA on Windows (or FFmpeg hwaccel), rendering into a GPU texture. Native already hands over whole, decrypted, Annex B / OBU frames. |
| Encode **fallback** + macOS encode | Renderer, WebCodecs `VideoEncoder` | Windows / Linux native paths already exist; macOS would need VideoToolbox. |
| Capture source list | Electron `desktopCapturer` | Enumerate monitors / windows (DXGI outputs, `EnumWindows`) and produce the same ids. |
| Attachment upload | Renderer (`uploadAttachment.ts`: init / PATCH / complete / thumbnails, pacing); native only returns `{host, port, jwt, maxBytes, certFingerprint}` (`get_attachment_target`) | HTTP client with the pinned cert (§4.2). |
| Attachment download, Range playback, save-as, Transfers | Electron main (`protocol.ts`, `mediaServer.ts`, `netFetch.ts`, `downloads.ts`, `downloadPacer.ts`) | Same HTTP client. |
| Attachment encryption | Renderer encrypts (`features/chat/attachmentCrypto.ts`, WebCrypto); main decrypts (`electron/main/attachmentCrypto.ts`, `attachmentFetch.ts`) | Re-implement the 64 KiB AES-256-GCM chunk format byte-for-byte (or move it into native). |
| Upload thumbnails + ThumbHash, image decode, EXIF orientation | Renderer (canvas, `thumbhash.ts`) | Image crate + a ThumbHash port. |
| Link previews, GIF search | Electron main (`linkPreview.ts` + `publicNet.ts` SSRF guard, `gifs.ts`) | Port; keep the private-network guard. |
| E2EE at-rest key | Electron main `safeStorage` (`e2eeLocalKey.ts`) | Windows DPAPI / Credential Manager, macOS Keychain, libsecret. |
| Auto-away, lock / sleep | Renderer timer + `powerMonitor` | `GetLastInputInfo`, session-lock notifications. |
| UI sounds, mic test outside a call | Web Audio | Play through the native audio stack. |
| Notifications / attention | `flashFrame`, taskbar progress (`window.ts`) | `FlashWindowEx`, `ITaskbarList3`. |
| Auto-update, crash reports | electron-updater, Sentry | Own updater; `sentry` crate. |
| Settings | Mostly native `config.json`; ~18 `localStorage` keys (layout, codec-cap caches, native-encode opt-outs) | Move those into `config.json`. |
| Global hotkeys | Native listeners; main forwards `--hotkey=` and does macOS `globalShortcut` | Bindings use DOM `KeyboardEvent.code` names (`hotkeys/keys.rs`). |
| Message rendering | Renderer: markdown subset, code highlighting, KaTeX, twemoji | Whole UI layer. |

## 8. Client-side-only features

No server involvement, on purpose:

- **Link previews**: unfurled in the Electron main process (OpenGraph / Twitter / `<title>` + oEmbed,
  image dimension probe, private-network guard, caps, cache), behind the Privacy toggle. Invite links
  resolve against our central and ignore the toggle.
- **GIF search**: KLIPY (or GIPHY) from main with a key baked into release builds (`resources/gifs.json`);
  a sent GIF is just its https URL. Tenor's API is gone (2026-06-30); never use it.
- **Optimistic bubbles**: faded until the `nonce` echo; a 30 s watchdog backs them up.
- **Send pacing and slowmode mirroring**, so a person never hits the server's limits.
- **Drafts, scroll positions, layout memory, theme**: local only. Nothing about searches is stored, and
  attachment images are not kept across restarts (decisions).

---

## 9. Repository map

```
proto/messages.proto            wire contract (C++ via protoc, Rust via prost)
src/server/                     central (main.cpp, auth_manager.*, session_manager.hpp, pg_pool.hpp,
                                abuse_limits.hpp, wire_limits.hpp) + tests/ (e2e, load.py)
src/community/                  community (main.cpp, db.*, authz.hpp, attachment_http.*, search.*,
                                central_sync.hpp, udp_path.hpp, rate_limit.hpp, connection_limits.hpp) + tests/
src/common/                     framing, UDP layouts, Ed25519 helpers
cmake/DecibellSQLite.cmake      pinned SQLite amalgamation with FTS5 for the community build
CMakeLists.txt                  production build of both servers
electron-client/                the client (HANDOFF.md, PACKAGING.md)
  src/                          renderer: features/, stores/, components/, layouts/, lib/, hooks/, utils/
  electron/main, electron/preload
  native/                       Rust addon: net/, media/, e2ee/, hotkeys/, commands/, events.rs, state.rs
aur/                            decibell-bin PKGBUILD + .SRCINFO
.github/workflows/              electron-release.yml (ev* tags), win-native-check.yml; release.yml is dead
docs/FEATURES.md                feature catalogue
docs/superpowers/specs|plans/   design records (index: docs/README.md)
docs/reviews/                   reviews + the running feature log (2026-08-23-community-server-review.md)
src/client/, tauri-client/      retired clients
```
