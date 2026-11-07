# Decibell Qt client — hand-off

The native desktop client for Windows and Linux: Qt 6 Quick (QML) over the shared Rust core. The Electron client
(`electron-client/`) keeps shipping until this one reaches parity and is the reference for every behaviour.

**Status (2026-11-07): spike, day 1** (branch `qt-spike`). Design (approved):
`docs/superpowers/specs/2026-11-07-qt-native-client-design.md` (read it first). Built so far: the CMake +
cxx-qt skeleton (`decibell-qt`: a Rust `Decibell.Core` module, `Decibell.Theme`, `Decibell.App`), the theme
generator (`tools/gen-theme`: 225 tokens × 7 themes from `globals.css`, `--check` for the verification bar), the
pinned font fetch (`tools/fetch-assets.mjs`) and `--capture`. The app shows a theme gallery (all seven themes'
surfaces, type and elevation side by side).

Build: `qt-client\tools\build-windows.cmd [Release|Debug] [builddir]` (fetches assets, checks the theme is
current, configures and builds into `C:\b\qt`). Run with `C:\Qt\6.12.0\msvc2022_64\bin` on PATH.

Read first: `CLAUDE.md` (rules, verification bar, owner decisions; all apply here), `ARCHITECTURE.md` (§7.3: what
of `electron-client/native` is reusable and what Electron does that this client replaces), `docs/FEATURES.md`
(the parity checklist), `electron-client/HANDOFF.md` §5 (most native gotchas apply here).

## 1. Building on Windows

One script sets up a machine (idempotent; only Visual Studio Build Tools asks for admin):

```powershell
powershell -ExecutionPolicy Bypass -File tools\windows\setup-dev.ps1
```

It installs, per user into `C:\dev`: Node 20, CMake 4.x, protoc 27.x, LLVM (libclang), uv, Rust stable (MSVC),
vcpkg at CI's pinned commit with `ffmpeg[nvcodec,amf,qsv]` (the same FFmpeg Electron uses), and Qt 6.12.0
`msvc2022_64` (+ qtshadertools, qtimageformats) into `C:\Qt\6.12.0\msvc2022_64`. It sets `VCPKG_ROOT`,
`LIBCLANG_PATH` and PATH for the user. In every build shell also set `CMAKE_POLICY_VERSION_MINIMUM=3.5`
(audiopus_sys vs CMake 4, as CI does).

A Qt app build runs inside the MSVC environment:

```bat
call "C:\Program Files (x86)\Microsoft Visual Studio\2022\BuildTools\VC\Auxiliary\Build\vcvars64.bat"
set CMAKE_PREFIX_PATH=C:\Qt\6.12.0\msvc2022_64
set QMAKE=C:\Qt\6.12.0\msvc2022_64\bin\qmake.exe
cmake -S qt-client -B C:\b\qt -G Ninja -DCMAKE_BUILD_TYPE=Release
cmake --build C:\b\qt
```

Run with `C:\Qt\6.12.0\msvc2022_64\bin` on PATH (or after `windeployqt`). `tools\build-windows.cmd [Release|Debug]
[build dir]` does all of the above (assets, theme check, configure, build); FFmpeg for the video module comes from
`DECIBELL_FFMPEG_ROOT` (default `C:\dev\vcpkg\installed\x64-windows`; not `VCPKG_ROOT`, which vcvars64 repoints).

