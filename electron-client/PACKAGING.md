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
| Linux    | `Decibell-<v>-<arch>.AppImage`, `.deb`, `.pacman` |
| Windows  | `Decibell-<v>-x64.exe` (NSIS installer) |
| macOS    | `Decibell-<v>-<arch>.dmg` (CI builds on `macos-15`, Apple silicon) |

Plus `latest*.yml` + `*.blockmap` for electron-updater (the `publish` block
in `electron-builder.yml` exists only to make electron-builder emit them;
CI uploads everything itself with `--publish=never`).

`npm run package` runs `build:twemoji` + `build:native` (cargo via napi-rs) +
`build:renderer` (Vite) + `build:tsc` (main process) + electron-builder.

Release builds bake two optional files into `resources/` (gitignored):
`sentry.json` (`{"dsn":…}`, from the `SENTRY_DSN` secret; crash reports are
sent only by packaged builds) and `gifs.json` (`{"provider":…,"key":…}`, from
the `GIF_API_KEY` secret + `GIF_API_PROVIDER` variable, default `klipy`).
Write them by hand for a local package that should have those features.

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
  libopus-dev libclang-dev pkg-config fakeroot libarchive-tools \
  nasm libva-dev libdrm-dev
```

Plus `protoc` and CMake. No system FFmpeg: the native build runs
`native/scripts/ensure-ffmpeg-linux.cjs`, which builds a **static LGPL FFmpeg**
(`build-ffmpeg-linux.sh`: pinned FFmpeg + nv-codec-headers, NVENC / VAAPI
encoders only, no libx264) into the gitignored `native/vendor/ffmpeg` the
first time (~5–10 min; CI caches it). `native/.cargo/config.toml` points
`PKG_CONFIG_PATH` there. Static linking is what escapes Electron's bundled
`libffmpeg.so`, whose exported symbols otherwise shadow a system libavcodec.
`THIRD_PARTY_LICENSES.md` (this folder) carries the notice.

### Windows system deps

MSVC + Windows SDK (Visual Studio 2022 "Desktop dev with C++"
workload), CMake, `protoc`, LLVM / libclang (bindgen). `audiopus`
vendors libopus; cpal uses the bundled `windows` crate. Full dev setup:
`HANDOFF.md` §6.1.

**FFmpeg via vcpkg** is also required: the native addon links
dynamically against `avcodec` / `avutil` / `avformat` / `swscale` /
`swresample` for the Windows HW encoder pipeline (NVENC / AMF; the
port is installed with QSV too, but the probe leaves QSV out). Use
the **same vcpkg commit CI pins** (`e5a1490e…` in
`.github/workflows/electron-release.yml`; newer FFmpeg 8 ports broke
the d3d11va + NVENC path). On a fresh dev machine:

```powershell
git clone https://github.com/microsoft/vcpkg C:\dev\vcpkg
git -C C:\dev\vcpkg checkout e5a1490e1409d175932ef6014519e9ae149ddb7c
C:\dev\vcpkg\bootstrap-vcpkg.bat -disableMetrics
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

Xcode Command Line Tools (`xcode-select --install`), `protoc`, CMake.
macOS has no native capture / encode path (WebCodecs only) and no
stream-audio capture.

## Castlabs Electron note

`package.json` pins
`electron: github:castlabs/electron-releases#v33.4.11+wvcus` —
the castlabs fork, used because upstream Electron strips the
proprietary WebCodecs **encoders** (H.264 via OpenH264, platform HEVC)
that the renderer's encode path needs (it also carries Widevine; we
don't use DRM). electron-builder's
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

After a tag is pushed and the whole "Electron Release" run has finished
(the `release` job waits for all three platform builds, ~40 min; until
then `gh release view ev<VER>` says "release not found"):

1. Confirm the asset exists: `gh release view ev<VER> --json assets --jq
   '.assets[].name'` lists `Decibell-<VER>-x64.pacman` (and its download
   URL answers 200).
2. In the main repo, bump `pkgver` in `aur/PKGBUILD` and the `pkgver` +
   `source` URL in `aur/.SRCINFO` (checksums are `SKIP`); check
   `cd aur && makepkg --printsrcinfo | diff .SRCINFO -` prints nothing.
3. Commit to `main` (`aur: bump decibell-bin to <VER>`) and push.
4. Clone `ssh://aur@aur.archlinux.org/decibell-bin.git`, copy the two
   files in, commit `decibell-bin <VER>`, `git push origin master`. The
   push can hang in `git-receive-pack`: wrap it in `timeout 90`, retry,
   and confirm with `git ls-remote origin master`.

Git identity isn't configured globally on the dev box; pass
`-c user.name='sunkhan' -c user.email='gunhanserhat@gmail.com'`.

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
on tag push (bump `electron-client/package.json`, the only version
site, push, then):

```sh
git tag ev0.9.4
git push origin ev0.9.4
```

The `ev*` tag namespace is intentionally separate from the dead
tauri-client workflow's `v*` tags (`release.yml`; don't push `v*`
tags). Artifacts attach automatically to a GitHub Release named
after the tag. If the macOS job never gets a runner ("The job was
not acquired by Runner"), `gh run rerun <id> --failed` rebuilds only
it and the release job picks up the other platforms' artifacts.

`.github/workflows/win-native-check.yml` runs `cargo check
--all-targets` on windows-latest for every push touching
`electron-client/native/**` or the proto, and publishes the compiler
output to the orphan branch `ci/win-native-check-log` (`git fetch
origin ci/win-native-check-log && git show FETCH_HEAD:cargo-check.log`).
Let it go green before tagging when Windows code changed.
