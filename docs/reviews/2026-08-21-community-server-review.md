# Community Server Review — 2026-08-21 (pre "Discord-parity" pass)

Scope: `src/community/{main,db,attachment_http}.cpp`, the wire contract in
`proto/messages.proto`, the community↔central seam in `src/server/`, and the
client's community surface (`electron-client/native/src/net/community.rs`,
`commands/`, `src/features/servers|channels`). Baseline: `main` @ `08cb09e`.

Method: one full read by the primary reviewer + one independent full-file
review agent over the community server, a reader over the central server, a
reader over the client. Findings both reviewers hit independently are marked
**[×2]**. The community server was compiled standalone (g++ 15, fetched
headers, 23 s) and B1 was reproduced against the live binary.

Deliberately excluded (already tracked in `CODE_REVIEW.md`): Theme A (HS256
key shared with central), Theme B (plaintext/unauthenticated UDP media plane),
TLS `verify_none`, "DB + file I/O on the single io thread" as a general refactor.

---

## 1. How the pieces fit (what the review is grounded on)

```
Client ──TLS 8080──▶ Central   auth/JWT, friends, DMs, presence, directory,
  │                            invite-code→host:port, user_communities, server pictures
  │                               ▲ one-shot TLS per event, auth = shared secret
  │                               │ (heartbeat 60 s → server_id, invite reg/unreg,
  │                               │  membership reg/revoke, picture sync)
  ├──TLS 8082──▶ Community      COMMUNITY_AUTH (JWT[, invite]) → channels, roles,
  │                             members, messages/history, mod actions, voice/stream
  │                             signaling. SQLite: members, bans, invites, channels,
  │                             messages(+FTS5), attachments, roles, member_roles.
  ├──UDP 8083──▶ voice relay    AUDIO/STREAM_AUDIO/PING, keyed by last-31 JWT chars
  ├──UDP 8084──▶ media relay    VIDEO/FEC/NACK/KEYFRAME, watcher-set fan-out
  └──HTTPS 8085▶ attachments    tus-style init/PATCH/complete/GET, JWT bearer
```

- One `io_context`, one thread, shared by all five listeners. `SessionManager`
  owns `sessions_`, `voice_channels_`, `active_streams_`, `stream_watchers_`,
  `udp_key_index_`, `latest_thumbnails_`, `last_keyframe_relay_`.
- Permissions (shipped 2026-08-17): server-wide roles with a dense hierarchy,
  `everyone` seeded at position 0, `effective = everyone | OR(roles)`,
  owner = `kAll` / level `INT32_MAX`, ADMINISTRATOR expands to all bits but
  does not bypass hierarchy. Every mutating handler calls `has_permission()`.
- Client: one `CommunityClient` per server (all live concurrently), per-channel
  state keyed `serverId:channelId`, roster/roles/channels are full-snapshot
  pushes the client replaces wholesale. Permission UI is computed client-side
  from `RoleListResponse` + `MemberInfo.role_ids`; server is authoritative.
- Central knows nothing about roles, bans, nicknames or ownership; it sees a
  ban only as a membership revoke.

---

## 2. Bugs (ranked)

