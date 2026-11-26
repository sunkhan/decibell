# Decibell GPUI client — hand-off

The native desktop client: Rust + GPUI (Zed's UI framework) over the shared core `crates/decibell-core`.
The Electron client (`electron-client/`) keeps shipping until this one reaches parity and is the reference
for every behaviour. Owner decision 2026-11-26: GPUI, not Qt (`qt-client/` is the set-aside spike).

**Read first**: `CLAUDE.md` (every standing decision applies), the design record
`docs/superpowers/specs/2026-11-26-gpui-native-client-design.md`, then this file. Reference while working:
`gpui-client/docs/gpui-notes.md` (GPUI at the pin, with source lines), `gpui-client/docs/video-design.md`
(the stream path), `gpui-client/PARITY.md` (the checklist), `docs/FEATURES.md`, `ARCHITECTURE.md` §7.3,
`electron-client/HANDOFF.md` §5 (native gotchas apply here too).

## 0. The goal and its rules (owner's brief, 2026-11-26)

Full feature parity with the Electron client, as a faster, leaner client: far less RAM, low CPU, GPU, wakeups,
startup time and install size; every design choice is judged by those.

- Branch `gpui-client` (from `qt-spike`); push only that branch. Never push to `main`, tag, release or deploy.
  No wire or server changes.
- GPUI pinned (`gpui-client/GPUI_PIN`); read its source, don't trust memory; other Zed crates are mostly GPL:
  never copy from them.
- Electron defines behaviour: read a feature's renderer code before porting it. Protocol, media and crypto stay
  in `crates/decibell-core`.
- Test against the local stack and test accounts only (`tools/dev/`), never the owner's account or the live
  servers (never launch `/opt/Decibell` with its default profile).
- Resources: `tools/perf/measure.sh`; Electron measured on this machine first; the Qt design's §3 ratios hold in
  every phase and the numbers are logged here (§4). No polling timers, no frames rendered at idle, no per-frame
  allocation or copy on hot paths. A feature that breaks a target isn't done.
- **RAM and the GPU driver** (owner, 2026-11-26): keep the GPU; the driver's own memory (~60 MB of any Vulkan
  process on NVIDIA) doesn't fail the RAM target. Report total private memory first; if it is over, report the
  driver's share (a minimal Vulkan window, same tool) beside it and judge the row on what is ours.
- **Media quality first** (owner, 2026-11-26): no audio popping, top-notch audio and video; never pause or
  restart audio streams or processing to save wakeups; anything that could change audible output needs the
  owner's listening test (CLAUDE.md, "Audio and video quality outrank resource savings").
- **Codecs on every vendor** (owner, 2026-11-26): hardware encode and decode on NVIDIA, AMD and Intel on both
  OSes, integrated GPUs with a hardware codec included; software (dav1d / FFmpeg decode, SVT-AV1 encode) only
  when no hardware path works. Design record §7.0.
- Linux is finished in this session; Windows code must compile (Windows Native Check on the branch); what only
  the owner can do goes in §6 and is not waited on.
- A parity row is done only when it was run, with a self-test where possible.

Done when, printed with real command output: `PARITY.md` with every FEATURES §1–§9 row "done" (with evidence)
or "needs owner" (with what to test); a table of this client against Electron for every §3 target (idle RAM
first); Electron's bar passing (renderer typecheck, `npx napi build`, `cargo test --lib`, Windows Native Check
on the branch), `cargo test` for `decibell-core` and this client, the client's self-tests; captures of the main
screens in all seven themes beside Electron's.

## 1. Status

