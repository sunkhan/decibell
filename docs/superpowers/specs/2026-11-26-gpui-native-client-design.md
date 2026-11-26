# GPUI native client

2026-11-26. **Owner decision: the native client is built on GPUI, not Qt.** It lives in `gpui-client/`, on the
`gpui-client` branch (cut from `qt-spike`, which carries the Qt spike's report and tools), and goes for full
feature parity with the Electron client: every row of `docs/FEATURES.md` §1–§9. The Electron client keeps
shipping and defines every behaviour until then.

The point of the migration is a faster, leaner client: **far less RAM than Electron, and low CPU, GPU, wakeups,
startup time and install size.** Every choice below is judged by those, measured, on the same machine as the
Electron client it is compared with.

This record replaces the Qt design (`2026-11-07-qt-native-client-design.md`) as the plan of record. What of that
design still holds, unchanged, is listed in §1; the rest of this document is what GPUI changes.

Status: §2–§7 are decided (§5 from the source survey, `gpui-client/docs/gpui-notes.md`; §7 from the video
prototype, `gpui-client/docs/video-design.md`); the running state of the work is
`gpui-client/HANDOFF.md`, the checklist `gpui-client/PARITY.md`.

---

## 1. What carries over from the Qt design

| Qt design | Status here |
|---|---|
| §1 How the client is judged (memory, CPU, GPU, wakeups, startup, size; one harness for both clients) | Holds. The Linux harness is `tools/perf/measure.sh` + `startup.sh` (§3). |
| §2 Electron baseline | The Windows numbers stand for Windows. **This session is on Linux**, so the baseline is re-measured on this machine under Linux, against the local test stack (§3). |
| §3 Resource targets | Hold **as ratios** against the Electron baseline of the same machine and OS (§3 below). |
| §6 The core split (`crates/decibell-core` + a thin napi crate) | Holds, done first (§4). |
| §7 Threads and the runtime model | Holds with GPUI's names: one UI thread (GPUI's foreground executor; there is no separate render thread), a tokio runtime the client owns, the core's media threads, one decoder thread per watched stream. One wake of the UI thread per burst of core events. No polling. |
| §8 Video: decode on the GPU, convert at drawn size, newest-frame pick-up, one redraw per new frame, the persistent player | The model holds; the mechanism is GPUI's (§7). |
| §9.2 Message-list invariants (exact heights, rows keyed by identity, anchor-based positions, 0 px on a prepend, cross-row selection, Chromium-matched wheel) | Hold; implemented as a GPUI element (§6.3). |
| §10 What replaces Electron's main process and Chromium services | Holds row for row (§8). |
| §11 Fidelity: generated tokens, deterministic fixtures, captures of both clients side by side | Holds (§9). |
| The spike's findings and tools (`docs/reviews/2026-11-07-qt-client-spike-report.md`, `qt-client/HANDOFF.md` §2a–§2f, `tools/perf/`) | Used as reference: fidelity rules (line height, Inter `opsz`, emoji boxes, inline-code chips), the wheel curve, selection colours, the `.dbv` stream format, where memory went. |
| Qt Quick, cxx-qt, CMake, QML, `qt-client/` code | Set aside. Nothing is built on `qt-client/`. |
| D13 (Windows hotkeys: `RegisterHotKey` for press actions) | Already on `main`. |

Standing decisions in `CLAUDE.md` apply to this client without exception (uid identity, bound-keys-only hotkeys,
strict MLS voice, no recent searches, attachment images not cached across restarts, link previews unfurled by
the client, KLIPY, …). The one that does not carry over is "prefer Chromium APIs": there is no Chromium.

## 2. Rules of the build

- **Branch**: `gpui-client` only. Nothing is pushed to `main`, tagged, released or deployed from this work.
- **No wire or server changes.** The client speaks today's protocol to today's servers.
- **GPUI is pinned** to one Zed commit, recorded in `gpui-client/GPUI_PIN`
  (`a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281`, 2026-11-26). Its source is read, not remembered. GPUI itself is
  Apache-2.0; most other Zed crates are GPL and are neither depended on nor copied from. Changes GPUI needs are
  kept as patch files against the pin (`gpui-client/patches/`), applied by `gpui-client/tools/fetch-gpui.sh`.