**Linux** (not built yet; the dual-boot session's first step): `QT_PREFIX=~/Qt/6.12.0/linux_gcc_64
tools/build-linux.sh` (Qt through `tools/qt/fetch_qt.py --host linux_x64 --arch linux_gcc_64`, see the script).
The video module is Windows-only so far: CMake leaves it out elsewhere, `main.cpp` guards it with
`DECIBELL_HAS_VIDEO`, and `Main.qml` loads `VideoScreen.qml` by URL so the App module compiles without it. Only
reviewed for portability, never compiled on Linux: expect first-build fixes.

## 2. Gotchas found so far

- **MAX_PATH.** cxx-qt-build generates
  `<build>\cargo\build\x86_64-pc-windows-msvc\release\build\cxx-qt-<hash>\out\cxx-qt-build\initializers\crate_cxx_qt\call-initializers.cpp`;
  under a long build root that passes 260 characters and `cl.exe` fails with `C1083: Cannot open source file`.
  Keep the build directory short (`C:\b\qt`) or set a short `CARGO_TARGET_DIR`.
- **aqtinstall can't install Qt 6.12** (3.3.0): it asks for `qt6_6120/qt6_6120/Updates.xml`, but Qt now publishes
  per-arch folders (`qt6_6120/qt6_6120_msvc2022_64/`) whose archives have no `<version>/<arch>/` prefix.
  `tools/qt/fetch_qt.py` reads that layout (SHA-1 checked) and extracts into `<dest>/<version>/<arch>`.
- **uv's managed Python has no CA bundle**: `fetch_qt.py` uses `truststore` (the OS store).
- **LLVM's `.tar.zst` doesn't open with Windows `tar`** (zstd window too large); use the `.tar.xz`.
- **PowerShell 5.1 + native stderr**: with `$ErrorActionPreference = 'Stop'`, a native tool writing to stderr
  (rustup's `info:` lines, git progress) throws and can kill the tool mid-install (a half-installed rustup with no
  toolchain). Run native tools through `cmd /c "… 2>&1"`.
- **cxx-qt-cmake's upstream example fetches `GIT_TAG main`**; pin a tag (the tags have no `v`: `0.10.0`).
- **Logs of a `WIN32` app go to the debugger**, not stderr: set `QT_FORCE_STDERR_LOGGING=1` to see QML errors
  (a failed load otherwise just exits 1).
- **Captures**: `QT_QPA_PLATFORM=offscreen` renders text with FreeType (grayscale, different advances; lines wrap
  differently). Capture an unshown window on the real platform instead: `decibell-qt --capture out.png`. The
  window must still be `create()`d: a never-created window gets no geometry on Windows and its content lays out at
  width 0.

## 2a. Fidelity rules (Chromium → Qt)

- **Line height**: CSS `line-height: 1.45` is 1.45 × the font size. QML's default `lineHeight` multiplies the
  font's own line spacing, so use `lineHeightMode: Text.FixedHeight; lineHeight: px * leading`, unrounded
  (Chromium keeps fractional line boxes).
- **Inter's optical size**: Chromium sets `opsz` from the font size (`font-optical-sizing: auto`, clamped to
  14–32). Qt doesn't: Inter text sets `font.variableAxes: {"opsz": Theme.opsz(px)}`.
- **Italics are synthesised**: the renderer loads no italic face, so the client bundles none either and Qt
  slants the upright one, as Chromium does.
- **Mona Sans width**: server tiles and the wordmark use the `wdth` axis (`tTileStretchWdth`,
  `tWordmarkStretchWdth`), through `font.variableAxes`.
- **Names**: a QML property can't be called `console` (the JS global) or `layer` (`Item.layer` is FINAL).
- **Fractional sizes**: QML's `font.pixelSize` (and `QFont::setPixelSize`) is an **int**, so the theme's 14.5 / 11.25 /
  11.5 px sizes were silently truncated. Use `font.pointSize: 0.75 * px` (Qt 6's logical DPI is 96; scaling goes
  through the device-pixel ratio); `MessageText` takes real CSS px (`pixelSize`).
- **Emoji**: twemoji 15.0.0 SVGs (same as the renderer), a 24 px box with its bottom 0.2 em below the baseline
  (`vertical-align: -0.2em`); an unformatted emoji-only message draws them at 56 / 44 / 38 / 32 px for 1 / 2 / 3 / more
  (MessageText.tsx `jumboSizeFor`). File names drop U+FE0F inside multi-code-point sequences (Twemoji.tsx
  `toCodePoints`); a single text-default symbol (©, ❤) is an emoji only with the FE0F after it.
- **Inline code**: `bg-bg-darkest`, `rounded-sm`, 6 × 2 px padding, mono at 0.85 em, one box per line it covers
  (`box-decoration-clone`).

## 2b. Spike findings (measured on the owner's machine: i7-12700K, RTX 4080, 279 Hz panel)

**Chat screen v1, idle with the 10k-message fixture channel open** (`tools/perf/measure.ps1`, 30 s settle + 120 s;
Electron's row is the baseline in the design doc §2: DailyGaming #general, another window focused).

> **Not like for like yet.** The Qt column is the UI alone, reading a fixture file. Electron's includes the shared
> Rust core (connections, protocol state, E2EE / MLS, the hotkey and device threads), which the Qt client will also
> carry. Compare against "Qt UI + core" only (core measured on its own, below), never these columns directly, and
> don't count the memory target as met from them.

**Memory, like for like (VMMap private working set, idle, a channel open):**

| | MB | What it is |
|---|---|---|
| Electron 0.9.4, all 7 processes | 246–285 | `measure.ps1` / VMMap at another moment |
| Electron main process | 76.4 | Heap 17.5, Private Data 52.0 (V8 + PartitionAlloc), Image 3.6, Stacks 1.0 |
| **→ the Rust core in it, upper bound** | **≤ 19** | Rust's Windows allocator uses the process heap directly; Chromium's code doesn't, so the main process's 17.5 MB heap bounds the core's allocations (other DLLs' heap use is in it too) + its share of stacks |
| Electron GPU process | 75.5 | where Electron keeps the NVIDIA driver's user-mode memory |
| Qt spike UI, 10k-message fixture | 63.0 | Heap 50.6 (≈ 11 of it the fixture, held twice), Private Data 7.3, Image 4.6; includes the driver's memory (one process) |
| Qt spike UI, 150-message fixture | 69.8 | Heap 39.8, Private Data 25.1: different rows on screen, more decoded pictures resident in CPU memory |
| **Projected Qt client = UI + core** | **≈ 82–89** | ~3× below Electron; **at or just over the 82 MB target: not met yet** |

The definitive figure needs the core in-process, signed in (after the core split; a headless `decibell-core` harness
measures the core alone). The fixture-era `QString` copies of every row only matter for the 10k fixture (the real
model keeps a ~150-row window).

**Where the UI's memory goes** (`--selftest hold`: the hidden window rendered once, VMMap private working set after
12 s; lower than a visible window, which adds the swap chain and the driver's per-window state, but the same way
every time, and nothing on screen):

| Hidden window, 150-message fixture | Total | Heap | Private Data | Image |
|---|---|---|---|---|
| no screen at all (`--screen empty`) | 33.9 | 21.5 | 7.4 | 4.7 |
| the same on WARP (no NVIDIA driver) | 23.0 | 18.3 | 1.8 | 2.7 |
| theme gallery | 46.0 | 32.1 | 8.9 | 4.7 |
| chat screen, no pictures (`--no-images`) | 48.7–49.0 | 33.8–34.0 | 9.7–10.1 | 4.7 |
| chat screen with pictures | 50.3–51.0 | 35.3–36.0 | 9.9 | 4.8 |

So: NVIDIA's user-mode driver ≈ 11 MB of any D3D11 process; Qt + the QML engine + fonts + the theme ≈ 23 MB before
any screen; the chat screen ≈ 15 MB on top, spread over the whole frame (hiding row parts *raised* memory: more rows
fit). Decoded pictures cost only 1.3–2.1 MB (they're decoded at the drawn size), so a texture-only image provider
isn't worth it. Without a heap profiler (none installed; the Debugging Tools' UMDH would attribute the 34 MB heap)
the next levers are guesses; the core's real share (after the split) decides whether the 82 MB target needs them.
- **Crash fixed on the way**: `MessageText`'s emoji / chip textures were process-wide caches; a second grab of a
  hidden window renders through a new QRhi and was handed the destroyed one's textures (access violation in
  `setShaderResources`). Any scene-graph invalidation (a lost device) would have done the same. The caches are now
  per QRhi and forgotten in its cleanup callback (`<rhi/qrhi.h>`, hence `Qt6::GuiPrivate`).

| | Electron 0.9.4 | Qt spike |
|---|---|---|
| Private working set | 246 MB | **65 MB** (60.6 MB with our own D3D11 device, below) |
| Private bytes | 716 MB | 142 MB |
| GPU memory (dedicated) | 240 MB | 35 MB |
| Processes / threads | 7 / 213 | 1 / 44 (25 with our own device) |
| Startup to a visible window | 904 ms | **257 ms** (median of 5 warm runs) |

Wakeups, and what they turned out to be (`tools/perf/threads.ps1` names each busy thread):

- **`QDxgiVSyncThread`** (Qt's Windows platform plugin) waits on every vertical blank whether or not anything
  renders: 239 wakeups/s on the 279 Hz panel. The threaded render loop paces itself on Present without it, so
  `main.cpp` sets `QT_D3D_NO_VBLANK_THREAD=1` by default: **310 → 79 context switches/s** for the process.
- **NVIDIA's D3D11 driver** (`nvwgf2umx.dll`) keeps one service thread waking ~80/s in any process with a D3D11
  swap chain. `D3D11_CREATE_DEVICE_PREVENT_INTERNAL_THREADING_OPTIMIZATIONS` (on in `gpu_device.cpp`; off with
  `DECIBELL_D3D_DRIVER_THREADS=1`) removes the driver's other worker threads (47 → 25 threads, −5.6 MB) but not
  that one. Electron pays the same driver thread in its GPU process and again in a utility process.
- The client's own threads wake ~7–9 times a second at idle.
- **What the target counts** (owner, 2026-11-07, design D12): only our own threads (≤ 30/s); GPU driver threads
  are reported beside it. On that count the spike's chat screen is at ~8/s.
- **Electron, settled, same machine** (`threads.ps1`, 2 min after launch): 534–618 wakeups/s. The biggest is
  **our own core**: the `hotkeys-poll` thread (`native/src/hotkeys/windows.rs`, `GetAsyncKeyState` per watched
  key, then `sleep(10 ms)`) at **287/s** while any hotkey is bound, about 2.9 switches per pass, so each key query
  costs one. The Qt client inherits it at the core split. **Changed on main 2026-11-07** (owner, D13 option c): press actions
  are now `RegisterHotKey` system hotkeys (no wakeups until pressed); push to talk / mute and keys Windows can't
  take are polled every 20 ms (standalone benchmark: 74/s with 9 keys, 52/s for the sleep alone). A press
  binding on a key that types without Ctrl / Alt / Win (e.g. the owner's mute on `;`) stays polled by design. NVIDIA's driver thread wakes ~130/s in Electron's GPU
  process and again in a utility process; Chromium's own threads ~10/s each.

**Frame pacing while scrolling** (`decibell-qt --bench-scroll <s> [--bench-speed px/s]`: a constant scroll into
history, frame-stage timestamps on the render thread; Electron's counterpart `tools/perf/electron-scroll.mjs`, a
requestAnimationFrame-driven scroll of the real message list over CDP):

- The scene is cheap: per frame sync 0.03 ms, render 0.6–0.7 ms, Present 0.65 ms (p50). Hiding every part of the
  rows changed nothing, so row content isn't the cost at this stage.
- Pacing is the issue, and the vblank thread is load-bearing: Qt delivers a window's update request on it. Turned
  off alone (what was first shipped), the request falls back to a 5 ms timer that Windows' 15.6 ms granularity
  stretches to **~65 frames/s**. With it on, ~150 frames/s (the request waits for the next vblank, the frame then
  misses the one after). **Off + `QT_QPA_UPDATE_IDLE_TIME=0`** (now the default in `main.cpp`): the request is
  immediate and the swap chain paces the frame; idle wakeups from our threads stay ~0.
- Paired runs, same conditions, 1000 px/s, 6 s: quiet machine — Qt p50 3.6 ms with 3–8 missed vsyncs, Electron p50
  3.6 ms with 45–90; busy machine (the owner working) — Qt p50 5.4–6.1 ms, Electron 6.4–7.3 ms. Qt matched or beat
  Electron in every pair. **Neither holds every frame at 280 Hz while the machine is in use**; the go / no-go
  criterion needs a controlled session (nothing else running, same GPU power state for both).
- `--ablate avatar,header,body,images,divider` (debug) hides row parts for this kind of bisection.

Qt's other RHI backends at idle, same screen: D3D12 119/s and 114 MB, Vulkan 201/s and 171 MB, OpenGL 32/s but
188 MB. D3D11 stays (design D6).

## 2c. MessageText (`src/ui/messagetext.*`, QML module `Decibell.Ui`)

The message body: the richText.ts subset (bold, italic, underline, strike, inline code, links, fenced blocks, line
breaks) laid out once per row in a `QTextDocument` (wrap anywhere, minimum line height = the CSS line height, half
leading added on top), drawn as one `QSGTextNode` (`addTextDocument`) plus rounded backgrounds (nine image slices of one
rounded-square atlas texture per colour: **not `QSGNinePatchNode`, which owns and deletes its texture**, so nodes
sharing one crashed at teardown) and twemoji as atlas textures at exactly the CSS position. `updatePaintNode` runs
only when the text or width changes, never while scrolling. With it: scrolling the 10k fixture at 1000 and 3000 px/s
holds 280 Hz at p50 with 1–4 missed vsyncs in 6 s (sync p95 ≤ 0.4 ms); idle private working set 68.6 MB (heap +4 MB
for the documents). Not done yet: selection (across rows), hover / link clicks, KaTeX (MicroTeX), code highlighting
(KSyntaxHighlighting), the CodeBlock header, emoji vertical metrics on lines that grow (checked by eye only).

`decibell-qt --fixture qt-client/assets/fixture-features --capture out.png` renders a hand-written fixture with every
feature on one screen; `src/crash_trace.cpp` prints a symbolised stack on abort / crash (build RelWithDebInfo).

## 2d. Wheel scrolling (`src/ui/smoothwheel.*`)

Measured on the Electron client with real `WM_MOUSEWHEEL` messages posted to its window (`tools/perf/electron-wheel.mjs`;
**CDP-injected wheel events skip Chromium's smooth scrolling and jump**, and an occluded Electron window runs no
requestAnimationFrame, so keep it topmost while measuring): a notch scrolls **100 px over ~175 ms on a smoothstep
curve**; a notch mid-animation adds 100 px and restarts from the current position keeping the speed. `SmoothWheel`
does exactly that (a cubic Hermite from (position, velocity) to (target, 0)), on wall time, ticked once per frame by a
`QVariantAnimation`: Qt's animation driver steps one assumed vsync per rendered frame, so its own clock lags whenever
frames come slower (a 175 ms notch took 196 ms). Touchpads pass through unanimated. Measured: a notch matches
smoothstep within ~1 px at every sample; three quick notches tick at 3.6 ms (Electron 6–8 ms). Open: the first gesture
after idle ticks at ~12 ms (clocks ramping up from deep idle; Electron ~6.5 ms) — for the owner's feel review.
**Keys** (`SmoothWheel::handleKey`, on the message list once it has focus: a press in it gives it focus) scroll on the
same curve with Blink's steps: arrows 40 px, PageUp / PageDown and Space / Shift+Space a page = max(87.5 % of the
view, the view − 40 px), Home / End to the ends. The renderer has no key handling of its own here (Chromium's
default for a focused scroller); the composer keeps its keys while it has focus.

## 2e½. History paging and new messages

The message list runs bottom-to-top over a newest-first model, so older history is an *append* at the model's end
(above the rows on screen) and a new message is an insert at row 0 (the bottom). With `--page-size N` the model
shows the newest N and pages N more in (`loadOlder`) once the top comes within 1000 px; `receiveNew` simulates an
arriving message. `--selftest history --page-size 100` measures it in the hidden window: an older page added while
reading 800 px up moved the row being read **0 px**; a new message while reading history moved it **0 px**; a new
message at the bottom of the conversation showed flush with the list's bottom (gap 0); scrolling near the top paged
the next 100 in by itself. No anchoring code: ListView keeps the rows on screen where they are. (Not yet: the "new
messages" bar and jump-to-present that the client shows while scrolled up.)

## 2f. Selection, copy, links (`src/ui/chatselection.*`)

`ChatSelection` sits on the message ListView and owns the selection (anchor / focus as row + text position), since
delegates come and go and are reused; each row's `MessageText` binds its range to `startFor` / `endFor`, re-read on
`version`. Press and drag selects through every row between (the list autoscrolls while the pointer is past an
edge, faster the further out), Shift+press extends, a click collapses it, a double click takes a word, Ctrl+C
copies (`StandardKey.Copy`): bodies as displayed (markup gone, emoji as their characters, code without its padding)
with a "name time" line before each message that starts a group. A click on an http(s) link opens it
(`Qt.openUrlExternally`, as `lib/openExternal.ts`); the pointer is a hand over links and an I-beam over text.
`--selftest selection [--capture f.png]` drives it with synthetic input in the hidden window and checks it (drag,
click, double click, copy, PageUp, End).

- **Gotchas**: `ListView.itemAt` with `verticalLayoutDirection: BottomToTop` answered row 0 for every point, so rows
  are found from the delegates' own geometry. `QSGTextNode`'s built-in selection painted the highlight a few
  characters off ([5, 22) drawn as [5, 17)), so `MessageText` draws its own rectangles (the line box high, under the
  text) and the selected text keeps its colour. A hidden window is never polished: the ListView hasn't placed its
  rows and bodies are laid out at width 0 until something renders it (the self-test grabs once first).
- **Found on the way, fixed: no QtQuick.Shapes.** After the scene graph is re-created, `Shape` items on Qt's curve
  renderer never draw again (Qt 6.12): on the second grab of a hidden window (each renders through a new QRhi)
  every icon, avatar gradient and status dot was gone, and a lost device (driver update, TDR) re-creates the scene
  graph the same way. Re-creating the Shape items didn't help (new ones don't draw either: the broken state is
  inside Qt), and the geometry renderer has no anti-aliasing without MSAA. Icons, avatar gradients and status dots
  are now `SvgImage` (`src/ui/svgimage.*`): the SVG rasterised once at the drawn device-pixel size (QSvgRenderer,
  as Chromium rasterises inline SVG), shared through the per-QRhi texture cache (`src/ui/texturecache.*`, also
  MessageText's). At 4× zoom they match the curve renderer's output; memory unchanged (−0.3 MB, one DLL fewer).
  `--selftest regrab --capture f.png` checks it (three renders through new devices).
- **Open**: the highlight colour is a stand-in (`#663390ff`) until a screenshot of the Electron client with a
  selection; in Chromium the sender name and time are selectable too (`select-text` covers the header), here only
  bodies; dragging after a double click doesn't extend by words; no triple click (paragraph).

## 2e. Watched streams (`src/video/`, QML module `Decibell.Video`, Windows)

- **Path**: FFmpeg (the vcpkg build the Electron addon links; `DECIBELL_FFMPEG_ROOT`, default
  `C:\dev\vcpkg\installed\x64-windows`; **not** `VCPKG_ROOT`, which vcvars64 points at Visual Studio's own vcpkg)
  decodes with D3D11VA on **the window's own device** (`gpu_device.cpp` creates it with `VIDEO_SUPPORT`), so pictures
  stay on the GPU. At each scene-graph sync the newest picture goes through the D3D11 video processor (NV12 → RGBA and
  the scale in one pass) into a texture the size the item is drawn at, never above the video's size (Electron's canvas
  backing-store rule), wrapped with `QSGD3D11Texture::fromNative` (Qt wraps native textures as **RGBA8**, so the
  processor writes RGBA, not BGRA) and drawn by a plain image node. FFmpeg's decode submissions take the device's
  `ID3D11Multithread` lock (`AVD3D11VADeviceContext.lock`), as does the blit.
- **Threads**: per stream a decoder thread (`decibell-video-decode`) behind a packet queue; in the spike a
  `spike-stream-source` thread stands in for the core's receive thread (reads a `.dbv` file, feeds at the stream's
  rate on a high-resolution waitable timer). The decoder publishes the newest picture; one the renderer never took
  is `replaced` (what a canvas does silently).
