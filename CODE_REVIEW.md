# Decibell Codebase Review

**Date:** 2026-07-19 (fix pass started 2026-07-22)
**Reviewer:** Claude (11 parallel deep-read agents, findings de-duplicated and cross-corroborated)

---

## Fix progress — batch 18 (2026-08-22): relay/FTS/keep-alive perf + central B29/B30

**C++ community server — 163/163 e2e (new: UDP echo, audio fan-out with sender rewrite, 50-packet burst, server-mute drop, keep-alive across requests, FTS gone):**
- ✅ **P5 UDP relay**: both sockets user-level non-blocking; each reactor wakeup drains up to 256 queued datagrams before re-arming; all fan-outs (`broadcast_to_voice_channel`, `broadcast_to_watchers[_voice]`, keyframe/NACK relay, PING echo) use synchronous `send_to` (drop on would_block) with a reusable target vector — no per-datagram heap buffer, no per-recipient completion handler — `main.cpp`
- ✅ **P6 FTS**: the `messages_fts` table and its three triggers are dropped once (`fts_dropped_v8` meta); message writes, wipes, prunes and ban purges no longer tokenise. Re-enable with `'rebuild'` when search ships — `db.cpp`
- ✅ **P8 HTTP keep-alive**: the attachment listener serves multiple requests per TLS connection (HTTP/1.1 default; `Connection: close`, HTTP/1.0, error responses and unconsumed bodies close); a 100 MB upload no longer pays one handshake per PATCH — `attachment_http.cpp`
**C++ central server — syntax-checked against fetched libpqxx/libpq headers (the only diagnostics are the pre-existing `pqxx::binarystring` uses vs. libpqxx 8 on the mirror; the project targets 7.x); NOT runtime-tested here (no PostgreSQL):**
- ✅ **B29 ghost sessions**: `SessionManager::leave()` now closes the socket (`Session::close_connection`), so kicked / swept sessions actually end instead of staying authenticated-but-invisible — `src/server/main.cpp`
- ✅ **B30 directory + identity**: `SERVER_LIST_RES` only lists servers with a heartbeat in the last 5 minutes; `ServerHeartbeat.server_id` / `SyncServerPictureReq.server_id` let a community keep its central row (and every membership) when its public IP or port changes — central updates the row by id and evicts a stale row squatting on the new address; falls back to the `(host_ip, port)` upsert when the id is unknown — `src/server/{main.cpp,auth_manager.{hpp,cpp}}`, `src/community/main.cpp`, `proto`

## Fix progress — batch 17 (2026-08-22): server management + moderation (items 1–7)

Design: `docs/superpowers/specs/2026-08-22-server-management-moderation-design.md`. Review §4 D3/D4/D5 + B15.
**C++ community server — schema v7, 151/151 e2e:**
- ✅ In-app server rename/description (`SERVER_UPDATE_REQ`, live `SERVER_META_UPDATE`); DB is the source of truth, env only seeds (B15); heartbeat re-reads the DB
- ✅ Audit log table + `AUDIT_LOG_REQ/RES`, written by every mod/management handler, pruned after 180 days (D3)
- ✅ Timeouts (`TIMEOUT_MEMBER_REQ`, enforced in `Authorizer` for send/attach/voice/stream), ban expiry + reason-to-target + message purge (broadcast as `CHANNEL_MESSAGE_DELETED`), richer `BAN_LIST_RES`, per-channel slowmode (MANAGE_MESSAGES bypass), voice moderation (server mute/deafen enforced in the UDP relay, move, disconnect, `VOICE_FORCE_NOTIFY`), ownership transfer (D4/D5)
- ✅ New bits `VIEW_AUDIT_LOG` / `MODERATE_MEMBERS` / `VOICE_MODERATE` (1<<16..18)
**Client — renderer tsc clean, native `cargo check` clean (83 warnings, unchanged):** editable Overview + transfer card, Audit Log tab, moderation dialogs with reason/duration/purge, timeout chip + countdown bar, slowmode control + hint, richer Bans tab, voice-mod context menu + server-mute badges, `voice_force_notify` handling.

## Fix progress — batch 16 (2026-08-22): roster deltas (P1 proper)

Review §3 P1. Compatibility with older clients deliberately dropped (app not distributed yet).
**Proto:** `MEMBER_LIST_REQ{after, limit}` is now a **paged snapshot** (first page = every online member + first `limit` offline by username; later pages offline only; `revision`, `total_members`, `has_more`, `next_after`, `first_page`); new `MEMBER_UPSERT` (106), `MEMBER_REMOVE` (107), `BAN_LIST_REQ/RES` (108/109); `MemberListResponse.bans` reserved.
**C++ community server — 109/109 e2e:**
- ✅ The full-roster push (`broadcast_members`, O(members × online) per join/leave/nickname/role change, plus one `has_permission` per online user) is gone. `emit_member_upsert` / `emit_member_remove` send one `MemberInfo` to everyone — O(online); presence flips only when a user's LAST session closes; role delete upserts each former holder; bans are their own list pushed to BAN_MEMBERS holders. Every packet carries a per-process monotonic roster revision — `main.cpp`, `db.{hpp,cpp}` (`get_member`, `count_members`)
**Client — renderer tsc clean, native `cargo check` clean (83 warnings, unchanged):**
- ✅ `chatStore`: `applyMemberPage` / `upsertMember` / `removeMember` / `memberRosterMeta` (revision gap → refetch page 1); members sidebar and MembersTab page offline members in on scroll / "Load more"; `list_members{after, limit}`, `list_bans`; mod actions no longer refetch the roster.

## Fix progress — batch 15 (2026-08-22): permissions v2 — resolver, enforced bits, per-channel overwrites

Design: `docs/superpowers/specs/2026-08-22-permissions-v2-design.md`. Review §4 D1/D2/D6.
**C++ community server — compiled standalone, 96/96 e2e checks:**
- ✅ **`Authorizer`** (`authz.hpp`): `check(Action, AuthCtx{user, channel, target})` replaces the 15 inline `has_permission` / owner / hierarchy blocks; uniform reasons; `can_moderate()` is the single hierarchy rule (strictly higher assigned-role level; owner ∞; ADMINISTRATOR never bypasses) — `main.cpp`
- ✅ **Enforced bits**: `SEND_MESSAGES` (CHANNEL_MSG, attachment init), `CONNECT_VOICE` (JOIN_VOICE_REQ), `STREAM` (START_STREAM_REQ); denials answer via `MOD_ACTION_RES{action="message"|"voice"|"stream"}`
- ✅ **Per-channel overwrites**: `channel_overwrites` table (v6), `CHANNEL_OVERWRITE_SET_REQ` / `CHANNEL_OVERWRITES_REQ/RES`, Discord resolution (base → everyone → roles → member; owner/admin bypass; no VIEW ⇒ 0), cached per (user, channel) and invalidated with the role cache + on overwrite/channel changes; cascades on channel delete / role delete / member removal; escalation guard (only bits held in that channel), role-hierarchy guard, self-lock-out guard — `db.{hpp,cpp}`, `main.cpp`
- ✅ New bits `VIEW_CHANNEL` / `READ_HISTORY` / `ATTACH_FILES` (1<<13..15), default-on; v6 migration ORs them into an existing `everyone` once
- ✅ **Per-recipient channel lists** (`COMMUNITY_AUTH_RES`, `CHANNEL_LIST_UPDATE`) with `ChannelInfo.my_permissions`; channel-scoped fan-out (`broadcast_to_channel`) for messages, deletes, wipes, prunes, retention updates, voice/stream presence; attachment GET requires VIEW on the attachment's channel; history requires READ_HISTORY

**Client — renderer tsc clean, native `cargo check` clean (83 warnings, unchanged):**
- ✅ `permissions.ts`: new bits, all enforced bits now editable in the role editor, `useChannelPermission()`; `ChannelPermissionsSection` (tri-state allow / inherit / deny per role or member) mounted in `ChannelSettingsModal`; sidebar gear follows per-channel MANAGE_CHANNELS/MANAGE_ROLES; composer shows a read-only bar without SEND, attach button hidden without ATTACH_FILES, voice join blocked without CONNECT, Stream button hidden without STREAM; native `set_channel_overwrite` / `list_channel_overwrites` + `channel_overwrites_received`

**Open next:** P1 proper (delta roster events — design in the 2026-08-22 conversation notes), server rename / ownership transfer / audit log, timeouts + slowmode.

## Fix progress — batch 14 (2026-08-21): remaining review bugs + hardening

