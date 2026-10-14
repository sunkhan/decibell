# Decibell — project guide for Claude Code

Decibell is a self-hostable, Discord-like chat app: text channels, voice, screen
streaming, DMs, communities people host on their own machines. Read this first;
deeper docs are linked at the bottom.

## Tiers and where they live

| Tier | Path | Stack |
|---|---|---|
| Community server (one per hosted community) | `src/community/` | C++20, Boost.Asio single io_context/thread, SQLite (WAL) |
| Central server (accounts, DMs, friends, community index) | `src/server/` | C++20, Boost.Asio, Postgres via libpqxx, Ed25519 JWTs |
| Client | `electron-client/` | Electron; React + zustand + Tailwind v4 renderer; Rust napi-rs native backend (`electron-client/native/`, tokio, prost) |
| Wire contract | `proto/messages.proto` | Shared by C++ (protoc) and Rust (prost via `native/build.rs`) |
| Community e2e harness | `src/community/tests/` | Standalone build + Python black-box tests (see its README) |

`tauri-client/` and `src/client/` are the retired Tauri-era client — don't build on them.
`.github/workflows/release.yml` is the dead Tauri workflow (fires on `v*` tags); the live
one is `electron-release.yml` on `ev*` tags.

## Wire-contract rule (bites every feature)

Any change to `proto/messages.proto` must be carried through all three sides:
1. **Community**: `src/community/tests/setup_deps.sh` regenerates the C++ pb into the deps
   dir; the tracked `src/common/proto/messages.pb.*` are stale and regenerated at build time.
2. **Central**: same protoc regen on the host (it is built on the Hetzner box, not locally).
3. **Rust**: `npx napi build` regenerates prost. Prost struct literals are **exhaustive** —
   every `ChannelMessage { .. }` / `DirectMessage { .. }` literal in `native/src` must gain
   any new field or the build fails. Renderer payload types are hand-written in
   `electron-client/src/types/index.ts`; mirror new fields there.
- napi args: fields declared non-`Option` in Rust (e.g. `beforeId`) are **required** at
  runtime — omitting them from an `invoke()` fails silently-ish. Large `i64` event fields
  can arrive as `BigInt`; coerce with `Number()` at the listener.

## Verification bar (do all that apply before committing)

```sh
# Renderer typecheck (0 errors expected)
cd electron-client && npx tsc -p tsconfig.web.json --noEmit
# Native (also regenerates prost + index.d.ts)
cd electron-client/native && npx napi build --platform --js index.js --dts index.d.ts
# Native unit tests (transport / crypto / STUN / punch / voice sim)
cd electron-client/native && cargo test --lib
# Community server build + e2e (proto regen included) — see src/community/tests/README.md
cd src/community/tests && D=/tmp/decibell-deps && ./setup_deps.sh "$D" ../../.. \
  && ./build.sh "$D" ../../.. "$D/community_server" \
  && DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/community_server" DECIBELL_E2E_RUN="$D/run" python3 e2e.py
# Central server build + e2e against a bundled PostgreSQL — see src/server/tests/README.md
cd src/server/tests && D=/tmp/decibell-central-deps && ./setup_deps.sh "$D" ../../.. \
  && ./build.sh "$D" ../../.. "$D/central_server" \
  && DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/central_server" DECIBELL_E2E_RUN="$D/run" "$D/pgvenv/bin/python" -I e2e.py
```
The system has no libpqxx/boost/jwt-cpp, so IDE diagnostics on `src/server/*` are header
noise; the central harness builds against its own copies (libpqxx 7 + pgserver's PostgreSQL
16 / libpq). A central schema change bumps `central_schema` and gets a migration check: build
the pre-change binary and pass it as `DECIBELL_E2E_SERVER_OLD`. GCC trap seen in production: a
generic lambda (`const auto&`) calling `row[i].as<T>()` needs the `template` keyword — use plain
range-for loops instead. New community e2e checks go in `e2e.py`; per-session message rate
limit is 10 burst / 3 per s, so seed bulk rows via `sql(...)`, not `CHANNEL_MSG`.