- **Test streams**: `tools/streamgen` encodes synthetic streams with the streamer's NVENC settings
  (`bitrate_preset.rs`: p4, ull, CBR, no B-frames, zero latency, forced IDR) into `.dbv` (one frame per record, the
  frame number as a barcode band at the top). Built against the same vcpkg FFmpeg; no outside download.
- **Run**: `decibell-qt --video a.dbv[,b.dbv] [--bench-video <s>] [--maximized]`.
- **Comparison** (`tools/perf/video-compare.ps1`): the same files in the Qt client and in `tools/perf/electron-video`
  (the client's Electron + switches, StreamVideoPlayer's WebCodecs + canvas path, frames over IPC from main), each run
  measured by the app itself, by `measure.ps1`, and by `tools/perf/framecount`, which reads the barcode off the
  screen through Desktop Duplication: frames actually displayed, never displayed, and their spacing. **The apps'
  own counters don't compare**: Electron counts `drawImage` calls, and Chromium shows only the newest canvas per
  frame. Node timers in Electron's main process tick at 15.6 ms (77 fps for a 120 fps stream), so the harness paces
  in the renderer and pulls each frame from main.
- **First numbers (2026-11-07, not a quiet machine: a game held ~85% of the GPU, Discord's clip recorder ran)**:
  1440p120 H.264, 1280 × 800 window: CPU **8–10 % of a core vs Electron 40–41 %**, private working set **43–56 MB vs
  160–177 MB** (Electron here is a bare harness page, not the client). Every 1440p120 / 2160p120 file decodes all 120
  frames/s on both. Frames displayed: both clients fell short under the game's load (Qt 100–111/s, Electron 84–120/s,
  run to run); not a verdict until a quiet session. Runs after the first pair of the AV1 / 4K / two-stream set are
  void: harness windows left running were summed in (fixed: the harness quits, `video-compare.ps1` refuses to start
  over a leftover process).
