# Decibell Electron Client — Resume Handoff

**Read this entire document before touching the client.** It's the hand-off that lets a fresh
conversation resume client work without inheriting false assumptions. First written 2026-05-08 at the
end of PR8 (the Tauri-to-Electron migration). Kept current: the release table (§2), the gotchas (§5),
how to build and test (§6), the open items (§7), the file map (§8) and the recent commits (§9) —
**last refreshed 2026-11-07 (0.9.4)**.

Companions: `ARCHITECTURE.md` (system + wire reference, and §7.3 on reusing the native crate in a
native client), `docs/FEATURES.md` (every feature and where it lives), `CLAUDE.md` (rules + the owner's
standing decisions), and the running feature log `docs/reviews/2026-08-23-community-server-review.md`.

The owner's Claude Code auto-memory on the Linux dev box
(`~/.claude/projects/-home-sun-Desktop-decibell/memory/`) held a condensed copy of much of this plus a
set of gotchas; on 2026-11-07 every durable note from it was folded into this file (§5.13–§5.15, §6, §7)
and CLAUDE.md, so a checkout on another machine loses nothing.

---

## 1. What Decibell is

Decibell is a self-hostable, Discord-like app (see CLAUDE.md for the tier table):
- **Central server** (`src/server/`, C++, PostgreSQL): accounts, sign-in, devices, DMs, friends,
  presence, the community directory and invites, E2EE key directory, call signalling.
- **Community server** (`src/community/`, C++, SQLite): one per hosted community: channels, messages,
  roles, voice relay, streams, search, attachments.
- **Shared protobuf wire** in `proto/messages.proto`.
- **Client**: this Electron app (`electron-client/`): React renderer + Rust napi-rs native addon.

The **only** client that ships is this one. The QML client (`src/client/`) and `tauri-client/` are
historical; don't modify them or keep compatibility with them. The servers do change, usually together
with the client (wire changes go through all three sides; CLAUDE.md, "Wire-contract rule").

---

## 2. Migration status — what's done, what's in progress

The migration is sequenced into PRs. Status as of this hand-off:

| PR | Scope | Status |
|----|-------|--------|
| PR1 | Scaffold + ping smoke test | ✅ done |
| PR2 | Tokio runtime + AppState + EventBus + IPC shim + sounds.rs port | ✅ done |
| PR3 | Network + auth (login/register, central + community connections) | ✅ done |
| PR4 | Channels + text chat + history | ✅ done |
| PR4.5 | UI parity pass — full visual match to tauri-client layout | ✅ done |
| PR5 | Voice pipeline (audio capture/playback/mixing) | ✅ done |
| PR6 | Attachments + chat polish (uploads, image viewer, emoji) | ✅ done |
| PR7a | Streaming foundation — native code in place | ✅ done |
| PR7b | Cross-platform + GPU + extra codecs (wlr-screencopy, DMA-BUF, Windows captures) | ✅ done |
| PR7c | Per-stream Buffer TSFN for encoded frames (binary IPC) | ✅ done |
| PR8 | Move video encode renderer-side via WebCodecs | ✅ done |

The migration is complete and shipping — Electron is the client, and
`tauri-client/` is legacy (unmaintained; changes here are not ported).
Everything listed as "remaining" in the PR8-era version of this section
has since landed: settings persistence + modal, the persistent
video/audio layers, image context menu, channel management,
electron-builder packaging, electron-updater, and the deep-link handler.

**Sections 3–6 below are PR8 history.** They remain accurate about the
streaming architecture and its gotchas, which is why they're kept — but
read them as "how streaming got here", not as current open work.

### Shipped since PR8 (see git log for detail)

| Release | Scope |
|---------|-------|
| 0.6.x | persistent DMs, message deletion, custom avatars + server pictures, auto-rejoin, crash reporting (Sentry), AUR packaging |
| 0.6.10 | five-palette theme system + Appearance settings tab; Friends home screen; chat-scroll fixes; ThumbHash attachment placeholders |
| 0.6.11 | attachment scroll glitch fixed for real (decode-side, B12–B15); streaming fix round — per-sender wire receiver, stream-audio A/V sync, Windows/AMD fallback + escape hatch, 120fps codec levels, fresh-launch watch-lock fix |
| 0.7.0 | roles/permissions with Discord-style hierarchy; channel management — create/rename/delete, categories, drag-reorder; nicknames + unban; unified server-settings screen (Overview/Members/Roles/Bans); per-channel voice bitrate incl. live mid-call retune; cross-server channel-key fix; community-server hardening pass (owner seeding, zombie sessions, inert central-sync deadlines, wire validation) |