## Workflow conventions

- Commit and push straight to `main` after the verification bar passes (feature-sized
  commits). Use a branch for multi-commit refactors that would leave `main` half-migrated.
- Each shipped feature/fix gets an entry in `docs/reviews/2026-08-23-community-server-review.md`
  (the running review + feature log). Design docs go in `docs/superpowers/specs/`.
- Release: bump `electron-client/package.json` (the only version site), push, tag
  `ev<VER>` → "Electron Release" workflow (~30–40 min, all platform assets). Then the AUR
  bump: `aur/PKGBUILD` + `aur/.SRCINFO` (checksums are SKIP), verify with
  `makepkg --printsrcinfo | diff .SRCINFO -`, push to `main`, then push the same two files
  to `ssh://aur@aur.archlinux.org/decibell-bin.git` (`master`). Git identity is not set
  globally — pass `-c user.name='sunkhan' -c user.email='gunhanserhat@gmail.com'`.
  Release builds bake two secrets into `resources/`: `SENTRY_DSN` → `sentry.json` and
  `GIF_API_KEY` (+ the `GIF_API_PROVIDER` variable, default `klipy`) → `gifs.json`; a dev
  checkout uses the same gitignored files or env vars (HANDOFF §5.9a).
- Wire changes that add a **community→client** packet or field ship client-first safely
  (older clients ignore them); anything central must *populate* (e.g. the invite preview
  fields) waits for the Hetzner rebuild — the client degrades gracefully until then.

## Standing decisions (don't "fix" these)

- Server-mute stops a member talking but their stream keeps its audio — intentional. Same for
  a member without SPEAK.
