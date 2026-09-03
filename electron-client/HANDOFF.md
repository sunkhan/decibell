# Decibell Electron Client — Resume Handoff

**Read this entire document before touching anything.** It's a complete
hand-off so a fresh conversation can resume client work without inheriting
any false assumptions from a previous session. Written 2026-05-08 at the end
of PR8 (the Tauri-to-Electron migration); the release table in §2, the
gotchas in §5, the file table in §8 and the commit list in §9 are kept
current — last refreshed 2026-08-27.

The companion auto-memory file
`~/.claude/projects/-home-sun-Desktop-decibell-decibell/memory/project_electron_migration.md`
also records this state but is condensed; **this file is the canonical
human-readable hand-off**.

---

## 1. What Decibell is

Decibell is a self-hosted, federated Discord-like app:
- **C++ community server** (`server/`) — text/voice/streaming
- **C++ central server** (also `server/`) — auth, server discovery, friends
- **Shared protobuf wire** in `proto/messages.proto`
- **Client** — currently being migrated

The **only** client that ships is the Electron one being built in
`electron-client/`. The QML reference client and `tauri-client/` are
historical reference only — do not modify them, do not consider them
for compat. (See `feedback_electron_only_client.md` in auto-memory.)

The C++ server is **not changing**. Wire compatibility with it is
mandatory.

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

0.7.14 is the current release (0.7.13 GPU cursor compositing + fps pacing on every capture path; 0.7.14 the multi-app audio picker + Linux self-leak fix). It carries **four proto additions** —
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

## 3. PR8 architecture — what we changed and why

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

## 4. Current state of PR8 — what works, what doesn't

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

---

## 7. Suggested next steps in priority order

**Freshly shipped, lightly tested (2026-08-27) — worth an eyeball before the
next release:** link-preview cards against a narrow side panel and a bubble
that also has attachments; the pending fade in DMs (channels verified live,
DMs only by construction); invite cards against the *rebuilt* central
(name/picture/description — degrades to host:port until then); the send
pacer under a real flood on a fresh server (the e2e covers the server side
only). GIF search is verified live with a KLIPY test key; request production
access before a release.

**P2P DM calls (2026-08-28) — shipped, verified only by construction.** All
four milestones are on `main` (relay, sealed transport + STUN/punch, call
lifecycle, in-call screen share) with the transport unit-tested on loopback,
but the two-machine matrix has not been run: same LAN (expect `path: host`),
two networks (expect `srflx`), a symmetric-NAT pair (clean "couldn't connect"),
community voice regression, mutual exclusion both ways, central reconnect
mid-call, stream both ways incl. Linux PipeWire tap / Windows WASAPI, PLI
round trip, mini player while browsing. Central must be rebuilt first
(§5.10). Start from the spec's Verification section.

**The one older live thread: the attachment scroll glitch.** Start by reading
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

### Chat rendering + scroll (the live work — see §7)
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
  `start_screen_share` then `startActiveStream`.
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

---

## 9. Recent commits

```
ae40b8c feat(client): pending messages render faded until the server echoes them
9a5b3f9 fix: spam bursts no longer leave ghost bubbles — typed rejection, client pacing, 10/3 bucket
95a122c fix(client): opening a server from an invite card lands on its first text channel
bcae97d feat(client): invite links are code-only — decibell://invite/<CODE>
e6ef534 feat(client): invite card in the browse-card shape — banner on top, details + Join below
0a8675f feat: invite cards in chat — decibell://invite links preview the server
010d350 feat(client): "Unfiltered GIF search" toggle in the Privacy tab
1bc5a16 fix(client): GIF search over KLIPY / GIPHY — Tenor's API is gone
ac6fe2d feat(client): GIFs — picker tab, animated GIF attachments
106386e feat(client): hyperlinks + link previews
79587ea aur: bump decibell-bin to 0.7.7
```

`main` is the working branch; feature-sized commits go straight to it
after the verification bar (CLAUDE.md). Releases are tagged `ev*` (the
`v*` namespace fires the dead tauri workflow). The AUR package (`aur/`)
is bumped *after* a release builds, because makepkg needs the published
`.pacman`.

---

## 10. To start a new conversation

For the open scroll-glitch work:

> Read `electron-client/HANDOFF.md` and
> `docs/reviews/2026-07-27-frontend-review.md`. I want to fix the
> attachment scrolling glitch. Start from the "Where to look next"
> list — everything before it is already done and verified, so don't
> redo it.

For anything else, this file plus the auto-memory at
`~/.claude/projects/-home-sun-Desktop-decibell/memory/MEMORY.md` is the
right starting pair. The memory is condensed; this has the detail.

Two things worth saying out loud to a fresh session, because both cost
real time this round:

- **Verify library behaviour by experiment, not by reading.**
  `firstItemIndex` silently changes what `itemContent` and
  `rangeChanged` mean but not `initialTopMostItemIndex`; that broke
  message grouping and scroll restore in ways that typecheck cleanly.
- **Don't let failures be silent.** A cosmetic fallback that swallows
  its cause (the ThumbHash encoder returning `""`) turned a one-line CSP
  problem into three debugging rounds.
