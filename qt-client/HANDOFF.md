# Decibell Qt client — hand-off

The native desktop client for Windows and Linux: Qt 6 Quick (QML) over the shared Rust core. The Electron client
(`electron-client/`) keeps shipping until this one reaches parity and is the reference for every behaviour.

**Status (2026-11-07): design stage.** No client code yet. The design and its open decisions:
`docs/superpowers/specs/2026-11-07-qt-native-client-design.md` (read it first). The toolchain is set up and
proven on the owner's Windows machine (cxx-qt's minimal QML example builds and runs against Qt 6.12.0).

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
- **cxx-qt-cmake's upstream example fetches `GIT_TAG main`**; pin a tag.

## 3. Measuring

`tools/perf/` (see its README): `measure.ps1` samples a client's whole process tree (CPU, memory, context
switches as the wakeup proxy, GPU engines and memory); `startup.ps1` times launch → visible → settled. The
Electron baseline lives in the design doc §2; every phase reports against it in its review-log entry.

## 4. Open items

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
