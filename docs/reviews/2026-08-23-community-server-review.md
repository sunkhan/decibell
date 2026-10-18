# Community Server Review — 2026-08-23 (Discord-parity pass #2)

Scope: `src/community/{main,db,attachment_http}.cpp` + `{authz,rate_limit,central_sync}.hpp`,
the community↔central seam in `src/server/`, and the client's community surface
(`electron-client/native/src/net/{community,central}.rs`, `commands/`,
`src/features/servers|channels|voice`). Baseline: `main` @ `0833da5` (0.7.5).

Method: primary reviewer + six agents — central-server map, client-seam map, and four
full-file review passes (main.cpp handlers, main.cpp infra, db layer, attachment HTTP).
Every finding ranked **High** or the top of **Med** was re-read against source by the
primary reviewer before landing here (line refs verified). Cross-confirmed findings
(two independent agents) are marked **[×2]**.

Deliberately excluded (already tracked): the plaintext/unauthenticated UDP media plane
(`project_stream_watch_audit`), and the prior review's cosmetic leftovers B22 (`remove_ban`
true on no-op), B24 (`SERVER_META_UPDATE` dead — note: the client *does* now handle it, so
it's live in one direction), B28 (thumbnail fetch not voice-gated).

---

## 0. How it fits together (verified current, deltas from 2026-08-21)

```
Client ──TLS 8080──▶ Central     auth (Ed25519 JWT: sub, uid, 24h exp), friends/blocks,
  │                              DMs (plaintext at rest), presence, directory, invite→host:port,
  │                              user_communities, server pictures + cert_fingerprint
  │                                ▲ community→central only; one-shot TLS per exchange,
  │                                │ auth = ONE global shared secret (not per-community)
  │                                │ heartbeat 60s (carries server_id + cert fp), invite reg/unreg,
  │                                │ membership reg/revoke, picture sync — all via 1 worker + queue
  ├──TLS 8082──▶ Community        COMMUNITY_AUTH (JWT[,invite]) → channels, roles, overwrites,
  │                              members (paged + deltas), messages/history, mod actions, voice/stream.
  │                              SQLite (WAL): members, bans, invites, channels, messages(+FTS5),
  │                              attachments, roles, member_roles, channel_overwrites, audit_log.
  ├──UDP 8083──▶ voice relay      AUDIO/STREAM_AUDIO/PING, keyed by last-31 JWT chars
  ├──UDP 8084──▶ media relay      VIDEO/FEC/NACK/KEYFRAME, watcher-set fan-out, drain-loop
  └──HTTPS 8085▶ attachments      tus init/PATCH/complete/GET, JWT bearer, keep-alive, channel-aware
```

- **Single io thread** still services all five listeners. Anything that blocks or crashes
  it stalls/kills voice for everyone — the recurring theme below.
- **JWT is Ed25519** now (was HS256). Central holds the private key and mints; each community
  holds only the public key and verifies. Claims: `sub`, **`uid`** (stable int64), `iat`,
  `exp=iat+24h`. No `aud`, `jti`, or revocation. Bans match by username **or** uid (rename-safe);
  membership/roles do **not** (finding **C2** below).
- **The "community secret" is now a bearer credential**, not the JWT key — but it's still **one
  global secret shared by every community operator** (finding **S1**).
- **Cert pinning** both directions: client TOFU-pins central and pins each community to the
  fingerprint central advertises; community TOFU-pins central. Central pins nothing and never
  validates that a heartbeat's fingerprint belongs to the reporting community (feeds **S1**).
- **Stale docs**: `ARCHITECTURE.md:33,586` and `attachment_http.hpp:30` still say HS256;
  `electron-client/HANDOFF.md` stops at 0.7.1 and describes a "FFmpeg removed" world that no
  longer holds (native encode is back). Refresh when convenient.

---

## 1. Bugs (ranked)

Sev key: **High** = crash / server-wide stall / privilege or identity break.
**Med** = a feature is broken or a moderation control is bypassable. **Low** = bounded/cosmetic.

| # | Sev | Where | Finding |
|---|-----|-------|---------|
| **A1** | **High** | `attachment_http.cpp:266-271` vs `:570` | **NULL `FILE*` → whole-server SIGSEGV.** `arm_deadline()`'s timer handler does `fclose(*patch_fp_); *patch_fp_=nullptr; close()`. `expires_after()` can't cancel a completion already queued `ec==success`. If a PATCH read completion and the 30s expiry land in the same reactor batch and the timer dequeues first, the read-success path runs `fwrite(..., *patch_fp_)` at :570 with a NULL stream → glibc segfault, taking chat + voice + both relays with it. The error path at :566 null-checks; the success path doesn't. Attacker can retry every 30s to align the batch. Fix: generation-counter guard on the deadline (like `Session::closing_`), and null-check `*patch_fp_` on the success path. |
| **C2** | **High** | `main.cpp:847`, `:898`; `db.cpp` `set_member_uid` | **Username reuse inherits the old member's roles.** Membership at auth is decided by `is_member(username)` only; the token's `uid` claim is never compared to the stored `members.uid`. If "alice" (uid 42, admin role) is renamed/freed at central and someone registers "alice" (uid 99), their valid JWT authenticates straight into alice's member row **with her roles + member overwrites**, and `set_member_uid` then re-stamps the row to uid 99, erasing the evidence. This is exactly the hole v8 closed for *bans*; membership/roles got no equivalent. Fix: at auth, if `member.uid>0 && token.uid>0 && differ` → treat as not-a-member (require invite); make `set_member_uid` fill only `uid=0` rows. (Impact assumes central permits rename/re-registration — it does; the community-side hole is verified regardless.) |
| **A2** | **High** | `main.cpp:1923-1943` ban purge | **Ban-with-purge stalls the whole server.** The purge loop calls `broadcast_to_channel()` once per deleted message (each = fresh frame + full `sessions_` scan + per-session `channel_permissions()` resolve) plus 5 synchronous `filesystem::remove` per attachment. `delete_messages_by_sender_since` is capped to 7 days but **not to a message count**. Banning a spammer with thousands of recent messages — the exact use case — is O(messages × sessions) synchronously on the io thread → voice/video/chat freeze for everyone. Fix: one batched deletion packet (id-range, like `CHANNEL_PRUNED` uses), or chunk + `post()` the fan-out. |
| **I1** | **Med-High** | `main.cpp:2958-2991` `SessionManager::leave` | **Sibling-session disconnect destroys a live stream.** `leave()` erases the user's `active_streams_`, `stream_watchers_`, and thumbnail cache by **username**, with no owner-session guard — even though `udp_key_index_` four lines up has exactly that guard (its comment describes this very reconnect race). Sequence: client reconnects (new session, same JWT), rejoins voice, restarts stream; ~30-90s later the stale predecessor session hits the idle deadline and its `leave()` deregisters the **new** session's stream → watchers freeze, presence drops it, thumbnail dies, encoder keeps running. Fix: record the owning session in the stream state and only erase user-keyed maps when it matches (or when `sessions_by_user_[user]` is empty). Same root family: **I2** `main.cpp:3712-3733` — keyframe/NACK relay targets the *first* session in `sessions_by_user_`, which after reconnect is the dead predecessor, so PLIs go to a closed socket and watchers can't force an IDR. |
| **M1** | **Med [×2]** | `main.cpp:2607-2631`; `authz.hpp:172-185`; relay `:4517` | **Timeout does nothing to a member already in voice/stream.** `timed_out()` denies SendMessage/Attach/Connect/Stream at *request* time, but `TIMEOUT_MEMBER_REQ` neither disconnects the target from voice nor stops their stream, and the UDP relay only checks `is_server_muted()`. Timing out someone mid-rant in voice — the flagship use — visibly does nothing until they leave on their own (up to 28 days). Fix: on `until>now`, `disconnect_from_voice(target)` (which also stops the stream), mirroring the ban path. |
| ~~**M2**~~ | — | `main.cpp:4517` vs `:4521-4528` | ~~Server-mute doesn't gate STREAM_AUDIO.~~ **Not a bug — intentional** (owner, 2026-08-23): server-mute is talk-only; a muted member's stream/desktop audio is deliberately still relayed. |
| **M3** | **Med [×3]** | `main.cpp:2895` (`last_message_at_`), `:4017` buckets; no per-user session cap | **Slowmode and all rate limits are bypassable by opening multiple connections.** Token buckets and the slowmode map live on the `Session`; `sessions_by_user_` is uncapped. N connections with one JWT = N× every limit and a fresh (empty) slowmode map each; a plain reconnect also resets slowmode. Contrast timeouts, which are DB-backed and can't be bypassed this way. Fix: cap authenticated sessions per username (close oldest at ~5), and key slowmode + the message bucket per-username in the manager. |
| **D1** | **Med [×2]** | `db.cpp` `create_channel` (~`:1723-1823`) | **`create_channel` never invalidates `channel_perm_cache_`.** `channel_permissions_unlocked_` caches `(user,channel)→0` for unknown ids, and `CHANNEL_HISTORY_REQ`/`CHANNEL_OVERWRITES_REQ` run authz before any existence check. Delete a channel, a stale client probes it (caches 0), admin recreates under the same slug (the common delete+recreate fix) → that user's channel list omits it and every action is denied until an unrelated role/overwrite/channel mutation or restart clears caches. `delete_channel` and `set_overwrite` both invalidate; `create_channel` is the one mutation that doesn't. Fix: clear/`erase` the cache in `create_channel`. |
| **M4** | **Med** | `authz.hpp:110-198` | **Timeout doesn't restrain a timed-out member's *powers*.** `check()` consults `timed_out()` only for send/attach/connect/stream. A member below the actor can still hold MANAGE_MESSAGES/CHANNELS/KICK etc. and, while "timed out," delete others' messages, wipe/rename/delete channels, kick lower members, mint invites. The header frames this as intentional; flagging because it means timeout is not an effective restraint. **Decide the rule** before building more timeout-gated actions — Discord suppresses all perms under timeout. |
| **H1** | **Med** | `attachment_http.cpp:411`, `main.cpp:3557` | **No storage quota.** Init limiter is ~43k/day, each upload up to `max_attachment_bytes` (100 MB), ready-unbound rows live 24h and bound rows until retention (off by default). One ATTACH_FILES member can retain TBs and ENOSPC the volume that also holds the SQLite DB → PATCH failures 500 everyone. Fix: per-user and global byte quota at init (sum of live `expected_size`) + an ENOSPC headroom check. |
| **X1** | **Med** | `attachment_http.cpp:786-797` | **Range parser violates RFC 7233.** `bytes=-500` (suffix) is served as the first 501 bytes with a wrong `Content-Range`; `bytes=0-<huge>` returns 416 instead of clamping. Chromium mostly sends open ranges (which work); the Windows Media Foundation path the proxy exists for may send suffix ranges (UNVERIFIED whether it does in practice — the spec violation is certain). Fix: implement suffix ranges, clamp `end=min(end,total-1)`. |
| **DB1** | **Med** | `db.cpp:148-157` + multi-stmt writes | **No `sqlite3_busy_timeout`; `BEGIN IMMEDIATE` return unchecked.** With a single internal mutex, normal operation is safe. But any *external* writer on the DB file (operator `sqlite3` CLI, backup tool) makes `BEGIN IMMEDIATE` fail with BUSY; the following statements then run in autocommit and COMMIT/ROLLBACK is a no-op → partial writes (e.g. member_roles stripped but ban not inserted). Fix: `sqlite3_busy_timeout(db_, ~2000)` at open, and check the BEGIN result before proceeding (bail if it didn't start). `add_ban`/`delete_channel`/`wipe_channel`/`delete_message` already ROLLBACK on failure; the gap is undetected BEGIN failure. |
| **X2** | **Low-Med [×2]** | `main.cpp:1909`; `db.cpp:1017` | **Ban `reason` has no length cap.** Every other user string goes through `clamp_utf8` (nick 32, names 64, desc 512, audit 512); ban/kick reason is stored raw (up to the 2 MiB frame cap) and re-broadcast in `BAN_LIST_RES` to every ViewBans holder on each ban/unban/ownership-transfer. Storage bloat + broadcast amplifier writable by any BAN_MEMBERS holder. (UTF-8 corruption not possible — protobuf runtime rejects invalid strings at parse.) Fix: `clamp_utf8(reason, 512)` in the handler. |
| **X3** | **Low** | `attachment_http.cpp:485,627,845,975` | **PATCH/complete/thumbnail/DELETE don't re-check membership.** init/GET/HEAD check `is_member`; the four id-addressed mutations check only `uploader==username_`. A member kicked mid-upload with a still-valid JWT can PATCH to completion + push thumbnails; data sits 24h. Inconsistent with the chat path's per-packet re-check. Fix: add `is_member` (PK lookup) to the four handlers. |
| **X4** | **Low** | `attachment_http.cpp:597-623` | **HEAD skips the VIEW_CHANNEL check** that GET applies, so any member can probe attachment ids in overwrite-hidden channels and learn existence/size/status. Fix: apply the ViewChannel check for ready rows with a channel_id. |
| **I3** | **Low** | `main.cpp:1611-1627`; `:2663-2680` | **Two info-leak / access nits.** `MESSAGE_DELETE_REQ` calls `get_message_sender` before any VIEW gate → a member can probe message ids in channels they can't view (distinct "not found" vs "no permission"). And `VOICE_MOD MOVE` verifies only the *target's* ConnectVoice, not the moderator's access to the destination — a mod can move someone into a channel the mod can't see. Fix: gate the sender lookup on ViewChannel; require the actor's ConnectVoice on the MOVE destination. |
| **R1** | **Low** | `main.cpp:4537-4554`, `:4424`, `:4950` retry | **A thrown exception in a UDP-receive or accept completion silently kills that subsystem forever.** The `for(;;) run()` retry re-enters the loop, but `do_receive_{voice,media}_udp`/`do_accept` re-arm at the *tail* of the handler; a throw (e.g. `bad_alloc` in fan-out) unwinds past the re-arm and that socket is dead with no distinguishing log, while the process "keeps serving." Session read chains self-heal via the deadline timer; these don't. Fix: try/catch the handler bodies so the tail re-arm always runs. |
| **R2** | **Low** | `main.cpp:4473`, `:4566` | **Both UDP PING echoes are unauthenticated 1:1 reflectors** (no key check; media echoes even on failed sender lookup). No amplification, but a spoofed source can bounce traffic off 8083/8084. Fix: echo only when the token/endpoint lookup succeeds; enforce min header size. Related: `relay_nack` (`:4617`) has no throttle or watcher-relationship check (keyframe relay got a 250ms throttle; NACK didn't) — junk-flood relay, though today's client ignores inbound NACKs. |

---

## 2. Central server (`src/server/`)

| # | Sev | Where | Finding |
|---|-----|-------|---------|
| **S1** | **High (infra)** | `auth_manager.cpp:341` | **One global community secret → any operator can hijack another community's directory row + pinned cert.** The heartbeat upsert `DELETE FROM community_servers WHERE host_ip=$1 AND port=$2 AND id<>$3` plus the fingerprint write means any holder of the shared secret can evict a competitor's row and republish the `cert_fingerprint` clients pin against. The comment acknowledges the single-secret weakness. **Fix before onboarding third-party operators**: per-community credential (central issues a secret at registration, keyed to server_id), and validate that a reported fingerprint belongs to that community. |
| **S2** | **Med** | `main.cpp:222-246`; `auth_manager.cpp:166` | **bcrypt cost-12 runs synchronously on the io thread with no login throttle** (and a deliberate second full hash on the unknown-user path). Unauthenticated login spam is a trivial whole-central CPU stall. Fix: offload hashing to a worker, add per-IP attempt throttling. |
| **S3** | **Med** | `main.cpp:1060` | **`check_dm_allowed` runs `getFriends()` (fresh Postgres connection + query) while holding `SessionManager::mutex_`** on the io thread — every friends-only DM blocks all broadcasts + presence. Fix: resolve friends before taking the lock (like `isBlocked` already does). |
| **S4** | **Low** | `main.cpp:1095` | **`do_accept` re-arms unconditionally on error → EMFILE 100% CPU spin.** This is community's B9, never applied central-side (community got `accept_backoff_`). Fix: backoff timer on accept error. |
| **S5** | **Low** | `main.cpp:963` | **`kick_user` breaks after the first session** and the token is never re-examined after `LOGIN_REQ`, so a user holds multiple central sessions past logout/expiry. Fix: evict all sessions for the username; check `exp` on the idle sweep. |

Also: `SYNC_SERVER_PICTURE_REQ` stores community-supplied bytes + version with zero validation (the avatar path checks JPEG magic + size); only the 2 MiB frame cap bounds it.

---

## 3. Client (`electron-client/`)

| # | Sev | Where | Finding |
|---|-----|-------|---------|
| **CL1** | **Med** | `stores/voiceStore.ts:143`; `useVoiceEvents.ts:64` | **Voice presence keyed by bare `channelId`.** Every server seeds identical slugs (`voice-lounge`, `voice-lounge-2`), so with two servers connected, server B's occupants show under server A's channel. `chatStore` already fixed this with `channelKey(serverId, channelId)`; `voiceStore` didn't get it. |
| **CL2** | **Med** | `net/community.rs` (JWT captured at construct, never reassigned) | **Community JWT never refreshed.** Central issues 24h tokens; the community reconnect loop re-sends the JWT captured at connect. A session alive >24h eventually reconnects with an expired token → `error_code:"auth"` → `terminated` → server dropped permanently with a "Couldn't join" toast. Fix: refresh from the central session's current JWT on reconnect. |
| **CL3** | **Med** | `net/central.rs:302` + `commands/servers.rs:360` | **A central-socket blip tears down every healthy community session.** The central reconnect's silent re-login fans out `connect_with_invite` for all memberships, which unconditionally removes + disconnects the existing `CommunityClient` — churning voice/stream presence server-side while the local VoiceEngine keeps sending UDP. Fix: skip reconnect for communities already connected. (This bug is also what usually *masks* CL2.) |
| **CL4** | **Low-Med** | `features/servers/permissions.ts:244` | **Permission checks fail open when the resolved mask is 0** (`mine===0 → return true; // legacy`). A v2 server that legitimately resolves a channel to zero bits is treated as legacy → composer/attach/voice-join enabled. Cosmetic only if the server is always authoritative (it is), but the UI offers actions the server will refuse. Fix: distinguish "legacy (field absent)" from "resolved to 0". |
| **CL5** | **Low** | `features/servers/useServerEvents.ts:426` | **A server-rejected send leaves the optimistic bubble forever.** `mod_action_responded` (slowmode/timeout/missing-SEND all reply `action:"message"`) carries no nonce, so the `id:0` bubble is never reconciled. Fix: include the nonce and call `removeMessageByNonce`. |

Protocol gaps (server accepts, client never sends): `UPDATE_CAPABILITIES_REQ` and `STREAM_CODEC_CHANGED_NOTIFY` — so mid-call codec/caps changes (AV1 toggle, encoder hardware→software fallback) never reach peers/watchers until a voice rejoin. `caps_refreshed` client event is dead. No server→client type is dropped.

---

## 4. Perf / cleanup (short list)

- **P1 — `broadcast_to_members` / `broadcast_to_voice_channel_tcp` / `send_initial_voice_presences` call `deliver()` under `mutex_`** (`main.cpp:3223,3627,3942`), so SSL record encryption of a 128 KB thumbnail × every member runs under the lock every relayed datagram must take. Convert to snapshot-then-deliver (the other broadcasts already do).
- **P2 — `is_member()` uncached SQL on every post-auth packet** (`main.cpp:951`), and it runs *before* rate limiting so dropped packets still pay it. Cache membership per session; rate-limit first.
- **P3 — `CHANNEL_MSG` fetches the channel row twice** (existence at `:1235`, slowmode at `:1259`) + `timeout_until`'s full `get_member` = 4 DB hits/message before insert. Reuse the first fetch; fold `timed_out_until` into the perm-cache entry.
- **P4 — `list_bans` runs a DELETE sweep on every read** (`db.cpp:1035`) → a write txn/WAL churn on every ViewBans auth + broadcast. Move expiry sweeping to the retention timer; keep reads read-only.
- **P5 — `member_count()` materializes every row per 60s heartbeat** though `count_members()` exists (`main.cpp:4040`). One-liner.
- **P6 — central-sync worker: dropped revokes never re-fire** (registers re-fire on next auth; revokes don't), and a down central burns 5s/job with no dedupe (`central_sync.hpp`). Add per-(type,key) coalescing + fail-fast backoff.
- **Dead code / stale comments**: `can_delete_others()` (`db.cpp:2577`) unused at the wire layer; `db.hpp:645` comment still describes the old every-boot channel seeding (behavior is correctly versioned now); the `431` branch in `attachment_http.cpp:283` is unreachable (16 KB streambuf cap drops oversized heads first).

---

## Fix status

**Batch 1 (2026-08-23) ✅** — stop-the-bleeding, server-side, verified against the
standalone build + e2e harness (`src/community/tests/`, now 174 checks incl. a new
`test_c2_username_reuse`):

- **A1** — attachment PATCH crash: generation guard on the inactivity timer +
  null-check `*patch_fp_` on the read-success path (`attachment_http.cpp`).
- **C2** — identity is now anchored on the stable `uid`, not the reusable username:
  auth resolves membership via `get_member_by_uid`; a reused name is admitted only as
  a fresh member (never inheriting roles), a central rename carries roles via
  `rename_member`; `set_member_uid` only back-fills `uid=0` rows; `redeem_invite`'s
  already-member check is uid-aware; `add_member` evicts a stale same-name row; a
  partial unique index enforces one row per `uid` going forward. (`main.cpp`, `db.cpp`,
  `db.hpp`.) The wire protocol stays username-keyed (client unchanged); the known
  residual is that a member who is renamed AND whose old name is claimed before they
  next log in must re-join — safe (no inheritance), documented.
- **A2** — ban-purge fan-out: `broadcast_message_deletions` scans the session list
  once and resolves each viewer's channel access once, instead of per message.
- **I1** — `StreamInfo` records the owning session (`weak_ptr`); `leave()`/`stop_stream`
  only tear down user-keyed stream/watcher/thumbnail state when the acting session owns
  it, mirroring the `udp_key_index_` guard — a stale session's disconnect no longer
  kills a reconnected session's live stream.
- **R1** — the two UDP receive completions and the accept completion wrap their bodies
  in try/catch so the tail re-arm always runs; a throw no longer silently kills a
  subsystem.

**M2 is intentional** (owner clarified 2026-08-23): server-mute is talk-only; a muted
member's stream audio is deliberately still relayed. Removed from the bug list.

**Batch 2 (2026-08-24) ✅** — make moderation actually moderate, verified against the
standalone build + e2e harness (now 188 checks; new `test_m1_m4_timeout`,
`test_m3_slowmode_per_user`, `test_m3_session_cap`):

- **M1** — `TIMEOUT_MEMBER_REQ` now calls `disconnect_from_voice(target)` on an active
  timeout (not on clear), so a member timed out mid-call is pulled out of voice and their
  stream stopped, instead of continuing until they leave. (`main.cpp`.)
- **M4** — decided rule: a timed-out member is **benched from all non-passive actions**,
  management and moderation included; only reading (view channel/history/bans/audit/
  overwrites) stays. Enforced uniformly at the top of `Authorizer::check` via
  `is_passive_action`, replacing the four scattered per-case timeout checks. (`authz.hpp`.)
- **M3** — slowmode moved off the `Session` into a per-**username** map in the manager
  (`slowmode_remaining`/`slowmode_record`), so a second connection or a reconnect can't
  reset the window; the window is stamped only after a message is accepted+persisted, so a
  rejected (oversized/empty/unsaved) message no longer consumes it. Concurrent sessions per
  user are capped (`kMaxSessionsPerUser = 8`, oldest evicted), bounding the per-session
  rate-limit/slowmode multiplication. (`main.cpp`.)
- **D1** — `create_channel` now clears `channel_perm_cache_`, so a delete→recreate under
  the same slug no longer leaves a stale "channel doesn't exist → 0" negative that hides
  the new channel from a user who probed the id in between. (`db.cpp`.)

**Storage feature (2026-08-25) ✅** — H1's headroom half + a Storage settings tab
(cross-stack: proto → C++ → Rust → React; server side covered by 7 new e2e checks, 195 total):

- **Upload headroom (H1)**: `attachment_http` init queries `std::filesystem::space`
  (cross-platform: statvfs / GetDiskFreeSpaceEx, `ec` overload so it never throws on the
  io thread) and refuses with `507 Insufficient Storage` when a declared upload would leave
  the store's volume below the configured minimum free. Per-user quota was **not** added
  (owner's call — headroom only).
- **Storage tab** (MANAGE_SERVER): new `STORAGE_INFO_REQ/RES` + `STORAGE_CONFIG_SET_REQ`.
  Server reports the store volume's total/available (`filesystem::space`) plus this
  community's footprint from the DB (`SUM(size_bytes)`/thumbnails/SQLite file sizes, by-kind,
  by-channel, largest — no filesystem walk). The min-free threshold is editable in the tab,
  persisted in `server_meta`, clamped to `[0, volume capacity]`, audited, and env-seeded
  (`DECIBELL_MIN_FREE_BYTES`) on first boot. Client: `get_storage_info` /
  `set_storage_min_free` commands, a `storage_info_received` event, and `StorageTab.tsx`
  (usage bar, footprint, by-type/by-channel/largest, editable headroom).
- **X8 ✅** (2026-08-25): `finish_patch` now checks `fflush`/`fclose` and returns 500
  instead of a 204 that lied about the stored offset — a disk-full write that slips past the
  507 (fwrite only buffers; ENOSPC surfaces at flush/close) now fails loudly, and the client
  re-HEADs and resumes from the real on-disk offset. Partial file left intact for resume.

**Hardening batch (2026-08-25) ✅** — DB1 + X3 + X4 (198 e2e checks, 3 new):

- **DB1**: `sqlite3_busy_timeout(db_, 2000)` at open (in-process access is already
  mutex-serialized; this only matters for an external writer — operator `sqlite3` CLI,
  backup/WAL-copy tool), and every multi-statement write now checks the `BEGIN IMMEDIATE`
  return and bails cleanly instead of degrading into autocommit partial state (member+banned,
  stripped roles, etc.). 14 transaction sites guarded; `redeem_invite` left unguarded on
  purpose (single-mutation per path — safe under autocommit, and still lands atomically under
  contention thanks to the busy_timeout).
- **X3**: `handle_patch` / `handle_complete` / `handle_thumbnail_upload` / `handle_delete` now
  re-check `is_member` (cheap PK lookup), so a user kicked/banned mid-upload with a still-valid
  JWT can't drive the upload to completion — matching the chat path's per-packet re-check.
- **X4**: `HEAD` on a ready attachment now applies the same `VIEW_CHANNEL` gate as `GET`, so an
  overwrite-hidden channel's attachments can't be probed for existence/size via HEAD.

**Public-listing / discovery join (2026-08-25) ✅** — cross-stack (proto → community →
central directory → client), server side covered by 7 new e2e checks (205 total):

- Each community has a **public listing** toggle (owner/MANAGE_SERVER), persisted in
  `server_meta`, default **off** (invite-only). Reported in the heartbeat.
- Central stores it as `community_servers.is_public` and the discovery directory
  (`getCommunityServers`) now returns **only** public, live servers.
- Community auth gate: when a non-member connects with no invite and the server is public,
  they **join directly** (bans still rejected); private servers stay invite-only
  (`not_member`). Extends `ServerUpdateRequest` with `optional public_listing`; carried on
  `ServerHeartbeat`, `ServerMetaUpdate`, `CommunityAuthResponse`.
- Client: a toggle in the Overview settings tab (`update_server` with `publicListing`), a
  `serverPublicListing` store slice fed by `community_auth_responded` / `server_meta_updated`.
  The browse-view tile click already calls `connect_to_community` with no invite, so a public
  server is now join-on-click; only public servers appear in the list, so every tile is
  joinable. (Central compiled by inspection here — no local libpqxx.)

**Message editing (2026-08-25) ✅** — own-message edit in channels + DMs, cross-stack
(proto → community + central → Rust → React), community side covered by 8 new e2e checks
(213 total):

- Wire: `MESSAGE_EDIT_REQ/RES` + `CHANNEL_MESSAGE_EDITED` (community) and
  `DM_EDIT_REQ/RES` + `DM_MESSAGE_EDITED` (central), plus an `edited_at` field on
  `ChannelMessage` / `DirectMessage` / `DmHistoryMessage`. Mirrors the delete flow.
- Ownership enforced in SQL (`UPDATE … WHERE id=? AND sender=?`), so a forged id is a
  no-op; edits require SEND_MESSAGES in the channel (also blocks a timed-out editor);
  empty/oversized rejected; `edited_at` stamped and returned on history.
- Client: `edit_channel_message` / `edit_dm_message` commands, edit event listeners
  (non-optimistic — the broadcast applies the edit, `_edit_responded` surfaces failures),
  `applyEdit`/`applyDmEdit` store setters, `edited_at` threaded through message + history
  ingestion. UI: a pencil hover-action + inline textarea editor on own messages (Enter
  saves, Esc cancels), an "(edited)" indicator, and ArrowUp-on-empty-composer to edit the
  latest own message (`RichInput.onArrowUpEmpty`). Works in both server channels and DMs
  via the shared MessageBubble. (Central compiled by inspection — no local libpqxx.)

**Message replies (2026-08-25) ✅** — reply-to in channels + DMs, cross-stack, community
side covered by 7 new e2e checks (220 total):

- Just an optional `reply_to` (parent message id) on the normal send path — no new packet
  types. Added to `ChannelMessage` / `DirectMessage` / `DmHistoryMessage`; persisted in
  `messages.reply_to` (SQLite) and `dm_messages.reply_to` (Postgres), idempotent migrations.
- Community validates `reply_to` points at a real message in the same channel, else stores
  0 (drops stale/cross-channel/forged refs); reflected on the broadcast. Echoed on
  broadcast + history.
- Client: `send_channel_message` / `send_private_message` carry `reply_to`; `replyTo`
  threaded through message/history ingestion. UI (shared MessageBubble): a reply hover
  action, a "Replying to @user" bar above the composer (Escape/✕ to cancel), and a quoted
  preview above a reply (author + one-line snippet) resolved client-side from loaded
  messages (falls back to "Original message" when the parent isn't loaded/was deleted).
  Replies are force-ungrouped so each shows its own header + preview. (Central compiled by
  inspection — no local libpqxx.)

**Windowed jump-to-message (2026-08-25) ✅** — Discord-style jump to an out-of-view
message (e.g. clicking a reply preview to an old parent) without fetching all the
intervening history. Community side covered by 8 new e2e checks (228 total):

- `ChannelHistoryRequest` gained `around_id` (context window centered on a target) and
  `after_id` (downward pagination); `ChannelHistoryResponse` gained `has_more_after` plus
  echoes of `around_id`/`after_id` so the client can route the reply (replace window vs
  append newer vs the existing older/most-recent prepend).
- `db.fetch_messages_around` fetches `id<=around DESC` + `id>around ASC` (limit+1 each) and
  stitches them oldest→newest with the target included, reporting both `has_more_before`
  and `has_more_after`; `db.fetch_messages_after` pages `id>after ASC`. The
  `CHANNEL_HISTORY_REQ` handler branches on which id is set.
- Client: a `hasMoreAfter` store slice (chatStore for channels, per-conversation on
  dmStore) + window setters `setChannelWindow`/`setDmWindow` (replace),
  `appendNewer`/`appendNewerDm` (down-page), and `resetChannelForJump`/`resetDmForJump`
  (present); `addMessage`/`addDmMessage` drop live messages while windowed (they'd land
  past a hidden gap). `useChatEvents`/`useDmEvents` route the reply by the echoed mode.
  Both ChatPanel and DmChatPanel: `jumpToMessage` fetches an around-window when the target
  isn't loaded and remounts Virtuoso centered on it (epoch key + `initialTopMostItemIndex`),
  `endReached` pages newer, and a "Jump to present" pill reloads the newest page. Sending
  while windowed snaps to present first.
- DMs (central, 2026-08-25 follow-up ✅): same design ported to Postgres —
  `DmHistoryReq.around_id/after_id`, `DmHistoryRes.has_more_after` + echoed ids;
  `AuthManager::fetchDmHistoryAround` (id<=around DESC + id>around ASC over the
  LEAST/GREATEST conversation pair, stitched oldest→newest) and `fetchDmHistoryAfter`; the
  `DM_HISTORY_REQ` handler branches on the mode. (Central compiled by inspection — no local
  libpqxx; community channel path re-verified at 228/228 after the shared-proto change.)
- Live-test fixes (2026-08-25, after Hetzner testing ✅):
  1. *Unloaded parents rendered an unclickable "Original message"* — the client could only
     resolve reply previews from its loaded window, so the around-fetch never fired.
     Discord-style fix: the server now embeds the parent's author + content on every reply
     (`reply_to_sender`/`reply_to_content` on `ChannelMessage`/`DirectMessage`/
     `DmHistoryMessage`), resolved via LEFT JOIN on history fetches and a parent lookup on
     the send path (community `get_message_preview`, central `fetchDmPreview` — the DM
     lookup is constrained to the sender↔recipient pair so a forged reply_to can't leak
     another conversation's content; central also validates DM reply_to before persist
     now). Previews always render + are clickable; a deleted parent comes back with an
     empty embedded sender and renders as an unclickable "Original message was deleted"
     tombstone. 3 new e2e checks (231 total).
  2. *First jump to a far-but-loaded message landed wrong* — `scrollToIndex` with smooth
     behavior estimates unmeasured row heights, so long hops are approximate (second click
     worked because the first pass measured them). Both panels now smooth-scroll only when
     the target is within ~5 rows of the rendered range and otherwise remount Virtuoso
     centered on the target (epoch key + `initialTopMostItemIndex`) — exact, like the
     windowed-jump landing.
  3. *Round 2 (2026-08-25): attachment-scroll glitch regression + smooth-jump fighting* —
     `jumpToMessage` is a `memo(MessageBubble)` prop (`onJumpToReply`) and had `messages`
     in its `useCallback` deps, so every history prepend / live arrival gave it a fresh
     identity and re-rendered every visible bubble mid-scroll — undoing the 2026-08-15
     attachment-scroll work. Now identity-stable (reads the store at call time, deps
     `[flash]` only). Separately, the smooth near-jump glitched back and forth because the
     eager paginator prepended a page mid-animation, shifting every index under the
     in-flight `scrollToIndex`: auto-paging is now paused for the animation window
     (`pauseAutoPagingUntilRef`, 900ms + settle catch-up), and a near-jump falls back to
     the exact remount path when a prepend is already in flight. Both panels.
  4. *Jump animation, final form (rounds 3-5)* — every scroll-driven animation glitched:
     native smooth `scrollToIndex` (eases toward a pixel computed ONCE from estimated row
     heights), the same with paging paused, a post-animation settle-snap, and even a rAF
     loop re-deriving the destination from the target row's live rect each frame (tall
     code-block rows mounting mid-flight re-anchor the scroller under the animation →
     overshoot-and-settle). Conclusion: animating the real scroll position through a
     virtualized list cannot be made clean. Final design inverts it — *structural jump
     first, visual motion second*: every jump lands via the exact remount (epoch key +
     `initialTopMostItemIndex`), and the freshly mounted list plays a 0.26s CSS transform
     slide (`jumpArriveUp/Down` keyframes, direction from target-vs-viewport) + the
     highlight flash. The transform never touches scroll geometry, so it cannot mis-land
     or bounce, and near/far/unloaded jumps all share one arrival. All scroll-driving
     machinery (pause refs, settle timer, rAF helper, `data-mid` tags) removed. Both
     panels. Round 6: the remount itself could still mis-land next to code blocks —
     `align:"center"` picks the topmost row by walking up from the target with ESTIMATED
     heights and anchors that row, so under-estimated code blocks push the target off
     center once measured. Switched to `align:"start"`: anchoring the target's own top
     edge uses no estimates and cannot drift (Discord lands jumped messages near the top
     too — their list keeps the loaded chunk as real DOM, so they never estimate at all).
     Round 7: one case remained — code blocks BELOW the target. At mount the rows below
     are still estimates; when they under-estimate, Virtuoso thinks the content below is
     less than a viewport and CLAMPS the scroll upward instead of honoring
     `initialTopMostItemIndex`, and the clamp sticks once they measure tall. Fix: a ~1s
     post-jump assertion window — `totalListHeightChanged` re-pins the target at the top
     on every height change until real heights land (`jumpAssertUntilRef`).
  4b. *Attachment-only reply previews (2026-08-25)* — a reply to a message with only
     attachments rendered as a bare `@author`. The community server now also embeds the
     parent's attachment kinds (`ChannelMessage.reply_to_attachment_kinds`, Attachment.Kind
     in position order — a GROUP_CONCAT subquery on the history JOIN, a second query in
     `get_message_preview` on the send path); the client labels the preview "Image" /
     "Video" / "Document" / "Audio" (or "2 Videos" / "3 Attachments"), preferring the
     loaded parent's live attachments when available. DMs unaffected (no attachments).
     2 new e2e checks (233 total).
  5. *Jump-to-present pill polish* — restyled to the client's accent-button idiom
     (`rounded-md bg-accent text-on-accent hover:bg-accent-hover`), so its radius tracks
     the theme scale (flat on console, soft on default) instead of the hardcoded
     `rounded-full`. It now also appears on a plain scroll-up of >20 rows above the live
     bottom (not just while windowed); clicking then scrolls to the bottom directly —
     no refetch, since the loaded history is contiguous. Both panels.

**Real-DOM message list (2026-08-27) ✅** — react-virtuoso replaced by
`features/chat/RealMessageList.tsx`, a Discord-style sliding window: the loaded slice renders
as plain DOM in an `overflow-y:auto` scroller, so every row height is real and placement
(jump landing, bottom-follow, prepend anchoring) is arithmetic done in one no-deps
`useLayoutEffect` before paint. Motivation + the postmortem of the seven Virtuoso jump rounds:
`docs/superpowers/specs/2026-08-25-real-dom-message-list-plan.md`.

- *Bounded DOM.* Past 150 rows the list asks the panel to trim the far end
  (`onOverflow(side, keep)`), cutting by pixel distance — never within 2×NEAR_PX (1600px) of
  the viewport, or a trim would land inside the paging zone and ping-pong with the paginator
  (trimTail → nearBottom → appendNewer → trimHead → nearTop …). New store setters
  `trimTail`/`trimHead` (chatStore) and `trimDmTail`/`trimDmHead` (dmStore) flip
  `hasMoreAfter`/`hasMoreHistory`, so the existing windowed machinery re-fetches the dropped
  end. Behavior change: scrolling up past ~150 rows makes a channel windowed (live arrivals
  held back, "Jump to present" pill) — as on Discord.
- *Anchoring.* Chromium's native scroll anchoring stays ON as the primary adjuster: it
  adjusts a running compositor wheel animation in place, whereas a programmatic `scrollTop`
  write cancels it (live-tested as a stutter on page-in with `overflow-anchor:none`). The
  list's own math is measured after the forced layout in which Chromium already anchored, so
  it is a residual — it writes only where Chromium doesn't anchor (offset 0, slice replaced,
  bottom-follow, jumps) or on settles with no React commit (ResizeObservers on the inner
  wrapper and the scroller). Measurement is `offsetTop` only: the arrival slide and the last
  row's `fadeUp` are transforms and would pollute rects.
- *Jumps.* `jumpTarget {id, epoch, dir}` is set at click time even before the target is
  loaded; the list lands (centered — exact, nothing is estimated) on the first commit whose
  rows contain it, i.e. an around-window paints at the right position on its first frame.
  Arrival slide via WAAPI on the inner wrapper (no remount); `flash` fires from
  `onJumpLanded`. The epoch-remount, the `align:"start"` rule and the 1s landing assertion
  are all unnecessary on this path.
- *Scroll restore* is `{anchorId, offset, atBottom}` per channel/peer (`ScrollPosition`) — a
  message id survives trims/evictions where a pixel or index would not. Positions are written
  per key by the list's `onScrollState` closure (`positionsRef`), so a non-click channel
  switch (auto-select, deep link — passive effects flush from a scheduler task) can't race
  the persist cleanup.
- *Keys.* Rows are keyed by message identity only, never index (React reuses an index-keyed
  node for a different message under prepend → wrong anchor); id-less DM rows get a WeakMap
  synthetic key. The DM sidebar preview now reads a slice-independent
  `conversation.lastMessage` (highest id ever seen) — `messages[last]` was already wrong
  after a jump window and would be after a tail trim.
- *Rollout.* Shipped behind a temporary toggle for the live checklist — passed in channels
  (bottom landing, zero-shift paging incl. the group-flip boundary, trim → pill → present,
  centered jumps loaded/unloaded, composer growth, edits/deletes above the viewport, video
  clipping, wheel-during-prepend) — then react-virtuoso and every fallback path were deleted
  the same day: `useVirtuosoPrepend`, the epoch remount + landing assertion, the
  `jumpArrive*` keyframes (WAAPI now), the `HISTORY_EAGER_THRESHOLD` eager trigger (the
  group-flip is compensated in the same commit), `attachmentPrefetch`'s warm-ahead (rows
  mount ≥ NEAR_PX before they're visible, so their `<img>` fetches are the prefetch; only
  `previewUrlFor` survives as `attachmentPreviewUrl.ts`), and `topIndex` from
  `ScrollPosition`. Opt-in placement trace `decibell.real_message_list_debug=1`. One
  unreproduced sighting of landing one row above the newest on a channel switch; DMs not yet
  exercised with a long thread (revert the removal commit if needed).

**Unread-DM tiles in the ServerBar; vertical DM rail removed (2026-08-27) ✅** — the 68px
avatar column on the far left (`layouts/DmSidebar.tsx`) listed every DM conversation
permanently and duplicated `ConversationSidebar` (the home / dm views' list). It is gone; the
ServerBar's home button and server tabs are unchanged. In its place `ServerBar.tsx` renders
`UnreadDmTiles` immediately right of the home button: one avatar tile per peer with
`unreadCount > 0`, most-recent activity first (leftmost = newest), with the unread-count
badge; clicking opens the conversation. A tile exists exactly as long as the conversation is
unread — the existing `DmChatPanel` mark-read effect zeroes `unreadCount` when the
conversation is on screen, so the tile drops the moment it is opened. Nothing new in the
store: the tiles are a filtered view of `dmStore.conversations` (`addDmMessage` already
declines to bump the count for the conversation being viewed, so a live DM from the open peer
never spawns a tile). Fallout: the ServerBar's bottom separator now runs full width (it used
to start after the home column so bar + rail read as one strip); the mini stream player's
left inset is the window edge (`[data-pip-dm-rail]` is gone — covering the channel list was
already allowed). Not carried over from the rail: presence dots (the tile is a notification,
not a roster entry — presence lives in `ConversationSidebar`).

**Inset workspace panel (2026-08-27) ✅** — follow-up to the rail removal: with the chrome tone
no longer running down the left edge, the sidebar's `bg-bg-dark` met the frameless window edge
directly and the left side read as cut off. The chrome is now the ground and the workspace
floats in it: `MainLayout` wraps the content row in an 8px chrome-toned gutter (left / right /
bottom — the ServerBar's own bottom padding is the top one) and the sidebar + main area sit
inside as a `rounded-lg` panel with a `border-border` ring (12px in the graphite themes, the
console themes' 4px through the same token). The gutter paints via a new `.chrome-ground` rule
(`globals.css`) that reads `var(--tc-chrome, var(--t-chrome))` directly — the resolution
`.chrome-scope` does for `--color-bg-darkest` — as a background only: a `chrome-scope`
ancestor would re-scope `--color-*` and drag the chat canvas into the chrome ramp in
`console-split`, and a negative-z backdrop would need `isolate` on `MainLayout`, which would
flatten the image viewer (z-200) / profile popup under the AppLayout-level toasts (z-90).
ServerBar: bottom separator dropped (the gap separates), home button left-aligned with the
panel edge. `MiniStreamPlayer` docks to the panel rect on all four sides (`panelBounds`)
instead of the window edge.

**Layout memory (2026-08-27) ✅** — the left sidebar's width was per-component React state in
`useSidebarResize`, so it reset not only on restart but on every home ↔ server switch
(`ConversationSidebar` and `ServerChannelsSidebar` are different mounts); the mini stream
player's width and docked corner were unpersisted `uiStore` fields. All three are now
`uiStore` fields read from localStorage at store creation (`decibell.layout.*` — synchronous,
so the first `MainLayout` paint already has the width) and written by their setters through a
per-key 200ms trailing coalesce (`setPipWidth` fires per pointermove during a resize).
`sidebarWidth` is one value shared by both sidebars — only one is mounted at a time and "the
sidebar is this wide" is one preference. The bounds (`SIDEBAR_WIDTH_*`, `PIP_WIDTH_*`) moved
to the store so a stored value is clamped on read too. localStorage rather than the native
config blob on purpose: per-install view state that must not roam to a machine with a
different window size (same reasoning as Electron's own window bounds). The members-list and
DM friends-list toggles are remembered the same way. Also fixed: `ConversationSidebar` kept the
last-opened conversation highlighted on the home view (sticky `activeDmUser`, no `activeView`
gate — the removed rail had one).

**Confirm-dialog shell + server-bar polish (2026-08-27) ✅** — `components/ConfirmModal.tsx`
is the shared shell for the small destructive confirmations (delete message, leave server):
the settings modals' chrome — plain darkening backdrop (the `backdrop-blur-sm` is gone), 300ms
fade + scale in and out with the double-rAF start — and Esc / Enter handling in one place.
The parents render it unconditionally and drive `open`; gating the mount on `activeModal` (the
old pattern) unmounted it the instant it closed, so there was never a fade-out. The body is
frozen while closing so a parent clearing its state on confirm doesn't blank the copy
mid-fade, and only the backdrop's own `transitionend` unmounts (a button's colour transition
bubbles the same event). `CertMismatchModal` / `DeepLinkJoinModal` still blur — different
kind of dialog, untouched. Also: no hover lift on the server-bar tiles (server + unread-DM),
matching the home button; the server-name button in the channels header is sized to its
content (name + chevron) instead of `flex-1`, with the Public/Private badge kept at the right
edge via `ml-auto`.

**`font-meta` split from `font-mono`; graphite metadata in Inter (2026-08-27) ✅** — graphite
set its metadata face (`--t-font-meta`) to IBM Plex Mono on purpose, and every timestamp /
status / count / uppercase group label reached it through the `font-mono` utility — so the
"terminal font" the user noticed in graphite was the token, not stray classes. But the same
utility also dressed real code, certificate fingerprints, invite codes and addresses, which must
stay monospace in every theme. The token layer now separates the two: `font-meta` →
`--t-font-meta` (each theme's metadata face: Inter in graphite / graphite-light, JetBrains Mono
in console*), `font-mono` → new `--t-font-code` (Plex in graphite, JetBrains in console — real
monospace everywhere). 24 metadata sites moved to `font-meta` (message timestamps, presence
labels, friends / members section titles and counts, Public/Private and role badges, attachment
meta rows, Appearance readouts, the emoji shortcode); code blocks, inline code, the rich
composer, MathTex fallback, CertMismatch, DeepLink host + code, InviteModal code, the browse
address input, type-to-confirm names, `<kbd>` and the stream stats overlay stay `font-mono`.
Graphite's meta / micro / section sizes sit 0.5px above their Plex values (Inter's glyphs are
narrower, so the same px reads lighter); console is untouched.

**Hyperlinks + link previews (2026-08-27) ✅** — links in messages were plain text. Now:
(1) the rich-text parser autolinks `http(s)://` URLs — recognised *before* the emphasis
markers so a URL's own `_` / `*` / `~` (wiki paths, query strings) can never open a span; a
URL runs to whitespace / `<>"` / backtick, then sheds trailing sentence punctuation and any
unbalanced closing bracket (`/Foo_(bar)` keeps its paren, `(see https://x.y/z).` doesn't);
scheme-only like Discord so `file.txt` never turns blue; `<https://…>` is the link-without-a-
card form. Links render as `<a>` and open in the OS browser through a new validated
`decibell:shell:openExternal` IPC (the renderer never navigates — `hardenNavigation`).
(2) Previews are unfurled by **main** (`electron/main/linkPreview.ts`; the renderer's fetch is
CORS/CSP-bound): OpenGraph → Twitter card → `<title>` / `description`, with an oEmbed
discovery fallback for the YouTube/Vimeo class, direct-image URLs as image embeds, and a
header-only PNG/GIF/WebP/JPEG dimension probe so every image box is reserved before the
pixels land (the attachment list's no-pop rule). Guards, since this is main-process HTTP
driven by other people's message text: http(s) only, no userinfo, redirects followed by hand
(≤5, each hop re-validated), hostnames resolved and refused when any address is loopback /
private / link-local / CGNAT / multicast, 512 KiB page + 64 KiB image read caps, 8 s
timeout, 4 in flight, cache (30 min hit / 5 min miss) + in-flight dedup. Client-side rather
than server-side on purpose: no proto/server work, works against every server version, and
the trade — the linked site sees the reader's IP, as clicking would — is the new Privacy-tab
toggle **Show link previews** (`link_previews_enabled` in the native config, default on).
(3) `features/chat/LinkEmbeds.tsx` renders up to three cards under a message's attachments:
site card (site name / title / description, side thumbnail or — for `summary_large_image`,
player and video pages — a full-width image, theme-color edge) and direct images sized by
the same sqrt-scaled `reserveBoxFor` the attachment list uses, opening the lightbox on click.
No skeleton: a card appears once its data is in (a skeleton that later vanishes is a second
layout change for nothing); the one growth when it lands is the settle `RealMessageList`'s
ResizeObserver already absorbs. The composer's live preview ignores autolinks
(`hasFormatting`) so typing a URL doesn't summon it; DM-list previews flatten links to their
text. Remote images stay direct `<img src=https:>` (already in the CSP's `img-src`); `http:`
image references are upgraded to https and simply hide on failure.

**GIFs: picker tab + animated attachments (2026-08-27) ✅** — the emoji picker gained an
**Emoji | GIFs** tab strip (remembered in `decibell.picker.tab`). The GIFs tab
(`features/chat/GifPicker.tsx`) is a search box over **Tenor v2** — Discord's provider — with
Tenor's featured feed while the query is empty, a two-column masonry whose cells reserve their
height from the preview dimensions, infinite paging on Tenor's `next` cursor, a monotonic
request id so a stale page can't land on a newer query, and the "Powered by Tenor" attribution
its terms require. Clicking a GIF **sends it as a message of its own**: the message text is the
`https://media.tenor.com/….gif` URL, so every client (old ones included) sees a link, and this
client's link preview renders the animated image. The typed draft and queued attachments are
untouched; a pending reply target is consumed by the GIF. The search runs in **main**
(`electron/main/gifs.ts`) so the API key never reaches the renderer and CORS is moot (Tenor's
error responses carry no ACAO). Key provisioning mirrors the Sentry DSN: CI bakes
`resources/tenor.json` from a **`TENOR_API_KEY` secret** (new release-workflow step +
`extraResources` entry); a dev checkout reads `electron-client/resources/tenor.json` or the
`TENOR_API_KEY` env var; without one the tab says so instead of searching. Two rendering rules
came with it: (1) a message that is *only* a media link hides its URL text once the link
resolves to an image embed (`loneLink` in richText.ts; the bubble subscribes to the
link-preview entry), so a sent GIF is just the GIF, and the DM list previews it as "GIF" /
"Image" (`mediaLabelFor`) instead of a CDN URL; (2) **uploaded GIFs now animate** —
`previewUrlFor` returns the original for `image/gif`, since the upload-time thumbnail is a JPEG
of the first frame and the chat had been showing that frozen frame.

**GIF provider: Tenor → KLIPY / GIPHY (2026-08-27) ✅** — the entry above shipped against Tenor
v2, which Google discontinued on 2026-06-30 (no new keys since 2026-01-13); Discord moved to
GIPHY and KLIPY. `electron/main/gifs.ts` is now a provider layer over both vendors' **native**
APIs (not their Tenor-compatibility shims, which are second-class — GIPHY's documents "partial
support for mapped rendition names only"): KLIPY `api/v1/{key}/gifs/{search|trending}`
(page-numbered, `data.data[].file.{hd,md,sm,xs}.gif`, `has_next`; `content_filter=medium`,
`format_filter=gif`, `locale` = the region of `navigator.language`) and GIPHY
`v1/gifs/{search|trending}` (offset-paged, `images.{original,fixed_width,…}`, `rating=pg-13`,
`lang`). Both normalise to the same `GifResult` and an opaque `next` cursor, so the picker is
provider-blind except for the attribution the vendors require (placeholder "Search KLIPY" /
"Search GIPHY", footer "Powered by …"). Config is `resources/gifs.json`
`{"provider","key"}` — CI: `GIF_API_KEY` secret + `GIF_API_PROVIDER` variable (default
klipy); dev: the same env vars or the file. Both vendors' error shapes were verified live
(KLIPY `result:false, errors.message[]`; GIPHY `meta.msg`), and KLIPY end to end with a real
test key on 2026-08-27 (trending, search, send → animated in chat). KLIPY is the default:
free production access on request; the key model is one app-level key baked per release
(the norm for GIF pickers; read-only, no billing attached — see HANDOFF §5.9a).

**GIF content filter: `low` by default + "Unfiltered GIF search" toggle (2026-08-27) ✅** — the
picker searches with KLIPY `content_filter=low` / GIPHY `rating=r` (only explicit adult
content dropped — the owner wants the widest catalogue), and a Privacy-tab toggle
(`gif_unfiltered` in the native config, default off) sends `off` / no rating instead for
users who want everything. The preference travels with each `decibell:gifs:search` call, so
flipping it with the picker open re-runs the current query.

**Invite cards in chat (2026-08-27) ✅** — a `decibell://invite/<host>:<port>/<code>` link in a
message now renders Discord's "You've been invited to join a server" card
(`features/chat/InviteEmbed.tsx`) in the browse directory's card shape — the server picture as
a wide banner on top (gradient initial when none), name / description / member count below —
with a **Join** button that redeems the invite in place (`redeem_invite`, the DeepLinkJoinModal path
without the second confirm — the card is the preview); "Joined" once the auth response lands
(checked under both the `host:port` key the join connects with and the central id it is
re-keyed onto), "Invalid invite" when central says unknown / expired. The plain link text
still opens DeepLinkJoinModal. Wire: `InviteResolveResponse` gained `server_id`,
`server_name`, `server_description`, `member_count`, `picture_version` (fields 7–11) —
central's `resolveCommunityInvite` LEFT JOINs the invite onto its `community_servers`
heartbeat row, so a private community previews too (the invite is the authorisation, as on
Discord); zero / empty from older centrals or a community that never heartbeated, in which
case the card titles itself `host:port` and Join still works (the community checks the code
itself). Carried through the three sides: central (`auth_manager` + `main.cpp`), Rust
(`ResolvedInvite` napi struct, prost regen), renderer (`ResolvedInvite` type). The picture
comes through the existing public `FETCH_SERVER_PICTURE_REQ` path — `useFetchServerPictureIfMissing`
was lifted out of ServerBrowseView into `features/servers/useServerPicture.ts` and the card
registers the version first so `server_picture_received` accepts the bytes. One invite-link
grammar now lives in `features/servers/inviteLink.ts` (autolinker, card, deep-link receiver;
bracketed IPv6 hosts and the `/host/port/code` shape included). Lookups are memoised in
`stores/inviteResolveStore.ts`: definitive answers stick for the session, connectivity
failures retry after 30 s so cards heal when central returns. Invite cards ignore the
link-previews privacy toggle (they talk to our own central, not the linked site).

**Invite links are code-only (2026-08-27) ✅** — the Invites modal now copies
`decibell://invite/<CODE>`; no host or port in the link. Central already maps codes to the
community's endpoint (`INVITE_RESOLVE`; communities register every invite and re-register live
ones on reconnect), so the card, the deep-link modal and the browse view's paste box all take
the endpoint from `resolve_invite_code` (memoised in `inviteResolveStore`) and join with it.
The older `<host>:<port>/<code>` and `<host>/<port>/<code>` shapes still parse and still join
without central. `PendingInvite.host/port` became optional; `DeepLinkJoinModal` shows the
resolved server name / description / member count instead of a bare `host:port`, and disables
Accept until the endpoint is known (or the invite is reported invalid). Native
`parse_invite_link` accepts the code-only shape (host "" / port 0) though the renderer no
longer calls it.

**Spam-burst ghost bubbles: typed message rejection + client pacing (2026-08-27) ✅** — reported
as "slowmode off, spam a while → *sending too fast* toast, then my messages render wrong (some
missing, some trailing ~5 behind)". Not slowmode: the per-session anti-spam bucket (was 8 burst /
1.5 per s, shared with edits) *drops* the message and answers with a generic
`MOD_ACTION_RES(action="message")`, which carries no nonce — so the client toasted and left the
optimistic id-0 bubble in place. `mergeMessage` anchors id-0 rows at the tail and inserts every
later real message *above* them, hence the "trailing" ghosts and the "missing" newest message
(it was there, above the ghosts); the ghosts also blocked `trimTail`. Fix, three layers:
(1) **wire** — new `CHANNEL_MSG_REJECTED {channel_id, nonce, reason}` (type 127 / payload 129)
sent by `reject_channel_msg()` at every refusal site (rate limit, permission, slowmode,
attachment cap, persist failure), followed by the legacy `MOD_ACTION_RES` for older clients;
the renderer (`channel_message_rejected` → `removeMessageByNonce` + toast) withdraws exactly
that bubble and skips the legacy toast when it lands within a beat of the typed one (older
servers still toast through the legacy path). (2) **client pacing** —
`features/chat/sendPacing.ts` mirrors the bucket (10 burst, 2.7/s, reset on community auth)
and queues sends per server FIFO, so a burst is *delayed* (the bubble is already on screen),
never dropped — the toast stops appearing for fast typing at all. (3) **echo watchdog** — an
id-0 bubble with no echo after 30 s is withdrawn with a toast, so no lost response can leave a
permanent ghost. Bucket retuned to **10 burst / 3 per s** (CLAUDE.md updated). e2e `[B11]`
now sends nonces and asserts `CHANNEL_MSG_REJECTED` names exactly the dropped ones.

**Pending messages render faded until the server echoes them (2026-08-27) ✅** — Discord's
"sending" look, so the client-side pacing above is legible: an optimistic bubble draws at 50 %
opacity and the echo replaces it at full colour. `Message.pending` / `DmMessage.pending` flag the
optimistic object (the echo is a new object without it; rows stay keyed by id-else-nonce, so the
un-fade is a row replacement, not a transition — keys must equal `String(id)` for the scroll
anchor's `byKey` lookup). Channels already had optimistic bubbles; **DMs didn't** — sends just
waited for central's echo. Now: `DirectMessage.nonce` (field 10) is set per send by the renderer
(`newNonce()`), central's echo carries it for free (it copies the received packet), and its two
error replies (friends-only recipient, persist failure) set it explicitly; the DM store
reconciles by nonce (`addDmMessage` replaces the pending row in place; `removeDmMessageByNonce`
for failures), `useDmEvents` turns a nonce-bearing, id-less self echo into "withdraw the bubble +
toast the reason" instead of rendering central's error text as a message of ours, and
`watchDmEcho` clears a bubble nothing confirms within 30 s. Older centrals still echo the nonce
on success (unknown fields survive the packet copy); their error replies lack it and fall back
to the previous rendering, with the watchdog clearing the bubble.

**P2P voice calls + screen share in DMs (2026-08-28) ✅ — live test pending** — the first
media path not hosted by a community, and the first that is encrypted. Design:
`docs/superpowers/specs/2026-08-28-p2p-dm-calls-design.md`. Four commits (M1–M4): (1) **wire +
central** — `CALL_SIGNAL` (type 128 / payload 130) is an ephemeral relay: central stamps `from`,
applies a per-session token bucket (20 burst / 5 per s; central had none, and a signal storm on
its single io thread + synchronous Postgres would stall every user), runs `check_dm_allowed`
on INVITE only, `send_private`s, and answers `PEER_OFFLINE` / `NOT_ALLOWED` itself from the
recipient's POV; nothing persisted. `LoginResponse.stun_servers` (from `DECIBELL_STUN_SERVERS`)
+ `call_signaling` gate the client's Call button so an old central never rings into the void.
(2) **native transport** — `media_socket.rs` wraps both UDP sockets: `Plain` is byte-identical
for the community relay, `Sealed` is AES-256-GCM per direction with a `[0xE5][u64 ctr][ct][tag]`
envelope (25 B), replay window, the *unchanged* inner datagram (so the audio pipeline,
`VideoReceiver`, PING/RTT, NACK/PLI/FEC all work untouched), peer address learned from the last
authenticated datagram, and peer PINGs reflected re-sealed so the 3 s keepalive/RTT contract
holds peer-to-peer. `call_crypto.rs` (ephemeral X25519 + HKDF via `ring`), `stun.rs` (RFC 5389
Binding, RFC 5769 vectors), `punch.rs` (sealed PINGs at every candidate, first datagram that
opens = live path, 300 ms LAN preference, 10 s → `no_path`). `VoiceEngine::start_p2p`;
`call_prepare` / `call_connect` / `call_end` / `call_watch_stream`; 15 s peer-loss watchdog.
(3) **client** — `callStore` state machine, `callActions` (ringtone loop, 45 s timeout, BUSY,
glare → lower username's INVITE wins), `IncomingCallModal` (window flash IPC), the call **stage**
at the top of the DM (`CallStage`, Direction A of the 2026-08-28 canvas: tiles → focused stream
→ theater → fullscreen, one control dock), "Call · <peer>" in `UserPanel`; `applyVoicePrefs` shared with the channel join; a call and a
voice channel are mutually exclusive (one engine, one mic — renderer leaves/hangs up first,
native refuses otherwise). (4) **in-call screen share** — `STREAM_START/STOP` over central stand
in for the community's stream presence; the whole player stack (`StreamPipManager` /
`StreamViewPanel` / `MiniStreamPlayer`) runs over the sealed sockets, thumbnails are skipped
without a channel. Verified: 97/97 `cargo test --lib` (16 new), tsc, napi build, community e2e
unchanged, central `-fsyntax-only`. **Not yet run:** the two-machine live matrix (LAN host path,
cross-network srflx path, symmetric-NAT failure copy, stream both ways) — see the spec's
Verification section. Limits by design: STUN-only (no relay), IPv4, central can MITM the key
exchange (safety number planned). `MediaSocket::Sealed` is the seam for finally sealing the
community media plane (the tracked HIGH item).

**UDP payload cap unified at 1200 + bitrate guards (2026-08-28) ✅** — `src/common/udp_packet.hpp`
said 1400 while the client had chunked video at 1200 since 0.5.4 (CODE_REVIEW #11); the C++
structs over-allocated and the relay's `sizeof`-based buffers disagreed with the wire. Now
`UDP_MAX_PAYLOAD = 1200` for audio, video and FEC on both sides (structs 1237 / 1245 / 1243,
byte-identical to Rust), `UDP_MAX_DATAGRAM` sizes the relay's receive buffers from the largest
struct, and the audio path can no longer silently truncate: the client's Opus output buffer is
`MAX_PAYLOAD_SIZE - 1` (libopus fits the frame to its buffer, ≈479 kbps instant cap),
`UdpAudioPacket::new_audio/new_stream_audio` return `None` instead of clamping, and voice
bitrates are clamped to **320 kbps** on the community server (create + update; was 512, above
Opus's own 510 where a 20 ms frame reaches 1275 B) and in the client encoders. The wire format is
unchanged (compact header + payload); community servers pick it up on their next deploy.

**Friend-add "Database error." — friends CHECK collation vs C++ byte order (2026-08-31) ✅ —
needs Hetzner rebuild** — field report from live testing: a user with a capitalized username
could not add anyone and nobody could add him; a fresh (also capitalized) account reproduced it.
Root cause: `friends` was created with `CHECK (user1 < user2)`, which Postgres evaluates in the
*database collation* (dictionary order — case-insensitive at the primary level, punctuation
ignored), while `handleFriendAction`/`isBlocked` order the pair with `std::min`/`std::max`
(byte order, all uppercase before all lowercase). Any pair where the two orderings disagree
("Zeki"/"adam" — and pure-lowercase pairs too: "a_b"/"aab") failed the CHECK on INSERT →
"Database error.", symmetrically (u1/u2 are the same whoever initiates) and account-independently
(same-styled username, same violation). Fix: the CHECK is now the named constraint
`friends_pair_byteorder` with `COLLATE "C"` (byte order, matching the C++), plus a one-shot
`DO` migration that swaps the old auto-named `friends_check` on deployed DBs — existing rows
revalidate clean because only byte-ordered pairs ever inserted successfully. Alongside it,
registration now enforces **lowercase-only usernames** (`[a-z0-9._-]`, 3–32; signed-char range
tests reject non-ASCII bytes too): mixed case was already a UX trap with exact-match friend
lookups. Existing mixed-case accounts still log in and are addressable — the gate is
registration-only, which is exactly why the constraint fix is still required. Client folds the
username to lowercase as you type in register mode only (`LoginPage`). Verified: stub g++
compile of the new statements + validation table, renderer tsc clean, migration semantics
checked against locale collation ordering; central is inspection-only locally and the migration
runs on the next Hetzner rebuild — until then the bug persists in production.

**Forced voice move left the client's roster on the old channel (2026-08-31) ✅** — field
report from live testing: after a moderator MOVE, the moved client showed the old channel's
users under the *new* channel too (everyone listed twice), and being moved back into a channel
showed an empty room — not even yourself. Root cause: on a MOVE the server broadcasts
presence for the old channel, then the new channel, then `VOICE_FORCE_NOTIFY` — but the
renderer's presence handler only refreshes `voiceStore.participants` (the connected channel's
roster; what the stage and the connected sidebar row render) when the update matches
`connectedChannelId`, which still pointed at the *old* channel when the destination's presence
arrived. The notify handler then flipped `connectedChannelId` without reconciling, so
`participants` froze on whatever last matched: the old channel's remainder (duplication), or
the emptied channel just left (empty room on a move back). Fix (client-only,
`useServerEvents.ts` moved branch): after `setConnectedChannel`, rebuild `participants` from
the always-correct `channelPresence`/`channelUserStates` caches, rebuild `activeStreams` from
`streamsByUser` for the destination, re-apply saved per-user gains, clear watch state
(watcher entries were dropped server-side), and tear down our own capture if we were
streaming (the server stops a stream on MOVE; the client previously kept a zombie encoder and
a stuck Stop button). Works against all server versions — the server's broadcast order
(presence before notify) is what makes the cache-rebuild safe. Verified: renderer tsc clean;
server-side MOVE flow already covered by `test_voice_moderation` in e2e.py.

**Windows 10 yellow capture border removed for screen shares (2026-08-31) ✅ — needs a
Windows live test** — field report: streaming on Win10 draws a yellow border around the
captured monitor (TeamSpeak has it, Discord doesn't). Root cause: the native Windows capture
is Windows.Graphics.Capture, whose border is mandatory on consumer Win10 —
`SetIsBorderRequired(false)` (capture_wgc.rs) needs IGraphicsCaptureSession3, a Win11/Server
2022 API, so the existing best-effort call silently failed there (the 2026-03-28 design doc
recorded this as accepted degradation). Fix: monitor capture now goes through **DXGI Desktop
Duplication** (`native/src/media/capture_dxgi.rs`, mined from the Tauri client's
capture_dxgi.rs) — no OS border on any Windows version, same `SyncSender<ID3D11Texture2D>`
contract into the encoder thread. Deltas vs. WGC: frames are copied into a 4-slot BGRA ring
(the duplication surface is only valid until ReleaseFrame; re-sent when the desktop is static
so the encoder/GOP keeps running), copies happen only when a paced send is due (mouse-only
duplication updates can fire at polling rate), and the mouse pointer is composited manually —
duplication doesn't include the hardware cursor — via a cursor-sized staging round-trip and
pure blend math in the new cross-platform `cursor_blend.rs` (COLOR src-over, MONOCHROME
AND/XOR, MASKED_COLOR; 11 unit tests run on the Linux box). Fallback to WGC (border on Win10,
none on Win11) whenever duplication can't start: window capture (duplication does whole
outputs only), cross-adapter outputs, rotated displays, HDR desktops without DuplicateOutput1
(BGRA is requested via IDXGIOutput5 so DWM tone-maps; a non-BGRA mode bails), exclusive-
fullscreen access loss — the start handshake reports failure within ~3s so `start_windows`
can degrade. Renderer-encode fallback path hardened too: `electron/main/index.ts` disables
Chromium's WGC capturer features on win32 (`WebRtcAllowWgc{Desktop,Screen,Window}Capturer`)
so getDisplayMedia also uses the borderless legacy capturers. Known remaining: **window**
shares on Win10 keep the border (WGC is the only per-window path we have; Discord ships a
BitBlt/hook capturer for that — future work if it matters). Verified: `cargo test --lib`
110/110 (cursor_blend + clipping tests new), tsc web+node clean; the cfg(windows) half is
compile-verified by the release CI and needs a live Win10 stream test.
*Follow-up (2026-09-01, user decision):* DXGI duplication is now **Win10-only**. Where WGC
can itself go borderless — `capture_wgc::borderless_supported()` asks WinRT metadata whether
`GraphicsCaptureSession.IsBorderRequired` exists (Win11 / Server 2022+; new
`Foundation_Metadata` crate feature) — monitors stay on WGC, which keeps native cursor
compositing and HDR/rotation handling. Routing lives in the `start_windows` match guard;
behavior on Win10 is unchanged (DXGI first, WGC border as last resort).
*Release fallout (2026-09-01):* the first `ev0.7.11` build failed its Windows job — the
blind-written `capture_dxgi.rs` called `IDXGIOutputDuplication::GetDesc(&mut desc)`, but the
windows 0.61 projection returns the desc by value. Two permanent fixes came out of it:
(1) **`.github/workflows/win-native-check.yml`** — `cargo check --all-targets` of the addon on
a Windows runner on every push touching `electron-client/native/**` / the proto, with the
compiler output published as an artifact *and* force-pushed to the orphan branch
`ci/win-native-check-log` (`git fetch origin ci/win-native-check-log && git show
FETCH_HEAD:cargo-check.log`) — run logs/artifacts need an authenticated `gh`, a branch
doesn't; vcpkg binary archives are cached (the FFmpeg port is ~20 min uncached).
(2) **Local API verification for cfg(windows) code:** `cargo fetch --target
x86_64-pc-windows-msvc` in `native/` pulls the `windows` crate sources into
`~/.cargo/registry/src/…/windows-0.61.x/` without needing the target's std, so signatures
(Dxgi/mod.rs, Direct3D11/mod.rs, Foundation/Metadata/mod.rs) can be grepped before pushing.
Every other API the DXGI path uses was verified that way.

**Stream watch stuck on "loading" forever (2026-08-31) ✅ (hardening; root cause = stale
post-move state, fixed above)** — field report: a watcher sat on the spinner indefinitely for
a Win10 friend's stream during the same session as the voice-move testing. Two findings:
(1) most likely trigger: pre-fix, a forced move left `activeStreams` stale, so the old
channel's stream cards stayed clickable from the new channel — and the server drops a
WATCH_STREAM_REQ from outside the stream's channel **silently** (log + return, no response
packet), so the client waits forever. The voice-move reconciliation fix removes the ghost
cards. (2) independent hardening: StreamVideoPlayer's stall watchdog explicitly skipped the
pre-first-frame phase, so if the single mid-stream-join keyframe request (or its answering
IDR) was lost — it's UDP — the player never re-asked and stayed black until the next natural
GOP… or forever. The watchdog now keeps calling `requestKeyframe()` (self-throttled to 1/s)
until the first frame paints.

**DXGI capture went choppy whenever the mouse stood still (2026-09-01) ✅ — needs a Win10
live test** — field report after 0.7.11: a 60 fps game share via the new DXGI path was smooth
while the streamer moved the mouse and choppy the moment the cursor stayed put. Root cause was
the capture loop's pacing: a desktop frame that arrived *before* the next paced send was due
was released without being copied — and Desktop Duplication hands each frame out exactly once,
so a released frame is gone; the next Acquire waits for a *new* present. With the game's
presents out of phase with the send schedule, almost every real frame was discarded and the
timeouts re-sent a stale ring slot. Mouse movement produced extra Acquire returns (mouse-only
updates still carry the current desktop image) that happened to land on due sends, which is why
it looked fine while the mouse moved. Fix (`capture_dxgi.rs`): keep the latest desktop in a
`clean` texture updated on every content frame (`LastPresentTime != 0 || AccumulatedFrames >
0`, or the first frame); mouse-only updates refresh cursor metadata only; each paced send copies
clean → ring slot and composites the cursor at its current position; the Acquire wait is bounded
by the send deadline (≤ 8 ms) instead of a flat 8 ms, so sends no longer slip by up to a frame.
No frame is ever dropped on the floor now and the cursor can't be stale either. Verified:
Windows Native Check green on `772a5a4` (no new warnings), `cargo test --lib` 110/110.

**Mini-player "back to stream" showed "Server · Private" with no channels (2026-09-01) ✅** —
field report: watch a stream, go Home or into a DM, click the floating mini player → the voice
stage came back but the channel sidebar was blank with the Private badge. `expandToStream`
(MiniStreamPlayer) only flipped `activeView` to "voice"; the sidebar renders
`chatStore.activeServerId`, which Home/DM navigation had set to `null`, so the header fell
back to "Server" and the not-a-known-server badge. Fix: re-open the stream's server first
(`openServer(connectedServerId)` — the same path the server bar uses, so the channel selection
stays sane) when it isn't the active one, then show the voice view. Same hole in
`UserProfilePopup.handleJoinAndWatch` (clicking a live thumbnail from a DM's profile popup) —
fixed the same way with the stream's `serverId`. Verified: renderer tsc clean.

**Every capture path now paces to the configured fps — 120/60/30/15 (2026-09-01) ✅** —
audit prompted by the WGC-vs-DXGI performance question: the capture API costs tenths of a
millisecond per frame; what actually taxes a streamer's GPU is *how many* frames reach the
encoder. Findings per path: **WGC (Win11)** forwarded every compositor frame, so a 144 Hz
monitor drove ~144 copies + hardware-encode submissions per second into a 60 fps stream (the
encoder's wall-clock pts collapsed duplicates but the work was done). **Renderer WebCodecs**
(Linux/macOS + fallback) relied solely on getDisplayMedia's `frameRate` constraint, which
Chromium treats as a request. **DXGI (Win10)** was already paced by the 2026-09-01 fix; the
**Linux native** paths were already right (PipeWire interval gate + framerate in the format
pod, wlr-screencopy absolute deadlines, encoder re-encode once per interval when idle). Fixes:
`capture_wgc.rs` holds only the newest pool frame (pool bumped to 3 buffers), copies it into an
own 4-slot texture ring on each deadline — which also removes the latent hazard of a recycled
pool buffer being rewritten while queued for the encoder — re-sends the last slot when the
screen is static so cadence and keyframe requests keep working, and asks Win11 24H2's
`GraphicsCaptureSession.SetMinUpdateInterval` for the target up front (best-effort, like
`SetIsBorderRequired`). `StreamCapture.ts` pumpLoop gates frames on a drift-proof deadline
with half-an-interval tolerance (an exact-rate source isn't halved by accept/drop jitter; a
faster source is thinned to the target) using the VideoFrame timestamp. Verified: Windows
Native Check green on `63aea98` (no new warnings), `cargo test --lib` 110/110, renderer tsc
clean (via deno — see the node/libada note).

**DXGI share still ~32 fps with the cursor still → cursor compositing moved to the GPU
(2026-09-01) ✅ — needs a Win10 live test** — second field report on the Win10 DXGI path
after 0.7.12: ~20 fps became ~32 fps with the cursor still, 60 while it moved. Root cause: every
paced send blended the cursor on the CPU through a staging-texture `Map` — a hard wait for the
GPU to reach our copy. On a GPU-bound game that copy queues behind the game's in-flight
frame(s), so one send cost ≈ one game frame (≈ 30 ms ⇒ ~32 sends/s), and since the D3D11
context is multithread-protected the encoder thread's blits waited out the same stall. Mouse
motion makes DWM composite far more often, which forces finer GPU preemption and lets our copy
through promptly — hence 60 fps only while the mouse moved (and it stacked with the earlier
discard bug: ~20). Fix: `cursor_gpu.rs` — the pointer shape is converted once per shape change
to straight-alpha BGRA (`cursor_blend::cursor_to_bgra`, unit-tested; COLOR exact, the legacy
XOR-style MASKED/MONOCHROME pixels approximated as opaque inverted / mid-grey) and uploaded as
a texture; each send draws it as one alpha-blended quad (SV_VertexID quad, two 3-line HLSL
shaders via `D3DCompile` — new `Win32_Graphics_Direct3D_Fxc` feature; the rasterizer clips
off-frame cursors). The capture loop now contains **no CPU↔GPU synchronisation at all** —
treat that as an invariant for every native capture path. Also per-second `[capture_dxgi] 1s:`
telemetry (content / mouse-only / timeouts / sends / drops / max send µs) so the next report
carries numbers. Verified: Windows Native Check green on `f0bd1f5` (0 errors, no new
warnings), `cargo test --lib` 104/104, all D3D11/Fxc signatures checked against the crate.

**Multi-app audio picker for streams (2026-09-02) ✅ — needs Linux + Win10 live tests** —
"Share audio" was one boolean: Windows scoped stream audio to the picked window's process (or
everything-minus-self for a screen), Linux always everything-minus-self. Now the Go Live dialog
(and a speaker button next to Stop while live — UserPanel, VoicePanel, the own-stream card in a
DM call) lists the applications with an audio output under a **Selected apps / All except / All
apps** rule; one ticked set, persisted by program name (`stream_audio_mode` /
`stream_audio_apps`), applied to the running capture at once via `set_stream_audio_filter`. The
picker owns the rule for window and screen sources alike (the picked window's app is flagged
"this window"); default is All apps. Native: cfg-free `media/stream_audio_filter.rs` (mode +
identities, `plan_clients` for WASAPI's one-client-one-tree constraint — a blacklist becomes a
dynamic include-set and a blacklisted child suppresses its allowed parent rather than leak),
`AudioStreamEngine` owns a `StreamAudioCapture` handle with `set_filter`. Linux: the PipeWire tap
is filter-aware (link allowed nodes, `pw-link -d` the rest, 2 s poll + wake on change) and
**fixes a real leak**: Decibell's own PipeWire-ALSA node carries its PID only on the Client
object, so the old "PID-less = not us" rule tapped the voice chat into the stream. Windows:
`ProcessLoopbackMixer` — one include-tree client per allowed app root or the old exclude-self
client, summed by a wall-clock-paced `Mixer` (20 ms prime, 200 ms trim), session snapshots from
`IAudioSessionManager2` (+ Toolhelp parent map; `IsSystemSoundsSession` returns S_FALSE for
"no") every 2 s, dead clients reaped and re-planned. No wire change. Design:
`docs/superpowers/specs/2026-09-02-multi-app-audio-picker-design.md`. Verified: `cargo test
--lib` 131/131 (was 104), tsc 0 errors, Windows Native Check green on `a29293e`; the live matrix
in the spec is pending.

**End-to-end encrypted DMs (2026-09-03) ✅ — needs the Hetzner central rebuild + a two-account
live test** — every DM sat in central's Postgres in plaintext (`dm_messages.content`), readable
by whoever runs or breaches a central. First step of the encryption programme: DM *messages*.
Design: `docs/superpowers/specs/2026-09-03-e2ee-dms-design.md`. Protocol: one long-term identity
per user (X25519 agreement + Ed25519 signing, self-signed bundle, central-assigned monotonic
`key_id`); conversation root = static–static X25519; every message gets its own key from a
32-byte salt via HKDF-SHA256 (both usernames + both key ids in the info) → AES-256-GCM with the
sender's name in the AAD (reflection-proof); versioned envelope `0x01 ‖ sender_kid ‖
recipient_kid ‖ salt ‖ ct ‖ tag` in a new `DirectMessage.envelope` (+ `reply_to_envelope`,
`DmHistoryMessage`, `DmConversationPreview.last_message_envelope`, `DmEditReq` /
`DmMessageEdited.envelope`, `DmMessageEdited.sender`). Private keys are backed up on central
wrapped by Argon2id(64 MiB, t=3) + AES-256-GCM under a user passphrase (≥10 chars; central
sees the login password so it can't be the KEK) — a second device unlocks with the passphrase
once; the local store `<userData>/e2ee/<hex user>.json` is AES-GCM at rest under a
safeStorage-wrapped random key (`electron/main/e2eeLocalKey.ts`), falling back to the config.rs
derivation. **Policy: seal whenever both sides have keys, plaintext otherwise, visibly; a pinned
peer is never downgraded** (central saying "no keys" for a pinned peer → refuse to send). Old
clients see a fixed placeholder in `content`; old centrals are detected by
`LoginResponse.e2ee_keys` and the feature hides itself. **Not forward secret** (static keys —
deliberate: there is no local message store, history is re-fetched from central every login; a
ratchet would make history device-bound; the envelope is versioned for it). Central:
`user_e2ee_keys` / `user_e2ee_backup` tables + `dm_messages.envelope BYTEA`,
`E2EE_PUBLISH_KEYS_REQ/RES` (129/130, bundle and/or backup, broadcasts `E2EE_KEYS_CHANGED` 135
like AVATAR_CHANGED), `E2EE_FETCH_KEYS_REQ/RES` (131/132, key_id 0 = current, any authed
user), `E2EE_FETCH_BACKUP_REQ/RES` (133/134, own only), all behind a 20/5 per-session bucket;
DM/edit handlers cap the envelope at 64 KiB + 64 and store it opaquely. Native: `e2ee/`
(identity, envelope, backup, keystore, session) — all crypto stays in Rust, the renderer only
sees decrypted events with `encrypted` + `decryptError` (`locked | no_key | peer_key | bad`);
DM packets are routed through an **ordered crypto worker** (`session::enqueue`) because opening
may need a peer's historical key = a round-trip whose reply lands on the same router loop;
`seal_outbound` in `send_private_message` / `edit_dm_message`; TOFU pin per peer with an
`e2ee_peer_changed` event on identity change; a remote rotation of our own keys (another
device reset) drops this device to *locked*. Commands: `e2ee_get_status / setup / unlock /
change_passphrase / reset / peer_info`. Renderer: `e2eeStore`, `useE2eeEvents` (first "locked"
report opens the unlock prompt once; becoming ready invalidates all loaded DM history and
re-pulls), `PassphraseModal`, Privacy-tab `E2eeSection` (fingerprint), DM header
`EncryptionBadge`, `DmEncryptionBanner` (locked / keys changed / set-up nudge / peer has no
keys), bubble lock glyph + muted placeholder rows (not editable), profile-popup safety number
(12×4 base32 groups, identical on both sides). Verified: `cargo test --lib` 146/146 (was 131;
envelope round-trip/tamper/reflection/wrong-pair, bundle signature, backup wrong-passphrase +
hostile params, keystore at-rest + username binding), tsc 0 errors, community build + e2e
235/235 against the new proto (additive; no community change), central syntax-checked against
a mocked pqxx. Non-goals now: forward secrecy, per-device keys, verified-contact state,
channel encryption, signed call keys (the identity keys make that the next small step).

**Authenticated P2P calls (2026-09-03) ✅ — same central rebuild + live test as E2EE DMs** —
DM calls sealed every datagram (AES-256-GCM, ephemeral X25519 per call) but the ephemeral
keys travelled through central unauthenticated, so a malicious central could swap them and
sit in the middle (the calls spec's documented gap). Now each side signs its ephemeral key
with its E2EE identity — `Ed25519("decibell-call-auth-v1" ‖ call_id ‖ from ‖ to ‖ pub_key)`,
`CallSignal.pub_key_sig` + `key_id` (INVITE / ACCEPT; central relays, caps at 64 B) — and
`call_connect` verifies against the peer's *current* pinned identity before any media key is
derived (`e2ee/call_auth.rs`, `session::sign_own_call_key` / `verify_peer_call_key`). Fail
closed: a peer with keys must have signed with its current identity or the call is refused
("Couldn't verify X"); a peer without keys yields a sealed-but-unverified call; a signature
from a peer the server claims has no keys is refused (the server is lying about one of the
two). Verification needs only the peer's public key, so a not-yet-set-up device still
verifies. Renderer ferries `pubKeySig` / `keyId` opaquely from `call_signal` into
`call_connect`; `call_connected.verified` shows a **Verified / Not verified** pill on the call
stage; the profile popup's safety number now covers calls and DMs (one identity). Design:
`docs/superpowers/specs/2026-09-03-authenticated-p2p-calls-design.md`. Verified: `cargo test
--lib` (call_auth round trip + every bound field / foreign identity / malformed), tsc 0
errors, community build + e2e on the new proto, central mocked-pqxx syntax check. Community
voice channels and stream watching remain the plaintext relay (tracked HIGH item).

**MLS-encrypted voice channels and streams (2026-09-03) ✅ — needs a community-server release +
a multi-client live test** — the relay path was the last plaintext media plane: Opus and encoded
video in the clear through the community server, a JWT-tail sender id never re-verified. Now every
voice channel session is an **MLS group (RFC 9420, OpenMLS 0.9)** keyed by the E2EE identity
(Ed25519 signing key = MLS credential, username = BasicCredential identity, ciphersuite
X25519/AES-128-GCM/Ed25519). Members join by **external commit** against a GroupInfo the server
holds; leaves are commits by an elected member; every epoch exports one secret from which each
member derives every sender's keys locally (`media/frame_crypto.rs`: HKDF per member × kind).
Media: new sealed packet types **AUDIO_SEALED 7 / STREAM_AUDIO_SEALED 8 / VIDEO_SEALED 9** keep the
plain twins' headers (the relay needs them) and seal only the payload — audio `[epoch][counter]ct`
(28 B), video **one sealed frame** `[epoch][stream_salt]ct` (24 B/frame) chunked as before so FEC and
NACK work on ciphertext untouched; AADs bind type, username (the relay's rewrite), sequence /
frame id / keyframe flag / codec; previous-epoch keys stay usable 2 s. **Strict policy (user
decision):** a device without E2EE keys can't join voice (renderer opens the set-up/unlock
prompt; native refuses), receivers ignore plaintext media in a channel session, no fallback.
**Thumbnails stay plaintext** (user decision: non-members should see what a stream is about).
Identity binding: after every merge each leaf's (identity, signature key) is checked against
central's directory; an unverified leaf **quarantines** the epoch (nothing sealed, badge +
toast) and the elected member removes it — a server can put anyone on the roster but not a fake
identity in the group. Server = the MLS delivery service (`MLS_GROUP_INFO_REQ/RES`,
`MLS_GROUP_CREATE_REQ`, `MLS_COMMIT_REQ/RES` with the `epoch == current + 1` rule,
`MLS_COMMIT_BROADCAST` to participants except the committer; group dropped when the channel
empties; 20/5 bucket; 256 KiB blob cap; relay treats 7/8/9 like 0/6/1) — it never holds a key.
Native: `e2ee/group.rs` driver task per session (create / external join / process / merge /
resync-by-rejoin / ghost removal after 3 s by the lowest verified name / quarantine),
`voice_mls` in AppState, ring threaded through `run_audio_pipeline`, `VideoSender`
(frame-level seal), `run_video_recv_thread`, `AudioStreamEngine`; P2P calls keep the plain
inner types inside the sealed socket. Renderer: `voice_e2ee_state` → header badge (Encrypted /
Securing… / Unverified member / Encryption failed), join gate. Known: one device per user per
channel (MLS requires unique leaf keys; the relay never distinguished devices anyway); server-side
leaf verification and the UDP endpoint-hijack item stay open. Design:
`docs/superpowers/specs/2026-09-03-mls-voice-channel-encryption-design.md`. Verified: `cargo test
--lib` (frame_crypto round trips / tamper / epoch grace / quarantine, an in-process 3-member MLS
session through a simulated DS incl. removal + rejoin + stale-info rejection), community e2e
(create/info/epoch rule/broadcast-except-committer/participant gating/sealed relay/drop on
empty), tsc 0, community build.

**Encrypted text channels — per-channel opt-in (2026-09-04) ✅ — needs a community-server
release + a live test** — the last plaintext content on a community server. **User decisions:**
a per-channel toggle, **default off** (people who never open settings must keep server-side
search when it ships — note that no message search exists today; the FTS table was dropped as
unmaintained), strict inside an encrypted channel, thumbnails/attachments out of scope for now.
Protocol: **channel epoch keys escrowed to members** (not MLS — members are offline, history must
stay readable to late joiners and on every device, one identity per user would pin a channel to
one device): a random 32-byte key per epoch, sealed by a member to every entitled member's static
identity with the DM envelope (content tag `0x02`, `e2ee/channel_envelope.rs`), stored on the
server per recipient (`channel_key_epochs` / `channel_key_blobs`), fetched on login. Messages are
sealed like DMs with the epoch in the header (`0x02 ‖ epoch ‖ salt ‖ ct`, channel + server-stamped
sender bound into key and AAD). The first sender creates epoch 1; **kick / ban / leave rotate**
(`channel_keys_on_removal` deletes the leaver's blobs and names a filler with `rotate`); a member
(re)joining lacking blobs triggers `CHANNEL_KEYS_CHANGED` naming the lowest online holder as
**filler**; `CHANNEL_KEYS_RES` also lists `needs` so any holder fills on open. The server enforces
the wire format both ways (`channels.encrypted`, `messages.envelope`: plaintext refused in
encrypted channels, envelopes refused in plaintext ones, attachments refused in encrypted ones),
carries `envelope` on CHANNEL_MSG / history / edit / reply preview, audits the toggle, and gates
`CHANNEL_KEYS_*` on VIEW for both sealer and recipient (non-viewer blobs dropped; 500 blobs / 2 KiB
caps; existing blobs never overwritten). Switching off leaves sealed history sealed. Native:
`e2ee/channel_keys.rs` (keyring per (server, channel), ensure / refresh / create_epoch / fill_gaps
/ rotate, fills paced to central's key bucket, an ordered channel-crypto worker like the DM one),
`seal_message` in `send_channel_message` / `edit_channel_message` (renderer passes the channel's
flag). Renderer: lock in the sidebar row, the toggle in channel settings with the search /
attachment caveat, attach hidden in encrypted channels, placeholder rows via the existing
`decryptError` rendering. Design: `docs/superpowers/specs/2026-09-04-encrypted-text-channels-design.md`.
Verified: `cargo test --lib` (channel envelope + key-blob round trips / bindings), community e2e
(toggle + audit + permission, enforcement both ways, envelope through broadcast / history / edit /
reply, escrow create / fetch / needs / gap fill / stale epoch / viewer gating / CHANGED on join and
removal / no-overwrite, toggle off), tsc 0, community build. Open: attachments in encrypted
channels (client-side file encryption + thumbnails), rotation on permission-overwrite changes, a
batch key-fetch endpoint on central, client-side search once the local store exists.

**Attachments in encrypted channels (2026-09-04) ✅ — needs the same community-server release
+ live test** — closes the "attachments refused in encrypted channels" gap. The server never
processed attachment content (bytes in, Ranges out, client-made thumbnails), so this is a
client-side transform around the existing upload / fetch paths plus one flag. **Renderer
seals on upload** (`features/chat/attachmentCrypto.ts`, WebCrypto): random 32-byte key per
file, content in 64 KiB AES-256-GCM chunks (nonce `u64le(i) ‖ 00 00 00 01`, aad
`"decibell-att-v1" ‖ 0x00 ‖ u64le(i)`, ciphertext = concatenated sealed chunks so a plaintext
Range maps to one upstream Range of whole chunks), thumbnails sealed whole under
`u64le(sizePx) ‖ 00 00 00 02`. `uploadAttachment.ts` sends `/attachments/init` stand-ins
(filename "encrypted", octet-stream, ciphertext size) plus `encrypted: true` and the **kind**
(retention is per kind); the server zeroes width / height / duration / placeholder regardless,
stores `attachments.encrypted`, and `bind_attachments` binds only uploads whose flag matches
the channel's (a plain upload never lands in an encrypted channel, a sealed one never in a
plaintext channel — dropped like any other ineligible upload, not rejected). Wire:
`Attachment.encrypted` (17), `EncryptedAttachmentMeta` / `EncryptedMessageBody`. **Metadata +
key ride the envelope**: content tag 0x03 = prost `EncryptedMessageBody { text, attachments }`
(0x01 stays the text-only body; `channel_envelope::seal_tagged` / `open_tagged`), native
emits `encryptedAttachments` on message / history / edit events, `send_channel_message` /
`edit_channel_message` take `encryptedAttachments` (an edit re-seals the same metadata).
`useChatEvents` merges the real metadata into the `Attachment` (`encrypted`, `keyB64`,
`chunkBytes`) and registers the keys with main (`decibell:attachments:registerKeys`, session
only). **Main decrypts** (`electron/main/attachmentFetch.ts` + `attachmentCrypto.ts`) in all
three byte paths — `decibell-attachment://` (images, thumbnails), the loopback media server
(`<video>` / `<audio>`: covering chunks fetched, opened, trimmed; real 200 / 206 +
Content-Range) and `netFetch` (save-as); plain attachments proxy as before. Not hidden from
the server: count, kind, ciphertext size, thumbnail sizes — accepted. Verified: `cargo test
--lib` (157, incl. tagged bodies), a node cross-check that renderer sealing and main opening
agree (0 … 3 chunks, ranges incl. suffix / cross-chunk, tamper, wrong index, thumbnails),
tsc web + node 0, napi build, community build + e2e (sealed init stand-ins + kind check,
binding rule both ways, `encrypted` on broadcast + history). Open: live test (image, video
seek, save-as), and the community-server release the whole channel-encryption feature waits on.

**Central: E2EE key publish failed with "Storage error." (2026-09-03) ✅** — first production
run of the key directory. Postgres refused `publishE2eeKeys`: `inconsistent types deduced for
parameter $1 — text versus character varying`. The insert picked the next `key_id` with
`INSERT … SELECT $1, MAX+1, $2… FROM user_e2ee_keys WHERE username = $1`. Postgres types an
untyped parameter from its uses, and the two uses of `$1` disagree: as an inserted value it takes
the column's varchar, while `username = $1` resolves through the text equality operator and takes
text — a `VALUES` + scalar-subquery rewrite failed identically. Fix: pass the username twice, one
parameter per context (`VALUES ($1, (SELECT … WHERE username = $6), …)`). Rule for central: never
reuse one parameter both as an inserted value and in a comparison. Central has no local build
here — verified by inspection; needs the Hetzner rebuild + restart.

**Client: encryption cues (2026-09-03) ✅** — the per-message lock glyph next to every sealed
message (channels and DMs) is gone: a channel is either encrypted or it isn't, so it's said once.
Text channels with encryption on show an **E2EE** pill in the header (`ChannelEncryptionBadge`,
same style as the DM header's Encrypted pill); the sidebar lock and the unreadable-message
placeholders (which still carry the glyph) are unchanged. tsc 0.

**Client: AGC default, menus at the window edge, a profile screen, smaller inline media (2026-09-05) ✅**
— four fixes from a usage pass. **(1) AGC defaults on.** `agc_enabled` is `default_true` in
`AppSettings` and `true` in the renderer store; a config that already carries the field keeps
its value, so existing installs stay as the user left them until they toggle it. Found on the
way: `AppSettings` *derived* `Default`, which zeroes every field, and `config::load()` hands
that out when there is no file yet while the login path writes it to disk — a fresh install
therefore started with crash reporting, link previews and AV1/H.265 all *off* and the theme
`""` (the renderer whitelist happened to rescue the theme). `Default` now goes through serde
(`from_str("{}")`), unit-tested. **(2) Context menus clipped at the bottom/right edge.** Each
menu clamped against a hard-coded "approximate" size: the voice user menu with 160 px (it is
~300 with the moderation section), the channel-list menu with 170, the image menu with 84, and
the device menu floated a fixed 300 px above its button. New `hooks/useMenuPosition.ts`:
measures the rendered menu (layout effect, so the corrected position is what the first frame
paints; ResizeObserver + window resize re-clamp), prefers flipping to the other side of the
anchor (a right-click near the bottom opens upward, like every desktop menu), slides along the
edge only when neither side fits, and caps `maxHeight` to the viewport. All four menus use it;
the popovers that already measured (`EmojiInfoPopover`, `StreamAudioPopover`, the profile
popup) are unchanged. **(3) A full profile screen.** The safety number left the anchored popup,
where it took half the card. Clicking the popup's avatar (hover scrim "View profile" — Discord's
idiom) opens `features/profile/UserProfileModal`: banner, 96 px avatar, display name +
username, status, Message / friend actions (add, accept / decline, cancel request, unblock,
remove), two tabs. *Profile*: About (username, nickname, status, friendship, joined date), roles
on the server it was opened from (`RoleChips`, now shared with the popup). *Privacy*: the pair's
encryption state (On / Off / Locked / not set up / unsupported, each with its next step), the
safety number as a 4-column grid with Copy, the peer's fingerprint, pin state, and the
key-change warning with its date; our own profile shows our fingerprint and links to Settings →
Privacy. `openUserProfile` rides `activeModal === "user-profile"` and closes the popup;
`E2eeProfileSection` is gone. **(4) Inline media too large.** The sqrt curve alone put a
maximised 1080p window at ~790 × 530: a GIF filled a third of the screen, and an attachment
whose largest thumbnail (≤1280) was smaller than that box was upscaled past its own pixels.
`attachmentSizing.ts` caps the curve at Discord's 550 × 350 (`IMAGE_MAX_W/H`; a 16:9 frame
lands at 550 × 309, a square at 350 × 350; panels narrower than ~930 px still shrink along the
curve), the pre-measure fallbacks match, and `LinkEmbeds`' reserved-box image renders at
`max-*` rather than `h-full w-full`, so a page that overstates its `og:image` dimensions gets a
centred image, never a blown-up one. Follow-up the same day: **GIFs get a tighter box** (440 × 280,
`GIF_MAX_W/H`, 80% of the still-image ceiling) — uploaded GIFs by mime, picker sends by a new
`gif` flag the unfurler sets on direct-image previews (content type or `GIF` magic; mirrored in
main / preload / renderer types). Verified: tsc web 0, `cargo test --lib`, napi build. Open:
a live pass on the profile screen against a peer with keys, and on the Privacy tab's locked /
not-set-up states.

**Client: Go Live dialog no longer pushes its button off-screen (2026-09-29) ✅** — friends
reported the Go Live button falling out of the window, worst since the stream-audio app picker
landed. `CaptureSourcePicker` was a fixed 560 px centred box with no height cap or inner
scroll: on Windows the source grid (≤260) + settings + the app list (≤200) came to ~1100 px,
so on anything shorter the footer (and, since the box was centred, the top) left the window —
reproduced in an isolated render at 1366 × 690 (button at y 782–822) and on a 1280 × 720 Linux
window with the app picker. The dialog now has a pinned header ("Go live" + close) and
footer (Cancel / Go Live, the start error, and the "pick a source" hint), with only the body
scrolling (`max-h-full` inside a `p-6` overlay). On Windows at `lg`+ it goes side by side
(≤1040 × 900): sources on the left, settings in a 480 px column on the right, each scrolling on
its own; the grid pins `auto-rows-max`, because its cards are `overflow-hidden` and in a
fixed-height grid their rows would otherwise shrink to fit and clip the titles. Settings are
grouped: *Video* (resolution, frame rate, quality, codec, Show cursor) and *Audio*, where the
Share-audio switch now heads the bitrate + "Audio from" controls it gates. The macOS / Linux
"system dialog will appear" block is a compact one-line notice. Escape closes the dialog. Go Live logic
untouched. Verified: tsc web 0; screenshots of the real component + stylesheet (stubbed
bridge) at 1920 × 1040, 1366 × 690, 900 × 620, 800 × 600 (Windows) and 1280 × 720 / 800 × 600
(Linux), dark and light, with Go Live on-screen in all of them. Open: a live Windows pass with
real capture sources.

**Client: smoothness pass — re-renders, typing latency, paint, main-thread stalls (2026-10-02) ✅** —
six parallel read-only audits of the client (stores/subscriptions, chat surface, composer + pickers,
voice/stream/call UI, CSS/paint + sidebars, main process/IPC/native events), every finding re-read
against source before fixing. What was jank and what changed:
*Chat.* (1) Every channel's live messages were appended forever while unseen (the community pushes
all viewable channels), so returning to a busy channel mounted the whole backlog in one commit before
the list trimmed it. `addMessage` now drops live messages for never-loaded channels and caps every
slice no list is trimming at 150 (`setDisplayedChannelKey` marks the mounted one); a channel left
scrolled up goes windowed instead (anchor kept, jump-to-present pill), never with a send in flight
(the echo watchdog would withdraw a delivered message). (2) `ChatPanel` subscribed to whole maps
(`messagesByChannel`, `historyLoading`, `hasMoreAfter`, `channelsByServer`) and held the draft in
state, so any message anywhere and every keystroke re-rendered the panel and the list pass; it now
selects per channel, and the preview + send button subscribe to `draftsStore` themselves. Same for
`DmChatPanel`, whose row adapter also rebuilt every bubble object per change (now a per-message
WeakMap), and whose composer was disabled during a send (keystrokes lost) — it now clears up front
and chains sends in order. (3) Reply rows got a fresh `.map` array per render (memo defeated);
`toLocaleTimeString` with options built an ICU formatter per row (~40 µs × 2); `useDisplayName` ran a
roster `find` per row per store write (now a WeakMap index, also used by `usePermission` and the
timeout check); own-message echoes re-ran the fade-in on remount (blink); media boxes re-sized on
composer growth (the view size is now panel − header − resting composer); link-preview images decoded
async (empty frame per mount; small known ones decode sync); the persistent video committed React
state every scroll frame (now on settle); the image viewer dropped its fit cap at the first zoom step
(a 4K photo jumped to natural size — zoom is now × fit); the inline editor forced a layout per key
(`field-sizing: content`). *Composer.* `RichInput` inserted a caret sentinel on every input event and
left an empty text node behind each time, so each keystroke got slower than the last (~25 ms/key ~500
keys in, measured); tokenizing is now gated on a cheap emoji/shortcode pre-check. The live preview
renders from `useDeferredValue` and parses uncached; emoji grids are memoised (hover re-rendered up to
~1,900 cells). *Chrome.* `dropPulse` animated `box-shadow` (main-thread repaint per frame) on the
"Update ready" dot — on screen for days — and on every channel row + composer during a file drag; it
is now `.drop-pulse`, a static glow on `::after` whose opacity breathes (composited). Modal cards
transition only opacity/transform; the root no longer fades the palette swap; two full-viewport
`backdrop-blur` scrims are plain; `MembersList` / `ConversationSidebar` rows are memoised with narrow
selectors; `VoiceRow` subscribes to its own presence; the window-title hook no longer re-renders
`MainLayout`; the Sentry tag no longer re-serialises the scope to main on every chatStore write;
avatar versions are batched and avatars pre-decode; no-op store updates return `state` (zustand
notifies on `{}`). *Streams / voice / main.* The native→JS stream queue leaked every dropped frame
(napi-rs 2 boxes the value before `napi_call_threadsafe_function` and never frees it on QueueFull —
`send_stream_frame` now reserves a slot first) and dropped frames left the decoder smearing until the
next IDR (a `discontinuity` flag now makes the player gate + request a keyframe). Frames nobody displays
— own self-preview, watched-but-hidden streams — no longer cross native → main → IPC (`set_stream_frame_sink`,
mirrored from the preload's subscriptions). After "Back" the persistent player stopped painting into a
hidden full-res canvas. Renderer-encoded frames are sealed + packetised + sent on a `decibell-video-send`
thread instead of the Electron main thread (and the Windows WebCodecs fallback now actually sends video —
`send_video_frame` was still a Windows no-op). `probe_native_encoders` runs off the main thread;
`voice_input_level` is only emitted while Settings → Audio is open; the Go Live picker polls every 5 s with
JPEG thumbnails for the active tab and cached icons; the titlebar no longer round-trips `isMaximized()` per
resize step; encrypted-attachment ranges are capped at 4 MiB (an open-ended `bytes=0-` decrypted the whole
file on main); per-request main-process logs are dev-only; speaking rings no longer animate `box-shadow`;
VoicePanel / CallStage / mini-player re-render only what changed (ping, thumbnails, 1 s call tick,
resize drag). Verified: tsc web 0 / node 0, `napi build`, `cargo test --lib` 160 (one new), vite build.
Windows-only Rust (encoder-thread sink check, async probe) is mechanical — Windows Native Check before
the next `ev*` tag. Open (live): two watched streams + Back; native self-preview resume (Linux NVENC,
Windows); a stalled renderer recovering via `discontinuity`; renderer-encode shedding; the Windows picker;
the Windows WebCodecs fallback stream; encrypted-channel video open + seek with the 4 MiB window; Audio-tab
meter in voice; a busy background channel → switch back (cap + windowed return). Not done (noted): save-as
still moves the file through IPC twice (stream it in main); CL1 (voice presence keyed by bare channelId)
still open.

**Client: Windows streaming on AMD — encoder, video processor, capture, fallback (2026-10-02) ✅** —
three read-only reviews (FFmpeg/AMF semantics against the pinned FFmpeg **n8.0.1** source — the vcpkg
pin e5a1490e builds it; D3D11 capture/convert; lifecycle/fallback), every claim re-checked before
fixing. The native path had only ever run on NVENC. *Black tiles on AMD (definite).* (1) A keyframe
request set `pict_type = I`, and amfenc's `forced_idr` defaults to 0, so AMF emitted a plain I /
intra-only frame that isn't flagged key — watchers (and the self-preview) waited for the 4 s GOP. NVENC
has the same default (`forced-idr`, with a hyphen — the Linux NVENC path set `forced_idr`, silently
ignored); both now force IDR. (2) AMF doesn't repeat SPS/PPS (H.264) or VPS/SPS/PPS (HEVC) on its own
IDRs by default and `GLOBAL_HEADER` is ignored by amfenc, while the Windows path never ships extradata —
a late joiner got an IDR it couldn't decode. Now `header_spacing` = GOP (h264), `header_insertion_mode`
idr / frame (hevc / av1), plus a codec-independent safety net: an H.264/HEVC keyframe without an SPS gets
the Annex B extradata spliced in (after a leading AUD). AMF also runs with `latency` on and RC frame
skipping off (a skipped frame could starve amfenc's surface counter into an endless wait). *Video
processor (best lead for the reported crash).* `ID3D11VideoProcessor` was created with the ENCODE size as
its input size while capture hands native-size textures, and the stream destination rect was never set —
documented as "no data is written" (NVIDIA ignores it); Chromium notes a mismatched content desc can crash
drivers. It is now built per input size (rebuilt on change), with explicit source / aspect-fit dest /
target rects (letterboxed, no more stretching), auto-processing off, RGB-black bars. *Other native fixes.*
The device is created on the encoder vendor's adapter (hybrid laptops opened AMF/NVENC on the other GPU);
QSV left the Windows probe (the encoder can't open it, so Intel-primary machines failed every Go Live);
the probe runs once per process and off the AppState lock; amfrt64.dll is pinned for the process (each
open/close unloaded it — the classic `<Unloaded_amfrt64.dll>` fault); a failed `av_hwdevice_ctx_init`
double-Released the device and early returns leaked the hw refs; transient `EAGAIN` / pool exhaustion
drops a frame instead of killing the stream (the old EAGAIN test compared against a negative errno and
never matched); the encoder thread reports failure before anything that can hang and never drains; stop
signals the encoder before stopping capture and detaches a wedged thread after 3 s. *Capture.* WGC start
is now a handshake (closed / minimized window, bad monitor → start error → renderer fallback, not an
announced dead stream) with a first-frame watchdog and no busy-spin; it follows content-size changes
(Recreate + ring rebuild) and ends on item close; DXGI duplication survives ACCESS_LOST (UAC, lock screen,
mode change, exclusive fullscreen — recovers for up to 10 s re-sending the last frame); `screen:N` resolves
through EnumDisplayDevices like WebRTC; thumbnails downscale on the GPU and read back asynchronously (the
blocking Map every 3 s stalled the pipeline); masked-colour cursors no longer draw a white box; cursor
compositing and the blit run under `ID3D11Multithread` Enter/Leave. *Fallback + watchers.* WebCodecs
H.264 now emits Annex B (it defaulted to length-prefixed with the parameter sets only in a description
that `send_video_frame` never forwards for H.264); the fallback announces the codec it really encodes
(HEVC/AV1 → H.264, enforcement dropped if it no longer matches); an encoder error no longer turns `stop()`
into a no-op (leaked capture); `stop()` always tears down the native VideoEngine (voice kick → "Already
sharing screen" forever); session-state errors don't trip the sticky native-failed flag; a dead pump ends
the stream; the picker can't resurrect a session that ended mid-start; a start whose announce failed
drops its engines; main stops an orphaned share after a renderer reload/crash (native remembers the
channel); a vanished picked source is refused instead of sharing the primary monitor. Watchers rebuild a
WebCodecs decoder closed by an error (it was dead until remount) and drop to software when a hardware
decoder never paints. The encoder caps cache is versioned and refreshed in the background. Verified: tsc
web 0 / node 0, `cargo test --lib` 184, napi build, Windows Native Check, and a Linux-side type-check of
the Windows modules against windows 0.61.3 + ffmpeg-next 8. Open (live, AMD box): join a stream mid-way
(immediate picture), source size ≠ preset (letterbox, no crash), window resize, UAC / fullscreen game on
Win10, hybrid laptop, the new AMF options accepted on pre-RDNA cards, TDR mid-stream. Open (cross-platform):
the Linux native H.264 path sends AVCC with no description to remote watchers — check that a Linux NVENC
H.264 stream decodes on another machine. Remaining: AV1 64×16 padding on RDNA3 (crop side data dropped).

**Client: voice view redesign — tiled stage, floating dock, "+N", multi-watch (2026-10-04) ✅** —
Design: the "Voice View Redesign" canvas + `docs/superpowers/specs/2026-10-04-voice-view-redesign.md`.
`VoicePanel` is now a stage of 16:9 tiles (same language as the DM CallStage): each participant is a
tile washed with their avatar colour (`--tile-tint`, per theme), a theme name chip with the status
glyphs (self mute / deafen, a filled red badge for a moderator's mute, the accent speaker for "muted by
you"), a "you" tag, a LIVE pill while streaming, and an instant success outline + soft halo while
speaking. The grid best-fits the space (`stage/stageLayout.ts`: every column count tried, the row width
capped to the chosen count); below a 200 px tile floor the tail collapses into a "+N more" tile (stacked
avatars, green while someone in it talks) whose popover lists the rest. You and streamers are always
shown; a hidden user who talks for 0.6 s swaps into the slot of the visible, unpinned user quiet longest,
in place, at most once per 3 s (`useVisibleParticipants` — speaking is read by a store subscription, so a
VAD flip only re-renders on an actual swap). Streams no longer hide the people: live streams get large
tiles on top, people a single compact row below (same "+N"). Watching several streams: hovering an
unwatched stream offers Watch (nothing else playing) or **Switch** / **Watch too**; a watched tile offers
Focus / Stop; a click on the tile runs its first action (`stage/streamActions.ts`). The focused view
(`StreamViewPanel`) is the video with auto-hiding scrim overlays (LIVE / owner / quality, stats, back to
grid, fullscreen, stop, volume), double-click for fullscreen (a single click no longer drops you back to
the grid), and a filmstrip of the other streams (watched ones keep playing; a click focuses without
stopping anything) + people; the header becomes a "Lounge › mira's screen" breadcrumb; the old 160 px
side column is gone. A floating dock (mic, deafen, share / stop + stream-audio apps, leave) replaces the
full-width button bar and now plays the same sounds as UserPanel. Fullscreen draws the same dock on a
scrim. Alone in the channel: your tile large + an invite (MANAGE_INVITES, active server) / share prompt.
*Player.* The grid's tile for the stream the persistent player holds now reparents that player's host
(like the full view / mini player) instead of showing a poster, so StreamPipManager's 20 s "idle on the
grid" drop is gone — it only ever swapped the tile to a second, cold decoder. *Tokens.* `--color-on-error`
(near-black under `console`, whose coral fails white at 2.76:1 — mirrors `on-accent`), `--color-live`
(one fixed red, #d1362e, for LIVE pills over video), `--tile-tint`. Verified: tsc web 0; every state
(alone, 2, 5, 23 people + open list, streams, three streams + hover, unsupported codec, focused) rendered
in all five themes through a throwaway harness (real components, seeded stores, mocked player, Electron
offscreen capture); the swap timing (a 0.3 s blip ignored, a swap at 0.6 s, the second held by the
cooldown then taken) checked the same way. Open (live): a real channel with two watched streams (also the
R9 frame-dispatch test), the grid-tile ↔ full view ↔ mini player handoff of the persistent player, and
the swap with real VAD.

**Client + community: 960 px stream thumbnails, 15 s cadence, 256 KB cap (2026-10-04) ✅** —
The redesigned voice view shows stream tiles up to ~960 px wide, but thumbnails were 320 px (JPEG 70,
every 3 s), so tiles upscaled them 3–5× on HiDPI. All three generators now make **960 px** (longest
edge) JPEGs at quality 80, stepping down to 65 / 50 until they fit **256 KB**, at **first frame,
+2 s (the first is often black while capture warms up), then every 15 s**. Constants:
`features/voice/thumbnailConfig.ts` (renderer) and `native/src/media/thumb_encode.rs` (native), in step
with the server. *Renderer path* (`StreamCapture`): `imageSmoothingQuality = "high"` (the default
single bilinear tap aliased text from 4K). *Native paths:* the box downscale + JPEG encode moved off
the encoder threads onto a `ThumbnailWorker` thread (depth-1 channel, drop-not-queue): at 960 px they
cost several ms, which on the encoder thread would be a late frame every interval. Windows now reads
back the smallest mip still ≥ 960 px (1080p → 960×540, 1440p → 1280×720, 4K → 960×540; it was ≤ 640)
and only memcpys the rows out before Unmap. Linux replaces its nearest-neighbour subsample (q60) with
the same area average. *Receivers* keep thumbnails for watched streams too (stopping watching showed
a poster from before you started), and the voice view fetches the server's cached copy
(FETCH_STREAM_THUMBNAIL) for any stream without one, so late joiners and people opening the view
don't wait up to 15 s. The profile popup polls at 15 s (it was 3 s). *Community server:*
`MAX_STREAM_THUMB_BYTES` 128 → 256 KB. The per-session thumbnail bucket goes from 6 burst / 1 per s to
3 burst / 1 per 5 s, which never refuses the real cadence and lowers a misbehaving client's ceiling
from 128 KB/s to ~51 KB/s per listener. Owner decision: no 128 KB fallback, because there are no other
servers pre-1.0. Steady state is ~4–8 KB/s per listener per stream (it was ~3). Verified: tsc web 0,
`cargo test --lib` 191 (7 new: target size, downscale bounds, 1080p noise → 960×540 under the cap,
schedule, worker), napi build, community build + e2e (new `[thumbs]`: 200 KB relayed, 256 KB + 1
dropped, exactly 256 KB relayed, 4th in a burst refused, fetch serves the last accepted, bucket refills
at 5 s). Windows: Windows Native Check on the push. Open (live): thumbnail sharpness and the encoder
thread staying hitch-free on a 4K source (Linux copies the full frame, ~33 MB, once per interval).

**Client: focused stream — click the video to go back to the grid (2026-10-04) ✅** — Owner feedback
on 0.8.5: in the voice view's focused stream, a click on the video now returns to the grid, the
video shows a pointer cursor, and the top-right "back to grid" button is gone (stats, fullscreen and
stop remain). Esc and the header breadcrumb still work. Double-click no longer enters fullscreen
from the panel (the first click already leaves it); inside fullscreen a click does nothing and a
double-click exits. The overlay controls swallow clicks, so the stats / fullscreen / stop buttons and
the volume slider never bounce you to the grid. Verified: tsc web 0; preview harness — the button is
gone, a click on the stats button keeps the stream focused, a real mouse click on the video clears
`fullscreenStream` and the grid shows both watched streams live.

**Client: Nocturne / Matinee — new default look (2026-10-04)** — Owner brief: a look that makes
people want to leave Discord (modern, sleek, elegant) with the layout untouched. It's two new
palettes, and `nocturne` replaces `graphite` as the default for new installs (`DEFAULT_THEME`,
`theme-boot.js`, `config.rs::default_theme`). Console is unchanged. The idea is a recording studio
after hours. *Type (shared by nocturne, matinee and both graphite themes):* Mona Sans (variable,
wght + wdth, `@fontsource-variable/mona-sans`) at width 100 for the UI and its expanded cut for the
server tabs (`--t-tile-stretch` 112.5 %) and the footer wordmark (`--t-wordmark-stretch` 125 %),
like the lettering on audio gear. Headers (`font-display`, which the chat / voice headers now use
too) stay in Inter at −0.02em: the owner found the expanded cut too wide there. The uppercase group
labels (ONLINE / OFFLINE, friends groups, channel categories, the stage's "N more") go through a new
`font-section` role: Inter at graphite's old size (11.25px at the default body) and 0.12em
tracking, the owner's pick. It is the meta face (JetBrains Mono) in console, so console doesn't
change. Code is JetBrains Mono, so
IBM Plex Mono is dropped. The type scale is written at the shipped 14.5px body. The stretch rides
on unlayered `.font-tile` / `.tracking-wordmark` rules and does nothing on fonts without a wdth axis.
`--font-tile` and the body tracking are per-theme knobs whose shared defaults are console's.
Graphite sets them in its own `[data-theme=graphite*]` block, because the graphite palette block
also answers the bare `:root` and would leak them into console. *Colour:* both ramps are built in
OKLCH at hue 300 (plum-black lacquer, not blue-grey). Nocturne's single accent is brass `#e8b966`
with near-black text on it. Its selection washes are champagne rather than brass, because a
saturated yellow at low alpha over plum goes brown. Matinee is a lilac-tinted paper (never cream)
with a velvet-plum accent `#6e3685`. One accent token has to work both as a fill under white and as
link text, and any gold dark enough for that is brown. The identity ramps (avatar and name colours)
share one lightness and chroma and vary only in hue, and every text pair clears 4.5:1. *Brand:* a
new `components/BellMark` draws the app icon's bell curve of level bars in the accent colour. It
replaces the music-note tile on login (where the bars rise once from the centre, skipped under
reduced motion) and the five flat bars in the members footer. *Small cross-theme fixes:* an empty
composer's send button is neutral (`surface-active`) instead of the accent at 40 %, and it lights
up when there's something to send. The chat and voice headers use `font-display`, which renders
identically in Graphite and Console. Verified: tsc web 0, `cargo test --lib config`, vite renderer
build (Mona Sans woff2 emitted, rules present in the CSS), and the preview harness across chat,
home, DM, voice, settings and login in Nocturne, Matinee and Graphite. Open: the app icon is
still azure (a brass variant would match the new default).

**Client: attachment speed caps actually apply (2026-10-04) ✅** — The Settings → Network
upload / download limits had done nothing since the Electron migration (0.6.0): the native
`set_transfer_limits` only saved the values, and Tauri's `RateLimiter` went away with the Rust
transfer path. They are now enforced per file, for attachments only (files, images, video, audio).
Voice, streams, link previews and GIFs never pass through the pacers. *Downloads* are paced in main
(`electron/main/downloadPacer.ts`) by a token bucket keyed on (server, attachment). It's applied in
the `decibell-attachment://` protocol, the loopback media server, netFetch attachment GETs (save-as,
copy image) and the encrypted-attachment fetch. Pacing the body read throttles the socket through
TCP backpressure (measured: the upstream send rate tracks the cap). net.fetch delivers bodies in
2 MiB chunks, so the bucket charges and delivers in 64 KiB slices. *Uploads* are paced in the
renderer (`features/chat/uploadPacing.ts`), because each PATCH is one buffered request. Under a cap
the loop sends about half a second of it per PATCH, in whole 64 KiB sealed chunks so encrypted
uploads stay aligned, and waits on a per-file bucket. Thumbnails count too. Both sides re-read the
cap on every slice or chunk, so a change applies mid-transfer, and a cancel never waits. A non-zero
cap is floored at 0.1 MB/s, because below that the community server's 30 s per-write inactivity
deadline could cut a download off. The native command is gone (`save_settings` already persists
both values); main learns the download cap over `decibell:attachments:setDownloadLimit`. The tab's
preset highlight used to match on rounded MB/s (0.1 MB/s lit "Unlimited", 1.4 lit "1 MB/s"); it
now matches exactly. Verified: tsc web + node 0, napi build, `cargo test --lib` 191. An Electron
harness against a local HTTP server: unlimited 24 MB in 0.1 s, 12 MB at 2 MB/s in 5.97 s, a cap
cleared mid-transfer finishes at full speed, two reads of the same file share the cap (6 s) while
two files don't (3 s), the floor holds, and cancel is immediate. The renderer pacer under a stubbed
store: chunk sizing, 10 MB at 2 MB/s, clearing the cap mid-upload, abort. Open: a live capped
upload and download against a real community server. An upload that takes over an hour is swept
as abandoned (`kUploadingTimeoutSeconds` counts from `created_at`), which a cap can only reach on a
server with a raised `DECIBELL_MAX_ATTACHMENT_BYTES`. Save-as still shows no progress while a capped
download runs.

**Client: channel / DM lists end above the user panel (2026-10-04) ✅** — Owner report: in a server
with a long channel list, the floating UserPanel covered the last channels, even when scrolled to
the bottom. Both sidebars reserved a fixed `pb-14` (56px), but the resting panel plus its 8px inset
is already 66px, and a voice session or stream controls (or the update chip) grow it to ~140px.
MainLayout's new `FloatingUserPanel` measures the panel with a ResizeObserver and writes its height
to `--user-panel-h` on the sidebar group (a style write, no re-render). `.sidebar-above-panel`
reserves inset + height + an 8px gap, so the scroll area ends just above the panel and follows it
as it grows and shrinks. `.fade-end` dissolves the list's bottom edge over its own 10px padding, so
rows fade out mid-scroll while the last row stays fully opaque at the end. Applied to
ServerChannelsSidebar and ConversationSidebar. Verified: tsc web 0, vite build, preview harness
with 26 channels scrolled to the bottom (at rest, in voice + streaming in Nocturne, in voice in
Graphite Light): the last channel sits 8px above the panel and the scrollbar stops there.

**Client: bigger channel-type icons (2026-10-04) ✅** — Owner request: the `#` / microphone at
the head of each channel row were small (a row-size `#` in a 14px box, and a fixed 14px mic that
ignored the Text size setting). Both now sit in one `.channel-icon` slot sized at 1.45em of the
row's `text-channel`, so they grow with the Text size setting in every theme. The `#` is set at
the slot size and the mic is drawn at 0.82em, because its strokes fill the box where a `#` sits
inside its line height. Names start at the same x for text and voice rows. Verified: tsc web 0,
vite build, harness (Nocturne / Graphite / Matinee / Console, incl. the active and connected-voice
states).

**Client: offline members light up on hover (2026-10-04) ✅** — Owner request: hovering an offline
member left the name muted, while an online member's name brightens. `MembersList` now brightens the
offline name to `text-primary` on hover too, so both states end in the same place. The offline
avatar keeps its dimming. The DM view's friends rail already brightened every name on hover, and the
Friends page shows every name at full brightness, so neither needed a change. Verified: tsc web 0;
harness hover on an offline member in Nocturne / Graphite / Matinee / Console Light.

**Client: Transfers panel, phase 1 — downloads (2026-10-04) ✅** — A Chrome/Firefox-style downloads
popover from a new title-bar button. Design: `docs/superpowers/specs/2026-10-04-transfers-panel-design.md`.
Owner decisions: downloads go straight to a folder (the OS Downloads folder by default, changeable in
Settings → Network, plus an "Ask where to save each file" toggle that's off by default), history
persists across restarts, Downloads | Uploads tabs, and the button is always visible.
*Main:* `electron/main/downloads.ts` owns each download. It streams to `‹final›.part` through the
speed-cap pacer and renames on completion, so nothing crosses IPC except small records. The old save
path buffered the whole file in main and sent it over IPC twice. Pause keeps the `.part`; resume
sends `Range: bytes=‹size›-`, and a 200 means the server ignored the range, so it starts over.
Encrypted files go through the protocol's decrypt path in chunk-aligned windows (`fetchDecryptedWith`
+ abort support in `readPaced`). The file key is snapshotted at start and kept wrapped by
`safeStorage` until the download finishes, so a paused one resumes after a restart. Records live in
`userData/downloads.json` (last 100, atomic writes); quitting saves running downloads as paused.
Filenames from the sender are sanitised (separators, Windows reserved names and characters, leading
and trailing dots, a 200-byte cap) and made unique (`name (1).ext`). The renderer addresses
downloads by id only: open, reveal and remove act on paths main recorded. *Renderer:*
`features/transfers/` has the button and the panel.
- The button: a ring fills with the combined progress, a dot marks a finish or failure while the
  panel was shut, the arrow drops when a download starts, and Ctrl/Cmd+J toggles the panel.
- The panel: rows for active, paused, done (click opens the file), failed, cancelled and deleted;
  thumbnails; a right-click menu; a footer with the speed cap, Open folder and Clear.
- Opening an executable or script asks first (`ConfirmModal`). The four save sites (file card, audio,
  video, "Save as…") now call `startDownload`; "Save as…" still asks for a location.
- `AppSettings` gains `download_dir` + `ask_download_location`.
Verified: tsc web + node 0, napi build, `cargo test --lib`, and an Electron harness against a local
HTTPS server with 30 checks over two launches. It covered sanitisation, collisions, pause/resume over
Range, a server that ignores Range, cancel, 404, encrypted pause/resume, the cap (3 MB at 1 MB/s in
2.9 s), deleted-file detection, Clear, and a quit mid-download followed by a relaunch that resumed a
plain file and an encrypted one from the wrapped key alone. The preview harness covered every row
state in Nocturne, Matinee, Graphite and Console Light, plus the empty states and the settings
section. Open: a live run against a real community server, Open / Show in folder on Windows and
macOS, and the uploads tab (phase 2).

**Transfers panel, phase 2 — uploads (2026-10-04) ✅** — The Uploads tab: every attachment from
the moment its message starts sending, with live progress, speed and the target channel, then
Uploaded / Failed / Cancelled for the rest of the session. *Client:*
- `features/transfers/uploads.ts` mirrors `attachmentsStore` into `transfersStore`, so a row outlives
  its pending (which is removed when the message sends). Speed is an EWMA kept outside the store.
- Pause (`PendingAttachment.paused`) holds the upload loop before its next chunk, and the message
  waits with it. The bubble shows "Paused · N%". Cancel is the existing abort; `markFailed` now
  records that it was a cancel.
- A PATCH 404 fails immediately as "Upload expired on the server" instead of five backoff retries.
- Clicking an uploaded row opens its channel. The button's ring and counts include uploads, and only
  a failed upload lights the dot, not every successful send.
- Opening the panel shows whichever tab saw something start since it was last opened.
*Community server:* the abandoned-upload sweep deleted `uploading` rows by `created_at` > 1 h, so a
paused or slowly capped upload that was still alive could vanish under the client. It now sweeps by
idle time: schema v10 adds `attachments.last_activity_at`, every accepted PATCH stamps it, and the
sweep compares `MAX(created_at, last_activity_at)`. The client needs no server release for anything
else; until the server is rebuilt, an upload paused for over an hour fails cleanly as "expired".
Verified: tsc web 0; community build + e2e 301/301, including a new `[transfers]` test: PATCH stamps
the column, a never-patched upload and one idle for 2 h are swept, an old upload with recent
progress is kept and resumes. A preview-harness simulation runs the real upload loop against a fake
attachment server: pause holds before the next chunk and resume finishes, cancel while paused ends
cancelled, a 404 fails at once with no retries and lights the error dot, a row survives its pending's
removal, one that vanishes mid-flight shows as cancelled, and opening the panel picks the tab with
news. Screenshots in Nocturne and Matinee. Open: a live capped upload with pause against a real
server; the community release for the sweep change.

**Transfers panel, phase 3 — polish (2026-10-04) ✅** — The rest of the design:
- *Go to message*: from a download's right-click menu, and by clicking a finished upload, which finds
  its message by attachment id once the echo is loaded, else just opens the channel. `chatStore`
  gains a `jumpRequest` that `ChatPanel` consumes once that channel is active and its first page is
  in. It runs two frames later because a freshly mounted list first trims a large cached slice to the
  viewport; jumping before that aimed at a row the trim dropped, so no window was fetched and nothing
  happened. Unconsumed requests go stale after 15 s.
- *Taskbar / dock progress*: `setProgressBar` over every running transfer, in paused mode when
  everything is paused, sent at most 4 times a second.
- *Quit guard*: an unfinished upload is lost on quit along with its message, so closing the window,
  or Cmd+Q / an update restart through `before-quit`, asks "Keep uploading / Quit anyway" once per
  quit. Downloads don't ask: they pause and resume next launch.
Verified: tsc web + node 0. An Electron test of `window.ts` with a stubbed message box: progress bar
calls, no prompt without uploads, Keep uploading keeps the window, Quit anyway closes it and
before-quit doesn't ask again. The preview harness with the real `ChatPanel`: a jump into a small
cached channel lands and highlights the row in view; one into a 200-message cache now requests a
window around the target (before the fix it did nothing). Open: a live check of taskbar progress on
Windows and macOS (Linux shows it only on Unity-style launchers).

**Client: members-list toggle in text channels (2026-10-04) ✅** — Owner request: a header button in
server text channels that shows / hides the members list, the same way (and with the same icon) as
the friends toggle in a DM conversation. `uiStore` already had `membersPanelVisible` +
`toggleMembersPanel`, remembered per install in local storage (`decibell.layout.membersPanel`, read
synchronously at store creation, like the DM toggle's key), but nothing in the UI called it.
`ChatPanel`'s header now ends in `MembersPanelToggle`: a filled state while the panel is shown,
"Hide members" / "Show members" as its title. It's its own component, so a toggle re-renders only the
button. Verified: tsc web + node 0, vite build; harness with a real click: the panel hides and shows
and the key flips 0 / 1, and a reload with the key at 0 or 1 restores that state.

**Client: download button beside image / video attachments (2026-10-04) ✅** — A message's images and
videos get one icon button to their right, shown on hover or keyboard focus. Clicking it downloads
every one of them straight to the download folder through the Transfers manager, one at a time, so
"Ask where to save each file" still asks for each. The label reads "Download" or "Download all N",
and the button shows a check for 1.6 s once they've started. Owner's call: beside the media rather
than over its corner, and vertically centred on the media (owner's call, after a first bottom-aligned
cut). It isn't top-aligned because the message's hover toolbar owns the row's top-right corner: on
chat panels under about 660 px it covered a top-aligned button on a media-only follow-up message.
The grid and the video card carry `mt-2`, so the button takes the same margin to centre on the media
itself. To make room, attachment media (not link previews) reserve 38 px more
(`MEDIA_ACTION_RESERVE_PX`, inside `reserveBox`, so the thumbnail prefetcher stays byte-identical).
That only shrinks media on chat panels under about 530 px. A grid's 320 px minimum now yields to that
room (`gridWidth`) instead of overflowing into the button. `startDownload` reports whether a download
started. Verified: tsc web 0. The preview harness with the real `ChatPanel` at 380 / 420 / 620 / 1100
px: a 6 px gap, never past the column, centred to the pixel on an image, a grid, a video card and a
media-only follow-up, and clear of the hover toolbar at 620 px. Clicking "Download all 4" starts
attachments 31–34 in order with no dialog, the right channel and message, and shows the check.

**Client: day dividers in the message list (2026-10-04) ✅** — Owner request: a separating line
between messages whose dates differ. When two consecutive loaded messages fall on different local
calendar days, the later one gets a `DayDivider` (a hairline with the date centred on it, e.g.
"Saturday, October 3, 2026"; an absolute date, because a "Today" label would go stale at midnight on
a mounted list). It applies to both server channels and DMs. The divider renders *inside* that
message's list row, above the bubble, so RealMessageList's item model (one row per message, keyed by
identity, offsetTop measurement) is untouched. The rare case where a prepended page gives the
window's first row a divider is the same in-row height change as the existing grouping flip, which
the every-commit placement effect and native anchoring already handle. `shouldGroup` now also breaks
at a day boundary, so a 23:58 → 00:01 pair from one sender opens a full row under the divider.
Day keys are cached per message (WeakMap), like the epoch cache. Verified: tsc web + node 0, vite
build; harness with messages spread over four days in Nocturne / Matinee / Console (dividers between
the days only, the midnight pair split, today's pair still grouped) and in a DM.

**Client: Linux native H.264 streams showed nothing to watchers (2026-10-04) ✅** — the Linux native
encoder (`encoder_linux.rs::build_encoded_frame`) converted FFmpeg's Annex B H.264 to AVCC
(length-prefixed NALs) and moved SPS/PPS into an avcC description, but the wire only carries a
description for HEVC/AV1 (`encoder_thread_linux.rs`, receiver strip in `media/mod.rs`) — so remote
watchers got length-prefixed H.264 with nothing to configure from and decoded nothing, while the
streamer's own preview (which got the description locally) looked fine. The receiver-side avcC
derivation the old comment relied on left with the Tauri-era native decoder. H.264 now goes out as
Annex B with SPS/PPS inline, no description — the same format Windows native and the WebCodecs
encoder send — and a keyframe that arrives without SPS gets the last-seen parameter sets spliced in
(AMF on Linux, like on Windows, only repeats them on forced IDRs). The dead avcC helpers are gone.
Verified: `cargo test --lib` 194 (3 new), napi build. User-confirmed symptom; live re-test pending
(watch a Linux NVENC H.264 stream from another machine). AV1 / HEVC paths unchanged.

**Permissions v3 (2026-10-09) ✅ — needs a community-server release + live test** — the first
step of the Discord-parity push: every community feature mapped to the permission that should gate
it, then the gaps closed (spec: `docs/superpowers/specs/2026-10-09-permissions-v3-design.md`).
New bits, all enforced server-side: **Create Invite** (on for everyone by default; makes invites and
lists / revokes your own, while Manage Invites now means everyone's and implies create), **Change
Nickname** (own nickname, default on), **Embed Links** (default on; the server marks a message
`suppress_embeds` when the sender lacks it, persisted and carried on history + edits; clients skip
link-preview / GIF cards and keep the URL text, invite cards still render), **Speak** (default on;
the relay drops AUDIO and its sealed twin like server mute, presence carries `is_suppressed`, the
local mic shows blocked; stream audio is untouched, as with server mute). **Voice Moderate split**
into Mute / Deafen / Move Members, now channel-scoped: resolved in the target's current voice channel,
or server-wide when they aren't in voice. Disconnect is Move. Migration v11 gives every role / overwrite that
held the old bit (same value, now Mute) the other two. **Moves check only the mover** (owner
decision): they must see the destination; a target who couldn't view / join it gets a runtime
**voice pass** (VIEW + CONNECT there while any of their sessions is in it, everything else through
the normal chain), and their channel list is re-sent before `VOICE_FORCE_NOTIFY{MOVED}`; leaving
ends it. **"Members with this role can manage each other"** (`RoleInfo.manage_each_other`):
members whose highest role is the same flagged role may change each other's nicknames and
voice-moderate each other. Kick / ban / timeout / role changes stay strictly above-only, and the
shared role sits at their own level, so a peer can never remove it. **Category sync**
(`channels.perm_synced`, `ChannelInfo.permissions_synced`, `CHANNEL_PERMISSIONS_SYNC_REQ = 147`):
a synced channel resolves through its category's overwrites; editing it copies them over first and
un-syncs; "Sync now" drops its own rows behind the usual escalation and lock-out guards. Deleting a
category copies its rows onto its synced channels so a private category's channels stay private. A
category is now listed only when the user can view it or a channel under it. **Fix:**
`FETCH_STREAM_THUMBNAIL_REQ` served any streamer's thumbnail by username, so anyone could
pull the live thumbnail of a stream in a voice channel hidden from them. It now requires View
Channel where the stream is (no live stream → no thumbnail). Invites follow their creator on a
username rename. Client: grouped role editor (General / Members / Text / Voice) with the peer toggle,
per-bit voice menu gated on the target's channel ("Move to" lists every voice channel you can see),
`useHierarchy().canManage`, invite UI on Create Invite ("Your invites" without Manage), "No speak"
badge, Speak blocked mic, category Permissions copy + "Synced with …" / "Sync now";
`useChannelPermission` no longer treats `myPermissions = 0` as a legacy server (the community server
must ship with or before this client). Verified: community e2e 371 / 0 (73 new: v11 migration on a
downgraded DB, invites / nicknames, peers, voice split + passes, Speak relay drop, Embed Links,
thumbnail scope, category sync incl. guards and category delete), `cargo test --lib` 194, napi build,
tsc web 0. Live tests pending: moving someone into a private voice channel and talking with them
(MLS join under a pass), Speak-denied rooms, the role / channel permission UIs.

**Category sync is opt-in and never silent (2026-10-09) ✅ — same community release + live test** —
a review note on the entry above: following the new category on a drag (Discord's behaviour), and
falling back to "no rows" when a synced channel was dragged *out* of its category, could quietly make
a private channel public. Owner decision: categories are plain groups unless their **"Channels
follow this category's permissions"** switch is on (`category_sync`, default off, which is how every
server behaved before v3). Dropping a channel into a syncing category asks **Sync permissions /
Keep permissions / Cancel** (`CategorySyncPrompt`; Sync needs Manage Roles in the channel). Server
rule: nothing but an explicit sync changes what a channel allows. Moves (including channels shifted
by a moved header), switching a category off, and deleting it all copy the rows a channel was
following onto it and unsync it. Switching a category on changes nothing until channels sync.
Channels created inside a syncing category follow it from the start. New wire:
`CATEGORY_SYNC_SET_REQ = 148`, `ChannelInfo.category_sync = 14`, and
`ChannelPermissionsSyncRequest.category_id`: the category the user agreed to, so a sync sent
right after the reorder can't land on the old category if the reorder failed. A category id syncs
every channel under it ("Sync all"). Migration v12: a v10 server's categories all stay plain,
even ones carrying formerly inert overwrites; a DB that ran v11 live keeps syncing the categories
whose rows were applying. Client: the switch + "N of M channels follow" + "Sync all" in category
settings (overwrite editor hidden for a plain group), the channel sync row only under syncing
categories, `ConfirmModal` gained an optional middle choice and an accent tone. Verified: community
e2e 391 / 0 (v12 migration both ways; plain groups, opt-in, the expected-category guard, drag out /
back in, switch off, Sync all, delete), `cargo test --lib` 194, napi build, tsc web 0; the prompt
and both category states screenshotted in Nocturne + Matinee through the preview harness.

**Client: channel permission edits are saved with Save (2026-10-10) ✅** — Owner report: changing
permissions in channel settings left the Save button grey (the changes still got saved). The
overwrite toggles applied on every click while Save / Cancel only covered the other settings, so
Cancel couldn't undo them either. `ChannelPermissionsSection` now stages edits per target (an edit
back to the stored value drops out) and the category sync switch. It reports `onDirtyChange` and
exposes `commit()`, which sends overwrites first and then the switch. `ChannelSettingsModal` lights
Save for either kind of change and no longer requires Manage Channels to save permission edits (those
need Manage Roles in the channel, which the section checks). Unsaved targets read "(unsaved)" in
the picker; "Sync now" / "Sync all" stay immediate actions and wait until staged edits are saved
or discarded. Verified: tsc web 0; in the preview harness, clicking a toggle in the real dialog
enables Save, and setting it back disables it again.

**Client: bigger settings windows (2026-10-10) ✅** — Owner request: client and server settings were
a fixed 820 × 560 px card (wider than the 800 px minimum window, so it ran off the edge there). Both
shells now fill the window up to `max-w-400` × `max-h-280` (1600 × 1120 px; first 1100 × 760, raised
for fullscreen use), with a `p-8` margin on the backdrop: a 1080p fullscreen window is filled
apart from the margin, and they shrink to fit small windows. The sidebar went from 210 px to `w-60`
(240 px). The two shells stay identical. Verified: tsc web 0; preview-harness screenshots at
800 × 600, 1440 × 900, 1920 × 1080 and 2560 × 1440.

**Client: global hotkeys + push-to-talk (2026-10-10) ✅ — live tests pending** — Owner request:
mute / deafen etc. from anywhere. Seven actions: toggle mute, toggle deafen, push to talk, push to
mute, leave voice / hang up, answer / decline a DM call. Owner calls: all seven; macOS gets press
actions through Electron `globalShortcut` only (no Input Monitoring prompt). Electron's own
`globalShortcut` can't be the base: Electron 33 has no Wayland backend (we run native Wayland) and it
never reports releases. So the listener is native (`native/src/hotkeys/`): the GlobalShortcuts
portal on Wayland (zbus; host `Registry.Register("decibell")`, preferred triggers, KDE's confirm
dialog, `Activated` / `Deactivated` → push-to-talk works; v2 `ConfigureShortcuts` behind "Open
system shortcut settings"), falling back to XInput2 through XWayland on desktops without the portal;
XInput2 raw events on X11 (x11rb, new dep); Raw Input on a message-only window on Windows
(passive, nothing swallowed; mouse registered only while a binding uses it). Bindings are DOM
`code`s (generic modifiers, sided only when modifier-only, Mouse3–5) mapped to evdev / scancodes by a
table generated from Chromium's `dom_code_data.inc`. One shared matcher (Rust, mirrored in TS for
the renderer's focused fallback). Push-to-talk / push-to-mute drive `media/voice_gate.rs` straight
from native: a forced-closed gate isn't a mute (no `FLAG_MUTED`, the fade tail still runs), and PTT
sends no pre-roll from before the key went down. Press actions round-trip to the renderer as
`hotkey_action` and run `features/voice/voiceActions.ts`, now the only copy of the mute / deafen /
leave logic (UserPanel, the voice dock and CallStage each had one). `decibell --hotkey=<action>`
(`:down` / `:up` for holds) goes through the single-instance handshake for window managers that
bind commands. Settings → Keybinds (recorder, per-backend notes, the desktop's actual trigger) and
Settings → Audio → Input Mode (voice activity / push to talk + release delay, default 100 ms).
Found on the way: a `ShortcutsChanged` body read as `(o, Value)` types as `ov` and never matches
`oa(sa{sv})` — read it with its real signature (now unit-tested through a real message encode).
Verified: `cargo test --lib` 215 (+ key table, matcher, gate timing, portal message parsing), the
live portal test against KDE 6.7 (Register, v2, CreateSession, ListShortcuts), the Windows module
type-checked on Linux against `windows` 0.61, napi build, tsc web 0 / node 0, the TS matcher against
the Rust cases. Pending: KDE dialog + push-to-talk end to end, X11, Windows, macOS.
Design: `docs/superpowers/specs/2026-10-10-global-hotkeys-design.md`.

**Client: global hotkeys watch bound keys only (2026-10-10) ✅ — live tests pending** — Owner
question: "is the client listening for every key press?" On X11 and Windows it was (XInput2 raw
events / Raw Input deliver everything; the matcher dropped what wasn't bound), and both listeners
kept running after the last binding was removed. Owner call: bound keys only — the other way "gives
keylogger vibes". Windows now polls `GetAsyncKeyState` for just the watched keys (the bound ones,
plus Ctrl/Shift/Alt/Win when a press binding needs exact modifiers) every 10 ms: nothing hooked or
registered, nothing swallowed, mouse buttons and modifier-only combos still work; layout-dependent
keys resolve through `MapVirtualKey(scancode)`. X11 (and XWayland) grabs exactly the bound combos
(synchronous grabs, ungrabbed the instant they fire so input typed meanwhile goes to the focused
app, never to us), then samples the bound keys' bits until release; a combo another client grabs
lands in `Status::failed` and the row says so. Both threads exist only while a binding does. Cost of
grabbing on X11 (as with the portal and macOS): the bound press doesn't also reach the focused app —
the Keybinds tab warns when a typing key is bound there. Discord, for comparison, listens to all
input on every platform as far as its permissions and failure modes show. Verified: hotkeys unit
tests 19 (+ watch list, VK table, X11 grab specs / lock variants), the Windows poller type-checked on
Linux against `windows` 0.61 (planted error caught) and green on the Windows Native Check, tsc
web 0. X11 end to end (`hotkeys::tests::x11_listener_end_to_end`, ignored; real listener vs. a
throwaway Xvfb driven over XTEST) passes: Ctrl+M fires once, Ctrl+Shift+M and bare M don't, a
Mouse4 push-to-talk press/release reaches the voice gate, a sub-10 ms tap counts once, unbound keys
typed idle and while push-to-talk is held never reach the matcher, and unbinding drops every grab.

**Client: mouse side buttons on Wayland get a remap guide (2026-10-10) ✅ — live test pending** — A
side-agent note: on KDE / GNOME the portal can't bind mouse buttons, and many gamers keep
push-to-talk on a side button. Owner asked whether a different listening approach could fix it. It
can't within bound-keys-only: the shortcuts spec is keys-only, and `/dev/input` is `root:input` with
logind `uaccess` for joysticks only, so reading the mouse would mean the `input` group and every
keyboard with it. Owner call: guide the user rather than write desktop config. Recording a side
button on the portal backend now opens a guide in that row: KDE → Extra Mouse Buttons (KWin
`buttonsrebind`, confirmed on Plasma 6.7.5) with an "Open mouse settings" button
(`systemsettings kcm_mouse`) and a suggested combo (Ctrl + Meta + F9 since the owner noted Meta + F9 is a
KDE default), then record the button again; GNOME →
Piper / input-remapper; others → their input settings or the command line. The recording hint says
side buttons need a remap; the Keybinds tab's top card is "On this computer", the list "Your
keybinds". Verified: tsc web 0 / node 0; the guide, triggered by a synthetic back-button press in
the real recorder, screenshotted in Nocturne and Matinee, plus Audio → Input Mode. Pending: whether
KWin's remap delivers press and release (push-to-talk) through the portal.

**Client: "games won't see it" badge + the push-to-talk key in Audio (2026-10-10) ✅** — Owner live
test on KDE Plasma 6.7.5 (Wayland portal): keybinds and push-to-talk work. Owner requests: (1) mark,
where the desktop takes the bound press, that the game in front won't register it; (2) show the
bound push-to-talk key in Audio when push to talk is on. A shared `features/hotkeys/KeybindDisplay`
now holds the key chips, `isSwallowed` (portal / x11 / xwayland, and press actions on macOS; never
Windows polling or the focused fallback), the typing-key check and the desktop-assigned trigger. A
warning pill, "Games won't see it" (tooltip explains and suggests a key games don't use), sits in each
affected Keybinds row and once in the "On this computer" card. Audio → Input Mode lists the push to
talk key(s) with the same pill, the desktop's own trigger when it differs, and "Change in Keybinds".
Verified: tsc web 0; screenshots in Nocturne + Matinee (portal) and Nocturne (Windows: no pill).

**Client: X11 hotkeys read the keyboard only while a key hold is down (2026-10-10) ✅** — A
side-agent note: after every grabbed press the X11 listener sampled `XQueryKeymap` until release,
and X answers that with every key held, so the Keybinds tab's "nothing else you type" overstated it.
Now press actions fire straight from the grabbed event (the grab already matched exact modifiers)
with no state read; a hold on a mouse button or a lone modifier polls only `XIQueryPointer`
(buttons + modifier state); only a hold on an ordinary key polls the keymap, while it's down, and only
its bit is read. The X11 path no longer goes through the shared matcher. The tab's X11 / XWayland text
now says exactly that, including how to avoid it. Verified: hotkeys tests 19, the X11 e2e against
Xvfb with a new keymap-read counter (zero reads for toggles, exact-modifier misses, taps, typing and a
mouse hold; reads only during a B hold, none after), and a deliberate always-read mutation fails it.

**Client: one on/off switch, drawn right (2026-10-10) ✅** — Owner report: some switches look off,
Public listing in server settings for sure when on. Six hand-rolled copies. Public listing's knob was
an absolutely positioned `<span>` with no inset, so it sat where the button centres inline content:
measured 20 px in when *off* (reads as on) and 16 px past the track when on. The other five (Audio,
Privacy, Codecs, Downloads, capture picker) shared one markup whose knob sat 1 px low (3 / 1 px
top / bottom) and stopped 4 px short of the end when on (3 px start when off). All six are now
`components/Switch.tsx`: the track lays the knob out (inline-flex, 2 px padding), so it's 2 px from
every edge in both states, with `role="switch"` / `aria-checked` everywhere (three copies lacked
them) and a dimmed disabled state. Verified: tsc web 0; knob geometry measured in Nocturne,
Matinee and Console (2 / 2 / 2 / 2 px, off and on, disabled too); real Server Settings → Overview
(Public listing off / on) and Settings → Privacy screenshotted.

**Client: settings windows leave more room on small app windows (2026-10-10) ✅** — Owner request:
a little smaller at small window sizes, max unchanged. Both shells' backdrop margin is now 56 px
(sides) / 48 px (top-bottom) below 1024 px wide / 720 px tall and stays 32 px above, so at the
800 × 600 minimum the card is 688 × 504 (was 736 × 536) while 1024 × 768 and up — and the
1600 × 1120 cap — are untouched. Verified: card measured at 800×600, 900×700, 1024×768, 1280×800,
1920×1080, 2560×1440 before / after (identical from 1024×768 up); tsc web 0.
Follow-up (owner: start shrinking at a larger window size): thresholds raised to 1280 px wide /
860 px tall. 1024×768 → 912 × 672, 1280×800 → 1216 × 704, 1366×768 → 1302 × 672; 1440×900,
1920×1000, 1920×1080 and 2560×1440 unchanged (max 1600 × 1120 intact). Measured at all of them.
Then (owner: always keep distance from the edges, feel like a floating menu): the margin scales with
the window — max(56 px, 5 % of width) at the sides, max(48 px, 6 % of height) top and bottom — instead
of stepping to 32 px on larger windows. 1280×800 → 1152 × 704, 1440×900 → 1296 × 792, 1920×1080 →
1600 × 950 (was 1600 × 1015); small windows as before; the 1600 × 1120 cap is unchanged.

**Client: no wordmark under the members list (2026-10-10) ✅** — Owner request: drop the bell mark +
"DECIBELL" footer at the bottom of `MembersList`. Removed; the list keeps 12 px of bottom padding in
its place so the last row doesn't sit on the edge. `BellMark` stays (login page). Verified: tsc web 0;
the list rendered with 20 members.

**Client: mouse back / forward no longer walk history (2026-10-10) ✅** — Owner report: the side
buttons step between screens, back to the login screen too. Main already swallowed the
`browser-backward/forward` app-commands and cleared history on load, but Chromium navigates on the
buttons' *release inside the renderer*, which app-command never sees, and login / logout pushed new
hash entries after that one clear. Now: `lib/mouseNavGuard.ts` cancels the default of a back /
forward `mouseup` (capture phase; the buttons still reach the keybind recorder and the hotkey
fallback); login and logout `navigate(…, { replace: true })`, so history never grows; and /login
redirects a signed-in user to the app (`SignedOutRoute`). Verified: a real back press sent through
CDP `Input.dispatchMouseEvent` moves `#/` → `#/login` without the guard and stays on `#/` with it;
tsc web 0 / node 0.

**Native: "Default" audio devices follow the OS default (2026-10-10) ✅ — Windows live test pending** —
Field report (Windows): with Default chosen, changing the system default left Decibell on the old
device. WASAPI (and CoreAudio) bind a stream to the endpoint that was default when it opened;
PipeWire / PulseAudio move unpinned streams themselves, so Linux never had this. New
`media/default_device_watch.rs`: a thread per pipeline (not on Linux) reads the defaults about once
a second — Windows: the default communications + console endpoint IDs through one
`IMMDeviceEnumerator`, matching `get_default_device`'s preference; macOS: CPAL's default names — and
on a real change sends `DefaultInputChanged` / `DefaultOutputChanged`. The pipeline now tracks the
selections and turns those into the same `SetInputDevice(None)` / `SetOutputDevice(None)` /
`SetStreamOutputDevice(None)` hot-swaps a Settings pick sends, only for what is on Default — the
2026-08 "don't re-push on every devicechange" rule (pops on device churn) holds, since only a
different default endpoint triggers a swap. Also fixed: toggling "Separate stream output device"
rebuilt the voice output on the default device instead of the chosen one. Verified: watcher unit
tests (scripted defaults: only real per-direction changes fire, a gap with no default is ignored,
a refused send ends the thread), `cargo test --lib` 221, the Windows probe type-checked against
`windows` 0.61 (planted error caught), napi build, tsc web 0.

**Client: right-clicking a voice participant opens one menu (2026-10-10) ✅** — Owner report: in the
channels list, right-clicking a user under a voice channel opened the user menu *and* the channel
list's menu. The participant rows sit inside the list container, whose delegated
`onListContextMenu` ran after the row's handler (React bubbling) and, finding no
`[data-reorder-id]` above the row, opened the "empty area" menu on top — only for members who can
manage channels. The list handler now returns when `e.defaultPrevented`: a nested row that opened
its own menu already called `preventDefault`. Verified: tsc web 0.

**Native: a moved member can talk in the new channel (2026-10-10) ✅ — live test pending** — Owner
report: after moving someone into another voice channel and joining it, neither side heard the
other. The server switches the session and relays UDP by its live channel, but the moved client
only updated the renderer: its MLS group driver stayed bound to the *old* channel (its requests
there were refused, the new channel's GroupInfo / commits were filtered out by channel id), so it
never joined the destination's group and its media was sealed under keys nobody there had. Now
the community read loop handles `VOICE_FORCE_NOTIFY{MOVED}` itself (`rebind_voice_group` in
`commands/voice.rs`): the engine keeps running, the old driver is dropped, the shared key ring is
emptied (nothing is sealed until the new epoch), native watches are cleared, and a driver for the
new channel starts on the same ring — seeded with that channel's roster from a per-loop cache,
because the destination's presence update arrives *before* the notice. No leave / rejoin, so a
voice pass granted by the move stays held. The renderer resets its e2ee badge on the move.
Verified: napi build, `cargo test --lib` 221, tsc web 0; two-client move test pending.

**Client + community: voice channel rosters in the sidebar (2026-10-10) ✅ — community deploy pending
for item 7** — Owner report: with people in two channels, leaving one hid the members of the other
until rejoining and leaving it. Audit of the presence path found:
1. *The reported bug.* `voiceStore.disconnect()` wiped `channelPresence` / `channelUserStates` /
   `streamsByUser` — but those mirror every channel of every community (the server sends them to all
   members: a snapshot at auth, then each change), not the voice session. Every other channel stayed
   empty until it next changed. Same on a DM-call hang-up, a failed join and a forced disconnect.
   `disconnect()` now clears session state only.
2. *Channel ids are name slugs*, so communities share them (`general`, `lounge`), yet presence was keyed
   by channel id alone and the listener ignored `serverId`: another server's same-named channel
   overwrote this one's roster, could replace the connected channel's participants, lit up as
   "connected" in the sidebar (clicking it opened the voice view instead of joining), and wasn't seen
   as a switch by the join flow. Presence is now keyed by `voiceKey(serverId, channelId)`, and the
   connected checks compare the server too.
3. *Rows of channels we're not in* used the connected voice session's server for nicknames, the profile
   popup and the context menu — no server-scoped actions (move / mute) when not in voice, the wrong
   server when in voice elsewhere — and took LIVE from the connected channel's streams, so nobody
   streaming in another channel showed LIVE. They now use their own server and `streamsByUser`. The
   context menu looks the target up on its own server only.
4. *Join / leave sounds* compared against the previous channel's roster after a switch or a later
   rejoin, playing one per member of both channels; the baselines now re-seed from the cache whenever
   the connected channel changes. Same for stream start / stop.
5. *Optimistic switch* kept the old channel's members under the new one until the server's update;
   the roster is seeded from the cache (shared with the move handler: `seedParticipants`).
6. *Stale rosters*: the auth snapshot omits empty channels, so a channel that emptied during a
   reconnect kept ghost members; a channel that became hidden kept its last roster. A community's
   presence is now cleared at auth, on connection loss and on membership revocation, and pruned to the
   channels still listed on each channel-list push.
7. *Community:* a channel that became visible (role / overwrite change) got no presence until it next
   changed — broadcasts go to viewers only. `broadcast_channels` / `send_channels_to_user` now re-send
   the presence snapshot after the list.
8. `setParticipants` kept each existing participant's mute / deafen over the server's fresh roster,
   so icons lagged until the next audio-packet flags; the roster now wins.
Also: the voice panel, user panel and stream view looked the connected channel's name up in the
*viewed* server (showed "Voice" or the wrong name while browsing elsewhere). Verified: tsc web 0;
e2e 394 (new `test_presence_follows_visibility`, which fails on the old server); a node run of the
store (slug collision, leave keeps rosters + streams, prune / clear per server, no-op identity,
seeding). Live multi-channel / multi-server test pending.

**Client: drag a voice member onto another channel to move them (2026-10-10) ✅ — live test pending** —
Owner request (Discord-style). In the channel sidebar, a voice participant row is draggable where we
hold MOVE_MEMBERS in its channel (the owner always); the hierarchy (above them, or a peer with "manage
each other") is checked when the drag starts, so a refused drag never starts and rows don't each
subscribe to the member list — `canManageMember` reads the same rule as `useHierarchy` once. Our own
row, the owner and anyone we can't manage don't drag. While a drag is in flight every other voice
channel on that server pulses like a file-drop target and the hovered one fills with accent; the row
and its member list are one drop zone, the source channel isn't one. A drop sends the same
`voice_mod` MOVE as "Move to…" (server checks and voice passes unchanged; refusals toast via
`mod_action_responded`). Works without being in voice ourselves. `features/voice/memberDrag.ts`; the
member drag stops propagation so the list's delegated reorder handler (which cancels drags that don't
start on a channel row) never sees it, and window-level dragend / drop end a drag whose source row
unmounted. Verified: tsc web 0; a harness rendering the real sidebar with seeded stores fired real
drag events — draggable only under MOVE_MEMBERS, higher role / owner refused at dragstart, source not
a target, member rows inside a target accept, one `voice_mod` per drop with the right args, a stray
drop clears state — plus a screenshot of the in-flight styling.

**Client: themed dropdown pickers replace native selects (2026-10-10) ✅** — Owner request: the
dropdowns looked "system default" (a native `<select>` popup is drawn by the OS and ignores the theme).
New `components/Select.tsx` — select-only combobox (ARIA 1.2: focus stays on the trigger or the filter
field, aria-activedescendant), keyboard parity with the native control (arrows, Home / End,
PageUp / PageDown, Enter / Space, type-to-jump with repeat-cycling, Tab), Escape stopped so it never
reaches a modal's window listener, groups, hints, leading icons, disabled rows, a filter field past 10
options, md / sm sizes and field / sunken tones. The popup is portaled to `<body>` with fixed
positioning (flips above near the bottom edge; closes on outside press, outside scroll, blur), so a
scrolling modal can't clip it. Per-family styling in `globals.css` (`.dsel-*`): nocturne / matinee get a
champagne wash + brass rule on the chosen row and a sheen; graphite* a solid-accent active row with a
leading check; console* the meta face, a `›` cursor, `//` group labels, a typed `▾` and a print-in
wipe. All 11 native selects moved over (invite expiry, ownership transfer with avatars, keybind
action, composer code language, ban duration / purge / timeout, slowmode, retention presets, channel
permission targets with role dots / avatars / override hints) plus the Audio tab's hand-rolled device
list (an unplugged saved device shows as "Not connected"). The user menu's "Move to…" became an
in-menu expanding list — a dropdown inside a context menu would stack a second popup (and trip the
menu's outside-click). Verified: tsc web 0; a harness rendering the component with real input passed
20 interaction checks (keyboard open / move / skip disabled / choose, focus return, type-to-jump,
Escape isolation, outside click, disabled click, filter + Enter, upward flip inside the window);
screenshots in all seven themes (design review artifact shared with the owner, approved). Live pass
over the real settings / channel / server modals pending.

**Client: hovering a picker's bottom row no longer scrolls the list (2026-10-10) ✅** — Owner report:
resting the pointer near the last visible entry scrolled the dropdown down. Hover made the row active
and the keep-active-in-view effect (meant for the keyboard) revealed the half-visible row; Chromium's
synthetic mousemove after the scroll made the next row active, and so on to the end. Now only the
keyboard, opening and the filter scroll the active row into view (`activeBy` ref in `Select.tsx`).
Verified in the harness: pointer parked on the bottom row → scrollTop stays 0 (was revealing the row);
ArrowDown still scrolls to the end; the 20 interaction checks pass; tsc web 0.

**Client: pickers no longer close when something else scrolls (2026-10-10) ✅** — Caught in review (a
side-agent note, confirmed in code): the picker closed on *any* scroll outside its list, but the
settings / server / channel modals render over a still-mounted chat, and `RealMessageList` writes
`scrollTop` to stay pinned to the bottom on each new message — so a busy channel could snap a dropdown
shut mid-choice. It now closes only when the scrolled element contains the trigger (a scroll container
holding it, or the page) — the scrolls that actually move it. Verified in the harness against the
committed and the fixed component: with a neighbouring list scrolling itself every 100 ms the old one
closed, the new one stays open; scrolling a container around the trigger still closes it; scrolling
the list itself doesn't; the 20 interaction + hover checks pass; tsc web 0.

**Client: "Channel cache" setting removed (2026-10-10) ✅** — Owner asked what Network → Channel cache
did. It was wired end to end (uiStore → `channel_cache_size` in the native config → chatStore's LRU on
every channel switch, `enforceChannelCacheSize` on change): it set how many visited text channels keep
their messages, history flags and scroll position. But since the real-DOM list every slice is capped at
150 messages, so the RAM it guarded is small, and its copy ("~1000 messages, 0.5–1 MB per channel") was
stale; it didn't touch server traffic (every viewable channel's messages arrive regardless; uncached
channels' live messages are dropped) or DMs. Owner decision: remove it. The cap is now
`CHANNEL_CACHE_SIZE = 10` in chatStore; the tab section, uiStore field, save / load and the native
config field are gone (old configs carrying the key still load — no deny_unknown_fields).
Verified: tsc web 0; cargo build + `cargo test --lib` 221; a node run of the store (ten channels stay
cached, an eleventh evicts the least recent, no enforce action left).

**Client: images stay loaded across channel switches (2026-10-10) ✅** — Owner report: images seen in
a channel reloaded after switching through a few other channels (or ping-ponging between two),
unreliably. Cause: `decibell-attachment://` responses never reach Chromium's disk cache (custom
protocol), and the community server sends no cache headers, so the only copy is Blink's memory cache —
which holds an image no element references only weakly past a few MB per page, so the next GC
(channel switches allocate plenty) drops it and the next view is a full server round trip. Measured in
an offscreen Electron harness with a counting protocol: with a GC between switches, 29 of 30 images of
the channel left behind were fetched again; without GC, none — i.e. timing, as reported. New
`features/chat/imageRetention.ts`: an LRU of `Image` holders keyed by URL (128 MB estimated encoded
budget, ~0.4 B/px for bounded thumbnails, `sizeBytes` for full-size fallbacks), fed from ImageItem's
`onLoad` and VideoItem's server poster; the holder joins the in-flight / finished request, so it costs
no fetch. Cleared on sign-out; nothing survives a restart (owner: not wanted). Verified with the real
`AttachmentList` in the harness, GC between switches: control build (retention stubbed) 29/30 refetched
on A → B → A and after six more channels; with retention 0/30 both; over-budget entries are released
(30/30 refetch when each claims 20 MB); tsc web 0.

**Client + native: dead cache code removed (2026-10-10) ✅** — Owner request, after the attachment
caching audit. Gone: the Tauri-era `decibell-asset://` scheme (`registerProtocol`, `cacheDir`, its
privileged-scheme entry) and the `media-cache` folder it served — nothing in the renderer used it;
the `cacheDir` / `cache_dir` boot option Electron passed to native, which native stored and never read;
and native `voice_caps_cache` + `PeerCaps`, filled from every channel's presence update (cleared and
refilled each time, so it held the last-updated channel's members, not ours) and never read — the
renderer's voiceStore carries the same caps for watch gating. Also: upstream attachment fetches (the
attachment protocol, the loopback media server, encrypted-attachment chunks, downloads) now use
Fetch `cache: "no-store"` (`electron/main/noStore.ts`). The community server sends no cache headers,
so Chromium stored those responses (61 on the owner's machine) but could never reuse them; the
renderer's own retention covers reuse. Verified: Electron honours the mode (a no-store response is
never stored — checked against a cacheable local server: 2 server hits vs 1 by default); tsc web 0 /
node 0; napi build (index.d.ts no longer has cacheDir); `cargo test --lib` 221.

**Client: reply redesign (2026-10-11) ✅** — Owner: replies looked "sloppy and unpolished". Before: the
quote sat inside the text column (so the avatar lined up with it, not the author), showed `@username`
(no nickname), raw markup / shortcodes / GIF CDN URLs, everything in one 11.5px muted line, and a
deleted-parent notice styled like a name; the composer said only "Replying to @x". Now
(`features/chat/ReplyPreview.tsx`): the reply line sits above the avatar + name, indented to the text
column, with a 2px spine curling from the avatar's centre line into a 16px avatar, the parent's server
nickname in identity colour (sender face, so mono under console) and a one-line snippet through
MessageText's preview mode (no markers, Twemoji, "GIF"/"Image" for lone media links); attachment-only
parents show kind + count + glyph; deleted parents a muted tombstone. Name opens the profile, the rest
jumps. Composer strip shows avatar, name and snippet with a divider; Esc cancels a pending reply (not
while the emoji picker is open). Owner decisions: rows where someone else replies to you get the accent
wash + left rail (server channels only — in a DM nearly every reply is to you; your own follow-ups to
yourself stay plain, fixed in a follow-up commit), the row being replied to gets the same
rail while composing, and the name/jump split. Proposal page (before/after, every theme, alternative
quote-card considered and rejected — 2–3× taller, reads as a link embed):
https://claude.ai/artifact/R625CcFWKHy6n6BhzgzazS. Scroll safety: the reply line's box is a fixed
whole-pixel height (`text-reply` = 0.9 × body with `--text-reply--line-height` rounded up to 1px; box =
max(16px, that)) and the snippet emoji scales with the text size — a content-sized 17.94px line, and a
15px emoji poking out of the 17px line at text size 13, gave rows fractional heights that Chromium's
anchoring rounded into ±0.5–1px nudges on history prepends. Long no-space nicknames truncate at half
the line instead of pushing the list into horizontal scroll. Verified in the offscreen harness with the
real RealMessageList + MessageBubble (400 rows, mixed reply kinds, dev build so the row-height audit
runs): 7 themes × text 11/14.5/17 × DPR 1/2 = 42 configs all pass — bottom pin through reply-mode
toggles, zero anchor drift on 80-row prepends mid-scroll (parents arriving), no row height change on
nickname/avatar arrival or parent edits, no post-mount settles, no horizontal overflow at 340px with a
32-byte nickname. Visual drift on prepend (rect-measured) is now 0 in nocturne at every size/DPR (old
build: up to 0.875px at non-default sizes) and ≤ old in console. Seen while testing, pre-existing and
untouched: if a web font first loads after a list mounts at the bottom, the bottom pin is lost by the
font's height delta (harness-only so far — the app loads fonts before any channel mounts); message
bodies use `break-all`, so words split mid-word at narrow widths. tsc web 0.

**Central + community + client: user statuses (2026-10-12) ✅** — Owner: online / away / do not
disturb / offline statuses, managed by central. Design: `docs/superpowers/specs/2026-10-11-user-status-design.md`.
Owner decisions: Invisible airtight (communities told too), auto-away after a user-adjustable idle
time (default 10 min, or never) plus screen lock / sleep, DND = incoming calls arrive silently, statuses
visible to everyone. Central: `users.presence_status` (+ `dm_friends_only`, see below) read at login
before the user is announced; per-session idle flag; `PRESENCE_DELTA` per visible change instead of a
full snapshot to every socket on every login/logout (snapshot once at login; older clients without
`LoginRequest.presence_deltas` keep snapshots); presence now reaches authenticated sessions only (it
went to pre-login and community-sync sockets too); a reconnect no longer flickers the user offline
(kick after `username_` is set, `leave()` announces only the last *visible* session); invisible users
are offline for friend lists and call INVITEs. Found while closing Invisible's leaks: friends-only DMs
lived only in the session — reset on every reconnect (the client only sent it on toggle), and its
connected/offline difference let a DM probe see through Invisible — now saved on the account and
re-pushed by the client after login. Community: `CommunityAuthRequest.presence_hidden` +
`PRESENCE_VISIBILITY`; rosters report `is_online` = has a non-hidden session, upserts only on a visible
change (an invisible member's connect/disconnect is silent; new/renamed members still get an offline
upsert). Client: `presenceStore` (central truth, community flag only before the first snapshot; replaces
`chatStore.onlineUsers`), shaped `StatusDot` (disc / moon / bar / ring) + `StatusPill` at every presence
site, status menu on the user panel, `useAutoAway` (`powerMonitor.getSystemIdleState` polled 15 s,
lock/suspend pushed), DND skips the call ring + taskbar flash. Verified: community e2e 404/0 incl. 10 new
`test_presence_hidden` checks; central main.cpp + auth_manager.cpp syntax-checked against real Boost /
jwt-cpp / protobuf headers with a pqxx stand-in; native napi build + `cargo test --lib` 223 (2 new);
tsc web 0 / node 0; dots, pills, menu and member grouping captured in six themes. Pending: Hetzner
central rebuild + community release + live two-account test; idle/lock detection on KDE Wayland.

**Client: status dot + reply name polish (2026-10-12) ✅** — Owner feedback on 0.8.9. (1) Reply-line
names cut off at ~10 characters: the cap was `max-w-1/2` of a button that is only as wide as its
content, so a short snippet made the cap tiny; now a fixed `max-w-56` (≈24 characters at the default
size; still fits the 340px narrow-window layout). (2) The offline / invisible ring's hole sat at a
different spot per user: it was a nested box at `inset: 26%` of a ~6px canvas, which Chromium snapped
to whole pixels independently of the disc — a 1–2px blob, jagged and off-centre at 125% scaling. The
glyphs (disc, moon, bar, ring) are now one SVG in a 20×20 viewBox, so the geometry stays centred
wherever the dot lands; compared old vs new pixel-for-pixel at 1× and 1.25× with fractional row
density. (3) Dots 2px larger everywhere (`.avatar-dot` 11→13px, DM list 10→12, DM header 10→12,
profile popup 14→16, profile card 20→24, status menu 10→12). tsc web 0.

**Client + native: auto-away on Linux Wayland (2026-10-12) ✅** — Owner report: a 1-minute idle timer
never turned them Away (KDE Plasma Wayland). Measured: Electron's `powerMonitor` reports 0 s idle /
"active" forever there, native Wayland and XWayland alike, and KDE's org.freedesktop.ScreenSaver answers
GetSessionIdleTime with "not supported on this platform". New native `system_idle.rs` +
`system_idle_state` command: ext-idle-notify-v1 from the compositor (KWin offers v2 → input-idle
notification, unaffected by video inhibitors), one watcher thread per threshold polled with a 1 s
timeout so a threshold change replaces it; GNOME via org.gnome.Mutter.IdleMonitor; screen lock via
ScreenSaver GetActive. The renderer asks native first and falls back to powerMonitor on "unsupported"
(Windows, macOS, X11). Verified on this KDE Wayland box: 5 s threshold → "idle" after 5 s with no
input (powerMonitor: 0 s throughout), threshold changes swap the watcher and leave one thread; cargo test
223, tsc web 0. GNOME path untested.

**Central + client: custom status messages (2026-10-12) ✅** — Owner: a custom status message under
the user's name in lists, in their profile popup and on their profile page, "like Discord". Design:
`docs/superpowers/specs/2026-10-12-status-messages-design.md`. An emoji and up to 128 characters, set
from a new first row in the status menu (edit by clicking it, × clears it). The dialog uses the app's
emoji picker and offers Clear after Today / 4 h / 1 h / 30 min / Don't clear.

- **Where it shows:** server member list, friends rail, friends page (in place of "Online"), user panel,
  DM header, profile popup and profile page.
- **DM list:** rows keep their last-message preview.
- **Visibility:** the message rides presence (`UserPresence.status_text/_emoji`), so others see it only
  while the user is visible, and Invisible stays airtight with no new path.
- **Central:**
  - `SET_STATUS_MESSAGE_REQ` (153) is normalised: one line, bidi controls and malformed UTF-8 dropped,
    trimmed, 128 code points, emoji ≤ 32 bytes. The client mirrors this rule for its own copy, and the
    two agree on 15 tricky cases.
  - Three `users` columns. An expired message is cleared at the login read or by the 30 s sweep, which
    then sends a delta.
  - `LoginResponse` echoes the message and the seconds left.
- **Row heights:** pinned so a message appearing never changes them; members stay 48px and the friends
  rail 44px, measured.

Verified:
- community e2e 404/0 with the new proto;
- central syntax-checked (pqxx stand-in) with only the known warnings;
- native napi build + `cargo test --lib` 223 (proto round-trips extended);
- tsc web 0 / node 0;
- every surface captured in nocturne and matinee, and the dialog's save path driven end to end in the
  harness.

Pending: Hetzner central rebuild, then a live two-account test.

**Community + client: Members, Roles and Audit Log redesign with server-side search (2026-10-12) ✅** —
Owner: rework the three server-settings tabs to look "modern, polished and pleasing" in every theme
before 0.8.10. Approved from a proposal page (https://claude.ai/artifact/E1oE5pNjeSF9XLKiBbj8vL) with
three decisions: a Members section inside each role (yes), search on the **server** rather than over
the loaded pages, and unsaved role edits shake the save bar instead of being dropped.

- **Members:** search + role filter + counts toolbar; Member / Roles / Joined columns; role chips whose
  dot turns into ✕ to remove, a + popover to add; one ⋯ menu per row (also right-click) instead of five
  buttons per row (76 on a 16-member page); nickname edited in place; a new kick / ban / timeout dialog
  (timeout duration chips).
- **Roles:** sticky hierarchy list (drag to reorder, Alt+↑/↓, locks on roles at or above yours), editor
  with Display (swatches + a live chat preview), Permissions (Switch rows, search, Administrator
  callout, "manage each other" under Hierarchy) and Members (holders + a multi-select add picker). Edits
  are a draft behind a sticky save bar; the old editor put Save 1,523px down.
- **Audit Log:** day-grouped timeline with tone tiles and a rail; a readable sentence for each of the 25
  actions the server writes, decoding `role:N`, `allow/deny` bitmasks, voice_mod verbs, storage bytes
  and the details grammar into chips; action-group filter + search.
- **Wire:** `MEMBER_SEARCH_REQ/RES` (154/155): every member whose username or nickname contains the
  query (ASCII case-insensitive) and/or who holds a role (`role_id` > 0, or -1 = no roles), online or
  not, by username, paged, `seq` echoed. Its own packet so results never land in the sidebar roster.
  `AuditLogRequest.query/actions/seq`: one cached statement (`instr` for the verb list, LIKE with the
  user's `%`/`_` escaped) matching actor, target, action, details, the channel's name and the nickname /
  role name behind actor or target (`role:<id>`, `member:<user>`). `RoleInfo.member_count`, re-pushed
  with the role list on role assignment and on every member removal, so counts include offline members
  the sidebar never loaded. Search shares the `query_bucket_` (20 / 4 per s); the client debounces
  typing by 250 ms and drops answers to superseded seqs. An older community never answers, so the tabs
  say so after 8 s instead of spinning.
- **Client:** `tabs/useMemberSearch.ts` (debounce, seq, roster deltas patch the loaded pages: replaced,
  dropped when they stop matching, slotted in when they start), `tabs/settingsUi.tsx` (icons, search
  field, viewport-clamped popover with arrow-key menus, role chip). Moderation gates read the record's
  own role ids (`useHierarchy().canManageRecord`) since a search hit may not be in the roster.
- **Fixed on the way:** `text-white` on accent / error fills (1.8:1 on Nocturne's brass, 1.4:1 on
  Console's mint, 2.8:1 on Console's coral) in the role and nickname Save buttons, the moderation
  confirms and `ConfirmModal`'s danger button → `on-accent` / `on-error`.

Verified:
- community e2e 423/0, with 19 new checks (search by name / nickname / role / no role, paging, seq,
  literal `%` and `_`, nickname / role-name / channel-name audit matches, action filter, filtered
  paging, member_count + its push on assignment);
- native napi build + `cargo test --lib` 223;
- tsc web 0 / node 0;
- the real tabs driven in the offscreen harness against a stub server (search, role filter, paging,
  live upsert / remove, role holders + picker, audit search / filter / no match, the save-bar shake,
  the no-answer message), in all seven themes for the proposal.

Pending: the community server must be rebuilt with (or before) the 0.8.10 client, since the tabs need
the new search; then a live test, including drag-reorder with a real mouse.

**Client: custom role colors (2026-10-13) ✅** — Owner: an RGB picker with hex entry as the rightmost
role color option. The role Color row ends in a custom tile: a color wheel with a +, or the custom color itself (checked)
once the role has one that isn't a preset. It opens `tabs/ColorPicker.tsx`, a popover with a
saturation / brightness field, a hue strip, a hex field (focused and selected on open, so a pasted code
replaces it) and R / G / B fields. Every change applies live to the draft, so the list dot, the
preview and the save bar follow while you drag. Six-digit hex applies as typed, three digits on Enter
or blur (so typing `33aaff` never flashes `#3333AA`); the sliders and R / G / B take arrow keys (Shift
= 10). Pure black is stored as `#010101`, because 0 means "no color" on the wire. Client-only: the
server already accepted any 24-bit color.

Verified: tsc web 0; in the offscreen harness with real mouse and key input (drag in both sliders,
6-digit, 3-digit + Enter, arrows, black, Escape closes only the popover, outside click closes it,
read-only roles disable the tile), all seven themes, DPR 1.25.

**Client: auto-away comes back faster (2026-10-14) ✅** — Owner: on KDE, going Away worked, but coming
back Online took a while after input. `useAutoAway` polled every 15 s in both directions; the OS has
no "input resumed" event for powerMonitor, and the Wayland watcher's `resumed` only lands in an atomic
that the poll reads. It now polls every 15 s while active and every 1 s while away. Return takes
about a second on every platform; going Away still lags the threshold by up to 15 s. Overlapping polls
(an unlock polls at once) keep one timer chain. A poll is cheap: one IPC plus GetLastInputInfo, or one
atomic read plus the lock-flag D-Bus call. Central only hears changes, well inside its 1 per 2 s status
bucket.

Open: Windows didn't go Away at 1 minute after ~3 idle minutes. Windows uses the unchanged powerMonitor
path (GetLastInputInfo), and nothing in Decibell injects input, so it needs a check on that machine for
what keeps resetting Windows' last-input time.

Verified: tsc web 0.

## 5. Suggested order of work

1. **Stop-the-bleeding (crash + stall + identity):** A1 (attachment NULL fp), C2 (username-reuse role inheritance), A2 (ban-purge fan-out), I1/I2 (reconnect stream/relay ownership), R1 (UDP handler try/catch). Small, high-value, verifiable against the standalone build + e2e harness.
2. **Make moderation actually moderate (unblocks the feature work):** M1 (timeout kicks from voice/stops stream), M3 (per-user slowmode + session cap), M4 (decide timeout-vs-powers rule), D1 (create_channel cache). These are prerequisites for the roles/management/moderation build — a control that's bypassable isn't a feature. (M2 dropped — intentional.)
3. **Hardening the growth path:** H1 (storage quota), DB1 (busy_timeout), X2/X3/X4/I3 (attachment re-checks, reason cap, info leaks), P1/P2/P3 (lock-held encryption + hot-path DB).
4. **Central for multi-operator:** S1 (per-community credential — do this before third parties onboard), S2/S3/S4/S5.
5. **Client seam:** CL1 (voice keying), CL3 (reconnect teardown), CL2 (JWT refresh), CL4/CL5.
6. **Then the new features** (the actual ask): categories already exist; next parity gaps are message edit/reply/pins/reactions/mentions/typing, per-channel read state, and richer roles UI — all sit on the D8/D9 design smells from the 2026-08-21 review (immutable message rows, slug-based channel ids).

**Welcome banner actions: Invite friends, channel topics, Manage channel (2026-10-14) ✅** — Owner: the
empty-channel banner's three buttons did nothing. Design:
`docs/superpowers/specs/2026-10-14-welcome-banner-actions-design.md`.
- **Invite friends** opens `InviteFriendsModal`, which lists accepted friends who aren't members. Invite
  creates a *personal* invite and DMs its `decibell://invite/<CODE>` link, which renders as the usual
  invite card. Personal invites (`InviteCreateRequest.for_user`) are always single-use and never expire
  as sent. Only that user can redeem one; anyone else gets "This invite was sent to someone else."
  without using it up. Asking again returns the creator's live personal invite instead of minting
  another. The server refuses one for an existing member or for yourself. `request_id` is echoed on the
  response so each reply reaches its caller, since rate-limited requests are dropped without an answer.
  Membership is judged on the loaded roster. Owner decision: the friend list never goes to a
  community, whose host could log it, and the server isn't asked to filter it. A first cut sent it via
  `MemberSearchRequest.usernames` and was withdrawn before release. On a big server an offline member
  the roster didn't load can still be listed. Inviting them is refused with
  `InviteCreateResponse.already_member`, and the row reads "Already here". Gate: CREATE_INVITE or
  MANAGE_INVITES.
- **Channel topics:** `ChannelInfo.topic` / `ChannelUpdateRequest.topic` (optional), text channels,
  MANAGE_CHANNELS, schema v13 (`channels.topic`, `invites.for_user`). One line of ≤ 1024 UTF-8 bytes,
  trimmed and clamped on a character boundary. It shows in the header after the name (truncated, with a
  tooltip). It is edited from the banner's Edit topic dialog or a new Channel topic field in channel
  settings, and audited as `topic`. Topics stay in the clear even in encrypted channels, like channel
  names (owner decision). Both editors say "Topics aren't encrypted. The server host can read them." on
  encrypted channels.
- **Manage channel** (was "Set rules") opens channel settings. It shows for MANAGE_CHANNELS or
  MANAGE_ROLES, like the sidebar gear. The banner leaves out actions the user can't take.

Verified: community e2e 447/0, with 24 new checks (topic set / trim / clamp / broadcast / denied / voice ignored /
channel list / clear / audit; personal invite bound / reused / refused for members (flagged
already_member) and self / wrong user can't redeem or spend it / request ids echoed on success and
refusal; v12 → v13 migration); native napi build + `cargo test --lib` 223; tsc web 0.

Pending: rebuild the community server with (or before) the client; a live two-account test of the
invite DM → card → join, and of the header topic in every theme.

**Encrypted text channels: chosen at creation, fixed for life (2026-10-14) ✅** — Owner asked whether
switching E2EE on an existing channel was fragile; it was:
- **Switching off:** an edit of a sealed message went out in plaintext. `ChatPanel` sealed by the
  channel's current flag, and `edit_message` rewrote the envelope, erasing the attachment keys stored
  in it for good. Key escrow also stopped filling keys for new members, so they couldn't read the
  sealed history.
- **Switching on:** plaintext history stayed under the lock badge, and members without encryption
  were locked out mid-conversation.
- **Either way:** sends and uploads in flight at the moment of the switch bounced or were silently
  dropped.

Owner decision: encryption is chosen when a channel is created and is fixed for its life.
- **Wire:** `ChannelCreateRequest.encrypted` (5), text channels only, audited as `channel_create`
  with `encrypted`. `ChannelUpdateRequest.encrypted` was removed (`reserved 9`), along with the
  server toggle and `set_channel_encrypted`.
- **Client:** an "End-to-end encrypted" switch in the create-channel dialog (default off, "can't be
  changed later"). Channel settings shows the state read-only.
- **Legacy guard:** for channels toggled before this change, the server refuses a plaintext edit of
  a row with an envelope, and the client doesn't offer Edit on those messages (including ArrowUp).

Verified: community e2e 449/0 (the encrypted-channels test now runs on a channel created encrypted, plus
checks for voice-ignores-the-flag, settings updates keeping it on, and the legacy edit guard); native
napi build + `cargo test --lib` 223; tsc web 0.

**Client: topic edited in channel settings only (2026-10-14) ✅** — Owner: drop the welcome banner's
Edit topic button and keep the topic in channel settings. The banner now offers Invite friends and
Manage channel. Manage channel opens settings, which has had the Channel topic field (with the
encrypted-channel warning) since topics shipped. `EditTopicModal` and `uiStore.openEditTopic` are
gone. Verified: tsc web 0.

**Client: channel settings redesign (2026-10-14) ✅** — Owner asked for text and voice channel settings
that look polished and match the other menus. They approved the proposal page
(https://claude.ai/artifact/DvAitWEoGZRXH5h4Tq2kXT) and answered its three questions.
- **Window:** `ChannelSettingsModal` now uses the Server Settings chrome: a sidebar with the channel's
  identity, tabs Overview / Retention (text only) / Permissions, and Delete at the foot like Leave
  Server. It's one 2,259px scroll no longer. The tabs live in `features/servers/channelSettings/`.
- **One draft, one save bar:** rename, topic, slowmode, retention, bitrate and permission edits are
  held together and saved by the RolesTab bar, with a dot on each tab that has changes. After Save
  the drafts stay until the server's update matches them (5 s fallback), so fields don't flick back,
  and a refused edit stays visible. **Owner: Esc, ✕ or a click outside with unsaved changes shakes
  the bar** instead of dropping them.
- **Permissions** take the Roles layout: every role / member with overrides listed with allow / deny
  counts (@everyone apart, roles at or above yours locked), a deny / inherit / allow segmented
  control per bit, and a + picker that searches members on the server (`useMemberSearch`) instead of
  the loaded roster. **Owner: bits that don't apply are hidden** — text channels no longer list
  Connect / Speak / Stream / Mute / Deafen / Move, voice channels no longer list the text bits;
  categories keep all 14 and stored values are untouched. The category switch is `Switch.tsx`.
- **Wipe** moved to the end of Retention. **Owner: delete and wipe confirm in `ConfirmModal`, with no
  typed name** for any channel type.
- Fixes on the way: retention menus were live for Manage-Roles-only users and never saved; the
  confirm buttons hard-coded `text-white` on the error fill.

`ChannelPermissionsSection.tsx` is replaced by `channelSettings/PermissionsTab.tsx`. Same IPC calls as
before; no proto or server change. Verified: tsc web 0 / node 0; offscreen harness captures in all
seven themes; save → echo and save → timeout driven through the real components with the outgoing
calls checked. Pending: a live run against a community.

**Client: settings saves settle on the server's echo (2026-10-14) ✅** — Owner asked for the channel
settings save behaviour everywhere a settings tab saves. Before, most saves dropped the draft as
soon as the request was sent. Fields flicked back to the old value for a round trip, and an edit the
server refused just vanished.
- **One helper:** `features/servers/saveEcho.ts` (`useEchoSave`). After Save the draft stays and the
  bar reads "Saving…" until the store shows what was sent. A refusal ends the wait at once, and
  otherwise it ends after 5 s. In both cases the draft stays for another try. Edits made while a save is
  out survive the echo as a fresh draft. Reset is disabled while saving.
- **Roles:** save and create both settle on the role-list push. A new role is selected by the id in the
  create reply, not by matching its name.
- **Overview:** the name / description draft follows the server until edited, so another admin's
  change no longer wipes your typing. The Public listing switch shows the flip at once and holds it
  until the meta push (it used to snap back for a round trip).
- **Members:** the nickname field stays open until the member's record shows the new name. Role
  toggles show at once and the next toggle builds on the pending set. Before, two quick toggles sent
  the second set without the first, so a role was lost.
- **Storage:** the headroom save already settled on its reply; it now gets the 5 s fallback too.
- **Channel settings** moved onto the helper. A refused rename or overwrite now ends the wait at once.
  A refused topic / slowmode / retention update (`channel_updated` failure, which nothing surfaced)
  now shows its message.
- **Byte caps typed, not truncated:** names, nickname and description clamp to the server's UTF-8
  byte caps as you type (`lib/utf8.ts` `clampLine`, caps in `features/servers/limits.ts`). Line
  breaks become spaces, as for topics. Otherwise a server-side clamp would never echo, and the draft
  would linger after the wait.

Renderer only; no proto or server change. Verified: tsc web 0. In the offscreen harness against the
real components:
- Roles: save → echo, refusal, timeout, typing during a save, and create → reply → list.
- Overview: echo, refusal, Public listing echo and timeout, multi-line description.
- Members: two quick role toggles (second call carries both), refusal, and nickname → echo.
- Channel settings: save → echo, timeout and refusal.

Pending: a live run against a community.

**Client: unsaved server settings hold the window (2026-10-14) ✅** — Owner asked for the channel
settings close behaviour in Server Settings too. Esc, ✕ and a backdrop click closed the window, and a
tab switch unmounted the tab, dropping an unsaved role, server name / description or headroom edit
without a word.
- **Guard:** `features/servers/leaveGuard.ts`. The open tab reports its unsaved changes
  (`useLeaveGuard(dirty)`); a close or tab switch while they exist shakes that tab's save bar and
  swaps in "Save or reset your changes first." A role switch in Roles still shakes the same bar.
- **One save bar:** `SaveBar` in `tabs/settingsUi.tsx`, used by Roles, Overview, Storage and channel
  settings. Overview's inline Save and Storage's inline Save moved into it, with a Reset. A shake
  belongs to the changes it guarded; the next edit's bar starts calm.
- **Storage:** any headroom text other than the stored value counts as a change, a half-typed or
  blank number included, so Save explains what's wrong instead of the edit vanishing.
- **Esc in a dialog** inside the window (role delete, Leave Server) now closes only the dialog. It
  used to close the whole window as well.
- Not guarded: the Members nickname field (Esc there closes the field, as before) and the ownership
  transfer form (a confirmation, not a draft).

Renderer only. Verified: tsc web 0. In the offscreen harness against the real components: Overview
Esc / ✕ / backdrop / tab switch shake, Reset then Esc closes; tab switch held while saving, goes
through after the echo; Roles Esc, role switch and tab switch shake, the next edit starts calm; Esc
in the role-delete and Leave Server confirms closes only the confirm; Storage edit → shake → save → echo, blank value
→ "Invalid value"; Esc in the nickname field closes only the field; channel settings shake on Esc.
Screenshots in nocturne and matinee.

Pending: a live run against a community.

**Client: settings windows keep still when a change arrives (2026-10-14) ✅** — Owner feedback on the
settings windows:
- **Empty space:** channel settings' Overview and Retention were capped at 672 px (`max-w-2xl`) and
  left-aligned. Permissions fills the pane, so they left a wide empty strip on the right (at 1920 px
  the pane is 1284 px wide). They now span the pane like Permissions and the Server Settings tabs.
  The error banner lost the same cap.
- **Scrollbar on the save bar's arrival:** the bar entered the flow when a change was made. When the
  content almost filled the pane, it pushed the tab 44 px past it (1100×700, channel Overview). The
  scrollbar that appeared took 10 px and narrowed everything above. Two changes:
  - `SaveBar` now always holds its space wherever the draft can be edited (`show` prop). The bar
    appears inside space that is already there, so the tab's height never changes.
  - Both settings windows' scroll panes reserve the scrollbar gutter (`scrollbar-gutter: stable`), so
    the content width doesn't change between a tab that scrolls and one that doesn't.

Renderer only. Verified: tsc web 0. Harness at 1920×1080, 1280×800, 1100×700 and 1100×640: making a
change in channel Overview, server Overview, Roles and Storage leaves the scroll height and content
width unchanged, and the content width is the same on every tab. Screenshots in nocturne and matinee.

**Client: settings windows fit the smallest app window (2026-10-14) ✅** — Owner: channel Permissions
"looks terrible" at the smallest window; should the minimum change? Proposal (artifact
XmC9pGcsshn4ujHYxrYt1A), approved with three decisions: keep the 800×600 minimum, switch at 1024 px,
labels per icon.
- **Why not a bigger minimum:** with the old frame, the two-column tabs need about 1080 px. That's
  wider than half of a 1920 monitor (960) and Discord's own minimum (940). At 800×600 the frame used
  352 of the 800 px on margins and the sidebar, and the Roles / Permissions editor got 116 px and
  scrolled sideways. Members cut names to one letter.
- **Shared frame:** `components/SettingsWindow.tsx` (backdrop, window, sidebar, tab item, foot
  action, pane header, body). Settings, Server Settings and Channel Settings were three hand-copied
  versions of it. Settings now also holds the scrollbar gutter, like the other two.
- **Below 1024 px (`max-lg:`):** a 16 px margin instead of 56 / 48. The sidebar becomes a 64 px rail
  of the same icons; a label shows beside an icon on hover or keyboard focus, over the pane. The
  server or channel name moves beside the tab's title. The unsaved-changes dot sits on the icon's
  corner. Leave Server / Delete Channel stay at the foot as icons. The list in Roles and channel
  Permissions narrows from 224 to 192 px.
- **Result:** editor column 116 → 420 px at 800×600 and 316 → 620 px at 1000×640. No sideways scroll
  at 800, 1000, 1023, 1024, 1080 or 1280 px. At 1024 px and wider nothing changes: the 1280×800
  captures before and after are pixel-identical.

Renderer only. Verified: tsc web 0. The proposal captured four screens at two sizes in all 7 themes.
After porting, the harness reran on the tree: leave guard (Esc / ✕ / backdrop / tab switch, confirms,
Leave Server), save-on-echo (roles, create, overview, public listing, members, channel saves) and the
width measurements. The label on keyboard focus is in the build but untested: the offscreen window
never has focus.

Pending: a run in the real app, resizing across 1024 px with a settings window open.

**Slowmode in the composer (2026-10-14) ✅** — Owner asked for an indicator and a countdown by the
message box in slowmode channels, only for members it applies to. Before, the only sign was a
"· slowmode 30s" suffix on the placeholder. A send inside the window went out, the server refused it,
the bubble vanished, and the typed text was lost with a toast.
- **Indicator:** an hourglass pill beside the composer's buttons (`features/chat/SlowmodeIndicator.tsx`).
  While you may post, it shows the interval ("30s"). After a message, it counts down ("0:24") in the
  accent, and the send button dims. Hidden for the owner and anyone with Manage Messages in the
  channel, which mirrors the server's bypass. The tooltip and `aria-label` spell it out.
- **Tracking:** `features/chat/slowmode.ts`. The echo of any message of ours, from this device or
  another, starts a full window. It runs a little after the server's, which stamps it at persist.
- **Sends held:** inside a window, or while a message of ours is still unconfirmed, Enter, Send and the
  GIF picker keep the text and attachments and shake the pill.
- **Wire:** `ChannelMessageRejected.retry_after_seconds` (new field 4, community→client). The server
  fills it on a slowmode refusal, and the client sets the countdown from it. That covers a window the
  client didn't see, such as after an app restart. Every other refusal sends 0.
- **Refused text comes back:** on any `CHANNEL_MSG_REJECTED`, the bubble's text goes back into the
  message box if nothing new was typed there (`draftsStore.restoreChannelDraft`). The toast says so.

Verified: tsc web 0, napi build, cargo test --lib (223 passed), community build + e2e (451 passed).
The two new e2e checks: a slowmode refusal carries 1..3 s for a 3 s window, and a rate-limit refusal
carries 0. In the harness, as a plain member: idle pill, a send before the echo held, the echo starting
0:30, a send during the countdown held with the text kept, ticking, a refusal restoring the text with
0:24, sending again after expiry. No pill for the owner, a moderator, or a channel without slowmode.

Needs the community server rebuilt for the countdown after a refusal. Until then the field arrives
as 0, so the text still comes back but no countdown starts. Live test pending.

**Message search, server + native (2026-10-14) ✅ — UI awaiting the owner's proposal OK** — Owner
asked for search over text channels (threads later) with sender, date, content, file and channel
filters, without heavy load on the server. Spec: `docs/superpowers/specs/2026-10-14-message-search-design.md`.
Owner decisions: encrypted channels get filters only (their text isn't searched, and the results say
which channels were skipped); a Discord-style flow (header box, filter chips, results panel); a
proposal page before the UI is ported; backend identity is the uid.
- **Index (schema v14):** FTS5 `messages_fts(content, files)`, contentless with `contentless_delete=1`
  and `unicode61 remove_diacritics 2`. Triggers keep it current on send, edit, delete, wipe, channel
  delete, retention, ban purge and attachment bind. Only plaintext rows of plaintext channels are
  indexed. Deleting is a tombstone by rowid, so the cost that got the old triggers dropped
  (re-tokenising every deleted row) is gone. It needs SQLite ≥ 3.43 with FTS5. Without that, the
  triggers are removed and searches answer "isn't available".
- **Search worker** (`src/community/search.{hpp,cpp}`): one thread with its own read-only connection
  (WAL), never the DB mutex. The io thread only validates, resolves readable channels and senders, and
  hydrates ≤ 50 rows by primary key. Every query walks an index in id order: FTS rowid, `(sender_uid, id)`,
  attachments `message_id`, `(channel_id, id)`, or the primary key. Date bounds become id bounds by a
  binary search over the primary key. Relevance ranks the newest 2,000 matches with bm25.
- **Bounds:** 3 s per query (progress handler); counting stops at 10,000 or 250 ms past a full page
  ("N+"); one query at a time server-wide with up to 32 queued; one per session (a new search
  supersedes, a disconnect cancels); `search_bucket_` 6 burst / 1 per s, answered with
  `retry_after_ms`, never dropped; pages cut at 1.5 MB.
- **Wire:** `MESSAGE_SEARCH_REQ/RES` (156/157, payload 158/159). Community-only and client-first safe.
- **uid, not username:** `messages.sender_uid` (back-filled from `members.uid`, set from the JWT on
  every send, and back-filled for a pre-Theme-A member on first auth). The sender filter, editing and
  deleting your own messages, and the ban purge now go by account. An encrypted message sent under an
  earlier name can't be edited, since its envelope binds that name.
- **Native:** `search_messages` → `message_search_received`. Pages with a sealed row go through the
  crypto worker (`ChannelJob::Search`), which opens each row with its own channel; plaintext pages
  skip the queue. The history and search paths share `open_stored`.
- **Fixed alongside:** `AttachmentPayload` never carried `encrypted`, so the renderer never merged the
  real metadata of an encrypted-channel attachment or registered its key (since be3f9d9).

Measured on a synthetic 1M-message community, localhost round trips:
- **Typical searches:** rare word 0.9 ms, common word 24 ms, relevance 51 ms (1,066 ms before the
  newest-2,000 pool), date-only 1–2 ms (32–62 ms before a `min(id), max(id)` full scan was split).
- **Slowest:** a common word plus one sender, 213 ms, nearly all of it the exact count.
- **Index size:** +29% database (+66 MB on 230 MB), built in about 5 s.
- **Send path:** 12 → 34 µs per insert.

Verified: community build + e2e 507 passed (50 new search checks + 6 in a v14 migration test), napi
build, cargo test --lib 223 passed, tsc web 0.

Pending: the renderer UI (the proposal goes first), a community rebuild, and a live test. The encrypted
attachment fix needs a live check in an encrypted channel.

**Message search UI (2026-10-14) ✅** — The renderer side of message search, ported from the
approved proposal (https://claude.ai/artifact/UZfpnk8sLX8eXfWodykBej, "design looks great"). Owner
answers: float the panel over the chat below 1024 px; search every readable channel by default; keep
no recent searches.
- **Search box** in the text channel header (`features/search/SearchBox.tsx`, Ctrl+F): typed filters
  become chips (`from:` with server-side member lookup, `in:`, `has:`, `before:` / `during:` /
  `after:` with a calendar). The menu keeps focus in the input; Enter searches, Tab completes,
  Backspace removes a chip.
- **Results panel** in the member list's place (`SearchResultsPanel.tsx`): count, sort, notices for
  skipped encrypted channels and partial results, grouped results with the matched words marked
  (`MessageText` gained a `highlight` option), compact attachments, infinite scroll, Jump
  (`features/chat/goToMessage.ts`, moved from the Transfers code). Below 1024 px it floats over the
  chat and Jump closes it; closing keeps the results, so the same search reopens without a request.
- **State** (`searchStore.ts`): seq-matched answers, an 8 s no-answer message for older servers,
  automatic retries on `retry_after_ms`. Nothing is stored on the device.
- History and search share `mapStoredMessage`; `SegmentedControl` options can be disabled with a
  tooltip; three icons were added to the settings set.

Verified: tsc web 0. The harness ran the real tree: chips by keyboard, Backspace, Enter, paging to
the end, Esc closing, reopening without a request, and an empty `localStorage` after searches. The
proposal captured every state in all 7 themes.

Pending: a community rebuild, then a live test, including results from an encrypted channel.

**Community server: built-in SQLite (2026-10-14) ✅** — Search's index needs SQLite ≥ 3.43 (FTS5
contentless_delete), and the Hetzner box runs Debian 12, which ships 3.40, where the server would
switch search off. `cmake/DecibellSQLite.cmake` now fetches the SQLite 3.53.4 amalgamation (the
version every search test and the benchmark ran on), checks its SHA-256, builds it with FTS5, and
links it into `decibell_community` statically. `-DDECIBELL_SYSTEM_SQLITE=ON` uses the system
library instead and fails at configure time below 3.43. Central is unchanged (Postgres). Verified: the
module on its own (download, hash, FTS5 and contentless-delete calls) and the community server linked
against that exact static library with e2e 507 passed. The full CMake configure needs the Hetzner
box's libpqxx and jwt-cpp, so it runs there for the first time. Operators: a sqlite3 CLI older than
3.43 can read the database but can't write to `messages`, because the index triggers fail.

**Search: ı matches i, and an "inside words" option (2026-10-14) ✅** — Owner asked for both, the
second only if it isn't hard on the server.
- **ı ↔ i:** unicode61 treats the dotless ı as its own letter, so "kisa" missed "kısa" and "cagri"
  missed "Çağrı". The index now stores ı as i (search_index_version 2, rebuilt once at startup;
  version 1's `search_fts_v14` flag is retired), and queries fold the same way, on the server and in
  the client highlighter. Send cost: 44–47 µs per insert with or without it, within noise.
- **Inside words** (`MessageSearchRequest.match_within_words`): "abc" finds "xabcy". Options measured
  on 1M messages: a trigram index with positions (+104% disk, sends 15 → 390 µs), one without
  positions (+36%, 14 → 490 µs, and longer substrings need Unicode re-checks), or a scan. Chose the
  scan: free until used, then bounded like every search (worker thread, 3 s budget, one at a time).
  The worker registers `decibell_fold` (case, Latin accents, ı/İ, Greek and Cyrillic capitals) and
  folds both sides, so they always agree. Punctuation is kept (`c++`), file names count, exclusions
  work, encrypted channels are skipped and named, and results sort newest first. Measured through
  the server: 4–29 ms with a sender or channel filter, 7 ms for a common fragment, 434 ms for a rare
  one across the whole server, 635 ms worst (no match, full scan).
- **UI:** a "Match → Inside words" switch row in the box's menu, shown as a `match: inside words`
  chip; fragments are marked inside words in the results; Relevant is off in this mode.

Verified: community build + e2e 526 passed (16 new checks for ı and inside words, 3 for the index
rebuild), napi build, cargo test --lib 223 passed, tsc web 0. In the harness: the switch row, the
chip, `matchWithinWords` sent, fragment marks, and "kisa" marking "kısa".

**Channel header spans the members list (2026-10-14) ✅** — Owner asked for the header with the search
box to run across the whole client, with the members list starting under it, as in Discord. The header
moved out of `ChatPanel` into `features/chat/ChannelHeader.tsx` (with the members toggle). `MainLayout`
now stacks it above a row of chat plus members or search results. Below 1024 px the floating search panel
starts right under it (`top-0` of that row). The chat's media sizing no longer subtracts a header it
doesn't contain: `chatViewSize` reads 780 × 682 at 1280 × 800, as before. The DM view is unchanged.
Follow-up (owner): the members toggle sits left of the search box, and the box ends the header at the
right edge.
Verified: tsc web 0. Harness captures in Nocturne, Matinee, Graphite and Console Split with members,
with search results, and at 1000 px.

**Central refuses unknown names and uid-less tokens (2026-10-15) ✅** — These are the two present-day issues
from the uid audit (`docs/superpowers/specs/2026-10-14-uid-identity-audit.md`). The owner asked for them
fixed first, before the uid refactor.
- **Messages for names nobody holds.** DIRECT_MSG never checked that the recipient exists. The DM gate
  allowed an unknown name and `insertDm` stored the message, so anyone could leave messages for a name
  that whoever registers it later would find waiting.
  - Now the recipient is resolved with `AuthManager::resolveUid` (> 0 found, 0 unknown, −1 DB error)
    before the gate. An unknown name gets "This user doesn't exist." back with the sender's nonce; the
    client withdraws the bubble and shows that text in a toast (`useDmEvents`, unchanged).
  - `markDmRead` and `registerMembership` check that the account exists before writing, so a read cursor
    or a server list can't be stored ahead of a registration either.
  - Calls store nothing, and an INVITE to an unknown name was already answered PEER_OFFLINE.
- **Tokens with uid 0.** `authenticateUser` read the uid in a second query, and on a DB error it signed the
  JWT with uid 0, which sends communities down the name-only path. `getCredentials` now reads
  `uid, password_hash` in one query, and no token is signed without a positive uid. The timing
  equalization for unknown names is unchanged.

Verified by inspection and a g++ `-fsyntax-only` build of `auth_manager.cpp` and `main.cpp` against a
declaration-only pqxx stand-in (jwt-cpp, nlohmann, boost and the generated pb from the community deps).
Needs the Hetzner central rebuild.

**Users by uid, stage 1: central (2026-10-15) ✅** — The owner's rule is that backends key users by uid,
so a rename never breaks a feature. Design: `docs/superpowers/specs/2026-10-15-uid-identity-design.md`
(stages, wire rules); the audit it follows is `2026-10-14-uid-identity-audit.md`.
- **Schema (migration 1).** Central gained a `central_schema` version row; migration 1 runs once, in
  one transaction, and logs its row counts.
  - `users` is keyed by `uid` (username stays unique).
  - friends, read state, E2EE keys and backups, and community memberships moved to new uid-keyed
    tables, with foreign keys: `friendships`, `dm_read_cursors`, `e2ee_keys`, `e2ee_backups`,
    `community_memberships`.
  - `dm_messages` gained `sender_uid` / `recipient_uid`, and its indexes moved to them. The stored
    names stay as the names each row was sent under, which a v1 envelope binds; `e2ee_keys.signed_name`
    does the same for bundle signatures.
  - The old tables are kept as `legacy_*` until the deploy is verified. DMs left for names nobody held
    are deleted.
  - A failed migration now stops central from starting, instead of serving on a half-built schema.
- **Sessions** carry the uid from login, and every SessionManager lookup matches on it. The DM gate
  asks `areFriends` instead of loading the recipient's friend list.
- **Wire** (additive):
  - Every central message that names a user gained a `*_uid` field.
  - Requests prefer the uid. The name is the typed-name fallback, so today's clients and communities
    keep working.
  - Payloads carry current names. DM rows with an envelope also carry `sealed_sender` /
    `sealed_recipient`.
  - LoginResponse returns the account's uid.
- **New central e2e harness** (`src/server/tests/`). Central had no automated tests, and every change
  was verified by inspection. Now it builds against libpqxx 7.10.1 and the PostgreSQL 16 / libpq bundled
  by `pgserver` (a uv venv), and runs black-box checks over TLS. With `DECIBELL_E2E_SERVER_OLD` it also
  seeds a database with the pre-change binary and migrates it. Verified: 123 passed, including:
  - uid-addressed requests and forged uids;
  - a simulated rename: the renamed account keeps everything, and a newcomer under the old name gets
    no friends, DMs, keys, backup or memberships, and can't edit old messages;
  - the migration of seeded data;
  - 2081fb0's fixes, now exercised at runtime.

Needs the Hetzner central rebuild; take a `pg_dump` first. Communities and clients need no change for
this stage.

**Users by uid, stage 2: community (2026-10-15) ✅** — The community server keys every user reference on
the account uid (design: `docs/superpowers/specs/2026-10-15-uid-identity-design.md`).
- **Auth.** Tokens must carry a uid; every live one does. The session's state is keyed by it.
  - A changed name is learned at auth. The member row takes the new name, and the roster gets a
    `MemberRemove` for the old one plus an upsert.
  - `CLIENT_PING` is held to the per-packet membership check, so a removed member can't keep a session
    alive on pings (from the audit).
- **Schema v15** (one transaction):
  - Primary keys move to the uid on members, member_roles, bans and key blobs. Member overwrites name
    the uid in decimal.
  - Uid columns are added to invites, uploads, key epochs and the audit log. `server_meta.owner` becomes
    `owner_uid`.
  - Names stay only as "the name at the time": message and key-blob senders (a v1 envelope binds them)
    and audit actor / target.
  - **Legacy rows.** A name with no uid gets a negative placeholder in `legacy_claims`: a member who
    hasn't connected since uids arrived, a name-only ban, a personal invite for a never-seen name, or a
    fresh server's configured owner. The first token with that name claims it, and every table is
    rewritten at once. A member renamed onto a name never absorbs it.
  - This replaces the old evict-by-name logic in `add_member` / `rename_member`, which could delete a
    live member's roles (from the audit).
- **Code.**
  - `CommunityDb`, `Authorizer` and every SessionManager map are keyed by uid: sessions, streams,
    watchers, thumbnails, keyframe throttle, slowmode, voice passes and permission caches.
  - Attachment HTTP reads the token's uid for membership, uploader ownership and the rate limiter.
  - Search filters senders by `author_uids`.
  - Membership sync to central sends the uid.
- **Wire** (additive):
  - About 47 `*_uid` fields; requests prefer them, and names resolve against members' current names.
  - Payloads carry current names. A sealed row also carries `sealed_sender`, the name it was sealed
    under.
  - The media plane and `stream_id` stay name-based for the client stage.
- A pointer bug the type change surfaced: the stream-presence snapshot built `stream_id` as
  `uid + "_screen"`, which compiles as pointer arithmetic once the key is an integer. Fixed; nothing
  similar found.

Verified:
- community e2e 570 passed, including:
  - v15 run on a database `main`'s binary filled, plus legacy rows, with every claim checked
    (`DECIBELL_E2E_SERVER_OLD`);
  - uid-addressed moderation, invites and search;
  - current names in history after a rename;
  - the C2 / Theme A checks rewritten for uid keys;
- central e2e 123 passed after the proto change;
- `cargo test --lib` 223 passed.

Needs the community rebuild on the Hetzner box (back up `c.db` first). Clients need no change for this
stage.

**Users by uid, stage 3: client (2026-10-15) ✅** — The client carries the uids the servers now send and
targets people by them (design: `docs/superpowers/specs/2026-10-15-uid-identity-design.md`).
- **Native.**
  - About 40 payload fields gained uids: members, messages, DMs and conversations, friends, presence,
    voice and stream presence, bans, invites, audit, mod results and calls.
  - `login_succeeded` carries our own uid, also kept in `AppState.uid`.
  - Every command that targets a person takes an optional uid.
- **Renderer.**
  - `features/users/userDirectory.ts` maps each name to the latest uid the servers sent with it.
  - Requests send `uidOf(name)`, about 45 call sites: moderation, roles, nicknames, voice moderation,
    ownership, overwrites, personal invites, DMs, friends, calls, streams and search senders. An
    unknown name sends no uid, and the server resolves the name.
  - Stores stay keyed by name, by design. E2EE and per-user voice settings are left for stages 4–5.

Verified:
- `tsc` 0 errors;
- `napi build`, whose `index.d.ts` gains only the optional uid arguments;
- `cargo test --lib` 223 passed.

A live client test against the new servers is still pending.

**Users by uid, stage 4: encryption bound to the uid (2026-10-15) ✅** — A rename no longer touches
encryption (design: `docs/superpowers/specs/2026-10-15-uid-identity-design.md` §Stage 4). The identity
keys stay; what they sign and what the key schedules bind moves from the name to the uid.
- **Formats (v2).**
  - Bundles sign the uid (`E2eeKeyBundle.binding` 2).
  - Fingerprints and safety numbers hash the uid.
  - DM envelopes use header byte `0x03` and channel envelopes `0x04`; both bind uids in the HKDF info
    and the AAD. Key blobs ride v2 DM envelopes.
  - Call auth v2 signs `call_id ‖ from_uid ‖ to_uid ‖ key`. MLS credentials are `"<uid>:<name>"`.
  - New envelopes are always v2. v1 rows open with the names the server keeps for them (`sealed_*`).
- **Rebind instead of re-key.** After login, a device holding the current key re-signs it over the uid.
  Central swaps the signature in place: same keys, same key id, so history and other devices are
  untouched. The rebind is gated on `LoginResponse.e2ee_uid_binding`, because an old central would have
  taken it for a fresh publish and forked the key ids.
  - A current v1 bundle whose signed name is no longer the account's name is treated as no keys.
- **Device.**
  - The keystore moved to `e2ee/u<uid>.json` with a uid AAD.
  - The name-keyed file migrates on first load. Its pins wait in `legacy_peers` and are adopted by uid
    when that peer next resolves under the same name.
  - Caches and pins key on the uid. Key lookups route by uid or name.
- **MLS** verifies leaves by uid and elects / removes ghosts by uid (`active_uids`). A leaf whose
  credential can't be read now counts as unverified. Before, it was silently skipped.
- **Servers.**
  - Central: migration 2 (`e2ee_keys.binding`), the rebind path, and the login flag.
  - Community: the "sent under your previous username" edit refusal applies to v1 envelopes only.
- **What users see.** Fingerprints and safety numbers change once, with no "keys changed" warning
  (pins hold the signing key, which didn't change). Old messages stay readable.
- **Compatibility.** A client from before this release can't read v2 messages, verify v2 calls or
  join v2 voice groups, so release to everyone at once after the central deploy.

Verified:
- `cargo test --lib` 232 passed (v2 formats, v1 history, bundle bindings, keystore migration and
  legacy pins, MLS credentials, session-level opening);
- central e2e 140 passed (rebind accepted in place / refused for other keys, old generations or a
  backup; binding stored and served; migration 2 on the pre-uid database);
- community e2e 573 passed (a renamed sender's v1 edit refused, v2 edit accepted);
- `tsc` 0 errors; `napi build`.

Live two-account tests are still pending: DMs both ways, history from before the switch, a call, a
voice channel and an encrypted channel. Needs the Hetzner central rebuild (`pg_dump` first); the
community rebuild is optional for this stage.

**Users by uid, stage 5: username changes (2026-10-15) ✅** — People can change their username. Every
feature follows the account (design: `docs/superpowers/specs/2026-10-15-uid-identity-design.md`
§Stage 5).
- **Central.**
  - `RENAME_REQ` is password-confirmed and follows the registration rules (now shared). It allows two
    renames a day, and a per-session bucket keeps a stolen token from guessing the password.
  - Each rename is recorded in `name_changes` (migration 3).
  - The old name is **reserved for the account for 14 days**. Nobody else can register it or rename to
    it, while the account can rename back and can still sign in with it (the response carries the
    current name).
  - `USER_RENAMED` goes to every session, only for visible users (an invisible user's rename would show
    activity).
  - `LoginResponse.renames` advertises the feature.
- **Community.** Schema v16 adds `members.name_iat`: a token older than the one that last named a member
  can't rename them back (a device holding its 24 h token across a rename). That session runs under
  the newer name.
- **Client.**
  - Settings → Account → "Change username" opens `RenameUsernameModal`.
  - Native `rename_account` swaps the token, moves the saved credentials to the new name and
    reconnects every community. It is refused while in a voice channel, because the reconnect would
    drop it.
  - `features/users/applyRename.ts` moves everything held under the old name, in place: DM threads and
    their senders, channel senders, members and owners, friends, presence, avatars, drafts, E2EE
    notices, the call peer, profile targets, and the saved per-user volume / local mute (saved again
    and re-applied).
  - Signing in with a reserved old name moves the saved credentials to the current name.

Owner decisions applied: 14-day reservation, password confirmation. Defaults I chose, open to change:
two renames a day, sign-in with the old name during the reservation, no live broadcast for invisible
users, rename refused while in a voice channel.

Verified:
- central e2e 159 passed (wrong password, rules, taken / reserved / same name, the token, the
  broadcast and its invisible exception, renaming back, the daily limit, old-name sign-in, release
  after 14 days, migration 3);
- community e2e 576 passed (an older token keeps the newer name, and its session acts under it);
- `cargo test --lib` 232 passed;
- `tsc` 0 errors; `napi build`;
- a scratch bundle exercising `applyRename` (16 checks: re-keying, unchanged references, self rename);
- dialog and Account tab captured in nocturne, matinee and console.

Live tests are still pending: a rename seen from a friend's client, DMs and history afterwards, and
the old name signing in from a second device. Needs both the Hetzner central rebuild (`pg_dump`
first) and the community rebuild (back up `c.db` first).

**Live renames: the media plane by uid (2026-10-15) ✅** — A rename is now only a database change:
voice, streams and calls carry on through it (design: `docs/superpowers/specs/2026-10-15-uid-identity-design.md`
§Live renames). Stage 5 had reconnected every community after a rename, and refused one in voice,
because communities learned names only at authentication and the media plane still named people.
- **Token renewal.**
  - Central mints a fresh token on `TOKEN_REFRESH_REQ`. The client asks every 6 hours and with a rename.
  - The client hands the token to each community as `SESSION_TOKEN_UPDATE`. The community checks it
    (same uid, not older than the last one) and renames the live session, the member row, the roster
    (held back for invisible members), and voice and stream presence, without a reconnect.
  - A connection's UDP key stays derived from the token it authenticated with.
  - Reconnects now authenticate with the newest token. Before, they reused the original one, which
    expires after 24 h.
- **Media identity = the uid.**
  - The relay stamps the decimal uid into `sender_id`.
  - Keyframe requests and NACKs target a uid (`target_id`), and stream ids are `<uid>_screen`.
  - MLS credentials and per-sender media keys are the uid.
  - P2P calls name both ends by uid (`decibell-call-v2`).
  - Native keys every per-speaker structure on it. The renderer maps uid → name where events arrive
    and sends uids in `set_user_volume`, watch / stop watching, thumbnail fetches and keyframe requests.
  - `applyRename` now also moves rosters, speaking, watches, the full-screen and mini player and
    thumbnails.
- **Fixed on the way:**
  - The relay forwarded a viewer's NACK verbatim, handing the viewer's UDP key (a session secret) to
    the streamer. It now zeroes it.
  - The first voice presence after auth carried no uids, so the MLS roster started empty.
  - A 31-byte truncation of sender names against a 32-byte AAD pad broke sealed media for 32-character
    names.
- The rename dialog no longer refuses while in a voice channel.

Verified:
- community e2e 588 passed (relay stamps the uid; keyframe and NACK by uid with the requester's key stripped;
  names are no longer media targets; audio keeps flowing through a mid-call rename on the same UDP
  key; a live `SESSION_TOKEN_UPDATE` renames roster, voice and stream in place while older, foreign or
  bad tokens, or a removed member's, change nothing);
- central e2e 163 passed (token refresh);
- `cargo test --lib` 233 passed;
- `tsc` 0 errors (web + node); `napi build`;
- a scratch `applyRename` test (22 checks, including the voice state).

Ships with the community rebuild: an older client and this relay can't decode each other's media.
Live tests are still pending: a rename mid-call and mid-stream with two clients, a P2P call, and token
renewal past 6 h.

**The renderer by uid, and no name fallbacks on the community (2026-10-15) ✅** — Owner asked to use the
uid everywhere, avatar colors included (`2026-10-15-uid-identity-design.md` §The renderer by uid). Every
renderer store that holds users keys on the account uid now, so a rename changes one entry and nothing
moves.
- **Users store.** `stores/usersStore.ts` maps uid → current name, fed by every payload that names someone
  with their uid. Views draw names with `useUserName(uid, fallback)` / `nameOf`. People never see uids: the
  fallback is the payload's name, else "Unknown user".
- **Converted:** DMs (threads, active DM, drafts, pending deletions), presence and status messages,
  friends, avatars, E2EE notices, profile popup / modal / context menu, voice and streams, calls, the
  server owner, rosters and member search, the Members / Roles / Bans / Audit Log tabs, channel overwrites
  (`member:<uid>`), ownership transfer, personal invites, the search `from:` chip and the hierarchy checks.
- **Avatar colors** hash the uid, so a rename keeps them. `useDisplayName(serverId, uid, name)`.
- `applyRename.ts` and the temporary name bridges (`legacyUid`, `uidOf`, `mediaKeyOf`, `ownUid`) are
  gone. Saved per-user volumes from before uids migrate through `uidNamed`.
- **Fixed on the way:**
  - Channel messages dropped `senderUid` in all three paths (live, history, search). The name bridge hid
    it. `Message.senderUid` is required now, and own-message checks, grouping and replies compare uids.
  - A profile popup or modal for a community placeholder (a legacy name with no account) offered DM and
    friend actions that couldn't reach anyone. They're hidden now.
- **Community: request names are labels only.** The client always sends uids, so the community dropped
  every name fallback: key-blob recipients, watch / stop watching, thumbnail fetches, the codec notify,
  `resolve_member` (kick, ban, timeout, nickname, roles, voice moderation, overwrites, transfer), unban,
  personal invites, bans of a never-seen name, and search senders by name. Native moderation commands
  take a required uid. Central keeps its typed-name lookups.

Verified:
- community e2e 588 passed, including the v14 migration and a new check that a kick naming its target
  only by name is refused (the harness now targets by uid; the "from: non-member by name" check is gone
  with the feature);
- `cargo test --lib` 233 passed; `napi build`; `tsc` 0 errors (web + node);
- offscreen captures of the real chat, members list, DM, friends page, profile popup and context menu,
  with a live `user_renamed`: every view relabels, colors stay, the list re-sorts, and nothing is logged
  to the console.

Central is unchanged. This ships with the community rebuild and the client release, all at once.

**Renames stop announcing and keeping old names (2026-10-15) ✅** — A side review found that renames gave old
names away. Checking it showed the leak reached further than reported.
- **Old name in the notice.** `USER_RENAMED` told everyone "old name → new name". Clients hold people by
  uid now, so the old name served nobody. It's gone (`reserved 2` in the proto, dropped from the native
  event too).
- **Audience.** `SessionManager::broadcast()` delivered to every connected socket, including connections
  that never signed in. Anyone could watch renames, avatar changes and E2EE key changes without an
  account. It now delivers to signed-in sessions only. Pre-login and community-sync connections never
  needed these.
- **Retention.** `name_changes` kept every rename forever, although nothing reads past the 14-day
  reservation (the daily limit looks back one day). Rows past the reservation are now deleted hourly
  (from the first sweep after start) and with every rename.

Not changed: signed-in users who aren't friends, DM partners or co-members still get the notice. Presence
(uid + current name) already reaches every signed-in user by owner decision, so narrowing only the
rename notice would hide nothing. That's an owner question, raised separately.

Verified: central e2e 166 passed (no old name on the wire, a connection that never signed in hears
nothing, expired renames forgotten); community e2e 588; `cargo test --lib` 233; `napi build`; `tsc` 0
(web + node). Ships with the Hetzner central rebuild.

**Owner decisions on rename reach and invisible pushes (2026-10-15) ✅** — Answering the two questions above:
- **Statuses stay visible to every signed-in user**, so rename notices aren't narrowed to friends, DM partners
  or co-members either.
- **Profile pushes go out even while invisible**: renames, avatar changes and E2EE key changes are
  essential to clients, and revealing activity is accepted. Avatars and keys already did. Renames had
  been held back until others' next fetch, and are now announced live like anyone's. CLAUDE.md records
  this as the exception to "invisible is airtight".

Verified: central e2e 166 passed ("invisible user renamed ... and announced like anyone's").

**Audit sweep: no name left as identity (2026-10-15) ✅** — The owner asked for another review once the
servers were rebuilt. Five parallel audits read each tier with two failure modes in mind: a rename breaks
something, or a newcomer who takes a freed name inherits something. Findings, each verified in the
code and fixed (details: uid spec §Audit sweep):
- **High (community): a ban could become a ban by name, outside the hierarchy.** BAN_MEMBER_REQ accepted
  any negative uid, and `ban_active_unlocked` matched negative-uid rows against every connecting name.
  So a BAN_MEMBERS holder could lock out the owner. Bans now match by uid only, a placeholder can be
  banned only as a member, and an account banned by uid is refused before it can claim a placeholder.
  This also fixed a member renamed onto a legacy-banned name being refused.
- **High (native): pre-uid pins never adopted and kept being looked up by name.** A newcomer under a
  pinned name looked pinned, could get stuck behind "refusing to send unencrypted", and could be handed
  the old account's key. Adoption is now by identity key, and a legacy pin only opens v1 history.
- **Medium-high (renderer): stale community names overwrote current ones.** Central's names now win;
  ban, audit and invite labels no longer feed the store.
- **Medium:**
  - live avatar changes never reached anyone (native dropped the uid);
  - audit entries reached the client without uids;
  - a prepared call failed if the callee renamed while it was ringing;
  - the DM jump-to-reply fetch sent a number where native wanted a name;
  - unclaimed legacy placeholders never expired. They now end 2026-10-29; until then nobody else can
    hold a legacy member's name.
- **Low:**
  - central answered non-typed requests by name (BLOCK by a stale name hit a newcomer); now uid-only;
  - a rename's MEMBER_REMOVE + upsert became one upsert, invisible members and every session included;
  - member paging had a name-only cursor;
  - search authors by name (now reserved);
  - escrow sealed channel keys to placeholders by name lookup;
  - our own pre-uid key file migrated on a name match alone;
  - "am I the sender" fell back to names;
  - stream watchers were keyed by name;
  - logout kept the uid;
  - saved volumes were migrated by name, and all-digit names read as uids;
  - downloads labelled the sender by a stale name;
  - the OS window title showed a uid;
  - blank names where "Unknown user" belonged;
  - stale comments and types.
- **Not a name issue, fixed on the way:** logout kept the previous account's DMs, drafts and friends.

Verified:
- central e2e 168 (name-only revoke and BLOCK refused, the test suite now targets by uid);
- community e2e 592 (made-up placeholder ban refused, placeholder ban never matches by name, claims
  expire, audit uids on the wire, renames relabel without MEMBER_REMOVE);
- `cargo test --lib` 234 (key-based pin adoption); `napi build`; `tsc` 0 (web + node);
- offscreen smoke captures through a live rename, no console errors.

Central's `legacy_*` tables (pre-migration name-keyed copies, unused) stay, by owner decision: central moves
to a new machine eventually, without them. Ships with a community + central rebuild and a client release, together.

**Central: ready to scale (2026-10-15) ✅** — Owner asked to do now what keeps a later scale-out cheap
("optimized, clean and scalable, no bugs or memory leaks"). Design: `2026-10-15-central-scaling-design.md`.
- **Threads.** Central ran everything on one io thread and blocked it on every Postgres query and every
  bcrypt (cost 12, about 0.25 s per login). One login stalled everyone, and a reconnect storm after a
  restart froze central for seconds.
  - It now runs `DECIBELL_IO_THREADS` io threads, with a strand per connection.
  - Password hashing moves to a CPU pool; the connection stops reading meanwhile, so its packets stay
    in order.
  - In the login-storm check, a DM crosses with no measurable delay during 12 simultaneous logins.
- **One delivery seam.** Fan-out code walked every session reading its fields, which was only safe with one
  thread.
  - All shared state now lives in `SessionManager`: connections, signed-in accounts with their presence,
    and the watch index.
  - Sessions touch only their own fields. Deliveries and closes are posted to the target's strand.
  - `deliver_to_user` is the one way to reach an account: the place a message bus plugs in later.
- **Presence by subscription.** Every status change went to every signed-in session, and every login got a
  snapshot of every visible user, so the work grew with the square of the online count.
  - Clients now send `PRESENCE_SUBSCRIBE` with the users on screen: status hooks and `UserAvatar`
    register interest, a throttled flush sends the set, and native re-sends it after every login.
  - Central answers newly watched users with `PRESENCE_UPDATE.covered_uids`, and sends deltas, renames,
    avatar changes and key changes to watchers only (at most 5,000 watched per connection).
  - Statuses stay visible to everyone: anyone may watch anyone, only delivery changed.
  - Native forgets a newly watched user's cached bundle, so a key change missed meanwhile is caught.
  - `LoginRequest.presence_deltas` is reserved.
- **Postgres pool.** `PgPool` replaced 46 connections-per-query: idle connections are reused, broken ones
  dropped, and acquiring never blocks.
- **Fixed on the way:**
  - Concurrent key publishes of one account now take a row lock; key ids used to rely on the single
    thread.
  - Expiring a status message no longer wipes one set at the same moment (conditional clear).
  - Handler exceptions no longer kill an io thread.
  - The per-packet debug log line is gone.
  - SIGTERM shuts down cleanly (threads joined, pool released).
- **Decided for later:** DM attachments go to object storage as encrypted blobs; message ids promise only
  "64-bit, increasing over time".

Verified:
- central e2e 182 (subscriptions: covered answers, watcher-only deltas and profile pushes, a replaced set
  stops deltas, the 5,000 cap, cleanup on disconnect and on a replacing login, a login storm);
- the same suite under ThreadSanitizer (zero reports) and under AddressSanitizer + UBSan + LeakSanitizer
  (zero reports, clean shutdown). Two documented protobuf-ASan mismatches are configured around;
- warnings-enabled compile clean for the new code; community e2e 592; `cargo test --lib` 234;
  `napi build`; `tsc` 0 (web + node);
- offscreen smoke: one throttled subscribe with exactly the users on screen, no console errors.

Ships with the Hetzner central rebuild and a client release together: an older client gets no presence from
this central, and this client gets no presence from an older one.

### Fix: your own avatar after presence by subscription (2026-10-15)

Reported by the owner right after the central rebuild: their own picture showed as a letter. The client
learned its own avatar version from the login snapshot, which subscriptions removed, and it doesn't watch
itself, so nothing told it. Changing the picture had the same hole: Settings waited for the
`AVATAR_CHANGED` broadcast, which now reaches only an account's watchers. (Before, an invisible user never
saw their own picture either: the snapshot listed visible users only.)

- At login the renderer seeds its own entry (`avatarStore.seed`), and the avatar fetch fills in the version.
- `upload_avatar` announces the account's own new version (`avatar_changed`) when central accepts it.

Client-only; no server change. Verified: `tsc` 0 (web + node), `napi build`, `cargo test --lib` 234, and an
offscreen run of the real login listener: one `fetch_avatar` for the own uid and the picture in all three
places it shows.

### Offline people's pictures (2026-10-15)

Owner request, after the own-avatar fix: people who are offline (and not friends) showed a letter. Central
only ever told clients the picture versions of online users (presence) and friends (friend list), and the
client fetches a picture only once it knows the version. Pre-existing, not a subscription regression.

- **Central:** a subscription answer gains `PresenceUpdate.avatars` (`AvatarVersion {uid, version}`): the
  picture of each covered account that isn't visible and has one. Accounts not signed in are read in one
  query (`getAvatarVersions`, `uid = ANY($1)`) outside the directory lock; signed-in ones come from the
  directory. An invisible account answers byte for byte like an offline one (checked by the e2e). The
  answer is now built and posted under the lock, so a push about those accounts that comes after it can't
  overtake it (it used to be posted after unlocking, a small window for a stale answer).
- **Client:** every covered user gets a version (presence entry, `avatars`, else none). "None" marks the
  entry missing without a fetch (it used to fetch to learn that). The avatar cache was reworked for the
  extra load: at most 6 fetches in flight (newest first; a list of 100 used to fire 100 at once against a
  5 s timeout), users scrolled away before their turn are skipped, at most 400 loaded pictures of users off
  screen are kept (bytes dropped, version kept), a failed fetch is retried the next time central names
  the version, and a logout drops results still in flight.

Verified: central e2e 166 (new: offline picture, invisible = offline byte for byte, visible picture only in
the presence entry, no picture → covered only) and the same under ThreadSanitizer, 0 reports; community e2e
(proto change); `cargo test --lib` 234; `napi build`; `tsc` 0 (web + node); offscreen cache run: an answer
loads the offline picture and marks the others missing without a fetch, peak 6 in flight for 40, 16 fetches
for 30 requested with 20 scrolled away, 450 loaded → 400 after going off screen, an evicted picture
reloads, logout mid-fetch leaves nothing; smoke scenes clean.

Ships with the Hetzner central rebuild: against a central without `avatars`, this client takes every
covered user without a presence entry as having no picture, so offline friends would show a letter.

### Central hardening + load test (2026-10-15)

Owner asked whether central was ready for hundreds of thousands of users; the answer was "not yet" for
four reasons, all fixed here (design: `docs/superpowers/specs/2026-10-15-central-hardening-design.md`):

- **Reconnects no longer use the password.** A password sign-in returns a resume token; a dropped
  connection signs back in with it (a signature check and three reads, no bcrypt). Resume tokens are
  central-only by audience (`decibell_central_resume`): communities hold every member's session token, so
  one must never sign anyone into central. Central refuses an audience token per packet, and communities
  refuse any token with an audience. They last 30 days from the password sign-in; a resume echoes rather
  than extends. The client falls back to the password silently when a resume is refused, stores the new
  connection before sending its login (the LoginRes handler writes through it), and jitters its
  reconnect delays (50–150%).
- **Abuse limits** (`abuse_limits.hpp`): per address (256 open connections; 60 new then 10 a second;
  30 password sign-ins then one every 2 s; 10 sign-ups then one every 2 minutes), per account (10 failed
  sign-ins then one a minute, refused before hashing; its resume token still works), and a server-wide
  connection cap. Per connection, DMs (10 / 3 a second), reads (60 / 20), writes (20 / 1) and sign-in
  attempts (5 / 1 per 2 s) are paced: an over-rate request waits with reading paused, never dropped.
- **Open files:** central raises its soft limit to the hard one and caps connections below it (a reserve
  for the database pool); accept() out of descriptors waits 100 ms instead of spinning.
- **Token checks:** keys parsed once (one signer, two verifiers shared by all threads); a session checks
  only the expiry of a token it already verified.
- A received buffer larger than 64 KB (an avatar upload, up to 2 MB) is freed after parsing instead of
  staying allocated for the rest of the connection.

**Load test** (`src/server/tests/load.py`): central and PostgreSQL pinned to one thread of six P-cores of
an i7-12700K (6 io + 3 CPU threads; stands in for the 6-core E-2226G box), 8 Python client workers on the
other cores, real TLS connections from many loopback addresses, clients signing in by resume token.

| Connected (idle, each watching 50) | 10,000 | 20,000 | 40,000 |
|---|---:|---:|---:|
| central memory | 816 MB | 1.6 GB | 3.2 GB (82 KB each) |
| central CPU, idle | 0.00 cores | 0.01 | 0.01 |
| presence / DM latency p50 | 0.2 / 0.7 ms | 0.2 / 0.7 ms | 0.2 / 0.7 ms |

| At 40,000 online, DMs/s delivered | 3,860 | 7,620 | 15,100 |
|---|---:|---:|---:|
| delivery p50 / p99 | 1.9 / 5.6 ms | 3.1 / 24 ms | 5.6 / 17 ms |
| central / PostgreSQL CPU | 0.36 / 0.67 cores | 0.70 / 1.34 | 1.16 / 2.26 |

Plus, during traffic, ~640 presence deltas and ~320 subscription answers a second (2% of users
flipping away per minute, everyone switching view every 2 minutes); no drops at any step.

- **Restart with 40,000 connected:** all back 19.7 s after central was listening again (50% in 7.4 s),
  central using 57 CPU-s (~1.3 ms per resume, so 6 cores absorb ~4,600 a second); the Python clients
  spent more CPU than central did and set the pace. By password the same storm is ~2,340 s of bcrypt
  (17 sign-ins a second on 3 CPU threads).
- **Memory floor:** ~82 KB per connection, almost all TLS buffers: Boost.Asio's SSL stream allocates two
  17 KB record buffers and a 2 × 17 KB BIO pair per connection, whatever OpenSSL is told
  (`SSL_MODE_RELEASE_BUFFERS` measured no difference and was left out). 100,000 connections ≈ 8 GB. Going
  below needs TLS terminated elsewhere (a proxy) or a different TLS layer: later, if memory binds first.
- **Certificate:** an ECDSA P-256 certificate instead of RSA-2048 cut central's CPU per resume by 30%
  (9.0 vs 12.8 CPU-s for 10,000). Not changed: clients pin central's certificate on first use, so a new
  one shows existing clients the certificate-changed prompt; worth doing together with a release that
  ships the new fingerprint.

Verified: central e2e 215 (incl. the migration check; new: resume, refused session / junk / expired /
foreign-audience tokens, resume after a rename, account and address limits, DM pacing at 3.3 s for 20,
the connection cap, out of descriptors without spinning and recovering), the same under ThreadSanitizer
and ASan + UBSan + LeakSanitizer; community e2e (the audience check); `cargo test --lib` 234 (jittered
backoff, resume field); `napi build`; `tsc` 0; warnings-enabled compile clean.

Ships with the Hetzner central rebuild and the next client release (an older client keeps reconnecting by
password, which still works; this client against an older central gets no resume token and does the same).
The community rebuild adds the audience check.

### Cancelling resume tokens (2026-10-15)

A review note on the hardening above: central keeps no list of the resume tokens it issues (it checks only
signature and expiry), so a copied one would keep signing its account in for up to 30 days, with nothing to
cancel it short of replacing the signing key for everyone. Added before the central rebuild:
`users.sessions_valid_after` (migration 4, `central_schema` 4). A resume token whose password sign-in
predates the account's cutoff is refused; the client falls back to the password. For now it is an
operator's lever (SQL); "log out other devices" and a password change (neither exists yet) would set it and
close the account's live connections.

Verified: central e2e with the migration check (schema 4, no account starts with a cutoff; a token from
before the cutoff is refused, one from a later password sign-in works) and under ThreadSanitizer.

### Failed sign-ins per account and address (2026-10-15)

A review note: counting wrong passwords per account across all addresses let anyone who knows a username
keep that person out of password sign-in (one machine could hold about 30 accounts indefinitely: its 30
attempts a minute are enough to keep 30 accounts at one failure a minute). Owner decision: count them per
account and address (10, then one a minute), so a griefer holds only their own address, plus an
account-wide cap of 100 (then one a minute) against guessing spread over many addresses. A success forgets
its address's failures, not the account-wide count. Signed-in apps were never affected (they resume).

Verified: central e2e 221 with the migration check (10 failures hold the griefer's address, the owner signs
in from another, which doesn't release the griefer; 100 failures from many addresses hold the account,
whose resume token still works) and under ThreadSanitizer.

### Correction: the load test's DM rate was measured with PostgreSQL in RAM (2026-10-15)

The "Central hardening + load test" entry's traffic table (up to 15,100 DMs/s at 17 ms p99) ran PostgreSQL
in `/tmp`, which is tmpfs on the test machine: commits never waited for a disk flush. Rerun with the
database on the machine's NVMe (consumer Samsung 990 EVO Plus, btrfs; `pg_test_fsync` ≈ 620 flushes/s,
1.6 ms each), 10,000 clients:

| DMs/s target | delivered /s | p50 | p99 |
|---:|---:|---:|---:|
| 500 | 485 | 8 ms | 92 ms |
| 1,000 | 973 | 23 ms | 46 ms |
| 2,000 | 1,543 | 1.5 s | 4.1 s |

So central sustains **about 1,000–1,500 DMs/s** on a disk like that, bound by the disk flush each DM's
commit waits for (holding one of the six io threads meanwhile), not by CPU. The connection, memory,
presence and restart figures are unaffected (they don't commit per packet). What sets this ceiling: the
disk's flush latency (datacenter SSDs with power-loss protection flush in tens of microseconds, a 7,200 rpm
HDD in ~8 ms) and how many commits can share one flush (Postgres groups concurrent commits; six io threads
allow at most six). `load.py` now refuses a tmpfs database (`--pg-dir`).

### Central: database work on its own pool (2026-10-15)

Follows the correction above: the DM rate was bound by disk flushes, and with handlers on the six io threads
at most six commits could share one. Handlers that query Postgres now run on a database pool
(`DECIBELL_DB_THREADS`, default 64); the session pauses reading while its handler is there, so its packets
keep their order and its handlers never overlap. Sign-up, password sign-in and rename stay on the strand
(they hand bcrypt to the CPU pool). At startup central reads PostgreSQL's `max_connections` and uses fewer
database threads if they wouldn't fit next to the io and CPU threads and some room for psql and backups.

Load test, PostgreSQL on the NVMe (≈ 620 flushes/s), 10,000 clients, 6 pinned cores:

| DMs/s | handlers on 6 io threads | 32 database threads | 64 database threads |
|---:|---|---|---|
| 1,000 | p50 23 ms, p99 46 ms | p50 6 ms, p99 57 ms | |
| 2,000 | falls behind (1,543/s, p99 4.1 s) | p50 16 ms, p99 94 ms | |
| 4,000 | | p50 16 ms, p99 37 ms | p50 10 ms, p99 39 ms |
| 6,000 | | | p50 8 ms, p99 46 ms |
| 8,000 | | falls behind (6,126/s) | p50 12 ms, p99 84 ms (central 1.1 + PostgreSQL 2.9 cores) |

With the defaults at 40,000 connected (same disk): 7,478 DMs/s delivered at p50 12 ms / p99 70 ms; an
idle DM 4.7 ms (a real flush; tmpfs had shown 0.7 ms); 3.0 GB; a restart brings all 40,000 back in 24.8 s.
Ceiling on a disk like this: about 7,500 DMs/s, up from ~1,500. A datacenter SSD with power-loss
protection flushes far faster; run `pg_test_fsync` on the production box to know.

Verified: central e2e 221 (with the migration check), the same under ThreadSanitizer and ASan + UBSan +
LeakSanitizer.

### Fix: central relayed the sender's session token, and other central security items (2026-10-17)

From the pre-1.0 review (`docs/reviews/2026-10-17-pre-1.0-review.md`, items S1, S12, S13, S17).

- **S1 (critical).** DIRECT_MSG and CALL_SIGNAL forwarded a copy of the client's whole Packet, including
  `auth_token` — the sender's 24 h session token, which every community accepts. Zero-click: a call INVITE
  makes the callee's client answer RINGING on its own, and central relayed that back with the callee's
  token. Both relays now build the outbound message field by field (client fields: recipient, content,
  reply_to, nonce, envelope / call_id, keys, candidates, stream meta; the rest central-stamped), and
  `deliver_to_user` strips any `auth_token` as a backstop. Client-set reply previews and `edited_at` no
  longer cross. DMs also gain the edit's 64 KiB plaintext cap, a 64-byte nonce cap and nonce'd refusals for
  "too long" and self-DMs (they used to hang until the 30 s watchdog). **Tokens issued before this deploy
  should be considered exposed** (rotating central's signing key invalidates them all).
- **S13.** Non-INVITE call signals answered PEER_OFFLINE on failed delivery, so a CANCEL with a made-up
  call_id told offline (PEER_OFFLINE) from invisible (silence); and they skipped the block check. Now only an
  INVITE is answered, every other kind is refused when the pair is blocked (friends-only still governs only
  the INVITE: the rest of a handshake goes both ways), clients can't send central's own kinds, and candidate
  addresses are capped at 45 bytes.
- **S12.** REJECT / REMOVE deleted the pair's row whatever its status, so a blocked user could lift the
  block themselves; BLOCK's upsert handed the row to whoever blocked last. A block now belongs to whoever
  placed it: the other side's REMOVE / REJECT is a quiet no-op, blocking back makes the row `BLOCKED_BOTH`
  (both list it; each lifts only their own side), and the row is read `FOR UPDATE`. No schema change
  (`status` is free text). Unknown actions are refused instead of "Action successful".
- **S17 (central half).** The write queue was capped at 1024 frames; a client that stops reading while
  asking for 1 MiB avatars pinned ~1 GiB. It is now also capped at 16 MiB, and the overflow disconnect
  happens once.

Verified: central e2e 220 (new checks: relayed DM / invite / ringing carry no token, forged reply preview
dropped, oversized and self DMs refused with the nonce, non-INVITE probe unanswered, central-only kinds
refused, oversized candidate dropped, the block lifecycle); the same suite against the previous binary
fails the token, preview, size and block checks; ThreadSanitizer and ASan + UBSan + LeakSanitizer: 220 each,
no reports.

### Fix: E2EE downgrade and nonce-reuse holes (2026-10-17)

From the pre-1.0 review (items S6–S9); client-only, no wire change.

- **S9 — AES-GCM nonce reuse on MLS media.** Sealed audio used a counter from 1 per sealer: restarting a
  screen share inside an epoch built a new stream-audio sealer under the same K(epoch, uid, kind) and
  replayed nonces 1, 2, 3…, and two sessions of one account in a channel (allowed; they derive the same
  uid-named keys) collided from the first packet — the relay could recover the GCM key and forge audio.
  Audio nonces now come from one process-wide counter with a random 64-bit start; video `stream_salt` from a
  process-wide counter with a random 32-bit start (it was random per stream, 2^-32 collisions).
- **S8 — a central could switch E2EE and the pins off.** `LoginResponse.e2ee_keys=false` left the key store
  unloaded, so pins were never read and DMs to pinned contacts went plaintext. Native ignores the flag and
  always loads the store; the status payload's `supported` now means "signed in and bootstrapped"; renderer
  copy that blamed the central now says the keys are still loading.
- **S7 — pinned peer's call verified as "not verified".** With central reporting no identity and an
  unsigned call, `verify_peer_call_key` allowed the call even for a pinned peer (MITM by stripping
  signatures and swapping ephemerals). Now Rejected, as `seal_outbound` refuses plaintext. The
  `seal_outbound_raw` doc now says why escrow skips rather than errors.
- **S6 — historical generations weren't pin-checked.** A fetched `(uid, key_id)` bundle was only
  self-signature checked, then kept: central could forge a pinned contact's side of DMs or channel key
  blobs under a key_id nobody pinned. For a pinned peer a generation now opens only if its identity is the
  pin or one pinned before it (new `PeerRecord.previous`, filled on a key change, additive storage); an
  unknown identity is accepted only as their new current identity, which surfaces the change and re-pins.
  Otherwise the message shows `peer_key` and nothing is stored. Known limits: entries injected before this
  fix stay (history entries carry no identity); on a fresh store, history sealed under a peer's identity
  from before their last reset no longer opens once they're pinned (indistinguishable from a forgery
  without a signed key history).

Verified: `cargo test --lib` 241 (new: restarted sealers never repeat a nonce, distinct stream salts,
pins load with no central flag, pinned-without-keys call refused, a pinned peer's unpinned identity never
opens a DM or channel blob and is never stored, a key change first seen in history is surfaced and still
opens, the retired-identity list survives save/load), napi build, renderer `tsc` 0.

### Fix: relay endpoints bind only through a return-routability challenge (2026-10-17, review S11)

Any datagram with a valid UDP key moved the session's voice/media endpoint to its source, so a member
could watch every stream, then send one keyed packet from a spoofed source and aim the whole relay at a
victim (unbounded reflection). Now a keyed datagram from an unbound address only earns it a 41-byte
`BIND_CHALLENGE` (8-byte random cookie, ≤ 1 per session per socket per 250 ms, only for triggers ≥ 41 B);
a `BIND_ECHO` of that cookie from the same address binds or moves it (`src/community/udp_path.hpp`).
Nothing is relayed to an unbound endpoint; PINGs echo only there, unkeyed PINGs get nothing (the voice
port used to echo anything to anyone). Native answers inside `MediaSocket::recv` (Plain only; sealed P2P
sockets never answer) and pings at once, every 500 ms until echoed, so joining binds in one round trip and
a NAT rebinding re-binds on the next packet. Wire change on both sides (client + community ship together).
Residual: an on-path observer can still complete a challenge — the UDP key travels in clear anyway (the
open plaintext-media-plane-header item). Not yet tested live across two machines / a NAT.

Verified: community e2e 587/0 (+20: no relay before the echo; wrong / stale / other-address cookies bind
nothing; another session's key from a new socket doesn't move it; an echo from a new address moves it;
pacing, the 41-byte floor, unknown keys, the media path), `cargo test --lib` 247 on main with the E2EE
fixes, napi build.

### Fix: client security items — tokens, community ids, loopback server, link previews, saved sign-in (2026-10-17)

From the pre-1.0 review (S1 native half, S3, S4, S5, S10).

- **S1, native half.** Native put the 24 h session JWT in `Packet.auth_token` on every packet to a
  community (channel, voice, stream, MLS and key requests, the keepalive's captured copy). The community
  never reads it; it only spread a live credential into packets a server might relay or log. All 27
  community sends now pass none (COMMUNITY_AUTH_REQ / SESSION_TOKEN_UPDATE keep the token in their payload,
  where it's read). Central packets, which central checks, are unchanged.
- **S3.** `COMMUNITY_AUTH_RES.server_id` re-keyed a connection onto whatever id the server named and
  disconnected the real holder, so a hostile community could take another's messages, attachment target and
  channel keys. Native keeps central's id → host:port record (`net/directory.rs`, from memberships, the
  server list and invite resolution) and re-keys only a synthetic "host:port" connection whose address
  central confirms for that id, never displacing a client connected elsewhere.
- **S4.** The 127.0.0.1 media proxy attached the JWT and decrypted E2EE attachments for anyone (local port
  scan, DNS rebinding). URLs now start with a per-launch 128-bit token (constant-time compare), `Host` must
  be `127.0.0.1:<port>`, ids are digits only, GET only. Preload gets port + token over a sync IPC — not argv,
  which every local user can read.
- **S5.** The link-preview guard missed IPv4-mapped IPv6 in WHATWG form (`::ffff:7f00:1`), `::7f00:1`,
  `::ffff:a9fe:a9fe`, and checked before connecting (DNS rebinding). `electron/main/publicNet.ts` classifies
  with `net.BlockList` (mapped spellings, NAT64, 6to4, Teredo, site-local, documentation, reserved), and every
  hop (page, redirects, og:image probe, oEmbed) connects through a `lookup` that refuses non-public answers,
  so the address checked is the address connected. Caps, timeouts and cache unchanged.
- **S10 (owner decision: keep the stored password, locked down).** The saved password was sealed with a key
  any local process can derive (hostname + user), written 0644, with a home-made nonce, saved *before*
  central accepted it, and handed to the renderer on every boot. Now: sealed under a subkey of the
  keychain-wrapped local key (derived-key fallback when there's no keychain or Linux's `basic_text`, logged),
  random nonce, 0600 atomic write; native signs in itself (`auto_login`), the renderer only learns the
  saved username; credentials are saved only after central accepts a typed password and forgotten when it
  refuses that pair as "Invalid username or password." (rate limits / network errors keep them); old files
  migrate silently. Central now answers a database error at sign-in with its own message and doesn't count
  it (it used to say "wrong password", which would now have cleared everyone's saved sign-in during an
  outage). Also fixed: napi exported the init field as `e2EeLocalKey`, so main's `e2eeLocalKey` was never
  read and **every E2EE key store since 2026-09-03 was sealed with the derived key**; stores now open with
  the key their `protector` names and are re-sealed under the keychain key on load. If the keychain can't be
  opened at a launch, the store shows locked and the passphrase restores it (local.key is never regenerated
  on a read failure). Not done: a distinct "revoked" refusal for "log out other devices" (feature doesn't
  exist yet — until it does, a refused resume still falls back to the saved password).

Verified on main: renderer + main `tsc` 0, napi build (index.d.ts gains `autoLogin`), `cargo test --lib`
262 (re-key decisions, saved sign-in sealing / migration / forgetting, refusal matching, derived-store
re-seal), central e2e 221 (+ database error isn't "wrong password") under the normal build, ThreadSanitizer
and ASan + UBSan, no reports. Scripted Electron checks: the media server refuses rebound / localhost /
portless Host (403) and missing / wrong / truncated tokens, non-digit and traversal ids (404); link
previews refused 127.0.0.1, `[::ffff:127.0.0.1]`, `[::ffff:7f00:1]`, `[::7f00:1]`, `[::1]`, `0x7f000001`,
`2130706433`, `localhost.` and /etc/hosts names with zero connections reaching the listener, while public
pages, oEmbed, image probes and http→https redirects still unfurl. Needs an app restart to try live (main
and preload changed).

### Fix: community server security items — broadcast whitelist, channel ids, key escrow, connections, write queue (2026-10-17)

From the pre-1.0 review (S2, S14, S15, S16, S17 community half).

- **S2.** The CHANNEL_MSG broadcast was `routed = packet`: an unbounded nonce, `edited_at`, reply previews,
  appended `reply_to_attachment_kinds` and unknown fields reached every viewer — a 2 MiB nonce plus a 64 KB
  quote disconnected every viewer, 3×/s. The broadcast is now a fresh ChannelMessage: from the client only
  channel, text, envelope, reply target, nonce (≤ 64 B; clients send 20–36) and upload ids; the server fills
  the rest. Unknown / category channels, oversized and empty messages get CHANNEL_MSG_REJECTED {nonce}
  instead of silence.
- **S15.** `channel_permissions_unlocked_` cached a 0 for every unknown channel id a request named, and ids
  were never length-checked (random 2 MiB ids grew it ~64 MiB/s). Unknown channels resolve against the
  layout map and are never cached; one reflection pass per post-auth packet drops any request whose
  `channel_id` / `category_id` / `channel_ids` exceeds 36 bytes (the 32-byte slug + "-999", the longest id the
  server makes).
- **S14.** Any viewer could mint epochs without limit (plaintext channels too) and pre-fill others' missing
  epochs with permanent garbage. Now: encrypted channels only; a new epoch only when there is none, or when
  a rotation is due (a kick / ban / leave since the current epoch, tracked in memory, as the client rotates
  only on CHANNEL_KEYS_CHANGED{rotate}) and the publisher holds the current epoch; fills only from holders of
  that epoch; the epoch and its blobs in one transaction. Also fixed: a "fill" sealed to its own publisher is
  a creation that lost a race (two quick bans) — it used to be accepted, leaving the loser sending messages
  nobody could read; now refused as `stale_epoch` and the client re-fetches. Still open (protocol work):
  letting a recipient reject a bad blob, paging CHANNEL_KEYS_RES; a rotation is lost if no holder is online
  at removal time.
- **S16.** Neither listener had connection caps, RLIMIT_NOFILE stayed at the soft limit, and error paths left
  sockets in CLOSE_WAIT until the 30–90 s deadline. New `connection_limits.hpp` (central's AbuseLimits,
  simplified): per listener a server-wide cap from the fd limit (soft raised to hard at startup; chat ½,
  attachments ¼ of what's left after 256 reserved; `DECIBELL_MAX_CONNECTIONS`), per address 256 open (as
  central) and 60 + 10/s new. Every chat error path ends in `drop_connection()` (leave, close, cancel timers,
  free the queue), every attachment one in `close_now()`.
- **S17 (community half).** `Session::deliver` capped the backlog at 1024 frames only, so a non-reading client
  requesting history pages could pin ~1.5 GB, and every overflowing deliver posted another disconnect. Now
  also 16 MiB; an `overflowed_` flag posts the disconnect once, and the in-flight write keeps its own buffer.
- The ban handler now answers after its purge (the "ban purge goes by uid" e2e check raced the reply).

Verified on main with the S11 relay change: community e2e 627/0 (+40: whitelist / nonce / refusals, id caps
and no cached misses, epoch and fill gating, the lost-race refusal, connection caps and immediate descriptor
release, the byte-capped queue), and 627/0 under ASan + UBSan with no reports. 17 of the 31 targeted new
checks fail against the pre-change binary.

### Fix: key escrow — members drop blobs they can't use, key checks, paged replies, stored rotations (2026-10-17, review S14)

The rest of S14 (the server-side gating shipped in 308b3d9). Design notes in
`docs/superpowers/specs/2026-09-04-encrypted-text-channels-design.md` § Escrow hardening.

- **Blobs a member can't use.** A stored blob was kept for good (`INSERT OR IGNORE`), so a holder escrowing
  garbage, or a blob sealed to keys the member has since reset, left them unable to read the channel, with
  nothing they could do. New `CHANNEL_KEYS_REJECT {channel_id, epochs, bad}` (no reply): the server deletes
  the caller's own rows (encrypted channels only), so they show up in `needs` again, and names a filler.
  Native drops a blob as *stale* when it's sealed to a generation of the member's key older than their
  current one, and as *bad* when it doesn't authenticate, binds another channel or epoch, or fails the key
  check below. Anything else (locked, the sender's key not fetchable, a generation newer than this device
  holds) is retried, never dropped. A bad blob's sender goes into `channel_key_refusals`: their blobs to that
  member are dropped, they're never named filler on that member's behalf, and that member is left out of
  their `needs`.
- **Key check.** A blob that opened could still carry the wrong key, and a member had no way to tell (every
  message would then show as unreadable, and their sends too). A creation now carries
  `key_check` = HMAC-SHA256(key, label ‖ channel ‖ epoch), stored on the epoch (schema v17) and returned with
  every blob. A mismatch is a bad blob. Epochs minted before this have no check and open as before.
- **Orphaned epochs.** If every holder of the current epoch left or dropped it, nobody could fill or rotate
  it, and S14's gating refused everyone else a new one, so the channel was stuck for good. When nobody holds
  the current epoch (`ChannelKeysRes.current_orphaned`), any viewer may mint the next one, and the client does.
- **Paging.** `CHANNEL_KEYS_RES` carried every epoch's blob, plus up to 200 needs with unbounded epoch lists:
  past the client's 2 MiB frame cap after about 1000 rotations, which meant a reconnect loop. Now
  `ChannelKeysReq.after_epoch` / `needs_after_uid`. The first page has the viewers (≤ 1000), the needs by uid
  (≤ 200 viewers, ≤ 4096 epochs) and the first 256 blobs. Later pages carry one list, picked by the cursor,
  which the reply echoes for routing. Native walks the blob pages on every fetch, and the filler walks the
  needs pages. A full page measured under 512 KiB. Needs skip viewers who already hold every epoch, using one
  grouped count instead of a query per viewer.
- **Stored rotations.** The "rotation due" mark was in memory, so it was lost on a restart, and also lost when
  no holder was online at the removal: the client rotates only when told, and nobody ever told it. The mark
  is now a row (`channel_key_rotations`), cleared by the next epoch, and the next holder to sign in is sent
  `CHANNEL_KEYS_CHANGED{rotate}`.
- **Fillers re-check keys.** `E2EE_KEYS_CHANGED` reaches watchers only, so a filler could keep sealing to a
  member's pre-reset keys, and that member would keep dropping the result. A filler now looks each
  recipient's current keys up again before sealing to them (people known to have no keys stay cached). Fills
  are now paced at 250 ms per lookup, inside central's 5/s key bucket: the old pace was 4 seals per 250 ms
  (16/s), which central drops.
- Residual: someone dropping a *stale* blob can have garbage refilled by a holder who races the named
  filler. Stale refuses nobody, but a bad blob after that refuses the sender.

Verified: community e2e 681/0 with the v14 → v17 migration check (+29: key checks, stale / bad drops and
refusals, refused fillers and needs, orphaned epochs, all four page kinds and their sizes, channel-delete
cleanup, the owed rotation), and the same under ASan + UBSan with no reports. Native `cargo test` 264
(+2: the key check binds key / channel / epoch; every blob classification), napi build clean with
`index.js` / `index.d.ts` unchanged; central (proto only) builds and passes its e2e 221/0. Against the
pre-change community binary, 15 of the new checks fail before the paging seed stops the run (no
`key_check` column).

### Fix: channel crypto worker per community, remembered key loads, no epochs minted from the open path (2026-10-17, review A4)

- **One worker for every community.** Incoming channel messages, history pages, edits and sealed search
  pages, plaintext included, all went through one ordered worker (queue of 512), and the routers awaited
  room in it. A message under an epoch we don't hold made the worker wait on a key load, up to 6 s per
  page. Meanwhile the router that carries the key reply could be blocked on the full queue, and every other
  community's chat waited behind it. Now each community has its own worker on an unbounded queue (the
  router never waits). One community's key trouble stays in that community.
- **One load per page, not per message.** `open_message` refreshed the keyring for every message whose
  epoch key was missing, with no dedupe and no memory. A history page under an epoch we don't hold, or a
  server naming epochs that don't exist, cost a full round trip per row, and the community silently drops
  CHANNEL_KEYS_REQ past its 20 / 5 per s bucket. Now loads run one at a time per channel, and a caller that
  waited out another's load reuses its result. The open path loads at most once per channel per 10 s,
  whether the load worked or not, and CHANNEL_KEYS_CHANGED (a new epoch, or a fill on its way) resets that.
  The renderer never re-opens a row that showed `no_key`, so the window costs nothing on screen.
- **No epochs minted while opening.** A reply with `current_epoch == 0` (or, since S14, an orphaned
  epoch) made the open path create an epoch inside the worker, sealing to every listed viewer at 250 ms
  each. Only a send, a named filler or a rotation mints now. Epoch creation also sleeps only for members
  whose keys need a lookup from central, instead of 250 ms for everyone.
- **Paced key requests.** A long load (pages past the 8th) and a fill's publishes (one per epoch: a
  newcomer in a channel with dozens of epochs) are spaced 250 ms apart past a burst of 8, inside the
  community's per-session bucket. Past the burst they used to be dropped and time out.
- Also fixed: two native tests saved key stores under the same uid in the same temp dir and failed when
  they ran at the same moment.

Verified: native `cargo test` 266 (+2: the open path's load memory and its reset; `needs_lookup`), stable
over three runs; napi build clean, `index.js` / `index.d.ts` unchanged. Not live-tested: a slow or hostile
community holding up only its own chat.

### Fix: native — UDP receive loops survive every error, oversized TCP frames are skipped (2026-10-17, review A1 client half, A2)

- **A2: receive loops.** The voice and video receive threads, the STUN query and the hole punch ended on
  any receive error outside a short allowlist, so incoming voice or video died while TCP and the UI stayed
  connected. On Windows a datagram bigger than the buffer fails the read with WSAEMSGSIZE. The relay
  forwards audio up to 1245 B into a 1237 B buffer, so any member could silence every Windows client in the
  channel, and any host that knows a P2P socket's address could end call media or fail a punch. On Linux an
  ICMP unreachable on the connected voice socket (relay restart, Wi-Fi roam) reads back as ECONNREFUSED.
  New `media/udp_recv.rs`: every error is transient. Errors are logged at most every 5 s, with a count of
  the ones not logged. After 8 errors in a row the loop backs off 1, 2, 4 … 64 ms, so a socket that fails
  instantly can't spin a core. A datagram or an idle timeout ends the streak. The loops exit only on their
  stop flag. The voice loop also exits when the audio thread drops its receiver, and that thread reports
  its own crash. Receive buffers are 2 KiB, and a read that fills one is dropped as junk: nothing we send is
  that big, and Linux truncates silently where Windows fails the read.
- **Exits are reported.** The voice receive thread wasn't guarded. It now runs under `catch_unwind`, like
  the audio and video threads, and reports a panic as `voice_error`. A failed `set_read_timeout` is reported
  by both threads; the video thread used to ignore it and could block past its stop flag. A guard sets the
  voice receive thread's stop flag when the audio pipeline unwinds; before, the thread polled a silent
  socket forever. No new event: `voice_error` already shows in the user panel and the call stage.
- **Sealed receives return to the caller** after 64 datagrams in a row that they didn't surface (junk, or
  reflected peer PINGs): `recv` with an empty read, `recv_probe` with WouldBlock. Before, a junk flood kept
  `recv` looping internally, so a hang-up's join waited behind it.
- **Send paths** already ignore or count errors (`let _ = socket.send(..)`, the video sender's error
  count), so they needed no change. The TCP write task still ends on a write error, which is correct.
- **A1, client half: oversized frames.** The client treated any frame over 2 MiB as a fatal read error. It
  then reconnected, asked for the same reply and failed again, in a loop. The codec now reads such a frame
  and drops it as it arrives, so it never sits in memory, and the connection stays up. It logs a warning
  that names the frame's packet type (peeked from the leading `type` varint). A length over 64 MiB still
  ends the connection, so a hostile length can't make the client read gigabytes. A skipped reply's waiter
  times out. The native oneshot waiters (key / backup fetches, invite resolve, thumbnails) already time out
  and clean up. The renderer-side waits don't: a skipped history page leaves "Loading history…" until the
  B4 guard reset lands, and after a skipped DM_CONVERSATIONS_RES the DM list isn't loaded (it's requested
  once at login, by event). Both beat a reconnect loop. The server-side budgets are the real fix.
- Not verified live: the Windows WSAEMSGSIZE path (Linux truncates instead, so the tests can't produce it;
  it goes through the same "every error" branch) and a real Wi-Fi roam. No `cfg(windows)` code was added.
  The old Windows allowlist entry ERROR_IO_PENDING (997) is still treated as an idle read, so it never
  triggers the back-off.

Verified: `cargo test --lib` 280 on the branch, 282 on main with A4 (+16: the codec skips an oversized frame between two normal ones across
reads of 1 B to the whole stream, skips back-to-back ones, accepts a frame at the limit, refuses lengths
over the ceiling even after a skip, waits for the type peek; error classification, back-off and its reset,
a hot error loop sleeps, rate-limited logging; the voice and video loops survive ECONNREFUSED from a
"restarted relay" on loopback and deliver its next datagram; an oversized datagram is dropped; stop flag
and dropped-receiver exits; sealed junk and oversized datagrams are skipped and a flood returns to the
caller; STUN finds its answer behind junk bigger than the old buffer). With the loops' error branch made to
panic on ECONNREFUSED, both survival tests fail, so the loops do see the error the old allowlist broke on.
napi build clean with `index.js` / `index.d.ts` unchanged. No renderer files changed.

### Fix: central responses fit the client's frame cap (2026-10-17, review A1)

The central half of A1. The native client treated any frame over 2 MiB as a dead connection and asked again
after reconnecting (`native/src/net/framing.rs`; it now skips such a frame, see the native entry above), and no central response had a byte budget, so one big row
could lock a user out until someone edited the database. The limits live in the new
`src/server/wire_limits.hpp`: a list or page response holds at most 1.5 MiB of rows (`MAX_RESPONSE_BYTES`,
the community search figure), and every stored string a response repeats is capped when written and again
when read, for rows stored before the cap.

- **DM history.** `DM_HISTORY_RES` had a 200-row limit and no byte budget: 200 rows of 64 KiB bodies, or of
  body + envelope + quoted envelope (~200 KiB each), is far past the cap. A page is now cut where it would
  pass 1.5 MiB, and the cut side sets `has_more` / `has_more_after`, which the client already pages on, so a
  short page is no longer the end. An older or latest page keeps its newest rows, a newer page its oldest,
  and a window around a message grows outwards from the target, one row on each side in turn. A page always
  holds its first row (the target): the caps bound a row to ~200 KiB.
- **Old oversized messages.** DIRECT_MSG capped `content` at 64 KiB only since the S1 fixes, so rows of up to
  ~2 MiB exist. Every read now cuts a body to 64 KiB, ending in "…": SQL reads `LEFT(content, cap + 1)`, so
  such a row isn't even fetched whole, then a byte cut on a character boundary (a split character makes
  prost reject the whole packet). Reply quotes (`reply_to_content`, live and in history) are cut to 512 bytes
  the same way, so a reply to an old 1.9 MB message no longer makes a ~2 MB DIRECT_MSG for both people.
  Envelopes have been capped since they exist.
- **DM conversations.** `DM_CONVERSATIONS_RES`, sent at every login, carried every conversation with its
  whole last body: the review's 1.9 MB DM dropped its recipient at every login. It now comes in pages, newest
  first, at most 200 conversations and 1.5 MiB each: `DmConversationsReq.before_id` (the previous page's
  oldest `last_message_id`), `DmConversationsRes.has_more` and `before_id` (an echo). A plaintext preview is
  cut to 512 bytes. An encrypted last message can't be cut, and the client opens it for the preview line, so
  its envelope goes whole and counts toward the page's budget: a page of big encrypted previews holds fewer
  conversations (each ≤ ~65 KiB, so at least one). Native's DM worker asks for the next page after emitting
  one, until `has_more` is false and only while the cursor moves down; each page merges into the renderer's
  list as before. The renderer's preview row, synthesized into `messages` at hydrate, used to win over the
  server's copy in the history dedup (the newest message showed no reply quote or "(edited)" for the rest of
  the session); it is now flagged `preview` and replaced by the first history page, which matters now that its
  text can be cut.
- **Friend lists.** `FRIEND_LIST_RES` listed every friendship, and anyone can send a request: ~19k requests put
  a victim's list (fetched at every login, pushed on every friend action) past the cap. It now holds at most
  5000 rows (≤ ~1 MiB): accepted friends first, then the user's blocks, requests sent, requests received. A
  block placed on the user takes no row. An ADD is refused once 1000 requests wait for the target ("This user
  can't take more friend requests right now.").
- **Logins, the directory, invite previews.** `LOGIN_RES` listed every membership with its community's
  description, which central never capped (a heartbeat could carry ~2 MiB, from anyone holding the shared
  community secret): one community could lock every member out at every login, and the directory and its
  invite previews out for everyone. The heartbeat now clamps the name to 64 bytes and the description to
  512, the community's own caps (a name over the column's 64 characters used to fail the whole heartbeat),
  reads cut old descriptions, and a login lists at most 200 memberships, the oldest.
- **Server pictures.** Central took any size from `SYNC_SERVER_PICTURE_REQ` and broadcast its `version` to
  every member. A picture over 1 MiB (the community's upload cap) or a version over 64 bytes is now dropped,
  and a stored picture over 1 MiB is served as none.
- **Presence answers.** A `PRESENCE_UPDATE` for 5000 newly watched visible accounts with long status messages
  is over 3 MB. It now goes in parts of ≤ 1.5 MiB, each a whole answer for the accounts it covers; clients already
  apply each one on its own.
- **Backstop.** `Session::enqueue` no longer sends a frame over the client's cap (it logs one): one unanswered
  request beats a reconnect loop. Nothing reaches it now; a response that echoes a request's field back is
  smaller than the request, which central caps at 2 MiB.
- Smaller: an email over the column's 128 characters is refused as invalid instead of failing the insert as
  "Registration failed"; an invite code over 32 bytes skips the lookup.

Checked and already bounded: the fixed-text or one-name replies (`REGISTER_RES`, `LOGIN_RES` refusals,
`RENAME_RES`, `TOKEN_REFRESH_RES`, `FRIEND_ACTION_RES`, `SERVER_HEARTBEAT_RES`, `E2EE_PUBLISH_KEYS_RES`);
`SERVER_LIST_RES` (50 rows); `INVITE_RESOLVE_RES` (one row); the `DIRECT_MSG` relay and echo and
`DM_MESSAGE_EDITED` (64 KiB body, envelopes ≤ 64 KiB + 64 since they exist, now a 512-byte quote);
`DM_EDIT_RES`, `DM_DELETE_RES`, `DM_MESSAGE_DELETED`; `CALL_SIGNAL` (the S13 caps); `E2EE_FETCH_KEYS_RES`
(fixed-size keys, a name of ≤ 32 characters) and `E2EE_FETCH_BACKUP_RES` (8 KiB since the feature);
`FETCH_AVATAR_RES` (1 MiB: the cap went from 200 KB to 1 MiB and was never higher); one-account pushes
(`PRESENCE_DELTA`, `USER_RENAMED`, `AVATAR_CHANGED`, `E2EE_KEYS_CHANGED`, `SERVER_PICTURE_CHANGED`; status
text has been normalized to 128 characters and the emoji to 32 bytes since status messages exist). A reply
that echoes an unknown name or code goes to its requester only and is smaller than the request. Client
strings that reach the database: usernames (3–32 ASCII), status messages, DM bodies, envelopes and nonces,
key bundles and backups, avatars and call-signal fields were already capped; invite codes and hosts and a
community's host and fingerprint are bounded by their columns (an oversized one fails its own insert).

Verified: central e2e 250/0 (+29: history pages cut by bytes in all three modes and paged to the end, a
pre-cap 2.1 MB message and a reply's quote of it, conversation pages by rows and by bytes with envelopes kept
whole, the 5000-row friend list and the request cap, clamped heartbeats and old descriptions in logins, the
directory and invite previews, 200 memberships, picture caps, a 3000-account presence answer in parts), and
250/0 under ThreadSanitizer and under ASan + UBSan with no reports. 21 of the 29 new checks fail against the
pre-change binary, which sends 3.0 MB history, 5.0 MB conversation and 2.2 MB presence frames. Native
`cargo test --lib` 264/0, napi build clean with `index.js` / `index.d.ts` unchanged, renderer tsc 0 errors.
`load.py` not rerun: nothing here changes threads, locks or per-connection memory, and the login path only
gains a LIMIT on the memberships query; the one new per-request cost on a hot path is a size computation per
entry while building a subscription answer.

Left open: B11 (the conversations query still scans all of the user's DMs, now once per page); having native
skip an oversized frame instead of reconnecting (theme 2) is not done here. Older clients read only the first
conversation page (the newest 200); pre-1.0, client and central ship together.

### Fix: community responses fit the client's frame cap, client strings are capped, bulk changes fan out once (2026-10-17, review A1 / A2 / A3)

The community half of A1, the relay half of A2, and A3. The client dropped the connection on any frame over
2 MiB and reconnected (it now skips such a frame; see the native entry above), and a reply that was too big came back the same way after every reconnect, so one
oversized row could make a channel, the ban list or the invite list unreadable for everyone, with DB surgery
the only way out. One constant, `kMaxResponseBytes` = 1.5 MiB (search's old `kMaxPageBytes`), is now the
budget of every list and page response.

- **History pages (A1).** A page was up to 200 rows of up to 64 KB each, and every reply also carried its
  parent's whole text (another 64 KB) and, in an encrypted channel, the parent's whole envelope: ~16 replies
  to long messages and the channel couldn't be opened. Pages now stop at the budget and drop the far end,
  flagged so paging stays continuous: a latest / older page keeps the newest rows (`has_more`), a newer page
  the oldest (`has_more_after`), a jump grows outwards from the target a row per side (both flags). The
  client pages from the oldest / newest row it got, and only stops on a false flag, so a short page never
  reads as the end. A page always holds at least one row; rows stored before the 64 KB send cap are cut to
  it as sent.
- **Reply quotes.** `reply_to_content` is the first 512 bytes of the parent, cut on a UTF-8 boundary, in
  history, search and live broadcasts (the client shows one line of it). A sealed parent's
  `reply_to_envelope` can't be cut, so it is kept whole: replies to long encrypted messages keep their
  quote, it costs at most one more message cap per row, and the page budget counts it like everything else
  (a page of replies to 64 KB sealed messages holds ~12 rows instead of 50). One stored over the envelope cap
  is dropped.
- **Moderation strings.** Kick / ban / timeout reasons are clamped to 512 bytes (UTF-8 safe, control
  characters out) as received, and a ban's label for a non-member (from the request) to 64 bytes; central
  registers names of at most 32. `add_ban` clamps too, and ban rows, audit rows (target 128, details 512),
  invite labels and attachment names / types are clamped as read, so rows stored before the caps stop
  hurting. Other request strings were already capped (role / channel names, nicknames, topics, server
  name / description, invite `for_user`, message text, nonces, MLS blobs, thumbnails, search queries) or
  only echoed to the requester.
- **Ban and invite lists.** Both loaded every row. Now newest first, at most 10,000 rows read, cut at the
  byte budget, with a new `truncated` flag (`BanListResponse.truncated = 6`, `InviteListResponse.truncated
  = 4`). The Bans tab and the invite list say "Showing the most recent N bans / invites." when it's set
  (native passes the flag through). `broadcast_bans()` sent the whole list to every ban viewer on every ban
  or unban, so a raid answered with 100 bans queued 100 lists each; it is now throttled on the io thread:
  the first change goes out at once, changes in the next second go out together when the timer fires.
- **Attachments.** Upload init stored the filename and type as given, re-sent in every history page. The
  name is now capped at 255 bytes on a UTF-8 boundary and the type normalised with the serve path's rule
  (now `chatproj::safe_mime`, shared); a malformed one becomes `application/octet-stream`. Existing rows are
  clamped as read.
- **Other client strings found on the way.** A role overwrite's `target_id` was parsed with `stoll`, which
  read 5 out of `"5<anything>"` and stored the rest (up to a frame) as the row's target, re-sent in every
  overwrite list and audit row: it must now parse whole and is stored as the canonical id. Capabilities,
  stream thumbnails and codec-change notices kept fields this build doesn't know and passed them to everyone
  in the voice channel (a codec notice could carry ~2 MB at 10 burst, past a member's write queue): the
  stored capabilities drop unknown fields, and thumbnails and codec notices are rebuilt from their fields
  (`strip_client_envelope` is gone with its last callers). A key blob whose recipient name is longer than
  any name is refused (a v1 envelope binds the name, so it isn't cut), and names are bounded as read.
  Roles are capped at 250 and channels at 500 (Discord's numbers): both lists go out whole, and 1 KB topics
  on unbounded channels could have outgrown the sign-in reply.
- **Relay (A2).** The receive buffers are sized for the video struct (1,245 bytes), so audio up to 1,245
  bytes was relayed, past the client's audio-sized voice buffer (1,237): on Windows that is a receive error,
  which stopped incoming voice for everyone in the channel. Audio and stream audio (plain and sealed) larger
  than `sizeof(UdpAudioPacket)` are dropped, FEC larger than `sizeof(UdpFecPacket)` and NACKs larger than
  `sizeof(UdpNackPacket)` too; video can't exceed its struct. Keyframe requests are rebuilt by the relay.
- **Role delete (A3).** It sent one `MEMBER_UPSERT` per holder from one handler, and no write completes
  inside a handler, so a role with ~1,020 holders overflowed every session's 1,024-frame queue. The role list
  that follows is now the whole delta: clients drop role ids it no longer names from every member they hold
  (`chatStore.setRolesForServer`, and the settings member search, which restarts a "no role" search). No
  roster revision is used, so the delta chain is unchanged.
- **Other bulk fan-outs (A3 audit).** A ban's message purge sent one `CHANNEL_MESSAGE_DELETED` per message to
  every viewer (over the frame cap from ~1,000 purged messages): it now sends `CHANNEL_PRUNED` per channel,
  which clients already apply. Retention sweeps batched 2,000 ids per packet (hundreds of frames for a big
  first sweep): `broadcast_pruned` now packs 100,000 ids and 20,000 tombstones per packet (≤ ~1.5 MB). Left
  as they are, each bounded by the channel count (now ≤ 500) rather than by members or items: a category
  sync's overwrite lists (one per synced child), key-rotation notices on removal (one per encrypted channel),
  and the voice presences `broadcast_channels` re-sends (one per occupied voice channel). Member role
  assignment was already one upsert + one role list per request.
- Not done: the first `MEMBER_LIST_RES` page still carries every online member (~20,000 at typical sizes
  fit the budget); paging online members is a protocol change. Invite creation has no per-member cap.

Verified (commits 256cfb7, a1023a3, d5ecdb8): community e2e 699/0 (+44: page budgets and continuity in all
three modes, one-line and sealed reply quotes, reason / label / stored-row clamps, ban and invite truncation,
ban-push coalescing, attachment name / type caps, overwrite ids, unknown fields on caps / thumbnails / codec
notices, key blob names, role and channel caps, oversized audio / FEC / NACK, role delete and ban purge with
1,100 holders / messages, the 100,000-id prune batch), and 699/0 under ASan + UBSan with no reports. Each
commit builds; the A1 and A2 states pass their own checks. Against the pre-change binary 38 of the 45 new or
changed checks fail (the rest are controls: exact-size datagrams still relay, a short list isn't flagged, ...);
before the fix the role delete and the purge also cut off the requesting owner. Renderer typecheck 0 errors;
napi build clean with `index.js` / `index.d.ts` unchanged; native `cargo test --lib` 264/0. On main with
the native and central A fixes: community 725/0 (with the v14 → v17 migration check) in both builds, no
sanitizer reports; central 250/0; native 282; tsc web / node 0 / 0.

### Fix: a key-blob refusal steers fills, it never strands a member (2026-10-18, review S14 follow-up)

S14 made a member's *bad* verdict on a key blob refuse its sender for good: no more blobs from them to that
member in that channel, never their filler, no undo short of removal, deleting the channel or editing the
database. Against a holder escrowing garbage that's right, but a client bug that flags honest blobs as bad
(say a change to the key-check formula) would leave the member unable to get the key from that sender even
after the bug is fixed — and in a two-member channel, from anyone. Now a refusal holds only while someone else
the member hasn't refused holds the epoch (`channel_key_unrefused_holder_exists`). Then the refused sender's
blobs to them drop and the filler is someone else, as before, so a holder racing an honest filler still
can't refill garbage. When only refused senders hold it, they're named filler, see the member as a need and
may seal to them again. If that sender really is hostile, the member loses nothing: nobody else could have
given them the key.

Verified: community e2e 733/0 (+8: the only holder, though refused, is named filler, sees the need and may
seal; with a second holder the refusal applies again, the other holder is named and fills, the refused
sender's blob drops and the member leaves its needs), with the v14 → v17 migration check, and the same
under ASan + UBSan with no reports. Against the previous server the four stranding checks fail.

### Fix: the DM conversation list reads a table, not every DM the user has (2026-10-18, review B11)

DM_CONVERSATIONS (every login and resume, each page since A1) grouped all of the user's DMs:
`sender_uid = $1 OR recipient_uid = $1`, with no index leading on sender_uid, so a full scan of dm_messages
per call, and the unread count per conversation walked everything the user received above their cursor.
Right after a central restart, every resume ran one.

- **`dm_conversations`** (central migration 5): one row per pair (`lo_uid < hi_uid`) holding the pair's
  newest message id. A page reads the user's rows through the primary key's prefix and a `hi_uid` index,
  sorted by `last_id`. Triggers on dm_messages keep it current for every write path: an insert raises
  `last_id` (GREATEST, in case a smaller id commits after a bigger one); deleting the newest message falls
  back to the one before (`last_id = OLD.id`, so a message sent meanwhile wins); deleting the last removes
  the row. The migration fills it from dm_messages.
- **Unread count:** `dm_messages_unread_idx (recipient_uid, sender_uid, id)` makes it a range scan per
  conversation. It replaces `(recipient_uid, id)`, which only that count used.
- **Write cost, measured:** the first version (a row per side, `last_id` indexed) cost ~35% more PostgreSQL
  CPU at 6,000–8,000 DMs/s and doubled p99. With one row per pair, `last_id` in no index and
  `fillfactor = 70`, each DM makes one heap-only update. Load test, same machine and day, 10,000 clients,
  PostgreSQL on the NVMe:

| DMs/s | before: p50 / p99, PostgreSQL CPU | after: p50 / p99, PostgreSQL CPU |
|---:|---|---|
| 2,000 | 5.2 / 15.7 ms, 0.55 | 5.0 / 19.2 ms, 0.54 |
| 4,000 | 7.8 / 99.2 ms, 1.01 | 5.9 / 34.0 ms, 1.00 |
| 6,000 | 12.1 / 32.6 ms, 1.31 | 10.9 / 34.1 ms, 1.43 |
| 8,000 | 14.2 / 32.6 ms, 1.69 | 15.0 / 37.4 ms, 1.87 |

  Delivery keeps up at every step in both.
- **Read cost, measured** (2 M DMs, one user with 50,000 across 1,000 conversations): a page took 282 ms
  for that user and 35 ms for a light one; now 2.7 ms and 1.8 ms, with the same rows. The migration took
  5–9 s on 2 M rows.
- Also: a debug build of the client takes `DECIBELL_CENTRAL=host:port` (release builds compile it out), which
  is how DM conversation paging was checked live: the native addon signed in to a local central holding 450
  conversations and fetched pages of 200, 200 and 50.

Verified: central e2e 279/0 with the migration check from the pre-uid schema (+8: listing order, one row per
pair, unread counts, falling back on delete, the conversation ending with its last message, rows written with
plain SQL, the index; migration 5 logs its fill), the same under ThreadSanitizer and ASan + UBSan with no
reports.

### Fix: re-sync after a reconnect, every history wait ends, sends settle from history (2026-10-18, review B4)

Nothing posted, edited or deleted while a community or central connection was down reached the loaded
channels or DMs. A lost history reply left "Loading history…" up for good, a lost older page was never asked
for again, and a send whose echo was lost was withdrawn by the 30 s watchdog though it was stored, so the
user sent it twice. Since A1 the client skips a frame over 2 MiB instead of reconnecting, so a reply can also
be lost with the connection up.

- **Community: nonces in history.** History rows never carried the nonce ("never persisted"), so a refetched
  row couldn't be matched to its pending bubble. The server now remembers every accepted message's nonce for
  30 minutes (`SessionManager::remember_nonce`, in memory, at most 20 000, never on disk) and a history page
  sets it on the requester's own rows only. No field changes (ChannelMessage.nonce exists; its comment says
  so now). A restart forgets them; the client then falls back to matching text (below).
- **One merge for every fetched page** (`lib/historyMerge.ts`, both stores). A page settles optimistic sends:
  by nonce, else by our text, reply target and `sentAfterId` (the newest stored id when the bubble was
  made; the row must be newer and not already loaded), oldest bubble first, so two identical sends can't
  both claim one row. Attachment-only bubbles wait for their nonce. A page is a contiguous run, so inside its
  range a missing row was deleted and a changed one edited (unchanged rows keep their object). A newest page
  entirely above the slice replaces it: there may be a gap. An after page above the slice's newest row (or
  into nothing) doesn't connect and only settles bubbles; one from below the slice's head adds nothing
  under it. Only a windowed slice reaches the present on `has_more_after` false. Deletes awaiting an answer
  aren't put back. DM `appendNewerDm` no longer sorts a pending bubble to the top.
- **Community re-auth** (`markServerStale`, before the channel list is set): requests made on the old
  connection are abandoned; slices of deleted channels go; a slice left caught up is dropped (the next visit
  loads the newest page); one left scrolled up, or holding an unconfirmed send, goes windowed (keeps its
  place, live arrivals wait, it pages down to the present). The channel on screen is ChatPanel's, on
  `resyncEpoch`: at the bottom, the newest page merges in (or replaces across a gap) while the slice is
  windowed in the store only (`resyncing`: no pill, bottom-follow on, sends go through), so a live message
  can't land past a gap; scrolled up, it goes windowed and pages down as the user nears the bottom.
- **Every wait ends** (`features/chat/historyRequests.ts`). Each channel / DM history request is tracked until
  its reply (matched by the echoed around / after id, newest and older pages in order) or 10 s, and abandoned
  when the connection drops. The newest-page load (first visit, jump to present, re-sync) tries three times
  while connected and always ends `historyLoading`; with no answer at all the panel shows "Couldn't load
  messages" and Retry instead of "Loading history…" or the empty-channel welcome. Paginators clear their
  boundary guard on no answer and look at the edges again a second later (while connected; a failing send
  can't spin). A jump stops waiting on no answer or a window without the message, and says which.
  `RealMessageList` gained only a `recheckEdges` handle; the placement rules are untouched.
- **Echo watchdogs** ask before withdrawing: at 30 s a still-pending send waits out a reconnect (until the
  next re-sync), then pages through what was stored after `sentAfterId` (up to 5 × 100 rows). The reply
  settles the bubble through the merge; only a send the server doesn't have is withdrawn, with its text put
  back in the message box (new `restoreDmDraft` for DMs).
- **Central re-sign-in** (a `login_succeeded` for the account already signed in: resume, or the password after
  a refused resume). DM waits are abandoned; loaded conversations drop their rows and reload on open (the
  conversations refetch, already sent on every sign-in, puts the previews and unread counts back), except one
  holding an unconfirmed send, which goes windowed; the open one catches up in DmChatPanel as channels do. The
  friend list is asked for again, retried up to three times when no list comes (also at mount). Unread badges:
  `markRead` clears a count central still reports even at the same read marker, and the open conversation
  marks read again when such a count arrives (a mark-read lost to the drop left it counting forever).
- **Native:** the DM conversations walk remembers the page it's owed; one still missing after 10 s is sent
  again (three sends), and a newer request takes over. A page skipped by the codec used to end the walk.
- DM history rows carry no nonce, so DMs settle by text only. Making that exact needs
  `DmHistoryMessage.nonce` filled by central (memory, as on the community); left for the central work.
- Not changed: a send made during the brief re-sync window has its echo dropped (the slice is windowed) and
  settles by the watchdog's check after 30 s, faded until then. A channel wiped during an outage whose newest
  page comes back empty stays windowed with the pill. CL3 (every central sign-in reconnects every community)
  is untouched; the resulting re-auths go through the same re-sync. Roster paging is another fix's.

Verified: community e2e 713/0 (+6: the sender's history row carries its nonce on after and newest pages and
for its other session; another member's row and someone else's row carry none) and the same under ASan +
UBSan with no reports; no schema change, so no migration check. `cargo test --lib` 285 (+3: a conversations
page is owed until answered, only the owed page's answer settles it, a newer request takes over the retries).
napi build clean, `index.js` / `index.d.ts` unchanged. `tsc` web and node: 0 errors. The renderer has no test
runner: `historyMerge.ts` compiled on its own passed 33 throwaway checks (nonce and text settling, two
identical sends, other sessions' nonces, attachment-only bubbles, gap replacement, deletion and edit inside a
page's range, skip ids, after pages that don't connect or start below the head), and chatStore / dmStore
compiled with zustand passed 36 (re-sync with small and large gaps, live arrivals waiting, a lost echo settled
by the re-sync page and by the watchdog's after page, windowed paging to the present, markServerStale's drop /
window / leave-alone choices and epoch, jump to present keeping bubbles, pending deletes, DM text settling,
markHistoriesStale and the preview coming back, stale unread counts clearing, DM re-sync).

On main with B11: community 739/0 (normal and ASan + UBSan, with the migration check), native 285, tsc 0 / 0;
DM conversation paging checked live against a local central (pages of 200, 200, 50).

### Fix: online members page too; counts ride every roster packet (2026-10-18, review A1 follow-up)

A1 left the first `MEMBER_LIST_RES` page carrying every online member: past ~20,000 online it outgrew the
client's frame cap, the client skipped the frame, and the member list never loaded. It couldn't simply be cut,
because the sidebar took page 1 as the complete online set and counted it for "Online — N". Design:
`docs/superpowers/specs/2026-10-18-roster-paging-design.md`.

- **One sequence.** Pages run through every online member, then every offline member, each part in roster
  order, at most `limit` per page (now 1–1000, default 100) and within the frame budget; a page can cross from
  the online part into the offline part. Roster order is the sidebar's: the display name (nickname, else
  username) folded as message search folds it (`search_fold`: case, accents, ı as i), then the name, then the
  uid. The client mirrors it exactly (`features/members/rosterOrder.ts`; identical to the server's C++ on
  20,000 generated names), so a page only ever extends the list at its end.
- **Counts everywhere.** `online_count` / `total_members` on every page and, new, on every `MEMBER_UPSERT` /
  `MEMBER_REMOVE` (the counts after the change). A client can't derive them from a delta about a member it
  hasn't loaded (a flip or a nickname change look alike), and the revision gap check keeps them exact. The
  client used to bump `totalMembers` on any upsert for a uid it didn't hold, which paging would have made wrong.
- **New fields** (appended): `MemberListRequest.uids` (a lookup: up to 200 members wherever they sort);
  `MemberListResponse.online_count`, `online_complete` (the pages so far hold every online member), `extra`
  (a first page's requester row when the page doesn't reach it; a lookup's finds), `lookup_uids`;
  `MemberUpsert.online_count / total_members`, `MemberRemove.online_count / total_members`. Native maps them.
  A rate-limited `MEMBER_LIST_REQ` is answered (`success` false, echoing `first_page` / `lookup_uids`) instead
  of dropped, which used to leave the sidebar's "Loading…" stuck.
- **Roster index.** The server keeps every member in roster order in two sets, online and offline, changed only
  by `emit_member_upsert` / `emit_member_remove`, so pages and counts agree with what the deltas told clients.
  A page costs O(log members + page); before, every page (and every client's first page on every connect)
  loaded and sorted the whole membership. A kicked member leaves the counts with their `MEMBER_REMOVE`, while
  their session is still closing. It is rebuilt when another connection commits (`PRAGMA data_version`: hand
  edits, the harness's seeded rows). Role ids come from one batched query per page. A claimed legacy
  placeholder now gets a `MEMBER_REMOVE` (clients kept it beside the account's new row until a refetch).
- **Sidebar.** The roster holds what pages, deltas and lookups brought; the sidebar draws each part only up to
  its loaded edge (the last member of that part a page brought), so a member coming online past the edge is
  held, not drawn, until the pages reach them. The offline part starts once the online part is complete, the
  loading row sits at the end of what's loaded, and the headers are the server's counts ("Online — 1,234").
  First page 500 members (every online member of most communities, as before), then 200 per scroll. A gap or a
  reconnect refetches page 1 once while deltas wait; a later page whose revision isn't ours refetches too.
- **Online / offline follows the community** (`MemberInfo.is_online`), so the counts add up; central's status
  still draws the dot inside the online part (away, Do Not Disturb). Someone online on Decibell but not
  connected to this community used to show under Online and now shows under Offline: an owner decision to
  confirm (CLAUDE.md says member lists show central's status).
- **Consumers of "the roster holds every online member".** Fixed: display names (`useDisplayName` looks a
  missing member up, so nicknames show on messages, replies, search results, voice and stream views, the audit
  log), the profile popup and modal and the user context menu (`useMember`; roles, joined date, hierarchy), our
  own permissions / timeout / nickname (our row rides page 1), the Members tab's online count, the Overview
  tab's owner name, and opening Server Settings (it re-fetched page 1, which would now drop what the sidebar
  paged in). Lookups are batched per server, once per roster generation, retried three times when refused, and
  only for uids the community itself named. Documented: the transfer-ownership picker lists loaded members only
  (it already missed unloaded offline ones; a server-search picker is the fix), and Invite friends judges
  membership on the loaded roster by owner decision (no friend list to the community, so no lookups either).
  Fine: the search `from:` picker (merges a server search), settings searches (`MEMBER_SEARCH_REQ`), native
  (maps packets only). Mention autocomplete and typing indicators don't exist yet.
- Not done: the sidebar isn't virtualized (rows accumulate as it scrolls). An offline member whose rename the
  community hasn't seen yet sorts under the old name, labelled with the new one.

Verified (commits c4674a9, aaeaab3, 8c3f8e8, 938829c, 8799d77): community e2e 764/0 (+31 net, 37 new or
rewritten: the sequence's order and coverage with folded names, `online_complete` and the online → offline
crossing, counts on pages and on join / leave / kick deltas, the requester's row, deltas while partly loaded —
an unloaded member going offline or coming online, a nickname moving a member behind the edge — lookups,
answered refusals, the 1,000-member and byte caps, the placeholder swap), with the v14 → v17 migration check,
and the same under ASan + UBSan with no reports. Against the previous server 20 of the 37 fail (the rest are
controls: coverage, the caps). Renderer typecheck web / node 0 / 0; napi build clean (`index.d.ts`: the `uids`
argument); native `cargo test --lib` 282/0. Sidebar screenshots in every theme (loaded stretch, counts, the
loading row at the end of a partial online part, folded order, invisible self).

Owner decision (2026-10-18): Online / Offline follows the community's view, central's status the dot; CLAUDE.md
updated. On main with B4 and B11: community 770/0 (normal and ASan + UBSan, with the migration check), native
285, tsc 0 / 0, central builds with the proto.

### Fix: "Invite friends" asks central which friends are already members (2026-10-18)

The welcome banner's "Invite friends" judged "not a member yet" on the roster the client had loaded (owner rule:
the friend list never goes to the community, so the community can't be asked). With paged rosters a friend who
was a member but not loaded was offered an invite, which came back `already_member`. Central already holds
both sides: the friend list and every community's members (`community_memberships`, kept by the communities'
membership sync). New `FRIENDS_IN_COMMUNITY_REQ {server_id}` → `RES {server_id, success, member_uids}`:
the caller's accepted friends who are members, answered only for a community the caller is a member of
(so it reveals only mutual communities, which that community's member list shows them anyway). Read-paced
like the friend list; two indexed lookups per friend set. Native `friends_in_community` (`known: false` for a
community central doesn't list, such as a raw host:port join, or on no answer), and the modal hides central's
members plus the loaded roster's, falling back to the roster alone when central can't say.

Verified: central e2e 283/0 with the migration check (+4: friends who are members and only they; nothing about a community the caller
isn't in, also for a friend who isn't a member; both sides of the friendship), with ThreadSanitizer and
ASan + UBSan, no reports; community 770/0 with the proto; cargo test 285; napi build (the new export); tsc 0 / 0; the modal in the UI preview harness
(central's answer hides a member the roster lacks; without one the roster decides).

