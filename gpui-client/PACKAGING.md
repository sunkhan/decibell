# Packaging the native client

How the GPUI client becomes something that ships: the lean **ship build**, the **Linux packages**
(tarball, AppImage, `.deb`, the AUR recipe), the **Windows installer**, the **release workflow**, the
**updater** and **crash reports**. Each mirrors what the Electron client does
(`electron-client/PACKAGING.md`, `electron/main/{update,sentry}.ts`) unless a section says otherwise.
Measurements are in §9; what only the owner can decide or do is in §11.

## 1. Build flavours

| Flavour | Command (from `gpui-client/`) | What is in it |
|---|---|---|
| **development** (default) | `cargo build --release -p decibell-app` | every screen (`--screen`), self-test (`--selftest`), fixture, the scroll benchmark, the release crate's test hooks (`DECIBELL_TEST_*`) |
| development + local stack | `… --features dev-local` | the above, plus `DECIBELL_CENTRAL`, `--login`, the stdout markers and the control pipe (never shipped) |
| capture tool | `cargo run -p decibell-app --features capture --bin decibell-capture` | the above, plus GPUI's test platform |
| **ship** | `cargo build --release -p decibell-app --no-default-features --features ship` | none of that: the app, the updater, crash reports |

`decibell-app/src/lib.rs` refuses a mix at compile time (`ship` with `dev`, `dev-local` or `capture`; `ship`
in a debug build; `ship` when the model's `local-central` is on — `decibell_model::core::DEVELOPMENT_BUILD`).
`decibell-harness`'s `ship` feature compiles the self-tests' probes out (their `DECIBELL_TEST_PROBES`
switch is gone from the binary). The ship build:

- never honours `DECIBELL_CENTRAL`: the core's `local-central` is off (the assertion above), and
  `packaging/check-ship.sh` refuses a binary in which the string occurs at all, with the other
  development strings (`DECIBELL_TEST_`, `DECIBELL_DEV_*`, the stack's password, self-test names);
- keeps the at-rest key in the OS keychain (`decibell_app::app::local_key_provider`: `FileKey` only in
  development or debug builds);
- reads the baked resources beside the executable (`resources/install.json`, `sentry.json`,
  `gifs.json`: §2), as Electron reads `process.resourcesPath`;
- takes `--theme`, `--perf`, `--hotkey=`, `decibell://` links, `--version`, `-h` (and the updater's
  hidden `--update-trial=`, `--apply-update`, the installer's `--create-shortcut`); anything else is an
  unknown argument.

Releases build it with `CARGO_PROFILE_RELEASE_STRIP=false`: the packaging step splits the symbols off
into `decibell-<v>-linux-x86_64.debug` (kept as a workflow / release asset for symbolicating crash
reports) and strips the shipped binary.

**The one version** is `electron-client/package.json`'s (`decibell-release`'s `build.rs` reads it into
`decibell_release::VERSION`; also `DEP_DECIBELL_RELEASE_VERSION` for the app's Windows resources). About,
the device list (`CoreOptions::app_version`), the updater, crash reports, `--version`, the Windows
version resource, the packages and their file names all use it. A native release is tagged
`native-v<that version>` (§5).

## 2. What a package contains (Linux)

Every package holds the same program directory: `decibell` (stripped), `resources/install.json`
(`{"kind": …, "version": …}`: the updater's mode and the crash reports' `install_kind` come from it),
`resources/sentry.json` / `gifs.json` when the release baked them, `licenses/THIRD_PARTY_LICENSES.md`,
plus the desktop entry `decibell.desktop` (`MimeType=x-scheme-handler/decibell;`, `Exec=decibell %U`,
`StartupWMClass=decibell`) and the icon in the hicolor sizes 16–512 (`assets/linux/icons`, cut from
`electron-client/resources/icon.png` by `packaging/gen-icons.py`). All assets (fonts, icons, the Twemoji
pack, the emoji data, KaTeX fonts, sounds) are **inside the executable** (§9.3). The program finds
`resources/` beside its own executable (symlinks resolved), registers nothing (no URL handler, no
autostart: the package's desktop entry does that), writes only to its profile, and runs from a
read-only directory (checked: §10).

| Package | Built by | Layout | Updates |
|---|---|---|---|
| `decibell-<v>-linux-x86_64.tar.xz` | `packaging/linux/package.sh` | `decibell-<v>-linux-x86_64/{decibell, resources/, share/applications, share/icons, licenses/, README.txt}` | notify-only |
| `Decibell-<v>-x86_64.AppImage` | the same, with `appimagetool` and the type-2 runtime | AppDir: the program directory, `AppRun → decibell`, `decibell.desktop`, `decibell.png`, `.DirIcon`, `usr/share/{applications,icons}` | **self-update** (as Electron's AppImage) |
| `decibell-native_<v>_amd64.deb` | `packaging/linux/mkdeb.py` (no dpkg needed; `dpkg-deb --info` checks it in CI) | `/usr/lib/decibell/{decibell,resources}`, `/usr/bin/decibell →`, `/usr/share/{applications,icons/hicolor,doc/decibell-native}` | notify-only |
| AUR `decibell-native-bin` | `aur/decibell-native-bin/PKGBUILD` (consumes the tarball) | as the `.deb`, licences in `/usr/share/licenses/decibell-native-bin/`; `install.json` says `aur` | notify-only |

Electron ships AppImage, `.deb` and `.pacman` on Linux. Not built here: **`.pacman`** — Electron's is
electron-builder's fpm output, which its AUR package unpacks; this client's AUR recipe consumes the
tarball directly (makepkg builds the pacman package on the user's machine with correct metadata), and a
hand-assembled `.pkg.tar.zst` without makepkg would be a second, unchecked package format.

`.deb` dependencies (`mkdeb.py`): exactly the shared libraries `decibell` links (`check-ship.sh` keeps the
allow-list: libc, libgcc_s, libxkbcommon(-x11), libxcb(-xkb), libva(-drm), libdrm, libopus, libpipewire-0.3,
libasound) by Ubuntu 24.04 / Debian 13 names (with the pre-t64 names as alternatives), plus the Vulkan
loader. The binary is built on Ubuntu 24.04 and needs glibc ≥ 2.39 (as Electron's addon does) and
DT_RELR support (glibc ≥ 2.36; `build.rs` links with `-z pack-relative-relocs`, §9.2). The `.deb`
conflicts with `decibell-electron-client` (both install `/usr/bin/decibell` and handle `decibell://`).

## 3. Windows

**The installer** is NSIS, as Electron's (electron-builder generates its script and cannot package a native
executable, so `packaging/windows/decibell.nsi` is written by hand with the same behaviour): per user, no
administrator rights, `%LOCALAPPDATA%\Programs\Decibell Native` (Electron's install owns
`Programs\Decibell` while both ship), a Start menu shortcut, an "Apps & features" entry
(`HKCU\…\Uninstall\DecibellNative`), and the `decibell://` keys HANDOFF.md §6 lists, under
`HKCU\Software\Classes\decibell`: `(Default)` = `URL:Decibell`, `URL Protocol` = `""`,
`DefaultIcon\(Default)` = `"<dir>\decibell.exe",0`, `shell\open\command\(Default)` =
`"<dir>\decibell.exe" "%1"`. The app never writes the registry. The uninstaller removes exactly what was
installed (the protocol key only while it still points here) and leaves the profile, as Electron's does.
Electron ships no portable zip; neither does this.

`packaging/windows/package.ps1` stages `decibell.exe`, runs `check-ship.sh` on it, writes
`resources/install.json` (`nsis`), and **finds the DLLs by walking the PE import tables**
(`packaging/windows/dlls.py`, normal and delay-load imports, recursively) through vcpkg's
`installed\x64-windows\bin` — so the list is whatever the overlay's install tree really holds
(expected: FFmpeg's `avcodec`, `avformat`, `avutil`, `swscale`, `swresample`, `avfilter`, `avdevice`
DLLs, `dav1d.dll`, `SvtAv1Enc.dll` and anything they import) — and copies the Visual C++ runtime
(`vcruntime140*.dll`, `msvcp140.dll`) app-local from Visual Studio's redistributable folder. A DLL found
nowhere fails the build. The list is in the workflow's `dlls.txt` artifact.

`packaging/windows/test-install.ps1` (run by the workflow on the runner) installs silently into a
temporary folder, checks the files, every registry value above, the shortcut's AppUserModelID, runs
`decibell.exe --version` from the install (every imported DLL must load), uninstalls silently and checks
nothing is left.

**The executable's identity** (`crates/decibell-app/build.rs` + `build/winres.rs`): a Win32 `.res`
written by the build script itself — no resource compiler, nothing to install on the runner; link.exe /
lld-link take a `.res` as input (checked on Linux with `llvm-cvtres` + `llvm-readobj --coff-resources`,
and against `llvm-rc`'s output for the version block) — holding the app icon
(`electron-client/resources/icon.ico`, resource 1, which GPUI's `load_icon` loads for the window), the
version information (the one version) and an **application manifest** (`windows/decibell.manifest.xml`:
PerMonitorV2 DPI awareness, Common Controls 6, long paths, the segment heap). GPUI is built without its
own manifest (`default-features = false` drops its `windows-manifest` feature), so before this the window
was DPI-unaware (stretched above 100 % scaling) and GPUI's task dialogs had no Common Controls 6. The
ship build opens no console (`windows_subsystem = "windows"`; development builds keep stderr). At start
the process sets the AppUserModelID `com.decibell.native` (`decibell_release::APP_USER_MODEL_ID`), and
the Start menu shortcut carries the same id: NSIS cannot set that property, so the installer runs
`decibell.exe --create-shortcut <lnk>` (IShellLink + IPropertyStore, `decibell_release::win`), falling
back to a plain shortcut.

On X11 the window gets `_NET_WM_ICON` (GPUI's `WindowOptions::icon`, its only icon option and X11-only)
from the 64 px icon; Wayland and desktops on X11 take the icon from the desktop entry.

## 4. Licences

`packaging/licenses.py` writes `THIRD_PARTY_LICENSES.md` for the flavour and target being packaged:
every Rust crate linked (from `cargo metadata`, normal dependencies only) with its own licence files
(deduplicated: ~660 crates, ~1.1 MB of text on Linux), the C libraries (Linux: FFmpeg, dav1d, SVT-AV1 with
its AOM patent licence, the NVIDIA and Vulkan headers compiled into FFmpeg, from the prefix's
`share/licenses` and headers; Windows: every vcpkg port's `copyright`), the bundled assets (the fonts'
OFL texts, Twemoji's CC-BY 4.0 attribution, emoji-mart's MIT, KaTeX, Lucide, highlight.js). It fails on a
crate with no licence or a copyleft-only one (`self_cell` is `Apache-2.0 OR GPL-2.0`: used under Apache).

**FFmpeg is LGPL-2.1-or-later and statically linked on Linux.** LGPL-2.1 §6 asks, for a work linked with
the Library: (1) prominent notice that the Library is used and is LGPL, with a copy of the licence; (2) the
Library's copyright notice among any the program displays (it displays none); (3) a way for the user to
modify the Library and relink — (a) the Library's source and the work in relinkable form, (b) a shared
library, (c) a written offer for three years, or (d) equivalent access from the same place as the binary;
(4) no terms forbidding modification for own use or reverse engineering to debug it; (5) no further
restrictions. How the packages answer: (1) the generated file's FFmpeg section, installed with every
package; (3) Linux uses **(d)**: every release carries the exact `ffmpeg-<v>.tar.xz`, `dav1d-<v>.tar.bz2`
and `SVT-AV1-<v>.tar.gz` the binary was built from (SHA-256-pinned by `tools/build-ffmpeg.sh`, attached by
the workflow) and the script with the configure line, and the client's own source at the tag is in the same
public repository: rebuild FFmpeg from a modified tree into `vendor/ffmpeg`, then the ship build (§1).
Windows uses **(b)**: the FFmpeg DLLs sit beside `decibell.exe` and can be replaced. (4)–(5) are the
project licence's to honour — the repository has no `LICENSE` file (§11). SVT-AV1 (BSD-3-Clause-Clear +
AOM Patent License 1.0) and dav1d (BSD-2-Clause) need their notices reproduced: they are in the file.

## 5. The release workflow

`.github/workflows/gpui-release.yml` ("Native Client Release"):

- **`native-v<VER>` tag** (proposed pattern; `ev*` is Electron's, `v*` the dead Tauri one). Rule: `VER`
  must equal `electron-client/package.json`'s version at the tagged commit (the `version` job fails
  otherwise). Both clients share the version number; Electron `ev0.9.5` and native `native-v0.9.5` may be
  different commits.
- **push to `gpui-client`** touching the packaging (`paths:` the workflow, `gpui-client/packaging/**`, the
  release crate, the app's manifest / build script / Windows resources, `assets/linux/**`,
  `aur/decibell-native-bin/**`), or a manual run: the **dry run** — the same build and packaging, artifacts
  only. No release, no tag, no channel update, no secrets baked (the `publish` job's `if:` requires a tag
  push; the bake steps require `PUBLISH`).

Jobs: `version`; `linux` (Ubuntu 24.04: GPUI fetch + patches, the client's FFmpeg from
`tools/build-ffmpeg.sh` (cached with its sources), the assets (`tools/fetch-assets.mjs`), `cargo test -p
decibell-release`, the ship build, `package.sh` with appimagetool + the type-2 runtime, checks: tarball
contents, `dpkg-deb --info/--contents`, `desktop-file-validate`, the AppImage's `--version`, `install-size.sh`);
`windows` (the same vcpkg pin, overlay and binary cache key as the Windows Native Check's gpui-client job,
the ship build, `package.ps1`, `test-install.ps1`, `tools/perf/windows/install-size.ps1` on the staged
tree); `publish` (tags only): the feed (`packaging/feed.py`), signed when the `UPDATE_SIGNING_KEY` secret
exists (and refused when `UPDATE_PUBLIC_KEY` is set without it), `gh release create native-v<VER>
--latest=false`, the channel update, and the guard below. Artifacts: `gpui-linux` (tarball, AppImage,
`.deb`, `.debug`, the C sources, `build-ffmpeg.sh`), `gpui-windows` (installer, `dlls.txt`).

### Releases while both clients ship

The Electron client's updater (electron-updater 6.8.3, `provider: github`, `allowPrerelease` unset) asks
`https://github.com/sunkhan/decibell/releases/latest` for the release GitHub flags as **latest** and then
downloads `latest.yml` / `latest-linux.yml` / `latest-mac.yml` from it. **A native release that became
"latest" would break auto-update for every Electron install** (`ERR_UPDATER_CHANNEL_FILE_NOT_FOUND`,
"Cannot find latest.yml in the latest release artifacts") until the next Electron release. So:

- native releases are created with `--latest=false` (never the default) and are **not** pre-releases (they
  are real releases of this client; a pre-release flag would also hide them from people browsing);
- the channel release `native-latest` is a **pre-release** (it can never be "latest") holding only the feed;
- after publishing, the workflow reads `releases/latest` back and **fails loudly** if it names a `native-*`
  tag, printing the repair: `gh release edit <the newest ev* tag> --latest --repo sunkhan/decibell`;
- the native updater **never asks for `releases/latest`** and ignores Electron's tags and assets (§6).

## 6. Updates

`decibell-release` (`update.rs`, `feed.rs`, `apply.rs`, `http.rs`) + `decibell-app/src/updater.rs`:
`update.ts` state for state and string for string, through the model's `UpdateStore` (Settings → About,
the user panel's chip).

**Modes** (`detectMode`): **disabled** in a development build or a ship binary outside a package
(Electron: `!app.isPackaged`; About: "Updates are disabled in development builds."); **self-update** for
the Windows installer's install and a Linux AppImage (`APPIMAGE` set by the AppImage runtime, its folder
writable); **notify-only** for every other package — the tarball, the `.deb`, the AUR package — exactly as
Electron's `.deb` / `.pacman` / AUR installs: "<v> is available." and "Open release page". (Electron's AUR
build reports notify-only, not disabled: `update.ts` returns `notify-only` for any packaged Linux install
without `APPIMAGE`.)

**Where the feed is**: one fixed URL that only native releases write, so no GitHub API, no rate limit, and
nothing an Electron release does can change it: `https://github.com/<repo>/releases/download/native-latest/decibell-native.json`
(+ `.sig`). `<repo>` and the channel tag are baked (`DECIBELL_UPDATE_REPO`, default `sunkhan/decibell`;
`DECIBELL_UPDATE_CHANNEL`, default `native-latest`). Until the first native release the URL answers 404,
which reads as "You're up to date", not an error. Alternatives considered: `releases/latest` (Electron's —
impossible while both ship); the releases API filtered by tag (60 requests / hour per address
unauthenticated, shared behind NAT); the Atom feed filtered by tag (only the newest ten releases: ten
Electron releases in a row would hide the native one).

**Trust**: the feed and the downloads come over HTTPS (rustls, Mozilla's roots) from `github.com` and the
two hosts its downloads redirect to (`objects.githubusercontent.com`, `release-assets.githubusercontent.com`);
the client contacts no other host, follows redirects only among them, and the feed's download URLs must be
on `github.com/<repo>/releases/download/`. electron-updater trusts exactly HTTPS + the SHA-512 in its
`latest*.yml`. This client adds: **with a public key baked in** (`DECIBELL_UPDATE_PUBLIC_KEY` at build time,
the `UPDATE_PUBLIC_KEY` repository variable), the feed's Ed25519 signature must verify or nothing is
offered; the download must match the feed's size and **SHA-512** before it is staged, and again before it
is applied; versions only go up; a version that failed to start is never offered again. Without a key, the
check is HTTPS + SHA-512 (Electron's level).

### Signing key (the owner's commands)

Make the key once, offline (never in the repository, never on CI unless you choose so):

```sh
openssl genpkey -algorithm ed25519 -out decibell-update.pem
openssl pkey -in decibell-update.pem -pubout -outform DER | tail -c 32 | base64   # → UPDATE_PUBLIC_KEY
```

Put the second line's output in the repository **variable** `UPDATE_PUBLIC_KEY` (it is public); from the
next release on the binaries refuse unsigned feeds. Either store the PEM as the **secret**
`UPDATE_SIGNING_KEY` (the publish job signs), or sign by hand before the channel is updated:

```sh
openssl pkeyutl -sign -inkey decibell-update.pem -rawin -in decibell-native.json | base64 -w0 > decibell-native.json.sig
```

(`packaging/tests/release_e2e.py` uses these exact commands against a test key.)

**When**: once, 5 s after the main window's first frame (`kickoffInitialCheck`), and on Check now / Try
again — one GPUI one-shot timer; `RECHECK_EVERY` re-arms it after each check (`None`: Electron never
re-checks; §11). A check while one runs is ignored; a disabled build never checks. The check runs on a
thread (`decibell-update`) that exists only while it runs.

**Download**: to `<profile>/update/<file>.partial`, resumed with `Range: bytes=<n>-` (a `206` whose
`Content-Range` starts there; anything else restarts) up to three times in one check, and at the next
check after that; a partial download of another release is discarded. Verified (size, SHA-512), renamed,
recorded in `staged.json`. A later check finds it staged and downloads nothing (electron-updater's cache).

**Apply, with a trial start and rollback** (`apply.rs`). The **old** version — known to work — applies a
staged update at its next start, before any window (electron-updater installs at quit:
`autoInstallOnAppQuit`), or at once with Restart (About's button, the chip), which starts the old version
again with `--apply-update` and quits. It gives up the profile, puts the new version in place keeping the
old (AppImage: `<name>` → `.<name>.previous`, the download → `<name>`; Windows: runs the staged installer
`/S /UPDATE /D=<install dir>`, which moves the install into `previous\` — a running image may be renamed —
and installs), starts the new version with `--update-trial=<pipe>` and the start's own arguments, and waits
up to 60 s for `ok`, which the new version writes once its first frame is on screen. `ok`: the old exits
(the backup is removed; on Windows by the new version at its next start). No `ok` (it exited, crashed or
hung — killed): the old files go back, the version is recorded in `failed.json` and never offered again,
and the old version carries on starting; About then says "Couldn't check: version <v> didn't start
(<why>), so <old> was kept". The wait is one blocking pipe read: no polling.

## 7. Crash reports

`decibell-release/src/crash/` + `decibell-app/src/crash.rs`: `sentry.ts` and `lib/sentry.ts` without a
Sentry SDK.

**When** (Electron's three gates, its log lines): a packaged ship build ("[sentry] disabled (dev mode)"
otherwise), the Privacy toggle on (`crash_reporting_enabled`, default on, read from `config.json` at start:
a change applies at the next start, as Electron), a DSN baked (`resources/sentry.json`, from the
`SENTRY_DSN` secret). Otherwise nothing is installed — no hook, no handler, no socket ever. The install id
(`crash_reporting_install_id`, a random UUID) is generated and written into `config.json` like
`loadSentryBootConfig`, keeping every other field.

**What**: a **panic** — the ship profile aborts on panic, so a panic hook writes the envelope to
`<profile>/crash-reports/<event id>.envelope` first, then sends it within **2 s** (Electron's flush
timeout, `@sentry/node`'s `shutdownTimeout`) before the process aborts; what couldn't be sent goes at the
next start (at most 10 kept, 30 days). A **native crash** (SIGSEGV / SIGBUS / SIGILL / SIGFPE / SIGABRT; on
Windows an unhandled SEH exception — a fault in a driver, like the NVIDIA one at exit): an
async-signal-safe handler writes a record (signal, faulting address, instruction pointer, the
`/proc/self/maps` line naming the library it is in, the build's ids), puts the previous handler back and lets
the crash proceed as before (Rust's stack-overflow message, the core dump); the record is sent as an
envelope at the next start. There is no out-of-process handler (Electron's crashpad is a process awake for
the app's life) and no minidump: a native crash is reported with one frame (its instruction pointer) and
the module it is in.

**The envelope**: one event, `platform: native`, `level: fatal`, `release: decibell@<version>`,
`environment: production`, `user.id` = the install id, Electron's tags (`platform`, `arch`, `os_release`,
`is_packaged`) plus `install_kind`, `thread` and `panic_location`; the exception (type `panic` or the
signal, the scrubbed message, mechanism unhandled) with the instruction addresses of the panicking thread
(`backtrace::trace`, unresolved) and this executable's `debug_meta` (ELF build id as Sentry's debug id; PE
timestamp + size, PDB GUID + age when there is one), so the reports symbolicate once the release's `.debug`
file is uploaded (`sentry-cli debug-files upload decibell-<v>-linux-x86_64.debug`: §11). No session
tracking, no performance tracing, no breadcrumbs, no request, no `server_name`, no IP
(`user.ip_address` is never set; Sentry's "Prevent storing IP" should stay on as for Electron).

**Scrubbing** (`crash/scrub.rs`) before anything is written: Electron's `beforeSend` only deletes
`user.ip_address` (its SDK sends no default PII and rewrites stack paths to `app:///`); a native panic
message is free text, so it keeps its shape and loses its data — quoted text (`{:?}` of strings: where
message content leaks) → `"<redacted>"`; URLs, e-mail addresses, IPv4 / IPv6 addresses, JWTs and long
base64 / hex runs → placeholders; the home directory → `~`; the OS user's name → `<user>`; the signed-in
account's name and every joined community's (`decibell_app::crash::watch_names`) → `<name>`; absolute
paths that aren't source files → `<path>`; source paths from the crate on; at most 1,024 characters.
Electron's "No component available" filter is Chromium's and has no counterpart.

**Transport**: the same blocking HTTPS client as the updater, allowed only the DSN's host; `POST
/api/<project>/envelope/` with `X-Sentry-Auth`; a 4xx other than 429 drops the report, anything else keeps
it for the next start. Nothing at rest: the sender thread exists while a report goes out.

## 8. Tests

- `cargo test -p decibell-release`: 36 tests — versions, install stamps, the HTTP client (length, chunked,
  redirects, refused hosts, HTTPS with a test CA), the feed (signature with / without a key, tampering,
  another key, URL rules), the updater against a loopback fake GitHub (no update; available → downloading
  → downloaded → verified → staged; a tampered download refused, nothing left; a bad signature refused
  before anything is offered; resume after two cut connections with exact `Range`s; a download cut on every
  attempt resumes at the next check; notify-only; a failed version not offered again; one check at a time;
  none when disabled; **Electron's releases interleaved and `releases/latest` pointing at an `ev*` tag or a
  native one: never asked for**; no native release yet = no update), the apply supervisor with stand-in
  executables (starts → replaced; exits → rolled back, nothing left; hangs → killed, rolled back), scrubbing,
  envelopes, DSNs, the install id, and a real SIGSEGV in a child process leaving a record.
- `packaging/tests/release_e2e.py --binary <development release build>`: the same flows through the real
  binary (its updater states on stdout in development builds), the apply of a real binary on trial and the
  rollback of one that exits, and crash reports against a fake Sentry: one envelope per panic, scrubbed;
  none (and no connection) with the toggle off or no DSN; a segfault recorded and sent once at the next
  start.
- `packaging/tests/ship-isolated.sh <ship binary>`: the ship binary on a private D-Bus with a throwaway
  keyring and an `LD_PRELOAD` network logger (`netlog.c`) for 10 s.
- `packaging/check-ship.sh`: the binary-level checks every package runs first.

## 9. Measurements

See the tables below (this machine; filled by the packaging work, 2026-11-26).

### 9.1 Size by crate (ship build, unstripped symbols: `packaging/size-by-crate.py`)

### 9.2 Levers

### 9.3 Assets

### 9.4 The ship build against the targets

## 10. Read-only and isolated runs

Results of 2026-11-26/27 on this machine (KDE Plasma 6.8 Wayland, RTX 4080).

**The release tests** (`packaging/tests/release_e2e.py`, a development release build `--features dev-local`
built from this branch, which has the test hooks; windows open for a few seconds per updater / apply case):

```text
PASS updater: no update: Idle → Checking → NotAvailable { checked_at: … }
PASS updater: available → downloading → downloaded, staged and verified: 29 states, last Downloaded { version: "9.9.9" }
PASS updater: never asks for releases/latest or Electron's files: …/native-latest/decibell-native.json, ….sig, …/native-v9.9.9/Decibell-9.9.9-x86_64.AppImage
PASS updater: a tampered file is refused, nothing staged: Error { message: "the download doesn't match its checksum" }
PASS updater: a bad signature is refused (test key baked): Error { message: "the update feed's signature doesn't match" }
PASS updater: resumes after a cut connection: ranges [None, 'bytes=1000000-', 'bytes=2000000-']
PASS updater: a tarball / package-manager install only says it is available: Idle → Checking → Available { version: "9.9.9" }
PASS apply: the new version starts on trial, says so, and replaces the old file
PASS apply: a version that doesn't start is rolled back and remembered: Error { message: "version 9.9.9 didn't start (it exited before showing a window (exit status: 3)), so 0.9.4 was kept" }
PASS crash: Privacy toggle off → nothing sent, no connection
PASS crash: no DSN baked → nothing sent, no connection
PASS crash: a segfault leaves a record and dies as SIGSEGV: exit -11, 1 record(s)
PASS crash: the next start sends the native crash, once: SIGSEGV at 0x8 in decibell
```

The panic case sent exactly one envelope; its value read `cannot open <path> for perf1 with token <token> at <ip>:
"<redacted>"` with tags `platform linux, arch x64, os_release 7.3.6-1-cachyos, is_packaged true, install_kind
tarball, panic_location crates/decibell-release/src/crash/mod.rs:308, thread decibell-test-panic`: everything but
the account name was taken out, because the scrubber learns the account's and communities' names at sign-in
(`watch_names`) and this child panicked before signing in. The test now registers the names as a signed-in app
does (`DECIBELL_TEST_CRASH_NAMES`).

**A packaged tree run read-only** (`packaging/tests/readonly-run.sh`; the tarball's layout with the same
development `dev-local` build, since the ship build has neither self-tests nor `--login`; bubblewrap binds the
tree read-only): `--selftest boot` PASS (17 bundled faces resolve), then `--login perf1` against the local stack:
`decibell: signed-in perf1`, `decibell: connected Daily Gaming`; the tree is unchanged afterwards.

**The ship binary, isolated** (`packaging/tests/ship-isolated.sh`: no saved sign-in, a private D-Bus with a
throwaway gnome-keyring, an `LD_PRELOAD` logger of `socket` / `connect` / `sendto` / `sendmsg` /
`getaddrinfo`, 10 s at the sign-in page): first frame shown; `[sentry] disabled (dev mode)` and
`[update] mode=Disabled` (a ship binary outside a package); sockets: Unix only — Wayland, the X11 socket,
the private session bus, the system bus — **no inet socket, no connection, no name lookup**; the
throwaway keyring holds **one item, label `Decibell`, `url decibell://e2ee-local-key/<profile>`**: the at-rest
key is created at the first start, before any sign-in (§11, item 10); the profile holds only
`instance.lock`, `instance.sock` and `video-decoders.txt`.

## 11. Decisions and work for the owner

1. **The tag pattern and version rule**: `native-v<VER>`, `VER` = `electron-client/package.json`'s version at
   the tagged commit; the channel tag `native-latest` (created by the first publish, a pre-release). Say if
   another pattern is wanted (the workflow, `feed.py`'s release page and `DECIBELL_UPDATE_CHANNEL` change
   with it).
2. **The signing key** (§6): make it, set `UPDATE_PUBLIC_KEY` (variable) and either `UPDATE_SIGNING_KEY`
   (secret) or sign by hand. Until then feeds are unsigned and trusted as Electron trusts its own.
3. **Pin the AppImage tools**: the dry run prints `appimagetool sha256 …` and `runtime sha256 …`; put them in
   the variables `APPIMAGETOOL_SHA256` / `APPIMAGE_RUNTIME_SHA256` (and, to stop following GitHub's
   `continuous` builds, `APPIMAGETOOL_URL` / `APPIMAGE_RUNTIME_URL` at a versioned release). A tag run
   refuses to package without the hashes.
4. **Publishing**: push the first `native-v<VER>` tag when the client should reach users; the workflow
   creates the release (never "latest") and the channel. Nothing was published from this work.
5. **The project's licence**: the repository has no `LICENSE` file; `aur/PKGBUILD` (Electron) says MIT,
   `crates/decibell-core/Cargo.toml` says Proprietary. The LGPL obliges whoever ships the binary to allow
   modification for the user's own use and reverse engineering to debug it (§4): add the licence, and the
   packages will install it (`licenses/`, the `.deb`'s `copyright`, the PKGBUILD's `license=`).
6. **The AUR package**: `decibell-native-bin` (conflicts with `decibell-bin`), pushed by the owner as for
   `decibell-bin` (`makepkg --printsrcinfo | diff .SRCINFO -` passes); at the switch-over it could take over
   the name `decibell-bin`.
7. **Windows naming while both clients ship**: install folder and shortcut "Decibell Native",
   AppUserModelID `com.decibell.native`, uninstall key `DecibellNative` (`decibell.nsi`,
   `APP_USER_MODEL_ID`). At the switch-over they can become Electron's (`Programs\Decibell`,
   `com.decibell.app`), which also moves existing pins. **Code signing**: neither client is signed;
   SmartScreen warns on a new unsigned installer. An Authenticode certificate would be applied after
   `makensis` (`signtool sign` on `decibell.exe` before packing and on the installer).
8. **How often to check**: Electron checks once per start and on "Check now"; so does this client
   (`RECHECK_EVERY = None`). A long-running native client never hears of an update otherwise; set e.g. 6 h.
9. **Crash report symbolication**: upload each release's `decibell-<v>-linux-x86_64.debug` (a release /
   workflow asset) with `sentry-cli debug-files upload` (needs `SENTRY_AUTH_TOKEN`, as Electron's source
   map step); keep Sentry's "Prevent storing IP" on. Windows builds write no PDB (`debug = false`); for
   symbolicated Windows reports build with `debug = "line-tables-only"` and upload the `.pdb`.
10. **The at-rest key is created at the first start** (before any sign-in), in the OS keychain, by both
    clients (`decibell_model::core::start` → `KeychainKey::load`; Electron's `loadOrCreateE2eeLocalKey`).
    Measured with the ship build on a private keyring (§10): one item "Decibell" after 10 s at the
    sign-in page. Say if it should wait for the first sign-in.
11. **Moving Electron users to the native client** (not decided here). Electron's updater can only install
    what its `latest*.yml` names, from the release flagged latest. Options:
    - **A last Electron release that points the way**: an `ev*` release whose app shows "Decibell is now a
      native app" with a link (or, on Windows / AppImage, downloads the native installer / AppImage and
      starts it). Needs Electron UI work and one more Electron release; afterwards native releases may
      become latest (drop `--latest=false` and the guard).
    - **Let Electron's updater install the native client**: a release flagged latest whose `latest.yml` names
      the native NSIS installer (electron-updater runs NSIS installers with `/S`, which this one accepts) and
      whose `latest-linux.yml` names the native AppImage (electron-updater replaces the AppImage file).
      Automatic for Windows and AppImage users; `.deb` / `.pacman` / AUR users only see "available". Needs
      careful testing (electron-updater's arguments, the Electron install left behind on Windows, data
      migration from Electron's profile — its own `userData` folder — to this client's `~/.config/decibell`).
    - **Side by side for a while** (what the workflow does now): both clients release under their own
      tags, Electron's stay "latest", users switch by hand; Electron's last release says so.
    In every case: what happens to `releases/latest` (Electron's until its last release), the AUR names
    (`decibell-bin` → native?), and the profile (the native client starts with a fresh one).
12. **Size levers left on the table** (§9): the items listed there under "could go".

## 12. macOS

The Electron release has a macOS job (`macos-15`, Apple silicon `.dmg`); this client has none. GPUI runs on
macOS (`gpui_macos`, Metal), but this client's video, capture, desktop services and packaging have no macOS
side. What a macOS build would need:

- **Video**: decode through VideoToolbox (FFmpeg's `videotoolbox` hwaccel or `VTDecompressionSession`)
  into `CVPixelBuffer`s, drawn with GPUI's own macOS surface path (`Window::paint_surface` takes a
  `CVPixelBuffer` at the pin: the Metal renderer composites it, no copy); `decibell-video`'s Vulkan Video /
  NVDEC / VA-API paths and the wgpu external-surface patches (`0001`, `0005`–`0007`) don't apply. Software
  decode (dav1d / FFmpeg) as the fallback, as elsewhere.
- **Encode and capture** (the core): ScreenCaptureKit for the screen and its audio (Electron used
  `getDisplayMedia`); VideoToolbox encoders (`h264_videotoolbox`, `hevc_videotoolbox`) added to the
  encoder probe; the core's FFmpeg built for macOS (`tools/build-ffmpeg.sh` is Linux-only: a macOS variant
  with `--enable-videotoolbox`, static and LGPL, arm64 and x86_64 or universal).
- **Services** (`decibell-platform`): single instance (the Unix socket works as is), `decibell://` through
  the `open-url` Apple Event (GPUI's `on_open_urls`) and `CFBundleURLTypes`, press hotkeys as Electron's
  accelerators (`hotkeys.ts` macOS branch; the core has no macOS global-hotkey backend), attention (dock
  bounce), dock progress, idle (`CGEventSourceSecondsSinceLastEventType`), sleep / lock (`NSWorkspace`
  notifications), the keychain (GPUI's credentials API already uses the macOS Keychain), microphone and
  screen-recording permission prompts (TCC).
- **Packaging**: an `.app` bundle (`Info.plist`: identifier, `CFBundleURLTypes` for `decibell`,
  `NSMicrophoneUsageDescription`, `LSMinimumSystemVersion`; the icon from `electron-client/resources/icon.icns`;
  `resources/` inside `Contents/Resources`, which `install.rs` would look up there), a `.dmg`
  (`hdiutil`), a `macos-15` job in this workflow.
- **Updates**: replace the bundle (download a zip of the `.app`, verify, swap on restart with the same
  trial / rollback), a `macos-arm64-app` feed entry.
- **Signing and notarisation**: an Apple Developer ID (paid membership), `codesign` with the hardened
  runtime and entitlements (audio input, the JIT is not needed), `notarytool submit --wait` and `stapler`.
  Without them Gatekeeper refuses to open the app; Electron's `.dmg` is built unsigned today.