| 0.7.1 | audio-device hotplug sync (roster store + mid-call re-resolution); stream carry-over on same-server channel switch ("take stream with me"); deafened-listener relay skip; server-settings modal animation; light-theme stream controls; mouse back/forward no longer walks history |
| 0.7.2 | seamless stream mini-player (resizable, flick-to-corner) + watch-path hardening, viewer count + stats; stream stall recovery + HW decode; nicknames everywhere |
| 0.7.3 | Theme A security — Ed25519 JWTs, separate community secret, uid, cert pinning; permissions v2 (Authorizer, per-channel overwrites + editor); server management + moderation (rename, audit log, timeouts, ban expiry/purge, slowmode, voice moderation, ownership transfer); roster deltas (paged member list, live upsert/remove); community fix batches 13–14 (rate limiting, deadlines, perm/statement cache, central-sync worker); central ghost sessions + directory expiry |
| 0.7.4 | voice: consumer-paced playback rings, PLC on underrun, gated tail frames (the 0.7.1+ pops) |
| 0.7.5 | voice: gate-edge shaping — pre-roll + faded tails on send, fades on receive |
| 0.7.6 | chat: embedded reply previews, exact far jumps + jump-to-present pill, attachment-only reply labels; plain-text DM previews |
| 0.7.7 | design pass — inset workspace panel, 64px server bar with hover-reveal pictures, unread-DM tiles replacing the DM rail, `font-meta`/`font-tile` tokens, ConfirmModal shell, remembered layout (sidebar width, mini-player size/corner, panel toggles); real-DOM message list is the only list (react-virtuoso removed) |
| **0.7.8** | **P2P voice calls + screen share in DMs** — `CALL_SIGNAL` relay on central, sealed (AES-GCM) UDP transport with STUN + hole punch in native, call UI in the DM (`features/call/`; design `docs/superpowers/specs/2026-08-28-p2p-dm-calls-design.md`); hyperlinks + link previews (main-process unfurl, Privacy toggle); GIFs tab in the picker over KLIPY/GIPHY + animated GIF attachments + unfiltered toggle; invite cards in chat + code-only invite links (`decibell://invite/<CODE>`); spam-burst fix — `CHANNEL_MSG_REJECTED` with nonce, client-side send pacing, 10/3 bucket; pending messages fade until echoed (DMs gained optimistic bubbles via `DirectMessage.nonce`) |
| **0.7.9** | **call stage** — the P2P call redesigned as a stage at the top of the DM (Direction A of the DM Call Stage canvas): tiles → focused stream → theater → fullscreen, one control dock; per-peer local mute + volume in calls; mini-player click returns to the DM; controls on theme radius tokens; fullscreen no longer under the user panel; UDP payload cap unified at 1200 with bitrate guards (server 320 kbps clamp) |
| **0.7.10** | call fixes from the second live test — stream engine teardown races (scoped frame sink, synchronous watched-streams clear, HANGUP on quit), stream audio gated on watching, stage host leak; resizable call stage (drag the bottom edge, per-mode heights); live shares as cards in the tile row; 5 s teardown watchdog log for the Windows native path |
| **0.7.11** | server live-test fixes — forced voice move now reconciles the moved client (roster/stream/watch state rebuilt from the per-channel caches; was: users shown in two channels, empty room on move-back, zombie encoder); **borderless screen share on Windows 10** — monitor capture via DXGI Desktop Duplication with manual cursor compositing (`capture_dxgi.rs` + `cursor_blend.rs`), WGC stays wherever `IsBorderRequired` exists (Win11+); Chromium WGC capturers disabled for the renderer-encode fallback; stream watch re-requests the first keyframe until it paints (lost-PLI spinner fix) |
| **0.7.12** | DXGI capture pacing fix — frames arriving before a paced send were released without a copy (Desktop Duplication hands each frame out once), so a 60 fps game share went choppy whenever the mouse stood still; now a `clean` desktop texture is updated on every content frame and clean + cursor are copied into the ring per send. Mini-player / live-thumbnail "back to stream" from Home or a DM re-opens the stream's server (was: "Server · Private", empty channel list). Windows Native Check CI workflow (cargo check on windows-latest, log on `ci/win-native-check-log`) |
| **0.7.13** | Win10 DXGI share held ~32 fps with the cursor still: the per-send CPU cursor blend (staging `Map`) waited out the game's GPU queue — cursor compositing moved to the GPU (`cursor_gpu.rs`, one alpha-blended quad; `cursor_blend.rs` now just shape→BGRA), capture loop has no CPU↔GPU sync; every capture path paces to the configured 120/60/30/15 fps (WGC own-ring pacer + Win11 24H2 `SetMinUpdateInterval`, renderer pump deadline gate); per-second `[capture_dxgi] 1s:` telemetry |
| **0.7.14** | Multi-app audio picker: Share audio now lists the apps with an audio output under a Selected apps / All except / All apps rule, in Go Live and live via a speaker button next to Stop; ticks persist by program name and re-plan the running capture at once. Linux: filter-aware PipeWire tap (also fixes Decibell's own ALSA node leaking into the stream); Windows: process-loopback mixer (one WASAPI client per allowed app, summed). Window sources no longer auto-scope audio — the picker owns the rule |
| **0.8.0** | The encryption release. E2EE DMs (static X25519 identities, per-message HKDF + AES-256-GCM, passphrase backup on central, TOFU pins + safety numbers); P2P calls sign their ephemeral keys with the same identity (fail closed); community voice channels + streams MLS-encrypted (OpenMLS, one group per session, server = delivery service, strict, thumbnails plaintext); text channels encrypt per channel (default off, epoch keys escrowed to members); attachments in encrypted channels sealed client-side (64 KiB AES-GCM chunks, sealed thumbnails, metadata + key in the envelope). All crypto in native `e2ee/` |
| **0.8.1** | E2EE cues: no per-message lock glyph (channels + DMs), an **E2EE** pill in encrypted channel headers, reworded encryption copy. Central fix shipped alongside (not in the client): the key-publish insert typed one parameter two ways. No wire change |
| **0.8.2** | Usage-pass fixes: AGC on by default (and a fresh install now gets the serde defaults — `AppSettings` derived `Default` had crash reporting / link previews / AV1 starting *off*); context menus measured and flipped inside the window (`hooks/useMenuPosition.ts`, all four menus); the full profile screen (`features/profile/UserProfileModal.tsx`, Profile + Privacy tabs — safety number, fingerprint, pin state, key-change notice) opened from the popup's avatar; inline media capped at 550 × 350 and GIFs at 440 × 280 (picker GIFs recognised by URL and by the unfurler's `gif` flag). No wire change |
| **0.8.3** | Go Live dialog fix: the stream-start dialog could grow taller than the window (source grid + settings + stream-audio app list ≈ 1100 px) and push Go Live off-screen. Pinned header + footer with a scrolling body; side-by-side sources / settings on wide Windows windows; settings grouped Video / Audio (Share audio heads the controls it gates); Escape closes. No wire change |
| **0.8.4** | Smoothness pass + Windows/AMD streaming. Renderer: unseen channels capped at 150 messages (a busy channel no longer mounts its whole backlog on return), the draft lives only in `draftsStore` (no list pass per keystroke), per-channel selectors, the composer's per-keystroke slowdown fixed (caret sentinel only when there's something to tokenize), composited `.drop-pulse`, memoised sidebars. Streams: frames only where displayed (`set_stream_frame_sink`), a `discontinuity` flag on dropped frames, renderer-encoded frames sent off the main thread, the stream-queue leak fixed. Windows/AMD: forced keyframes are real IDRs (`forced_idr` / `forced-idr`), parameter sets on every keyframe, the D3D11 video processor sized from the input with explicit rects (letterbox), device on the encoder vendor's adapter, no QSV in the probe, amfrt64 pinned; WGC/DXGI capture survive resizes, access loss and dead sources; the WebCodecs fallback actually sends video (Annex B H.264, true codec announced); watchers rebuild a decoder closed by an error. No wire change |
| **0.8.5** | Voice view redesign + sharper stream thumbnails. `VoicePanel` is a stage of 16:9 tiles: avatar-tinted participant tiles (`--tile-tint`), a status chip, an instant speaking outline. The grid best-fits the space; past a 200 px floor the tail collapses into "+N more" with a popover, and hidden speakers swap in (0.6 s of speech, at most one swap per 3 s; `stage/stageLayout.ts`). Streams sit large above a one-row people strip. Watch / Switch / Watch too / Focus / Stop on stream tiles (`stage/streamActions.ts`). The focused view has scrim overlays, a filmstrip and a breadcrumb header. A floating dock replaces the button bar. The grid tile hosts the persistent player (StreamPipManager's 20 s grid-idle drop is gone). New tokens: `--color-on-error`, `--color-live`. Thumbnails are 960 px (JPEG ≤ 256 KB) at start, +2 s, then every 15 s; the native paths encode on a worker (`media/thumb_encode.rs`), and the voice view fetches the cached one for late joiners. No proto change, but the **community server** needs the rebuild for the 256 KB thumbnail cap (an older one drops anything over 128 KB) and the 3 / 1-per-5 s thumbnail bucket |
| **0.8.6** | Nocturne / Matinee become the default look (Mona Sans UI, brass on plum, plum accent in light). A Transfers panel in the title bar: a downloads manager (pause / resume over Range, encrypted files included, Show in folder) and an Uploads tab (pause, Go to message, taskbar progress, a quit guard while uploads run). The Settings → Network speed caps now actually apply to attachments (downloads paced in main, uploads in the PATCH loop). Chat: a download button beside image / video attachments, day dividers between dates, a members-list toggle in text-channel headers; click a focused stream to go back to the grid; channel / DM lists end above the user panel, bigger channel-type icons, offline members light up on hover. Fix: **Linux native H.264 streams showed nothing to watchers** (AVCC with no description on the wire) — now Annex B like every other path. No proto change; the **community server** rebuild (schema v10, `attachments.last_activity_at`) makes the abandoned-upload sweep idle-based — until then an upload paused for over an hour fails as "expired" |
| **0.8.7** | Global hotkeys: toggle mute / deafen, push to talk, push to mute, leave voice / hang up, answer / decline a DM call — Settings → Keybinds (recorder, per-platform notes, a "Games won't see it" badge where the key is taken from the app in front) and Settings → Audio → Input Mode (voice activity / push to talk, release delay; the bound key shown). Listeners learn only about bound keys: the GlobalShortcuts portal on Wayland (KDE confirm dialog; mouse side buttons get a remap guide), exact passive grabs on X11 / XWayland, `GetAsyncKeyState` on the bound keys on Windows, accelerators on macOS (holds focused-only); `decibell --hotkey=<action>[:down|:up]` for window managers. Push to talk gates the mic in native (`media/voice_gate.rs`). Permissions v3 client (Create Invite, Change Nickname, Embed Links, Speak, Mute / Deafen / Move split, peer management, opt-in category sync with a Sync / Keep / Cancel prompt; channel permission edits go through Save). Settings windows float clear of the edges and shrink on small windows; one shared on/off `Switch` (Public listing's knob sat off the track); no wordmark under the members list; mouse back / forward no longer walk between screens (or back to login). Windows / macOS: "Default" audio devices follow the OS default device (`media/default_device_watch.rs`); the separate-stream toggle keeps the chosen voice output. Proto (permissions v3): `CHANNEL_PERMISSIONS_SYNC_REQ` (147) + `ChannelPermissionsSyncRequest`, `ChannelInfo.permissions_synced` (13) / `suppress_embeds` (15), `is_suppressed`, the new permission bits — the **community server** must run permissions v3 first (an older one lacks SPEAK, so the mic reads as blocked) |
| **0.8.8** | Voice fixes + themed pickers. A member moved to another voice channel now joins that channel's MLS group (native rebinds the group on `VOICE_FORCE_NOTIFY{MOVED}`: same engine and key ring, new driver seeded with the cached roster) — before, nobody in the destination could hear them. Sidebar voice rosters: leaving voice no longer blanks every other channel's members (presence is community state, kept across voice sessions), presence is keyed by server + channel (slug ids collided across communities), other channels' rows use their own server (nicknames, the context menu's Move / Mute when not in voice) and show LIVE, join / leave sounds re-baseline per channel, a switch shows the cached roster at once, stale rosters are cleared at auth / connection loss / channel-list pushes. Drag a voice member onto another voice channel to move them (MOVE_MEMBERS + hierarchy; `voice/memberDrag.ts`); right-clicking a voice participant opens only the user menu. Every dropdown is a themed `components/Select.tsx` (lacquer / crisp / terminal per theme family, keyboard parity, filter past 10 options); the user menu's "Move to…" unfolds in place. Recently shown attachment images stay in memory across channel switches (`chat/imageRetention.ts`, 128 MB est. — GC used to drop them); the Network → Channel cache setting is gone (fixed at 10); dead cache code removed and attachment fetches use `cache: "no-store"`. No proto change; the **community server** rebuild re-sends the presence snapshot after a channel-list push (until then a channel that becomes visible shows no members until it changes) |
| **0.8.9** | **User statuses**: online / away / do not disturb / invisible, owned by central and shown to every signed-in user; auto-away (adjustable threshold + lock), Wayland idle through native `ext-idle-notify`; DND silences the call ring. **Reply redesign** (spine line, name → profile, line → jump). Custom status messages (emoji + 128 chars, clear-after). Server settings redesign: Members / Roles / Audit Log with server-side search (`MEMBER_SEARCH_REQ`, audit `query` / `actions`, `RoleInfo.member_count`), custom role colours. Channel settings window (one draft, Roles-style permissions), saves settle on the server's echo, unsaved edits hold the window. Welcome banner actions (Invite friends with personal invites, Manage channel), channel topics. Slowmode pill + countdown. **Message search** (community FTS5 index + worker, header box + chips + results panel; ı ↔ i, "inside words"). Encrypted text channels chosen at creation, fixed for life. Channel header spans the members list |
| **0.9.0** | **Users by uid** (stages 1–5): every backend keys users on the uid, every wire user reference carries `*_uid`, encryption binds the uid (bundles `binding` 2, DM envelopes `0x03`, channel envelopes `0x04`, call auth v2, MLS credentials `"<uid>"`), the renderer keys every user store on the uid (`usersStore`). **Username changes** (two a day, 14-day reservation, live: `SESSION_TOKEN_UPDATE` renames the community session in place, `TOKEN_REFRESH_REQ`). **Central ready to scale**: io threads + strands, PgPool, `SessionManager`, presence by subscription (`PRESENCE_SUBSCRIBE`, watchers only). Offline people's pictures. **Central hardening**: resume tokens on reconnect (no bcrypt storm), abuse limits (paced, never dropped), fd limits; `load.py` |
| **0.9.1** | **Pre-1.0 review fixes** (all of §1 of `docs/reviews/2026-10-17-pre-1.0-review.md`: S1–S17 security incl. the UDP bind challenge S11, A1–A4 availability, B1–B13 bugs: reconnect re-sync, paged online roster + windowed member sidebar, community requests paced instead of dropped, encrypted channels of any size keyed). **Several devices per account** (device ids, up to 8 online, own actions synced, calls ring every device). **Device list**, per-device / all-others sign-out (reaches communities via the heartbeat), **password change** |
| **0.9.2** | **Go Live redesign**: quality presets (Smooth / Sharp / Data saver), sound and cursor up front, encoder settings under Advanced; "Source" encodes the source's own size on the native paths; 5 fps under Advanced with a media path that holds up at low frame rates. The DM call in the voice view's design (tiles, call bar, dock). Talking in voice keeps you from going Away. Opening an unloaded DM / channel goes straight to its messages; the avatar cut-out follows the picture |
| **0.9.3** | **Image viewer** redesigned (themed stage, fly-in / out, real-pixel zoom + minimap, gallery = the clicked message, Copy image through main) and the **picture editor** for avatars and server pictures (`components/pictureEditor/`; EXIF-safe crop). The **Slides** preset (full res, 5 fps encoded for detail). Go Live's Options panel beside the dialog on wide windows. Every jump lands (not every other one). `npm run dev` compiles main first |
| **0.9.4** | A **12- / 24-hour time format** setting (Appearance; `lib/timeFormat.ts`). Switching voice channels shows you in the new one at once: a SelfMove queue corrects rosters for our own join / leave, `enterChannel` does the switch in one update, native sends LEAVE / mute / JOIN / GroupInfo before starting the engine, and the MLS identity is prefetched |

**0.9.4 is the current release** (2026-11-06). No proto change since 0.9.1, whose wire additions were
the multi-device / devices packets (168–178), `LoginRequest.device_id` / `automatic` / device fields and
`LoginResponse.device_limit` / `signed_out` / `devices`, `ServerHeartbeatRes.revoked`, and the UDP
`BIND_CHALLENGE` / `BIND_ECHO` (10 / 11). Releases ship the client first; the owner rebuilds central (Hetzner)
and the community server afterwards, so a new client runs against the previous servers for days (never act
destructively on a field an older server omits — §5.13). Which server build is live isn't recorded in the
repo: ask the owner. Deploy to-do carried since 0.9.1: **rotate central's signing key** when the
pre-1.0-review build goes live (review S1 leaked session tokens in relays).

### Per-release wire and deploy notes, 0.7.14 – 0.8.8 (history)

0.8.8 (current at the time): moved voice members can be heard in their new channel (MLS group rebind), the sidebar's voice rosters survive leaving voice and stay per server, drag-to-move for voice members, themed dropdown pickers everywhere, and images that stay loaded across channel switches — no proto change; the community server rebuild re-sends presence when a channel becomes visible; see the 2026-10-10 entries in docs/reviews/2026-08-23-community-server-review.md. 0.8.7 was the release before it: global hotkeys (mute / deafen / push to talk / push to mute / leave / answer / decline; bound keys only on every platform — design in docs/superpowers/specs/2026-10-10-global-hotkeys-design.md), the permissions v3 client (needs the community server running v3), floating settings windows, one on/off switch, mouse back / forward no longer navigating, and "Default" audio devices following the OS default on Windows / macOS; see the 2026-10-09 / 2026-10-10 entries in docs/reviews/2026-08-23-community-server-review.md. 0.8.6 was the release before it: the Nocturne / Matinee look as the default, the Transfers panel (downloads + uploads with pause / resume), attachment speed caps that actually apply, day dividers / download buttons / a members-list toggle in chat, and the Linux native H.264 fix (watchers saw nothing) — no proto change; the community server rebuild (schema v10) makes the abandoned-upload sweep idle-based. 0.8.5 was the release before it: the voice view redesign (tiled stage, floating dock, "+N more" with hidden-speaker swap-in, Switch / Watch too for several streams) and 960 px stream thumbnails every 15 s — no proto change, but the community server needs the rebuild for the 256 KB thumbnail cap; see the 2026-10-04 entries in docs/reviews/2026-08-23-community-server-review.md and docs/superpowers/specs/2026-10-04-voice-view-redesign.md. 0.8.4 was the release before it: the smoothness pass (bounded background channels, no list re-render per keystroke, composited animations, frames only where displayed) and the Windows/AMD streaming fixes (real IDRs and in-band parameter sets on AMF, a correctly sized D3D11 video processor, capture recovery, a working WebCodecs fallback) — no wire change; see the 2026-10-02 entries in docs/reviews/2026-08-23-community-server-review.md. 0.8.3 was the 0.8.2 client with a Go Live dialog that always keeps its button on-screen (pinned header / footer, scrolling body, two columns on wide Windows windows) — no wire change. 0.8.2 was the 0.8.1 client with AGC on by default, context menus that stay inside the window, the full profile screen (Privacy tab = the E2EE details) and the 550 × 350 / 440 × 280 inline-media caps — no wire change. 0.8.1 added the E2EE channel-header pill, dropped the per-message lock glyph and reworded the encryption copy. 0.8.0 was the encryption release (see §5.12 for how it hangs together). Proto additions: `E2EE_*` (129–135) + `LoginResponse.e2ee_keys`, `CallSignal.pub_key_sig`/`key_id`, `MLS_*` (136–141) + sealed UDP types 7/8/9, `ChannelInfo.encrypted` (12) + `ChannelUpdateRequest.encrypted`, `envelope` fields on channel / DM messages, edits and reply previews, `CHANNEL_KEYS_*` (142–146), `Attachment.encrypted` (17) + `EncryptedMessageBody`. All additive on the wire, but the features need the **community-server release** (MLS delivery service, channel key escrow, attachment flag — against an older community server voice channels don't work at all: the client sends only sealed media and ignores plaintext, by decision) and a **central rebuild** (key directory, passphrase backup, signed call keys — until then DMs stay plaintext and calls run unauthenticated).

