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

## 5. Suggested order of work

1. **Stop-the-bleeding (crash + stall + identity):** A1 (attachment NULL fp), C2 (username-reuse role inheritance), A2 (ban-purge fan-out), I1/I2 (reconnect stream/relay ownership), R1 (UDP handler try/catch). Small, high-value, verifiable against the standalone build + e2e harness.
2. **Make moderation actually moderate (unblocks the feature work):** M1 (timeout kicks from voice/stops stream), M3 (per-user slowmode + session cap), M4 (decide timeout-vs-powers rule), D1 (create_channel cache). These are prerequisites for the roles/management/moderation build — a control that's bypassable isn't a feature. (M2 dropped — intentional.)
3. **Hardening the growth path:** H1 (storage quota), DB1 (busy_timeout), X2/X3/X4/I3 (attachment re-checks, reason cap, info leaks), P1/P2/P3 (lock-held encryption + hot-path DB).
4. **Central for multi-operator:** S1 (per-community credential — do this before third parties onboard), S2/S3/S4/S5.
5. **Client seam:** CL1 (voice keying), CL3 (reconnect teardown), CL2 (JWT refresh), CL4/CL5.
6. **Then the new features** (the actual ask): categories already exist; next parity gaps are message edit/reply/pins/reactions/mentions/typing, per-channel read state, and richer roles UI — all sit on the D8/D9 design smells from the 2026-08-21 review (immutable message rows, slug-based channel ids).
