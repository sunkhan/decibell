# Qt native client

2026-11-07. Owner brief: a native desktop client for Windows and Linux in Qt 6 Quick (QML) + Rust, judged
first by resource use, then by full feature parity with the Electron client (the reference for every
behaviour), then by visual fidelity in all seven themes. The Electron client keeps shipping until this one
reaches parity.

**Status: approved, 2026-11-07.** The owner accepted every recommendation in §14 (D1–D11). The baseline (§2)
is measured except the stream scenarios, which wait for streamers. Nothing below has been built yet beyond the
toolchain (§5.1) and the measurement harness (`tools/perf/`); next is the spike (§12).

**Hardware for both OSes is one machine**: the owner's Linux box is this Windows machine dual-booted (same
i7-12700K and RTX 4080), so Windows and Linux numbers compare directly. It also means the only GPU on hand is
NVIDIA: the AMD and Intel routes of §8.3 have no owner hardware to run on (§15).

Decided by the owner before this doc (not reopened here without spike data): Qt 6 Quick / QML (≥ 6.8 LTS,
≥ 6.7 for per-corner radii, variable font axes and curve text), cxx-qt for the Rust↔Qt bridge, Qt linked
dynamically (LGPLv3); one shared Rust core (`decibell-core`) under both clients; zero-copy video (hardware
decode → native GPU texture → a custom `QQuickItem`, NV12 → RGB on the GPU, the render thread pulls the newest
frame on vsync); everything else native reuses the existing Rust; GPUI, Flutter and webviews set aside;
**Slint is the fallback** if the Qt spike fails. Every owner decision in CLAUDE.md applies to this client.

---

## 1. How the client is judged

Every design choice is judged by memory, CPU, GPU, wakeups, startup time and binary size, measured against the
Electron client on the same machine with the same harness:

