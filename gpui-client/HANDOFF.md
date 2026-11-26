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
| Core split step 2 (typed `CoreEvent`) | on the branch `1930905` + `ebb65d2`: 85 variants, `to_json()` pinned byte-for-byte by a golden test recorded from the old path; Linux bar passed (tsc 0, napi build with `index.d.ts` / `index.js` unchanged, 327 + 2 tests, smoke); Windows check running |
| Core split step 3 + 4 (commands → `decibell_core::api`, `lifecycle::init`, runtime handle) | in progress, worktree `.worktrees/core-api` (branch `wip/core-api`) |
| Core split step 5 (pooled receive path) | not started (with stream playback) |
| Local test stack (`tools/dev/`) | done `e245c8f`: central 127.0.0.1:18080 + Daily Gaming / Rustaceans / Design Lab on 127.0.0.11–13, 9 test accounts, 10k-message `#general`; `tools/dev/README.md` |
| Linux measurement tools | done `5285180`: `tools/perf/measure.sh`, `startup.sh`, `winctl.sh`, `install-size.sh` (README has the Linux section) |
| Electron baseline on this machine (Linux) | runner being built (`tools/perf/electron-local.sh`, `scenarios.sh`); the full run needs a quiet machine |
| GPUI survey + hello-world numbers | done `75e9008`: `docs/gpui-notes.md`; decisions in the design record §5 |
| Video research + prototype (`docs/video-design.md`) | in progress |
| Theme generator + assets | done `e846392`: `crates/decibell-theme` (162 tokens × 7 themes, 12 scaled, scopes), `tools/gen-theme`, `tools/fetch-assets.mjs`, `docs/css-component-rules.md` |
| Service crates (what Electron main did): attachments, link previews + GIFs | in progress (`crates/decibell-attachments`, `crates/decibell-unfurl`) |
| Skeleton (workspace, patched GPUI copy, fonts, window frame, capture + self-test harness) | in progress |
| Text pieces without GPUI: rich-text parser (`decibell-richtext`), highlight.js-compatible highlighter (`decibell-highlight`), KaTeX-compatible layout (`decibell-tex`) | in progress |
| Stream playback | not started |
| Chat (list, rich text, selection, composer, wheel) | not started |
| Sign-in, servers, channels, DMs + E2EE, voice + MLS | not started |
| Remaining parity rows | not started |
| Packaging, auto-update, crash reports | not started |

## 2. Building and running

_To be written with the skeleton._ The pin is `gpui-client/GPUI_PIN`; the Zed checkout lives in
`gpui-client/vendor/zed` (gitignored), patches in `gpui-client/patches/`.

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

**GPUI hello-world at the pin** (one window, custom title bar, 400 wrapping chat rows; `docs/gpui-notes.md` §13):
19.3 MB stripped binary (14.1 MB at `opt-level = "s"`); spawn → first frame 141–186 ms; Wayland idle: 0 frames,
0 main-thread wakeups, 0 ms CPU over 30 s; private memory 75 MB by default, 62 MB when wgpu doesn't load the GL
vendor libraries (59 MB at `opt-level = "s"`); GPU memory 56–60 MiB; under XWayland the main thread wakes 280×/s
until patched.

## 5. Open items

- **Every thread the client spawns gets a name.** `measure.sh` classes threads as own / driver / unnamed; NVIDIA's
  GL threads are unnamed, so an unnamed thread of ours would be counted with the driver's.
- **Pre-existing Electron bug found by the split's smoke test (for `main`, not fixed here)**:
  `electron/main/addon.ts` passes `e2eeLocalKey: loadOrCreateE2eeLocalKey()`, which is `null` when `safeStorage`
  is unavailable; napi-derive 2 accepts only `undefined` for an `Option` field, so `init` throws `Failed to
  convert JavaScript value Null into rust type String on InitOptions.e2eeLocalKey`. Fix: `?? undefined`.
- Core's `Cargo.lock` must be re-seeded when core's dependencies change (recipe in `CLAUDE.md`); CI checks
  `--locked`.

## 6. Needs owner

Things only the owner can do; nothing here blocks the rest.

- **Decide the active-tile glow's behaviour.** In Electron it breathes forever (2.4 s opacity cycle) and costs
  ~15 % of a core on Linux while the window is visible. This client breathes it only while the window is
  focused, at a capped frame rate, presenting without re-rendering; unfocused it rests. Say if it should
  breathe unfocused too (it then draws frames at idle) or not at all.