- **Permissions v3** (`docs/superpowers/specs/2026-10-09-permissions-v3-design.md`): voice moves
  check only the mover (MOVE_MEMBERS in the target's channel + VIEW on the destination); a target
  who can't view / join it gets a runtime voice pass, by owner decision. "Members with this role
  can manage each other" covers nicknames + voice moderation only, never kick / ban / timeout /
  roles. Categories are plain groups unless "Channels follow this category's permissions" is on;
  only an explicit sync (incl. the Sync / Keep / Cancel prompt on dropping a channel into a syncing
  category) changes what a channel allows. Moves, switching sync off and deleting a category keep
  every channel's permissions.
- No per-user storage quota; only the free-space headroom check before accepting uploads.
- Backend identity is the **uid**, not the username, unless the username is the point. Owner rule
  (2026-10-14): every backend, central or community, keys stored rows, filters, ownership checks and
  purges on the account uid so a rename never breaks a feature; resolve names to uids at the request
  edge (e.g. `messages.sender_uid`). Flag username-keyed spots instead of leaving them. The migration
  is staged in `docs/superpowers/specs/2026-10-15-uid-identity-design.md` (central and community done): every wire
  user reference carries `*_uid` next to the name, requests prefer the uid (the name is the typed-name
  fallback), payload names are current names, and rows with an envelope also carry `sealed_*` names
  (what a v1 envelope binds). Never rewrite a stored sent-under name.
- UI snaps to the Design System v1 tokens (`electron-client/src/styles/globals.css`):
  no arbitrary px/shadows; radii via `rounded-*` so themes (e.g. console) can flatten them.
  Buttons copy the settings idiom: `rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover`.
  On/off switches are `components/Switch.tsx` — never hand-roll one (an absolutely positioned knob
  without an inset lands wherever the button centres content, i.e. off the track).
- Server nicknames render through `useDisplayName(serverId, username)`; avatars/colors key
  on the real username.
- The text channel header (`features/chat/ChannelHeader.tsx`: name, topic, members toggle, then the
  search box at the right end, 244 px centred over the 260 px members column — owner's order and
  placement) spans the chat **and** the right panel (members list / search results), which start under it, as in
  Discord (owner, 2026-10-14). MainLayout stacks it above both; it stays 48 px (h-12).
- Message list: `features/chat/RealMessageList.tsx` (real-DOM sliding window, both panels;
  no virtualization — react-virtuoso is gone, don't bring it back). Invariants: measure with
  `offsetTop`, never rects; the placement `useLayoutEffect` has no deps (runs every commit);
  `overflow-anchor` stays auto — Chromium anchors prepends (a programmatic `scrollTop` write
  cancels the wheel animation), the list's math is only the residual; trims cut by pixel
  distance on the side opposite the growth; rows keyed by message identity, never index.
  Design + postmortem: `docs/superpowers/specs/2026-08-25-real-dom-message-list-plan.md`.
- zustand selectors must return stable refs (`?? []`/`.filter` inside a selector loops);
  never combine two store hooks with `&&`.
- **Link previews are unfurled by the client**, in the Electron main process
  (`electron/main/linkPreview.ts`: OG/Twitter/`<title>` + oEmbed, image dimension probe,
  private-network guard, caps, cache) — not by either server. No proto/server work, works
  against every server version; the privacy trade is the Privacy-tab "Show link previews"
  toggle. Invite cards resolve against *our* central and ignore that toggle. Remote images
  are direct `<img src=https:>` (the CSP allows it); `http:` image refs are upgraded.
- **GIF search is KLIPY** (GIPHY supported) via `electron/main/gifs.ts` + `resources/gifs.json`.
  Google shut the Tenor API down on 2026-06-30 — never propose Tenor. Sending a GIF sends its
  https URL as the message text; the link preview renders it, and a message that is only a
  media link hides its URL text (`loneLink`). Content filter defaults to `low`; the
  Privacy-tab "Unfiltered GIF search" sends `off`.
- **Invite links are code-only**: `decibell://invite/<CODE>`; central resolves the code to
  host:port (+ name/description/picture for the card). Older `host:port/code` links still
  parse (`features/servers/inviteLink.ts` is the one grammar).
- **Sends are paced client-side** to the community's per-session message bucket
  (`features/chat/sendPacing.ts`: 10 burst / 2.7 per s mirror of the server's 10 / 3, reset on
  community auth, FIFO per server) — a burst is delayed, never dropped. The server answers a
  refused message with `CHANNEL_MSG_REJECTED {nonce}` so exactly that optimistic bubble is
  withdrawn; a 30 s echo watchdog is the backstop. Keep the client and server numbers in step.
  Optimistic bubbles render faded (`pending`) until the echo; DMs carry `DirectMessage.nonce`.
  Slowmode is mirrored the same way (`features/chat/slowmode.ts`): our echo starts the window, a
  refusal's `retry_after_seconds` corrects it, and the composer holds a send (text kept) and counts
  down instead of letting the server refuse it. Owner and Manage Messages in the channel bypass, as
  on the server. A refused send's text goes back in the message box.
- **DM calls are P2P over the native UDP engine**, wrapped in `MediaSocket` (`Plain` for the
  community relay — byte-identical; `Sealed` = AES-256-GCM + replay window, inner datagram
  unchanged). Central only relays `CALL_SIGNAL` (never persisted, INVITE gated by
  `check_dm_allowed`, per-session bucket). STUN-only, no relay, IPv4; sealed sockets never
  `connect()` (peer learned from the last authenticated datagram); peer PINGs are reflected in
  `MediaSocket::recv`; `sender_id` = own username. A call and a voice channel are mutually
  exclusive (one engine, one mic). `features/call/` + `native/src/commands/call.rs` +
  `native/src/media/{media_socket,call_crypto,stun,punch}.rs`; design in
  `docs/superpowers/specs/2026-08-28-p2p-dm-calls-design.md`.
- **DM messages are end-to-end encrypted** when both users have set up encryption; plaintext
  otherwise, shown as such; a peer whose identity is pinned is never downgraded. Static
  X25519 identities (no forward secrecy — deliberate: history is server-fetched, there is no
  local message store), per-message HKDF + AES-256-GCM, passphrase (Argon2id) backup of the
  private keys on central, TOFU pins + safety numbers. **All crypto lives in native
  (`native/src/e2ee/`)** — the renderer only ever sees decrypted events with `encrypted` /
  `decryptError`, and DM packets go through the ordered crypto worker (never decrypt inside
  `route_packets`: key fetches reply on that same loop). Central stores envelopes opaquely and
  gates the feature with `LoginResponse.e2ee_keys`. Design:
  `docs/superpowers/specs/2026-09-03-e2ee-dms-design.md`. **P2P calls sign their
  ephemeral keys with the same identity** and `call_connect` verifies before deriving media
  keys (fail closed when the peer has keys) — `2026-09-03-authenticated-p2p-calls-design.md`.
- **Community voice channels and streams are MLS-encrypted** (OpenMLS, one group per voice
  session, the community server is only the delivery service). Strict: no keys → no voice
  (renderer opens the passphrase prompt), plaintext media is ignored in a channel session, no
  fallback. Packet headers stay in the clear for the relay; payloads are sealed per sender per
  epoch (`media/frame_crypto.rs`), video at frame level so FEC/NACK are untouched. Stream
  thumbnails stay plaintext on purpose. Adding a media packet type? Add its sealed twin and
  teach the relay. Design: `2026-09-03-mls-voice-channel-encryption-design.md`.
- **Text channels encrypt per channel, default off** (owner decision: server-side search, when
  it ships, must keep working for people who never open settings). **Chosen at creation and fixed
  for the channel's life** (owner decision 2026-10-14, `ChannelCreateRequest.encrypted`). Switching
  an existing channel left mixed history. Switching off let an edit turn a sealed message into
  plaintext (erasing its attachment keys) and stopped key escrow for new members. Don't bring the
  switch back; a new channel is the workaround. The server still refuses a plaintext edit of a sealed
  row, for channels toggled before. Epoch keys escrowed to
  members as identity-sealed blobs on the community server (`e2ee/channel_keys.rs`), never MLS
  here (offline members, readable history, multi-device). The server enforces the wire format
  both ways. Channel names and topics stay in
  the clear, by owner decision (2026-10-14): they label the channel, not the conversation. The
  channel-settings topic field says so for encrypted channels. Design:
  `2026-09-04-encrypted-text-channels-design.md`.
- **User statuses are central's** (online / away / dnd / invisible; `users.presence_status`), shown
  to every signed-in user. Others see only `PRESENCE_DELTA` / snapshot entries; an invisible user is
  absent, never "invisible" on the wire. Invisible is airtight by owner decision: every
  presence-revealing path asks `is_user_visible` (friend lists, call INVITE → PEER_OFFLINE), friends-only
  DMs are saved on the account so the DM gate answers the same online or not, and clients tell each
  community `presence_hidden` / `PRESENCE_VISIBILITY` so rosters list them offline. Auto-away is a
  session idle flag (user threshold, default 10 min, + lock/sleep); DND only silences the call ring /
  flash. Member lists show central's status, the community's `is_online` only before the first snapshot.
  Design: `docs/superpowers/specs/2026-10-11-user-status-design.md`. Custom status messages (emoji + 128
  chars, clear-after) ride the same presence entries, so others see them only while the user is visible;
  DM list rows keep the last-message preview instead (`2026-10-12-status-messages-design.md`).
- **Message search runs on the community server** (spec `2026-10-14-message-search-design.md`): an FTS5
  contentless-delete index kept by triggers (plaintext channels only), queried on a dedicated worker
  thread with its own read-only connection, never on the io thread. Every query walks an index in id
  order under a 3 s budget; counts stop at 10,000 or 250 ms ("N+"); one search per session, a new
  one supersedes. Owner decisions: encrypted channels get filters only (text never searched, skipped
  channels reported); senders match by uid; Discord-style UI (`features/search/`: header box + chips
  + results panel in the member list's place); a search covers every readable channel by default;
  below 1024 px the panel floats over the chat and Jump closes it; **no recent searches** — nothing
  about a search is stored on the device. The community server links its own pinned SQLite
  (`cmake/DecibellSQLite.cmake`, 3.53.4 amalgamation with FTS5, fetched by CMake): the index needs
  ≥ 3.43 and the Hetzner box's Debian 12 ships 3.40. Don't switch it back to the system library.
  The index stores the dotless ı as i (ı and i match each other, for Turkish; `search_index_version`
  2). "Inside words" (`match_within_words`, a chip/switch) is a bounded **scan** with the worker's
  `decibell_fold`, by measured choice: a trigram index cost +36–104% disk and ~25× per send.
- **Server-settings search runs on the community server** (owner decision): `MEMBER_SEARCH_REQ` for
  the Members tab / a role's holders / the add-members picker, `AuditLogRequest.query/actions` for the
  audit log, `RoleInfo.member_count` for role counts. Never filter only the loaded pages, and keep
  search results out of the roster store (`tabs/useMemberSearch.ts` owns them, patched by roster
  deltas). Unsaved role edits shake the save bar on a role switch; they're never silently dropped.
- **Channel settings** (`ChannelSettingsModal` + `features/servers/channelSettings/`) is the settings
  window with tabs, one draft and the RolesTab save bar. Owner decisions (2026-10-14): leaving with
  unsaved changes shakes the bar, never drops them; a channel's Permissions list only the overwrite
  bits for its type (categories keep all); delete and wipe use a plain `ConfirmModal`, no typed name.
- **Settings saves settle on the server's echo** (`features/servers/saveEcho.ts`, owner request
  2026-10-14): after Save the draft stays ("Saving…") until the store shows what was sent. A refusal
  or 5 s ends the wait and keeps the draft. Never clear a draft when the request goes out. Text
  fields clamp to the server's byte caps as typed (`lib/utf8.ts`), so a server-side clamp can't
  stop the echo from matching. Unsaved edits hold the window (owner, 2026-10-14): in Server Settings,
  Esc / ✕ / a backdrop click / a tab switch shake the open tab's `SaveBar` (`leaveGuard.ts`), as
  channel settings does on close.
- **Settings windows share one frame** (`components/SettingsWindow.tsx`: Settings, Server Settings,
  Channel Settings). Below 1024 px (`max-lg:`) the window goes nearly edge to edge and the sidebar
  folds into a 64 px icon rail; each icon shows its name on hover / focus. Owner decisions
  (2026-10-14): keep the app's 800×600 minimum window, switch at 1024 px, per-icon labels (no rail
  that slides open). Build new settings windows on this frame, not on a copy of it.
- **Attachments in encrypted channels are sealed client-side** (random key per file, 64 KiB
  AES-256-GCM chunks, sealed thumbnails; `features/chat/attachmentCrypto.ts` encrypts,
  `electron/main/attachmentCrypto.ts` + `attachmentFetch.ts` decrypt in the protocol / media
  server / netFetch paths — the two modules must stay byte-for-byte in step). Real metadata +
  key ride the message envelope (content tag 0x03, `EncryptedMessageBody`); the server keeps
  stand-ins + `attachments.encrypted` and binds only uploads whose flag matches the channel.
  The server still learns kind, ciphertext size and thumbnail sizes — accepted.

## Deeper docs

- `ARCHITECTURE.md` — system overview.
- `electron-client/HANDOFF.md` — canonical Electron client hand-off (read when resuming client work).
- `src/community/tests/README.md` — e2e harness usage.
- `docs/reviews/2026-08-23-community-server-review.md` — review findings + everything shipped since, with rationale.