| # | Sev | Where | Finding |
|---|-----|-------|---------|
| B1 ✅ | **High** | `db.cpp:114` `open()` → `ensure_default_channels_()` | Runs `INSERT OR IGNORE` for the four seed channels on **every** boot. A seed channel deleted via `CHANNEL_DELETE_REQ` comes back on restart (**reproduced**: delete `announcements`, restart → back at position 1). Fix: seed only from `seed_if_empty_`, or record a `defaults_seeded` meta key. |
| B2 ✅ | **High** | `attachment_http.cpp:927` `send_raw_and_close` | `ssl::stream::shutdown()` is the **synchronous** overload: it sends close_notify and then blocks reading the peer's close_notify. The attachment listener shares the one io thread with chat + both UDP relays. An unauthenticated peer that completes TLS, sends `GET / HTTP/1.1\r\n\r\n`, receives the 401 and then stays silent **freezes voice, video, chat and auth for everyone** until it disconnects. Fix: `async_shutdown` + short timer, or (every response is `Connection: close` with a Content-Length) skip the TLS shutdown and `lowest_layer().shutdown(); close()`. |
| B3 ✅ | **High** | `main.cpp:1621,1690` (role name), `:1822,1870` (channel name), `:1946` (nickname) | `std::string::resize(N)` truncates **mid-UTF-8 codepoint**. C++ protobuf still serialises the invalid string (logs only); prost on the client rejects the whole `Packet` (`connection.rs:68-76` "Failed to decode packet", dropped). A 33-byte nickname ending in an emoji poisons every `MEMBER_LIST_RES`; a 63-byte+`é` channel rename poisons `CHANNEL_LIST_UPDATE` **and `COMMUNITY_AUTH_RES`** (embeds the channel list) — nobody can auth until the DB is hand-edited. Fix: truncate on a codepoint boundary (or reject), also reject control chars/newlines, enforce in the DB layer. |
| B4 ✅ | **High** | `main.cpp:3050` `force_disconnect`, `:3031` `find_session_by_username` | Kick/ban disconnects **one** session per username; nothing stops the same JWT authenticating on N sockets and membership is never re-checked after auth. A banned user's second client keeps posting, joining voice, streaming (attachment endpoints re-check `is_member`; chat does not). Fix: `username → sessions` index, disconnect all; re-validate `is_member`/`is_banned` on mutating handlers (PK lookup). |
| B5 ✅ | **High** (client) | `native/src/net/community.rs:829-851` + `start_reconnect` | After kick / ban / leave / any terminal auth failure the server closes the socket; the read loop ends and `start_reconnect` retries forever, re-auths, gets `not_member`/`banned`, renderer toasts "Couldn't join server" every 30 s. No renderer code ever calls `disconnect_from_community`. Fix: stop on `MEMBERSHIP_REVOKED`, on `ModActionRes{leave}`, and on non-retryable `error_code` (`not_member`, `banned`, `invalid_invite`, `auth`). |
| B6 ✅ | Med | `main.cpp:1785-1790` `MEMBER_ROLES_UPDATE_REQ` | Only hierarchy is checked on the delta; the "can't grant bits you don't hold" guard that `ROLE_CREATE/UPDATE` enforce is absent here. A MANAGE_ROLES holder at position 2 can assign themselves (or anyone) an existing position-1 role carrying ADMINISTRATOR. Fix: require `(role.permissions & ~actor_perms) == 0` for every *added* role. |
| B7 ✅ | Med | `main.cpp:699` `JOIN_VOICE_REQ` | Switching voice channels while streaming never calls `stop_stream(old)` (LEAVE_VOICE does). `active_streams_[old][user]` lingers: old channel still shows the stream live, watchers there get nothing, the ghost counts against `max_streams_per_channel_`, thumbnails can still be pushed into the old channel. |
| B8 ✅ | Med | `main.cpp:1485-1490`, `:1526-1530` kick/ban | Roster refresh only happens as a side effect of the disconnect path. Kicking/banning an **offline** member never broadcasts `MEMBER_LIST_RES` — every client (incl. the actor's) still lists them; looks like the action failed. `UNBAN` does it right (`:1569`). |
| B9 | Med **[×2]** | `main.cpp` Session, `attachment_http.cpp`, `:3337`/`:1014` accept | **No pre-auth, handshake or idle timeout anywhere on the community side** (central sweeps at 60 s). TLS-but-never-authed peers pin a `Session` (and a 2 MiB `inbound_body_` if they send one big frame pre-auth) forever; half-sent HTTP requests hold an `AttachmentConnection` (+ open `FILE*` in PATCH). And both accept loops re-arm unconditionally on error → EMFILE becomes a 100 % CPU spin that starves the relay. Fix: per-session `steady_timer` (10 s auth, idle N min), pre-auth frame cap, backoff on accept error. |
| B10 | Med | `main.cpp:669-712` caps, `broadcast_voice_presence` | `ClientCapabilities` stored unbounded per session and re-serialised into every `VOICE_PRESENCE_UPDATE` (which goes to **all** sessions, not the channel). 1 MB caps × N × every mute toggle. Cap at ~16 entries. |
| B11 | Med **[×2]** | `main.cpp:819` `CHANNEL_MSG`, `JOIN_VOICE`, `VOICE_STATE_NOTIFY`, `/attachments/init` | No per-session rate limiting at all. 64 KB messages at line rate (persist + FTS + fan-out), presence spam (3 all-session broadcasts each). Needed anyway for slowmode — add a token bucket now. |
| B12 | Med **[×2]** | `db.cpp:2147` `prune_attachments` (also `bind_attachments`) | One `?` per doomed row in `UPDATE … WHERE id IN (…)`. Exceeding `SQLITE_MAX_VARIABLE_NUMBER` (999 on SQLite < 3.32) makes the prepare fail silently; the caller still unlinks blobs and broadcasts tombstones while rows keep `purged_at=0` → re-broadcast every sweep, GET 410 forever. Fix: chunk at ≤500 or reuse the JOIN predicate; cap attachments per message. |
| B13 ✅ | Med **[×2]** | `main.cpp:2131` `SessionManager::leave()` | Not idempotent; read-error + write-error + overflow paths each call it → two `broadcast_members()` per disconnect, and `erase_thumbnail_cache(username)` kills a second session's thumbnail. Guard on `sessions_.erase() > 0`. |
| B14 | Med | `main.cpp:2703` `relay_keyframe_request` | `last_keyframe_relay_[target] = now` is recorded **before** the target is looked up; `target` comes straight from the UDP datagram / WATCH req and is never erased → unbounded map growth from random target names. |
| B15 | Med | `db.cpp:503-505` `seed_if_empty_` | Overwrites `server_name`/`description` from env on every boot; `hb_name` read once at startup (`main.cpp:3739`). Any in-app rename (`PERM_MANAGE_SERVER` exists) would be clobbered on restart. Pick DB as source of truth before adding rename. |
| B16 | Med (client) | `InviteModal.tsx:50-63` | Still owner-gated ("Only the server owner can manage invites") while its entry point and the server gate on `MANAGE_INVITES` → dead modal for role holders. |
| B17 | Med (client) | `ServerBrowseView.tsx:208`, `DeepLinkJoinModal.tsx:51` | Invite-joined servers keyed by synthetic `host:port` while everything else uses central's numeric id → no `ServerBar` tile until re-login; same server can exist twice after a later `SERVER_LIST_REQ`, with messages cached under the wrong key. |
| B18 | Low **[×2]** | `main.cpp:927-982` `CHANNEL_MSG` | If `insert_message` fails the message is still broadcast with `id=0` → undeletable ghost on every client, absent from history. Drop + tell the sender. |
| B19 | Low | `attachment_http.cpp:424,534,559` | Throwing `std::filesystem::exists()` overloads inside handlers; an exception unwinds `io_context.run()` and `main()` exits 0. Use the `ec` overloads and wrap `run()` in a retry loop. |
| B20 | Low | `main.cpp:962,1037` | `Attachment.url` ships the absolute server filesystem path (`storage_path`). Client ignores it. Send `/attachments/<id>` or nothing. |
| B21 | **High** ✅ | `main.cpp:882` `routed = packet`, `:892`, `:2902` | **Live JWT leak** (initially misjudged as latent): the Electron client puts its bearer JWT in `Packet.auth_token` on *every* community packet (`build_packet(…, Some(&client.jwt))`), and `CHANNEL_MSG` / `STREAM_THUMBNAIL_UPDATE` / `STREAM_CODEC_CHANGED_NOTIFY` were forwarded as verbatim copies — every member received the sender's JWT with every message and could impersonate them for the token's lifetime. Fixed: `strip_client_envelope()` on all three forward paths. |
| B22 | Low | `db.cpp:724` `remove_ban` | Returns true on no-op → "Member unbanned." + roster broadcast for a non-banned user. |
| B23 ✅ | Low | `main.cpp:3045` `force_disconnect` | Queues `MEMBERSHIP_REVOKED` then `close_connection()` immediately; delivery depends on asio's speculative write. `close_after_flush()` exists for exactly this. |
| B24 | Low | `main.cpp:455`, `community.rs:823` | `SERVER_META_UPDATE` (44) never sent, unhandled by the client — dead protocol. Wire it (needed for rename / ownership transfer) or retire it. |
| B25 | Low | `main.cpp:1341` invites; `:585` auth | `max_uses < 0` = unlimited, past `expires_at` creates a dead invite that is still registered with central; `redeem_invite` burns a use before `add_member` can fail. |
| B26 | Low | `main.cpp:3360` voice UDP; `:808` | AUDIO path doesn't validate `payload_size` vs `bytes_recvd` like VIDEO does; `STOP_WATCHING_REQ` fires `LEFT` notify for never-subscribed watchers. |
| B27 | Low | `main.cpp:2614` `CHANNEL_PRUNED` | One id per pruned row; enabling text retention on a channel with 1M old messages = one multi-MB packet. Send `pruned_before_id` instead. |
| B28 | Low | `main.cpp:822` | `FETCH_STREAM_THUMBNAIL_REQ` works for any member regardless of voice-channel membership. |
| B29 | Low (central) | `src/server/main.cpp:888-905` | Central `SessionManager::leave` never closes the socket — kicked/swept central sessions become ghosts that can still send DMs (same class batch 11 fixed community-side). |
| B30 | Low (central) | `auth_manager.cpp:262`, `:291` | Directory never expires (`last_heartbeat` never read); a community whose public IP changes gets a new `server_id` and orphans every `user_communities` row. |

## 3. Performance / scalability

| # | Sev | Finding |
|---|-----|---------|
| P1 | **High [×2]** | `broadcast_members()` (`main.cpp:2410-2470`) = `list_members` + `list_bans` + `list_all_member_roles` + **one `has_permission()` per online user** + full roster serialised and delivered to **every** session, on every auth, disconnect (twice, B13), nickname, role assign, unban. 2k members / 200 online ≈ 400 SQL statements + ~20 MB egress per connect, on the thread that relays voice. Cheap fix: coalesce with a 250 ms timer + cache `sees_bans`. Right fix: `MEMBER_UPDATE`/`MEMBER_REMOVE`/`PRESENCE` deltas with a `revision`, and a lazy/paged member list. |
| P2 | Med **[×2]** | `has_permission()`/`member_level()` = 2–3 `sqlite3_prepare_v2` each incl. `get_meta_("owner")`; no prepared-statement cache anywhere (`Stmt` re-prepares every call; `create_channel`/`normalize_channel_order_` prepare one UPDATE per row in a loop). Cache owner in memory, cache per-user effective perms + level (invalidate on role/member-role/ban change), cache statements. Mandatory once `SEND_MESSAGES` is checked on every message. |
| P3 | Med **[×2]** | `find_session_by_username`, `relay_keyframe_request`, `relay_nack` are O(sessions) linear scans under `mutex_` — NACKs and watcher notifies are hot-path. A `username → sessions` index fixes this and B4 together. |
| P4 | Med **[×2]** | Every `sync_*` spawns a detached thread + a fresh TLS handshake to central. A restart-driven reconnect storm = N threads, N simultaneous TLS connections, up to 5 s each. The heartbeat thread captures `&manager` and can outlive `run()`. One worker thread + bounded queue (also gives ordering: a revoke can't overtake its register). |
| P5 | Med | UDP relay: 2 + N heap allocations per relayed datagram (`make_shared<vector>` copy, `targets` vector, one `async_send_to` completion per recipient) and one datagram per reactor wakeup. For 10 voice users at 50 pps ≈ 4,500 heap-allocated sends/s. Non-blocking sync `send_to` (drop on EWOULDBLOCK), `thread_local` target buffer, drain the socket in a loop per wakeup. |
| P6 | Low | FTS5 triggers double every message write and make wipe/prune row-by-row tokenise-and-delete, for a search UI that doesn't exist yet. Either ship search or drop the triggers and `rebuild` when you do. |
| P7 | Low **[×2]** | No index on `attachments(channel_id)` (wipe/delete_channel full-scan); legacy rows have `channel_id=''` and are orphaned by wipe/delete. `member_count()` loads every row per heartbeat. `send_initial_voice_presences` re-serialises per joiner under the lock. 8 copies of the framing code (`create_framed_packet` exists). |
| P8 | Low | Attachment HTTP is `Connection: close`; each PATCH chunk pays a TLS handshake. 2 MiB UDP socket buffers are silently capped by `net.core.rmem_max` — log the effective size. |

## 4. Design smells that will fight the roles / management / moderation work

| # | Smell | Consequence |
|---|-------|-------------|
| D1 | Permissions are **server-wide only**: no overwrite concept on `ChannelInfo`, attachment GET checks membership only, `CHANNEL_HISTORY_REQ` has no read gate, `DbAttachment` has no channel context, 15 inline `if (!has_permission) { build one of 5 response shapes }` blocks with free-text errors. | No private channels, no read-only `#announcements`, no mod-only channel — the biggest gap vs Discord. Per-channel overrides today mean editing every handler + the HTTP layer. → `authorize(Action, AuthCtx{user, channel, target})` + one uniform error response first. |
| D2 | Reserved bits `SEND_MESSAGES` / `CONNECT_VOICE` / `STREAM` are in `kKnownMask` (editable) but **unenforced**: `CHANNEL_MSG`, `/attachments/init`, `JOIN_VOICE_REQ`, `START_STREAM_REQ` never check them. | A role editor can clear SEND_MESSAGES on `everyone` and the server keeps accepting. Enforce (one call each) or unexpose. |
| D3 | **No audit log.** Actions go to stdout only; `deleted_by`, `wiped_by`, `banned_by` exist but nothing persists them queryably. Identity is a bare username (no stable uid claim in the JWT) — a username reuse/rename at central silently defeats bans and attribution. | Can't answer "who banned X". Ask central for a `uid` claim before bans/audit rows accumulate. |
| D4 | Owner is a magic string compared in 6 places and special-cased *inside* `effective_permissions`/`member_level`; fixed at seed via env; name/description env-owned (B15); no `SERVER_UPDATE_REQ`, no ownership transfer; `ServerMetaUpdate` dead (B24). | `PERM_MANAGE_SERVER` gates only the picture. Model owner as a synthetic top role + single `is_owner()`. |
| D5 | No **timeout** (temporary mute), server-mute/deafen, voice-kick/move; bans permanent, username-only, reason never reaches the target, no "delete last N messages"; kick/ban reason is always `""` from the client. | Moderation = kick/ban. |
| D6 | Hierarchy rule is inconsistent: `member_level` ignores `everyone` (KICK on `everyone` can never kick anyone — both level 0), ADMINISTRATOR bypasses bits but not hierarchy, assignment bypasses bits (B6). | Decide one rule in the roles layer before timeouts/mutes add more hierarchy-gated actions. |
| D7 | Everything is a full snapshot (`MEMBER_LIST_RES`, `ROLE_LIST_RES`, `CHANNEL_LIST_UPDATE`) with no revision; action discriminators are free strings (`"kick"|"ban"|"unban"|"leave"|"nickname"`, and `MembershipRevoked.action` also sends `"leave"` though the comment doesn't say so). | P1 grows with every feature; clients string-match. Deltas + `revision`, enums. |
| D8 | Channel id = slug of the original name, doubles as the attachment directory; `attachments.channel_id` is TEXT and `''` for legacy rows; per-channel runtime state lives in five maps cleaned in five places (B7, B14 are both "missed one map"). | Future channel-scoped tables FK to a slug; moderation (server-mute, move-to-channel) has no single place to act. Integer channel PK + a `ChannelRuntime` struct. |
| D9 | Messages are immutable rows: no `edited_at`, reply_to, pins, reactions, mentions, typing, per-channel read state. Retention kind is the client-labelled mime (`image/png` on a 100 MB video gets image retention). | Everything above "send text" is missing; retention is gameable. |
| D10 | Central has no owner/ban/role knowledge; ban = plain revoke. Proto comments are stale (`CHANNEL_UPDATE/WIPE` "owner-only", `MANAGE_CHANNELS` "create/delete later"). | Cross-server features need a new seam; the proto is what client authors read. |

---

## Fix status (2026-08-21, same day)

✅ = fixed in the working tree and covered by the Python e2e harness in
`src/community/tests/` (43 checks, incl. a negative build proving the
ghost-session check bites): B1, B2, B3, B4 (+ per-packet membership
re-check, username→sessions index also used by the NACK/keyframe relay =
P3), B5 (native reconnect stops on rejected auth / revoke / leave), B6,
B7, B8, B13, B21, B23 (`force_disconnect` now uses `close_after_flush`,
which gained a 3 s hard deadline and a `finish_close()` that detaches the
session — a closing session no longer re-arms its read loop, so the old
"socket error → leave()" path wouldn't have fired for leave/kick).

## 5. Suggested order of work

1. **Fix batch (small, verifiable with the standalone build + e2e client):**
   server B1, B2, B3, B6, B7, B8, B12, B13, B14, B18–B23, B25–B27, P7;
   client B5, B16, B17.
2. **Hardening the feature work depends on:** B4 + P3 (username index, kick all
   sessions, one session per user), B9 (timeouts + accept backoff), B10, B11
   (token bucket → becomes slowmode), P2 (permission + statement cache), P4
   (central-sync worker), P1 cheap-coalesce.
3. **Permission model v2:** `authorize()` abstraction (D1), enforce D2 bits, one
   hierarchy rule (D6), then per-channel overwrites (allow/deny per role + per
   member, `@everyone`→role→member resolution) incl. `VIEW_CHANNEL`,
   `READ_HISTORY`, `ATTACH_FILES`; make attachment GET / history / voice join /
   stream channel-aware.
4. **Server management:** `SERVER_UPDATE_REQ` (DB as source of truth, heartbeat
   re-reads), wire `SERVER_META_UPDATE`, ownership transfer, `audit_log` table +
   `AUDIT_LOG_REQ`, stable `uid` claim from central.
5. **Moderation:** timeouts (`timed_out_until` enforced at `CHANNEL_MSG`/voice),
   ban expiry + reason + purge-messages, server mute/deafen + voice
   move/disconnect, slowmode, reason UI.
6. **Roster scalability (P1 proper) + relay efficiency (P5)** before servers grow.