- **Wakeups while playing 120 fps**: GUI thread ~760/s (≈ 6 per video frame), render thread ~270/s, decoder ~230/s.
  The GUI thread handles only ~3 events per frame (the decoder's update request, the update request, a dispatcher
  wake); the rest is the threaded render loop's GUI ↔ render-thread handoff. The basic render loop was tried: worse
  (11.2 % CPU vs 7.7 %, more frames replaced). Open, see §4.
- **The renderer waits in `beginFrame`** (p95 11–15 ms) for the swap chain under the GPU load, and replaces 12–17 %
  of the decoded frames. `QT_D3D_MAX_FRAME_LATENCY=0` moves the wait into Present, `1` is much worse, `3` is about
  the same as the default. Re-measure on a quiet machine before tuning.

## 3. Measuring

`tools/perf/` (see its README): `measure.ps1` samples a client's whole process tree (CPU, memory, context
switches as the wakeup proxy, GPU engines and memory); `startup.ps1` times launch → visible → settled. The
Electron baseline lives in the design doc §2; every phase reports against it in its review-log entry.

## 4. Open items

- **Variable-font weights**: in the gallery, JetBrains Mono at 600 (console themes' active channel, section
  labels) looks heavier than Chromium's. Check whether DirectWrite picks the variable font's 600 instance or
  synthesises bold from 400; if the latter, set the `wght` axis explicitly with `font.variableAxes`.
