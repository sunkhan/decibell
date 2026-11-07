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

Run with `C:\Qt\6.12.0\msvc2022_64\bin` on PATH (or after `windeployqt`).

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
measures the core alone). Levers already visible: decoded pictures kept in CPU memory after upload (Qt's pixmap cache;
a texture-only image provider would drop them), and the fixture-era `QString` copies of every row (the real model keeps
a ~150-row window).

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

## 3. Measuring

`tools/perf/` (see its README): `measure.ps1` samples a client's whole process tree (CPU, memory, context
switches as the wakeup proxy, GPU engines and memory); `startup.ps1` times launch → visible → settled. The
Electron baseline lives in the design doc §2; every phase reports against it in its review-log entry.

## 4. Open items

- **Variable-font weights**: in the gallery, JetBrains Mono at 600 (console themes' active channel, section
  labels) looks heavier than Chromium's. Check whether DirectWrite picks the variable font's 600 instance or
  synthesises bold from 400; if the latter, set the `wght` axis explicitly with `font.variableAxes`.
- **Spike next**: (a) the stream dump switch in native (debug only) + the D3D11 decode → `VideoItem` path,
  developed against a synthetic NVENC bitstream until the owner streams; (b) the chat panel (fixture generator,
  `MessageText`, the list, the member list) in Nocturne and Matinee, captured against Electron's harness.

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