- `tools/perf/measure.ps1` samples one client's whole process tree from Windows performance counters (one
  `Get-Counter` stream, so rates are computed by PDH over the interval and the harness adds no polling inside
  the measured app): CPU, working set, private working set (Task Manager's "Memory"), private bytes (commit),
  threads, handles, **context switches per second as the wakeup proxy**, GPU engine utilisation by engine type
  (3D, VideoDecode, VideoEncode, Copy), dedicated and shared GPU memory, and optionally DWM's GPU time. It writes
  every sample (CSV) and mean / p50 / p95 / max (JSON). The Linux twin (`measure.sh`: `/proc` + `pidstat -w` +
  `nvidia-smi pmon` / `intel_gpu_top` / `amdgpu_top`) comes with the Linux half of the spike.
- `tools/perf/startup.ps1` (§2) times launch → first visible window and launch → settled (process tree under
  2 % of one core for 2 s), over 5 warm runs after one discarded run.
- Every phase reports its own numbers against §2 in its review-log entry.

## 2. Electron baseline (0.9.4, this machine)

Machine: i7-12700K (12 cores / 20 threads), 32 GB, RTX 4080 (driver 32.0.16.1714), Windows 11 Pro 26200,
primary display 2560 × 1440 @ 279 Hz plus a second display. Electron 0.9.4 as installed by its NSIS installer
(`%LOCALAPPDATA%\Programs\decibell-electron-client`), saved sign-in, release build.

| Scenario | Metric | Electron 0.9.4 |
|---|---|---|
| Install | Size on disk | **436.4 MB** |
| Startup (warm, ×5, saved sign-in) | Launch → window visible / → settled | **904 ms** (878–905) / **1.95 s** (1.69–5.92; sign-in latency) |
| Idle, signed in, Home (window focused; 7 processes, 218 threads) | Private WS / private bytes / WS | **262 MB** / 600 MB / 754 MB |
| | CPU, % of one core (mean / p95) | **0.56 %** / 2.98 % |
| | Context switches per s (p50 / p95) | **161** / 218 (one 29k outlier sample) |
| | GPU: 3D / dedicated memory | ~0 % / 125 MB |
| Idle, signed in, a busy channel open (DailyGaming #general; the brief's 3 servers don't exist: DailyGaming is the only community running), another window focused (verified every 5 s) | Private WS / private bytes / WS | **246 MB** / 716 MB / 744 MB |
| | CPU, % of one core (mean / p95) | **0.58 %** / 2.99 % |
| | Context switches per s (p50 / p95) | **153** / 270 |
| | GPU: 3D / dedicated memory | 0 % / 240 MB (the channel's images) |
| Same, focus not controlled (launched focused; the owner was using other apps) | Private WS / CPU mean / context switches p50 / p95 | 248 MB / 0.77 % / 150 / 518 |
| Watching 1 × 1440p60 / 1 × 1440p120 | CPU, GPU 3D + VideoDecode, memory, frame pacing | _needs streamers on other machines_ |
| Watching 2 × 1440p60 / 2 × 1440p120 | Same | _needs streamers on other machines_ |

Method: 2 min settle after the scenario is reached, then 300 samples at 1 s (`tools/perf/measure.ps1`), on a
machine with no build or download in progress. The owner uses the machine during runs, so focus is checked
rather than assumed: the Home and "not controlled" runs were launched focused but may have lost focus; the
unfocused run logs the foreground window every 5 s (it was Firefox / Discord throughout, never Decibell). The
channel was opened through CDP (`tools/perf/open-channel.mjs`, Electron started with
`--remote-debugging-port`), so nothing inside the page had keyboard focus (no composer caret). DWM's GPU time
is left out of the comparison: it includes whatever else the owner had on screen. Context switches are quoted as p50 / p95 because PDH reports an occasional outlier sample when
thread instances renumber.

## 3. Resource targets

Accepted by the owner (§14, D8). Ratios are against §2 on the same machine and scenario; the bold figure is that ratio applied to the measured baseline.

| Metric | Target |
|---|---|
| Idle private working set (busy channel open) | ≤ 1/3 of Electron: **≤ 82 MB** (Electron 246 MB) |
| Idle CPU | ≤ 1/10 of Electron: **≤ 0.06 % of one core** (Electron 0.58 %); no timer of ours firing at idle (only the 30 s central / community pings) |
| Idle context switches / s | **≤ 30 / s from our own threads** (owner, D12): the GPU driver's threads are reported beside it, not counted. NVIDIA's D3D11 driver keeps one thread waking ~80 times a second in any process with a D3D11 swap chain (Electron has it twice, ~110–130 each); see `tools/perf/threads.ps1` |
| Render loop at idle | 0 frames rendered while nothing on screen changes |
| Startup to first frame (warm) | ≤ 1/3 of Electron: **≤ 300 ms** to a visible window (Electron 904 ms) |
| Install size (Windows, with FFmpeg) | ≤ 1/4 of Electron: **≤ 109 MB** (Electron 436 MB) |
| Watching 1440p120 × 2 | CPU ≤ 1/3 of Electron, GPU 3D ≤ Electron, no frame dropped or duplicated beyond the source's, presented-frame jitter p99 ≤ one refresh interval |
| Scrolling a 10k-row channel | Every frame inside the refresh interval at 279 Hz (p99 frame time ≤ 3.6 ms on the GUI and render threads); a history prepend moves visible content by 0 px |

## 4. Repository layout

```
crates/decibell-core/        the shared core: net, media, e2ee, hotkeys, config, presence, system_idle,
                             state, proto (prost build.rs), the EventSink trait and the command API (§6)
electron-client/native/      the napi addon, now thin: lib.rs (init/shutdown), the napi EventSink,
                             commands/*.rs as #[napi] wrappers over decibell-core
qt-client/
  CMakeLists.txt             the app (CMake + cxx-qt-cmake / Corrosion, §5)
  rust/                      decibell-qt: cxx-qt bridge crate (models, event pump, commands, video hub)
  src/                       C++: main.cpp, VideoItem, MessageText, list helpers, window chrome
  qml/Decibell/...           QML modules (App, Chat, Members, Voice, Settings, Theme, Components)
  assets/                    fonts, twemoji SVGs (generated, gitignored), lucide SVGs
  tools/                     gen-theme (globals.css → Theme), capture harness
  HANDOFF.md                 the Qt client's hand-off: build, gotchas, open items
  PARITY.md                  the parity checklist, one row per docs/FEATURES.md row
tools/perf/                  client-agnostic measurement (measure.ps1, startup.ps1, later measure.sh)
tools/qt/fetch_qt.py         installs a prebuilt Qt without an account (aqtinstall can't read 6.12's layout)
```

**No root Cargo workspace** at first (§14, D3): `electron-client/native` and `qt-client/rust` each depend on
`crates/decibell-core` by path and keep their own `Cargo.lock` and target directory, so Electron's build, CI
caches (`swatinem/rust-cache` keyed on `electron-client/native`), `copy-dlls.cjs` and the release workflow are
untouched. CI path filters (`win-native-check.yml`, `electron-release.yml`) gain `crates/**`.

## 5. Build

### 5.1 Windows toolchain (set up on this machine, 2026-11-07)

| Tool | Version | Where / how |
|---|---|---|
| Visual Studio 2022 Build Tools | 17.14 (MSVC 19.44), VC tools + Windows SDK + CMake/Ninja component | `winget` (the only step needing admin) |
| Rust | 1.99.1, `stable-x86_64-pc-windows-msvc` | rustup, per user |
| Node | 20.20.2 (CI pins 20) | portable, `C:\dev\node20` |
| CMake | 4.4.5 (`CMAKE_POLICY_VERSION_MINIMUM=3.5` for audiopus, as in CI) | portable, `C:\dev\cmake` |
| Ninja | from the Build Tools CMake component | — |
| protoc | 27.5 (CI pins 27.x) | portable, `C:\dev\protoc` |
| LLVM / libclang | 23.1.5 (`LIBCLANG_PATH` for ffmpeg-sys-next's bindgen) | portable `.tar.xz`, `C:\dev\llvm` |
| vcpkg + FFmpeg | CI's pinned commit `e5a1490e`, `ffmpeg[nvcodec,amf,qsv]:x64-windows` | `C:\dev\vcpkg`, `VCPKG_ROOT` |
| Qt | 6.12.0 `msvc2022_64` + qtshadertools + qtimageformats | `tools/qt/fetch_qt.py` → `C:\Qt\6.12.0\msvc2022_64` |
| cxx-qt | 0.10.0 (cxx-qt-cmake via FetchContent, Corrosion 0.6) | smoke-built and run (cxx-qt's `qml_minimal`) |
| uv | latest | runs Python tools with a managed interpreter, nothing installed system-wide |

Gotchas found on the way (also in `qt-client/HANDOFF.md`): aqtinstall 3.3.0 asks for
`qt6_6120/qt6_6120/Updates.xml` but Qt now publishes per-arch folders (`qt6_6120/qt6_6120_msvc2022_64/`), and those
archives have no `<version>/<arch>/` prefix; uv's managed Python has no CA store (`truststore`); Windows `tar`
can't decode LLVM's `.tar.zst` (window too large; use the `.tar.xz`); **MAX_PATH**: cxx-qt-build's generated
`…\cargo\build\x86_64-pc-windows-msvc\release\build\cxx-qt-<hash>\out\cxx-qt-build\initializers\…\call-initializers.cpp`
overflows 260 characters under a long build root and `cl.exe` fails with C1083 (build from a short path or set a
short `CARGO_TARGET_DIR`); PowerShell treats a native tool's stderr as an error under `$ErrorActionPreference =
'Stop'` (run rustup / git / vcpkg through `cmd /c … 2>&1`).

### 5.2 How the app builds

**CMake + cxx-qt-cmake (Corrosion)** (§14, D2). The client needs C++ anyway (scene-graph items:
`VideoItem`, `MessageText`), and Qt's tooling is CMake-native: `qt_add_qml_module` runs **qmlcachegen**, which
compiles typed bindings and functions to C++ ahead of time; `qmlaotstats` reports what fell back to the
interpreter (the build fails if a hot-path module isn't fully compiled); `qt_generate_deploy_qml_app_script` /
`windeployqt` deploy. `cxx_qt_import_crate(MANIFEST_PATH rust/Cargo.toml CRATES decibell_qt …)` builds the Rust
bridge crate as a static library and its QML module (`cxx_qt_import_qml_module`). Rust links the release MSVC
runtime, so the app sets `CMAKE_MSVC_RUNTIME_LIBRARY=MultiThreadedDLL` and maps Qt's Debug config to Release.
cxx-qt-cmake is pinned to a tag, not `main` (the upstream example uses `main`).

QML rules: every QML file is in a module, every property and function is typed, no JavaScript in delegates or
hot paths (models and layout live in Rust / C++), no `Qt.binding` churn in delegates, `QtQuick.Templates` as the
base of our controls (no Basic / Fusion / Material styling to override).

### 5.3 FFmpeg

Windows uses the same vcpkg FFmpeg as Electron (dynamically linked, LGPL). Its default feature set already has
the H.264 / HEVC / AV1 decoders and the D3D11VA hwaccel. Software AV1 decode needs dav1d (not in CI's feature
list; decide when the decode fallback is built). The Linux build (`native/scripts/build-ffmpeg-linux.sh`,
`--disable-everything` + encoders) gains the h264 / hevc / av1 decoders and parsers and the decode hwaccels of
§8.3 for the Qt client; Electron doesn't load them.

### 5.4 Verification bar additions

For the Qt client, before committing: the build of `qt-client` (Release), the Rust tests of `decibell-core`
and `decibell-qt`, `qmllint --max-warnings 0` over the QML modules, the `qmlaotstats` gate, the theme-generator
check (regenerating produces no diff), and the capture harness for screens a change touches. For any change
under `crates/decibell-core`, Electron's full bar (CLAUDE.md) as well: it is Electron's native code.

## 6. The core split (`decibell-core`)

ARCHITECTURE §7.3 maps the coupling; napi lives only in `lib.rs`, `events.rs` and `commands/`. The split, done
on a branch that merges only when Electron passes its whole verification bar on Linux and Windows (and Windows
Native Check):

1. **Move** `net/`, `media/`, `e2ee/`, `hotkeys/`, `config.rs`, `presence.rs`, `system_idle.rs`, `state.rs` and
   the prost `build.rs` into `crates/decibell-core` as `pub` modules. Move the four call-outs from `net/` into
   `commands/` (`connect_with_invite`, `signed_out_elsewhere`, `apply_hidden`, `rebind_voice_group`) into core
   functions the napi wrappers call. Dead code named in §7.3 stays behind (not moved).
2. **Events** go through one trait instead of three `OnceLock` ThreadsafeFunctions:

   ```rust
   pub trait EventSink: Send + Sync + 'static {
       /// A UI event, from any core thread. Must not block.
       fn event(&self, event: CoreEvent);
       /// A whole, decrypted, encoded frame of a watched stream (or our own preview), on the
       /// video receive thread. Must not block; the sink owns its queueing and shedding.
       fn stream_frame(&self, frame: StreamFrame);
       fn stream_thumbnail(&self, thumb: StreamThumbnail);
   }
   ```

   `CoreEvent` is a typed enum, one variant per event constant in today's `events.rs`, each carrying today's
   payload struct (§14, D4). `CoreEvent::to_json()` returns today's `(name, serde_json::Value)`, so the napi sink
   emits byte-identical events and the renderer doesn't change; the Qt sink matches on the variant and updates
   its models with no JSON in between. The stream-frame shedding (cap 8, discontinuity flag) moves from
   `events.rs` into the napi sink; `STREAM_FRAME_SINKS` and input-level reporting move into core. Tests install
   a recording sink, so the `cfg(not(test))` emit wrappers (`hotkeys/mod.rs`, `e2ee/session.rs`,
   `video_pipeline.rs`, `encoder_thread_linux.rs`) go away.
3. **Commands**: the 132 `#[napi]` functions keep their names and argument structs; their bodies move into
   `decibell_core::api::<area>` as plain (mostly `async`) Rust functions returning `Result<T, CoreError>`. The
   argument structs live in core with `#[cfg_attr(feature = "napi", napi(object))]`, so there is one definition
   and the Qt build never compiles napi. The napi crate's commands become one-line wrappers.
4. **Runtime**: core never builds a runtime and never calls a bare `tokio::spawn`. `init` takes a
   `tokio::runtime::Handle` and core spawns through it (`rt::spawn`), so calls from threads outside the runtime
   (the Qt GUI thread) work. The napi addon passes napi's runtime; the Qt client owns its own (§7).
5. **Globals** (`AppState`, `WATCHED_STREAMS`, the voice gate, the hotkeys hub) stay as they are: one client per
   process. Turning them into an explicit `Core` handle is a later refactor, not part of the split.
6. **Per-frame allocations** on the receive path (one `Vec<u8>` per reassembled frame, decrypt buffers) move to a
   recycled buffer pool. The Qt decoder hands the buffer to FFmpeg as a refcounted `AVBufferRef` whose free
   callback returns it to the pool, so no frame is copied or allocated between the socket and the decoder.

## 7. Threads and the runtime model (Qt client)

| Thread | Owns | Wakes on |
|---|---|---|
| GUI (Qt event loop) | QML, models, input | input, posted core events, timers we can name |
| Render (Qt threaded render loop) | scene graph, swap chain, `VideoItem` texture pick-up | a frame requested by an update (never on its own at idle) |
| tokio runtime (2 workers; the spike measures 2 vs 4) | connections, protocol state machines, e2ee workers | sockets, core timers (30 s pings) |
| Media (existing core threads) | voice capture / playback, UDP receive, video reassembly, encode | audio device clocks, sockets |
| Decode (one per watched stream) | FFmpeg hardware decode + NV12 → RGB | a frame from the receive thread |

- **Core → UI**: the Qt `EventSink` pushes `CoreEvent`s onto a lock-free queue and posts one wake to the GUI
  thread only when the queue goes from empty to non-empty (`CxxQtThread::queue`); the GUI thread drains the whole
  queue in one turn. A busy channel costs one GUI wakeup per burst, not per message.
- **UI → core**: QML calls invokables on Rust QObjects; they spawn the core call on the runtime and post the
  result back the same way. The GUI thread never blocks on the runtime.
- **Models** are Rust structs behind `QAbstractListModel` (cxx-qt `#[base = QAbstractListModel]`, `cxx_override`
  for `rowCount` / `data` / `roleNames`; verified in cxx-qt 0.10's `custom_base_class` example). Users are keyed
  by uid; names come from one users model, as in the renderer's `usersStore`.
- **No polling**: no QML `Timer` without a named reason (the slowmode countdown, the 45 s ring timeout, the
  auto-away threshold, each running only while its state is live); animations run only while visible and while
  their state is live, so the render loop renders nothing at idle. A spinner left running off screen would make a
  279 Hz display render 279 frames a second; the capture harness checks the frame counter at idle.
- **Auto-away** uses `GetLastInputInfo` with a single timer at the threshold (re-armed from the last input
  time), and session lock / sleep notifications; on Wayland core's `system_idle.rs` as today.

## 8. Video

### 8.1 Common path

Core's receive thread reassembles, FEC-recovers and decrypts frames exactly as for Electron and calls
`EventSink::stream_frame`. The Qt sink routes each frame by streamer uid to that stream's decoder thread through
a 3-frame queue; on overflow it sheds to the next keyframe and asks the streamer for one (Electron's
discontinuity logic). The decoder thread feeds FFmpeg (hardware hwaccel, §8.2–8.3), converts the decoded NV12
surface to an RGB texture on the GPU **at the size the item is drawn** (a 1440p stream in a 480 px tile is
converted to 480 px wide), and publishes it into a small ring. `VideoItem` (C++ `QQuickItem`) holds a ring
reference; on the render thread, in `updatePaintNode`, it takes the newest completed slot and wraps it once per
slot with `QNativeInterface::QSG*Texture::fromNative`. A new frame calls `QQuickItem::update()` (posted to the GUI
thread), so the window renders only when a frame arrived and at most once per vsync. No frame touches the CPU.

The persistent mini-player (one player reparented between the full view, the grid tile and the mini-player,
never re-created) maps to one `VideoItem` per stream that is reparented the same way; the decoder lives with the
stream, not the item.

Self-preview decodes our own encoded frames like any other stream (what Electron does), which keeps one path; a
later optimisation can share the capture texture instead.

### 8.2 Windows: one D3D11 device, ours, handed to Qt

The client creates the D3D11 device itself (on the adapter Qt would pick, with `VIDEO_SUPPORT | BGRA_SUPPORT`, and
`ID3D11Multithread::SetMultithreadProtected(TRUE)`) and gives it to Qt before the window is exposed
(`QQuickWindow::setGraphicsDevice(QQuickGraphicsDevice::fromDeviceAndContext(device, context))`; verified present in
Qt 6.12). Owning the device means the multithread protection is on before Qt issues its first call, the device is
created the way FFmpeg's own d3d11va device is (with `VIDEO_SUPPORT`), and the decoder knows which adapter it is
on; reaching into the device Qt made after the fact gives none of those for certain.

- **Decode**: FFmpeg's d3d11va hwaccel on that device (`AVD3D11VADeviceContext` built from our device), on the
  decoder thread. H.264 / HEVC / AV1 on every GPU with the hardware block; AV1 needs RTX 30 / RX 6000 / Arc /
  Intel 11th gen or newer.
- **NV12 → RGB**: the D3D11 video processor (our `video_processor.rs` runs it in the other direction already)
  blits the decoder's texture-array slice into a BGRA8 texture of the ring, converting BT.709 limited range to
  full-range RGB and scaling to the drawn size in one fixed-function pass.
- **Synchronisation**: one device and one immediate context, so the GPU executes the decoder's work, the blit
  and Qt's draw in submission order; nothing waits on a fence or keyed mutex. The CPU side is a slot index
  published with release / acquire ordering, and a slot the render thread is drawing is never handed back to the
  decoder until the frame after it was presented (ring of 4).
- This is the mpv and Chromium model (one device shared by decoder and renderer). The alternative is Qt
  Multimedia's (separate decode device, a copy into keyed-mutex shared textures): more isolation, one more copy
  and a cross-device sync per frame. §14, D6; the spike measures render-thread stalls on the shared device and
  switches if they show.
- Device loss (driver update, TDR): `sceneGraphInvalidated` tears the decoders down; they restart on the new
  device with a keyframe request.
- D3D11 stays Qt's backend on Windows (`QSG_RHI_BACKEND` unset → D3D11). D3D12 brings nothing this client needs.

### 8.3 Linux

Two zero-copy routes; the spike picks with numbers (§14, D7):

- **Vulkan (recommended to try first)**: Qt Quick on its Vulkan RHI with a `VkDevice` we create (graphics +
  video-decode queues, the extensions FFmpeg's Vulkan hwaccel needs) and hand to Qt
  (`QQuickGraphicsDevice::fromDeviceObjects`). FFmpeg decodes with Vulkan Video into multi-planar images; a small
  compute pass, recorded into Qt's command buffer between `beginExternalCommands` / `endExternalCommands`,
  converts and scales into the ring's RGBA image, wrapped with `QSGVulkanTexture::fromNative`. One path for
  NVIDIA (proprietary driver), AMD (RADV) and Intel (ANV), with no CUDA and no EGL interop.
- **OpenGL (Qt's default on Linux)**: VAAPI → DRM PRIME (DMA-BUF) → one `EGLImage` per plane (R8 + GR88) → GL
  textures → an NV12 material (shaders built with `qsb`); NVIDIA has no VAAPI by default and needs NVDEC → CUDA →
  CUDA-GL interop (`cuGraphicsGLRegisterImage`), i.e. a second vendor path.
- Fallback for any driver without a working hardware decoder: FFmpeg software decode (dav1d for AV1) into a
  persistent mapped staging buffer → texture upload. One copy, still no per-frame allocation.

### 8.4 Measuring the video path

Per frame, the client records arrival (receive thread), decode done, converted, picked up by the render thread,
and presented (DXGI frame statistics / `vkGetPastPresentationTimingGOOGLE` where available) into a fixed ring,
dumped on demand (`DECIBELL_VIDEO_TRACE=<file>`). Reported: presented-frame interval histogram vs the source's,
frames dropped / duplicated, decode → present latency, plus `measure.ps1`'s CPU / GPU / memory. PresentMon
(Windows) cross-checks the display side.

## 9. UI layer

### 9.1 Theming (generated, never hand-copied)

`qt-client/tools/gen-theme` parses `electron-client/src/styles/globals.css` (the `@theme` block, `:root`, every
`:root[data-theme=…]` block including shared ones, `var()` chains, `calc()` over the type scale) and writes
`qml/Decibell/Theme/Palettes.qml`: one plain object per theme with every `--t-*` role resolved, plus the derived
`--color-*`, radius, shadow, font-role and type-scale tokens. `Theme` (a QML singleton) exposes typed properties
read from the active palette; a theme switch is one assignment, as `data-theme` is in Electron. The generated
file is committed and the verification bar regenerates it and fails on a diff, so tokens can't drift. Per-theme
component rules in the CSS (the console `dsel-*` overrides, `console-split`'s `.chrome-scope`, the image viewer's
light stages) are ported by hand into the components, with the CSS line referenced next to each.

- **Radii**: `Rectangle.topLeftRadius` etc. (6.7+); the console themes flatten them exactly as the CSS does.
- **Shadows**: `RectangularShadow` (an analytic rounded-rectangle shadow, no offscreen pass) for the three
  elevation tokens and the focus ring; `MultiEffect` / layers only where a design needs a real blur, each one
  measured.
- **Fonts**: Inter Variable, Mona Sans Variable (wght and wdth axes through `font.variableAxes`; the server tiles
  and the wordmark use the expanded end) and JetBrains Mono 400–700, embedded as resources, the same files the
  renderer imports. On Windows Qt 6.12 renders through DirectWrite; text uses `NativeRendering` (glyph cache,
  hinted, closest to Chromium's DirectWrite text). Where the two still differ (Chromium uses Skia's AA and its own
  gamma), the owner judges at the milestones; curve rendering is for large or animated text only.
- **Icons**: lucide SVGs (the renderer uses six stroke widths, 1.5 to 3.5, so an icon font can't match),
  rasterised by QtSvg at the exact device-pixel size and stroke, cached per (icon, stroke, size, DPR) and tinted
  in a shared material, so a colour change never re-rasterises.
- **Twemoji**: the same SVG set as the renderer (`@twemoji/svg`), rasterised at the device-pixel size into the
  scene graph's texture atlas and placed as inline objects by `MessageText` with the renderer's box size and
  vertical offset, so emoji sit exactly where Chromium puts the `<img>`. (A COLR emoji font is the alternative;
  inline objects match the renderer's layout without depending on colour-font support in Qt's font engines.)

### 9.2 Message list

The renderer's list is a real-DOM sliding window by owner decision; in Qt, a `ListView` over a windowed model is
the native equivalent, and the invariants carry over:

- **Model**: a Rust `QAbstractListModel` holding the loaded slice (paged from core like the renderer: around /
  before / after), rows keyed by message identity (id, else nonce), never index. 10k rows in the model cost a few
  MB of Rust structs; only rows near the viewport have delegates.
- **Rows**: one C++ `MessageText` item per message body: the markdown subset is parsed in Rust (a port of
  `richText.ts`) into styled runs; the item lays them out with `QTextLayout` and draws them as one
  `QSGTextNode` (`QQuickWindow::createTextNode`, `addTextLayout`, public since 6.7) plus rectangle nodes for inline
  code chips and spoilers and atlas image nodes for emoji. Hover, links and selection are hit-tested inside the
  item, not by per-span QML items. Fenced code blocks use KSyntaxHighlighting (LGPL, Qt-native, superset of the
  27 lowlight grammars); `$TeX$` uses MicroTeX (C++, MIT) instead of KaTeX. Chrome around the body (avatar,
  name, time, reply line, attachments) is QML with whole-pixel, content-independent heights, as in the renderer.
- **Heights** are exact, never estimated: a row's height is its laid-out height, so nothing jumps when a row
  scrolls into view. A history prepend inserts above the viewport; `ListView` moves `originY` and the visible
  rows stay put (0 px). Scroll positions are stored as `{anchorId, offset, atBottom}` per channel / peer, as in
  the renderer.
- **Selection** spans rows (as DOM selection does): a selection controller keeps (row id, character) endpoints;
  each `MessageText` draws its part through `addTextLayout`'s selection range; copy builds the text from the
  model, so rows scrolled out still copy.
- **Wheel scrolling** is animated to match Chromium on Windows (pixels per notch and the impulse curve, measured
  from a recorded wheel trace in Electron); touchpads pass through unanimated.
- **Images** decode at display size (`QImageReader::setScaledSize`; JPEG scales in the DCT), pick the server's
  320 / 640 / 1280 thumbnail by drawn size × DPR, and live in a byte-bounded LRU of textures; nothing is stored
  across restarts (owner).

### 9.3 Member list

The same pattern: a windowed Rust model fed by core's roster paging (Online / Offline by the community's view),
delegates recycled (`reuseItems`), avatars from one uid-keyed cache (6 fetches at a time, ≤ 400 off-screen
pictures, as `avatarStore.ts`).

### 9.4 Window

Electron's window is frameless with the renderer's own 32 px title bar (min / max / close, drag region), 1280 ×
800 by default, 800 × 600 minimum. The Qt window draws the same title bar; on Windows it keeps a native frame
whose client area covers the title bar (`WM_NCCALCSIZE` / `WM_NCHITTEST`), so dragging, edge resize and Aero snap
behave natively. Snap layouts on hovering maximise (which Electron's frameless window doesn't show) would be a
visible difference, so it waits for the owner (§14, D9). Linux: client-side decorations drawn the same way,
`startSystemMove` / `startSystemResize`.

## 10. What replaces Electron's main process and Chromium services

| Responsibility (ARCHITECTURE §7.3) | Qt client |
|---|---|
| Video decode + render | §8 |
| Encode fallback | Not needed on Windows / Linux (native capture + encode already default there); a machine with no hardware encoder gets FFmpeg's software H.264 (libopenh264 is LGPL-compatible as a separate DLL; decide at that phase) |
| Capture source list | DXGI outputs + `EnumWindows` with thumbnails (Windows), the portal dialog (Linux, as today), producing the ids `source_id.rs` parses |
| Attachment upload / download / Range playback / save-as / Transfers / pacing | A Rust HTTPS client in core (hyper + rustls, pinned to the community fingerprint), shared resumable upload / download with speed caps |
| Attachment encryption | Moved into core (Rust, AES-256-GCM 64 KiB chunks, byte-for-byte the format of `attachmentCrypto.ts`, tested against vectors produced by both TS modules) |
| Upload thumbnails, ThumbHash, EXIF orientation | Rust (`image` crate, a ThumbHash port), off the GUI thread |
| Inline video / audio attachments (persistent layers) | Our own FFmpeg player on the §8 path + the native audio output, not Qt Multimedia (which would ship a second FFmpeg) |
| Link previews (+ the private-network guard), GIF search | Ported to Rust from `linkPreview.ts`, `publicNet.ts`, `gifs.ts`; same caps, cache and Privacy toggles |
| E2EE at-rest key | DPAPI / Credential Manager (Windows), libsecret (Linux) |
| UI sounds, mic test outside a call | The native audio output (cpal), preloaded PCM |
| Taskbar flash / progress | `FlashWindowEx`, `ITaskbarList3` |
| Deep links | `decibell://` registration per OS, forwarded to the running instance (single-instance lock) |
| Global hotkeys | Core's listeners as today; bindings stay `KeyboardEvent.code` names, so the recorder maps Qt key events to them |
| Settings in `localStorage` (~18 keys) | Move into `config.json` |
| Auto-update, crash reports | Last phase: an own updater; sentry-native (crashpad) for C++/Qt plus the `sentry` crate |

## 11. Visual fidelity

- **Tokens** are generated (§9.1).
- **Fixtures**: a deterministic generator writes the same synthetic data for both clients (a 10k-message busy
  channel shaped after a real one's statistics: message lengths, code, emoji, replies, links, attachments with
  thumbnails; a roster; servers). Real community messages are never committed (§14, D5).
- **Electron captures** come from the existing UI preview harness (HANDOFF §5.15: a scratch Vite build of the
  real components, stores seeded with `setState`, captured by an offscreen `BrowserWindow`), fed the fixture.
- **Qt captures** come from the client itself (`decibell-qt --capture <png> [--theme <name>]`): the window is
  created but never shown, and `QQuickWindow::grabWindow()` renders it through the real platform, D3D11 +
  DirectWrite, so nothing flashes on screen. **Not `QT_QPA_PLATFORM=offscreen`** (checked in the spike's first
  capture): that platform swaps in Qt's FreeType font database with grayscale AA and different advance widths, so
  lines wrap differently from the screen. Per screen, theme and DPR (1.0, 1.25, 1.5, 2.0).
- A compare page shows each pair side by side with a difference overlay. The owner judges the feel (scrolling,
  motion, DPI) at milestones.

## 12. Phase 1: the two-week spike (go / no-go)

Built in `qt-client/` on a `qt-spike` branch; no change to Electron except, if D1 goes that way, a debug-only
stream dump switch in native.

**(a) Video.** Watch a 1440p120 stream, then two, through decode → native texture → `VideoItem`, on Windows and
on Linux. The streams are real streams dumped at the receiver (§14, D1) and replayed with their original timing
into the same decoder entry point core will call, so the measured path is exactly the one the client will use.
Report frame pacing (§8.4), CPU, GPU, memory, against Electron watching the same streams live.

**(b) Chat.** Port the chat panel of a busy channel in Nocturne and Matinee: 10k variable-height rich rows
(markdown, inline code chips, twemoji, replies, attachments, selection), the member list, smooth wheel scrolling,
history prepends that don't move content. Report scroll frame times at 279 Hz, memory, startup to first frame,
plus side-by-side screenshots against Electron with the same fixture.

**Plan**: days 1–2 skeleton (CMake + cxx-qt app, fonts, theme generator, capture harness, Electron fixture
captures); days 3–6 Windows video; days 5–9 chat panel and member list; days 7–10 Linux video (the same machine booted into Linux, so it runs in a session there and the branch carries
everything that session needs);
days 9–10 measurements and the report page. Then stop for the owner's verdict.

**Go** if (a) meets §3's video targets on both OSes and (b) holds every frame at 279 Hz with 0 px prepend
movement and screenshots the owner finds hard to tell apart. **No-go** → the same spike in Slint.

## 13. After the spike

1. **Core split** on a branch (§6) until Electron passes its full bar (Linux, Windows, Windows Native Check).
2. **Vertical slice**: sign-in (password, saved, resume, devices), servers, channels and messages, DMs (with
   E2EE), joining voice (MLS strict).
3. **Parity**, section by section through `docs/FEATURES.md`, tracked in `qt-client/PARITY.md` (one row per
   FEATURES row: status, notes, the Electron file it mirrors).
4. **Packaging, updates, crash reports.** Windows installer with a trimmed deploy (no `opengl32sw.dll`, only the
   plugins used); Linux AUR package against the system Qt (§14, D10).

Each step gets a review-log entry with its numbers against §2.

## 14. Decisions (owner: every recommendation accepted, 2026-11-07)

| # | Decision | Recommendation (**owner: accepted**) |
|---|---|---|
| D1 | Where the spike's streams come from | **Dump real streams and replay them** into the real decode path (a debug-only `DECIBELL_STREAM_DUMP` switch in native's receive thread, compiled out of release). The spike then needs no core split and runs the same bitstream for every measurement. The alternative, doing the core split first so the spike watches live, puts weeks of Electron-risk work ahead of a go / no-go. |
| D2 | Build system | **CMake + cxx-qt-cmake (Corrosion)**, not cargo-only: the client needs C++ scene-graph items and Qt's QML AOT / deploy tooling is CMake-native. |
| D3 | Cargo layout | **No root workspace yet**; both clients depend on `crates/decibell-core` by path, keeping Electron's lockfile, target dir and CI caches where they are. A root workspace (one lockfile) can follow once both clients are stable. |
| D4 | Event sink shape | **Typed `CoreEvent` enum** in core, with `to_json()` giving the napi side today's exact `{name, payload}`. The alternative (keep `send(name, Value)` behind a trait) makes the Qt side parse JSON it could have had typed. |
| D5 | Spike chat data | **Synthetic fixture** shaped after the real busy channel's statistics, generated deterministically; real messages only on screen, never committed. |
| D6 | Windows video device model | **One D3D11 device, ours, handed to Qt** (mpv / Chromium model, no cross-device sync), switching to Qt Multimedia's separate device + keyed-mutex copy only if the spike shows render-thread stalls. |
| D7 | Linux graphics path | **Vulkan first** (Qt's Vulkan RHI + FFmpeg Vulkan Video: one zero-copy path on NVIDIA, AMD and Intel), OpenGL + VAAPI / CUDA-GL as the fallback; the spike measures Qt Quick's idle cost on both RHIs before settling. |
| D8 | Resource targets | §3 as proposed. |
| D9 | Windows snap layouts on the maximise button | Electron's frameless window doesn't show them; matching Electron means not showing them either. Recommend **match Electron** now, offer it as a separate improvement later. |
| D10 | Linux Qt | **System Qt** for the AUR package (Arch ships current Qt 6; much smaller package), bundled Qt for any AppImage. |
| D11 | Qt version | **Track the latest minor; spike on 6.12.0.** The public 6.8 LTS line stops at 6.8.3 on download.qt.io (later 6.8 patches are commercial), dynamic LGPL linking lets us move minors freely, and 6.12 has everything the brief asks for. |
| D12 | What the wakeup target counts (raised by the spike's measurements) | **Only our own threads** (owner, 2026-11-07). GPU driver threads are reported separately. |
| D13 | The Windows hotkey poller: 287 wakeups/s in Electron while any hotkey is bound (`hotkeys-poll`, `GetAsyncKeyState` per key every 10 ms) | **Option (c)** (owner, 2026-11-07): press actions (mute, deafen, leave, answer, decline) become system hotkeys (`RegisterHotKey`) on exactly the bound combo, event-driven, so those combos no longer reach other apps (as on X11); push to talk / push to mute stay polled, every 20 ms, only while such a binding exists, and still reach apps. Shipped in the shared native code, so Electron gets it too. |

## 15. Risks

- **Text fidelity**: Chromium (Skia) and Qt (DirectWrite glyph cache) rasterise the same font differently at
  13–15 px. Mitigation: NativeRendering, the same font files and features, gamma checked in the capture harness;
  the owner's eye decides.
- **Cross-row selection and the markdown subset** are custom work in `MessageText`; they are in the spike so the
  cost is known before go / no-go.
- **Linux Vulkan Video maturity** varies by driver and GPU generation; the OpenGL route and software decode are
  the fallbacks, and the decision is measured (D7).
- **Only NVIDIA hardware on hand** (one RTX 4080, both OSes): AMD (RADV, AMF) and Intel (ANV, QSV) decode
  paths can only be reasoned about and unit-tested until someone runs them on such a GPU; they are listed as live
  tests in `qt-client/HANDOFF.md`, as Electron's AMD AMF encode is today.
- **The shared D3D11 device** puts decoder submissions and Qt's rendering under one lock; mpv and Chromium ship
  this way, and D6 has the measured fallback.
- **Parity is large**: the renderer is 287 files / 61k lines of TS + TSX and Electron main 5.8k. The parity
  checklist keeps it honest; the core split removes most protocol risk, since every packet path is already
  shared.
- **MAX_PATH on Windows** with cxx-qt's generated paths (§5.1): keep build roots short.