0.7.14 was the release before it (0.7.13 GPU cursor compositing + fps pacing on every capture path; 0.7.14 the multi-app audio picker + Linux self-leak fix). It carries **four proto additions** —
`InviteResolveResponse` preview fields (7–11), `CHANNEL_MSG_REJECTED`
(type 127 / payload 129), `DirectMessage.nonce` (10), and `CALL_SIGNAL`
(type 128 / payload 130) + `LoginResponse.stun_servers`/`call_signaling`
(5–6) — all additive: older clients ignore them. Deploy the community
server for the typed rejection + the 10/3 bucket, and rebuild central
(protoc regen on the Hetzner box) for invite-card metadata, the DM
error-reply nonce and call signaling; until then the client degrades to
host:port cards, a 30 s watchdog on pending bubbles, the legacy toast, and
a disabled Call button. The 0.7.x design
records are `docs/superpowers/specs/2026-08-17-roles-permissions-design.md`,
`2026-08-22-*`, and `2026-08-25-real-dom-message-list-plan.md`; the
running feature log is `docs/reviews/2026-08-23-community-server-review.md`.

---

## 3. PR8 architecture — what we changed and why (history, 2026-05)

> §3 and §4 record the PR8 migration. The architecture they describe still holds for the WebCodecs
> path, but native capture + hardware encode came back later for Windows and Linux (ARCHITECTURE §7.2),
> and §4's "open issue" is long past: streaming has shipped and been used since 0.6.

### Why PR8 exists

PR7 worked end-to-end conceptually but ran into a hard architectural wall:

- Electron bundles its own `libffmpeg.so` (stripped libavcodec, ~15
  codecs, version 61.5.x) and loads it eagerly at startup
- Our addon's `ffmpeg-next` calls bound to those globally-resolved
  symbols, so `find_by_name` returned `None` for every codec including
  libx264
- `RTLD_DEEPBIND` got `find_by_name` working but produced
  allocator-mismatch SIGSEGVs in unrelated voice code (cpal/PipeWire)
  because dual-loaded libav* / libpulse / libasound chains shared
  inconsistent state. The cure was worse than the disease.

### What PR8 does

Gut the native FFmpeg-encode path entirely. Move encode to Chromium's
`WebCodecs.VideoEncoder` in the renderer:

```
getDisplayMedia → MediaStreamTrack → MediaStreamTrackProcessor →
  ReadableStream<VideoFrame> → VideoEncoder.encode → encoded chunk →
  IPC `send_video_frame` to native → packetise + UDP
```

- Capture via `getDisplayMedia` (Chromium drives the OS screen-share dialog)
- Encode via `VideoEncoder` with `hardwareAcceleration: 'prefer-hardware'`
- Receive-side (`video_receiver.rs` → `WebCodecs.VideoDecoder`) was
  already in place from PR7c and **didn't change**

### Native deletions (~8000 lines)

`encoder.rs`, `capture.rs` (trimmed to just `AudioFrame`),
`capture_pipewire.rs`, `capture_wlr_screencopy.rs`, `capture_wgc.rs`,
`capture_dxgi.rs`, `gpu_interop.rs`, `gpu_capture.rs`, `gpu_pipeline.rs`,
`bitrate_preset.rs`, `video_processor.rs`, `thumbnail_reader.rs`.
`caps.rs` trimmed to types-only. `video_pipeline.rs` reduced to a single
`VideoSender` struct (~70 lines: packetise + UDP).
`media/mod.rs::VideoEngine` reduced to a thin send-side wrapper.

### Cargo deps removed

`ffmpeg-next`, `image`, `libloading`, `ashpd`, `khronos-egl`, `gl`,
`wayland-client`, `wayland-protocols-wlr`, `memfd`. Native binary
shrinks from ~300 MB to ~50 MB at debug.

### Castlabs Electron

Upstream Electron strips proprietary codec **encoders** from Chromium's
WebCodecs (H.264 + HEVC) — they include decode for HTML5 `<video>` but
not encode. We swapped to the castlabs fork:

```json
"electron": "github:castlabs/electron-releases#v33.4.11+wvcus"
```

This adds OpenH264 software H.264 encode + platform HEVC encoder
support + Widevine. AV1 (libaom) was always available.

---

## 4. State of PR8 when it landed (history)

### ✅ Working

