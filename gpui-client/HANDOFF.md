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
| Text inputs (`decibell_text::input`, `TextField`, `TextArea`) + GPUI patch for physical key codes | in progress, **in the worktree `.worktrees/land` (branch `wip/land`)**: merge that branch when it reports |
| Message list and rows (`crates/decibell-chat`) | in progress (main tree) |
| Video crate (`crates/decibell-video`, GPUI patches 0005 / 0006, `tools/build-ffmpeg.sh`, `tools/streamgen/`) | done `a7d5232`: Vulkan Video decode of H.264 / HEVC / AV1 on GPUI's device, software fallback, device-loss recovery, `VideoView`; self-tests `video-hw`, `video-idle`, `video-two`, `video-sw`, `video-recover`, `video-switch`; numbers in §4 "Stream playback". Not yet wired into the app's registry / boot on the branch (the lines are in the main tree's working files with the shell work) |
| Video, part 2: NVDEC and VAAPI hardware fallbacks, decoder selection by real probe, present-only fast path in the renderer | in progress, worktree `.worktrees/video2` (`wip/video2`) |
| Core: voice idle wakeups and polling, per-packet voice allocations, `FrameBuf` padding, idle / device-change events | in progress, worktree `.worktrees/core-voice` (`wip/core-voice`) |
| Encoders on every vendor + software fallback (core) | in progress, worktree `.worktrees/core-encoders` (`wip/core-encoders`) |
| Shell (boot with core + model, sign-in pages, server bar, channel sidebar, user panel, member list; `decibell-common`, `decibell-shell`) | in progress |
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
| `.worktrees/core-encoders` / `wip/core-encoders` | encoders on every vendor + SVT-AV1 software fallback (core) |
| `.worktrees/core-voice` / `wip/core-voice` | voice wakeups / polling, voice allocations, `FrameBuf` padding (core) |
| `.worktrees/land` / `wip/land` | text inputs + GPUI patch 0012 (physical key codes) |
| `.worktrees/video2` / `wip/video2` | NVDEC / VAAPI decode, probe-based selection, renderer fast path |
| main tree (uncommitted until each reports) | the shell (`decibell-common`, `decibell-shell`, app boot), the message list (`decibell-chat`) |

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
  `tools/build-ffmpeg.sh` builds it (no sudo; about 3 minutes; a no-op when the prefix is current): it fetches
  FFmpeg 8.1.3, dav1d, nasm, nv-codec-headers and the Vulkan headers, each checked against a pinned SHA-256,
  and fails if the configured tree is not LGPL-2.1+. It needs a C toolchain, make, curl, python3, ninja and
  the libva + libdrm headers. A prefix built before the script existed has no stamp and is rebuilt on the first
  run. Test streams for the video tests: `tools/streamgen/make-streams.sh` (needs NVENC) → `vendor/streams`.
- **Never build the shipped binary with `--features capture` or `--all-features`**: that feature turns on
  GPUI's test platform. `cargo build --workspace` is fine (the capture tool is skipped without its feature).

**Run**

```sh
target/release/decibell [--theme <id>] [--screen <name>] [--perf]
```

`--theme`: nocturne (default), matinee, graphite, graphite-light, console, console-light, console-split;
F8 / Shift-F8 step through them while the window is open. `--screen <name>` shows one registered screen
(`--screen list`); without it the window shows the default screen (the token gallery until the app shell
exists). The session's display is used: Wayland if `WAYLAND_DISPLAY` is set, else X11 (`WAYLAND_DISPLAY=
target/release/decibell` forces XWayland). Nothing here talks to a server yet.

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

**Windows compile status**: Windows Native Check run 45819750197 (`fc19cdf`) is green for both jobs: `check` (the
addon and the core) and `gpui-client` (`fetch-gpui.sh` + `cargo check --locked --workspace --all-targets` of
every client crate committed so far, and the capture build). Two things had to be fixed for the first client
run: patches arrived with CRLF on a Windows checkout (`.gitattributes` now pins LF under `gpui-client/`), and
the attachment tests read git-ignored `*.key` / `*.crt` files (they generate their certificate now).

### Stream playback (`crates/decibell-video`, 2026-11-26)

`tools/streamgen/measure.sh <label> 30 <streams>`: `decibell --screen video-test` (release, 1280×800 window on
the 280 Hz display; focus makes no difference: the main window has no inactive frame cap) fed from
`vendor/streams` at the streams' timing, 5 s settle, 30 s sampled by `tools/perf/measure.sh`. Streams are
NVENC with the streamer's settings: 1080p60 at 10 Mbit/s, 1440p120 at 20, 2160p60 at 30. "main" is the four
threads named `decibell` (the main thread and GPUI's helpers), "decoder" the `decibell-video-<n>` threads
(Linux shows the name cut to 15 characters); the feeder thread standing in for the core's receive thread
(0.1–0.3 % of a core, one wakeup per frame) is in the CPU total. "replaced" = published but overtaken by the
next picture before a frame was drawn; "dropped" = shed by the hub.

**The machine was never quiet** (other agents building): two series, the load average at the start of each
run in the last column. CPU moves by a factor of two between runs of the same scenario (4.7–7.3 % for one
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
- **Allocations** (`tools/streamgen/malloc-count.c`, 1080p60 HEVC): our Rust code allocates nothing per
  frame (`cargo test -p decibell-video --test alloc`: 0 over 100 steady-state frames, all three codecs); the
  decoder thread sees 38 `malloc` calls per frame on the hardware path and 16 on the software path, all inside
  FFmpeg and the driver; the main thread 285 per presented frame, inside GPUI's renderer and wgpu (each
  picture is one draw of the retained scene). That last number is where a stream's CPU goes ("main" is
  about two thirds of every hardware row) and is the next thing to cut (`docs/video-design.md` §12.6).
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
- **Modes**: the self-tests also pass with `DECIBELL_VIDEO_SHARED_QUEUE=1` (the single-queue mode AMD / Intel
  would use, forced on NVIDIA), with `DECIBELL_VIDEO_UPLOAD=wgpu`, and under XWayland.

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
- **Voice costs ~1,400 wakeups/s at rest in a channel, in the shared core** (numbers in §4): the voice socket is
  read with a 1 ms timeout loop instead of blocking, and the video receive thread runs for every voice session
  whether or not a stream is watched. Both clients pay it. To fix in the core (blocking receive with a wake on
  shutdown; start the video receiver with the first watched stream), with Electron's bar.
- **Physical key codes (must be fixed before the Keybinds screen and the focused-window hotkey path are done).**
  Bindings name keys by physical position (`KeyboardEvent.code`: `Semicolon`, `KeyQ`; `hotkeys/keys.rs`). GPUI's
  `Keystroke` carries only the character the key produced (`key`, `key_char`; `KeyDownEvent` has no code), so a
  recorder built on it assumes a US layout: on a German keyboard the key labelled Z would be saved as the
  physical Y position, and Turkish ş / ı / ğ keys could not be recorded or would be saved as the wrong key. The
  platform layers have the code at hand (Linux: the xkb keycode passed to `keystroke_from_xkb`; Windows: the
  scan code in the key message), so the fix is a small GPUI patch adding the physical code to key events, and
  the hotkeys store using it. A self-test must cover a non-US layout (e.g. an xkb `tr` / `de` keymap).
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

## 6. Needs owner

Things only the owner can do; nothing here blocks the rest.

- **Watch a stream in the Electron client built from this branch before it merges to `main`.** The core's video
  receive path was reworked for this client (frames reach the napi sink directly from the receive thread, one
  hop fewer; the Linux self-preview moves its buffer instead of copying; reassembly and FEC no longer copy
  fragments twice). Electron's bar and the allocation tests pass and frames are byte-identical in tests, but no
  one has watched a real stream through it yet. Also the decision list in `docs/electron-quirks.md`.
- **Decide the active-tile glow's behaviour.** In Electron it breathes forever (2.4 s opacity cycle) and costs
  ~15 % of a core on Linux while the window is visible. This client breathes it only while the window is
  focused, at a capped frame rate, presenting without re-rendering; unfocused it rests. Say if it should
  breathe unfocused too (it then draws frames at idle) or not at all.
