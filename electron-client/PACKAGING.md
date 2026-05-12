# Packaging

## Local build

From `electron-client/`:

```sh
npm install
npm run package
```

Output lands in `release/`:

| Platform | Artifacts |
|----------|-----------|
| Linux    | `Decibell-<v>-x86_64.AppImage`, `Decibell-<v>-amd64.deb` |
| Windows  | `Decibell-<v>-x64.exe` (NSIS installer) |
| macOS    | `Decibell-<v>-{x64,arm64}.dmg` |

`npm run package` runs `build:native` (cargo via napi-rs) +
`build:renderer` (Vite) + `build:tsc` (main process) + electron-builder.

### Cross-platform note

Each artifact must be built on its own OS. The native addon is a
napi-rs `.node` file that links system libraries (PipeWire on
Linux, WASAPI/D3D on Windows, AVFoundation on macOS) — there is no
cross-compile path. CI runs three matrix entries; locally you'll
need a VM/runner for any non-host target.

### Linux system deps

```sh
sudo apt-get install -y \
  libpipewire-0.3-dev libspa-0.2-dev libdbus-1-dev \
  libwayland-dev libegl-dev libgl-dev libasound2-dev \
  libopus-dev libclang-dev pkg-config fakeroot
```

### Windows system deps

MSVC + Windows SDK (Visual Studio 2022 "Desktop dev with C++"
workload). `audiopus` vendors libopus; cpal uses the bundled
`windows` crate.

**FFmpeg via vcpkg** is also required: the native addon links
dynamically against `avcodec` / `avutil` / `avformat` / `swscale` /
`swresample` for the Windows HW encoder pipeline (NVENC / AMF /
QSV). On a fresh dev machine:

```powershell
git clone https://github.com/microsoft/vcpkg C:\dev\vcpkg
C:\dev\vcpkg\bootstrap-vcpkg.bat
C:\dev\vcpkg\vcpkg.exe install "ffmpeg[nvcodec,amf,qsv]:x64-windows"
$env:VCPKG_ROOT = "C:\dev\vcpkg"
```

Then in any dev shell:

```powershell
$env:VCPKG_ROOT = "C:\dev\vcpkg"
$env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"  # audiopus_sys CMake-4 shim
npm run dev
```

The `build:native` post-step (`copy-dlls.cjs`) copies the FFmpeg
DLLs out of `$env:VCPKG_ROOT\installed\x64-windows\bin\` into
`electron-client/native/`, where both `npm run dev` and the
packaged build pick them up. `electron-builder.yml` ships them
into the installer via `files: native/*.dll` + `asarUnpack`.

Why dynamic + LGPL: shipping FFmpeg statically forces the binary's
license to GPL because libx264/libx265 (the software fallbacks
inside FFmpeg's GPL build) are GPLv2. We deliberately install
FFmpeg with only the hardware encoders (`nvcodec,amf,qsv`),
keeping the binary LGPL-clean. `THIRD_PARTY_LICENSES.md` carries
the LGPL text + redistribution acknowledgement.

### macOS system deps

Xcode Command Line Tools (`xcode-select --install`).

## Castlabs Electron note

`package.json` pins
`electron: github:castlabs/electron-releases#v33.4.11+wvcus` —
the Widevine-enabled fork required for DRM playback. electron-builder's
default mirror points at upstream `electron/electron`, which 404s on
the `+wvcus` tag, so `electron-builder.yml` overrides
`electronDownload.mirror` to the Castlabs releases URL. Don't remove
that override.

## Arch Linux (AUR)

The AUR package lives at `aur/PKGBUILD` and is named **`decibell-bin`**.
It is a thin binary wrapper: it downloads the `.pacman` artifact that
electron-builder produced in CI for the matching `ev<version>` tag and
extracts it into the install root (dropping the upstream pacman
metadata so makepkg writes its own).

The previous source-build PKGBUILD (Tauri-era `decibell`) is retired —
building the Castlabs Widevine fork on user machines is too heavy to
reasonably ship as an AUR source build.

### Publishing a new AUR release

After a tag is pushed and CI has uploaded the `.pacman` to the GitHub
release:

1. Bump `pkgver` in `aur/PKGBUILD` to match the release version
2. Regenerate `.SRCINFO`:
   ```sh
   cd aur && makepkg --printsrcinfo > .SRCINFO
   ```
3. Push the two files to the AUR git remote:
   ```sh
   git -C aur add PKGBUILD .SRCINFO
   git -C aur commit -m "decibell-bin <version>"
   git -C aur push aur master
   ```
   (assumes `aur` is configured as a separate worktree pointing at
   `ssh://aur@aur.archlinux.org/decibell-bin.git`)

### Local install (no AUR helper)

The `.pacman` from a GitHub release can be installed directly:

```sh
sudo pacman -U Decibell-<version>-x64.pacman
```

It registers in pacman's local db as `decibell-electron-client` (the
underlying npm package name leaks into fpm's metadata; the AUR PKGBUILD
overrides this to `decibell-bin` so AUR users see the proper name).

## CI

`.github/workflows/electron-release.yml` builds all three platforms
on tag push:

```sh
git tag ev0.1.0
git push origin ev0.1.0
```

The `ev*` tag namespace is intentionally separate from the
tauri-client's `v*` tags so the two release pipelines don't
collide. Artifacts attach automatically to a GitHub Release named
after the tag.