- **Electron defines behaviour.** A feature's renderer code is read before it is ported. Protocol, media and
  crypto stay in `crates/decibell-core`; the client never re-implements a packet path.
- **Testing** runs against the local stack (`tools/dev/local-stack.sh`: central + three communities + seeded
  test accounts), never the owner's account or the live servers.
- **Linux is finished here**; Windows code must compile (the Windows Native Check workflow builds the core, the
  addon and this client on the branch); its hardware checks are listed under "Needs owner" in
  `gpui-client/HANDOFF.md`, with everything else only the owner can do (two-machine tests, visual sign-off,
  other GPUs).
- A parity row is **done only when it was run**, with a self-test where one is possible.

## 3. Measuring, and the targets

`tools/perf/measure.sh` samples a client's whole process tree from `/proc`: CPU as % of one core, private memory
(`Private_Clean + Private_Dirty` of `smaps_rollup`, the analogue of Windows' private working set), PSS, RSS,
threads, **voluntary context switches per second per thread** (the wakeup count, by thread name, so our own
threads can be told from the GPU driver's), GPU engine use and memory per process (`nvidia-smi pmon`, DRM
fdinfo). `startup.sh` times launch → first visible window (seen by the compositor) → settled. `install-size.sh`
measures a directory. The Electron client is measured first, on this machine, from a checkout built with the
`local-central` feature against the same local stack and fixtures the GPUI client uses.

Targets (the Qt design's §3 ratios; the absolute figure is the ratio applied to the Linux baseline, recorded in
`gpui-client/HANDOFF.md` once measured):

| Metric | Target |
|---|---|
| Idle private memory, busy channel open | ≤ 1/3 of Electron |
| Idle CPU | ≤ 1/10 of Electron; no timer of ours firing at idle (only the 30 s server pings) |
| Idle wakeups | ≤ 30 / s from our own threads; GPU driver threads reported beside it |
| Frames at idle | 0 rendered while nothing on screen changes |
| Startup to a visible window (warm) | ≤ 1/3 of Electron |
| Install size (with FFmpeg) | ≤ 1/4 of Electron |
| Watching 2 streams (1440p120 where the display allows) | CPU ≤ 1/3 of Electron, GPU ≤ Electron, no frame dropped or duplicated beyond the source's, presented-frame jitter p99 ≤ one refresh interval |
| Scrolling a 10k-message channel | every frame inside the refresh interval; a history page moves what is being read by 0 px |

Every phase logs its numbers against these. A feature that breaks one isn't done.

**The GPU driver's memory** (owner, 2026-11-26). On NVIDIA's proprietary driver any process with a Vulkan
swap chain carries tens of MB of the driver's own private memory (the GPUI hello-world: 62 MB private in all,
under 20 MB of it the app's heap). Using the GPU matters more than that overhead, so hardware rendering and
decode are never traded away for it. The memory row is reported as the total first; if the total is over
the target, the driver's share is measured (a minimal Vulkan window on the same driver and session, same
tool) and reported beside it, and the row is judged on the remainder, as the wakeup row already sets driver
threads aside. Electron's figure is reported the same way (its GPU process carries the same driver).

What the client does to meet them, as rules of the code:

