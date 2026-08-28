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
```
The central server has no local libpqxx/boost/jwt-cpp: IDE diagnostics on `src/server/*`
are header noise, and central changes are verified by inspection (+ a mocked-pqxx g++
syntax check for anything template-shaped). GCC trap seen in production: a generic lambda
(`const auto&`) calling `row[i].as<T>()` needs the `template` keyword — use plain
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

- Server-mute stops a member talking but their stream keeps its audio — intentional.
- No per-user storage quota; only the free-space headroom check before accepting uploads.
- Backend identity is the **uid**, not the username, unless the username is the point.
- UI snaps to the Design System v1 tokens (`electron-client/src/styles/globals.css`):
  no arbitrary px/shadows; radii via `rounded-*` so themes (e.g. console) can flatten them.
  Buttons copy the settings idiom: `rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover`.
- Server nicknames render through `useDisplayName(serverId, username)`; avatars/colors key
  on the real username.
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
- **DM calls are P2P over the native UDP engine**, wrapped in `MediaSocket` (`Plain` for the
  community relay — byte-identical; `Sealed` = AES-256-GCM + replay window, inner datagram
  unchanged). Central only relays `CALL_SIGNAL` (never persisted, INVITE gated by
  `check_dm_allowed`, per-session bucket). STUN-only, no relay, IPv4; sealed sockets never
  `connect()` (peer learned from the last authenticated datagram); peer PINGs are reflected in
  `MediaSocket::recv`; `sender_id` = own username. A call and a voice channel are mutually
  exclusive (one engine, one mic). `features/call/` + `native/src/commands/call.rs` +
  `native/src/media/{media_socket,call_crypto,stun,punch}.rs`; design in
  `docs/superpowers/specs/2026-08-28-p2p-dm-calls-design.md`.

## Deeper docs

- `ARCHITECTURE.md` — system overview.
- `electron-client/HANDOFF.md` — canonical Electron client hand-off (read when resuming client work).
- `src/community/tests/README.md` — e2e harness usage.
- `docs/reviews/2026-08-23-community-server-review.md` — review findings + everything shipped since, with rationale.