- Native build clean (`cargo build` succeeds, 77 unused-symbol warnings — all in voice-stack code paths that don't yet have command-level callers, none streaming-related)
- Frontend typecheck + Vite build clean
- Electron boots, login/register works, text chat works, voice works, attachments work
- `getDisplayMedia` triggers the OS-native screen-share dialog (Chromium's PipeWire integration on Linux, system picker on macOS, native on Windows)
- `encoderProbe` runs at boot, populates the codec dropdown correctly:
  - AV1 ✓
  - H.264_HW ✓ (falls back to OpenH264 software via `prefer-hardware`-then-software retry)
  - H.264_SW ✓
  - HEVC ✗ on Linux+NVIDIA (expected — needs platform encoder support)
- Encoder construction succeeds for H.264 at any reasonable resolution
- IPC `send_video_frame` accepts `Buffer` (the `Uint8Array` deserialization works)

### 🔄 Open issue (where we stopped)

**AV1 at 1920×1080@60 hits `OperationError: Encoder creation error`**
asynchronously after `configure()` returns. Chromium accepts the config
in `isConfigSupported` but libaom-AV1 can't sustain 1080p60 software
realtime — the encoder construction async-fails.

This is not a code bug. It's a Chromium/libaom limitation. Options for
the next session to choose from:

1. **Lower-resolution AV1** — AV1 at 720p60 / 1080p30 should work. The
   resolution dropdown could be made aware of codec capability and
   silently downgrade.
2. **Use H.264 at high resolutions** — H.264 (OpenH264 software) handles
   1080p60 fine on a fast CPU.
3. **Hardware AV1 via `nvidia-vaapi-driver`** — installing the
   community libva-nvidia-driver on Linux unlocks NVENC AV1 on the
   user's RTX 4080 (Ada Lovelace) through Chromium's VAAPI path. On
   Windows the same castlabs build hits NVENC AV1 directly via Media
   Foundation — no driver work needed.
4. **Auto-fallback in StreamCapture** — when AV1 async-fails, retry as
   H.264. Mirrors the existing prefer-hardware → prefer-software fallback.

**End-to-end stream test was not reached** — i.e., we never confirmed
that an encoded frame goes from the streamer's renderer all the way
through native packetisation, the C++ server, the watcher's native, the
watcher's renderer, and into the watcher's `VideoDecoder`. The encode
side is now plumbed correctly through `Buffer` IPC; whether the wire
path + watcher decode all work is the next testable piece.

### Last unconfirmed assumption

The watcher path (`StreamVideoPlayer.tsx` consuming
`window.decibell.streamFrames.subscribe`) was last touched in PR7c and
hasn't been retested since the PR8 send-side rewrite. There may be wire
format mismatches if AV1/HEVC keyframe `WIRE_DESCRIPTION_MAGIC`
prefixing diverged on either side.

---

## 5. Hard-won gotchas — read before changing this code

These all cost real debugging time. Don't re-discover them.

### 5.1 napi-rs binary IPC

- `Vec<u8>` in napi-rs **expects a JS `Array<number>`**, not `Uint8Array`.
  Sending a `Uint8Array` to a `Vec<u8>` arg gives "Given napi value is
  not an array".
- Use `napi::bindgen_prelude::Buffer` for binary fields. It accepts
  `Uint8Array` zero-copy.
- `Option<Buffer>` does **NOT** accept JS `null` — it tries to create a
  Buffer reference from `null` and throws "Failed to create reference
  from Buffer". Send `undefined` (omit the field) instead. The
  renderer's `send_video_frame` call constructs args conditionally for
  this reason.

### 5.2 WebCodecs `isConfigSupported` quirks

- `latencyMode: 'realtime'` and `hardwareAcceleration: 'prefer-hardware'`
  are documented as **hints** but Chromium's `isConfigSupported` treats
  them as **hard constraints** and returns `supported: false` when it
  can't fulfil them.
- At `configure()` time the same fields are genuinely soft hints —
  Chromium falls back to software encoders.
- Therefore: probe + pre-flight `isConfigSupported` calls must NOT pass
  those fields. Only the actual `configure()` call passes them.
- The encoder-error callback in StreamCapture handles the async
  failure case where `configure()` accepted the hint but couldn't
  allocate the hardware encoder — it rebuilds with `prefer-software`.

### 5.3 H.264 / HEVC / AV1 codec strings

`webCodecsStringForCodec` in `StreamCapture.ts` picks profile/level by
frame size + framerate. **Level matters**:

| H.264 Level | Max | Codec string |
|---|---|---|
| 3.1 | 720p30 | `avc1.64001F` |
| 3.2 | 720p60 | `avc1.640021` |
| 4.0 | 1080p30 | `avc1.640028` |
| 4.2 | 1080p60 | `avc1.64002A` |
| 5.0 | 1440p30 | `avc1.640032` |
| 5.1 | 4K30 / 1440p60 | `avc1.640033` |
| 5.2 | 4K60 | `avc1.640034` |

Picking too low a level → `isConfigSupported` returns false at the
actual stream resolution, even with codec_string-as-Level-3.0 succeeding
at 720p in the probe.

### 5.4 Probe cache must always ship to native

Native's `state.encoder_caps` is in-memory and resets every app launch.
`probeEncoders` in `src/utils/encoderProbe.ts` calls
`invoke("set_encoder_caps", { encoderCaps: caps })` even when returning
the cached list. Without this, the codec dropdown collapses to "auto"
only on every restart after the first successful probe.

Cache key is `decibell.encoder_caps.v2`. Bump the version any time the
probe semantics change.

### 5.5 Electron `setDisplayMediaRequestHandler`

- Must register on **all platforms** (not just Linux). Chromium rejects
  renderer-initiated `getDisplayMedia` with `NotSupportedError` unless
  a handler is registered, even with `WebRTCPipeWireCapturer` enabled.
- `useSystemPicker: true` is **macOS-only** (15+). Other platforms
  ignore it. Guard with `process.platform === "darwin"`.
- On Linux, `desktopCapturer.getSources` triggers
  xdg-desktop-portal's screen-share dialog automatically — pass the
  result through. The user picks via the portal dialog, not our UI.

### 5.6 napi-rs CLI version

Pinned at v2 because v3 silently breaks `.d.ts` + `index.js` generation
with `napi-derive` 2.x. Don't upgrade to v3 without verifying generated
output. (See `feedback_napi_cli_version.md` in auto-memory.)

### 5.7 Attachment protocol quirks

(Still relevant for any attachment-related work.)

- Decibell's community server uses **tus.io `Upload-Offset`** semantics
  for chunked PATCH, NOT HTTP `Content-Range`. The renderer's
  `uploadAttachment.ts` sends `Upload-Offset` correctly.
- Custom-scheme URLs need a pseudo-host because numeric server IDs
  parse as IPv4 addresses. The pattern is
  `decibell-attachment://attach/<serverId>/<id>`.

(See `feedback_attachment_protocol_quirks.md` in auto-memory.)

---

### 5.8 The message list is real DOM — keep it that way

`src/features/chat/RealMessageList.tsx` renders the loaded slice as plain DOM: no
virtualization, no estimated row height anywhere. react-virtuoso was removed on 2026-08-27
after seven rounds of estimate-driven jump bugs (postmortem + design:
`docs/superpowers/specs/2026-08-25-real-dom-message-list-plan.md`; feature-log entry in
`docs/reviews/2026-08-23-community-server-review.md`). Rules, each learned the hard way:

- Measure rows with `offsetTop`/`offsetHeight`, never `getBoundingClientRect` — the
  arrival slide and the last row's `fadeUp` are transforms and pollute rects.
- The placement `useLayoutEffect` has no deps; it must run on every commit (a parent
  re-render can change row heights without a change to `items`).
- `overflow-anchor` stays `auto`. Chromium anchors prepends and adjusts the running
  compositor wheel animation in place; a programmatic `scrollTop` write cancels it (a
  visible stutter on page-in — live-tested). The list's own math is the residual.
- Trims are triggered by row count (150) but cut by pixel distance (≥ 2×NEAR_PX from the
  viewport) on the side opposite the growth — a count cut can land inside the paging zone
  and ping-pong with the paginator.
- Row keys are message identity only (id, else nonce / synthetic), never index.
- Scroll positions are `{anchorId, offset, atBottom}`; the panels write them per
  channel/peer key (`positionsRef`) so a channel switch can't race the persist cleanup.

Opt-in placement trace: `localStorage.setItem("decibell.real_message_list_debug", "1")`.

---

### 5.9a GIF search needs a KLIPY or GIPHY key (`resources/gifs.json`)

The picker's GIFs tab searches **KLIPY** or **GIPHY** from the main
process (`electron/main/gifs.ts`) — the two providers Discord moved to
when Google shut the Tenor API down on 2026-06-30 (Tenor is gone; don't
bring it back). Provisioning mirrors the Sentry DSN: the release
workflow writes `resources/gifs.json` (`{"provider":"klipy","key":"…"}`)
from the **`GIF_API_KEY`** repository secret and the **`GIF_API_PROVIDER`**
repository variable (`klipy` | `giphy`, default klipy), and
electron-builder ships it as an extra resource. For a dev checkout,
create `electron-client/resources/gifs.json` yourself (gitignored) or
export `GIF_API_KEY` (+ `GIF_API_PROVIDER`). Without one the tab shows a
"not set up" notice.

Keys: KLIPY — sign up at partner.klipy.com → API Keys → Add Platform
(test key immediately, 100 calls/hour; production access on request,
free). GIPHY — developers.giphy.com/dashboard (beta key immediately,
100 calls/hour; production keys are reviewed). Attribution: the search
placeholder reads "Search KLIPY" / "Search GIPHY" and the footer
"Powered by …" — KLIPY requires the placeholder, GIPHY the mark.

Sending a GIF sends its https URL as the message text — no
proto/server change; the link preview renders it and the bubble hides
the URL text for a lone media link (`loneLink`).

### 5.9 `fetch()` in the renderer is CSP-bound; `blob:` is not allowed

`index.html`'s `connect-src` lists `decibell-file:` but **not** `blob:`.
So `fetch()` works on a file picked from disk and fails on one that was
pasted or dragged — those become `blob:` URLs. This bit the ThumbHash
encoder, which fetched the source to re-decode it and silently produced
nothing for pasted images.

Drawing an image to a canvas taints nothing here (verified for both
`blob:` and custom-scheme sources), so prefer reading pixels off an
already-decoded `<img>` over re-fetching bytes. If you genuinely need
the bytes of a `blob:` URL, widen the CSP deliberately rather than
letting the failure fall into a catch block.

---

### 5.10 P2P DM calls — how it hangs together

- **One engine.** `AppState.voice_engine` is either a channel session or a
  call, never both. Renderer: `joinVoiceChannel` calls `endCall` first,
  `startCall`/`acceptCall` call `leaveCommunityVoiceIfAny` first; native
  refuses the other order (`join_voice_channel` ↔ `call_prepare/connect`).
- **`voiceStore.callPeer`** marks the call session (`connectedServerId` /
  `connectedChannelId` stay null). Components gate on "in a session", not on
  a channel id; `disconnect()` clears it.
- **Sealed sockets never `connect()`.** `MediaSocket` sends to the source of
  the last *authenticated* datagram. Don't add a `connect()` — two sides that
  picked different candidates would wedge.
- **PING reflection lives in `MediaSocket::recv`**, not the pipeline. A peer's
  PING is bounced re-sealed and never surfaced; our echo is returned. That is
  what keeps `pipeline.rs`'s RTT + 3 s keepalive and the punch responder
  working without pipeline changes.
- **`sender_id` = own username** in P2P mode; the peer's frames therefore
  arrive under their username and the whole username-keyed player stack
  (`WATCHED_STREAMS`, `streamFrames.subscribe`, `pipStream`) just works.
  `call_watch_stream({watch})` is the P2P `watch_stream`.
- **Streams announce over central** (`STREAM_START/STOP`) and both sides keep
  `voiceStore.activeStreams` in step (`announceCallStreamStart/Stop`); there
  are no thumbnails (no channel) — `StreamCapture` skips the pump when the
  community ids are absent.
- **Client-first is safe**: an old central ignores `CALL_SIGNAL`;
  `get_call_config().callSignaling` (from `LoginResponse`) gates the button.
- Ring/ringback are `loopSound("call_ring")` with a returned `stop()`; both
  sides time out at 45 s (`RING_TIMEOUT_MS`).
- **Several devices per account** (`docs/superpowers/specs/2026-10-18-multi-device-design.md`):
  central keeps a call registry. An INVITE rings every device of the callee; the first ACCEPT takes
  the call and the others get `ANSWERED_ELSEWHERE` (a REJECT: `DECLINED_ELSEWHERE`), which end the
  ringing card with a short end state (`callStore.incomingEnded`). BUSY reaches the caller only when
  no device can ring, so a device in a call keeps auto-answering BUSY. After the answer, signals go
  only between the two connections in the call; one that reconnects keeps its place by device id.
- **Devices and password changes** (`docs/superpowers/specs/2026-10-19-devices-and-password-design.md`):
  every `LoginRequest` comes from `ThisDevice` (`net/central.rs`). Typed sign-ins send
  `automatic: false`; the saved sign-in and the reconnect fallbacks send `true`, and central refuses those
  on a device it doesn't know any more (`LoginResponse.signed_out`). Never mark anything the user didn't
  just type as typed: that is what makes signing a device out stick. The refusal and
  `SESSION_CLOSED {SIGNED_OUT | PASSWORD_CHANGED}` go through `commands::auth::signed_out_elsewhere`, a
  full sign-out (saved sign-in cleared) with the reason in `logged_out`; Log out asks central to forget
  the device first (`commands/devices.rs::forget_this_device`).

### 5.11 Stream-audio capture — platform traps

- **One WASAPI process-loopback client = one process tree.** INCLUDE or
  EXCLUDE exactly one PID's tree per `IAudioClient`; there is no multi-exclude.
  That's why the Windows filter runs a *set* of clients through
  `ProcessLoopbackMixer` and why a blacklist is a dynamic include-set
  (`stream_audio_filter::plan_clients`). The virtual device has no
  `GetMixFormat` — read the default render endpoint's format once and hand
  the same block to every client.
- **`IAudioSessionControl2::IsSystemSoundsSession()` returns S_FALSE for
  "no".** Both S_OK and S_FALSE are success HRESULTs; compare `== S_OK`,
  never `.is_ok()` (that would drop every session).
- **PipeWire may put the PID on the Client, not the Node.** PipeWire-ALSA
  clients (Decibell's own CPAL output, `alsa_playback.decibell`) carry
  `pipewire.sec.pid` / `application.process.binary` only on the
  `PipeWire:Interface:Client` object; the node's props have neither. Resolve
  through `client.id` (`parse_app_nodes`) — treating a PID-less node as "not
  us" tapped the voice chat into the stream for months.
- **`pw-link -d` takes a link id** (or `output input`); link objects carry
  `input-node-id`, so "our taps" = links whose input node is the capture sink.
- Apps start playing between polls: both backends poll every 2 s and wake
  immediately on `set_filter`; that ≤ 2 s gap is the accepted v1 latency.

### 5.12 E2EE DMs — how it hangs together

Design: `docs/superpowers/specs/2026-09-03-e2ee-dms-design.md`. The short version:

- **Nothing cryptographic reaches the renderer.** `native/src/e2ee/` owns identities
  (`identity.rs`), the message envelope (`envelope.rs`), the passphrase backup (`backup.rs`),
  the encrypted-at-rest local store (`keystore.rs`) and the runtime (`session.rs`). The
  renderer gets `message_received` / `dm_history_received` / `dm_conversations_received` /
  `dm_message_edited` exactly as before plus `encrypted: bool` and `decryptError: string`
  (`"" | locked | no_key | peer_key | bad`), and drives everything through six commands
  (`e2ee_get_status / e2ee_setup / e2ee_unlock / e2ee_change_passphrase / e2ee_reset /
  e2ee_peer_info`) and two events (`e2ee_status_changed`, `e2ee_peer_changed`).
- **Never decrypt inside `route_packets`.** Opening an envelope can need a peer's historical
  public key, which is an `E2EE_FETCH_KEYS_REQ` round-trip whose reply arrives on the same
  loop. DM packets are handed to the ordered worker (`session::enqueue`) which awaits fetches
  and emits; the router only resolves waiters. Adding a DM-shaped packet? Route it through a
  new `DmJob` arm.
- **Another device of the account** has the identity only after the passphrase restores the backup
  (bootstrap reports *locked*; `setup` refuses to publish over a backup). A reset on one device reaches
  the others as `E2EE_KEYS_CHANGED` (they lock); one that was signed out compares its store with
  central's current bundle at bootstrap (`lock_if_rotated_elsewhere`).
- **Status is per login, per device:** `unavailable` (central lacks `LoginResponse.e2ee_keys`,
  or logged out) → `not_set_up` → `locked` (backup exists on central, no usable local store)
  → `ready`. `session::bootstrap` runs after every LoginRes (spawned). The renderer's
  `useE2eeEvents` opens the unlock modal on the first `locked` report of a login and, on
  becoming `ready`, calls `dmStore.invalidateAllHistory()` + re-requests conversations and the
  active peer's page (sealed rows that arrived while locked hold placeholders).
- **Send policy lives in `session::seal_outbound`:** ready + peer has keys → sealed (`content`
  = the fixed placeholder for old clients, body in `envelope`); peer has no keys and was never
  pinned → plaintext; peer pinned but central has no keys / we're locked → error to the
  sender. Edits go through the same function.
- **The at-rest key** comes from Electron main (`electron/main/e2eeLocalKey.ts`, safeStorage
  wrapped `<userData>/e2ee/local.key`) as `InitOptions.e2eeLocalKey`; absent → `config.rs`
  derivation. A store that doesn't open shows as *locked* — the passphrase rebuilds it.
- **key_id is bookkeeping, sign_pub is identity.** Fingerprints / safety numbers hash
  `sign_pub`; envelopes name key ids so old history finds the right generation (every old
  private key is kept locally and in the backup). A peer's `sign_pub` changing = "keys
  changed" banner + toast + re-pin.
- Argon2id runs on the blocking pool (~100 ms at 64 MiB); `x25519-dalek` is used because
  `ring` only offers ephemeral agreement (no static secrets).
- **Voice channels are MLS groups** (`e2ee/group.rs`, OpenMLS 0.9): `join_voice_channel` refuses
  without loaded keys, mints a `frame_crypto::KeyRing` and a driver task that fetches the server's
  GroupInfo, creates or externally joins, and pushes epoch keys into the ring after every merge.
  The ring rides `VoiceEngine::start(.., ring)` into the audio pipeline, the video receive thread,
  `VideoSender` (seals a whole frame, then chunks — FEC/NACK untouched) and `AudioStreamEngine`.
  Sealed packet types 7/8/9 mirror 0/6/1; an encrypted session drops the plain types, the P2P
  call path (`ring = None`) drops the sealed ones. The driver resyncs by rejoining on any
  processing error, removes roster ghosts after 3 s (lowest verified name commits), and
  quarantines the epoch (nothing sent) when a leaf's identity doesn't match central. Routing:
  `net/community.rs` forwards `MlsGroupInfoRes` / `MlsCommitRes` / `MlsCommitBroadcast` and the
  presence roster to `AppState.voice_mls.tx`. OpenMLS gotchas: `MlsMessageIn` has no
  `into_verifiable_group_info` — match `extract()` on `MlsMessageBodyIn::GroupInfo`; the
  commit bundle's GroupInfo already carries the ratchet tree and the external-init key when
  `use_ratchet_tree_extension` is on; handshake wire format is PURE_PLAINTEXT (it travels in TLS,
  the server knows the roster anyway).
- **Encrypted text channels** (`e2ee/channel_keys.rs` + `channel_envelope.rs`): the renderer
  passes `encrypted` (from `ChannelInfo`) into `send_channel_message` / `edit_channel_message`;
  native seals under the channel's current epoch key, fetching / creating / filling through
  `CHANNEL_KEYS_REQ` / `CHANNEL_KEYS_PUBLISH_REQ`. Incoming channel packets go through the
  ordered channel-crypto worker (`channel_keys::enqueue`), which also passes plaintext rows —
  same reason as the DM worker: a missing epoch key is a round-trip whose reply lands on the
  router. `on_keys_changed` acts only when the server names us as filler. Blobs are DM envelopes
  (`envelope::seal_bytes`, content tag 0x02) so pins and key-change notices apply. Keyrings are
  per session (`AppState.channel_keys`, dropped on disconnect / logout). Encryption is chosen when
  a channel is created (`ChannelCreateRequest.encrypted`) and fixed for its life (since
  2026-10-14). There is no on/off switch, so a channel's history is never half sealed.
- **Attachments in encrypted channels** are sealed in the renderer before upload
  (`features/chat/attachmentCrypto.ts`: random key per file, 64 KiB AES-256-GCM chunks,
  sealed thumbnails; `uploadAttachment.ts` sends stand-in metadata + `encrypted: true` + the
  kind to `/attachments/init` and the ciphertext size). The real metadata + key travel in the
  message envelope as content tag 0x03 (prost `EncryptedMessageBody`; `send_channel_message` /
  `edit_channel_message` take `encryptedAttachments`), come back as `encryptedAttachments` on
  message / history / edit events, and `useChatEvents` merges them into the `Attachment`
  (`encrypted`, `keyB64`, `chunkBytes`) and registers the keys with main
  (`window.decibell.attachments.registerKeys`). Main decrypts in `electron/main/attachmentFetch.ts`,
  used by the `decibell-attachment://` protocol, the loopback media server (Range → covering
  chunks → trim, real 200 / 206) and `netFetch` (save-as); keys live in `attachmentKeys.ts`
  for the session only. The two crypto modules (`src/features/chat/attachmentCrypto.ts` and
  `electron/main/attachmentCrypto.ts`) must stay byte-for-byte in step.
- **Calls are authenticated with the same identity** (`e2ee/call_auth.rs`,
  `session::sign_own_call_key` / `verify_peer_call_key`): `send_call_signal` signs the
  ephemeral key on INVITE / ACCEPT, `call_connect` verifies against the peer's *current*
  pinned identity before `call_crypto::derive` and fails closed when the peer has keys but
  the signature is missing / stale / wrong. The renderer only ferries `pubKeySig` + `keyId`
  from `call_signal` into `call_connect`; `call_connected.verified` drives the stage pill.
- **napi camel-casing splits on digits.** `e2ee_get_status` was exported as `e2EeGetStatus`,
  which the renderer's `invoke()` normaliser (`_x` → `X` → `e2eeGetStatus`) can never
  produce — every call would have failed with "unknown command". Any `#[napi]` fn whose name
  contains a digit needs an explicit `#[napi(js_name = "…")]` matching the normaliser
  (`commands/e2ee.rs` does this); check `native/index.js` after `npx napi build`.

### 5.13 Renderer / Chromium gotchas

Each of these cost a debugging round; most were recorded in the owner's Claude memory before 2026-11-07.

- **zustand selectors return stable references.** `s.x[id] ?? []` or a `.filter` inside a selector makes
  a fresh value per call → "getSnapshot should be cached" → render loop → blank screen. Fall back to
  `EMPTY_LIST` (`lib/empty.ts`) and derive with `useMemo` outside the selector.
- **Never combine hooks with `&&` / `??` / `?:`.** `useA() && useB()` skips the second hook when the first
  is falsy → "Rendered fewer hooks than expected" → black screen. Bind each hook to its own `const`.
- **A no-op `set()` must `return state`.** zustand 5 skips notifying only when `Object.is(next, state)`;
  `return {}` merges into a new object and re-runs every selector (a 150-row list pays for it).
- **i64 event fields can arrive as `BigInt`** once they leave the 32-bit range (byte counts, disk sizes);
  `Math.max` / division then throw → blank screen. `Number(x)` at the listener. The TS type says
  `number`; it lies for big values.
- **napi `Option<T>` rejects `null`.** Send `undefined` (omit the key), e.g. `deviceId ?? undefined`,
  never `?? null` ("Failed to convert Null into String"). Same for `Option<Buffer>` (§5.1).
- **`visibility: hidden` makes the whole subtree unfocusable** (`autoFocus`, `.focus()` silently fail).
  To hide before measuring, use `opacity: 0` + `pointer-events: none`.
- **Hover must not `scrollIntoView` the active row.** Chromium fires a synthetic mousemove after the
  scroll, the next row becomes active, and the list runs away to the end. Only keyboard / open / filter
  scroll (`components/Select.tsx`, `activeBy`). The offscreen harness doesn't send that mousemove.
- **Setting `video.src` fires a synthetic `timeupdate` at 0** before `loadedmetadata` (twice in Strict
  Mode). An initial seek target is one-shot; never let `timeupdate` overwrite it.
- **`HTMLMediaElement.duration` can be `Infinity`** (CBR MP3s without a Xing header);
  `JSON.stringify(Infinity)` is `null` and the server's init then 400s. Guard with
  `Number.isFinite(d) && d > 0`.
- **`[ERROR:ffmpeg_common.cc] Unsupported pixel format: -1` is noise** (decoder negotiation). Gate
  `v.load()` recoveries on `v.error != null`, never on log lines.
- **The dev renderer probes decoders afresh** (2026-11-07): `utils/decoderProbe.ts` caches its result in
  localStorage, per origin. `npm run dev` serves the renderer from `http://localhost:5173`, a different origin from
  the packaged `file://`, so it probes on its own, and on the owner's machine that probe said no AV1: an AV1 stream
  couldn't be watched in the dev client (Settings → Refresh codec capabilities re-probes). A checkout run with the
  production renderer (`npm run build:renderer && npm run build:tsc`, then `electron .`) shares the installed
  client's `file://` cache. A debug native addon also struggles with a 1440p120 stream; build release for that.
- **Recording watched streams** (the Qt client's spike, design D1): the native `stream-dump` cargo feature (off
  in every shipped build) plus `DECIBELL_STREAM_DUMP=<dir>` writes each frame the stream bus hands the renderer,
  with its arrival time, to `<dir>/<uid>-<time>-<codec>.dbv` (`native/src/media/stream_dump.rs`;
  `tools/perf/README.md` has the steps).
- **Image decode is a separate cache from the fetch.** `decoding="async"` guarantees a one-frame pop on
  every mount; prefetch with `img.decode()` and use `decoding="sync"` for bounded thumbnails.
- **Blink holds unreferenced custom-protocol images weakly.** `decibell-attachment://` responses never
  enter the disk cache; after a few channel switches a GC drops them and they refetch.
  `chat/imageRetention.ts` (`retainImage`, LRU, ~128 MB) keeps recent ones; any new place showing
  attachment images should feed it. Test with `--js-flags=--expose-gc` + `gc()` and a control build.
  Images are not persisted across restarts (owner decision).
- **`createImageBitmap(img, sx, sy, sw, sh)` crops EXIF-rotated photos in the stored layout.** Crop with
  `drawImage` onto a canvas, then `createImageBitmap(canvas)`. Test with a real Orientation = 6 file
  (`exiv2 -M"set Exif.Image.Orientation Short 6" f.jpg`).
- **Message-row chrome gets whole-pixel, content-independent heights** (reply line, badges); fractional
  rows turn into ±1 px anchor nudges on prepends. Scale inline emoji with `uiStore.textSizePx`.
- **Draw tiny glyphs with holes as one SVG** in a fixed viewBox (status dots); %-inset child boxes snap
  off-centre. Reproduce at DPR 1.25 with `--ui-row-scale` 0.85.
- **Tailwind v4**: `@theme` variables no utility uses are pruned (tokens read only via `var()` from JS go
  in a plain `:root` block); `shadow-*` resolves statically past the `--shadow-*` layer (override
  `--t-shadow-*`). Grep the built CSS after touching tokens.
- **WebCodecs AV1 and Annex B H.264 emit no `description`.** On a codec change, `reset()` +
  `configure({codec})` unconditionally; reconfigure again only if a description arrives (HEVC).
- **OpenH264's "Actual input framerate … is different" warning is meaningless.** Trust
  `StreamCapture`'s `last 5s: N frames` counter and the encoder `queueSize`.
- **`npm run dev` never restarts Electron for main / preload changes** (and main keeps its 30-min
  link-preview cache). Say so after touching `electron/main/**` or `electron/preload/**`. "No handler
  registered for '…'" = an old main under a new preload (window reload only, or a stale
  `dist/electron/main`; `dev` now compiles main once before starting).
- **Server skew is real.** Releases ship before the servers are rebuilt, so a new client meets old
  servers for days. Degrade, but never act destructively on a field an old server omits (proto3 reads
  it as 0): on 2026-10-28 reading `before_id == 0` as "newest page" made every scroll-up page wipe the
  loaded rows against the old servers.

### 5.14 Native gotchas

- **`eprintln!` / `println!` can panic under Electron** (`failed printing to stderr: … os error 11`):
  Electron makes stdio non-blocking, a burst fills the pipe, the print macros panic and a worker thread
  dies silently (it killed the HEVC encode thread). `lib.rs` `on_load` clears `O_NONBLOCK` on unix; still
  prefer `log::` macros and never log per frame.
- **napi-rs 2 leaks a TSFN value on `QueueFull`.** `ThreadsafeFunction::call` boxes the value before the
  call and never frees it on a failed enqueue; a bounded "drop when full" TSFN leaks every dropped item.
  Cap in-flight items yourself (`events.rs` `reserve_stream_slot`).
- **`cargo test` can't link the event bus** (`napi_call_threadsafe_function` exists only inside Node).
  Keep emits reachable from tests behind a `#[cfg(not(test))]` wrapper with a no-op twin
  (`hotkeys/mod.rs`, `e2ee/session.rs`), not a global change to `events.rs`.
- **No CPU↔GPU sync in capture loops.** A per-send staging `Map` waited out the game's GPU queue and
  held a 60 fps DXGI share at ~32 fps. Keep capture / composite fully asynchronous on the GPU
  (`cursor_gpu.rs`); readbacks only on the encoder thread at low rate. The tell is a large `send_max` in
  `[capture_dxgi] 1s:` telemetry.
- **zbus bodies need their real signature.** Deserializing `oa(sa{sv})` into `(OwnedObjectPath, Value)`
  fails as `SignatureMismatch` and the handler silently never fires; use concrete types and unit-test
  with `zbus::Message::signal(..).build(&body)` round trips.
- **Electron `powerMonitor` idle is dead on Wayland** (0 s forever, KDE measured, native and XWayland;
  KDE's `GetSessionIdleTime` is unsupported). Idle comes from `system_idle.rs` (`ext_idle_notifier_v1`;
  Mutter IdleMonitor on GNOME); powerMonitor only on Windows / macOS / X11.
- **Audio devices**: open each device in its reported sample format (virtual devices like Sonar /
  Voicemeeter refuse forced f32); guard 0-channel / 0-rate configs; a `devicechange` must never hot-swap
  the live pipeline (wireless headsets fire it constantly: pops). The jitter buffer ignores inter-arrival
  gaps ≥ 150 ms (keepalives) and re-buffers instead of concealing on underrun. Record a session with
  `DECIBELL_AUDIO_DUMP=<dir>` and read it with `native/scripts/analyze_audio_dump.py`.
- **Server-mute vs stream audio**: the relay drops AUDIO from a server-muted member but relays
  STREAM_AUDIO. Intentional (owner decision); don't "fix" it.

### 5.15 Process and tooling

- **Windows code from Linux**: `cargo test --lib` on Linux never compiles `cfg(windows)`. Push, let
  **Windows Native Check** (`.github/workflows/win-native-check.yml`, `cargo check --all-targets` on
  windows-latest; log on the orphan branch `ci/win-native-check-log`: `git fetch origin
  ci/win-native-check-log && git show FETCH_HEAD:cargo-check.log`) go green, *then* tag `ev*` (a blind
  Windows API mismatch failed the ev0.7.11 release). Select a run by head sha, not by time:
  `gh run list --workflow "Windows Native Check" --json databaseId,headSha`. On a Windows machine, just
  build and test natively (§6.1): that also runs the Windows-only unit tests CI never runs
  (`gpu_pipeline`, `encoder_probe`, `thumbnail`).
- **Native builds from a git worktree (Linux)**: `native/vendor/` (the static FFmpeg) is gitignored. Symlink
  main's `vendor` into the worktree and export main's **absolute** `PKG_CONFIG_PATH`
  (`…/native/vendor/ffmpeg/lib/pkgconfig`) and `CARGO_TARGET_DIR`, or ffmpeg-sys binds the system FFmpeg 8
  and ffmpeg-next fails to build. Don't `git add` the symlink.
- **Shared working tree**: other sessions edit the same tree. Commit only your own hunks (stage `HEAD +
  your hunk` via `git hash-object -w` + `git update-index --cacheinfo`, verify the staged diff in a
  detached worktree); never `git stash`, `git add -A`, or overwrite files you didn't touch.
- **`tsc` through a pipe**: read `${PIPESTATUS[0]}`; `| tail` hides a failing exit code.
- **UI preview harness** (screenshots / interaction tests of real components without a live server): a
  scratch Vite build with an `@app` alias to `electron-client/src`, `globals.css` imported plus an
  `@source` line, a Proxy stub for `window.decibell` (`invoke` resolves null, `listen` / `subscribe`
  return unsubscribers; record calls camel-cased), stores seeded with `setState`, captured by an
  offscreen `BrowserWindow` (`loadFile`, `executeJavaScript`, `sendInputEvent`, `capturePage`). Keep it on
  a side branch: crashes and reboots wipe the scratchpad. Lessons: `localStorage.clear()` before seeding
  (it persists across runs); drags need `modifiers: ["leftButtonDown"]`; offscreen `focus()` fires no
  focus events (dispatch `focusin`); `sendInputEvent` wheel deltas have the opposite sign; selectors for
  before / after jobs must exist in both builds; Electron 33 logs a harmless "No component available"
  rejection at startup; `pkill -f <pattern>` matching your own command kills the shell.
- **Redesign proposals (owner-approved format)**: build in a scratch copy, capture the real components
  with the harness, publish one page: before / after per theme, "what's off today", annotated anatomy,
  per-state pairs, the rejected alternative with reasons, files that will change, 2–3 numbered decisions
  with recommendations. Port to the real tree only after approval.
- **Releases**: the workflow's `release` job runs only after all three platform builds (~40 min); a
  macOS job that never gets a runner ("job was not acquired") is fixed with `gh run rerun <id> --failed`.
  One cancelled within a second with "The configuration '<image>' is not supported" means GitHub retired
  that runner image (macos-14, 2026-11-06): a rerun runs the tag's own workflow and fails again, so bump
  `runs-on` on `main` and move the unreleased tag onto that commit (`git push origin :refs/tags/ev<VER>`,
  then `git tag -f` + push).
  The AUR `git push` can hang in `git-receive-pack`: wrap it in `timeout 90`, retry, confirm with
  `git ls-remote origin master`.

## 6. How to test from a fresh checkout

```bash
git clone git@github.com:sunkhan/decibell.git
cd decibell/electron-client

npm install                 # pulls castlabs Electron + deps
cd native && npm install && cd ..

npm run dev                 # starts vite + tsc-watch + electron concurrently
```

The dev script runs `npm run build:native:debug` first, which compiles
the Rust addon. Native rebuilds:

```bash
npm run build:native:debug  # debug build (fast, ~5s incremental)
npm run build:native        # release build
```

A debug build connects to central at `DECIBELL_CENTRAL=host:port` when that's
set (e.g. `DECIBELL_CENTRAL=127.0.0.1:8080 npm run dev` against a local central
from `src/server/tests`); release builds compile the override out
(`native/src/net/central.rs` `central_addr`).

Type checking:

```bash
npm run typecheck           # both renderer + main process
```

To clear the encoder probe cache (forces re-probe at next boot — useful
if you change probe semantics):

```js
// In Electron DevTools console:
localStorage.removeItem("decibell.encoder_caps.v2")
```

To test streaming you need **two Decibell clients** connected to the
same community server in the same voice channel — one streamer, one
watcher. The C++ community server must be running. The user knows how
to start the server; ask them rather than guessing.

Local secrets: GIF search needs `resources/gifs.json` (`{"provider":"klipy","key":"…"}`, gitignored) or
env `GIF_API_KEY` + `GIF_API_PROVIDER` (§5.9a). Crash reporting runs only in packaged builds, with the DSN
from `resources/sentry.json`; release builds bake both files from repository secrets. `npm run build:twemoji` generates the gitignored `src/components/emoji/twemoji-data.json` (the
`dev` / `build` scripts run it for you).

### 6.1 Building on Windows

Everything client-side builds natively on Windows (CI's Windows job is the reference:
`.github/workflows/electron-release.yml`, `win-native-check.yml`). One-time setup:

- **Node 20** (what CI uses) and npm.
- **Rust stable, MSVC toolchain** (`rustup default stable-x86_64-pc-windows-msvc`).
- **Visual Studio 2022** "Desktop development with C++" (MSVC + Windows SDK) and **CMake** (audiopus
  builds its vendored libopus with it).
- **protoc** on `PATH` (CI pins 27.x); `native/build.rs` runs prost over `proto/messages.proto`.
- **LLVM / libclang** for ffmpeg-sys-next's bindgen (preinstalled on the CI image; on a fresh machine
  install LLVM and set `LIBCLANG_PATH` to its `bin` if the build can't find `libclang.dll`).
- **FFmpeg through vcpkg, at CI's pinned commit** (newer FFmpeg 8 ports broke the d3d11va + NVENC path):
  ```powershell
  git clone https://github.com/microsoft/vcpkg C:\dev\vcpkg
  git -C C:\dev\vcpkg checkout e5a1490e1409d175932ef6014519e9ae149ddb7c
  C:\dev\vcpkg\bootstrap-vcpkg.bat -disableMetrics
  C:\dev\vcpkg\vcpkg.exe install "ffmpeg[nvcodec,amf,qsv]:x64-windows"
  ```
- In every dev shell:
  ```powershell
  $env:VCPKG_ROOT = "C:\dev\vcpkg"            # ffmpeg-sys-next finds FFmpeg through vcpkg (or set FFMPEG_DIR)
  $env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"    # audiopus_sys's libopus vs CMake 4
  npm run dev
  ```
  `copy-dlls.cjs` (run by the native build) copies the FFmpeg DLLs next to the `.node`;
  `electron/main/addon.ts` puts that folder on `PATH` while loading it.

`tools/windows/setup-dev.ps1` does all of the above per user into `C:\dev` (only the Build Tools need admin)
and also installs Qt for the Qt client; first run on the owner's machine, 2026-11-07, FFmpeg took 11 min.
Seen on that first run:

- **A fresh checkout's typecheck fails on `twemoji-data.json`** (gitignored, generated): run
  `npm run build:twemoji` once (`dev` / `build` run it for you).
- **`npx napi build` on Windows rewrites the tracked `native/index.js` / `index.d.ts`**: line endings, and
  `probeNativeEncoders`'s doc comment comes from its `cfg(windows)` variant. Not a change; restore them
  (`git checkout -- native/index.js native/index.d.ts`), don't commit them from Windows.
- **Two native tests fail on Windows, deterministically** (they never ran there before; CI only compile-checks):
  `media_socket::tests::oversized_and_junk_datagrams_are_skipped` (Windows fails `recv_from` with `WSAEMSGSIZE`
  (10040) for a datagram larger than the buffer, where Linux truncates; `MediaSocket::recv` propagates it instead
  of skipping. Production is unaffected: the receive loops treat every error as transient, and Windows has
  already dropped the datagram) and `voice_sim_tests::tests::ring_paced_receiver_never_starves` (case
  "GatedTail / clean link, +15dB gain, +40ppm": one starve, 960 spliced zeros; deterministic, so presumably a
  numeric difference of the MSVC build). Open in §7.

What runs where:

| Check | Linux | Windows |
|---|---|---|
| Renderer typecheck `npx tsc -p tsconfig.web.json --noEmit` (+ `npm run typecheck` for main) | yes | yes |
| `npx napi build --platform --js index.js --dts index.d.ts` (in `native/`) | yes | yes |
| `cargo test --lib` (in `native/`) | host cfg only | host cfg, incl. the Windows-only tests CI never runs |
| Community / central e2e harnesses (`src/*/tests/`: bash, g++, Python, a bundled PostgreSQL) | yes | Linux only: use WSL2 or the Linux box |
| Linux-only native code (PipeWire, portal, X11 hotkeys, `system_idle`) | yes | no: rely on the Linux box or CI |

The native addon's `cfg(windows)` paths (WGC / DXGI capture, D3D11, AMF / NVENC through FFmpeg, WASAPI
loopback, `GetAsyncKeyState` hotkeys, default-device watching) can finally be run and debugged directly
on that machine; several of their live tests are still open (§7).

---

## 7. Open items (as of 2026-11-07)

**Windows `cargo test --lib`**: two deterministic failures on the first Windows run (§6.1); everything else
(309 tests) passes. Neither has been looked into beyond the diagnosis there.

**Live tests pending.** Built, unit / e2e tested, not yet exercised by real people on real networks or
hardware. Windows-side items are marked (W); they're the natural ones to run on a Windows box.

- **P2P DM calls**: the two-machine matrix has never been run: same LAN (`path: host`), two networks
  (`srflx`), a symmetric-NAT pair (clean "couldn't connect"), mutual exclusion with voice both ways,
  central reconnect mid-call, stream both ways (PipeWire tap / WASAPI), keyframe round trip, mini-player
  while browsing. Start from the spec's Verification section. The call stage redesign (0.9.2) rides on it.
- **Encryption**: E2EE DMs across two accounts and two devices of one account; MLS voice + streams on
  the live community; encrypted channels with attachments.
- **Multi-device + devices**: two machines on one account (status, read sync, calls ringing both,
  answered elsewhere), device sign-out reaching a community, password change.
- **Streaming**: (W) Win10 DXGI share incl. the cursor-still case; (W) AMD AMF encode; (W) multi-app audio
  picker on Win10 and Linux; Go Live presets incl. Slides / 5 fps; two watched streams at once
  (the preload's per-user stream-frame dispatch, "R9").
- **Audio**: (W) "Default" devices following a Windows default change mid-call (both Sound-settings
  defaults, unplug); Linux `PULSE_SOURCE` / `PULSE_SINK` device routing.
- **Hotkeys**: (W) press actions as `RegisterHotKey` system hotkeys (since 2026-11-07: a Ctrl / Alt / Win
  combo or an F-key fires; the combo no longer reaches the focused app; recording a keybind still sees it; a combo
  another app owns falls back to polling); (W) push to talk / mute via `GetAsyncKeyState`, now every 20 ms; KDE portal (do stale shortcut ids pile
  up?); Wayland mouse-button remap guide.
- **UI shipped since 0.8.5** with only harness verification: voice view, Transfers panel and speed caps,
  reply redesign, user statuses + status messages, server / channel settings redesigns, welcome-banner
  actions, slowmode pill, message search, image viewer, picture editor, voice switch (0.9.4). The
  real-DOM message list in DMs is verified only by construction.

**Deploys.** The servers lag the client (§2). Carried to-dos: rebuild central (Hetzner) and the community
server with the pre-1.0-review build, then **rotate central's signing key** (review S1).

**Before the "big launch"** (owner, 2026-10-15): off-machine PostgreSQL backups or a replica (central is
one box); on the new box run `pg_test_fsync` (the DM rate is flush-bound), check `max_connections` ≥ io +
CPU + 64 DB threads + ~10, copy the JWT key and TLS cert / key, switch the client from the hard-coded IP
to a hostname, and point communities' `DECIBELL_CENTRAL_HOST` at it. Deferred until usage demands:
avatars to object storage + CDN, an ECDSA central cert (clients pin it, so ship with a release), TLS
termination outside central (82 KB / connection floor; needs PROXY protocol so per-address limits keep
working), central across machines (message bus at `deliver_to_user`), sign-up CAPTCHA / email
verification if spam appears.

**Pre-1.0 review leftovers** (`docs/reviews/2026-10-17-pre-1.0-review.md`): §1 is all fixed; §2–§6
(cross-cutting themes, medium / low findings, dead code, harness gaps, doc drift) are not started. The
dead-code list there (and ARCHITECTURE §7.3) is the place to start a cleanup; pre-1.0 compat cruft can
go (no backward compatibility before 1.0, owner rule).

**Older thread: the attachment scroll glitch.** Start by reading
`docs/reviews/2026-07-27-frontend-review.md` — its header is written as
a handoff for exactly this, listing what is fixed *and verified*, what
has been **ruled out with measurements** (don't redo those), and the
suspects worth measuring next.

Short version: the 0.6.10 fixes all targeted *the fetch* (image bytes
absent at mount). The 2026-08-15 round found the actual dominant cause
one layer down — *the decode*: `decoding="async"` guarantees a
one-frame pop on every mount even for cached images. Pre-decoding in
the prefetcher + `decoding="sync"` for thumbnails + un-defeating
`memo(MessageBubble)` reduced the glitch drastically (user-verified
live). Small imperfections remain; the review doc's 2026-08-15 section
lists the four candidate mechanisms and the observable signature that
distinguishes each. A dev-only row-height audit
(`src/features/chat/devRowHeightAudit.ts`) now logs any post-mount
height settle to settle the oldest hypothesis with data.

Also open, lower priority:

1. **B6 is unverified** — the persistent video overlay's scroll
   positioning was rewritten but never tested against real playback.
2. **The ThumbHash server code shipped compiled but barely exercised.**
   It works end to end (confirmed live), but only new uploads carry a
   hash; anything uploaded before 0.6.10 stays blank forever. A backfill
   would need image decoding in the C++ server.
3. **Plan C codec negotiation renderer-side** (PR8-era, still deferred) —
   auto-downgrading the codec when a low-cap watcher joins.
4. **AV1 1080p60** — auto-downgrade-to-H.264 in StreamCapture's
   encoder-error path.

---

## 8. Files you'll touch most

### Where the newer features live

| Area | Renderer | Native / main |
|---|---|---|
| Sign-in, devices, password, rename | `features/auth/`, `features/settings/{DevicesTab,AccountTab}.tsx`, `ChangePasswordModal.tsx`, `RenameUsernameModal.tsx`, `stores/{authStore,devicesStore}.ts` | `commands/{auth,devices}.rs`, `net/central.rs` (`ThisDevice`, resume, reconnect), `config.rs` |
| Users, names, presence, statuses | `stores/usersStore.ts`, `features/users/userDirectory.ts`, `hooks/useDisplayName.ts`, `features/presence/` (`presenceInterest.ts`, `useAutoAway.ts`, `StatusMenu.tsx`), `stores/{presenceStore,avatarStore}.ts` | `commands/presence.rs`, `presence.rs`, `system_idle.rs`, `electron/main/idle.ts` |
| DMs, read sync | `features/dm/` (`DmChatPanel.tsx`, `readSync.ts`, `useDmEvents.ts`), `stores/dmStore.ts` | `commands/dm.rs`, `e2ee/session.rs` (DM worker) |
| Calls | `features/call/` (`CallStage.tsx`, `callActions.ts`, `IncomingCallModal.tsx`), `stores/callStore.ts` | `commands/call.rs`, `media/{media_socket,call_crypto,stun,punch}.rs` |
| Encryption UI | `features/e2ee/` (setup / unlock / passphrase, `E2eeSection.tsx`), `stores/e2eeStore.ts` | `e2ee/`, `commands/e2ee.rs`, `electron/main/e2eeLocalKey.ts` |
| Server + channel settings | `features/servers/{ServerSettingsModal,ChannelSettingsModal}.tsx`, `tabs/`, `channelSettings/`, `saveEcho.ts`, `leaveGuard.ts`, `components/SettingsWindow.tsx` | `commands/{community,channels}.rs` |
| Members / roster | `features/members/`, `tabs/useMemberSearch.ts` | `commands/community.rs` (`list_members`, `search_members`) |
| Message search | `features/search/` | `commands/channels.rs` (`search_messages`) |
| Voice + stage | `features/voice/` (`VoicePanel.tsx`, `stage/`, `voiceActions.ts`, `memberDrag.ts`), `stores/voiceStore.ts` | `commands/voice.rs`, `media/{pipeline,audio_device,jitter,voice_gate}.rs`, `e2ee/group.rs` |
| Go Live | `features/voice/CaptureSourcePicker.tsx`, `goLive/` (`presets.ts`, `QualityPresets`, `AdvancedSettings`, `SourceGrid`) | `commands/streaming.rs`, `media/capture_*.rs`, `encoder*.rs` |
| Hotkeys | `features/hotkeys/`, `features/settings/tabs/KeybindsTab.tsx` | `hotkeys/`, `commands/hotkeys.rs`, `electron/main/hotkeys.ts` |
| Image viewer, picture editor | `features/chat/imageViewer/`, `ImageViewer.tsx`, `components/pictureEditor/` | `electron/main/clipboard.ts` (Copy image) |
| Transfers | `features/transfers/`, `stores/transfersStore.ts` | `electron/main/{downloads,downloadPacer}.ts` |

### Chat rendering + scroll
| File | Role |
|------|------|
| `src/features/chat/RealMessageList.tsx` | real-DOM sliding-window list: placement pass, pixel trims, anchor/position reporting |
| `src/features/chat/ChatPanel.tsx` | channel list host: pagination guards, trims, jump target, position persistence |
| `src/features/dm/DmChatPanel.tsx` | the DM equivalent; same contract |
| `src/features/chat/AttachmentList.tsx` | image/video/audio/document rendering; reserve boxes |
| `src/features/chat/attachmentSizing.ts` | sqrt-scaled preview boxes + grid geometry |
| `src/features/chat/attachmentPreviewUrl.ts` | `previewUrlFor`: the exact thumbnail/full URL an `<img>`/`<video>` uses (rows mount 800px ahead, so mounting is the prefetch) |
| `src/features/chat/thumbhash.ts` | placeholder encode (upload) / decode (render) |
| `src/features/chat/richText.ts` | dependency-free marker parser (Discord conventions + $TeX$); cached per content string |
| `src/features/chat/CodeBlock.tsx` | lowlight/highlight.js fenced-block renderer — 27 registered grammars, HAST→React (no innerHTML), token colors from the palette vars in globals.css |
| `src/features/chat/MathTex.tsx` | KaTeX renderer (throwOnError:false, trust:false, expansion caps); the renderer's one sanctioned dangerouslySetInnerHTML |
| `src/features/chat/MessagePreview.tsx` | live send-preview in both input cards — appears only when the draft parses as formatted; renders through MessageText for fidelity |
| `src/features/chat/RichComposer.tsx` | code/math composer panels (button beside emoji picker) — textarea with Tab indent + auto-indent, live KaTeX preview; inserts marker syntax into the draft |
| `src/features/chat/PersistentVideoLayer.tsx` | the fixed-position video overlaid on its placeholder |
| `src/features/chat/sendPacing.ts` | client-side send/edit pacing to the community's message bucket (FIFO per server), the 30 s echo watchdogs (channel + DM), nonce minting |
| `src/features/servers/openServer.ts` | the one "switch to this server" (keep the channel if it's here, else first text channel) used by the bar, invite cards, deep-link modal, browse view |
| `src/stores/inviteResolveStore.ts` | memo of `resolve_invite_code` (definitive answers stick; connectivity failures retry after 30 s) |
| `src/features/chat/LinkEmbeds.tsx` | link-preview cards under a bubble (≤3): site card / direct image; boxes reserved from declared or probed dimensions; routes `decibell://invite/…` to InviteEmbed |
| `src/features/chat/InviteEmbed.tsx` | Discord-style invite card: picture / name / members / description + Join (redeem_invite in place); resolves via `stores/inviteResolveStore.ts` → `resolve_invite_code` |
| `src/features/servers/inviteLink.ts` | the one `decibell://invite/…` grammar (autolinker, card, deep-link receiver) |
| `src/features/chat/EmojiPicker.tsx` / `GifPicker.tsx` | the picker's Emoji \| GIFs tabs; GIF search + trending feed over KLIPY / GIPHY, click sends the GIF's URL as its own message |
| `electron/main/gifs.ts` | KLIPY / GIPHY client (provider + key from `resources/gifs.json` / `GIF_API_KEY`); `decibell:gifs:status` / `search` |
| `src/stores/linkPreviewStore.ts` | renderer memo of unfurls keyed by URL (main holds the TTL cache) |
| `electron/main/linkPreview.ts` | the unfurler: OG/Twitter/title + oEmbed fallback, image dimension probe, private-network guard, caps, cache; also `decibell:shell:openExternal` |

### Theming
| File | Role |
|------|------|
| `src/styles/globals.css` | the whole token system: `--t-*` palettes, type scale, `.chrome-scope` |
| `src/stores/uiStore.ts` | `theme`, `textSizePx`, `rowScale` + the `data-theme` plumbing; layout memory (`sidebarWidth`, `pipWidth`, `pipCorner` — localStorage `decibell.layout.*`) |
| `src/features/settings/tabs/AppearanceTab.tsx` | palette picker + the two scale sliders |
| `public/theme-boot.js` | pre-mount theme/scale application (separate file: CSP) |


### Streaming send-side

- `src/features/voice/streaming/StreamCapture.ts` — getDisplayMedia +
  VideoEncoder. Module-level singleton via `startActiveStream` /
  `stopActiveStream` / `activeStreamCapture`.
- `src/features/voice/CaptureSourcePicker.tsx` — Go Live UI. Calls
  `start_screen_share` then `startActiveStream`. Its parts live in
  `features/voice/goLive/`: `SourceGrid` (Windows sources), `QualityPresets` +
  `presets.ts` (Smooth / Sharp / Slides / Data saver, derived from the
  stored settings), `AdvancedSettings` (resolution, frame rate, bitrate,
  codec). On a window ≥ 880 px (≥ 1184 px on Windows, with the source grid)
  the dialog is two cards: the main one and an Options panel beside it
  (sound, cursor, Advanced); narrower windows get the one-card layout.
- `src/features/channels/UserPanel.tsx` — Stop sharing button. Calls
  `stopActiveStream` then `stop_screen_share`.
- `src/features/voice/StreamAudioAppPicker.tsx` + `StreamAudioPopover.tsx` +
  `streamAudioFilter.ts` + `src/stores/streamAudioAppsStore.ts` — the
  multi-app audio picker (mode + ticked apps) in Go Live and, while live,
  behind the speaker button next to Stop. `streamAudioFilter.ts` is the one
  place that persists a change **and** pushes it to native.
- `src/utils/encoderProbe.ts` — boot-time WebCodecs probe.
- `native/src/commands/streaming.rs` — `start_screen_share`,
  `stop_screen_share`, `send_video_frame`, `set_encoder_caps`, etc.
- `native/src/media/mod.rs::VideoEngine` — thin send-side wrapper around
  the UDP socket. `AudioStreamEngine` (same file) owns the share-audio
  pipeline plus a `StreamAudioCapture` handle (`set_filter` while live).
- `native/src/media/stream_audio_filter.rs` — cfg-free app filter (mode +
  identities) and the Windows client planning (`plan_clients`);
  `stream_audio_mixer.rs` — cfg-free multi-source summer;
  `capture_audio_pipewire.rs` (Linux tap) / `capture_audio_wasapi.rs`
  (Windows `ProcessLoopbackMixer`) — the two backends;
  `native/src/commands/stream_audio.rs` — `list_stream_audio_apps`,
  `set_stream_audio_filter`. Design:
  `docs/superpowers/specs/2026-09-02-multi-app-audio-picker-design.md`.
- `native/src/media/video_pipeline.rs::VideoSender` — packetise + UDP.
- `native/src/media/video_packet.rs` — `WIRE_DESCRIPTION_MAGIC`
  constant, packet framing.

### Streaming receive-side (untouched in PR8 but next to test)

- `native/src/media/video_receiver.rs` — UDP receive thread,
  reassembles packets into frames.
- `native/src/events.rs::send_stream_frame` — per-stream Buffer TSFN
  pushing binary frames to JS.
- `electron/main/addon.ts` — fans `decibell:stream_frame` over
  `webContents.send`.
- `electron/preload/index.ts` — exposes
  `window.decibell.streamFrames.subscribe`.
- `src/features/voice/StreamVideoPlayer.tsx` — WebCodecs.VideoDecoder
  consumer.
- `src/utils/decoderProbe.ts` — boot-time decoder probe.

### Boot + IPC

- `electron/main/index.ts` — Chromium feature flags
  (`WebRTCPipeWireCapturer,PlatformHEVCEncoderSupport,PlatformHEVCDecoderSupport`),
  `setDisplayMediaRequestHandler`, `setCertificateVerifyProc`,
  `createWindow`, `initAddon`.
- `electron/main/addon.ts` — loads the napi binary, calls `init` /
  `shutdown`, sets up bus + streamBus broadcasters.
- `electron/main/ipc.ts` — `decibell:invoke` handler routing to napi
  commands. Snake-case → camelCase normalisation lives here.

### Overlays (menus, popups, modals)

- `src/hooks/useMenuPosition.ts` — the one placement for `position: fixed`
  menus: measures the rendered menu, flips to the other side of the anchor
  when it doesn't fit, clamps to the viewport, re-measures on resize. Used by
  `UserContextMenu`, `ImageContextMenu`, `DeviceContextMenu` and the
  channel-list menu in `ServerChannelsSidebar`. Never clamp against a
  hard-coded "approximate height" again — that is what clipped them.
- `src/features/dm/UserProfilePopup.tsx` — the anchored popout (identity,
  live stream, roles, quick DM). Its avatar opens the full profile.
- `src/features/profile/UserProfileModal.tsx` — the full profile screen
  (`activeModal === "user-profile"`): Profile tab + Privacy tab (safety
  number, fingerprint, pin state, key-change notice). `RoleChips.tsx` beside
  it is shared with the popup.
- `src/features/settings/SettingsModal.tsx` — the tabbed settings modal
  (`activeModal === "settings"`, `settingsTab`). One `activeModal` at a
  time: opening settings from the profile screen replaces it.

---

## 9. Recent commits

```
9f78136 ci(release): build macOS on macos-15
871a232 chore(release): 0.9.4 — a 12- / 24-hour time format setting; switching voice channels shows you in the new one at once
6434f43 feat(client): a 12- / 24-hour time format setting
5d85a29 fix(native): others see a voice switch at once; encryption is ready sooner
a6d22b1 fix(client): switching voice channels no longer shows you in the old one
caf319d aur: bump decibell-bin to 0.9.3
1695514 chore(release): 0.9.3 — the image viewer and the picture editor, redesigned; the Slides preset and Go Live's options panel; Copy image and jump fixes
e606355 fix(client): a sideways-stored phone photo uploaded the wrong area
18fc371 feat(client): the picture editor, for profile and server pictures
abc8064 fix(dev): compile main before npm run dev starts Electron
56e40c1 perf(client): copy an attachment in one trip, and say when it's slow
46fd485 fix(client): the viewer's wheel at 800%, and Copy image through main
7c30d4f feat(client): the image viewer, redesigned
b38fb01 fix(client): every jump lands, not every other one
b836778 feat(client): Go Live's options in a panel beside the dialog
7c8d06a feat(stream): the Slides preset, and 5 fps streams encoded for detail
```

`main` is the working branch; feature-sized commits go straight to it
after the verification bar (CLAUDE.md). Releases are tagged `ev*` (the
`v*` namespace fires the dead tauri workflow). The AUR package (`aur/`)
is bumped *after* a release builds, because makepkg needs the published
`.pacman`.

---

## 10. To start a new conversation

For client work, this file plus CLAUDE.md is the starting pair; `docs/FEATURES.md` says where any feature
lives and `ARCHITECTURE.md` how it talks to the servers. For a new native client (the GPUI exploration),
start from ARCHITECTURE §7.3 (what of `native/` is reusable and what the Electron side does that a native
client must replace) and `docs/FEATURES.md` §10 (what isn't built, so don't look for it on the wire).

For the old scroll-glitch thread:

> Read `electron-client/HANDOFF.md` and
> `docs/reviews/2026-07-27-frontend-review.md`. I want to fix the
> attachment scrolling glitch. Start from the "Where to look next"
> list — everything before it is already done and verified, so don't
> redo it.

Two things worth saying out loud to a fresh session, because both cost
real time:

- **Verify library behaviour by experiment, not by reading.**
  `firstItemIndex` silently changed what `itemContent` and
  `rangeChanged` meant in react-virtuoso (since removed); that broke
  message grouping and scroll restore in ways that typechecked cleanly.
- **Don't let failures be silent.** A cosmetic fallback that swallows
  its cause (the ThumbHash encoder returning `""`) turned a one-line CSP
  problem into three debugging rounds.