- **No polling.** A timer exists only for a named, live reason (slowmode countdown, 45 s ring, auto-away
  threshold, a toast's lifetime) and is dropped when the reason ends.
- **No frames at idle.** Nothing animates off screen or while its state is not live; spinners, cursors and
  speaking rings stop with their cause. The client counts presented frames; the self-tests assert the counter
  stands still on an idle screen.
- **No per-frame allocation or copy on hot paths**: video (pooled packets from the socket to the decoder, the
  picture stays on the GPU), scrolling (row layouts cached by message identity and width; nothing is shaped
  again while only the offset changes), audio (the core's existing rings).
- **One UI wake per burst** of core events (§6.1), and stores notify only the views that read what changed.
- **Memory**: state is plain Rust keyed by uid / id; the message model keeps a window, not a channel's history;
  pictures are decoded at drawn size into a byte-bounded cache; fonts and assets are mapped from the binary,
  not copied to the heap; nothing is kept across restarts that Electron doesn't keep.

## 4. The core split (first, before any UI)

As the Qt design's §6, in steps; Electron passes its whole verification bar after each (renderer typecheck,
`npx napi build`, `cargo test --lib`, Windows Native Check on the branch):

1. **Move** `net/`, `media/`, `e2ee/`, `hotkeys/`, `config.rs`, `presence.rs`, `system_idle.rs`, `state.rs` and the
   prost build into `crates/decibell-core`; the four call-outs from `net/` into `commands/` become core functions;
   events go through an installed `EventSink` (untyped at this step: name + JSON, plus the two binary
   callbacks), the napi crate keeping its three ThreadsafeFunctions and the stream-frame shedding.
2. **Typed events**: `CoreEvent`, one variant per event with today's payload struct; `to_json()` gives the napi
   sink today's exact envelope, the GPUI sink matches on the variant with no JSON in between.
3. **Commands**: the bodies of the 132 `#[napi]` functions move to `decibell_core::api::<area>` as plain Rust
   returning `Result<T, CoreError>`; argument structs live in core (`#[cfg_attr(feature = "napi", napi(object))]`);
   the napi commands become one-line wrappers.
4. **Runtime**: core spawns through a `tokio::runtime::Handle` given to `init`, never a bare `tokio::spawn`, so
   calls from GPUI's threads work. The addon passes napi's runtime; this client owns a small one.
5. **Receive path without per-frame allocation**: reassembled frames and decrypt buffers come from a pool; the
   decoder hands the same buffer to FFmpeg refcounted.

No root Cargo workspace: `electron-client/native` and `gpui-client` each keep their own lockfile and target
directory and depend on the core by path.

## 5. GPUI at the pin

From the source survey (`gpui-client/docs/gpui-notes.md`: every claim there carries a path and line at the pin,
and §13 has the hello-world measurements on this machine). What it decides:

| Fact at the pin | Decision |
|---|---|
| GPUI is split into `gpui`, `gpui_platform`, `gpui_linux` (Wayland + X11), `gpui_wgpu` (the Linux renderer), `gpui_windows` (DirectX 11 + DirectWrite); all Apache-2.0. crates.io's `gpui` is 13 months old and unusable. 562 crates in the Linux build, none GPL-only. | Depend by **path** on a patched copy of the pinned checkout (`gpui-client/vendor/gpui-src`, made by `tools/fetch-gpui.sh` from the pristine clone + `patches/*.patch`), `default-features = false`, with Zed's `[patch.crates-io]` lines and its lockfile subset. |
| **Idle**: draws only on demand. Wayland: 0 frames, 0 main-thread wakeups, 0 ms CPU over 30 s (measured). X11: a refresh-rate timer wakes the main thread every vblank while the window is visible (280/s here), drawing nothing. Windows: a vsync thread wakes every vblank and invalidates every window. | Patches: X11 timer only while there is frame demand; Windows vsync thread parked while nothing is dirty (compile-checked here, run by the owner). |
| **Memory**: on NVIDIA the floor is the driver. Hello-world with 400 chat rows: 162 MB RSS / 75 MB private by default; wgpu also probing EGL / GL loads every GL vendor library. Without that: 110 MB RSS / 62 MB private (59 at `opt-level = "s"`), of which the app's heap is under 20 MB. | Patch: enumerate GL only when no Vulkan adapter works. The memory target is counted as private memory (`Private_Clean + Private_Dirty`), the analogue of the Windows figure the ratios were set on; RSS and PSS are logged beside it. |
| A `notify()` on a displayed view redraws the whole window, re-running `render` for every view that is not cached; a cached view whose state did not change replays last frame's paint. | One cached view per pane (server bar, channel list, message list, member list, composer, each overlay), stores that notify per key, so typing or a caret blink re-renders one small view. |
| Unfocused windows are capped at ~30 fps by default; after a burst of input GPUI keeps presenting for 1 s. | `inactive_frame_interval: None` (streams are watched in unfocused windows). The 1 s tail is measured; patched only if it shows. |
| **Text**: cosmic-text + swash on Linux, DirectWrite on Windows. No variable-font axes (Linux rasterises the default instance), no letter-spacing, no inline elements in text, colour emoji only from a font named `NotoColorEmoji`, no selection across text blocks; a fallback glyph shifts the paragraph's baseline; trailing spaces don't hang at a wrap. `shape_line` / `shape_text` give glyphs and positions without painting. | **Static font instances** generated from the variable files (Inter at optical size 14 per weight used; Mona Sans per weight at widths 100 / 112.5 / 125), so both platforms get exact faces with no patch. **Our own text elements** (`decibell-text`) on top of GPUI's shaping: line breaking as Chromium does it, letter-spacing, inline Twemoji boxes, backgrounds for code chips, hit-testing and selection ranges, painted glyph by glyph. GPUI's built-in text elements are not used for app text. |
| `list()` anchors on (item, offset) and measures only rows it lays out; no smooth scrolling; the wheel can be taken in the capture phase and the offset driven per frame. | **Our own message-list element** with exact row heights from `decibell-text` (cached by identity, content version and width) and the anchor model of §6.3; our own wheel animation. |
| Text input: only the IME protocol (`EntityInputHandler`) and an example. `gpui-component` (Apache-2.0) builds against the pin but costs +10.4 MB of binary and an idle 2 Hz thread. | **Our own input** (single-line field and the multi-line composer) on `decibell-text` + the input-handler protocol; no component kit. |
| Images: `img()` caches never evict; an animated image re-renders its view every frame. SVGs rasterise into the atlas at draw size and tint. | Our own byte-bounded image cache painting `RenderImage`s we decode at drawn size (freed with `drop_image`); GIFs advance on their own frame timer only while visible. |
| Capture: `HeadlessAppContext` + `capture_screenshot` renders a window to a PNG with no display on Linux (verified); not on Windows. | Captures come from a `decibell-capture` binary built with GPUI's `test-support`; the shipping binary doesn't carry it. |
| Platform gaps: no tray, no single-instance or URL-scheme handling on Linux / Windows (`on_open_urls` never fires), `request_attention` is a no-op on Wayland, no idle / lock signal. | Built in the client: a single-instance socket that forwards `decibell://` links, the xdg-activation attention request (a small patch), the core's `system_idle.rs`. |
| Threads: main, one `Worker-N` per CPU (parked), a timer thread, zbus / smol threads for portal settings and logind; none wakes at idle. The NVIDIA driver's `[vkps] Update` thread wakes 4×/s by itself. | Reported beside our own threads, as the target says. Worker count capped only if it measures. |
| Startup of the hello-world: 141–186 ms from spawn to first frame (warm). | Leaves room for the core's start; measured per phase. |

## 6. Architecture of the client

```
crates/decibell-core/            the shared core (§4)
gpui-client/
  Cargo.toml                     the client's workspace (own Cargo.lock, own target/)
  GPUI_PIN, patches/, tools/     the pin, patches against it, fetch-gpui.sh, gen-theme, fixtures, capture
  crates/
    decibell-theme/              tokens generated from electron-client/src/styles/globals.css (7 themes)
    decibell-ui/                 the design system as GPUI components (Button, Switch, Select, Modal, the
                                 settings frame, menus, tooltips, text field, icons, avatars, …)
    decibell-text/               rich text: the richText.ts subset → styled runs → layout, hit-testing,
                                 selection ranges, Twemoji, code highlighting, TeX
    decibell-video/              hardware decode and the external-texture element (§7)
    decibell-services/           what Electron's main process did (§8); no GPUI in it
    decibell-app/                the binary: stores, the event pump, features (auth, shell, servers,
                                 channels, chat, dm, friends, members, voice, call, streams, search,
                                 transfers, settings, profile, presence, hotkeys, e2ee)
  HANDOFF.md, PARITY.md, docs/
```

### 6.1 State and events

- **Stores are GPUI entities**, one per concern, mirroring the renderer's zustand stores so a ported feature
  reads like its original (`UsersStore` is the only source of names, keyed by uid; `ServersStore`,
  `ChannelsStore`, `MessagesStore`, `DmStore`, `VoiceStore`, `PresenceStore`, …). They hold plain Rust; views
  observe the store they read and are re-rendered only on its notification. Where the renderer selected a
  slice of a store to avoid re-renders, the store here notifies per key (a per-channel or per-user entity), so
  a message in one channel never re-lays-out another view.
- **Core → UI**: the client's `EventSink` pushes each `CoreEvent` onto a queue and wakes the UI thread only
  when the queue goes from empty to non-empty; the pump drains the whole queue in one turn and applies it to
  the stores, then GPUI draws once. Stream frames and thumbnails never go through the UI thread: frames go
  straight to the stream's decoder queue.
- **UI → core**: views call `decibell_core::api` through a thin `Core` handle that spawns on the client's tokio
  runtime (2 workers) and brings the result back to the UI thread; the UI thread never blocks on the runtime.
- **Settings** live in the core's `config.json`; the ~18 `localStorage` keys of the renderer move into it.

### 6.2 Theme, fonts, icons

- `gpui-client/tools/gen-theme` parses `globals.css` (the `@theme` block, `:root`, every `[data-theme]` block
  including shared ones, `var()` chains, `calc()` over the type scale) and writes `decibell-theme/src/generated.rs`:
  seven `static` palettes with every role resolved, plus radii, shadows, font roles and the type scale. The file
  is committed; the verification bar regenerates it and fails on a diff, so tokens can't drift. A theme switch
  swaps one pointer and repaints. Per-theme component rules in the CSS (console overrides, `.chrome-scope`,
  the image viewer's light stages) are ported by hand into the components, each citing its CSS line.
- Fonts are the renderer's files (Inter Variable, Mona Sans Variable, JetBrains Mono), embedded.
- Icons are the lucide SVGs the renderer uses, at its stroke widths, rasterised at device-pixel size and tinted
  at draw; Twemoji are the same SVG set as the renderer.

### 6.3 Chat

- **The message list is our own element.** The model holds the loaded window of a channel (paged around /
  before / after like the renderer), rows keyed by message identity (id, else nonce). Each row's height is its
  laid-out height for the current width, computed once and cached by (identity, content version, width); the
  scroll position is `{anchor row, offset, at bottom}`, so a page of history arriving above changes no pixel
  of what is being read, by construction. Only rows that intersect the viewport (plus overdraw) are painted.
- **Rich text** is parsed once per message (a port of `richText.ts`) into runs; `decibell-text` lays it out with
  GPUI's text system and paints text, inline-code chips, spoilers, emoji and selection itself; hover, links and
  selection are hit-tested inside the element. Code blocks are highlighted by a Rust highlighter covering the
  renderer's 27 grammars; `$TeX$` by a Rust TeX layout.
- **Selection spans rows**: a controller holds (row identity, offset) endpoints; rows paint their part; copy
  builds the text from the model, so rows scrolled away still copy.
- **Wheel scrolling** reproduces Chromium's on the same OS (distance per notch and the animation curve,
  measured from the Electron client on this machine), on wall time, one tick per frame while it runs and none
  after; touchpads pass through.
- **The composer** is our own multi-line input over GPUI's input-handler protocol (IME, selection, undo),
  with the renderer's live formatting preview, emoji / GIF picker, attachments, slowmode and pacing.

## 7. Video

The model of the Qt design's §8: the core's receive thread hands whole, decrypted, encoded frames to the
stream's decoder thread (a 3-frame queue; on overflow shed to the next keyframe and ask for one); FFmpeg decodes
on the GPU; the picture is converted and scaled on the GPU at the size it is drawn; the UI takes the newest
completed picture when it draws; a new picture asks for exactly one redraw; nothing redraws when no frame
arrives. One decoder per stream, however many places show it (grid tile, focus, mini-player); self-preview
decodes our own encoded frames like any other stream. Software decode only as a fallback.

### 7.0 Vendors (owner, 2026-11-26)

Hardware codecs on **NVIDIA, AMD and Intel, on Linux and Windows** (integrated GPUs with a working hardware
codec included); software only when no hardware encoder / decoder is usable. This client has no Chromium to
fall back on, so each row below is a path the client must have, probe and fall through in order:

| | Linux | Windows |
|---|---|---|
| Decode | Vulkan Video on GPUI's device (NVIDIA, RADV, ANV) → NVDEC (NVIDIA) / VAAPI (AMD, Intel) → software | D3D11VA on GPUI's D3D11 device (every vendor) → software |
| Encode (shared core) | NVENC → VAAPI (AMD, Intel) → software | NVENC / AMF / Quick Sync by adapter → software |
| Software | dav1d (AV1), FFmpeg H.264 / HEVC decoders; SVT-AV1 encoder | the same |

Only NVIDIA hardware is on hand: the AMD / Intel rows are built, compile-checked and unit-tested here and run
by the owner (`gpui-client/HANDOFF.md` "Needs owner").

### 7.1 Decided, from the prototype (`gpui-client/docs/video-design.md`)

- **Linux: FFmpeg's Vulkan Video hwaccel on GPUI's own `VkDevice`** (H.264, HEVC, AV1). GPUI's wgpu device is
  created through a hook that adds the video-decode queue family and extensions; FFmpeg's `AVVulkanDeviceContext`
  is filled from the same raw handles. One device, as mpv and Chromium do it (and as the Qt design's D6 did on
  D3D11).
- **One GPU copy, no CPU copy.** On this driver the decoder's output picture is also its reference picture
  (`reuse_dst_dpb`), held in a decode layout and guarded by FFmpeg's own semaphore, which wgpu cannot wait on.
  So each decoded picture is copied on the GPU (two `vkCmdCopyImage` regions: luma to an R8 image, chroma to an
  RG8 image) into one of 5 slots per stream; the renderer samples the newest completed slot and converts
  NV12 → RGB (BT.709 limited range, exact on the test pattern) in the fragment shader at the size it is drawn.
- **The GPUI patch** (`patches/0001-gpui-external-surfaces.patch`, 7 files, +574 / −46): GPUI's scene already
  had a surface primitive for macOS that the wgpu renderer skipped; the patch draws it (rounded corners,
  content mask, in scene order, so text and overlays sit on top), adds `Window::paint_external_surface` and
  **`Window::request_present()`**: a new picture re-presents the retained scene with the newest slot, with no
  layout and no paint. Nothing is drawn when no picture arrives.
- **Threads**: per stream a decoder thread behind a 3-frame queue (shed to the next keyframe on overflow); one
  device-wide queue lock shared by the renderer, FFmpeg's queue callbacks and the copy.
- **Fallbacks**: NVDEC (CUDA → Vulkan copy) on NVIDIA and VAAPI (DMA-BUF import) on AMD / Intel where Vulkan
  Video is missing: designed, the FFmpeg build carries both hwaccels; software decode (FFmpeg H.264 / HEVC,
  dav1d for AV1) uploading into the same plane textures: built and run (keeps up at 1080p60, not at 1440p120
  HEVC / AV1).
- **Windows**: D3D11VA on `gpui_windows`' D3D11 device, sampling the decoder's texture array slice directly
  (no copy needed there): designed, compile-checked in CI, run by the owner.
- **FFmpeg** for this client: a static LGPL 8.1 build with the three decoders, their parsers, the Vulkan /
  NVDEC / VAAPI hwaccels, dav1d and the encoders the core already uses, in its own prefix
  (`gpui-client/vendor/ffmpeg`); Electron's prefix and script are untouched.

Measured on this machine (RTX 4080, 280 Hz, other builds running, so CPU is an upper bound): one 1440p120
stream 4.1–6.0 % of a core, **two 1440p120 streams 7.5–12 %**, every decoded frame drawn (3362 of 3362 in
28 s), private memory 99–122 MB, packet → picture published 1.3–2.8 ms; zero draws after the stream ends.
(For orientation, on Windows the Qt spike measured 18.5 % and Electron's player path 52.6 % for two streams.)

Open before it ships (`video-design.md` §12): device-loss recovery (one unexplained loss in ~95 early starts,
none in 250 soak starts after the queue lock went in), no validation-layer run, AMD / Intel and Windows
unrun, mip-less downscale aliasing in small tiles, 10-bit streams refused.

## 8. What replaces Electron's main process and Chromium services

As the Qt design's §10, in `decibell-services` (plain Rust, shared by nothing else, no GPUI types) unless noted:

| Responsibility | Here |
|---|---|
| Video decode + render | §7 |
| Capture source list | Linux: the portal dialog, as today. Windows: DXGI outputs + `EnumWindows` producing the ids `source_id.rs` parses |
| Attachment upload / download / Range playback / save-as / Transfers / pacing | One HTTPS client (hyper + rustls, pinned to the community's fingerprint): resumable upload, downloads over Range, speed caps |
| Attachment encryption | Rust, byte-for-byte the format of `attachmentCrypto.ts` (tested against vectors from both TS modules) |
| Upload thumbnails, ThumbHash, EXIF orientation | Rust, off the UI thread |
| Inline video / audio attachments | Our FFmpeg player on the §7 path + the core's audio output |
| Link previews (with the private-network guard), GIF search | Ports of `linkPreview.ts`, `publicNet.ts`, `gifs.ts`: same caps, cache and Privacy toggles |
| E2EE at-rest key, saved sign-in | The OS keyring (Secret Service on Linux, Credential Manager / DPAPI on Windows) |
| UI sounds, mic test outside a call | The core's audio output, preloaded PCM |
| Attention, taskbar progress | The platform's urgency hint / `FlashWindowEx`, `ITaskbarList3` |
| Deep links, single instance | `decibell://` registration per OS, forwarded to the running instance |
| Global hotkeys | The core's listeners as today; the recorder maps GPUI key events to the `KeyboardEvent.code` names bindings use |
| Auto-away | The core's `system_idle.rs` on Wayland; one timer at the threshold elsewhere |
| Auto-update, crash reports | Last phase: an own updater against GitHub releases; the `sentry` crate, opt-in as today |

## 9. Fidelity and captures

Tokens are generated (§6.2). Both clients are fed the same deterministic fixtures through the local stack
(`tools/dev/local-stack.sh seed`). The GPUI client captures itself (`decibell --capture <png> --screen <name>
--theme <name>`), the Electron client is captured through its UI preview harness / CDP; a compare page shows
each pair. The owner judges the feel (scrolling, motion, DPI) and signs off the look.

## 10. Order of work

1. The core split (§4), Electron's bar after each step.
2. The skeleton: pin + patches, workspace, theme generator, fonts, window frame, the design-system components,
   the capture and self-test harness, the frame and wakeup counters.
3. Stream playback (§7).
4. Chat (§6.3).
5. Sign-in, servers, channels, DMs with E2EE, voice with MLS; then every other row of `docs/FEATURES.md` §1–§9,
   tracked in `gpui-client/PARITY.md` (per row: status, evidence, the Electron file it mirrors).
6. Packaging, auto-update, crash reports.

Each phase logs its numbers against §3 in `gpui-client/HANDOFF.md`.

## 11. Verification bar for the client

Before committing a change under `gpui-client/` (and Electron's full bar for anything under
`crates/decibell-core`):

```sh
cd crates/decibell-core && cargo test --lib
cd gpui-client && cargo test --workspace
cd gpui-client && cargo run --release -p decibell-app -- --selftest all     # hidden window, the local stack up
cd gpui-client && tools/gen-theme/check.sh                                    # regenerating changes nothing
```

and, for a change that could move a target, `tools/perf/measure.sh` on the affected scenario.

## 12. Risks

- **Parity is large**: the renderer is ~290 files / 62k lines of TS + TSX and Electron main 5.8k. The checklist
  keeps it honest; the shared core removes the protocol risk.
- **GPUI is a moving, unversioned target** with gaps a chat client meets early (text input, inline images in
  text, external textures, capture). The pin plus a small patch set contains it; each patch is written to be
  upstreamable.
- **Text fidelity**: Chromium and GPUI's text stack shape and rasterise the same fonts differently; variable
  axes may need a patch or static instances. The owner's eye decides.
- **Only NVIDIA hardware on hand**: AMD / Intel decode paths are reasoned and compile-checked until someone
  runs them ("Needs owner").
- **Windows is compile-checked only** in this session.
