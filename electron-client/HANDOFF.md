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
| **PR8** | **Move video encode renderer-side via WebCodecs** | 🔄 **in progress** |

Migration is feature-complete from a code-port standpoint. Remaining
items beyond PR8 are polish/distribution: settings persistence + modal
UI, PersistentVideoLayer/PersistentAudioLayer, ImageContextMenu,
channel management UI, electron-builder packaging, electron-updater
wiring, deep-link handler.

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

1. **Smoke-test the boot path on KDE Plasma.** Confirm
   `setDisplayMediaRequestHandler` triggers KDE's xdg-desktop-portal
   dialog cleanly (it should — KDE's portal is mature; Niri's was
   problematic).
2. **Verify H.264 end-to-end.** Pick H.264_HW or H.264_SW, 1280×720@30
   or 1920×1080@30. Confirm the watcher actually sees video. This is
   the smallest test that exercises encode → wire → decode in PR8's
   architecture. Failure modes to expect:
   - Frames flow but watcher spins → check `decibell:stream_frame` IPC
     in the watcher's preload + `StreamVideoPlayer.tsx`'s
     `subscribe(cb)` callback.
   - `WIRE_DESCRIPTION_MAGIC` mismatch — H.264 in Annex B doesn't use
     the magic prefix; verify `streaming.rs::send_video_frame` is
     gating the prefix on `args.codec == 3 || args.codec == 4`.
3. **Address AV1 1080p60.** The simplest fix is an auto-downgrade-to-H.264
   fallback in StreamCapture's encoder-error path, mirroring the
   existing prefer-hardware → prefer-software pattern. The harder fix
   is the resolution-dropdown becoming codec-aware.
4. **Plan C codec negotiation moving renderer-side.** The LCD picker
   should now move into the renderer where it has both watcher decode
   caps (via `voice_caps_cache` events) and the local encoder's
   `VideoEncoder.configure()` reconfigure hook. Currently the streamer
   uses the codec they selected at start; auto-downgrading when a
   low-cap watcher joins is deferred.
5. **Settings persistence + modal UI** — the codec toggles, voice
   thresholds, etc. live in stores but don't persist across restarts.
6. **electron-builder packaging** — `electron-builder.yml` exists but
   end-to-end packaging (AppImage / .deb / .exe / .dmg) hasn't been
   exercised.

---

## 8. Files you'll touch most

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

## 9. Recent commits (last 5)

```
524c95c feat(electron-client): WIP migration from Tauri to Electron + napi-rs
e997421 fix(linux): MSE QuotaExceededError recovery + blob URL leak
5deb041 feat(community): add second default voice channel
3cfdabd release: v0.5.7
62d2864 fix(linux): gap-free fMP4 timeline — fixes permanent MSE freezes
```

Everything through `524c95c` is on `origin/main`. The `electron-client/`
directory is one giant commit — split into multiple PRs in the original
plan but pushed as one for the dev-machine reformat.

---

## 10. To start a new conversation

When you come back, say something like:

> Read `electron-client/HANDOFF.md` and pick up where we left off. We
> just reformatted to KDE Plasma. Start with smoke-testing the boot
> path and then the H.264 end-to-end stream test.

The new model should also load the auto-memory at
`~/.claude/projects/-home-sun-Desktop-decibell-decibell/memory/MEMORY.md`
automatically — but tell it to **also read this file** since the memory
is condensed and this has the full step-by-step.
