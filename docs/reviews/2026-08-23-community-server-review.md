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

## 5. Suggested order of work

1. **Stop-the-bleeding (crash + stall + identity):** A1 (attachment NULL fp), C2 (username-reuse role inheritance), A2 (ban-purge fan-out), I1/I2 (reconnect stream/relay ownership), R1 (UDP handler try/catch). Small, high-value, verifiable against the standalone build + e2e harness.
2. **Make moderation actually moderate (unblocks the feature work):** M1 (timeout kicks from voice/stops stream), M3 (per-user slowmode + session cap), M4 (decide timeout-vs-powers rule), D1 (create_channel cache). These are prerequisites for the roles/management/moderation build — a control that's bypassable isn't a feature. (M2 dropped — intentional.)
3. **Hardening the growth path:** H1 (storage quota), DB1 (busy_timeout), X2/X3/X4/I3 (attachment re-checks, reason cap, info leaks), P1/P2/P3 (lock-held encryption + hot-path DB).
4. **Central for multi-operator:** S1 (per-community credential — do this before third parties onboard), S2/S3/S4/S5.
5. **Client seam:** CL1 (voice keying), CL3 (reconnect teardown), CL2 (JWT refresh), CL4/CL5.
6. **Then the new features** (the actual ask): categories already exist; next parity gaps are message edit/reply/pins/reactions/mentions/typing, per-channel read state, and richer roles UI — all sit on the D8/D9 design smells from the 2026-08-21 review (immutable message rows, slug-based channel ids).
