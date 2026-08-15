# Decibell Electron Client — Resume Handoff

**Read this entire document before touching anything.** It's a complete
hand-off so a fresh conversation can resume PR8 of the Tauri-to-Electron
migration without inheriting any false assumptions from the previous
session. Last updated 2026-05-08 immediately before the user reformatted
their dev machine to swap Niri → KDE Plasma.

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
| **0.6.10** | **five-palette theme system + Appearance settings tab; Friends home screen; chat-scroll fixes; ThumbHash attachment placeholders** |

0.6.10 is the current release. Its client-side design notes live in
`docs/reviews/2026-07-27-frontend-review.md`, which is also the handoff
for the one piece of open work — see §7.

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

### 5.8 react-virtuoso `firstItemIndex` rewrites other props

Both message panels set `firstItemIndex` so paging older history in at
the top doesn't jump the viewport. It silently changes what other props
mean, and inconsistently — all three verified by experiment:

| prop | basis with `firstItemIndex` set |
|------|--------------------------------|
| `itemContent(index, …)` | **absolute** — `data[0]` arrives as `1000000` |
| `rangeChanged({startIndex})` | **absolute** — `data[150]` reports `1000150` |
| `initialTopMostItemIndex` | **data-relative** — 150 still means `data[150]` |

Anything using those indices to reach into the array must rebase first
(`const i = index - firstItemIndex`). Getting it wrong typechecks fine
and fails quietly: it broke message grouping outright (every
`messages[i-1]` was ~1e6 out of bounds, so `shouldGroup` always saw
`undefined`) and made every channel switch restore to the bottom.

Two more facts, verified against the installed 4.18.6 source
(2026-08-15, see the review doc's section of that date):

- `rangeChanged` reports the **rendered** range, `increaseViewportBy`
  included on both edges — so a scroll position saved from its
  `startIndex` sits ~overscan-height *above* the true viewport top.
  Widening `increaseViewportBy` widens that restore error.
- The separate `overscan` prop is **directional**: `{main, reverse}`
  applies `main` in the current scroll direction, unlike
  `increaseViewportBy` which is fixed top/bottom.

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

**The one live thread: the attachment scroll glitch.** Start by reading
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
| `src/features/chat/ChatPanel.tsx` | channel Virtuoso host: pagination, viewport size, prefetch |
| `src/features/dm/DmChatPanel.tsx` | the DM equivalent; same Virtuoso contract |
| `src/features/chat/useVirtuosoPrepend.ts` | derives `firstItemIndex` from prepend deltas |
| `src/features/chat/AttachmentList.tsx` | image/video/audio/document rendering; reserve boxes |
| `src/features/chat/attachmentSizing.ts` | sqrt-scaled preview boxes + grid geometry |
| `src/features/chat/attachmentPrefetch.ts` | warms previews ±15 messages off `rangeChanged` |
| `src/features/chat/thumbhash.ts` | placeholder encode (upload) / decode (render) |
| `src/features/chat/PersistentVideoLayer.tsx` | the fixed-position video overlaid on its placeholder |

### Theming
| File | Role |
|------|------|
| `src/styles/globals.css` | the whole token system: `--t-*` palettes, type scale, `.chrome-scope` |
| `src/stores/uiStore.ts` | `theme`, `textSizePx`, `rowScale` + the `data-theme` plumbing |
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
- `src/utils/encoderProbe.ts` — boot-time WebCodecs probe.
- `native/src/commands/streaming.rs` — `start_screen_share`,
  `stop_screen_share`, `send_video_frame`, `set_encoder_caps`, etc.
- `native/src/media/mod.rs::VideoEngine` — thin send-side wrapper around
  the UDP socket.
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
06c519a aur: bump decibell-bin to 0.6.10
444f0e6 chore: 0.6.10
42f14a3 chore(chat): drop the ThumbHash rollout probe
c212e9d fix(attachments): compute the ThumbHash without re-fetching the source
e8ebcd2 chore(chat): temporary dev-only probe for the ThumbHash rollout
a823ccf fix(chat): reserve the image box before its URL exists; blur on video too
3088247 Merge ui-rework: theme system, Friends home screen, chat scroll fixes
2bb690c feat(attachments): ThumbHash placeholders so a row is never an empty box
```

`main` is the working branch; `ui-rework` was merged and deleted in
0.6.10. Releases are tagged `ev*` (the `v*` namespace fires the dead
tauri workflow). The AUR package (`aur/`) is bumped *after* a release
builds, because makepkg needs the published `.pacman`.

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