Findings: `docs/reviews/2026-08-21-community-server-review.md` §2 (B9–B27) and §3 (P1–P4).
**C++ community server — compiled standalone, 66/66 e2e checks (`src/community/tests/e2e.py`, now incl. short-timeout and fast-sweep server instances):**
- ✅ **Deadlines** (B9): 10 s TLS+auth deadline, 90 s idle deadline re-armed per frame (CLIENT_PING counts), 64 KB pre-auth frame cap, `inbound_body_` shrinks after big frames; attachment connections get a 30 s inactivity deadline (closes a stalled PATCH's `FILE*` too); both accept loops back off 500 ms on error instead of spinning on EMFILE. Knobs: `DECIBELL_AUTH_TIMEOUT_SECONDS`, `DECIBELL_IDLE_TIMEOUT_SECONDS` — `main.cpp`, `attachment_http.cpp`
- ✅ **Rate limiting** (B11): per-session token buckets by packet class (messages 8 burst/1.5 s⁻¹, signalling 10/2, queries 20/4, admin 20/2, thumbnails 6/1); throttled CHANNEL_MSGs get `MOD_ACTION_RES{action="message"}` so the optimistic bubble is withdrawn; `/attachments/init` limited per username (20 burst/0.5 s⁻¹ → 429) — new `rate_limit.hpp`
- ✅ **Capabilities cap** (B10): `ClientCapabilities` with > 16 encode or decode entries is dropped (was re-serialised into every VOICE_PRESENCE_UPDATE) — `main.cpp`
- ✅ **`prune_attachments` IN-list** (B12): tombstone UPDATE now reuses the SELECT's JOIN predicate (no per-row placeholders, no `SQLITE_MAX_VARIABLE_NUMBER` cliff); a failed UPDATE no longer unlinks/broadcasts; messages capped at 10 attachments server-side — `db.cpp`, `main.cpp`
- ✅ **`last_keyframe_relay_` growth** (B14): target resolved before the timestamp is recorded; pruned on stream stop / last session leave — `main.cpp`
- ✅ **id=0 broadcast** (B18): a CHANNEL_MSG whose insert fails is not broadcast; sender gets a failure `MOD_ACTION_RES` — `main.cpp`
- ✅ **Throwing `std::filesystem` overloads** (B19) → `error_code` overloads; `io_context.run()` wrapped in a log-and-continue loop — `attachment_http.cpp`, `main.cpp`
- ✅ `Attachment.url` is `/attachments/<id>` instead of the server's absolute path (B20); invite `max_uses < 0` → 0 and past `expires_at` rejected (B25); AUDIO relay validates `payload_size` and STOP_WATCHING from a non-watcher is inert (B26); `CHANNEL_PRUNED` batched at 2000 ids/packet (B27)
- ✅ **Permission + statement cache** (P2): per-user (effective permissions, level) cached in `CommunityDb`, invalidated on role create/update/delete, member-role set, remove/ban, owner change; owner cached in memory; prepared statements cached by SQL text (checked-out statements fall back to a one-off prepare) — `db.{hpp,cpp}`
- ✅ **Central-sync worker** (P4): one thread + bounded queue for heartbeat / invite / membership / picture sync instead of a detached thread + TLS handshake per event; joined on shutdown — new `central_sync.hpp`, `main.cpp`
- ✅ **Roster coalescing** (P1 cheap): `broadcast_members()` collapses calls within 250 ms into one fan-out — `main.cpp`
- ✅ Index `attachments(channel_id)` + one-time backfill of legacy `''` rows (P7); `DECIBELL_RETENTION_INTERVAL_SECONDS` knob

**Proto:** `CommunityAuthResponse.server_id = 10` (central-assigned id; 0 until learned).

**Client — renderer tsc clean, native `cargo check` clean (83 warnings, unchanged):**
- ✅ **Invite-joined servers keyed `host:port`** (B17): native re-keys the connection onto central's id when the auth response carries one (tearing down any stale client under that id) and reports `requestedServerId`/`host`/`port` in `community_auth_responded`; the renderer follows the re-key for the active server and `mergeServers` a tile entry so an invite-joined server shows up immediately — `net/community.rs`, `events.rs`, `useServerEvents.ts`, `types/index.ts`
- ✅ **InviteModal owner-gated** (B16) → `usePermission(MANAGE_INVITES)` — `InviteModal.tsx`

**Still open:** P1 proper (delta member events + lazy roster), P5 relay allocations, P6 FTS triggers, P8 HTTP keep-alive; then permission model v2 (§5 of the review).

## Fix progress — batch 13 (2026-08-21): community-server review fix batch

Findings + full context: `docs/reviews/2026-08-21-community-server-review.md`.
**C++ community server — compiled standalone (g++ 15, fetched headers) and e2e-tested against a live instance (43/43 checks in `src/community/tests/e2e.py`; a deliberately-reverted build fails the new ghost-session checks, so they're real guards):**
- ✅ **Deleted seed channels resurrected on restart** (High, reproduced): `ensure_default_channels_()` ran `INSERT OR IGNORE` on every boot. Now versioned + one-shot via `seed_channels_version` meta key; upgraded DBs are stamped without re-seeding — `db.cpp`
- ✅ **Synchronous TLS `shutdown()` on the shared io thread** (High): any unauthenticated peer that completed a handshake on 8085, got its response and went silent froze chat/voice/video/auth for everyone. Now `async_shutdown` with a 2 s deadline — `attachment_http.cpp`
- ✅ **Mid-UTF-8 truncation poisoned broadcasts** (High): `resize(64/32)` on role/channel names + nicknames produced invalid UTF-8 that prost rejects, making `MEMBER_LIST_RES` / `CHANNEL_LIST_UPDATE` / `COMMUNITY_AUTH_RES` undecodable for every client. New `clamp_utf8()` (codepoint-boundary cut, control chars stripped) applied in the handlers *and* the DB layer — `db.hpp`, `db.cpp`, `main.cpp`
- ✅ **Live JWT leak** (High): `CHANNEL_MSG` / `STREAM_THUMBNAIL_UPDATE` / `STREAM_CODEC_CHANGED_NOTIFY` were forwarded as verbatim copies of the client packet, and the client puts its JWT in `Packet.auth_token` on every packet → every member received the sender's bearer token with every message. `strip_client_envelope()` on all forward paths — `main.cpp`
- ✅ **Kick/ban closed only one session per user** (High): new `sessions_by_user_` index; `force_disconnect` hits every session and returns the count; membership is re-validated on every post-auth packet (one PK lookup) so a session whose member row vanished by any path is dropped — `main.cpp`. The index also replaces the O(sessions) scans in `relay_keyframe_request` / `relay_nack` / `find_session_by_username`.
- ✅ **Role assignment bypassed the escalation guard** (Med): `MEMBER_ROLES_UPDATE_REQ` now requires every *added* role to carry only bits the actor holds (mirrors ROLE_CREATE/UPDATE) — `main.cpp`
- ✅ **Switching voice channels while streaming left a ghost stream** (Med): `JOIN_VOICE_REQ` stops the stream in the old channel first — `main.cpp`
- ✅ **Kicking/banning an offline member never refreshed rosters** (Med): explicit `broadcast_members()` when `force_disconnect` found no session — `main.cpp`
- ✅ `SessionManager::leave()` is idempotent (read-error + write-error + overflow paths used to double-broadcast and wipe a second session's thumbnail); `force_disconnect` uses `close_after_flush()` (so MEMBERSHIP_REVOKED reliably arrives), which gained a 3 s hard deadline, ignores in-flight frames, and detaches via `finish_close()` — `main.cpp`

**Native Rust — `cargo check` clean, warning count unchanged (83):**
- ✅ **Reconnect loop never stopped after kick/ban/leave/rejected auth** (High): `CommunityClient.terminated` flag set on a failed `COMMUNITY_AUTH_RES`, on `MEMBERSHIP_REVOKED`, and on a successful `ModActionRes{leave}`; the read loop then retires the client (emits `connection_lost`, removes it from `AppState.communities`) instead of re-auth-and-toast every 30 s — `net/community.rs`

**Still open from that review (next batch):** B9 timeouts + accept backoff, B10 caps cap, B11 rate limiting, B12 `prune_attachments` IN-list, B14 `last_keyframe_relay_` growth, B15 env-owned name/description, client B16/B17, P1 roster deltas, P2 permission/statement cache, P4 central-sync worker; then the `authorize()` abstraction + per-channel overwrites (§5 of the review doc).

## Fix progress — batch 1 (2026-07-22): safe, self-contained wins

**Native Rust — built clean (`cargo build`), 23 video tests pass:**
- ✅ `total_packets` OOM cap + `frames_in_progress` cap (Critical #4) — `video_receiver.rs`
- ✅ `is_keyframe` `bool`→`u8` wire UB (#12) — `video_packet.rs` / `video_receiver.rs`
- ✅ FEC `group_end` `saturating_add` — `video_receiver.rs`
- ✅ `logout` now stops voice/video/audio engines + clears session state (#10, native half) — `commands/auth.rs`
- ✅ `stop_screen_share` engine teardown moved off the AppState lock — `commands/streaming.rs`
- ✅ Thumbnail send no longer holds AppState across `.await` (Critical #7), all 3 sites — `commands/streaming.rs`, `media/mod.rs`, `net/community.rs`
- ✅ `save_settings` preserves `use_av1`/`use_h265` (kills the codec-toggle clobber, native-side) — `commands/settings.rs`

**Renderer — typechecks clean (0 new errors; 169 pre-existing baseline unchanged):**
- ✅ Disconnect/Stop while streaming now tears down capture+encoder (Critical #8) — `VoicePanel.tsx`, `UserPanel.tsx`
- ✅ `community_auth_responded` no longer hijacks the active channel (High) — `useServerEvents.ts`
- ✅ `voiceStore.disconnect()` clears `activeStreams`/presence/`userCapabilities` (Medium) — `voiceStore.ts`
- ✅ Keyframe (PLI) request routes to native encoder on the Linux-native path (High) — `useVoiceEvents.ts`
- ✅ Watching own stream now restores remote-stream audio on exit (High) — `StreamViewPanel.tsx`
- ✅ `Infinity` MP3 duration guarded on the playback path (Medium) — `PersistentAudioLayer.tsx`, `audioController.ts`

**C++ servers — edited, NOT compiled here (cmake + libpqxx/jwt-cpp absent on this machine; needs a build to verify):**
- ✅ Central: `LOGIN_REQ` kick moved after password verify (High); oversized frame closes the connection (Medium); aliasing-safe length read (Low) — `src/server/main.cpp`
- ✅ Community: oversized frame closes the connection (Medium); `udp_key` erased only if it still maps to this session (High); aliasing-safe length read (Low) — `src/community/main.cpp`
- ✅ Attachments: overflow-safe PATCH size check (High); bounded request-head buffer (High); `mime` validated + `nosniff` + `Content-Disposition: attachment` on serve (Critical stored-XSS) — `src/community/attachment_http.cpp`

**Deferred from batch 1:** Theme A (JWT-key/secret split + cert pinning), Theme B (UDP media-plane crypto), Theme E (Electron preload lockdown).

## Fix progress — batch 2 (2026-07-22): more safe wins

**Native Rust — built clean (`cargo build`):**
- ✅ sws scaler failures propagate instead of `.expect()`-panicking the encode thread (2 sites) — `encoder_linux.rs`
- ✅ PipeWire `param_changed` no longer panics across the C/FFI boundary on a malformed format pod (video + audio capture) — `capture_pipewire.rs`, `capture_audio_pipewire.rs`

**Renderer — typechecks clean (169 baseline unchanged):**
- ✅ ChatPanel `handleSend` (High): claimed-pendings guard kills duplicate sends during upload; skips empty sends; surfaces upload/send failures via toast; drops the phantom optimistic bubble and restores the draft on send failure — `ChatPanel.tsx` (+ new `removeMessageByNonce` in `chatStore.ts`)
- ✅ `dmStore.addDmMessage` id-dedup for reconnect replays — `dmStore.ts`

**C++ servers — edited, needs a build to verify:**
- ✅ Community: voice + stream presence broadcasts now go only to authenticated sessions (closes the unauthenticated roster/codec-cap leak) — `src/community/main.cpp`
- ✅ Central: constant-time `verifySharedSecret`; reject an empty `DECIBELL_JWT_SECRET` at startup — `src/server/auth_manager.hpp`, `src/server/main.cpp`

**Deferred:** native EAGAIN frame-drop (needs a `Vec<EncodedFrame>` refactor + a live stream to test).

## Fix progress — batch 3 (2026-07-22): more safe wins

**Renderer — typechecks clean (169 baseline unchanged):**
- ✅ avatarStore drops the raw JPEG bytes once the blob URL exists (was doubling memory per avatar and growing all session) — `avatarStore.ts`
- ✅ `user_list_updated` skips the store write (and the friends re-render) when no presence actually flipped — `useFriendsEvents.ts`

**C++ servers — edited, needs a build to verify:**
- ✅ Central: login runs a same-cost bcrypt on the user-not-found path, so response latency no longer leaks whether a username exists (enumeration); `registerUser` now reports failure when the DB insert fails instead of a false "success" — `src/server/auth_manager.{cpp,hpp}`
- ✅ Community: the VIDEO relay rejects a packet whose declared `payload_size` overruns the datagram before fanning it out to every watcher — `src/community/main.cpp`

**Deferred:** community FEC/NACK/keyframe relay auth-gating (High, but more invasive — needs the requester's session looked up + watcher relationship checked); native EAGAIN frame-drop.

## Fix progress — batch 4 (2026-07-24): Theme E — Electron preload lockdown

**All client-side — typechecks clean (electron config 0 errors; renderer 169 baseline unchanged):**
- ✅ **`fs.writeFile` confined** to paths the user just chose via a save dialog (single-use approval in `writeApproval.ts`, granted by `dialog.ts`, enforced in `fs.ts`) — closes arbitrary-path write → RCE
- ✅ **`fs.readFile` removed** — it was an arbitrary local-read primitive with no live caller (renderer streams picked files via `file:register`) — `fs.ts`, `preload/index.ts`, `types/global.d.ts`
- ✅ **`netFetch` requires `attachmentTarget`** and a `/`-prefixed path — kills the raw-URL SSRF / `file://` read mode; every real caller already used `attachmentTarget` — `netFetch.ts`
- ✅ **`callCommand` own-property guard** — dispatches only the addon's own function exports, never inherited `Object.prototype` members reachable via an attacker-controlled method string — `addon.ts`
- ✅ **Navigation guards** — `will-navigate` blocks off-origin top-frame navigation, `setWindowOpenHandler` denies all `window.open` (https links go to the OS browser), `will-attach-webview` refuses `<webview>` — `window.ts`, `index.ts`
- ✅ **Deep-link validation** in main before forwarding to the renderer (scheme + length + no control/whitespace + parseable) — `index.ts`
- ✅ `DECIBELL_GPU_SANDBOX_OFF` gated on `!app.isPackaged` (can't take effect in a shipped build); `loadURL`/`loadFile` now have `.catch` — `index.ts`

**Deferred within Theme E (riskier / need runtime testing, not "safe" unverified):**
- Re-enabling `sandbox: true` / `webSecurity: true` on the BrowserWindow — `webSecurity:false` is currently relied on for cross-origin media loading; flipping needs the running app to verify nothing breaks.
- `--no-sandbox` on Windows (`index.ts:222`) — can't test Windows here; needs the narrower per-service flag investigated on a Windows box.
- Loopback **mediaServer per-session auth token** (Medium) — changes the renderer↔mediaServer URL contract used by every `<img>`/`<video>`; wants runtime verification.
- `file:register` arbitrary-path confinement (Medium) — legit paths come from drag-drop (`webUtils`) as well as dialogs, so confinement needs the drop-path flow reworked.
- `getDisplayMedia` main-side consent gate (Medium) — UX change.

## Fix progress — batch 5 (2026-07-24): more Highs/Mediums

**Renderer — typechecks clean (169 baseline unchanged):**
- ✅ **Channel-scroll pagination** (High): ChatPanel now wires Virtuoso `startReached` → `request_channel_history(beforeId=oldest)` with a single-flight guard. Channel chat could previously never load past the first 50 messages even though the store/protocol supported it — `ChatPanel.tsx`

**C++ central — edited, needs a build to verify:**
- ✅ **BLOCK now blocks DMs** (Medium): new `isBlocked(a,b)` (canonical-pair BLOCKED lookup), checked at the top of `check_dm_allowed` before the online/friends-only logic — so a blocked user can't DM even while the recipient is offline. Fails open on DB error (this gate runs for every DM) — `auth_manager.{cpp,hpp}`, `main.cpp`
- ✅ **`resolveCommunityInvite` expired-invite pruning actually runs now** (Medium): the old nested `pqxx::work` on the same connection threw `usage_error` (swallowed), so the DELETE never executed; now reuses the open transaction — `auth_manager.cpp`

## Fix progress — batch 6 (2026-07-24): renderer Mediums

**All client-side — typechecks clean (169 baseline unchanged):**
- ✅ Paste/drop no longer mis-routes to the last-active channel when you're not in the server view (`activeView === "server"` gate) — `usePasteToAttach.ts`, `useDragDrop.ts`
- ✅ `remove`/`removePending` now release the ChunkSource (unregister the `decibell-file://` whitelist entry) and clear the `lastProgressCommit` bookkeeping — a queued-then-cancelled upload no longer leaks a main-process file grant — `attachmentsStore.ts`
- ✅ Server-denied kick/ban/leave and invite create/revoke now surface a toast instead of failing silently (looked like a hang) — `useServerEvents.ts`
- ✅ Dropped delta frames re-arm the keyframe gate — a backpressure/lag drop no longer smears until the next natural IDR (skips deltas until a keyframe) — `StreamVideoPlayer.tsx`
- ✅ The poster-frame seek in `probeMetadata` now has a timeout — a truncated/stalling video can no longer hang `queueUpload` forever (attachment never appearing) — `uploadAttachment.ts`

## Fix progress — batch 7 (2026-07-24): relay auth-gate + server/renderer Mediums

**C++ community/central — edited, needs a build to verify:**
- ✅ **KEYFRAME_REQUEST + NACK relay now require an authenticated requester** (the remaining relay **High**): both look up the requester via `find_session_by_token(sender_id)` and drop if absent — closes the unauthenticated remote DoS where anyone reaching the media port could flood keyframe requests / inject NACKs at a named streamer — `src/community/main.cpp`
- ✅ **Stream-thumbnail size cap** (Medium): `STREAM_THUMBNAIL_UPDATE` drops updates whose JPEG exceeds 128 KB, so a member can't push repeated ~2 MB blobs into the per-username cache — `src/community/main.cpp`
- ✅ **Password policy** (Medium): registration rejects passwords shorter than 8 chars (empty passwords were accepted). Only affects new accounts — `src/server/auth_manager.cpp`

**Renderer — typechecks clean (169 baseline unchanged):**
- ✅ **encoderProbe always ships caps to native** (Medium): the `VideoEncoder`-missing path used to early-return before `set_encoder_caps`, leaving native's in-memory caps stale so the codec advertisement diverged; it now falls through and ships the (empty) list — `encoderProbe.ts`

**Still deferred:** community FEC relay size-validation + watcher-relationship check on KEYFRAME/NACK (the auth-gate lands the big win; scoping to actual watchers is a follow-up); native EAGAIN frame-drop; the remaining encoderProbe Windows/Linux-native ship paths.

## Fix progress — batch 8 (2026-07-24): backpressure caps + heartbeat DDL

**C++ servers — edited, needs a build to verify:**
- ✅ **Per-session write-queue cap** (Medium DoS), both central and community: a client that stops reading no longer lets broadcasts/presence/DMs pile up in memory without bound — over 1024 queued frames the connection is dropped. The disconnect is *posted* (not called synchronously) because `deliver()` runs inside broadcast loops that iterate `sessions_`, so erasing in-line would invalidate the iterator — `src/server/main.cpp`, `src/community/main.cpp`
- ✅ **No more DDL on the heartbeat path** (Medium perf): `community_servers` `CREATE TABLE` + two `ALTER TABLE`s moved from `upsertCommunityServer` (ran every heartbeat) into `initializeDatabase` (once at startup) — `src/server/auth_manager.cpp`

**Renderer — typechecks clean (169 baseline unchanged):**
- ✅ **Connect rejection now always shows feedback** (Medium): a plain browse-view connect that the community server rejects used to park the error in a store no mounted modal read, stranding the user on an empty server view — now it also toasts — `useServerEvents.ts`

## Fix progress — batch 9 (2026-07-24): renderer Lows

**All client-side — typechecks clean (169 baseline unchanged):**
- ✅ Channel composer draft no longer bleeds across channels — ChatPanel now persists/restores per-channel drafts via the existing `channelDrafts` store (mirrors the DM pattern) — `ChatPanel.tsx`
- ✅ InviteModal / MembersAdminPanel / ChannelSettingsModal are now dismissable with **Escape** (new shared `useEscapeToClose` hook, gated on the modal being active) — `hooks/useEscapeToClose.ts` + the three modals
- ✅ codec toggle (`setUseAv1`/`setUseH265`) rolls back the optimistic value + toasts on a failed persist instead of showing a value that never saved — `codecSettingsStore.ts`
- ✅ Voice-threshold meter no longer re-renders at 60fps while idle — the smoothing loop snaps to the target so React's same-value bailout kicks in — `AudioTab.tsx`

**Deferred (need a live stream / scroll to verify safely):** decoder hard-close recovery + StreamCapture stop/start race (WebCodecs, easy to break untested); DM/channel Virtuoso `firstItemIndex` scroll-jump polish.

## Fix progress — batch 12 (2026-08-17): cross-server channel-key collision

**Renderer + native — typechecks clean (0/0), native builds:**
- ✅ **Per-channel client state keyed by bare channel id** (Medium, latent data-mixing): every server ships a "general", and the client holds multiple live server connections — so `messagesByChannel`, `pendingDeletions`, history flags, scroll positions, the LRU cache order, and channel drafts could all mix two servers' data (and ServerBar deliberately keeps `activeChannelId` across a server switch when the ids match, hitting the collision on every such switch). All of it now keys on a **branded `ChannelKey`** (`serverId:channelId`, `src/lib/channelKey.ts`) — the brand makes tsc reject bare-string indexing (verified: TS7053 on a deliberate violation), so future call sites can't quietly reintroduce it. Store actions take `(serverId, channelId, …)` explicitly; `message_received` now carries `serverId` from the native router (empty for DMs) — `chatStore.ts`, `draftsStore.ts`, `ChatPanel.tsx`, `useChatEvents.ts`, `useServerEvents.ts`, `events.rs`, `net/community.rs`, `net/central.rs`

## Fix progress — batch 11 (2026-08-17): community-server hardening pass (pre-roles)

**C++ community server — compiled + linked standalone (g++, fetched headers) and e2e-tested against a live instance (9/9 scenarios pass: auth success/failure ordering, ghost-channel drop, voice-join validation):**
- ✅ **Fresh-DB owner never seeded** (Critical, found via smoke test): `seed_if_empty_` judged freshness by `COUNT(*) FROM server_meta`, but `init_schema_`'s migrations stamp `schema_version` there first — so fresh installs skipped seeding entirely and came up ownerless (owner can't even join). Freshness now keyed on the `owner` meta key; also self-repairs damaged DBs on next boot with `DECIBELL_OWNER_USERNAME` set — `db.cpp`
- ✅ **Offline kick/ban never revoked central membership** (High): `force_disconnect` early-returns for offline targets *before* `sync_membership_revoke`, leaving a stale `user_communities` row that auto-rejoins the banned user into a server that then rejects them. Revoke moved into the kick/ban/leave handlers, unconditional — `main.cpp`
- ✅ **Backlog-overflow "disconnect" didn't disconnect** (High): the batch-8 write-queue cap posted `leave()`, which only detaches from the manager — the slow reader stayed connected as a zombie that could still post while pinning up to 1024 queued frames. Overflow (and the oversized-frame path) now close the socket — `main.cpp`
- ✅ **Failed auth left the read loop armed** (High): new `close_after_flush()` — rejection response is delivered, read loop stops, socket closes once the queue drains (e2e-verified ordering). Repeat `COMMUNITY_AUTH_REQ` on an authenticated session is now ignored (was leaking stale udp_key index entries per re-auth) — `main.cpp`
- ✅ **Wire-input validation** (High): CHANNEL_MSG requires an existing channel (ghost rows were unreachable by retention/wipe/history), caps content at 64 KB (was ~2 MB), drops empty messages; JOIN_VOICE_REQ requires an existing voice channel; START_STREAM/WATCH_STREAM require being in that voice channel; STREAM_THUMBNAIL_UPDATE requires an actual live stream — `main.cpp`
- ✅ **Inert central-sync deadlines / io-thread freeze** (High, Theme D): `send_to_central_blocking` rewritten as an async chain driven by `run_for(5s)` — the old deadline timer lived on an io_context nobody ran and could never fire, so a stalled central hung threads forever; the heartbeat additionally did blocking connect/read **on the io thread** (freezing all TCP + UDP relay for minutes against a blackholed central) — now on a detached, time-bounded thread — `main.cpp`
- ✅ **Retention sweep deleted ready-but-unbound attachments after 1 h** (Medium): missing `upload_status` filter meant an upload sitting >1 h in a compose box vanished before the message was sent; 'uploading' rows keep the 1 h cutoff, 'ready' unbound rows get 24 h — `db.{hpp,cpp}`, `main.cpp`
- ✅ **Streamer disconnect leaked its watcher set** (Medium): `leave()` now clears `stream_watchers_[ch][username]` like `stop_stream` does (stale watchers no longer resume receiving on re-stream); empty entries in `voice_channels_` / `active_streams_` / `stream_watchers_` are pruned everywhere (unbounded-map-growth item from the original review) — `main.cpp`
- ✅ CHANNEL_UPDATE_RES failures now go only to the requester instead of being broadcast to every member (Low) — `main.cpp`

**Still open (deliberately untouched here):** Theme A (asymmetric JWT split — scheduled final batch); one-session-per-username enforcement; unauthenticated UDP PING reflection (Low); DB/attachment I/O still on the single io thread (larger refactor).

## Fix progress — batch 10 (2026-07-25): renderer typecheck baseline → 0

**The 169 pre-existing renderer type errors are gone (both `tsconfig.web.json` and `tsconfig.node.json` now typecheck at 0).** Root cause was tiny: `chatStore` and `dmStore` each referenced their own hook (`useXStore.getState()/.setState()`) *inside* the store's `create()` initializer, which makes the store's type circular → collapses to `any`. Because almost every component selects from those two stores, that one issue cascaded `TS7006` implicit-any onto ~160 selector call-sites across ~30 files.
- ✅ `chatStore` / `dmStore`: use the `get`/`set` creator params instead of the store hook inside `snapshotAndRemove`/`snapshotAndRemoveDm` (behavior identical — `get()`/`set()` *are* `getState()`/`setState()`) — fixed 168 of the 169
- ✅ `useFriendsEvents`: the mapped friends list omitted `FriendInfo.avatarVersion` (silently `undefined` at runtime — a latent bug); now populated from the payload — the last error

No performance change (types are erased at build); the win is a clean baseline so a real new type error is now visible immediately instead of hidden in the noise.

## ⏳ SCHEDULED — FINAL batch: Theme A (do NOT drop)

**Decision (2026-07-22): Theme A is deferred to the LAST fix batch, done deliberately as its own coordinated change.** This is the single highest-impact item in the report — logging it here so it isn't forgotten.

**The problem (see Theme A below for full detail):** JWTs are HS256 (symmetric), and every community server must hold that key to *verify* user tokens — so any community operator (or one leaked key) can *forge* a JWT for any user across the whole federation. The JWT signing key is *also* reused as the community↔central shared secret.

**Why a "just split into two HS256 keys" fix does NOT work:** with HS256 the verify key *is* the forge key, so community servers that verify tokens can always forge them. The fix must make signing **asymmetric**.

**Planned approach:**
- Move JWT signing to **Ed25519** (or RS256): central holds the private key and signs; community servers get only the **public** key — verify-only, cannot forge.
- Give the community↔central heartbeat / membership channel its own **separate symmetric secret** (a new env var, e.g. `DECIBELL_COMMUNITY_SECRET`), no longer the JWT key.
- Also part of Theme A: replace `verify_none` with **cert pinning / TOFU** scoped to the explicitly-joined host (central directory can carry each community's expected cert hash).

**Open decision — how community servers get central's public verify key:**
- (a) **Embedded/config at deploy** (leaning this way): ship the public key as an env var / file per community server. Simplest, no runtime dep on central, manual rotation.
- (b) **Fetched from central at startup**: enables rotation without redeploying every community server, but adds a startup dependency.

**Scope:** `src/server/` (central: keypair gen/load, sign) + `src/community/` (verify with public key; new heartbeat secret). **Client is unaffected** — it just carries whatever token it's issued. Wire format of the token itself is unchanged (still a JWT, different `alg`).

---

## Scope

**In scope (currently-shipping code):**
- C++ **central server** — `src/server/` (auth, JWT, DMs, friends, presence, directory)
- C++ **community server** — `src/community/` (channels, voice/stream signaling, UDP relay, attachment HTTP, DB)
- Shared wire — `src/common/` (`net_utils.hpp`, `udp_packet.hpp`), `proto/messages.proto`
- **Electron client** — Rust napi addon (`electron-client/native/`), Electron main/preload (`electron-client/electron/`), React renderer (`electron-client/src/`)

**Excluded (per your instruction):** the Qt/QML client (`src/client/`), `tauri-client/`, and generated protobuf (`messages.pb.*`).

**Method:** each subsystem got a dedicated agent doing a full read (not a skim) against the shared wire-protocol contract and the known project hazards. Two subsystems (central + community servers) were independently double-covered; where two agents flagged the same issue it's marked **[corroborated]** below and should be treated as high-confidence.

A severity-tagged, file\:line-referenced breakdown per subsystem lives in the scratchpad findings set; this document is the consolidated action list.

---

## Executive summary

The architecture is sound and a lot of the hard parts are done well: **no SQL injection anywhere** (every query parameterized on both servers), **no XSS in the message renderer** (all user content goes through React text nodes), disciplined WebCodecs `VideoFrame` lifecycle, clean TCP framing with a pre-allocation size cap, and no `eprintln!`/`println!` left on hot paths (the EAGAIN-panic hazard is respected). Selector discipline in the renderer is good and the known zustand `&&`-short-circuit footgun does **not** appear anywhere live.

The serious problems cluster into a handful of **structural themes** rather than one-off typos. The top one is that **transport authentication is disabled end-to-end and a single symmetric secret is reused for everything** — together these mean one on-path attacker or one leaked env var compromises the whole federation. After that: the **UDP media plane has no cryptography**, the **native receiver trusts untrusted wire fields** (one packet can OOM a viewer), **blocking work freezes the single-threaded servers and the Rust async runtime**, the **Electron preload hands the renderer OS-level primitives**, and **logout / stream-teardown leave capture and state running**.

None of these require a protocol redesign to fix incrementally, but the transport-trust and secret-separation items are the ones that make everything else exploitable remotely, so they come first.

---

## Fix-first list (highest impact, ranked)

| # | Issue | Where | Why it's first |
|---|-------|-------|----------------|
| 1 | JWT signing key **is** the community shared secret | `src/server/auth_manager.hpp:25`, `main.cpp:1062-1074` | Any community operator — or one leaked env var — can forge a JWT for **any** user across the whole network |
| 2 | TLS `verify_none` on every link + Electron `callback(0)` session-wide | `net/tls.rs:12-67`, community `main.cpp:2207,2638`, `electron/main/index.ts:619-635` | On-path attacker MITMs the handshake and harvests password + JWT; makes #1 remotely reachable |
| 3 | UDP media plane unencrypted; per-packet bearer = JWT suffix in cleartext; endpoint learned from any packet | community `main.cpp:2454-2488, 2464-2465, 2581-2582` | Sniff one datagram → impersonate on media plane **and** redirect a victim's audio/video to yourself |
| 4 | One crafted VIDEO packet allocates ~75 MB on a viewer (`total_packets` untrusted) | `native/media/video_receiver.rs:63, 214-222` | Remote OOM crash of any watcher; no auth needed beyond being relayed |
| 5 | Blocking DB connect + bcrypt on the single event-loop thread | central `main.cpp:1096`, `auth_manager.cpp:118-136` | Unauthenticated `LOGIN_REQ` flood freezes the entire central server (~4 logins/sec ceiling) |
| 6 | Preload exposes arbitrary-path `fs.readFile`/`writeFile`; `netFetch` is unrestricted (SSRF + `file://`) | `electron/main/fs.ts:33-52`, `netFetch.ts:44-115` | Any renderer foothold → arbitrary file write (RCE) and internal-network/file read |
| 7 | AppState mutex held across a network `.await` in the thumbnail path | `native/media/mod.rs:684-694, 796-807`, `commands/streaming.rs:764-775` | A stalled TCP peer freezes the **entire** command surface (~100% lock occupancy) |
| 8 | Disconnecting/stopping while streaming never stops capture+encode+UDP | `features/voice/VoicePanel.tsx:88-111` | Screen keeps being captured and transmitted after "leaving"; **no UI left to stop it** (privacy) |
| 9 | Attachment `mime` is client-controlled → stored XSS + HTTP response splitting | community `attachment_http.cpp:298, 691` | Uploaded HTML/JS executes from the server's own TLS origin; no `nosniff`/`Content-Disposition` |
| 10 | `logout` leaves voice/video/audio engines running (native) and DM/friends/drafts in memory (renderer) | `commands/auth.rs:126-152`, `features/auth/useAuthEvents.ts:72-78` | Mic/screen keep transmitting after logout; next account on a shared machine sees the previous user's DMs |
| 11 | `UDP_MAX_PAYLOAD` is 1200 in Rust but 1400 in the C++ header; recv buffer truncates | `native/media/video_packet.rs:19` vs `src/common/udp_packet.hpp:14` | **[corroborated]** Any 1400-chunking sender → permanently corrupt-but-"reassembled" video, invisible server-side |
| 12 | `bool is_keyframe` memcpy'd from an untrusted wire byte = UB | `native/media/video_packet.rs:172-179` | **[corroborated]** Remotely-triggered undefined behavior (miscompile-class) |

---

## Cross-cutting themes

### Theme A — Transport trust is off everywhere, and one secret does everything
> **⏳ SCHEDULED as the FINAL fix batch** (decision 2026-07-22) — see the "SCHEDULED — FINAL batch" note near the top of this file for the planned approach (asymmetric JWTs + separate heartbeat secret + cert pinning) and the open key-distribution decision.

This is the backbone issue. `verify_none` / `NoVerifier` / `callback(0)` appears on **every** TLS surface: client→central, client→community, community→central, and the Electron session as a whole. Independently, the HS256 JWT signing key is the *same value* the community servers present as their shared secret (**[corroborated]** by both the central and community reviews). The combination is what turns a passive network position into a full compromise: MITM the (unverified) handshake, read the shared secret out of the first community→central heartbeat, and now you can mint a valid JWT for any username on any server. Even without MITM, every self-hoster who runs a community server already holds the key that forges everyone's identity.

**Direction:** split the two roles (asymmetric RS256/EdDSA for JWTs, or at minimum a distinct HS256 key; a separate, ideally per-community, shared secret that is never a signing key), and move from blanket cert-bypass to trust-on-first-use / cert-pinning scoped to the specific host the user explicitly joined. The central directory can carry each community's expected cert hash. You control both endpoints, so pinning is cheap and keeps self-signed operation working.

### Theme B — The UDP media plane has no cryptography or endpoint binding
Voice/video UDP is entirely cleartext, the only per-packet authorization is the last 31 chars of the JWT embedded in every datagram, and the server rebinds a session's downstream endpoint to whatever source last sent a packet with that token-suffix. So a co-channel member (or anyone on-path) who sees one datagram can both inject media as the victim and hijack the victim's inbound stream to their own IP. On top of that, `KEYFRAME_REQUEST` and `NACK` relays do no auth at all, giving a cheap remote DoS against any named streamer. **Direction:** negotiate a per-session UDP token/key over the authenticated TLS channel (not derived from the JWT), pin the endpoint at auth time, and gate PLI/NACK on an authenticated watcher relationship.

### Theme C — Untrusted wire fields drive allocation, indexing, and a `bool`
The native receiver mostly validates well (the `payload_size`-lie clamp is good and tested), but three fields still bite: `total_packets` sizes a `Vec` before any completion check (75 MB per packet, #4 above), `group_start + group_count` is unchecked `u16` arithmetic that overflows, and `is_keyframe` is a raw byte written into a Rust `bool` (UB). The relay server also forwards `payload_size`/`nack_count`/`total_packets` without sanity-checking them against the datagram length, so it happily amplifies a malformed packet to every watcher. **Direction:** cap `total_packets` and the in-progress-frame map, use `saturating_add`, represent wire booleans as `u8`, and have the relay validate declared sizes against `bytes_recvd` before forwarding.

### Theme D — Blocking work on single-threaded reactors and the async runtime
Both C++ servers run one `io_context` on one thread, and several handlers do synchronous work on it: bcrypt (~250 ms), per-call PostgreSQL connects, the community heartbeat's synchronous TLS read (with a deadline timer that can't fire because the thread it lives on is blocked), and the retention sweep. **[corroborated]** across both server reviews. The Rust side has the mirror-image problem: the global `AppState` mutex is held across `.await` points (thumbnail send, engine startup with synchronous DNS, engine teardown that `thread::join`s) — the project's own comments forbid exactly this. **Direction:** move blocking work to worker threads / `spawn_blocking`, use a DB connection pool, and adopt the "clone the `write_tx` under the lock, drop the guard, then send" pattern that most commands already follow.

### Theme E — The Electron renderer→main boundary is a privilege surface
`contextIsolation`/`nodeIntegration` are set correctly and there's a real CSP, but the preload then hands the renderer arbitrary-path file read/write, an unrestricted `net.fetch` (SSRF + `file://` read), and a by-name dispatch into the entire Rust command surface — all unvalidated — while `webSecurity:false`, `sandbox:false`, `--no-sandbox` (Windows), and the absence of any `will-navigate`/`setWindowOpenHandler` guard remove the containment that would stop a renderer foothold from escalating. **Direction:** confine the fs handlers to known roots, require the registry/`attachmentTarget` form for fetches (allowlist community hosts, block `file:`/localhost/internal), allowlist invokable command names, add navigation/window-open guards, and re-enable the renderer sandbox.

### Theme F — Logout and stream-teardown don't release things
Logout leaves the native voice/video/audio engines capturing and transmitting, and leaves DM history, drafts, friends, and in-flight uploads in renderer memory (next account sees them). Stopping or leaving a voice channel while streaming similarly leaves the capture pipeline alive — **[corroborated]** across the native-commands, renderer-voice, and renderer-stores reviews. `voiceStore.disconnect()` also forgets several maps, so stale streams/badges linger. **Direction:** a single `resetForLogout()`/teardown path that stops engines (off the lock, via `spawn_blocking`), aborts uploads, revokes object URLs, and clears every per-session store.

---

## Findings by subsystem

Severities: **Critical** (remote compromise / crash / data loss), **High** (serious security or correctness), **Medium** (real bug, bounded blast radius), **Low** (hardening / quality). Only Critical and High are expanded here; Medium/Low are listed compactly with locations.

### C++ Central Server (`src/server/`)
**Critical**
- Blocking DB connect + bcrypt on the single event-loop thread → unauthenticated whole-server DoS via `LOGIN_REQ` flood. `main.cpp:1096`, `auth_manager.cpp:118-136`.
- Community shared secret == JWT signing key (Theme A). `auth_manager.hpp:25`, `main.cpp:1062-1074`.

**High**
- `LOGIN_REQ` calls `kick_user(username)` **before** verifying the password → any unauthenticated party evicts any user by name. `main.cpp:190-192`.
- `SessionManager::leave()` never closes the socket or stops the read loop → "kicked"/security-dropped sessions keep processing packets and leak fds; makes the kick DoS persistent. `main.cpp:862-879, 116-130`.
- Username enumeration via login timing (bcrypt runs only for existing users) + explicit registration oracle. `auth_manager.cpp:118-123, 105-116`.

**Medium** — BLOCK doesn't block DMs (`main.cpp:985-1017`); DM `friends_only` not persisted, bypassable while offline (`main.cpp:985-1017, 624-628`); registration returns success even when the insert fails, plus TOCTOU (`auth_manager.cpp:105-116, 170-182`); no password policy, empty passwords accepted (`auth_manager.cpp:105-116`); `resolveCommunityInvite` nested transaction throws-and-swallows so expired-invite pruning never runs (`auth_manager.cpp:316-347`); oversized frame `return`s and wedges the read loop (`main.cpp:103-107`); no cap on connections or DM/server-picture sizes (`main.cpp:1043-1053, 218-288, 721-728`); Session fields read cross-session without synchronization — latent race the moment you add threads (`main.cpp:833-845`); `check_dm_allowed` holds the manager mutex across a DB query (`main.cpp:985-1017`); `upsertCommunityServer` runs DDL on every heartbeat (`auth_manager.cpp:242-265`); unvalidated heartbeat data allows directory-ranking abuse + cross-community row hijack (`main.cpp:631-653`); write queue unbounded → slow-reader OOM (`main.cpp:63-95`); bcrypt salt from a 32-bit-seeded mt19937 / non-crypto `random_device` (`bcrypt.h:445-449`).

**Low** — non-constant-time secret compare (`auth_manager.hpp:25`); JWT subject never bound to the session (`main.cpp:157-173`); no username normalization/charset validation; TLS pinned to 1.2, relative cert paths (`main.cpp:1026-1038`); empty `DECIBELL_JWT_SECRET` passes startup (`main.cpp:1062-1073`); strict-aliasing read of the length prefix (`main.cpp:103`); avatar size cap says 1 MB in code vs 200 KB in spec; verbose unsanitized logging of attacker-controlled fields.

### C++ Community Server (`src/community/`)
**Critical**
- Media plane cleartext + per-packet JWT-suffix bearer token → sniff-and-impersonate (Theme B). `main.cpp:2454-2488`.
- UDP endpoint learned/overwritten from any incoming packet → media hijack/eavesdrop (Theme B). `main.cpp:2464-2465, 2581-2582`.
- Client-controlled `mime` → stored XSS + HTTP response splitting, served with no `nosniff`/`Content-Disposition`. `attachment_http.cpp:298, 691, 682-694`.
- `verify_none` on community→central + shipping the shared HMAC secret as the auth token (Theme A). `main.cpp:2207, 2638, 2269`.

**High**
- Unauthenticated `KEYFRAME_REQUEST`/`NACK` relay → remote DoS on any named streamer. `main.cpp:2532-2563`.
- Integer overflow in the PATCH size check bypasses the upload cap → disk-fill DoS. `attachment_http.cpp:395-403`.
- Unbounded request-head buffering (streambuf `max_size == SIZE_MAX`, 16 KB check only after the delimiter) → memory DoS pre-auth. `attachment_http.cpp:203-218`.
- No socket read/write timeouts anywhere → slow-loris / connection exhaustion. `attachment_http.cpp` (whole `AttachmentConnection`).
- `udp_key` collision on reconnect (same JWT suffix, erase-by-key) drops the live session's UDP routing. `main.cpp:476-482, 1365-1367, 2111-2114`.
- Unbounded map growth from unvalidated `channel_id`/`target_username` (arbitrary-key `operator[]`, empty outer entries never erased). `main.cpp:509-518, 544-558, 569-588`.
- Blocking heartbeat (sync TLS read with a dead deadline) + retention sweep run on the single io_context thread → periodic freeze of all relay/TCP (Theme D). `main.cpp:2636-2688, 1643-1760`.
- Unauthenticated connections receive voice/stream presence (usernames, mute/deafen, codec caps) — broadcasts don't filter on `is_authenticated()`. `main.cpp:2038-2041, 1485-1488`.

**Medium** — attachments served inline with no `nosniff`/`Content-Disposition` even for octet-stream (`attachment_http.cpp:682-694`); IDOR — any member fetches any attachment by enumerable id, including ready-but-unposted (`attachment_http.cpp:548-553`); concurrent PATCH to the same upload corrupts the file, no locking (`attachment_http.cpp:373-439`); `shared_ptr<FILE*>` never `fclose`s → fd leak on dropped chains (`attachment_http.cpp:406, 633`); `send_file_body` can under-deliver `Content-Length` on concurrent purge (`attachment_http.cpp:816-835`); no membership re-check on upload endpoints (`attachment_http.cpp:377, 513`); `channel_id` concatenated into the storage path — safe only because there's no channel-creation API yet (`attachment_http.cpp:324`); thumbnail cache has no size cap and allows cross-channel injection (`main.cpp:606-617`); server relays media without validating `payload_size`/`nack_count`/`total_packets` vs datagram length (`main.cpp:2474-2483`); presence fan-out hits every session, not just channel members → amplification (`main.cpp:2038-2041`).

**Low** — JWT `exp` only checked if present, no re-validation after auth (`main.cpp:402-407`, `attachment_http.cpp:77-88`); unauthenticated UDP PING reflection (`main.cpp:2438-2446`); oversized TCP frame wedges the read loop (`main.cpp:366-369`); no per-member storage quota / `retention_days=0` lives forever; TLS cert/key relative paths, no `check_private_key` (`attachment_http.cpp:937-938`); global DB mutex serializes long prune/wipe ops; `get_attachment` column-index drift risk (`db.cpp:1048-1078`); strict-aliasing length read (`main.cpp:366`). **DB layer confirmed free of SQL injection.**

### Rust Native — Media Encode + Capture (`native/src/media/*encoder*, *capture*, gpu_*`)
**High**
- EAGAIN retry path drains and **discards** a fully-encoded frame → silent decode corruption until the next keyframe on every back-pressure event. `encoder_linux.rs:1803-1806` (+4 sibling sites).
- Windows native AV1 is undecodable: `GLOBAL_HEADER` moves the sequence header into extradata that is never shipped as a description or prefixed on keyframes (Linux/renderer paths do it right). Also Windows ships Annex-B while Linux ships length-prefixed under the same codec byte. `encoder.rs:216-222, 307-330`, `encoder_thread.rs:171-189`.
- `CUDA_MEMCPY2D` struct has phantom "reserved" fields (those exist only in `MEMCPY3D`), shifting every dst field → every `cuMemcpy2D` fails and the NVIDIA zero-copy path dies on the first frame. `gpu_interop.rs:76-97`, dup at `encoder_linux.rs:878-898`.
- `UDP_MAX_PAYLOAD` 1200 (Rust) vs 1400 (C++ header) + 1245-byte recv buffer truncates legacy datagrams → corrupt-but-"reassembled" video. **[corroborated]** `video_packet.rs:19` vs `udp_packet.hpp:14`, `mod.rs:445`.

**Medium** — `bool is_keyframe` UB from raw wire byte (**[corroborated]**, `video_packet.rs:172-179`); `.expect()` panic inside the PipeWire C callback + on the sws hot path (`capture_pipewire.rs:785-787`, `encoder_linux.rs:1697,1752`); `encode_cuda_frame` dlopens libcuda per frame and leaks an AVFrame on early return (`encoder_linux.rs:862-873`); WGC pool texture handed to the encoder after the frame is released back to the 2-deep pool → tearing (`capture_wgc.rs:133-149`); encode error silently kills the stream with no event to JS → zombie stream (`encoder_thread_linux.rs:278-281`); negotiated capture dims computed then ignored, 0×0 "source" breaks the encoder (`mod.rs:809-828`); portal request can hang forever and the capture thread has no stop handle → OS "sharing" indicator persists (`capture_pipewire.rs:255-264`); EGL/CUDA import cache keyed by recycled raw fd + unbounded recursion on map failure (`gpu_interop.rs:586, 704-719`).

**Low** — large amounts of dead/triplicated code (three `build_wire_data` copies, an unused `CodecSelector` (444 lines), the entire unreachable DMA-BUF lane ~1300 lines, a duplicate Windows encoder inside the linux-cfg file); no validation of renderer fps/dims → div-by-zero panics (`encoder_linux.rs:594,1138,1790`); `receive_one_packet` swallows real errors and drains one packet/frame (`encoder_linux.rs:1550-1561`).

### Rust Native — Pipeline / Receive / Jitter / Audio (`native/src/media/pipeline.rs`, `video_receiver.rs`, `jitter.rs`, audio)
**Critical**
- `total_packets` (untrusted `u16`) sizes `vec![0u8; total_packets * UDP_MAX_PAYLOAD]` on the first fragment → ~75 MB per crafted packet → viewer OOM. `video_receiver.rs:63, 214-222`.

**High**
- UB: `is_keyframe` `bool` filled by raw `copy_nonoverlapping` from the wire (**[corroborated]**). `video_packet.rs:44, 173-179`.
- Jitter buffer does a full reset on any packet behind the play cursor (`diff >= 32768` fires for a normal late straggler) → a single late packet flushes the whole buffer. `jitter.rs:100-105`.

**Medium** — heap allocation (`.collect()`) inside the realtime CPAL output callback (`audio_device.rs:289-292`); PipeWire capture callback allocates per buffer + `.expect()` across FFI (`capture_audio_pipewire.rs:519, 531-573`); unbounded `VoiceEvent` channel carries large video frames → memory blowup if the JS consumer stalls (`mod.rs:121`); FEC datagram size-sensitivity vs the 1200/1400 mismatch → silently corrupt reconstruction (`video_packet.rs:19`, `mod.rs:445`); `u16` overflow computing FEC `group_end` (`video_receiver.rs:107`).

**Low** — LE-only struct memcpy undocumented (`video_packet.rs:156-191`); receiver sends NACKs the sender never handles and never emits FEC → wasted uplink, PLI is the only real recovery (`video_receiver.rs:283-327`); CPAL forces f32 regardless of device format → some devices fail to open (`audio_device.rs:185`); mixed `total_packets` across a frame's fragments not validated; PING RTT trusts any PING (`pipeline.rs:771-782`); `.expect()` on Opus/resampler construction kills the whole voice thread (`peer.rs:98`).

### Rust Native — Networking (`native/src/net/`)
**Critical**
- `NoVerifier` accepts every cert/signature; password + JWT cross an unauthenticated tunnel (Theme A). `tls.rs:12-67`, `connection.rs:38-45`.

**High**
- Community reconnect handle is stored by a *separately spawned* task → race window where disconnect/logout aborts nothing → orphaned eternal reconnect loop. `community.rs:347-353, 252-261`.
- Reconnect installs the new connection with no generation check → can clobber a healthy replacement client and orphan its live router (connection_lost/restored ping-pong). `community.rs:323-333`.
- Community reconnect re-auths with a stale JWT and gives up silently on rejection → zombie session, messages silently dropped after token rotation. `community.rs:283-291, 772-787`.

**Medium** — no outbound frame-size enforcement; a >2 MB avatar tears down the whole session (`framing.rs:42-46`); auto-rejoin fires on *every* `LoginRes`, tearing down healthy community connections on any central blip (`central.rs:297-310`); keepalive ping task exits permanently on one 5 s send stall (`central.rs:59-68` + 3 copies); plaintext password on disk under a locally-derivable key + non-CSPRNG GCM nonce (**[corroborated]** with commands review) (`config.rs:146-159, 205-218`).

**Low** — all inbound routing serializes through the global AppState mutex (`central.rs:259-264`); central host hard-coded as a raw IP, precludes SNI/pinning (`central.rs:21-22`); poisoned `voice_caps_cache` silently skipped; `is_alive()` dead code; ping/reconnect scaffolding duplicated 4×. **Framing DoS-safety, decode-error policy, backpressure bounds, and task teardown all verified clean.**

### Rust Native — Commands / State / Events / Config (`native/src/commands/`, `state.rs`, `events.rs`, `config.rs`)
**Critical**
- AppState mutex held across a network `.await` in all three thumbnail sites → a stalled peer freezes the entire command surface for up to 5 s at a time (Theme D). `mod.rs:684-694, 796-807`, `streaming.rs:764-775`.

**High**
- `logout` leaves voice/video/audio engines running (mic + screen keep transmitting) and doesn't clear `connected_voice_*`/pending maps — contradicts the documented contract (Theme F). `commands/auth.rs:126-152`.
- Blocking engine startup under the lock: `config::load()` ×2 + synchronous DNS in `UdpSocket::connect` + cpal opens + (Windows) live encoder probing. `voice.rs:107-176`, `streaming.rs:129-233`.
- Engine teardown `thread::join`s under the lock — the code's own comments forbid this. `streaming.rs:379-381`, `lib.rs:124-135`.
- Linux native start leaves a multi-second window (portal dialog) where the engine isn't in state → double-start, stop-during-start going live anyway, and global frame-sink clobbering. `streaming.rs:61-66, 261-334`, `mod.rs:627, 884`.

**Medium** — all three TSFNs created with `max_queue_size = 0` (unbounded) so the frame-drop diagnostics can never fire and a stalled main thread grows memory unbounded (`events.rs:28,69,147`); `join_voice_channel` failure path leaves the engine installed + state claiming membership, and starts the new engine before stopping the old (`voice.rs:176-215`); credential-at-rest weakness (**[corroborated]**, `config.rs:146-159`); blocking file I/O + `pactl` (no timeout) in async handlers (`settings.rs:108-187`); config read-modify-write races outside `SAVE_LOCK` lose settings (`streaming.rs:741-748`, `settings.rs:87-93`); `register` check-then-act race on `s.central` (`auth.rs:80-91`).

**Low** — second `init()` silently keeps the old dead event bus (HMR) (`events.rs:38`); TSFNs never aborted at shutdown (`lib.rs:122-138`); `mic_test_stop` dead field; thumbnail/avatar pending maps keyed by username only → cross-server clobber (`state.rs:112`). **napi Buffer/Option<Buffer> gotchas, O_NONBLOCK clearing, atomic config writes, and lock-across-await discipline elsewhere verified respected.**

### Electron Main + Preload (`electron-client/electron/`)
**Critical**
- Preload exposes arbitrary-path `fs.readFile`/`writeFile` → renderer foothold becomes arbitrary local file write (RCE) / read. `fs.ts:33-52`, `preload/index.ts:146-159`.
- `netFetch` is an unrestricted main-process fetch → SSRF (localhost/cloud-metadata/LAN) and `file://` read. `netFetch.ts:44-115`.

**High**
- No `will-navigate`/`setWindowOpenHandler`/`will-attach-webview` guards, with `webSecurity:false` + `sandbox:false` → one navigation lands attacker HTML in a renderer with SOP off, no sandbox, and the full bridge. `index.ts:350-386`.
- TLS verification globally disabled for the whole session (Theme A) → MITM steals the injected Bearer JWT and can tamper with the update channel. `index.ts:619-635`.
- `--no-sandbox` on Windows removes the sandbox from *every* child process. `index.ts:222`.
- `decibell:invoke` dispatches any addon export by attacker-controlled name → whole Rust command surface reachable from the renderer with no allowlist. `ipc.ts:12-20`, `addon.ts:117-123`.

**Medium** — loopback media server has no auth/origin check, so any local process can pull attachments with the user's JWT (`mediaServer.ts:56-96`); deep-link URLs forwarded to the renderer unvalidated → a web page can drive auto-join to an attacker community (`index.ts:53-63`); `decibell:file:register` whitelists arbitrary absolute paths (TOCTOU symlink swap) (`fs.ts:58-83`); `getDisplayMedia` handler auto-picks a source with no user-consent gate on X11/Windows (`index.ts:580-606`).

**Low** — `DECIBELL_GPU_SANDBOX_OFF` ships in release behind an env var (`index.ts:243-248`); unhandled `loadURL`/`loadFile` rejections; media-server port in argv (fine now, but don't put a future auth token there). **contextIsolation/nodeIntegration/CSP, the `decibell-asset://` traversal guard, registry-token protocols, and Sentry PII scrubbing verified good.**

### Renderer — Voice / Streaming (`src/features/voice/`)
**Critical**
- Disconnecting from voice while streaming never stops the stream — capture/encode/UDP keep running and the Stop button disappears (Theme F). `VoicePanel.tsx:96-111`, `UserPanel.tsx:96-101`.
- VoicePanel "Stop" leaks the renderer capture pipeline (doesn't call `stopActiveStream()` like UserPanel does). `VoicePanel.tsx:88-94`.

**High**
- Watcher keyframe requests (PLI) are dropped on the Linux native-encode path (branches on platform instead of `isNativeEncodeActive()`). `useVoiceEvents.ts:337-350`.
- Two `VideoDecoder`s run concurrently per watched stream (both views kept mounted, toggled with CSS `hidden`) → double decode of e.g. 1080p60. `VoicePanel.tsx:156-167, 219-223`.
- Watching your own stream permanently mutes remote stream audio (no restore branch). `StreamViewPanel.tsx:142-146`.
- Mid-stream codec downgrade is toast-only; the renderer encoder can't actually reconfigure → low-cap watcher spins forever. `useVoiceEvents.ts:292-318`, `StreamCapture.ts:50-53`.

**Medium** — dropped delta frames don't re-arm the keyframe gate → corruption until the next IDR (`StreamVideoPlayer.tsx:279-287`); fatal decoder close has no rebuild path → silent freeze (`StreamVideoPlayer.tsx:103-121`); first keyframe sent before the native engine exists → opening GOP lost (`CaptureSourcePicker.tsx:143-182`); `StreamCapture.start()` ignores `stopping` after awaits → stop/start race leaks capture (`StreamCapture.ts:202-266`); `voiceStore.disconnect()` leaves `activeStreams` stale (**[corroborated]**, `voiceStore.ts:248-272`); `decodeCaps` read via `getState()` in render → stale watchability (`VoicePanel.tsx:180`).

**Low** — thumbnail blob URLs for ended streams never revoked; conditional return before hooks in `StreamVideoPlayer`; StreamViewPanel timers not cleaned up + global Escape handler while hidden; watched-stream frames keep crossing UDP+IPC with no consumer after unmount. **VideoFrame lifecycle, AV1 no-description handling, and the R9 preload multi-stream Map all verified correct.**

### Renderer — Chat / DM / Attachments (`src/features/chat/`, `src/features/dm/`)
**High**
- No send guard — double-Enter during upload sends duplicate messages sharing the same attachment IDs. `ChatPanel.tsx:220-301`.
- Failed sends/uploads disappear silently (phantom "delivered" bubble, or an empty message sent, or failed chips removed with no toast). `ChatPanel.tsx:281-300`.
- Channel chat can never load past the first 50 messages — Virtuoso `startReached` pagination is wired in DMs but not channels. `ChatPanel.tsx:395-439`, `chatStore.ts:452`.

**Medium** — the `duration === Infinity` gotcha regressed on the *playback* path (VBR MP3 → scrub throws, total shows 0:00) (`PersistentAudioLayer.tsx:75`); DM history prepend without Virtuoso `firstItemIndex` → scroll jumps (`DmChatPanel.tsx:389-408`); DM history merge assumes in-order pages + no live-message id dedupe (**[corroborated]** with stores review) (`dmStore.ts:190-235`); paste/drop from any view silently attaches to the last-active server channel (`usePasteToAttach.ts:44-52`); removing a queued pending never releases its `decibell-file://` grant (`attachmentsStore.ts:165-183`); video poster-frame seek has no timeout → `queueUpload` can hang forever (`uploadAttachment.ts:250-255`); no timeout/abort on upload PATCH round-trips (`uploadAttachment.ts:459-507`); downloads materialize the whole file in renderer memory (`AttachmentList.tsx:523-531`); leaving the server view keeps a hidden video playing audio (`PersistentVideoLayer.tsx:536-563`).

**Low** — channel composer draft bleeds across channels (`ChatPanel.tsx:54`); playback caches/poster URLs survive logout; `MessageBubble` memoization defeated + whole-record subscriptions; server-controlled filename unsanitized into the save dialog `defaultPath`; minor dead code. **No XSS/injection path found; optimistic-send reconciliation and the upload-side Infinity guard verified correct.**

### Renderer — Stores / Servers / Channels / Auth / Settings (`src/stores/`, `src/features/servers/`, ...)
**High**
- `saveSettings` omits `use_av1`/`use_h265`, so any settings save rewrites `config.json` with both re-enabled → the user's codec toggles silently reset on next launch. `saveSettings.ts:42-72`.
- Logout doesn't clear dmStore/friendsStore/voiceStore/attachmentsStore/draftsStore → next account sees the previous user's DMs, drafts, and friends (Theme F). `useAuthEvents.ts:72-78`.
- `community_auth_responded` unconditionally hijacks the active channel → cross-server serverId/channelId mismatch during auto-rejoin (renders wrong channel, mis-targeted history requests). `useServerEvents.ts:162-167`.

**Medium** — connect-path auth failures are stored but never displayed → user lands on a dead server view (`ServerBrowseView.tsx:156-168`); server-rejected kick/ban and invite create/revoke are silently swallowed (`useServerEvents.ts:183-196`); `encoderProbe` has failure paths that skip the mandatory `set_encoder_caps` ship (`encoderProbe.ts:125-128, 274-277`); Escape while the picture cropper is open closes the whole settings modal (`ServerSettingsModal.tsx:69-76`); `voiceStore.disconnect()` forgets `activeStreams`/presence/`userCapabilities` (**[corroborated]**, `voiceStore.ts:248-272`); document-level Enter confirms destructive modals from anywhere with a stale `onConfirm` closure (`DeleteMessageConfirmModal.tsx:27-42`); three modals lack Escape/focus management.

**Low** — chatStore LRU doesn't prune `pendingDeletions`; duplicated server-picture fetch dedupe sets; avatarStore retains raw JPEG bytes it never reads (unbounded); `user_list_updated` rebuilds the friends array every presence tick; `dmStore.addDmMessage` no id-dedupe (**[corroborated]**); codec toggles optimistic with no rollback; `removePending` leaks throttle bookkeeping; VoiceThresholdBar runs a 60 fps setState loop while the Audio tab is open. **No live zustand `&&`-short-circuit; auth-token handling in the renderer verified clean (nothing sensitive in localStorage).**

---

## What's already solid (verified, worth preserving)

- **No SQL injection** on either server — every query parameterized (`db.cpp`, `auth_manager.cpp`).
- **No XSS** in the message/DM renderer — user content is React text nodes; the only `innerHTML` sinks are keyed off a build-time emoji table.
- **TCP framing** validates the big-endian length against the 2 MB cap *before* allocating, handles partial reads, and can't integer-overflow (`net/framing.rs`).
- **WebCodecs `VideoFrame`/`EncodedVideoChunk` lifecycle** is correct on both encode and decode ends; encoder backpressure is bounded and preserves a pending keyframe across drops.
- **napi gotchas respected** — `Buffer` for binary, `undefined` (not `null`) for optional buffers, `O_NONBLOCK` cleared so `println!`/`eprintln!` can't EAGAIN-panic; zero such calls remain on hot paths.
- **Task teardown** in the net layer aborts every spawned task on drop, in the right order.
- **zustand discipline** — primitive/memoized selectors, stable sentinels, and no instance of the `useA() && useB()` short-circuit footgun.
- **Renderer auth storage** — no tokens/passwords in `localStorage`; credentials only via the native encrypted store (though that store's key derivation is weak — see Theme A / native findings).

---

## Suggested sequencing

1. **Security foundation (do together):** separate the JWT key from the community shared secret; introduce cert pinning / TOFU and remove blanket `verify_none`/`callback(0)`; scope the Electron TLS-accept to explicitly-joined hosts. This closes Themes A and most of E's blast radius.
2. **Remote crash/DoS hardening:** cap `total_packets` + in-progress frames; reconcile `UDP_MAX_PAYLOAD`; represent wire booleans as `u8`; validate relayed sizes; move bcrypt/DB/heartbeat/retention off the servers' reactor threads; unblock the AppState-across-await sites.
3. **Media plane crypto + endpoint binding** (Theme B) — larger, protocol-touching; plan it once the above land.
4. **Electron preload lockdown** — confine fs, restrict `netFetch`, allowlist `invoke`, add navigation guards, re-enable the sandbox.
5. **Privacy/correctness cleanup** — unified logout/stream-teardown; the `saveSettings` codec clobber; channel pagination; attachment `mime`/`nosniff`; the auth-response channel hijack.
6. **Native encode consistency + dead-code removal** — fix the AV1/Annex-B divergence and the EAGAIN frame drop; delete or feature-gate the unreachable zero-copy lane so it stops hiding bugs.
