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