| Phase | State |
|---|---|
| Decision recorded (CLAUDE.md, design record, pin) | done (2026-11-26) |
| Core split step 1 (move + event sink) | done `76b77c3`: Electron's bar on Linux (tsc 0 errors, napi build with `index.d.ts` / `index.js` byte-identical, 322 + 2 tests, smoke load) and Windows Native Check run 45797141001 green |
| Core split step 2 (typed `CoreEvent`) | on the branch `1930905` + `ebb65d2`: 85 variants, `to_json()` pinned byte-for-byte by a golden test recorded from the old path; Linux bar passed (tsc 0, napi build with `index.d.ts` / `index.js` unchanged, 327 + 2 tests, smoke); Windows Native Check run 45806591240 green (after `34f6527` fixed a Windows-only test) |
| Core split step 3 + 4 (commands → `decibell_core::api`, `lifecycle::init`, runtime handle `rt`) | done `a67554d` + `79b7eba`: 132 functions in 16 areas returning `CoreError`, structs defined once in core (optional `napi` feature), every async api fn awaitable from any executor; Linux bar passed (tsc 0, napi build with `index.js` identical and `index.d.ts` identical in content, 340 core tests, `smoke-load.cjs`, `smoke-stack.cjs` signing in against the local stack); Windows Native Check run 45808677484 green |
| Client state model, foundation (`crates/decibell-model`, `docs/model.md`) | done `7b45b47`: core handle (2-worker runtime), one-wake-per-burst pump (10,000 events from 8 threads = 5 wakes), router over all 85 events, stores for auth / users / navigation / servers / communities / roster / permissions / settings, `decibell-headless`; 65 tests incl. stack tests |
| Local test stack (`tools/dev/`) | done `e245c8f`: central 127.0.0.1:18080 + Daily Gaming / Rustaceans / Design Lab on 127.0.0.11–13, 9 test accounts, 10k-message `#general`; `tools/dev/README.md` |
| Linux measurement tools | done `5285180`: `tools/perf/measure.sh`, `startup.sh`, `winctl.sh`, `install-size.sh` (README has the Linux section) |
| Electron baseline on this machine (Linux) | runner being built (`tools/perf/electron-local.sh`, `scenarios.sh`); the full run needs a quiet machine |
| GPUI survey + hello-world numbers | done `75e9008`: `docs/gpui-notes.md`; decisions in the design record §5 |
| Video research + prototype (`docs/video-design.md`) | in progress |
| Theme generator + assets | done `e846392`: `crates/decibell-theme` (162 tokens × 7 themes, 12 scaled, scopes), `tools/gen-theme`, `tools/fetch-assets.mjs`, `docs/css-component-rules.md` |
| Skeleton (workspace, patched GPUI copy, static font faces, window frame, theme gallery, capture + self-test harness, perf markers, Windows check job) | done `b120b2f`; `CONVENTIONS.md`; self-tests `boot`, `idle-frames`, `window-frame`, `themes` pass on Wayland and XWayland |
| Component kit (in `decibell-ui`) + Electron capture harness (`tools/electron-captures/`) | done `d1c99c9`: `ScrollAnimator` / `ScrollArea` (11 wheel vectors within 0.005 px), buttons, forms, `Select`, overlays, `SettingsWindow`, `SaveBar`, identity bits, 153 icons; 56 capture pairs against the real React components differ in ≤ 1.08 % of pixels; self-tests `kit-*` |
| Text engine (`crates/decibell-text`, GPUI patches 0010 / 0011) | done `d1c99c9`: line breaks identical to Chromium on 21,389 / 21,391 fixture paragraphs (heights identical where breaks agree), letter-spacing, synthesised italics (skew 0.25), inline Twemoji (2,879 SVGs packed in 2.6 MB), rich bodies with code / math slots, selection contract, fallback fonts loaded lazily; cold layout 16.6 µs median, cached paint 0 allocations |
| State model, both halves integrated | done `8ebaa28`: 176 unit tests, 16 stack tests; signed in with `#general` open and 100 users watched: 19.6 MB private, 0.17 wakeups/s; Windows check run 45833447742 green |
| Text inputs (`decibell_text::input`: `InputState`, `TextInput`, `TextArea`; the kit's `TextField` is a working field) + GPUI patch 0012 (physical key codes on key events) | done `5d634f1` + `b374268`: Chromium's caret / word stops and undo steps (51 of 52 recorded cases identical), atoms, IME protocol, password, byte caps; hotkeys recorder and focused fallback on physical codes, tested under xkb `tr` / `de`; self-tests `input-*`, `hotkeys-layout` |
| Message list and rows (`crates/decibell-chat`) | done `1b192c8`: one element, exact row heights, anchored by coordinates (0 px on a history page: `chat-history`), sliced measuring, selection across rows, Chromium wheel scrolling; 185 fixture rows equal Electron's heights to 1/64 px in four themes (console themes: 4 day-opening rows off by 1/32 px); scroll at 280 Hz under load: p50 3.57 / p99 4.07 ms, 2 missed of 1,680 (Electron 14); slots for attachments, embeds, editor, composer |
| Chat content (attachments, link / invite embeds, image viewer, Transfers panel) | in progress, worktree `.worktrees/chat-content` (`wip/chat-content`) |
| Composer, emoji / GIF pickers, inline editor (`decibell-composer`) | in progress, worktree `.worktrees/composer` (`wip/composer`) |
| DMs, friends, profile popup / modal, custom status, user menu (`decibell-home`) | in progress, worktree `.worktrees/home` (`wip/home`) |
| Server Settings, Channel Settings, picture editor (`decibell-server-settings`, `decibell-picture-editor`) | in progress, worktree `.worktrees/server-settings` (`wip/server-settings`) |
| Regional formats (`decibell_model::regional`: dates, times of day and counts follow the system's settings; English words) | done `1d2e9e1` (owner decision 2026-11-26 in CLAUDE.md): chat stamps, day dividers, member counts, ban expiry; 42 stamp cases equal the renderer's under en-US; self-test `regional` reads the drawn labels under four environments; Windows path compiled only. **Sweep each crate that merges later** with the grep in CONVENTIONS "Regional formats" |
| Video crate (`crates/decibell-video`, GPUI patches 0005 / 0006, `tools/build-ffmpeg.sh`, `tools/streamgen/`) | done `a7d5232`: Vulkan Video decode of H.264 / HEVC / AV1 on GPUI's device, software fallback, device-loss recovery, `VideoView`; self-tests `video-hw`, `video-idle`, `video-two`, `video-sw`, `video-recover`, `video-switch`; numbers in §4 "Stream playback". Not yet wired into the app's registry / boot on the branch (the lines are in the main tree's working files with the shell work) |
| Video, part 2: NVDEC and VAAPI hardware fallbacks, decoder selection by real probe, present-only fast path in the renderer | in progress, worktree `.worktrees/video2` (`wip/video2`) |
| Core: voice / media plane without polling | done `344068e`..`5840094` (Electron's bar passed on the merged tree: 419 core tests; Windows check run 45855500083 green): UDP receive loops block instead of polling at 1 ms, the video receive thread exists only while a stream is watched or sent, a talking peer allocates nothing per packet (PCM sample-identical in tests), `FrameBuf` keeps FFmpeg's padding, system idle as an event on Wayland. **No audio stream or processing is paused** (owner rule). In a voice channel alone and muted: 1,380 → 766 wakeups/s; what remains is the device clock and the 5 ms audio tick |
| Encoder quality (NVENC HEVC bitrate undershoot, per-codec tuning, colour signalling) | in progress, worktree `.worktrees/core-quality` (`wip/core-quality`) |
| Encoders on every vendor + software fallback (core) | done `4784c37`..`2e8bd1b`: backends NVENC / AMF / Quick Sync / VAAPI / software (SVT-AV1) behind a probe that really encodes, fallthrough at stream start, `DECIBELL_ENCODER` override, harness; ran here: NVENC (3 codecs) and software. **The software fallback is in the client's build** (`12c9115`, Windows `8a2a302`): `tools/build-ffmpeg.sh` builds SVT-AV1 4.2.0 into `vendor/ffmpeg` (rebuilt and swapped in on this machine; `scale_vaapi` too), `crates/decibell-app/build.rs` refuses an FFmpeg without `libsvtav1` and the software decoders, self-test `encoder-software` and `decibell-headless dump codecs` show it (`DECIBELL_ENCODER=software` → AV1 on `software`); Windows: vcpkg overlay ports `tools/vcpkg-overlay` (the pinned FFmpeg 8.0.1 + upstream's `svt-av1` feature + FFmpeg's own `libsvtav1.c` fix for SVT-AV1 4.x, SVT-AV1 4.2.0), installed by the Windows job's `gpui-client` step, **never built on Windows yet** (the first CI run is the check; checked on Linux: 8.0.1 + the fix + 4.2.0 passes the core's software tests); VAAPI / Quick Sync / AMF compile-checked with one-command owner checks (`crates/decibell-core/tools/verify-encoders.md`); Electron's bar passed (403 core tests) and Windows check run 45845969044 green; design `docs/superpowers/specs/2026-11-26-encoders-every-vendor-design.md` |
| Shell (`decibell-common`, `decibell-shell`, app boot with core + model) | done `f4ac4fe`: sign-in / register pages, server bar, channel sidebar with voice rosters, user panel, member list, friends frame, pane / overlay / menu registration (CONVENTIONS "Shell"); self-tests `shell-signin`, `-navigate`, `-members`, `-idle`, `-signout`, `-no-keyring`; captures within 0.3–1.2 % of Electron's; numbers in §4 "Shell". Left: channel drag-to-reorder, panes of the other features |
| Voice view, stream watching, mini-player, Go Live, calls (`decibell-voice`) + the stream bot for measurements | in progress, worktree `.worktrees/voice` (`wip/voice`) |
| Settings window (all tabs) + passphrase dialog (`decibell-settings`) | in progress, worktree `.worktrees/settings` (`wip/settings`) |
| Core follow-ups | done `2774316` (pooled frame buffers: 0 allocations per received video frame with the pool on, measured; Electron 1–2 instead of 4–8), `3ac2ebc` (`net::tls::exact_connector`), `502be85` (no `aws-lc-rs`), `9f0117e` (zbus runtime as a feature: `zbus-tokio` default for the addon, `zbus-async-io` for this client); Linux bar passed on the merged tree (tsc 0, napi build unchanged, 361 core tests, both smokes); Windows check running |
| Code highlighting (`crates/decibell-highlight`) | done `b636cb4`: highlight.js's engine + the renderer's 26 grammars, 0 mismatches against lowlight over 138,117 cases; +527 KB of binary; vendored patched `regress` (step budget) |
| Rich-text grammar (`crates/decibell-richtext`) | done `497ab53`: 0 mismatches against the renderer over 120,570 committed + 1.6M stress cases |
| Math (`crates/decibell-tex`) | done `aa0a020`: KaTeX 0.18.4 ported; markup byte-identical on 5,070 / 5,072 formulas, glyphs within 0.05 px of Chromium; +1.1 MB binary |
| Attachments (`crates/decibell-attachments`), link previews + GIF search (`crates/decibell-unfurl`) | done `08412dc`, `d835d32` (vectors from the real TS; stack tests) |
| Wheel / key scrolling spec | done `0777f91`: `docs/wheel-scroll.md` (120 px per notch, ease-in-out bezier, cc's duration + retarget rules, 11 vectors) |
| Stream playback: design + prototype | done `8c990eb`: `docs/video-design.md`, `patches/0001-gpui-external-surfaces.patch`; the real crate (`decibell-video`) not started |
| Stream playback (`crates/decibell-video`) | built, tested and measured on Linux (2026-11-26): Vulkan Video on GPUI's device, software fallback, `VideoHub` / `VideoView`, device-loss recovery, patches `0005` + `0006`, `tools/build-ffmpeg.sh`, `tools/streamgen/`; six self-tests (`video-*`); numbers in §4. Not wired to the model yet (the app calls `VideoDevice::install()` at boot; the frame sink is `VideoHub::push_frame`, feature `core`). Windows: compiles in principle, draws nothing until `gpui_windows` draws surfaces (`docs/video-design.md` §8) |
| Chat (list, rich text, selection, composer, wheel) | not started |
| Sign-in, servers, channels, DMs + E2EE, voice + MLS | not started |
| Remaining parity rows | not started |
| Packaging, auto-update, crash reports | not started |

## 1a. Worktrees in use (check `git worktree list`; merge or remove each when its work reports)

Parallel work that touches shared code runs in its own git worktree under `.worktrees/`, each on a `wip/*`
branch that is **never pushed**; finished work is cherry-picked or fast-forwarded onto `gpui-client` after it
is verified in a clean checkout (the branch head must always build: the Windows job checks it with `--locked`).
GPUI client worktrees need `gpui-client/vendor/{zed,ffmpeg}` linked from the main tree and their own
`vendor/gpui-src` (`tools/fetch-gpui.sh`); core worktrees need `electron-client/native/vendor`,
`electron-client/node_modules`, `electron-client/native/node_modules` and the generated `twemoji-data.json`
linked (all git-excluded through `.git/info/exclude`).

| Worktree / branch | Work |
|---|---|
| `.worktrees/core-quality` / `wip/core-quality` | encoder quality: HEVC undershoot, tuning, colour signalling (core) |
| `.worktrees/server-settings` / `wip/server-settings` | Server Settings and Channel Settings windows, the picture editor |
| `.worktrees/video2` / `wip/video2` | NVDEC / VAAPI decode, probe-based selection, renderer fast path |
| `.worktrees/voice` / `wip/voice` | voice view, streams UI, calls, stream bot |
| `.worktrees/settings` / `wip/settings` | settings window, passphrase dialog |
| `.worktrees/chat-content` / `wip/chat-content` | attachments, embeds, image viewer, Transfers panel |
| `.worktrees/composer` / `wip/composer` | composer, pickers, inline editor |
| `.worktrees/home` / `wip/home` | DMs, friends, profiles, user menu |

When landing work whose files overlap other agents' uncommitted edits in the main tree: build the commit in a
clean worktree, then move the branch with `git update-ref refs/heads/gpui-client <new> <old> && git reset -q`
(a mixed reset: it never touches working files).

## 2. Building and running

Everything below runs from `gpui-client/`. How code here is written: `CONVENTIONS.md`.

**Prerequisites** (Linux): Rust ≥ 1.100 (GPUI uses `cfg_select!`), git, a C toolchain, the development files
GPUI's Linux backends link (`libxcb`, `libxkbcommon`, `libxkbcommon-x11`, Wayland client) and a Vulkan driver.
Nothing is installed system-wide by the tools here. Windows: Rust stable (MSVC) and Git for Windows; a release
build also needs the Windows SDK's `fxc.exe` (`docs/gpui-notes.md` §11.1). Regenerating fonts needs a Python 3
with fontTools (`tools/gen-fonts.sh` finds or creates one); refetching assets needs Node ≥ 18.

```sh
tools/fetch-gpui.sh                     # once after cloning, and whenever GPUI_PIN or patches/ change
cargo build --release -p decibell-app   # → target/release/decibell (about 3 min clean, fat LTO)
cargo build -p decibell-app             # debug: our crates unoptimised, dependencies at opt-level 1
```

- **GPUI source**: `vendor/zed` is the pristine clone at `GPUI_PIN` (fetched by the script if missing),
  `vendor/gpui-src` a git worktree of it with `patches/*.patch` applied in order; cargo builds GPUI from there
  (`[workspace.dependencies]` in `Cargo.toml`). The script is a no-op while the pin and patches are unchanged
  and refuses to throw away edits that are in no patch. What each patch does and how to write one:
  `patches/README.md`. `Cargo.lock` started as Zed's own, so GPUI's dependencies are at the versions Zed ships.
- **FFmpeg**: the client links one static, LGPL FFmpeg 8.1 for decode (`crates/decibell-video`) and encode
  (`crates/decibell-core`), from `vendor/ffmpeg`; `.cargo/config.toml` points pkg-config at it.
  `tools/build-ffmpeg.sh` builds it (no sudo; under 2 minutes with 8 jobs; a no-op when the prefix is current):
  it fetches FFmpeg 8.1.3, dav1d 1.5.3, SVT-AV1 4.2.0, nasm, nv-codec-headers and the Vulkan headers, each
  checked against a pinned SHA-256, and fails if the configured tree is not LGPL-2.1+ or lacks a component the
  client opens by name. It needs a C / C++ toolchain, make, cmake, curl, python3, ninja and the libva + libdrm
  headers. In it: the hardware decoders (Vulkan, NVDEC, VAAPI hwaccels) and the software ones (H.264, HEVC,
  dav1d for AV1); the NVENC and VAAPI encoders with `scale_vaapi` (the VAAPI encoders' GPU conversion), and
  **SVT-AV1, the software encoder** of a machine without a working hardware one; licence files in
  `vendor/ffmpeg/share/licenses` (SVT-AV1: BSD-3-Clause-Clear + the AOM patent licence; a release adds them to
  `THIRD_PARTY_LICENSES.md`). The prefix is 79 MB (69 MB before SVT-AV1); the stripped release `decibell`
  grows by 9.5 MB (53.7 → 63.1 MB, measured 2026-11-26: `.text` +8.3 MB, SVT-AV1's encoder kernels) and links
  no new shared library. `crates/decibell-app/build.rs` refuses to build against an FFmpeg without `libsvtav1`
  and the software decoders; `decibell --selftest encoder-software` checks the binary itself (SVT-AV1 encodes
  through the stream's encoder, a requested key frame comes, the video crate's software path decodes every
  frame back). A prefix built before the script existed has no stamp and is rebuilt on the first run. Test
  streams for the video tests: `tools/streamgen/make-streams.sh` (needs NVENC) → `vendor/streams`.
- **Replacing `vendor/ffmpeg`** (worktrees link it, and their builds may be running): build beside it,
  `tools/build-ffmpeg.sh --stage vendor/ffmpeg.new` (the stage's pkg-config files already name
  `vendor/ffmpeg`), check it, then `mv vendor/ffmpeg vendor/ffmpeg.old && mv vendor/ffmpeg.new vendor/ffmpeg`.
  **A target directory that linked the old prefix keeps it**: ffmpeg-sys-next bundles the static libraries
  into its rlib and its build script does not re-run when they change, so run `cargo clean -p ffmpeg-sys-next`
  in each target directory (`build.rs` reads the new prefix and passes while the binary still holds the old
  one; `--selftest encoder-software` / `decibell-headless dump codecs` tell).
- **FFmpeg on Windows** is vcpkg's, at CI's pin, with **the client's overlay ports**
  (`tools/vcpkg-overlay`: an `ffmpeg` 8.0.1 port with an `svt-av1` feature, an `svt-av1` 4.2.0 port, and the
  pinned `dav1d` 1.5.3 port plus dav1d's own fix for Visual Studio 2026 (`a34e0685`: MSVC 19.5x rejects the
  undefined `__has_include_next` in its atomics shim; first seen in run 45864663165); what
  differs from upstream: `crates/decibell-core/tools/ffmpeg-encoders.md`, Windows). In its own vcpkg tree,
  not the Electron addon's: one tree holds one FFmpeg, and the addon's build copies only the FFmpeg DLLs, so an
  FFmpeg that needs `SvtAv1Enc.dll` / `dav1d.dll` would not load there. The binary cache is shared, so ports
  both trees use are built once. From PowerShell, in the checkout:
  ```powershell
  git clone https://github.com/microsoft/vcpkg C:\dev\vcpkg-gpui
  git -C C:\dev\vcpkg-gpui checkout e5a1490e1409d175932ef6014519e9ae149ddb7c
  C:\dev\vcpkg-gpui\bootstrap-vcpkg.bat -disableMetrics
  C:\dev\vcpkg-gpui\vcpkg.exe install "ffmpeg[nvcodec,amf,qsv,dav1d,svt-av1]:x64-windows" `
      "--overlay-ports=$PWD\gpui-client\tools\vcpkg-overlay"
  # every dev shell for the GPUI client:
  $env:VCPKG_ROOT = "C:\dev\vcpkg-gpui"                                  # ffmpeg-sys-next and build.rs read it
  $env:Path = "C:\dev\vcpkg-gpui\installed\x64-windows\bin;$env:Path"   # the DLLs, to run decibell / tests
  $env:CMAKE_POLICY_VERSION_MINIMUM = "3.5"                             # audiopus_sys's libopus vs CMake 4
  ```
  The Windows Native Check's `gpui-client` job installs the same line. After a change to the overlay:
  `vcpkg.exe remove ffmpeg svt-av1 --recurse`, then the `install` line again (classic mode does not rebuild an
  installed port whose recipe changed). Not yet run on Windows: the first CI run is the check.
- **Never build the shipped binary with `--features capture` or `--all-features`**: that feature turns on
  GPUI's test platform. `cargo build --workspace` is fine (the capture tool is skipped without its feature).

**Run**

```sh
target/release/decibell [--theme <id>] [--screen <name>] [--perf]
```

Without `--screen` / `--selftest` this is the app: the state model and the core start (profile:
`$DECIBELL_USER_DATA_DIR`, else `~/.config/decibell`; the at-rest key in the keychain), the window shows the
sign-in page or, with a saved sign-in, the main layout (`decibell-shell`). **A build without `dev-local`
talks to the live central**: for the local test stack build `--features dev-local` (honours
`DECIBELL_CENTRAL`, adds `--login`; never ship it) and use your own profile:

```sh
cargo build --release -p decibell-app --features dev-local
eval "$(../tools/dev/local-stack.sh client-env)"            # DECIBELL_CENTRAL=127.0.0.1:18080
DECIBELL_USER_DATA_DIR=/tmp/p DECIBELL_DEV_PASSWORD=decibell-test-1 target/release/decibell --login perf1
```

`--login <account>` (dev-local only) signs in without typing (the text fields are display-only until the
editor lands): refused unless `DECIBELL_CENTRAL` is loopback and the account is in the stack's
`accounts.json`. dev-local builds also print `decibell: signed-in <name>` / `connected <community>` on stdout
and read commands from the pipe `DECIBELL_DEV_CONTROL` names (`home`, `channel <community>\t<channel>`,
`minimize`, `quit`): what `tools/perf/clients/gpui.sh` drives.

`--theme`: nocturne (default), matinee, graphite, graphite-light, console, console-light, console-split;
it wins over the saved appearance for that run. F8 / Shift-F8 step through them while the window is open.
`--screen <name>` shows one registered screen (`--screen list`; the shell's are `auth`, `auth-register`,
`main-server`, `main-home`, drawn from a fixture without a core); `--selftest` runs in the token gallery's
window. The session's display is used: Wayland if `WAYLAND_DISPLAY` is set, else X11 (`WAYLAND_DISPLAY=
target/release/decibell` forces XWayland).

`--perf` (or `DECIBELL_PERF=1`) prints on stderr `decibell: first-frame <ms> ms since start
monotonic_ns=<ns>` when the first frame reached the screen and `decibell: frames drawn=<n> presented=<n>` at
exit and on `SIGUSR1`. The counters are always compiled in (`decibell_harness::perf`,
`patches/0002-gpui-frame-counters.patch`); they cost two atomic increments per frame and no timer.

**Check**

```sh
cargo test --workspace                           # unit tests (font matching, tokens, registry, args, …)
cargo clippy --workspace --all-targets
target/release/decibell --selftest all           # the app's self-tests, in its real window; exit code 0 / 1
WAYLAND_DISPLAY= target/release/decibell --selftest idle-frames     # the same under XWayland
tools/gen-fonts.sh --check                       # the static fonts match what the generator produces
tools/gen-theme/check.sh                         # the theme tokens match globals.css
```

Self-tests (`--selftest list`; one name, several separated by commas, or `all`) drive the real app on the
real platform: `boot` (first frame, every bundled font face resolves to itself), `idle-frames` (0 frames
drawn or presented and a sleeping main thread over 5 s; `DECIBELL_SELFTEST_IDLE_SECS` changes the window),
`window-frame` (maximise and restore: the client-side margin goes and comes back), `themes` (each theme
switch costs exactly one frame). They open a visible window for about 12 s and maximise it once: GPUI cannot
create a hidden window on Linux. Keep the pointer off it; a hover is a frame (the idle test retries twice
before failing).

The shell's self-tests (`shell-signin`, `shell-navigate`, `shell-members`, `shell-idle`, `shell-signout`,
`shell-no-keyring`) need a `dev-local` binary and the local stack (`tools/dev/local-stack.sh up`; they skip,
saying so, when it is down, and refuse a non-loopback `DECIBELL_CENTRAL`). Each starts the app again as a
child — `decibell --check <name> --login perf1` with a temporary profile and `DECIBELL_TEST_PROBES=1` — and
relays its log; `shell-no-keyring` runs it twice on a private session bus with no Secret Service.
`shell-idle` moves the focus with `tools/perf/winctl.sh` (KDE) and retries a measurement that another window,
the pointer or a core event from another client of the shared stack disturbed.

**Capture** (no window, no display server needed; Linux only)

```sh
cargo run -p decibell-app --features capture --bin decibell-capture -- \
    --screen gallery --all-themes --out-dir target/captures      # <dir>/<screen>-<theme>.png
cargo run -p decibell-app --features capture --bin decibell-capture -- \
    --screen gallery --theme console --size 1024x700 --scale 2 --out /tmp/console@2x.png
```

`--size` is the window's visible size in logical px (default 1280x800); the PNG is 20 px larger each way at
scale 1: it includes the transparent margin a client-decorated window draws its shadow in (black in the PNG).
Only the bundled fonts are loaded, so the same arguments give the same picture on any machine with a Vulkan
or GL driver.

**Measure** (`tools/perf/README.md`, Linux section)

```sh
../tools/perf/startup.sh --runs 5 --ready-marker 'first-frame .* monotonic_ns=(?P<ns>\d+)' -- \
    target/release/decibell --perf
target/release/decibell & ../tools/perf/winctl.sh unfocus $!
../tools/perf/measure.sh --pid $! --settle 20 --duration 60 --label "gpui skeleton idle"
```

**Where things are**

| Path | What |
|---|---|
| `Cargo.toml` | The workspace: members, the one GPUI definition, Zed's `[patch.crates-io]`, profiles (`release`: opt 3, fat LTO, 1 codegen unit, abort, strip; `release-s`: the same at `opt-level = "s"`) |
| `GPUI_PIN`, `patches/`, `tools/fetch-gpui.sh` | The pin, the patches against it, the script that builds `vendor/gpui-src` |
| `crates/decibell-theme` | Tokens generated from `globals.css` (`tools/gen-theme/`) |
| `crates/decibell-harness` | `Screen`, `SelfTest`, `TestCx`, `perf` |
| `crates/decibell-ui` | `Theme` global, fonts, `Label`, `Icon`, `window_frame`, `TitleBar`, embedded assets, the gallery screen |
| `crates/decibell-app` | `main.rs` → `args` → `boot` → `window` + `root`; `registry` (every screen and self-test); `selftests`; `bin/capture.rs` |
| `assets/fonts/static/`, `tools/gen-fonts.{sh,py}` | The static font faces GPUI loads, cut from the variable fonts in `assets/fonts/` |
| `assets/icons/` | SVG icons, embedded by `decibell-ui`'s build script |
| `.github/workflows/win-native-check.yml` (job `gpui-client`) | `cargo check` of this workspace on Windows; log on the `ci/win-gpui-check-log` branch |

## 3. Architecture notes and gotchas

- **Fonts as Chromium draws them** (found by the theme work, checked against the renderer's font files): Inter
  is used at **optical size 14 at every size** (the renderer imports fontsource's wght-only cut, so `opsz` never
  follows the font size; the bundled TTF has `opsz` 14–32 and must be held at 14). The OpenType features
  `cv11` / `ss01` / `ss03` named in the CSS **do nothing** in the renderer (its subset fonts lack them): leave
  them off. Mona Sans's default instance is weight 200: always pass a weight. Server tiles use Mona Sans at
  `wdth` 112.5, the wordmark at 125. JetBrains Mono ships as four static files (400 / 500 / 600 / 700). The
  renderer loads no italic face: italics are synthesised.
- **The local stack** moves the servers' hard-coded ports with an `LD_PRELOAD` shim (`tools/dev/bindshim.c`),
  so the server code is untouched. The community e2e suite wants 0.0.0.0:8082–8085: run it with the stack down
  or in its own network namespace (`tools/dev/README.md`).
- **Regional formats** (owner, 2026-11-26): dates, times of day and counts follow the system's regional
  settings with the app's English words, never en-US or the UI language as in Electron:
  `decibell_model::regional` (CONVENTIONS "Regional formats"). C / POSIX is no preference (7 Nov 2026,
  14:32, Monday first, no grouping); glibc's `en_US` zero-pads (11/07/2026) where Electron's ICU writes
  11/7/2026. Captures and the `chat-rows` comparison pin en-US (`Regional::pin`; Electron through `run.sh`'s
  `LC_ALL=en_US.UTF-8`: its `Intl` follows LC_ALL / LC_MESSAGES / LANG, never `--lang` or LC_TIME).
- **A client build reaches the live central unless** it is a debug build or has the `local-central` feature
  **and** `DECIBELL_CENTRAL=host:port` is set (a value without `:port` falls back silently). Check the address
  before typing a password.

## 4. Measurements (this machine: i7-12700K, RTX 4080, CachyOS / KDE Plasma Wayland, 2560×1440 at 280 Hz)

_The Electron baseline and each phase's numbers against the targets go here._

| Target | Electron 0.9.4 (Linux, this machine; **provisional**: the 30 s quick run of 2026-11-26 under load) | Target for this client | This client |
|---|---|---|---|
| Idle private memory, `#general` (10k) open, visible, unfocused | 341.7 MB (private dirty 253.4, PSS 476.7, RSS 947.1); minimized 335.6; Home 315.3 | ≤ 1/3: **≤ 114 MB** | — |
| Idle CPU, same | **14.5 %** of a core visible (the active server tile's glow animates: 276 frames/s); 0.54 % minimized; 0.49 % Home | ≤ 1/10: ≤ 1.45 % visible, ≤ 0.05 % with nothing animating | — |
| Idle wakeups, own threads | 4,292 / s visible; 4 / s minimized (+ ~210 / s from NVIDIA's unnamed GL threads either way) | ≤ 30 / s | — |
| Frames at idle | 276 / s visible; 0 minimized | 0 while nothing on screen changes | — |
| Startup to a visible window (warm, saved sign-in) | 671 ms (settled 1,178 ms) | ≤ 1/3: **≤ 224 ms** | — |
| Install size | 417.0 MB on disk (`/opt/Decibell`, 80 files) | ≤ 1/4: **≤ 104 MB** | — |
| Watching 2 streams | _pending (needs the stream bot)_ | CPU ≤ 1/3, GPU ≤ Electron, no frame lost beyond the source's | — |
| Scrolling the 10k channel, 1000 px/s | frame interval p50 3.60 / p99 3.70 ms, 14 missed vsyncs of 1,680 at 280 Hz | every frame in the refresh interval; 0 px on a history page | — |

Electron runs as an X11 client under XWayland here (as the shipped launcher starts it), packaged with
`electron-builder --linux dir` from the checkout with the `local-central` addon, on an isolated profile and a
private session bus (`tools/perf/electron-local.sh`). Take the real baseline on a quiet machine:
`tools/dev/local-stack.sh up && tools/perf/scenarios.sh electron all` (~24 min) → `tools/perf/results/electron-baseline.md`.

**Electron findings from the baseline work** (for `main`; nothing here was changed in the client):
- The active server tile's breathing glow (`ActiveTileGlow`, an infinite opacity animation) makes the whole
  client composite at the display's refresh rate whenever the window is visible: ~15 % of a core and ~4,300
  wakeups/s at 280 Hz on Linux, where Chromium under XWayland can't tell the window is covered. (On Windows
  Chromium stops drawing covered windows, which is how the Windows baseline read 0.58 %.)
- `--password-store=basic` (or any Linux machine without a keyring) leaves the client unable to sign in:
  `e2eeLocalKey: null` is refused by napi (see §5).
- The app re-points the `decibell://` handler at `decibell-electron-client.desktop` on every start.

**The skeleton** (`decibell` showing the theme gallery; release build, window unfocused, 20 s settle + 60 s;
startup = median of 5 warm runs; other builds were running, so startup is an upper bound):

| | Wayland | XWayland |
|---|---|---|
| Launch → window visible / first frame | 222 ms / 225 ms | 137 ms / 210 ms |
| Private memory (private dirty / PSS / RSS) | 78.0 MB (28.7 / 81.7 / 114.7) | 81.9 MB (29.0 / 85.7 / 118.7) |
| CPU, % of a core | 0.005 | 0.005 |
| Main-thread wakeups / s; frames at idle | 0.00; 0 | 0.00; 0 |
| GPU memory; threads | 57 MB; 33 | 58 MB; 34 |

- Where the 78 MB goes: NVIDIA's Vulkan driver ~37 MB, the binary's own pages 17.9 MB (file-backed, clean), heap
  17.9 MB, other anonymous 3.3 MB. The driver's `[vkps] Update` thread wakes 4.0/s and one unnamed driver
  thread 0.5/s; ours: 0.
- Patches' effect: `0003` (Vulkan before GL) −20 MB private, −30 ms startup; `0004` (X11 refresh timer parks)
  main-thread wakeups 280/s → 0 under XWayland.
- Binary 23.5 MB stripped at `opt-level = 3` (18.2 MB, −3.3 MB memory, +10 ms startup at `"s"`: profile
  `release-s`; decided once hot paths exist to measure).
**Core + model alone** (`decibell-headless`, release, signed in as a test account with Daily Gaming connected and
its 298-member roster loaded, 20 s settle + 60 s): **14.2 MB private** (2.6 dirty; PSS 14.2, RSS 17.0), CPU
0.0004 % of a core, **0.17 wakeups/s** for the whole process (10 in 60 s, all on one runtime worker: the
connections' keep-alives), 26 threads, all named, every other thread parked.

**Core + model in a voice channel** (headless, alone in Lounge, muted): 23.5 MB private, 2.75 % of a core,
**1,388 wakeups/s**, all from the shared core: `decibell-voice-recv` 442/s (a 1 ms read-timeout loop),
`decibell-audio` 197/s (5 ms pacing), PipeWire data loops 2 × 187/s, `decibell-video-recv` 160/s (runs in voice
although nobody streams), cpal in / out 94/s each, runtime 24/s; the model itself 0.9/s. Signed in with 100
users watched and not in voice: 16.2 MB private, 0.23 wakeups/s.

- **Lever not yet taken**: cosmic-text scans all installed fonts and walks them per (family, weight, style)
  query: ~70 ms of the 225 ms to first frame. The text engine work starts with bundled faces only and loads
  system fonts lazily for fallback.
- Private memory counts a single-process app's own binary pages (Private_Clean), which a multi-process app
  shares between its processes: quote private dirty and PSS beside it (Electron, same scenario: 341.7 private,
  253.4 private dirty, 476.7 PSS).

**Shell** (2026-11-26; `target/release/decibell` built `--features dev-local`, 52.9 MB stripped; signed in as `perf1`
with the saved sign-in, Daily Gaming connected with its 298-member roster; `tools/perf/scenarios.sh gpui <scenario>
--settle 20 --duration 60`, hooks `tools/perf/clients/gpui.sh`, results `tools/perf/results/gpui-*-20261126-15*`). **The
chat pane is still a placeholder** (the chat agent's slot), so the `#general` rows hold the shell — server bar, channel
list with voice rosters, member list, user panel — beside an empty pane, not 10k messages. The machine was busy (other
agents compiling): load average 14.5 / 8.3 / 3.3 / 9.3 at the start of startup / idle-home / idle-channel / hidden.

| | Electron 0.9.4 (quick run, §4 above) | This client (shell) |
|---|---|---|
| Startup to a visible window (warm, saved sign-in; median of 5, min–max) | 671 ms (settled 1,178) | **199 ms** (176–209; settled 200) |
| Idle private memory, `#general` open, visible, unfocused (private dirty / PSS / RSS) | 341.7 MB (253.4 / 476.7 / 947.1) | **124.6 MB** (36.0 / 129.6 / 141.1) |
| Same, minimized | 335.6 MB | 102.2 MB (36.2 / 117.8 / 140.7) |
| Home, focused | 315.3 MB | 98.6 MB (21.1 / 107.0 / 121.2) |
| Idle CPU, `#general` visible unfocused / minimized / Home | 14.5 % / 0.54 % / 0.49 % of a core | **0.01 %** / 0.01 % / 0.01 % |
| Idle wakeups, own threads (p50) | 4,292 / s visible; 4 / s minimized | **1 / s** in all three (`system-idle` 1.0 — the core's idle source —, `decibell-rt` 0.2; main thread 0.0); driver threads 0 / s |
| Frames at idle | 276 / s visible; 0 minimized | **0** in all three |
| GPU memory; threads | 43.3 MB; 9 processes / 152 threads | 58.2 MB; 1 process / 38 threads |
| The active tile's glow, window focused | the whole client composites at 276 Hz: 14.5 % of a core, ~4,300 wakeups / s | re-presents only (0 frames drawn): **28 presents / s** (cap 30); ~2.5 % of a core and ~125 own + ~170 driver wakeups / s while focused (p95 of a 30 s run focused 43 % of the time; the focus kept moving on the busy desktop); 0 when unfocused |

- Memory against the target (≤ 114 MB): the visible `#general` row is 124.6 MB total; the NVIDIA Vulkan driver's
  share measured for the skeleton is ~37 MB (§4 above), so the client's own is ~88 MB. Private dirty is 36 MB with the
  roster and the shell's layouts; the ~22 MB between visible and minimized is clean (file-backed binary and driver
  pages touched by drawing) and reclaimable — a 30 s run focused on the same state read 77.5–124.7 MB as the kernel
  dropped them. Not yet broken down further.
- The glow (owner decision, §6): breathing costs ~2.5 % of a core while focused at 30 presents / s, against
  Electron's 14.5 % always. Halving `decibell_shell::BREATHE_FPS` to 15 halves it; 0 if it should not breathe.
- Startup is the window with the shell booting (the core reads its configuration and signs in with the saved sign-in
  meanwhile: `decibell: signed-in` follows ~0.3 s later in the dev markers).

**Windows compile status**: Windows Native Check run 45855500083 (`b374268`) is green for both jobs: `check` (the
addon and the core, with the encoders on every vendor and the reworked voice receive path) and `gpui-client`
(`fetch-gpui.sh` with all nine patches + `cargo check --locked --workspace --all-targets` of every client crate
on the branch: theme, harness, ui, text, model, video, common, shell, chat, app, the service and content
crates, and the capture build). Self-tests and unit tests run on Linux only.

### Stream playback (`crates/decibell-video`, 2026-11-26)

`tools/streamgen/measure.sh <label> 30 <streams>`: `decibell --screen video-test` (release, 1280×800 window on
the 280 Hz display; focus makes no difference: the main window has no inactive frame cap) fed from
`vendor/streams` at the streams' timing, 5 s settle, 30 s sampled by `tools/perf/measure.sh`. Streams are
NVENC with the streamer's settings: 1080p60 at 10 Mbit/s, 1440p120 at 20, 2160p60 at 30. "main" is the four
threads named `decibell` (the main thread and GPUI's helpers), "decoder" the `decibell-video-<n>` threads
(Linux shows the name cut to 15 characters); the feeder thread standing in for the core's receive thread
(0.1–0.3 % of a core, one wakeup per frame) is in the CPU total. "replaced" = published but overtaken by the
next picture before a frame was drawn; "dropped" = shed by the hub.

**Which decoder.** Hardware on every GPU vendor, software only as the last resort (the owner's rule): Vulkan
Video on GPUI's own device first; where that is missing or fails, NVDEC on NVIDIA and VA-API on AMD / Intel;
then software. A decoder that does not open or fails in its first pictures is given up for that codec for the
session, with the reason logged and in the stream's stats; `VideoHub::probe_capabilities` decodes a test frame
per codec to know what to advertise; `DECIBELL_VIDEO_DECODER=vulkan|nvdec|vaapi|software` forces one
(`docs/video-design.md` §0.5). On this machine all four run, VA-API through NVIDIA's NVDEC-backed VA driver;
**nothing has run on an AMD or Intel GPU**: `docs/video-owner-checks.md` is the list for those.

**Every decoder** (2026-11-26, with GPUI patch `0007`; 5 s settle, 20 s sampled, "2x" the same file as two
streams; each row taken twice about half an hour apart, the run at the lower load shown, the other run's CPU and
load in the last column; load average 2–26 during the two passes, so read CPU as an upper bound):

| Run | Decoder | CPU, % of a core | main / decoder threads | Private, MB | Wakeups /s: main; decoder; driver | GPU sm / dec %, fb MB | In 20 s: received / decoded / presented / dropped | Decode p50 / p99, ms | Presented every p50 / p99, ms | Load avg | Other pass: CPU % at load |
|---|---|---|---|---|---|---|---|---|---|---|---|
| 1080p60-h264 | vulkan | **4.7** | 2.9 / 1.4 | 94 | 180; 260; 173 | 6.0 / 8.9, 161 | 1200 / 1200 / 1200 / 0 | 2.02 / 4.03 | 16.90 / 18.94 | 4.5 | 3.6 at 5.4 |
| 1440p120-h264 | vulkan | **6.6** | 4.0 / 2.2 | 77 | 352; 498; 142 | 3.9 / 19.6, 186 | 2400 / 2400 / 2396 / 0 | 1.89 / 2.37 | 8.45 / 8.96 | 4.9 | 11.1 at 8.3 |
| 2x1440p120-h264 | vulkan | **10.4** | 5.8 / 3.9 | 96 | 574; 1586; 142 | 8.7 / 36.9, 315 | 2401 / 2241 / 2235 / 160 + 2401 / 2401 / 2396 / 0 | 1.82 / 3.26, 2.88 / 3.65 | 8.45 / 12.03, 8.45 / 12.54 | 5.1 | 18.9 at 11.0 |
| 1080p60-hevc | vulkan | **4.0** | 2.3 / 1.4 | 77 | 178; 182; 171 | 0.2 / 4.5, 303 | 1200 / 1200 / 1200 / 0 | 0.72 / 1.70 | 16.90 / 17.92 | 5.0 | 6.0 at 15.5 |
| 1440p120-hevc | vulkan | **9.3** | 5.2 / 3.6 | 84 | 358; 390; 142 | 2.7 / 8.7, 332 | 2400 / 2400 / 2401 / 0 | 1.12 / 2.37 | 8.45 / 9.47 | 6.4 | 11.4 at 16.2 |
| 2x1440p120-hevc | vulkan | **16.3** | 8.4 / 7.1 | 111 | 584; 1320; 142 | 7.8 / 19.9, 606 | 2400 / 2400 / 2395 / 0 + 2400 / 2400 / 2390 / 0 | 1.18 / 2.50, 1.44 / 2.75 | 8.06 / 11.52, 8.06 / 12.03 | 7.9 | 19.9 at 16.0 |
| 1080p60-av1 | vulkan | **5.2** | 3.2 / 1.6 | 67 | 163; 221; 179 | 2.5 / 4.0, 201 | 1200 / 1201 / 1200 / 0 | 1.25 / 3.52 | 16.90 / 19.97 | 9.2 | 5.8 at 18.9 |
| 1440p120-av1 | vulkan | **11.1** | 7.0 / 3.5 | 115 | 304; 416; 141 | 3.4 / 12.7, 234 | 2400 / 2401 / 2384 / 0 | 1.70 / 3.78 | 8.45 / 11.52 | 13.5 | 11.7 at 22.4 |
| 2x1440p120-av1 | vulkan | **19.5** | 12.8 / 5.8 | 124 | 516; 1158; 141 | 5.5 / 25.7, 411 | 2400 / 2400 / 2373 / 0 + 2400 / 2401 / 2375 / 0 | 1.76 / 5.25, 2.24 / 5.50 | 8.45 / 15.62, 7.81 / 14.08 | 16.0 | 17.6 at 25.1 |
| 1080p60-h264 | nvdec | **6.9** | 3.9 / 2.2 | 204 | 157; 205; 189 | 4.2 / 5.0, 370 | 1200 / 1201 / 1200 / 0 | 1.70 / 4.48 | 16.90 / 22.02 | 20.0 | 6.9 at 26.0 |
| 1440p120-h264 | nvdec | **13.1** | 7.9 / 4.4 | 238 | 358; 502; 142 | 3.5 / 19.1, 393 | 2401 / 2401 / 2385 / 0 | 2.37 / 4.99 | 8.45 / 13.06 | 23.5 | 12.1 at 23.8 |
| 2x1440p120-h264 | nvdec | **21.7** | 11.9 / 7.5 (+ cuda-EvtHandlr 1.5) | 293 | 706; 1553; 142 | 7.8 / 52.0, 487 | 2400 / 2400 / 2401 / 0 + 2400 / 2401 / 2401 / 0 | 2.62 / 3.78, 2.88 / 4.22 | 8.45 / 10.50, 7.55 / 11.01 | 22.2 | 23.3 at 27.1 |
| 1080p60-hevc | nvdec | **7.2** | 3.6 / 2.3 | 234 | 169; 281; 181 | 0.3 / 3.0, 392 | 1200 / 1200 / 1200 / 0 | 2.11 / 6.27 | 16.90 / 20.99 | 21.6 | 7.2 at 25.3 |
| 1440p120-hevc | nvdec | **13.1** | 6.6 / 4.8 (+ cuda-EvtHandlr 1.1) | 254 | 348; 540; 141 | 12.2 / 11.9, 425 | 2400 / 2401 / 2400 / 0 | 2.11 / 5.25 | 8.45 / 11.52 | 22.1 | 13.1 at 24.3 |
| 2x1440p120-hevc | nvdec | **13.7** | 5.8 / 6.1 (+ cuda-EvtHandlr 1.2) | 298 | 596; 965; 142 | 7.0 / 17.6, 550 | 2400 / 2400 / 2399 / 0 + 2400 / 2401 / 2397 / 0 | 1.18 / 2.11, 1.12 / 2.02 | 8.06 / 11.52, 8.06 / 12.03 | 22.0 | 21.4 at 22.1 |
| 1080p60-av1 | nvdec | **4.3** | 2.0 / 1.2 | 215 | 179; 246; 171 | 1.2 / 3.8, 420 | 1200 / 1201 / 1201 / 0 | 1.06 / 1.44 | 16.90 / 16.90 | 17.0 | 6.5 at 22.9 |
| 1440p120-av1 | nvdec | **5.9** | 2.9 / 1.7 | 228 | 356; 543; 142 | 0.8 / 12.2, 472 | 2400 / 2401 / 2401 / 0 | 1.31 / 1.70 | 8.45 / 8.45 | 12.5 | 11.3 at 19.2 |
| 2x1440p120-av1 | nvdec | **17.0** | 8.5 / 6.2 (+ cuda-EvtHandlr 1.5) | 267 | 590; 1053; 142 | 3.2 / 25.8, 644 | 2400 / 2400 / 2398 / 0 + 2400 / 2400 / 2397 / 0 | 1.44 / 2.24, 1.38 / 2.11 | 8.45 / 11.52, 8.06 / 12.03 | 8.1 | 20.5 at 16.9 |
| 1080p60-h264 | vaapi | **6.7** | 3.0 / 2.4 | 238 | 174; 324; 173 | 2.2 / 5.0, 466 | 1200 / 1200 / 1201 / 0 | 1.44 / 3.26 | 16.90 / 18.94 | 9.1 | 7.3 at 20.8 |
| 1440p120-h264 | vaapi | **11.7** | 5.5 / 4.6 (+ cuda-EvtHandlr 1.2) | 278 | 350; 896; 140 | 2.8 / 22.9, 553 | 2400 / 2400 / 2400 / 0 | 2.75 / 5.50 | 8.45 / 11.52 | 11.1 | 13.5 at 19.5 |
| 2x1440p120-h264 | vaapi | **15.4** | 6.5 / 6.9 (+ cuda-EvtHandlr 1.3) | 310 | 585; 1873; 142 | 9.2 / 51.2, 805 | 2401 / 2401 / 2393 / 0 + 2400 / 2400 / 2394 / 0 | 2.62 / 4.48, 2.62 / 4.48 | 7.55 / 12.54, 7.55 / 12.54 | 13.1 | 20.0 at 17.6 |
| 1080p60-hevc | vaapi | **6.2** | 2.5 / 2.3 | 244 | 176; 335; 173 | 0.0 / 2.2, 482 | 1200 / 1200 / 1201 / 0 | 0.98 / 3.65 | 16.90 / 18.94 | 13.1 | 5.7 at 16.9 |
| 1440p120-hevc | vaapi | **11.0** | 4.5 / 4.7 (+ cuda-EvtHandlr 1.4) | 224 | 360; 792; 142 | 5.3 / 12.4, 578 | 2400 / 2400 / 2398 / 0 | 1.95 / 3.01 | 8.45 / 9.47 | 13.0 | 7.2 at 13.2 |
| 2x1440p120-hevc | vaapi | **12.8** | 4.6 / 6.6 (+ cuda-EvtHandlr 1.1) | 319 | 594; 1379; 142 | 6.3 / 17.3, 855 | 2400 / 2400 / 2397 / 0 + 2400 / 2400 / 2398 / 0 | 1.12 / 1.89, 1.12 / 1.70 | 7.55 / 11.52, 7.30 / 11.52 | 9.1 | 11.0 at 11.3 |
| 1080p60-av1 | vaapi | **3.9** | 1.5 / 1.3 | 228 | 179; 331; 171 | 1.5 / 4.0, 474 | 1200 / 1200 / 1201 / 0 | 1.06 / 1.50 | 16.90 / 16.90 | 6.9 | 4.0 at 7.8 |
| 1440p120-av1 | vaapi | **5.4** | 2.5 / 1.9 | 253 | 358; 771; 142 | 0.7 / 12.6, 570 | 2400 / 2400 / 2401 / 0 | 1.31 / 2.24 | 8.45 / 8.96 | 5.1 | 6.5 at 5.7 |
| 2x1440p120-av1 | vaapi | **13.7** | 6.0 / 5.9 (+ cuda-EvtHandlr 1.2) | 293 | 603; 1926; 142 | 17.1 / 32.5, 840 | 2400 / 2400 / 2401 / 0 + 2400 / 2400 / 2399 / 0 | 2.11 / 3.14, 3.01 / 4.22 | 7.81 / 11.52, 7.55 / 12.03 | 4.1 | 9.7 at 4.5 |
| 1080p60-h264 | software | **21.8** | 1.7 / 19.8 | 81 | 182; 158; 172 | 9.9 / 0.0, 74 | 1200 / 1200 / 1200 / 0 | 4.03 / 6.27 | 16.90 / 18.94 | 4.0 | 24.6 at 4.6 |
| 1440p120-h264 | software | **50.3** | 3.1 / 46.9 | 98 | 362; 288; 142 | 6.0 / 0.0, 86 | 2400 / 2400 / 2400 / 0 | 4.48 / 6.02 | 8.45 / 9.98 | 2.8 | 65.3 at 4.8 |
| 2x1440p120-h264 | software | **104.8** | 4.5 / 99.8 | 137 | 580; 716; 142 | 15.7 / 0.0, 114 | 2400 / 2400 / 2400 / 0 + 2400 / 2400 / 2401 / 0 | 4.74 / 6.27, 4.74 / 6.02 | 8.45 / 11.52, 8.06 / 11.52 | 2.4 | 138.8 at 7.8 |
| 1080p60-hevc | software | **31.8** | 1.7 / 29.8 | 91 | 182; 167; 171 | 7.7 / 0.0, 74 | 1200 / 1200 / 1200 / 0 | 5.76 / 7.81 | 16.90 / 18.94 | 2.5 | 33.0 at 8.2 |
| 1440p120-hevc | software | **84.1** | 2.9 / 80.9 | 116 | 362; 252; 142 | 10.2 / 0.0, 86 | 2400 / 2400 / 2401 / 0 | 7.30 / 9.47 | 8.45 / 10.50 | 2.4 | 83.5 at 6.7 |
| 2x1440p120-hevc | software | **78.1** | 2.8 / 74.6 | 214 | 230; 192; 70 | 4.0 / 0.0, 108 | 2400 / 883 / 883 / 1466 + 2400 / 885 / 885 / 1463 | 8.06 / 16.90, 8.06 / 16.90 | 9.47 / 18.94, 10.50 / 17.92 | 2.1 | 168.9 at 5.1 |
| 1080p60-av1 | software | **34.9** | 1.4 / 1.6 (+ dav1d-worker 31.6) | 88 | 182; 188; 171 | 3.3 / 0.0, 74 | 1200 / 1200 / 1200 / 0 | 4.99 / 6.78 | 16.90 / 17.92 | 4.2 | 57.9 at 5.1 |
| 1440p120-av1 | software | **73.1** | 2.8 / 5.3 (+ dav1d-worker 64.6) | 107 | 227; 181; 104 | 2.8 / 0.0, 85 | 2401 / 1475 / 1474 / 927 | 7.55 / 12.54 | 8.45 / 12.54 | 3.0 | 87.0 at 11.4 |
| 2x1440p120-av1 | software | **77.9** | 3.1 / 6.6 (+ dav1d-worker 67.5) | 188 | 175; 149; 63 | 2.5 / 0.0, 102 | 2400 / 606 / 606 / 1794 + 2401 / 602 / 602 / 1799 | 11.52 / 17.92, 11.52 / 16.90 | 11.01 / 17.92, 11.01 / 17.92 | 4.4 | 125.6 at 9.9 |

- Every hardware decoder plays every row, packet to published picture in 1–3 ms, nothing dropped in 53 of the
  54 hardware runs; in one (2 × 1440p120 H.264 on Vulkan Video, the row shown) a stream shed 160 frames once
  and waited for the file's next keyframe: a decoder thread that is not scheduled for three frame intervals
  overflows the three-frame queue, and with the machine at load 20–50 the paced self-tests shed the same way
  on every decoder (`docs/video-design.md` §12.17). Their
  CPU is within each other's noise: at load 4–8, one 1440p120 stream is 5.4–6.8 % of a core and two are
  9.6–10.4 %.
- The vendor decoders cost memory, not CPU: 67–140 MB private with Vulkan Video, 204–307 MB with NVDEC (a CUDA
  context: about 150 MB of host and 200 MB of GPU memory more), 194–319 MB with the VA driver here (its own
  CUDA context). They exist only on a machine where Vulkan Video failed, and only while such a stream plays.
- Software keeps up with 1080p60 of any codec, with one 1440p120 stream of H.264 (50 % of a core) or HEVC
  (84 %) and with two of H.264 (105 %); two 1440p120 HEVC streams and AV1 at 1440p120 drop frames on four
  threads per stream.
- Decoder start: Vulkan Video under 1 ms, NVDEC 65–270 ms, the VA driver 220–670 ms; frames that arrive
  meanwhile are queued (up to 64) and worked off in a burst.

**The present-only path** (GPUI patch `0007`: a present for a new picture encodes the frame the renderer kept
instead of writing the scene to the GPU again; `patches/README.md` §0007), before and after on the same binary:

| | Scene written per present (`DECIBELL_VIDEO_FASTPATH=0`: the renderer before `0007`) | Kept frame (`0007`) |
|---|---|---|
| UI-thread allocations per present (1 × 1440p120, 366 presents counted, two runs each) | 281, 288 | 140, 145 |
| `WgpuRenderer::draw` per present, mean of a 9 s run, three runs each, alternating, at load 5 | 263, 237, 281 µs | 183, 188, 175 µs |
| the same on the quietest machine of the day (load 3.5, one run each, a development build) | 139 µs | 108 µs |
| UI threads' CPU, 2 × 1440p120 (HEVC + AV1), % of a core, two runs each at load 4.0–4.8 | 6.25, 6.11 | 5.04, 4.83 |
| whole process, the same runs | 11.0, 10.8 | 9.8, 9.6 |
| UI threads' CPU, 1 × 1440p120 HEVC, two runs each (load 4.0–8.5) | 3.32, 4.37 | 3.44, 3.62 |
| whole process, the same runs | 5.8, 7.7 | 6.4, 6.8 |

**Earlier series** (Vulkan Video only, before patch `0007`; 30 s sampled). **The machine was never quiet**
(other agents building): two series, the load average at the start of each run in the last column. CPU moves by a factor of two between runs of the same scenario (4.7–7.3 % for one
1440p120 HEVC stream), and not simply with the load: a quieter machine clocks its cores down, and the figure is
time on a core, not work. Read the lowest figure of a scenario as "at most this on a quiet machine", and the
counts, wakeups and GPU columns as stable.

Series 2 (load average 1.3–8.3):

| Run | CPU, % of a core | main / decoder threads | Private / PSS, MB | Wakeups /s: main; decoder; driver | GPU sm / dec %, fb MB | In 30 s: received / decoded / presented / replaced / dropped | Decode p50 / p99, ms | Presented every p50 / p99, ms (source) | Load avg |
|---|---|---|---|---|---|---|---|---|---|
| 1080p60-h264 | **4.1** | 2.6 / 1.1 | 97.6 / 113.9 | 180; 238; 173 | 0.0 / 5.8, 161 | 1800 / 1800 / 1800 / 1 / 0 | 1.82 / 4.22 | 16.90 / 18.94 (16.67) | 1.56 |
| 1080p60-hevc | **4.7** | 2.9 / 1.4 | 79.1 / 105.9 | 180; 178; 172 | 4.4 / 2.5, 304 | 1800 / 1800 / 1798 / 3 / 0 | 0.88 / 3.26 | 16.90 / 18.94 (16.67) | 4.18 |
| 1080p60-av1 | **3.4** | 2.1 / 1.0 | 119.8 / 122.5 | 180; 237; 171 | 9.6 / 4.1, 202 | 1800 / 1801 / 1801 / 0 / 0 | 1.82 / 4.48 | 16.90 / 18.94 (16.67) | 5.19 |
| 1440p120-h264 | **6.8** | 4.5 / 2.0 | 129.1 / 131.0 | 360; 586; 142 | 9.5 / 28.7, 187 | 3600 / 3600 / 3601 / 0 / 0 | 3.14 / 4.74 | 8.45 / 9.98 (8.33) | 4.29 |
| 1440p120-hevc | **7.3** | 4.2 / 2.6 | 138.1 / 140.0 | 359; 476; 141 | 12.8 / 11.7, 332 | 3601 / 3601 / 3601 / 1 / 0 | 1.57 / 3.65 | 8.45 / 10.50 (8.33) | 5.00 |
| 1440p120-av1 | **10.2** | 6.7 / 3.0 | 120.5 / 122.4 | 358; 505; 142 | 15.1 / 14.0, 235 | 3600 / 3601 / 3601 / 0 / 0 | 2.37 / 5.25 | 8.45 / 11.01 (8.33) | 4.93 |
| 2x1440p120 | **13.6** | 8.4 / 4.6 | 147.2 / 149.1 | 720; 1029; 142 | 25.4 / 28.0, 509 | 3601 / 3601 / 3600 / 0 / 0 + 3600 / 3601 / 3601 / 0 / 0 | 1.31 / 2.62, 1.89 / 3.14 | 8.45 / 9.98 (8.33), 8.45 / 10.50 (8.33) | 8.31 |
| 2160p60-h264 | **3.3** | 1.9 / 1.1 | 141.7 / 143.8 | 180; 409; 169 | 7.3 / 30.4, 259 | 1800 / 1800 / 1800 / 0 / 0 | 6.78 / 8.45 | 16.90 / 17.92 (16.67) | 7.59 |
| 2160p60-hevc | **3.7** | 1.8 / 1.6 | 159.7 / 161.8 | 180; 265; 172 | 5.4 / 11.7, 412 | 1800 / 1800 / 1801 / 0 / 0 | 3.01 / 5.25 | 16.90 / 17.92 (16.67) | 5.36 |
| 2160p60-av1 | **3.2** | 1.9 / 0.9 | 121.1 / 123.0 | 180; 312; 170 | 1.2 / 12.1, 327 | 1800 / 1800 / 1800 / 0 / 0 | 4.74 / 7.30 | 16.90 / 18.94 (16.67) | 3.73 |
| 1440p120-hevc-mirror | **5.7** | 3.3 / 2.0 | 138.1 / 140.0 | 360; 479; 142 | 14.4 / 10.3, 332 | 3600 / 3600 / 3601 / 0 / 0 | 1.50 / 3.65 | 8.45 / 10.50 (8.33) | 3.00 |
| sw-1080p60-h264 | **21.1** | 1.7 / 19.1 | 127.1 / 128.9 | 180; 138; 170 | 5.8 / 0.0, 73 | 1800 / 1801 / 1800 / 0 / 0 | 4.22 / 6.78 | 16.90 / 18.94 (16.67) | 2.72 |
| sw-1080p60-av1 | **35.0** | 1.7 / 1.6 (+ dav1d 31.4) | 131.9 / 134.6 | 180; 197; 171 | 7.0 / 0.0, 73 | 1800 / 1800 / 1800 / 0 / 0 | 5.50 / 8.96 | 16.90 / 19.97 (16.67) | 2.27 |
| x11-1440p120-hevc | **4.7** | 2.8 / 1.6 | 148.8 / 152.9 | 359; 448; 142 | 11.4 / 11.2, 333 | 3600 / 3600 / 3601 / 0 / 0 | 1.44 / 3.39 | 8.45 / 9.98 (8.33) | 1.67 |
| x11-2x1440p120 | **7.8** | 4.1 / 3.2 | 158.5 / 160.5 | 493; 1517; 142 | 17.3 / 28.0, 510 | 3600 / 3600 / 3600 / 1 / 0 + 3600 / 3601 / 3600 / 1 / 0 | 1.31 / 2.62, 2.50 / 3.78 | 7.04 / 11.52 (8.33), 7.30 / 11.52 (8.33) | 1.27 |

Series 1 (load average 3.4–8.7), the same code:

| Run | CPU, % of a core | main / decoder threads | Private / PSS, MB | Wakeups /s: main; decoder; driver | GPU sm / dec %, fb MB | In 30 s: received / decoded / presented / replaced / dropped | Decode p50 / p99, ms | Presented every p50 / p99, ms (source) | Load avg |
|---|---|---|---|---|---|---|---|---|---|
| 1080p60-h264 | **3.0** | 2.0 / 0.8 | 126.4 / 127.4 | 180; 235; 173 | 4.5 / 6.1, 161 | 1800 / 1800 / 1801 / 0 / 0 | 1.63 / 3.78 | 16.90 / 18.94 (16.67) | 6.98 |
| 1080p60-hevc | **3.2** | 1.9 / 1.0 | 131.0 / 132.0 | 180; 224; 171 | 0.0 / 1.9, 304 | 1800 / 1800 / 1801 / 0 / 0 | 0.91 / 3.65 | 16.90 / 18.94 (16.67) | 5.73 |
| 1080p60-av1 | **2.8** | 1.8 / 0.8 | 120.0 / 121.6 | 180; 234; 170 | 0.0 / 3.8, 202 | 1800 / 1801 / 1801 / 0 / 0 | 1.76 / 4.22 | 16.90 / 18.94 (16.67) | 6.19 |
| 1440p120-h264 | **5.2** | 3.3 / 1.5 | 124.6 / 128.7 | 356; 630; 142 | 6.8 / 25.3, 187 | 3600 / 3600 / 3551 / 50 / 0 | 2.50 / 4.22 | 8.45 / 10.50 (8.33) | 3.95 |
| 1440p120-hevc | **5.3** | 3.0 / 1.9 | 138.7 / 139.8 | 360; 478; 142 | 9.8 / 9.7, 332 | 3600 / 3600 / 3597 / 4 / 0 | 1.50 / 3.52 | 8.45 / 10.50 (8.33) | 4.94 |
| 1440p120-av1 | **5.6** | 3.6 / 1.6 | 119.9 / 121.6 | 360; 519; 142 | 8.6 / 14.2, 235 | 3600 / 3601 / 3601 / 0 / 0 | 2.24 / 4.99 | 8.45 / 11.01 (8.33) | 3.68 |
| 2x1440p120 | **17.3** | 10.9 / 5.7 | 148.9 / 150.1 | 712; 1472; 142 | 25.8 / 28.0, 509 | 3601 / 3601 / 3601 / 1 / 0 + 3601 / 3602 / 3602 / 0 / 0 | 1.38 / 2.88, 2.50 / 4.22 | 8.45 / 9.98 (8.33), 7.30 / 11.01 (8.33) | 3.42 |
| 2160p60-h264 | **5.2** | 3.1 / 1.6 | 142.2 / 143.4 | 180; 423; 170 | 5.6 / 32.0, 259 | 1800 / 1800 / 1800 / 0 / 0 | 7.04 / 8.45 | 16.90 / 17.92 (16.67) | 5.49 |
| 2160p60-hevc | **5.5** | 2.7 / 2.3 | 160.3 / 161.4 | 180; 276; 172 | 14.0 / 11.0, 412 | 1800 / 1800 / 1800 / 0 / 0 | 3.39 / 5.50 | 16.90 / 17.92 (16.67) | 7.81 |
| 2160p60-av1 | **3.9** | 2.3 / 1.2 | 104.5 / 113.6 | 180; 310; 171 | 7.7 / 16.9, 327 | 1802 / 1803 / 1803 / 0 / 0 | 4.03 / 6.27 | 16.90 / 17.92 (16.65) | 6.82 |
| 1440p120-hevc-mirror | **6.8** | 4.0 / 2.4 | 138.6 / 139.7 | 360; 473; 142 | 13.0 / 11.3, 332 | 3600 / 3600 / 3601 / 0 / 0 | 1.50 / 3.65 | 8.45 / 10.50 (8.33) | 5.52 |
| sw-1080p60-h264 | **23.6** | 2.2 / 21.1 | 102.4 / 115.9 | 180; 138; 171 | 5.8 / 0.0, 73 | 1800 / 1801 / 1801 / 0 / 0 | 4.22 / 8.06 | 16.90 / 19.97 (16.67) | 5.01 |
| sw-1080p60-av1 | **37.5** | 2.0 / 1.8 (+ dav1d 33.3) | 121.2 / 128.5 | 180; 200; 172 | 8.0 / 0.0, 73 | 1800 / 1800 / 1800 / 0 / 0 | 5.76 / 9.47 | 16.90 / 18.94 (16.67) | 5.35 |
| x11-1440p120-hevc | **12.0** | 7.4 / 4.2 | 138.2 / 144.6 | 359; 428; 142 | 12.3 / 11.0, 333 | 3600 / 3600 / 3601 / 0 / 0 | 1.63 / 3.78 | 8.45 / 10.50 (8.33) | 4.10 |
| x11-2x1440p120 | **17.8** | 10.2 / 6.9 | 158.6 / 159.6 | 475; 1217; 142 | 20.6 / 28.0, 510 | 3600 / 3600 / 3599 / 1 / 0 + 3600 / 3601 / 3596 / 5 / 0 | 1.57 / 3.14, 2.11 / 3.39 | 8.45 / 12.03 (8.33), 8.06 / 12.03 (8.33) | 8.67 |

- **No stream** (the same screen, nothing to play): 0.01 % CPU, 0 wakeups / s of our threads, 107 MB private
  (25.7 MB dirty), 58 MB of GPU memory. A stream adds 10–50 MB of private memory depending on codec and size
  (mostly the driver's decode session) and 100–450 MB of GPU memory (the decoder's reference pool; the five
  slots are 16 / 28 / 62 MB at 1080p / 1440p / 2160p). After the stream ends everything of it is freed
  (`video-idle`: 0 threads, 0 textures, 0 frames drawn or presented over 5 s).
- **Frames**: in every hardware run every frame received was decoded and none was dropped; 0–4 of 3,600
  pictures were overtaken before a frame was drawn, except one run under load (1440p120 H.264, series 1: 50).
  Pacing: pictures reach a frame every 8.45 ms at p50 for an 8.33 ms source and every 16.9 ms for 16.67 ms (the
  histogram's buckets are 4–6 % wide), p99 within 2.7 ms of the source interval.
- **Wakeups per frame**: 3 on the main thread (the presenter's wake, the frame, the compositor's callback),
  4–5 on the decoder thread (H.264 at 2160p: 7), 0 anywhere when no picture arrives. The driver's
  `[vkps] Update` thread wakes 142 times a second in any process with a swapchain.
- **Small views**: the `--mirror` row adds a 384×216 mini-player of the 1440p stream (the 4×4-tap
  minification filter of patch `0005`): within the noise of the row without it (5.7–6.8 against 4.7–7.3 % CPU,
  13.0–14.4 against 9.8–12.8 % GPU `sm`); with the filter compiled out the mirror row measured 7.2 % CPU and
  18 % `sm`, i.e. the filter has no measurable cost.
- **Allocations** (`tools/streamgen/malloc-count.c`, 1440p120 HEVC): our Rust code allocates nothing per
  frame (`cargo test -p decibell-video --test alloc`: 0 over 100 steady-state frames, all three codecs, and
  again after a decoder was replaced); the decoder thread sees 38 `malloc` calls per frame with Vulkan Video,
  20 with NVDEC, 35 with VA-API and 17 in software, all inside FFmpeg and the drivers; the main thread 140–145
  per presented frame (281–288 before patch `0007`), what wgpu and the driver need for any frame
  (`docs/video-design.md` §12.6).
- **Against the prototype** (`docs/video-design.md` §11: 2.8–3.9 % for 1080p60, 4.1–4.4 % for 1440p120,
  7.5 % for 2 × 1440p120, the quietest runs of its day). The prototype binary measured again today with the
  same tool, between the two series: **8.5 %** for 1 × 1440p120 HEVC (main 5.2, decoder 2.9; 160 MB private)
  and **20.1 %** for 2 × 1440p120 (main 13.1, decoders 6.3), against this crate's 4.7–7.3 % and 7.8–17.3 %
  (139 MB private). So: not worse than the prototype like for like; the absolute numbers of both need a quiet
  machine.
- **Against Electron**: not measurable yet. A like-for-like row needs the streamer bot (two accounts streaming
  these same `.dbv` files into a voice channel of the local stack), Electron watching both in its grid with the
  window visible, and this client doing the same through the model (so the core's receive, FEC and decrypt
  cost is in both). The Qt spike's Windows figures for the same streams (Electron 31.5–33.1 % for one
  1440p120 stream, 52.6 % for two) put the target for two at ≤ 17.5 %.
- **Modes**: all ten video self-tests (`video-hw`, `-idle`, `-two`, `-sw`, `-recover`, `-switch`, `-nvdec`,
  `-vaapi`, `-order`, `-fastpath`) also pass with `DECIBELL_VIDEO_SHARED_QUEUE=1` (the single-queue mode AMD /
  Intel would use, forced on NVIDIA) and under XWayland; `video-sw` and `video-idle` with
  `DECIBELL_VIDEO_UPLOAD=wgpu`; `video-fastpath` under `crates/decibell-video/tools/alloc-count.c` (137
  allocations per present on the UI thread, budget 170).

## 5. Open items

- **Every thread the client spawns gets a name.** `measure.sh` classes threads as own / driver / unnamed; NVIDIA's
  GL threads are unnamed, so an unnamed thread of ours would be counted with the driver's.
- **Pre-existing Electron bug found by the split's smoke test (for `main`, not fixed here)**:
  `electron/main/addon.ts` passes `e2eeLocalKey: loadOrCreateE2eeLocalKey()`, which is `null` when `safeStorage`
  is unavailable; napi-derive 2 accepts only `undefined` for an `Option` field, so `init` throws `Failed to
  convert JavaScript value Null into rust type String on InitOptions.e2eeLocalKey`. Fix: `?? undefined`.
- Core's `Cargo.lock` must be re-seeded when core's dependencies change (recipe in `CLAUDE.md`); CI checks
  `--locked`.
- **zbus features**: every crate of this client depends on the core with `default-features = false, features =
  ["zbus-async-io"]` (GPUI's own dependencies use zbus on async-io; the core's default `zbus-tokio` is the
  Electron addon's). With both on, zbus picks tokio and GPUI's D-Bus calls panic outside a tokio context: check
  `cargo tree -e features -i zbus` shows no `tokio`. `decibell_model::core::prepare` (the earlier workaround) can
  go when the app's boot is wired. Keep `blocking` at 1.6.x in the lockfile (1.7.0 keeps an idle pool thread
  waking every 500 ms).
- **Where audio can click or gap today** (read-only audit of the core's voice path, 2026-11-26; pre-existing in
  both clients; file:line in the voice task's notes and the review log). Fixing any of these changes audible
  output, so each fix needs a sample-exact test and the owner's listening test:
  1. output device switched mid-speech: rings are cut with no fade and the next frame gets no fade-in (click);
  2. undeafen while someone talks: no fade-in (click);
  3. per-user volume change: gain steps at a 20 ms frame boundary, no ramp ("zipper");
  4. input device switched mid-speech: gate / pre-roll not reset, AEC reconverges (~1 s of echo);
  5. toggling echo cancellation / noise suppression / gain control rebuilds the processors (gain resets);
  6. the output pull estimate is not reset on a device change (crackle or extra latency for ~2 s, e.g. wired →
     Bluetooth);
  7. the output callback goes silent for one period if it cannot take the stream-audio lock (very rare);
  8. ten lost frames in a row reset the jitter buffer (~100 ms gap; by design);
  9. stream audio splices when 60 ms over target, no onset fade;
  10. push-to-talk press has no pre-roll (10 ms fade-in on the first frame); mute mid-word sends a 60 ms tail;
  11. building a device stream blocks the audio loop during a hot-swap (a hiccup for listeners);
  12. the audio thread (not the device callback) allocates per capture frame.
- Timed waits that remain in the core by decision: the 5 ms audio tick and device-clock wakeups in voice, the
  30 s server pings, the 500 ms MLS tick (could be deadline-driven), `default_device_watch`'s 1 s poll on
  Windows / macOS (should become OS notifications), the documented hotkey polls.
- **Physical key codes: done** (GPUI patch 0012, `patches/README.md`). Key events carry the platform's
  physical key (`KeyDownEvent::code`: the evdev code on Linux, the scan code on Windows) and modifier keys
  arrive with their side (`ModifiersChangedEvent::key`); `decibell_model::live::hotkeys` names them through
  the core's table (`code_of`), and the recorder (`Recorder`) and the focused fallback (`Hotkeys::key_event`)
  use that, tested under xkb's `tr` and `de` layouts. Still to do: the Keybinds screen feeds the recorder, the
  app's root feeds the fallback; and by hand, once: record a binding on a real Turkish or German keyboard
  under Wayland, and on Windows (scan codes there were never run).
- `system_idle` can only be queried, not subscribed to: auto-away is late by up to one threshold on Wayland.
  There is no device-change notification from the core and no mic test outside a voice session (Electron used
  Web Audio for the out-of-voice meter).
- **Voice receive path allocates per packet** (~5 allocations per voice packet per talking peer; the core
  follow-up measured it and left a plan in its notes: fixed datagram slots on the existing ring, peers keyed by
  the 32-byte sender id, in-place open, preallocated Opus slots). Not done; do it if voice CPU / wakeups need it.
- `media::watch_stream_add` and `events::set_stream_frame_sink` update their sets by load-clone-store: two
  concurrent writers can lose an update (found by the core follow-up's tests; production code unchanged).
- **A cached view goes stale unless it observes what it reads** (`docs/model.md` §4): GPUI re-renders a cached
  view only when that view itself is notified.
- **Community server bug (for `main`, not changed here)**: the first page of the member list returns the
  requester's own `role_ids` doubled when their row is already in the page (`src/community/main.cpp`); this
  client de-duplicates on arrival.

- **The message list's "today" is fixed when the list is built** (found by the regional-formats work): a
  list left open past midnight keeps labelling yesterday's messages with a bare time until it is rebuilt.
  Fix: one one-shot timer for the next local midnight (not a polling timer) that re-labels visible rows;
  check what the renderer does first (its bubbles are memoised, so it may be stale the same way).
- **Self-test order dependence**: `--selftest chat-rows,chat-history` fails in `chat-history` ("150 → 95
  rows"); each passes alone and in the registry's order (`chat-history` first). `chat-rows` leaves its
  fixture in the process's model. Make each chat self-test start from its own seeded state.
- **`message_stamp` costs ~117 ns** a call, of which ~58 ns is formatting and the rest `chrono::Local`'s
  zone conversion (it re-checks `TZ` / `/etc/localtime` at most once a second per thread). Rows cache
  their labels, so it is not on the scroll path; cache the local time per message if a profile ever shows it.

## 6. Needs owner

Things only the owner can do; nothing here blocks the rest.

- **Watch a stream in the Electron client built from this branch before it merges to `main`.** The core's video
  receive path was reworked for this client (frames reach the napi sink directly from the receive thread, one
  hop fewer; the Linux self-preview moves its buffer instead of copying; reassembly and FEC no longer copy
  fragments twice). Electron's bar and the allocation tests pass and frames are byte-identical in tests, but no
  one has watched a real stream through it yet. Also the decision list in `docs/electron-quirks.md`.
- **Listen to voice after the core's receive-path change** (two or three people in an MLS channel, talking in
  turns): no new crackle, word starts not clipped, speaking indicators and mute state as before; a P2P call both
  ways with a screen share in it; watch a stream (first picture as fast as before, stream audio plays), stop and
  re-watch; join / leave / switch channels quickly and quit: nothing hangs. The hand-off of a voice packet to
  the audio thread is 3–5 µs later at the median than the old 1 ms polling loop and better at p99.
- **Then listen for the pre-existing click sources** listed in §5 ("Where audio can click or gap today"),
  especially: switch output device while someone talks; undeafen mid-sentence; drag a user's volume while they
  talk; switch wired → Bluetooth mid-talk.
- **Run the encoder checks on AMD and Intel hardware** (`crates/decibell-core/tools/verify-encoders.md`: one
  command per backend on Linux and Windows) and a real Go Live through each.
- **Decide the active-tile glow's behaviour.** In Electron it breathes forever (2.4 s opacity cycle) and costs
  ~15 % of a core on Linux while the window is visible. This client breathes it only while the window is
  focused, at a capped frame rate, presenting without re-rendering; unfocused it rests. Say if it should
  breathe unfocused too (it then draws frames at idle) or not at all.