- **Spike (a) next**: the quiet-session comparison (nothing else using the GPU; the owner says when):
  `tools\perf\video-compare.ps1 -Streams <file> -Runs 3 -Seconds 10 -Resources` for 1440p120 H.264 / HEVC / AV1,
  2160p120 HEVC, two 1440p120 streams, and 1440p60, windowed and `-Maximized`. Then: the real-stream dump switch in
  native (debug only) once the owner streams; Linux (VAAPI / Vulkan Video) in the dual-boot session.
- **Video wakeups**: the GUI thread wakes ~6 times per video frame through the threaded loop's sync. Options if it
  matters after the quiet measurement: a video layer outside the scene graph (DirectComposition visual / child
  swap chain the decoder presents itself), or a custom render loop. Both cost overlay drawing on top of video.
- **Spike (b) next**: the selection colour and header selection (§2f open); Matinee captures against Electron's
  harness.

- The design's decisions D1–D11 are settled (owner accepted every recommendation, 2026-11-07). Next: the spike
  (design doc §12) on the `qt-spike` branch.
- Baseline still to measure: watching one and two 1440p streams at 60 and 120 fps; the owner streams later from
  another machine. The busy-channel baseline uses DailyGaming #general (the only community running).
- **Linux runs on the same machine, dual-booted** (i7-12700K, RTX 4080): the spike's Linux half happens in a
  session booted into Linux, from what the branch carries. Only NVIDIA hardware is on hand; AMD / Intel decode
  paths are live tests for someone with such a GPU.
- To drive the Electron client for a measurement without clicking (e.g. open a channel), start it with
  `--remote-debugging-port=<port>` and click through CDP; target text channels by `button[data-channel-id]`
  (voice rows don't carry it, so a script can't join voice by accident).
