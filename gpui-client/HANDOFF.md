# Decibell GPUI client — hand-off

The native desktop client: Rust + GPUI (Zed's UI framework) over the shared core `crates/decibell-core`.
The Electron client (`electron-client/`) keeps shipping until this one reaches parity and is the reference
for every behaviour. Owner decision 2026-11-26: GPUI, not Qt (`qt-client/` is the set-aside spike).

**Read first**: `CLAUDE.md` (every standing decision applies), the design record
`docs/superpowers/specs/2026-11-26-gpui-native-client-design.md`, then this file. Reference while working:
`gpui-client/docs/gpui-notes.md` (GPUI at the pin, with source lines), `gpui-client/docs/video-design.md`
(the stream path), `gpui-client/PARITY.md` (the checklist), `docs/FEATURES.md`, `ARCHITECTURE.md` §7.3,
`electron-client/HANDOFF.md` §5 (native gotchas apply here too).

## Resume here (state on 2026-11-27, written after a crash)

**What happened.** On the night of 2026-11-26 the machine ran out of memory (several agents each linking a
fat-LTO release binary or building the workspace's tests at once) and the session was killed. Nothing on the
branch was lost: `gpui-client` was pushed at `641bec1` with the Windows Native Check green on it. Five agents
were cut off with work uncommitted in their worktrees (§1a); they were resumed on 2026-11-27.

**The rule that came out of it (every agent, every session):** builds go through `gpui-client/tools/heavy.sh`.
- `tools/heavy.sh <command…>` for a heavy build: a release build (ship, development, `decibell-headless`),
  `cargo test --workspace`, any build or test of `decibell-app` (debug too), `tools/win-check.sh`. It takes one
  of three slots (the fourth waits) and sets `CARGO_BUILD_JOBS=8`.
- `tools/heavy.sh --light <command…>` for everything else that compiles (`cargo check`, clippy, one crate's
  tests): no slot.
- Both run the command in one systemd user slice with a memory ceiling shared by all builds (slowed above
  18 GB, the largest killed above 21 GB): a build that outgrows it fails alone and is rerun; the desktop and
  the terminal survive. Self-tests and the app itself run outside it, as many as the work needs.
- What the kernel log of that night showed (three kills: 17:47, 23:39, 02:00): 13 to 23 `rustc` processes
  holding 24 to 25 GB. Swap on this machine is zram (compressed RAM, not disk) and `/tmp` is tmpfs, so the
  machine has 31 GB, not 62: large files belong under `~/.cache/decibell-dev/`, never `/tmp`.
- The first answer to the crash was one build at a time with 4 jobs; that left 25 GB idle with five builds
  queued (owner, 2026-11-27: "a little bit too safe"), hence the slots and the ceiling.
- Commit after each finished item; nothing that matters lives only under `/tmp` (a reboot empties it: write
  measurements into this file or `docs/` as they are taken).

**Where the work stands.** Merged and verified on the branch head: the core split, the skeleton and themes,
stream playback (three hardware decoders + software), the chat list, sign-in, servers, channels, DMs with E2EE,
voice with MLS, streams and Go Live, calls, settings, server and channel settings, search, the platform services,
attachment playback (first pass), interface sounds / mic test / device events in the core, the Windows batch's
first checkpoint (§1c). Last full verification, on `0687ee3`: 758 unit tests pass, 24 of 25 key self-tests pass
one at a time (`media-idle` fails: with the media second pass), Electron's bar green, `tools/win-check.sh` clean.

**Merged since the crash (2026-11-27), each verified and pushed:** the Windows batch's second checkpoint (the
DirectWrite font-family fix, Go Live's source grid, every self-test group running in CI: `23f9adb`, Windows
run 45983095122 green, first Windows results in §1c); auto-away by idle time on X11 (`86c1b34`); packaging,
auto-update, crash reports and the ship flavour (`ba96e9e`: 690 unit tests, ship binary 80.4 MB passing
`packaging/check-ship.sh`, 14 of 14 release tests against loopback fakes; the first dry run of
`gpui-release.yml` started with that push).

**In flight (§1a has the worktrees):** cross-crate joins and self-test triage (`wip/integrate-d`); attachment
playback's second pass (`wip/media2`); the Windows batch's third round (`wip/windows`). Land each with
`tools/land.sh pick wip/<name>` (then `git merge --ff-only wip/land` in the main tree), run the `cargo check`
configurations (default, `capture`, `dev-local`, and `--no-default-features --features ship --release`),
`tools/win-check.sh`, the unit tests and the key self-tests (Electron's bar too when `crates/decibell-core`
changed), push `gpui-client`, read the Windows Native Check and the Native Client Release dry run.

**Self-tests go through `tools/selftest.sh <name>…`** (every agent, every session): one test window at a time on
the machine, each test in its own process with a fresh profile, the lock taken per test so sessions interleave.
Two test windows open at once take each other's focus and pointer: on 2026-11-27, with five agents testing side
by side, `input-typing`, `kit-idle`, `idle-frames` and `themes` each failed and passed on alternate runs (a focus
change is "3 drawn, 2 presented"), and `shell-signin` failed once ("timed out after 10 s waiting for Daily
Gaming's channels"). The owner's own pointer over a test window still disturbs a frame count: rerun once before
believing a failure. `--bin <path>` runs another build (the parity audit's frozen copy is
`~/.cache/decibell-dev/parity-root/target/release/decibell`).

**Then the final phase, on a quiet machine (nothing else building):**
1. `PARITY.md`: every `docs/FEATURES.md` §1–§9 row "done" with evidence or "needs owner" with what to test.
2. Electron's full baseline (`tools/perf/scenarios.sh electron all`) and the table of this client against it for
   every §3 target, with the ship build (idle RAM first; the driver's share reported beside it, per `CLAUDE.md`).
3. Captures of the main screens in all seven themes beside Electron's.
4. Two clean full self-test runs; Electron's bar, both `cargo test`s, the Windows Native Check, printed.
5. The "Needs owner" list (§6), with the open decisions below.

**Open owner decisions (collect, don't wait on them):** macOS (Electron ships it, the native client has none);
how Electron users move to the native client and its update path; the second-device half of the leave race
(needs a server change); the encoder's 10 s keyframe interval and 133 ms rate buffer; a "Block" entry in the
user menu; quirks #12 / #13 (`docs/electron-quirks.md`); a channel switch stops attachment playback (FEATURES
says it survives, Electron's code stops it); "7 Nov 2026" against Qt's "7 11 2026"; spellcheck and zoom (not
built); click-audit entry 11; the listening and by-eye checks; the Windows hardware checks
(`docs/windows-owner-checks.md`); a real X11 session.

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
| Chat content (attachments, link / invite embeds, image viewer, Transfers panel: `decibell-chat-content`) | merged (15 commits ending `d533496`): the picture pipeline (ThumbHash placeholder → decoded at the drawn size off the UI thread, byte-bounded caches, GIFs animate only while visible), attachment cards, embeds, invite cards, the viewer, Transfers; rows equal Electron's heights with content in place (`content-rows`); its ten `content-*` self-tests passed on its branch. Open: Copy image on Linux (GPUI patch, `wip/platform`), inline video / audio playback (`wip/media`) |
| Composer, emoji / GIF pickers, code / math panels, inline editor (`decibell-composer`) | merged (nine commits ending `9e98484`'s pick): typing costs one frame per key; picker opens in 42 ms and draws nothing at rest. Open with `wip/integrate-c`: the composer in the DM pane, send pacing under latency (`composer-burst`), type-to-focus at the root; `composer-typing-cost` needs GPUI patch 0014 (merged: re-run) |
| DMs, friends, profile popup / modal, custom status, user menu (`decibell-home`) | merged (five commits): its ten self-tests passed on its branch (49 / 49 in its full run). Open with `wip/integrate-c`: the owner's DM read rule (CLAUDE.md), one user menu with the voice entries, the emoji picker in the status dialog |
| Server Settings, Channel Settings, picture editor (`decibell-server-settings`, `decibell-picture-editor`) | merged (five commits ending `4fae600`'s pick): every tab, saves settling on the echo, the leave guard, the editor (EXIF upright); its eleven self-tests passed on its branch. Open: the avatar caller in Settings → Account (`PictureEvent::ChangeAvatar` → `PictureEditor`), Alt+arrow role reordering |
| Channel header + message search (`decibell-search`) | merged (ten commits ending `c90684e`'s pick): header with the E2EE badge, search box with chips / suggestions / date picker, results panel with Jump; the grammar equals the renderer's TypeScript on 108 cases; 9 of its 11 self-tests passed on its branch (`search-typing-cost` needs patch 0014: re-run; `search-idle` is sensitive to the shared stack) |
| Servers: browse / join, invites, server menu, channel create / reorder, welcome banner (`decibell-servers`) | merged (eleven commits ending `34df720`): browse grid, join by link / code / address, the deep-link join dialog (`decibell_servers::open_invite_link(url, cx)`), invites and invite-friends (the friend list never goes to a community), create dialog, drag reorder with the Sync / Keep / Cancel prompt, welcome banner through the chat pane's welcome slot; its nine self-tests passed on its branch. Open with `wip/integrate-d`: the shell's menu and leave confirm in the chrome palette |
| Platform: Copy image on Linux and the keyring label (GPUI patches from 0015), dev builds off the real profile, single instance, `decibell://` arguments, `--hotkey=`, attention, taskbar progress, window state (`decibell-platform`) | in progress, worktree `.worktrees/platform` (`wip/platform`) |
| Media playback of attachments (`decibell-media`, timed streams in `decibell-video`, `decibell-chat-content/src/playback.rs`) | first pass merged (seven commits ending `aee6dce`): engine with A/V sync to the audio clock (0.14 ms max over 60 s), every audio transition ramped with sample-exact tests, cards at rest cost nothing, H.264 / HEVC / AV1 on the GPU, VP8 / VP9 in software; `media-*` self-tests passed on its branch. **Second pass in `wip/media2`**: rotation metadata (phone videos draw sideways today), 10-bit, VP9 on the GPU, the missing control pieces and last-frame poster, the sealed-channel live test, 12 MB retained after a 1080p60 clip, the Electron comparison, captures |
| Core audio: interface sounds, the mic test outside voice, device changes without polling, the click audit's fixes | in progress, worktree `.worktrees/core-audio` (`wip/core-audio`) |
| Conversations joined (DM read rule, composer in DMs, one user menu, pacing, type-to-focus, regional sweep, headless `register` / `login-as`) | merged (nine commits ending `269a573`'s pick): the owner's DM read rule with `dm-read-rules` (four cases) passing on its branch; `MessageList` tells `AtBottom`; send pacing spends its token when the send returns (`composer-burst` in a debug build); KaTeX italic faces marked (formulas drew slanted); one midnight timer per list re-labels. Open with `wip/integrate-d`: type-to-focus against the root that holds focus and bare-key hotkeys |
| Cross-crate joins and the self-test suite as one run (`wip/integrate-d`): type-to-compose vs hotkeys, `render-cost`, the avatar editor in Settings → Account, kit layering inside modals, the shell's menus in the chrome palette, legacy refusal rows, the regional sweep of every crate, two clean full self-test runs | in progress, worktree `.worktrees/integrate-d` |
| Regional formats (`decibell_model::regional`: dates, times of day and counts follow the system's settings; English words) | done `1d2e9e1` (owner decision 2026-11-26 in CLAUDE.md): chat stamps, day dividers, member counts, ban expiry; 42 stamp cases equal the renderer's under en-US; self-test `regional` reads the drawn labels under four environments; Windows path compiled only. **Sweep each crate that merges later** with the grep in CONVENTIONS "Regional formats" |
| Video crate (`crates/decibell-video`, GPUI patches 0005 / 0006, `tools/build-ffmpeg.sh`, `tools/streamgen/`) | done `a7d5232`: Vulkan Video decode of H.264 / HEVC / AV1 on GPUI's device, software fallback, device-loss recovery, `VideoView`; self-tests `video-hw`, `video-idle`, `video-two`, `video-sw`, `video-recover`, `video-switch`; numbers in §4 "Stream playback". Not yet wired into the app's registry / boot on the branch (the lines are in the main tree's working files with the shell work) |
| Video, part 2: NVDEC and VA-API hardware fallbacks, decoder order and failure handling, a capability probe that really decodes, retained frames in the renderer (GPUI patch 0007) | done `7266df6`..`825cb99`: Vulkan Video, then NVDEC on NVIDIA / VA-API elsewhere, then software; `DECIBELL_VIDEO_DECODER` forces one; all three hardware decoders ran here for H.264 / HEVC / AV1 and match software decode pixel for pixel; 17 self-tests passed one at a time on the merged tree (load 4–11). AMD / Intel unrun: `docs/video-owner-checks.md`. Open: the probe is not called by the app yet, queue depth under a starved decoder thread, both with `wip/integrate-b` |
| Core: voice / media plane without polling | done `344068e`..`5840094` (Electron's bar passed on the merged tree: 419 core tests; Windows check run 45855500083 green): UDP receive loops block instead of polling at 1 ms, the video receive thread exists only while a stream is watched or sent, a talking peer allocates nothing per packet (PCM sample-identical in tests), `FrameBuf` keeps FFmpeg's padding, system idle as an event on Wayland. **No audio stream or processing is paused** (owner rule). In a voice channel alone and muted: 1,380 → 766 wakeups/s; what remains is the device clock and the 5 ms audio tick |
| Encoder quality (NVENC HEVC bitrate undershoot, per-codec tuning, colour signalling) | done on the branch (six commits ending `ea92ab8`): a first pass for NVENC (HEVC 88 → 99 % of target, text +5–8 dB), no held frames (68 → 3.4 ms call-to-packet at 720p30), H.264 without filler, AV1 on target, a 10 s keyframe interval on every backend, a complete colour description; quality bench + stream contract tests; core tests 436 passed. Windows code compiled only; AMF / Quick Sync / VAAPI quality unrun (`crates/decibell-core/tools/verify-encoders.md`, Quality) |
| Encoders on every vendor + software fallback (core) | done `4784c37`..`2e8bd1b`: backends NVENC / AMF / Quick Sync / VAAPI / software (SVT-AV1) behind a probe that really encodes, fallthrough at stream start, `DECIBELL_ENCODER` override, harness; ran here: NVENC (3 codecs) and software. **The software fallback is in the client's build** (`12c9115`, Windows `8a2a302`): `tools/build-ffmpeg.sh` builds SVT-AV1 4.2.0 into `vendor/ffmpeg` (rebuilt and swapped in on this machine; `scale_vaapi` too), `crates/decibell-app/build.rs` refuses an FFmpeg without `libsvtav1` and the software decoders, self-test `encoder-software` and `decibell-headless dump codecs` show it (`DECIBELL_ENCODER=software` → AV1 on `software`); Windows: vcpkg overlay ports `tools/vcpkg-overlay` (the pinned FFmpeg 8.0.1 + upstream's `svt-av1` feature + FFmpeg's own `libsvtav1.c` fix for SVT-AV1 4.x, SVT-AV1 4.2.0), installed by the Windows job's `gpui-client` step, **never built on Windows yet** (the first CI run is the check; checked on Linux: 8.0.1 + the fix + 4.2.0 passes the core's software tests); VAAPI / Quick Sync / AMF compile-checked with one-command owner checks (`crates/decibell-core/tools/verify-encoders.md`); Electron's bar passed (403 core tests) and Windows check run 45845969044 green; design `docs/superpowers/specs/2026-11-26-encoders-every-vendor-design.md` |
| Shell (`decibell-common`, `decibell-shell`, app boot with core + model) | done `f4ac4fe`: sign-in / register pages, server bar, channel sidebar with voice rosters, user panel, member list, friends frame, pane / overlay / menu registration (CONVENTIONS "Shell"); self-tests `shell-signin`, `-navigate`, `-members`, `-idle`, `-signout`, `-no-keyring`; captures within 0.3–1.2 % of Electron's; numbers in §4 "Shell". Left: channel drag-to-reorder, panes of the other features |
| Voice view, stream watching, mini-player, Go Live, calls (`decibell-voice`) + the stream bot for measurements | done (eight commits ending `044123b`, + `63515a7`): stage, tiles, focus / theater / fullscreen, dock, mini-player, Go Live with presets and the probed encoders, call ring and stage (`Slot::DmCallStage`), the app's video glue (frames, keyframe requests, frame-sink interest); `tools/perf/streambot.sh`, scenarios `watch-1` / `watch-2`; all `voice-*` and `call-ring` self-tests pass on the merged tree. Open with `wip/integrate-b`: the stall state, cursor hiding in fullscreen, two-stream measurement (needs a second keyed streamer on the stack), nested cached views |
| Settings window (all tabs) + rename, change-password and passphrase dialogs (`decibell-settings`) | done (nine commits ending `ff7c600`) and integrated: real text fields (the kit's `TextField` `Dialog` / `Compact` looks), the kit's button variants, the recorder on physical codes (German / Turkish layouts record the key pressed), the focused hotkey fallback in the app's root, regional formats, `actions::auth::log_out`, the attachment caps from the model; self-tests `settings-*`, `kit-confirm`, `hotkeys-focused`; captures within 0.13–2.4 % of Electron's (dialogs 0.23–0.70 %). Left: the mic test outside voice (a core API), the avatar editor (`PictureEvent::ChangeAvatar`), the updater |
| Core follow-ups | done `2774316` (pooled frame buffers: 0 allocations per received video frame with the pool on, measured; Electron 1–2 instead of 4–8), `3ac2ebc` (`net::tls::exact_connector`), `502be85` (no `aws-lc-rs`), `9f0117e` (zbus runtime as a feature: `zbus-tokio` default for the addon, `zbus-async-io` for this client); Linux bar passed on the merged tree (tsc 0, napi build unchanged, 361 core tests, both smokes); Windows check running |
| Code highlighting (`crates/decibell-highlight`) | done `b636cb4`: highlight.js's engine + the renderer's 26 grammars, 0 mismatches against lowlight over 138,117 cases; +527 KB of binary; vendored patched `regress` (step budget) |
| Rich-text grammar (`crates/decibell-richtext`) | done `497ab53`: 0 mismatches against the renderer over 120,570 committed + 1.6M stress cases |
| Math (`crates/decibell-tex`) | done `aa0a020`: KaTeX 0.18.4 ported; markup byte-identical on 5,070 / 5,072 formulas, glyphs within 0.05 px of Chromium; +1.1 MB binary |
| Attachments (`crates/decibell-attachments`), link previews + GIF search (`crates/decibell-unfurl`) | done `08412dc`, `d835d32` (vectors from the real TS; stack tests) |
| Wheel / key scrolling spec | done `0777f91`: `docs/wheel-scroll.md` (120 px per notch, ease-in-out bezier, cc's duration + retarget rules, 11 vectors) |
| Stream playback: design + prototype | done `8c990eb`: `docs/video-design.md`, `patches/0001-gpui-external-surfaces.patch`; the real crate (`decibell-video`) not started |
| Stream playback (`crates/decibell-video`) | built, tested and measured on Linux (2026-11-26): Vulkan Video on GPUI's device, software fallback, `VideoHub` / `VideoView`, device-loss recovery, patches `0005` + `0006`, `tools/build-ffmpeg.sh`, `tools/streamgen/`; six self-tests (`video-*`); numbers in §4. Wired to the model and the voice view since (the row "Stream playback, joined to the voice view" below). Windows: D3D11VA + software backend and GPUI patch `0020` written, not run on a GPU (§1c, `docs/video-design.md` §8) |
| Stream playback, joined to the voice view (`wip/integrate-b`) | done on Linux / NVIDIA (2026-11-26): the capability probe runs after the first frame and is what the client advertises, uses and shows (Settings → Codecs names each codec's decoder and what was given up); encoder caps with backend, label and limits in the model, one loader; the decoder queue holds 100 ms of frames (a deliberate difference from Electron: `docs/electron-quirks.md` #14); "Reconnecting…" over a stalled stream without a timer; the cursor hidden in fullscreen (GPUI patch `0013`); clean cached views nested in a dirty one are replayed (GPUI patch `0014`); decoder threads stopped before the process exits; `perf5` on the local stack for two-stream measurements. Self-tests `video-starve`, `video-stall`, `voice-stall`, `render-cost`; numbers in §4 "Stream playback in the app". AMD / Intel and Windows unrun (§6) |
| Attachment playback (`crates/decibell-media`, `decibell-chat-content/src/playback.rs`; `wip/media`) | built and tested on Linux / NVIDIA (2026-11-26), **not heard by anyone** (§6): video and audio attachments play in their cards; the engine demuxes through a custom AVIO over `MediaReader`, decodes video on `decibell-video`'s decoders as a timed stream (Vulkan Video here; VP8 / VP9 in software) and audio into its own output stream on the default device, synced to the audio clock, every transition a 10 ms ramp. `tools/build-ffmpeg.sh` gained the demuxers and decoders of the codec matrix (**`vendor/ffmpeg` was rebuilt and swapped; the old prefix is `vendor/ffmpeg.old`; every target directory needs `cargo clean -p ffmpeg-sys-next`**). Tests: 21 matrix files, sync, seek, pops, allocations, sealed reader; self-tests `media-idle`, `media-video-play`, `media-audio-play`, `media-persist` (fixture, memory sources) and `media-live` (a clip uploaded to the local stack's `#off-topic` as `perf2`, played from the attachment service's range reader, deleted again). Design, the renderer's rules and the list of what is missing: `docs/media-design.md`; numbers in §4 "Attachment playback". Not run: a sealed attachment against the stack, the Electron comparison, captures, AMD / Intel, Windows (§5) |
| Chat (list, rich text, selection, composer, wheel) | not started |
| Sign-in, servers, channels, DMs + E2EE, voice + MLS | not started |
| Remaining parity rows | not started |
| Desktop integration (`crates/decibell-platform`, GPUI patches `0015`–`0019`; branch `wip/platform`) | done on Linux (2026-11-26), Windows written for real and compiled only, every job of Electron's main process accounted for in §1b: one instance per profile (a second start hands its `decibell://` link or `--hotkey` over in ~20 ms and exits), links as the pending invite before sign-in, `--hotkey` from a second start, the taskbar flash on an incoming call (not under DND), launcher progress, the upload quit guard and paused downloads at quit, window state, sleep / lock → Away, Copy image on Wayland and X11 (patch `0015`), the keyring label `Decibell` (`0016`), development builds off the shipped profile. Self-tests `content-copy-image`, `single-instance`, `deep-link-arg`, `hotkey-cli`, `attention`, `taskbar-progress`, `keyring-label`, `window-state`, `profile-guard` (results: §4 "Desktop integration"). Windows: §5 "Windows batch". By hand: §6 |
| Packaging, auto-update, crash reports (`crates/decibell-release`, `packaging/`, `.github/workflows/gpui-release.yml`, `aur/decibell-native-bin`; merged `ba96e9e`: on the merged tree the ship binary is 80.4 MB, `check-ship.sh` passes and refuses the development binary, 14 of 14 release tests pass) | done on Linux (2026-11-26), Windows compiled (type-checked with `tools/win-check.sh`) and checked by the release workflow's dry run: the **ship build** (`--no-default-features --features ship`: no screens, self-tests, fixtures, probes or `DECIBELL_CENTRAL`; `packaging/check-ship.sh`), one version from `electron-client/package.json`, tarball / AppImage / `.deb` / AUR recipe, the NSIS installer (per user, the `decibell://` keys, DLLs found by walking the imports, the shortcut's AppUserModelID), the executable's icon / version / manifest, `THIRD_PARTY_LICENSES.md` with the LGPL answer, the updater (signed fixed-channel feed, SHA-512, resume, trial start + rollback) and crash reports (scrubbed Sentry envelopes, native crash records). Everything in `PACKAGING.md`; the owner's list in its §11 |

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
| `.worktrees/integrate-d` / `wip/integrate-d` | cross-crate joins and self-test triage (edits many crates; not `decibell-video`, the media cards or `copy_image`) |
| `.worktrees/windows` / `wip/windows` | the Windows batch, in rounds (§1c): checkpoints 1 and 2 merged; round 3 = the six self-tests and two `decibell-text` unit tests that fail on the Windows runner (run 45983095122: `chat-history`, `chat-switch`, `input-typing`, `input-ime`, `input-atoms`, `render-cost`; `input_vectors`' word stops), the groups skipped there, the rest of its gap table. After each checkpoint: merge, push, send it the run's logs from `origin/ci/win-gpui-check-log` |
| `.worktrees/media2` / `wip/media2` | attachment playback, second pass (rotation, 10-bit, VP9 hardware, UI pieces, sealed live test, retained memory, Electron comparison, captures) |
| `.worktrees/parity-a`, `-b`, `-c` / `wip/parity-*` | the parity audit (FEATURES §1–§3, §4–§6, §7–§9): each fills its sections of `PARITY.md` (columns: Status, Evidence / what is left, Windows) and writes `docs/parity-audit-<x>.md`, the gap list with sizes; they change no product code and run the frozen build in `~/.cache/decibell-dev/parity-root` |
| `.worktrees/land` / `wip/land` | the architect's landing tree, driven by `tools/land.sh` (pick an agent's commits, resolve, then fast-forward the main tree): empty of unmerged work when this was written |

When landing work whose files overlap other agents' uncommitted edits in the main tree: build the commit in a
clean worktree, then move the branch with `git update-ref refs/heads/gpui-client <new> <old> && git reset -q`
(a mixed reset: it never touches working files).

## 1b. What Electron's main process did, and who has it here

Every job of `electron-client/electron/main/*.ts` and the preload, with where this client has it (2026-11-26, the
platform work; "this work" = `crates/decibell-platform`, the GPUI patches `0015`–`0019`, the hooks listed in
CONVENTIONS "Platform").

| Main process (file) | What it did | Here | State / owner |
|---|---|---|---|
| `index.ts` `requestSingleInstanceLock`, `second-instance` | one instance per user-data dir; a second start restores and focuses the window and forwards its link | `decibell_platform::instance` (a lock per profile; a Unix socket / a named pipe on Windows, served by a thread blocked in `accept` / `ConnectNamedPipe`), `apply`: the window comes forward (Wayland: with the second start's `XDG_ACTIVATION_TOKEN`, patch `0018`; Windows: `AllowSetForegroundWindow` from the second start) | this work |
| `index.ts` + `hotkeys.ts` `parseHotkeyArg`, `injectHotkey("cli")` | `decibell --hotkey=<action>[:down/:up]` acts in the running instance without focusing it; with nothing running it exits | `launch::parse_hotkey`, `apply` → `actions::hotkeys::inject_cli`; boot exits 0 with a message when nothing runs. Works on Windows too, where it isn't needed (press actions are `RegisterHotKey` in the core) | this work |
| `index.ts` `setAsDefaultProtocolClient("decibell")` at every start | registers (and re-points) the URL handler | `assets/linux/decibell.desktop` (`MimeType=x-scheme-handler/decibell`), installed by the packages; the app never registers or re-points anything (Electron's re-pointing is a bug: §4) | done (`PACKAGING.md` §2, §3): Linux packages' desktop entry; Windows: the installer's `HKCU\Software\Classes\decibell` |
| `index.ts` `isValidDeepLink`, `pendingDeepLinks`, `deep_link_received` | validated links buffered until the renderer listens; before sign-in kept as the pending invite | `launch::is_valid_deep_link`; `apply` → `actions::servers::open_invite_link` → `ServersStore::pending_invite` (kept until the main layout shows it) | this work; the join dialog: `decibell-servers` (its `join` overlay already reads `pending_invite`) |
| `index.ts` `open-url` (macOS) | links on macOS | — | no macOS target |
| `index.ts` `LIBVA_DRIVER_NAME` / `NVD_BACKEND` for NVIDIA | Chromium's VA-API on NVIDIA | the video crate probes its decoders itself | video (done) |
| `index.ts` Chromium switches (HEVC, VA-API, Ozone, ANGLE, sandboxes, logging) | Chromium's configuration | — | no Chromium |
| `index.ts` `loadSentryBootConfig`, `sentry.ts` | crash reports, install id | `decibell_release::crash` (gates, install id, panic hook, native crash records, scrubbed envelopes) started by `decibell-app/src/crash.rs` | done (`PACKAGING.md` §7) |
| `index.ts` `registerCustomSchemes`, `protocol.ts` | `decibell-attachment:` / `decibell-file:` for the renderer | — (the attachments crate fetches; files are read directly) | done |
| `index.ts` `createWindow` | 1280 × 800, at least 800 × 600, frameless, dark background, icon, shown when ready | `decibell-app/src/window.rs` (size, minimum, our title bar, app id, the X11 icon `_NET_WM_ICON` from the 64 px PNG) + `window_state` (the last size, place where allowed, maximised); Windows: the icon, version and manifest resources (`build/winres.rs`) and the AppUserModelID | done |
| `index.ts` `dumpGpuInfo` | GPU diagnostics in the log | the video crate's probe | video (done) |
| `index.ts` `kickoffInitialCheck`, `update.ts`, `ipc.ts` update handlers, `openReleasePage` | auto-update | `decibell_release::{update, feed, apply}` + `decibell-app/src/updater.rs` (About's hooks, the chip's `RestartToUpdate`, the one-shot first check) | done (`PACKAGING.md` §6) |
| `index.ts` `stopOrphanedShare` | stop a native share a reloaded / crashed renderer left running | — (one process) | — |
| `window.ts` `hardenNavigation` | no navigation, `window.open` → the browser (https only), no `<webview>`, back / forward swallowed | — (no web content); links open through `cx.open_url` (`decibell_unfurl::links::external_link_target`) | chat (done) |
| `index.ts` `capture:listSources` / `setNextSource`, `setDisplayMediaRequestHandler` | the Windows source grid, the portal on Linux | the core's capture, Go Live | voice / core |
| `index.ts` `setCertificateVerifyProc`, `certificate-error` | community certificate pins for attachment HTTPS | the attachments crate's pinned TLS | done |
| `index.ts` `sweepStale` (5 min) | file-registry GC | — (no registry) | — |
| `mediaServer.ts` | loopback HTTP for `<video>` / `<audio>` | — (the player reads directly) | player (another crate) |
| `index.ts` `before-quit` | ask about uploads, pause downloads, stop the update check, shut the addon down (3 s), exit | `quit_guard` (asks, `on_app_quit` pauses downloads through `DownloadControl::pause_all_for_quit`), `boot.rs` (core shutdown, 3 s) | this work + done |
| `index.ts` `window-all-closed`, `activate` (macOS) | quit with the last window | GPUI `QuitMode::Default` | done |
| `window.ts` window controls (minimise, maximise, toggle, close, `isMaximized`, `setTitle`, `setFullscreen`), `attachWindowEvents` | the title bar's buttons and state | `decibell_ui::TitleBar`, `Root`, the voice view's fullscreen | done; the close button now goes through `decibell_ui::request_window_close` (the quit guard) |
| `window.ts` `setTransferProgress` → `setProgressBar` | launcher / taskbar progress of the transfers | `progress` (Linux `com.canonical.Unity.LauncherEntry`, Windows `ITaskbarList3`) | this work |
| `window.ts` `flash` → `flashFrame`, dock bounce | attention on an incoming call (not in DND) | `attention` + patch `0017` (Wayland) | this work |
| `window.ts` `close` → `confirmLosingUploads` | closing with uploads in flight asks | `quit_guard` | this work |
| `clipboard.ts` `copyAttachment`, `writeImage`, `writeText` | copy a picture (as PNG) / a link | `decibell-chat-content/src/actions.rs` + patches `0015` (Linux pictures) and `0019` (Windows `CF_DIB`) | this work (pictures); text: done |
| `dialog.ts` open / save | file pickers (with filters; a save name made unique in Downloads) | `cx.prompt_for_paths` (composer), `cx.prompt_for_new_path` (downloads); GPUI's portal dialogs take no file-type filters | composer / chat-content |
| `fs.ts`, `fileRegistry.ts`, `writeApproval.ts`, `noStore.ts` | the renderer's file access, sandboxed | — (native code reads and writes itself) | — |
| `netFetch.ts`, `attachment*.ts`, `downloadPacer.ts`, `downloads.ts` | attachment fetch / crypto / downloads / caps | `decibell-attachments`, chat-content's Transfers | done |
| `linkPreview.ts`, `publicNet.ts`, `gifs.ts` | link previews, GIF search | `decibell-unfurl`, composer | done |
| `hotkeys.ts` macOS accelerators | press actions on macOS | — | no macOS target |
| `hotkeys.ts` `desktop`, `openMouseSettings`, `launchCommand` | the Keybinds tab's desktop help | `decibell-settings` (keybinds tab) | done |
| `idle.ts` `getSystemIdleState` | auto-away readings | `decibell_platform::idle::source(model, cx)`, by the display server GPUI runs on: Wayland / GNOME the core's `system_idle`; **X11** (and XWayland when started with `WAYLAND_DISPLAY` empty) the X server's `IDLETIME` counter through two SYNC alarms on a connection of its own (`idle/x11.rs`: one thread blocked in `wait_for_event`, woken only when the threshold is reached and at the next input, existing only while auto-away is on; MIT-SCREEN-SAVER `QueryInfo` per reading on a server without SYNC `IDLETIME`; nothing selects input events); Windows `GetLastInputInfo` (exact idle time) | this work (X11 checked in a private Xvfb, `idle-x11`; a real X11 session: §6) |
| `idle.ts` `lock-screen` / `unlock-screen` / `suspend` / `resume` | Away at once on lock / sleep | `session` (sleep / wake: GPUI's logind and `WM_POWERBROADCAST` hooks; lock: `ActiveChanged` of `org.freedesktop.ScreenSaver` / `org.gnome.ScreenSaver` on Linux, `WM_WTSSESSION_CHANGE` on Windows) → `AutoAway::set_locked` | this work |
| `e2eeLocalKey.ts` `safeStorage` | the at-rest key in the OS keychain | `decibell_model::local_key::KeychainKey` (GPUI credentials) + patch `0016` (our label) | done + this work |
| `addon.ts` | load / init / buses / shutdown of the addon | `decibell-model` | done |
| preload `webUtils.getPathForFile` | dropped files' paths | GPUI's `ExternalPaths` | composer (done) |
| Chromium's spellchecker (squiggles in the composer; the renderer has no suggestion menu) | spelling | **not built, not small**: a checker (Hunspell / Nuspell dictionaries per language, shipped or the system's: there is no OS spelling service on Linux), the editor's wavy underline per misspelt range, re-checked as typed off the UI thread; suggestions would need a context menu the renderer never had | listed |
| Chromium's page zoom (the default menu's Ctrl + = / − / 0, if it reaches a frameless window: unverified) | zoom the whole UI | **not built, not small**: the theme already scales text and rows (`Theme::set_scale`, Settings → Appearance); a zoom would scale every token, icon, image box and the chat list's measured heights (`ChatStyle`), with the key bindings and a saved factor | listed |
| tray, autostart, power-save blocker, notifications | — | — | Electron had none (`FEATURES.md` §10) |
## 1c. Windows (the Windows batch, 2026-11-26)

The Windows half is written on Linux, where none of it can run. Three levels of evidence, in rising order; say
which one a claim rests on:

1. **Type-checked on Linux for `x86_64-pc-windows-msvc`**: `tools/win-check.sh` (the whole workspace with tests,
   `crates/decibell-core` and the patched `gpui_windows`, against the real `windows` crate; `--shaders` also
   compiles GPUI's HLSL with Wine's compiler). Run it before every push that touches `cfg(windows)` code: it
   answers in a minute what the workflow answers in half an hour.
2. **Compiled and partly run by the Windows Native Check** (`gpui-client` job, log branch
   `ci/win-gpui-check-log`): `cargo-check.log`; `shaders.log` (fxc, through a release check of gpui_windows);
   `unit-tests.log` (the crates without the core); `selftests.log` (the self-tests that need no server and no
   hardware codec, in a real window on the runner's WARP device); `idle.log` (`tools/perf/windows/row.ps1`
   against the built client). The run steps never fail the job.
3. **Run by the owner on real hardware**: `docs/windows-owner-checks.md`, in order. Nothing is at this level yet.

| Piece | Written | Level |
|---|---|---|
| Stream video: GPUI patch `0020` (surfaces in the Direct3D 11 renderer), `crates/decibell-video/src/windows/` (D3D11VA → software, slots, probe, device removal) | yes (`docs/video-design.md` §8) | 1; 2 pending the first run (`video-probe` on WARP covers software decode, upload, read-back and the surface draw; **hardware decode needs a GPU: owner**) |
| Idle: GPUI patch `0021` (the vsync thread parks; it and the main thread woke at the refresh rate at rest) | yes (`patches/README.md` §0021) | 1; 2 pending (`idle.log`'s thread table); the touchpad-gesture exception is the owner's to feel |
| Title bar: the client's caption buttons are the system's on Windows (snap layouts, restore) | yes (`decibell-ui/src/titlebar.rs`) | 1; owner |
| Measuring: `tools/perf/windows/row.ps1`, `install-size.ps1`, `-ProcessId` for `measure.ps1` / `threads.ps1`, the perf event (`--perf` on Windows) | yes (`tools/perf/README.md`) | 2 pending |
| Go Live: the screen / window grid with stills (`decibell-voice/src/sources/`, the dialog's "What to share") | yes: lists active displays (`screen:<EnumDisplayDevices index>:0`, the id the core resolves) and shareable windows (`window:<HWND>:0`; Chromium's filter), GDI stills and icons on a background thread, every 5 s while the dialog is open and never otherwise. The grid itself is platform-neutral and was looked at on Linux with `DECIBELL_GO_LIVE_SOURCES=fixture` (debug build, `--screen go-live`) | 1 for the Windows half; owner (checks §6) |
| Stream-audio app picker | nothing to write: the dialog calls the core's `list_stream_audio_apps` with the picked source on every platform, and the core's WASAPI list answers on Windows | owner (checks §6) |
| Fonts: the wide families renamed "Decibell Sans Plus" / "Max" (DirectWrite read "SemiExpanded" / "Expanded" as a stretch, so those families did not exist there: found by `boot` on the runner) | yes (`tools/gen-fonts.py`) | Linux `boot` passes; 2 pending |
| Auto-away, lock, single instance, attention, taskbar progress, window state, clipboard DIB, keyring name | the platform work (`crates/decibell-platform`, patches `0015`–`0019`) | owner (checks §7) |
| Executable identity (no console in the ship build, icon, AUMID) | the packaging work | — |

What the first run that executed anything showed (45900404424, WARP runner): fxc compiles the surface shader;
`boot` opens a window on a real Direct3D 11.1 device with patch `0020`'s pipeline (first frame 784 ms, debug
build); with patch `0021` the gallery at rest reads 0 context switches / s and 0 frames over 20 s
(`row.ps1`, 22.6 MB private working set); 290 unit tests pass, 2 failed and are fixed (a Linux expectation in
`decibell-text`, a 2 s deadline against Windows' slow refused connect in `decibell-unfurl`). Only `boot` ran of
the self-tests (the step stopped at its failure: fixed), so `video-probe` and the rest are still to be read.

**Which self-tests the Windows runner executes** (run 45983095122 and the groups added after it):

- *Passed there*: `boot`, `idle-frames`, `themes`, `window-frame`, `video-probe`, the seven `kit-*`, `input-undo`,
  `input-bytes-cap`, `input-select-filter`, `input-caret-idle`, `chat-resize`, `chat-selection`, `chat-rows`,
  `chat-idle`, `chat-hover`, `encoder-software`.
- *Failed there, tests corrected* (the product was right; each expectation now comes from Chromium's Windows
  behaviour, with its source in the test): `input-typing` and the `input_vectors` unit tests (Ctrl+Right ends
  past the spaces on Windows: Blink's `ShouldSkipSpaceWhenMovingRight` / `SkipWhitespaceIfNeeded`), `input-ime`
  (it placed its caret with that same key), `chat-history` (a wheel notch is 100 px on Windows:
  `kScrollbarPixelsPerLine = 100 / 3`).
- *Failed there, cause not known yet*: `chat-switch` (the first frame after returning to a channel had the row
  421 px off), `input-atoms` (a click did not focus the composer field), `render-cost` (a case drew no frames;
  it also loses a case on Linux). None reproduces on Linux, also not in a 1024 × 720 window (the runner's
  size, tried). Each now prints what is needed to tell: the list's positions over the following frames and
  its scroll range across the switch; the click point, the field's bounds, the window's size, scale and
  activation; the name of the case. The runner's desktop is also raised to 1920 × 1080 first.
- *Added, first run pending*: `chat-scroll-spec`, `input-clipboard`, `input-password`, `hotkeys-focused`,
  `hotkeys-layout`, `join-status-picker`, `join-dm-composer`, `picture-editor`, `settings-narrow`,
  `server-settings-idle`, `content-scroll-fixture`, `content-rows`, and the platform crate's `profile-guard`,
  `single-instance`, `deep-link-arg`, `hotkey-cli`, `attention`, `taskbar-progress`, `keyring-label`,
  `window-state` (whether those need more than a window is what the run will say).
- *Cannot run there*: everything that signs in (the local stack is Linux servers: `shell-*`, `chat-live`,
  `voice-*`, `call-ring`, `settings-*`, `composer-*`, the pickers, `dm-*`, `friends-*`, `profile-popup`,
  `status-custom`, `user-menu-moderation`, `home-idle`, the live `content-*` and `media-*`, the stack half of
  `server-settings`, `search-*`, `servers-*`, the invite and channel tests); `regional` (glibc locales, by
  design) and `idle-x11`, `video-nvdec`, `video-vaapi`, `video-fastpath` (Linux only).
- *Could run with some work*: the stream tests (`video-hw`, `-sw`, `-idle`, `-two`, `-switch`, `-order`,
  `-recover`, `-starve`, `-stall`) need `vendor/streams`, which is made with NVENC on Linux and not in the
  repository: a small software-encoded set committed or generated in the job would let the runner play real
  streams through the software path (hardware decode stays the owner's). `regional-read` needs only
  `--screen main-server` and would print what the runner's regional settings give.

Rules that came out of it:
- `decibell-video` names the renderer's surface types through `crate::surface` and splits on `cfg(vk)` (Linux),
  `cfg(d3d)` (Windows), `cfg(gpu)` (either), all set by its `build.rs`. A new decoder is a `DecodeBackend`
  variant with `built_here()`.
- A client crate that needs the Direct3D device or any type shared with GPUI uses `gpui_windows::windows`
  (the re-export), never its own `windows` version.
- gpui_windows' immediate context is shared between threads: hold `D3d11Devices::lock_context()` around any
  sequence of context calls made off the main thread.

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
- **The shipped binary is the `ship` flavour**: `cargo build --release -p decibell-app --no-default-features
  --features ship` (no screens, self-tests, fixtures, test hooks or `DECIBELL_CENTRAL`; the default
  `dev` feature has them). Packages, the release workflow and the updater: `PACKAGING.md`.

**Run**

```sh
target/release/decibell [--theme <id>] [--screen <name>] [--perf] [decibell://invite/<code>] [--hotkey=<action>]
```

Without `--screen` / `--selftest` this is the app: the state model and the core start (profile:
`$DECIBELL_USER_DATA_DIR`, else `~/.config/decibell` in a shipped build and **`~/.cache/decibell-dev/gpui-default`
in a development build** (`dev-local` or debug; `decibell_model::core::development_profile_dir`), which also refuses
`~/.config/decibell` when `DECIBELL_USER_DATA_DIR` names it (exit 2); the at-rest key in the keychain, in a file of the
profile for development builds), the window shows the sign-in page or, with a saved sign-in, the main layout
(`decibell-shell`). One instance per profile: a second start hands its `decibell://` link or `--hotkey` to the running
one and exits (`decibell-platform`; CONVENTIONS "Platform"). **A build without `dev-local` talks to the live
central**: for the local test stack build `--features dev-local` (honours `DECIBELL_CENTRAL`, adds `--login`; never
ship it) and use your own profile:

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
# Always a profile of its own: without one the app starts the core on the default profile (2026-11-26: a
# measurement like this created ~/.config/decibell; development builds now default to ~/.cache/decibell-dev/).
export DECIBELL_USER_DATA_DIR=$(mktemp -d)
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

### Test runs and the owner's desktop (2026-11-26)

- **The keyring.** Until `5d8e124` every run of the app with a throwaway profile stored its at-rest key in the
  desktop keyring (606 items in one afternoon, labelled `zed-github-account` by GPUI; the owner had them removed).
  A development build (`dev-local` or debug) now keeps the key in `<profile>/dev-local.key`
  (`decibell_model::local_key::FileKey`); only `DECIBELL_DEV_KEYCHAIN=real` reaches the keychain, and only the
  no-keyring self-test sets it, on its private bus. Never start the headless client with `--keychain` on the
  session bus. The label itself is fixed by a GPUI patch in `wip/platform`.
- **The local central's per-address limits.** 10 sign-ups per address, one more every 2 minutes
  (`src/server/abuse_limits.hpp`), likewise sign-in attempts and connections. A development build therefore
  connects to a loopback server from a loopback address of its own, one per process (`9b15e5a`, the core's
  `net/connection.rs`; `DECIBELL_DEV_SHARED_SOURCE` turns it off), as central's own test harness does.
- **Always pass `DECIBELL_USER_DATA_DIR`** to anything that starts the core: one run without it created the real
  profile directory `~/.config/decibell` (removed; `wip/platform` moves a development build's default profile
  under `~/.cache/decibell-dev/`).
- **Shared accounts and channels.** The voice self-tests use perf3 / perf4 and "Lounge"; the stream bots use perf4 /
  perf5 (perf5 was added by `tools/dev/local-stack.sh top-up`: "Daily Gaming" has 299 members now, which
  `shell-members` must not hard-code). Two runs at once interfere: rerun alone before believing a failure.
- **Focus and the pointer.** Test windows open on the owner's desktop: idle, hover and focus tests fail when another
  window takes the focus or the real pointer rests over the test window. Running the self-tests inside a private
  headless compositor (`dbus-run-session` + `kwin_wayland --virtual`, tried twice) hung without output and left
  the nested compositor holding the output pipe; if tried again, write output to a file, start it with `setsid`
  and stop it by PID. The final runs are to be taken with no other agent running.
- **The clipboard** was overwritten once by a test (a PNG); tests that write it must save and restore it.

### Self-tests need the headless client built beside the app

Several self-tests (`voice-*`, `call-ring`, the DM and settings tests with a second participant) start
`target/release/decibell-headless` as the other client. Build it whenever the model changes:
`cargo build --release -p decibell-model --features local-central --bin decibell-headless`. A stale one fails
those tests after a 60 s wait with "the peer never printed MARK ready" (seen 2026-11-26 after the voice merge).
A self-test that unlocks the E2EE identity must wait for the passphrase dialog (the settings crate's overlay)
to close before it clicks anything: the dialog takes every click while it is open and for its 300 ms fade.

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

**The ship build** (2026-11-27, `PACKAGING.md` §9: the `ship` flavour of this client at base `9605d9f`, stripped,
outside a package, at the sign-in page): install **77.1 MB** (Electron 417.0), first frame **159 ms**, idle
**70.1 MB** private (30.9 dirty, PSS 87.3), 0.012 % of a core, **0 own wakeups / s**, **0 frames / s** at rest; no
network at all without a saved sign-in. Size levers measured there (the ship flavour −7.0 MB, packed relocations
−2.6 MB, opt-level `s` −22 MB but the list's paint ~1.75×, thin LTO +17.5 MB): the release profile is unchanged.

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

**Windows compile status**: Windows Native Check run 45898166132 (`abffdab`) is green for both jobs, with the platform
work's Windows sides and GPUI patches `0015`–`0019`. The whole client also type-checks for `x86_64-pc-windows-msvc` **on
Linux** now: `gpui-client/tools/win-check.sh` (run it before every push that touches `cfg(windows)` code; it found the
three places where the Windows video backend and attachment playback met, before any CI run). Run 45900404424
(`704dff0`) is the first with the Windows batch's checkpoint 1 and the job's new run steps (shaders through fxc, unit
tests, the no-network self-tests on the runner's WARP device, `tools/perf/windows/row.ps1`): its logs are on
`ci/win-gpui-check-log`. See §1c for what each level of evidence means. Nothing has run on real Windows hardware.

### Desktop integration (`crates/decibell-platform`, 2026-11-26)

Release build with `dev-local`, KDE Plasma 6.8 (KWin 6.8.3) Wayland and XWayland, load average 6–20 (other agents
building). Every self-test runs the app as children with throwaway profiles on a private, empty session bus.

| Self-test | What it showed |
|---|---|
| `single-instance` | the second start on a profile exits 0 after **20 ms** (it hands over in < 1 ms and never starts GPUI); the first, unfocused, is marked by KWin as demanding attention on Wayland (no activation token from a shell; a launcher's token would focus it: §6) and is activated on X11 (`_NET_ACTIVE_WINDOW`); a second profile's instance runs beside the first |
| `deep-link-arg` | `decibell://invite/PLATFORMA1` on the first start is `ServersStore::pending_invite` before sign-in and still after it (as carol); a second start's link replaces it |
| `hotkey-cli` | `--hotkey=toggle_mute` with nothing running exits 0 in 20 ms without opening anything; with an instance, four second starts (`toggle_mute` ×2, `push_to_talk:down` / `:up`) exit in 20 ms each and the instance's voice actions run (`Play(Mute)`, `Play(Unmute)`) without raising the window |
| `attention` | erin (`decibell-headless`) calls dave (the app, unfocused): the window asks for attention and KWin reports `demandsAttention` within ~0.5 s (the KWin query's own time), on Wayland (patch `0017`) and XWayland (the urgency hint); under Do Not Disturb the call rings in and no request is made |
| `taskbar-progress` | `dbus-monitor` on the private bus sees `Update("application://decibell.desktop", …)` with progress 0 → 0.25 → 0.5 → 0.5 (paused: the protocol has no paused state) → hidden, then 0.1 → hidden for an upload; closing with that upload in flight asks and keeps the window, and closes once nothing uploads |
| `keyring-label` | a throwaway gnome-keyring on the private bus holds the at-rest key's item and a test item labelled `Decibell` (patch `0016`); read and delete find them |
| `window-state` | Wayland: resized to 1020 × 720 → quit → reopened at 1020 × 720; maximised → reopened maximised → unmaximised to 1020 × 720. X11: the same with the visible size (1000 × 700, exact after the one-time correction of §5), and a saved place at 100000,100000 reopens on a screen |
| `profile-guard` | with `HOME` a temporary directory and no `DECIBELL_USER_DATA_DIR`, the app and `decibell-headless` use `~/.cache/decibell-dev/gpui-default`; pointed at `~/.config/decibell` both exit 2 and create nothing; the real `~/.config/decibell` never appears |
| `idle-x11` | in a private Xvfb (21.1, `-displayfd`, stopped by its pid; input faked there with XTEST, nowhere else), SYNC `IDLETIME` alarms at a 2 s threshold: idle **2.001–2.002 s** after the last input, active **0.15 ms** after the next (the alarm thread's send, measured from the XTEST request), idle again 2.002 s after it; the `x11-idle` thread woke **0 times** in 3 s Away and 0 in 1 s active at rest; threshold raised to 4 s (no edge at 2 s), lowered to 2 s at 3 s idle (idle edge 0.12 ms later), raised to 5 s while idle (active by the reading, idle at 5.002 s); disable → the thread gone, enable → a new one. Two runs, both passed. MIT-SCREEN-SAVER (SYNC skipped: no X server lets it be switched off): readings only, no thread, no edges; a server without MIT-SCREEN-SAVER either: unsupported. The desktop, read-only: on Wayland the app picks the core's source and starts nothing of this; with `WAYLAND_DISPLAY` empty (XWayland) it picks the X11 source, starts nothing before statuses, then Xwayland 24.1 offers SYNC `IDLETIME` (no MIT-SCREEN-SAVER) and the thread woke 0 times in 5 s; statuses off → the thread gone |
| `content-copy-image` | Wayland and X11 each: three pictures copied with "Copy image" (a 1600 × 900 PNG, 313 KB; a GIF's first frame, 46 KB; 2.8 MB of noise) offered as `image/png` only (no text types), read back by another process (`wl-paste`; `tools/x11_clipboard.py`, INCR for the 2.8 MB) after the window lost the focus, pixels identical; on Wayland a `wl-paste --watch` reader (a clipboard manager's way) got each at once; the owner's clipboard read before and put back after (through Klipper), checked |

**At rest** (the sign-in page, unfocused, 15 s settle + 40 s, `tools/perf/measure.sh`; the same commit before and after
the platform work): threads **35 → 37** (`decibell-instance`, blocked in `accept`; one more `zbus::Connection`
executor, the screen-lock watcher's, blocked on its socket), own wakeups **0 → 0 / s**, frames 0, CPU 0.005 % of a core
both; anonymous memory 26.9 → 27.3 MB, private dirty 41.6 → 42.4 MB (private and PSS moved by tens of MB between
runs of the same binary: the machine was swapping, 17.7 GB of swap in use). The launcher-progress connection exists
only while a progress shows.

**X11 idle source at rest** (2026-11-27, `wip/platform2`, release + `dev-local`; the base `704dff0` built the same way;
sign-in page unfocused, 15 s settle + 40 s of `tools/perf/measure.sh`): Wayland 37 → 37 threads, XWayland
(`WAYLAND_DISPLAY=`) 38 → 38; own wakeups **0.0 → 0.0 / s** on both (driver 4.0, NVIDIA's `[vkps] Update`; 0.5 / s
on one unnamed thread, both builds); 0 frames; CPU 0.005–0.009 % of a core. The X11 source is chosen under XWayland
(`idle source x11`) but on the sign-in page auto-away is off, so it has no connection and no thread. With statuses on
(`--check platform-idle-source`, `DECIBELL_CHECK_HOLD_SECS=75`, Xwayland idle past the 10-minute threshold: the idle
alarm fired at once and the thread waited for input): 39 threads, the one more being `x11-idle`, **0 context switches
in 40 s**, parked in `ppoll`; own wakeups 0.0 / s. (One baseline XWayland run showed the main thread at 2 / s and the
driver at 15 / s with 0 frames, a disturbed run: rerun, 0.0.)

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

### Stream playback in the app (`wip/integrate-b`, 2026-11-26)

The whole path: `tools/perf/streambot.sh` streamers (headless `perf4`, and `perf5` for the second stream:
`tools/dev/local-stack.sh top-up`) send `.dbv` recordings through the community's relay, MLS-sealed; the client
(perf3, release + `dev-local`, in Lounge muted and deafened) receives, reassembles, opens and decodes them with
the decoder the probe found (Vulkan Video for all three codecs here) and presents them large, visible, the desktop
focused. `tools/perf/scenarios.sh gpui watch-1 | watch-2 --quick [--streamers perf4,perf5] --dbv …` (15 s
settle, 30 samples at 1 s). Wakeups per second by thread group: **video** = the `decibell-video-*` threads (the
hub's decoder thread per stream and the core's video receive thread), **UI** = the main thread and GPUI's
helpers, **voice** = the audio threads of the voice session the watcher is in (device clocks and the 5 ms tick,
kept by the owner's rule), driver threads 142–180 / s on top. The machine was never quiet (other agents'
builds: 1–8 `rustc` at 100 %, `kswapd` busy): the load average at the start of each row is its last column.

| Scenario | Stream(s) | CPU % of a core, mean / p95 | Wakeups / s: video; UI; voice | GPU decode % | Private MB mean (swapped out) | Received / decoded / presented per s, per stream | Dropped | Load |
|---|---|---|---|---|---|---|---|---|
| watch-1 | 1080p60 H.264 | 5.6 / 6.2 | 715; 177; 760 | 4.6 | 181.9 (0) | 60.0 / 60.0 / 59.9 | 0 | 7.5 |
| watch-1 | 1080p60 HEVC | 5.2 / 6.5 | 604; 180; 760 | 1.2 | 130.9 (0) | 60.0 / 60.0 / 60.0 | 0 | 6.3 |
| watch-1 | 1080p60 AV1 | 7.3 / 7.6 | 690; 181; 760 | 3.4 | 84.1 (10.5) | 60.0 / 60.0 / 60.0 | 0 | 3.2 |
| watch-1 | 1440p120 H.264 | 7.2 / 8.2 | 1,296; 356; 760 | 19.7 | 111.7 (0) | 120.0 / 120.0 / 118.2 | 0 | 8.0 |
| watch-1 | 1440p120 HEVC | 8.9 / 9.5 | 1,446; 356; 760 | 9.6 | 146.0 (0) | 120.0 / 120.0 / 120.0 | 0 | 9.6 |
| watch-1 | 1440p120 AV1 | 8.2 / 8.5 | 1,465; 356; 760 | 13.2 | 111.6 (0) | 120.0 / 120.0 / 120.0 | 0 | 5.2 |
| watch-2 | 2 × 1080p60 H.264 | 10.8 / 12.7 | 1,397; 358; 760 | 10.9 | 141.3 (0) | 60.0 / 60.0 / 60.0, twice | 0 | 6.1 |
| watch-2 | 2 × 1080p60 HEVC | 7.2 / 7.5 | 1,239; 359; 760 | 8.0 | 148.5 (0) | 60.0 / 60.0 / 60.0, twice | 0 | 3.6 |
| watch-2 | 2 × 1080p60 AV1 | 11.2 / 12.3 | 1,803; 355; 760 | 8.3 | 126.6 (9.9) | 60.0 / 60.0 / 60.0 and 59.7 | 0 | 6.6 |
| watch-2 | 2 × 1440p120 H.264 | 14.8 / 15.6 | 3,401; 710; 760 | 41.6 | 87.3 (38.0) | 120.0 / 120.0 / 119.8 and 119.9 | 0 | 13.1 |
| watch-2 | 2 × 1440p120 HEVC | 14.1 / 15.8 | 3,019; 713; 760 | 17.8 | 166.3 (0) | 120.0 / 120.0 / 119.9 and 119.8 | 0 | 6.5 |
| watch-2 | 2 × 1440p120 AV1 | 12.6 / 18.4 | 3,044; 716; 760 | 23.9 | 135.4 (0) | 120.0 / 120.0 / 119.6 and 119.5 | 0 | 4.5 |

- **Every frame of every row was decoded and none shed**, two 1440p120 streams of each codec included, with
  the machine at load 3–13; presented is within 0.5 frames a second of received (a picture overtaken before
  the compositor's frame is not drawn).
- **CPU**: one 1080p60 stream 5–7 % of a core, one 1440p120 7–9 %, two 1440p120 13–15 %, of which the voice
  session's audio threads are 1.8–2.6 % in every row. Rows at load 8–13 read high; none was repeated on a quiet
  machine because there was none.
- **Wakeups** per frame: about 3 on the UI threads (356 / s for one 120 fps stream, 713 for two) and 10–12 on
  the video threads (the decoder's wait for each picture's copy, the receive thread's per packet), as in the
  crate's own measurements above.
- **Memory** moved between 84 and 182 MB private from row to row with the same build, and three rows had
  part of the process swapped out: the machine was short of memory (`kswapd` at 44 % of a core in two rows).
  These are not numbers to judge the RAM target by: that target is the idle one, and the driver's share
  (about 60 MB of any Vulkan process on NVIDIA, CLAUDE.md's rule) is in every figure. Take them again on a
  quiet machine with `scenarios.sh gpui watch-1` (without `--quick`).
- **Against Electron**: its watch baseline was not taken (the target row "Watching 2 streams" above is
  still pending on `scenarios.sh electron watch-2 --streamers perf4,perf5`, which the new account makes possible).

**The decoder queue under starvation** (`decibell --selftest video-starve`, 1440p120 HEVC on Vulkan Video, load
9–19): the decoder thread held for 60 ms, three times: the queue reaches 7 frames, 0 shed, 0 keyframe requests,
4 pictures overtaken, the queue empty again 14.6–15.3 ms and 2 frames after the gap. Held for 400 ms: overflow
at 13 frames (100 ms), 60 shed, two keyframe requests, playing on on the same decoder. With the queue at three
frames the whole `video-*` suite failed some runs at load averages above 15 ("N frames dropped"); two full
passes of this work at load 10–25 passed every one of them (and a third at load 8–12).

**A stall** (`video-stall`, `voice-stall`): "stalled" 3.01–3.03 s after the last frame; while stalled 0 frames
drawn, 0 presented, 0 decoder threads; a picture again 40 ms (fed directly) to 100 ms (through the relay)
after the streamer resumes.

**The probe at start-up**: 60–132 ms on its own thread after the first frame (Vulkan Video, three codecs);
first frame before / after the wiring: medians of 6 warm starts 311 and 251 ms before, 342 and 218 ms after
(two alternating passes, load 5–18, single runs 183–507 ms): no difference outside the noise.

**Views rendered per change** (`render-cost`, GPUI patch `0014`, load 6 → 4): a speaking ring on a 12-tile
stage 18.2 → 5.0 views and 2,653 → 2,087 µs of main thread; the same ring in the channel list with a text
channel and its member list open 9.5 → 3.0 and 2,624 → 1,662 µs; a key typed under the message list 3.0 → 2.0
and 941 → 873 µs; a message arriving 3.0 → 2.0 views and no time difference outside the noise (1,021 → 977 and, in a second run,
1,002 → 1,073 µs) (`patches/README.md` §0014).

### Attachment playback (`wip/media`, 2026-11-26)

`docs/media-design.md` has the design and every number with how it was taken; in short (release build, RTX 4080,
load average 5–10 from other agents' builds):

| | |
|---|---|
| FFmpeg prefix (static archives) | 79 → 93 MB |
| `decibell` (release, stripped, `dev-local`) | 81,809,000 → 85,716,456 B with the new prefix (+3.9 MB), 86,258,696 B with the engine, the players and their self-tests (+4.4 MB, +5.4 % in all) |
| A/V sync, 60 s (headless: hand-over against the sample heard) | max 0.14 ms, mean 0.02 ms, 0 of 1,500 pictures dropped |
| Seek | the picture covering the target (≤ half a picture: 20 ms at 25 fps); the target's sample exactly (0 samples off) |
| Largest step handed to the device over every transition | 0.00164 of full scale from a half-scale constant (the 10 ms raised cosine's steepest: 0.00167) |
| Rust allocations in the steady state | 0 over 100 pictures + 4 s of sound; 0 over 4 s of sound alone |
| Cards at rest (`media-idle`) | 0 frames, 0 presents, no thread |
| 1080p60 H.264 playing, controls hidden | Vulkan Video; 60.2 pictures / s, 0 dropped; the window draws 0 frames; 3.0 % of a core in all (UI thread 1.2 %, decoder thread 1.0 %, control thread 0.5 %); wakeups / s: UI 148, decoder 127, control 60 |
| Paused | 0 pictures, 0 frames, 0 presents, 0 ms of CPU; the audio device closed after 250 ms of silence; two parked threads and the decoder session kept |
| Click → first picture | 50–62 ms (memory source); 58 ms from the local stack's attachment service (`media-live`; a seek 45 s ahead: 558 ms, a new range request and the decode from the keyframe 5 s before) |
| Private memory, 1080p60 clip: before / playing / after | 186.0 / 244.2 / 198.3 MB (the 43 MB test clip is held in memory while it plays; +12 MB stay afterwards, not attributed yet: `media-design.md` §8) |

The stream path after the changes to `decibell-video` (additive: `video-design.md` §0.11): `cargo test -p
decibell-video` 37 + 2 passed. One run of `boot`, `idle-frames` and every `video-*`, `voice-*`, `chat-*` and
`content-*` self-test on this build (40 tests, 10 minutes, load average 3–13 with other agents' builds and
windows on the same desktop): **37 passed**, among them `video-hw`, `video-nvdec`, `video-vaapi` (each decoder's
picture against software decode), `video-order`, `video-starve`, `video-stall`, `video-fastpath`, `voice-watch`,
`voice-stall`. Three failed in that run and **passed when run alone** right after (load average 3–5), all three
on frame counts or pointer focus, none on decoding: `video-recover` ("the window was still drawing after 10 s"),
`chat-hover` ("no tooltip on the Reply action"), `content-gif` ("the GIF kept drawing off screen": 2 frames in
1 s).

### Audio: sounds, the mic test, device changes (`wip/core-audio`, 2026-11-26)

Measured by the wiring agent with `tools/perf/measure.sh` (20 s settle + 60 s), release builds.

- **Idle app** (perf1, unfocused, `DECIBELL_CUE_SINK=off`, load ~2): own wakeups 0.55 / s (`decibell-rt` 0.3,
  main 0.2), 0.01 % CPU, 86.1 MB private, 37 threads. No `decibell-cue`, `decibell-mic-test` or
  `decibell-audio-devices` thread exists (each lives only while a sound plays without voice, a test runs, a
  device list is on screen).
- **Voice session** (`decibell-headless` as perf5, alone in Lounge, muted, on a PipeWire null sink through the
  pulse PCM, load ~3): 690 wakeups / s (766 in the voice-plane table's row A; 688 on null sinks there).
  `decibell-audio` 197.7, cpal in / out 100.2 / 93.9, `decibell-voice-recv` 0.3, runtime 6.0, the sound
  server's two loops 145 each. Our threads did not rise. The app itself in voice (which has the cue player)
  was not measured.
- **A sound outside voice**: the stream's open took 171 ms the first time and 91–113 ms after (the core's
  ignored test `cue::tests::open_latency_into_a_null_sink`, a named null sink through the pulse PCM, load ~10);
  the OS-default path the app uses was not measured (it would open the real device).
- Self-tests (`settings-audio-meter`, `voice-cues`, `call-ring`): a sink appearing is listed ~452 ms later and
  gone ~453 ms after removal (the 400 ms debounce included); the ring loops every 2.00 s; 244 ms after leaving
  voice nothing is open. **Every voice self-test now plays and captures on a null sink the test's parent
  makes** (`decibell_harness::test_audio`, `DECIBELL_CUE_SINK`); before, they opened the desktop's real
  devices muted. `CONVENTIONS.md`'s voice / settings self-test notes don't say this yet.

## 5. Open items

- **Attachment playback: what was not run or built** (`docs/media-design.md` §8 has the reasons).
  *Not run*: a sealed attachment end to end against the local stack (`media-sealed`: upload into an encrypted
  channel, play, grep the temp and profile directories; the engine's sealed test reads through a fake
  decrypting reader, and a plain attachment plays from the real service in `media-live`: what is unproven is
  only `MediaReader`'s sealed branch under a player, which pictures and downloads already use);
  `tools/perf/measure.sh` on a playing clip and the Electron comparison (`electron-local.sh`
  has no attachment scenario; the self-test's own per-thread CPU and wakeup table stands in); the captures of
  the playing states in seven themes and their Electron pages (a capture renders one frame on a headless
  renderer, where no picture is decoded: the playing card needs a still injected into the stage first);
  AMD / Intel; Windows (compiles in principle: cpal's WASAPI output and the default-device change are unrun,
  and pictures draw nothing there until `gpui_windows` draws surfaces).
  *Not built*: the controls' 300 ms fade, the bars' hover thumbs, the fullscreen download button, Space on a
  focused audio card, the audio card's error line, the last-frame poster, 10-bit video, rotation metadata,
  VP9 / VP8 hardware decode, stopping on a DM conversation change.
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
- **Where audio could click: the 2026-11-26 audit, fixed on `wip/core-audio`** (the review log's entry of
  that date has the table: what each did at the sample level, the test that failed before, the commit). Every
  entry has a sample-exact test in `crates/decibell-core/src/media/click_tests.rs` (the real receive path, mix
  and capture chain; no step between samples larger than the talker's own slope) and is waiting for the
  owner's two-person listening test (§6). Fixed: 1 output device switch, 2 deafen / undeafen, 3 per-user
  volume, 4 microphone switch, 5 processing toggles, 6 the pull estimate across a switch, 7 the stream-audio
  lock, 9 stream audio's start / stall / catch-up, 10 the end of a talkspurt cut mid-sound (push-to-talk
  release, mute), 12 the capture path's own allocations; and three found on the way (a concealment's fade
  stepped at every frame, late packets resumed at full level after it, voice cut to trim latency was spliced).
  Shown not to click: 8 (ten lost frames, the jitter buffer's reset), 10's push-to-talk press. **Open**:
  - 11, a device open blocks the audio loop (~90 ms here, more on Bluetooth): no capture frame goes out
    meanwhile, listeners hear a gap and then a burst. Fixing it means streams owned by a thread of their own,
    which changes who produces and consumes when: the owner's decision.
  - a device whose period is over 30 ms (some Bluetooth) starves the ring until the first 2 s look at what it
    pulls, after a switch and on a fresh join alike. Taking the first observed pull at once would fix both; it
    holds the ring deeper sooner (a buffering change): proposed, not made.
  - a new microphone restarts the echo canceller (~1 s to converge; the echo path is new either way); a
    deafen toggled twice within 30 ms can meet its own leftover audio; a mono output device plays a stereo
    stream ring at half speed (pre-existing, `OutputMix`); the `aec3` crate allocates ~134 times per capture
    frame with echo cancellation or gain control on.
- **Interface sounds, the mic test and device changes are the core's** (`api::audio`, `media/{cue,mic_test,
  device_watch}.rs`; the review log has the design). Two things to know: a sound played with no voice session
  opens a stream first (91–171 ms into a null sink at load average 10, then the device's buffer; one that
  follows within 250 ms reuses it), and in voice a sound plays on the voice output device, out of voice on the
  OS default (Electron: always the default). Both are in §6 for the owner.
- Timed waits that remain in the core by decision: the 5 ms audio tick and device-clock wakeups in voice, the
  30 s server pings, the 500 ms MLS tick (could be deadline-driven), the documented hotkey polls.
  `default_device_watch` (Windows / macOS) now sleeps until the OS's notification; it polls once a second only
  if the notification can't be registered.
- **Physical key codes: done** (GPUI patch 0012, `patches/README.md`). Key events carry the platform's
  physical key (`KeyDownEvent::code`: the evdev code on Linux, the scan code on Windows) and modifier keys
  arrive with their side (`ModifiersChangedEvent::key`); `decibell_model::live::hotkeys` names them through
  the core's table (`code_of`), and the recorder (`Recorder`) and the focused fallback (`Hotkeys::key_event`)
  use that, tested under xkb's `tr` and `de` layouts. The Keybinds tab feeds the recorder (self-test
  `settings-keybinds-record` records from events built as the `de` / `tr` keymaps produce them) and the app's
  root feeds the fallback (`decibell-app/src/hotkeys.rs`, self-test `hotkeys-focused`; a key that types into
  a focused text field is the field's, `docs/electron-quirks.md` #13). Still by hand, once: record a binding
  on a real Turkish or German keyboard under Wayland, and on Windows (scan codes there were never run).
- **X11 idle reading: done** (`decibell-platform/src/idle/x11.rs`, self-test `idle-x11`; the owner's check on a real
  X11 session is in §6). Left open: under **XWayland** (the client started with `WAYLAND_DISPLAY` empty) the source
  reads Xwayland's counter, which counts only the input Xwayland receives: its own windows', plus what KWin's
  "Legacy X11 App Support" forwards (KWin 6.8 has no other path into it: `libkwin` links neither XTEST nor
  ForceScreenSaver); whether that is enough is the §6 item's question. The core's `ext-idle-notify` would be exact
  there, but it finds the compositor through the same `WAYLAND_DISPLAY`, so the choice is this counter or
  "unsupported" (never Away by idle), as before. And the MIT-SCREEN-SAVER fallback (for a server without SYNC
  `IDLETIME`: Xvfb 21.1 and Xwayland 24.1 have it, Xorg's SYNC is the same server code; Xwayland has no
  MIT-SCREEN-SAVER at all) leaves `AutoAway` its one-second re-read while Away.
- `system_idle` can only be queried, not subscribed to: auto-away is late by up to one threshold on Wayland.
  (Device changes and the mic test outside voice exist since 2026-11-26: `api::audio`.)
- **Voice receive path allocates per packet** (~5 allocations per voice packet per talking peer; the core
  follow-up measured it and left a plan in its notes: fixed datagram slots on the existing ring, peers keyed by
  the 32-byte sender id, in-place open, preallocated Opus slots). Not done; do it if voice CPU / wakeups need it.
- `media::watch_stream_add` and `events::set_stream_frame_sink` update their sets by load-clone-store: two
  concurrent writers can lose an update (found by the core follow-up's tests; production code unchanged).
- **A cached view goes stale unless it observes what it reads** (`docs/model.md` §4): GPUI re-renders a cached
  view only when that view itself is notified. (And, since GPUI patch `0014`, no longer whenever a cached
  ancestor renders: a view that relied on that by accident now shows it. None was found: every self-test passes
  with it; `voice-join`, `voice-watch`, `shell-navigate` and `idle-frames` were also run with
  `DECIBELL_VIEW_REPLAY=0`, the pin's behaviour.)
- **Stream playback, left open** (`docs/video-design.md` §0.4, §0.4a, §0.5):
  - a GPU device lost while no stream is decoded is not seen by the video hub, so the decode capabilities are
    probed again only at the next stream, Settings refresh or start (GPUI has no "device replaced" hook);
  - a stream that never got its first picture is not asked for a keyframe again on a timer (the renderer
    re-asks every second); frames that arrive gated do ask, throttled;
  - `video-vaapi` under XWayland failed once in three runs at load average 15 ("H.264 with vaapi: the picture
    differs from software decode's (13.17 of 255)", every frame decoded and presented in both runs: the two
    captures were of different frames); not reproduced alone, before or after this work. The capture, not the
    decoder, is the suspect;
  - the voice self-tests share `perf3` / `perf4` on the stack: two agents running them at once disturb each
    other;
  - `settle` in the harness waits for presents as well as draws, so it never returns while a speaking ring
    breathes (`render-cost` has its own).
- **Community server bug (for `main`, not changed here)**: the first page of the member list returns the
  requester's own `role_ids` doubled when their row is already in the page (`src/community/main.cpp`); this
  client de-duplicates on arrival.

- **Done (integrate-c)**: the message list's "today" now moves on at the next local midnight (one timer per list,
  re-armed daily; the renderer's memoised bubbles stay stale the same way, ours relabel); the chat self-tests no
  longer depend on order (`fixture::open` purges the channel's slice before seeding: `chat-rows,chat-history`).
- **Done (integrate-d): DM rows of central's two legacy refusal strings** ("This user is currently offline…", "…only
  accepts direct messages from users in their friends list.") are drawn as `DmChatPanel`'s `ErrorCard` row: a notice
  card row kind in `decibell-chat`'s `row.rs` (exact height, no actions or hover wash), asked of the list source
  (`ListSource::notice`; `DmSource` answers for our own rows with those strings). Screen `dm-refusals`, Electron
  twin `pages/dm-refusals.tsx`: 0.53–0.96 % of pixels differ in the seven themes (`dm-plain`: 0.66 %). The renderer's
  list rests ~35 px above its end on that page (it pins before the cards' rows are final); the page scrolls it
  to the end before the capture.
- **Done (integrate-d): type-to-focus with the DM call's theater on** no longer reaches the hidden composer
  (`ComposerTarget::hidden`; self-test `join-type-to-focus-hotkeys`).
- **`message_stamp` costs ~117 ns** a call, of which ~58 ns is formatting and the rest `chrono::Local`'s
  zone conversion (it re-checks `TZ` / `/etc/localtime` at most once a second per thread). Rows cache
  their labels, so it is not on the scroll path; cache the local time per message if a profile ever shows it.
- **The root's `on_focus_lost` (integrate-d)**: GPUI calls it when the focused handle isn't in the frame just drawn,
  which is also the case for one frame after a view focuses a handle it is still building (the message list's
  inline editor focuses itself as its row is laid out): the root took the focus from it, and the edit's keys went
  to the composer (`dm-send-receive`: "timed out waiting for the edit's echo", every run). The root now takes the
  focus at once only when nothing is focused, and from a focused handle only if it still isn't drawn after the
  next frame. Still open: with a modal up, an explicit `window.blur()` gives the focus to the root, outside the
  modal's layer, so its Esc / Enter / Tab no longer reach it (a press in a modal focuses its layer, so only an
  explicit blur gets there); a fix would hand the focus to the topmost open modal instead.
- **Self-test harness (integrate-d)**: `TestCx::settle` waits for draws to stop; presents that run on without
  draws for 1.5 s are a present-only animation (the active tile's glow breathes while the window has the
  desktop's focus, a speaking ring) and no longer keep it waiting (the content tests failed on that whenever the
  test window got the focus). A test that could not measure returns `decibell_harness::inconclusive(reason)`
  and is reported INCONCLUSIVE, apart from passes and failures; a parent relaying a `--check` child's
  INCONCLUSIVE line is INCONCLUSIVE too.
- **Feature `init`s are not all idempotent**: `decibell-chat`, `-composer`, `-home`, `-chat-content`,
  `-server-settings` and `-search` register again when called twice (voice, servers and settings keep a once
  guard). The capture tool now calls `registry::init_features` for the shell's `main-*` screens (their panes are
  the features', as in the app: `main-home` 0.27–0.40 %, `main-server` 0.46–0.54 % against Electron); the other
  screens still join only (they install their own crate).

- **Leaving a community right after joining it** (owner's side agent, 2026-11-26). Central tells every device of
  an account "you joined" (MEMBERSHIP_ADDED) a moment after a join, the joining device included; the core waits
  3 s and connects if it isn't connected, and connecting to a *public* community without an invite makes the
  account a member. A leave inside that window (or before a late push) therefore did not stick. Fixed in the
  core (`6b9de79`: `AppState::left_communities`; a push for a community this device just left or was removed
  from is ignored for 60 s, until the user joins again; unit test `a_membership_push_right_after_leaving_is_stale`).
  **Not covered, and it needs a server change**: another device of the account that was online during a join
  and a leave within that window never saw the leave (central sends no "you left"), so it connects and re-adds
  the account. Decision for the owner: a MEMBERSHIP_REMOVED push from central, or the community refusing an
  auto-join for N seconds after a leave. Electron shares the core and the race; not run there.
- **Admin actions are not in the model.** `decibell-servers`, `decibell-server-settings` and `decibell-home` call
  `decibell_core::api` directly for invites, channels, roles, bans, kick / timeout / nickname, member search and
  the audit log (each behind its own small request type). Moving them into `actions::` would give one place for
  pending state and toasts; nothing depends on it.
- **`voice-watch` under load**: once, the child exited with SIGSEGV on its failure path in the video crate
  (conversation-integration run, load high, 53.7 pictures / s). The exit-time crash in NVIDIA's driver was fixed
  by `decibell_video::shutdown` in the quit hook (video / voice integration); check whether a failing self-test
  leaves by another path that skips it.
- **Windows batch (desktop integration)**: real implementations (owner, 2026-11-26: the native client replaces Electron
  on Windows too), written against `windows` 0.62.2 (the bindings GPUI's Windows platform uses; every call checked in
  the crate's source) and GPUI's own `gpui_windows`, compiled only by the Windows Native Check, **never run**
  (`crates/decibell-platform/src/win32.rs`, `instance.rs`):
  - single instance: the exclusive lock on `<profile>\instance.lock` (`File::try_lock`, `LockFileEx`), and a named
    pipe `\\.\pipe\decibell-<hash of the profile path>` (`CreateNamedPipeW` with `FILE_FLAG_FIRST_PIPE_INSTANCE` and
    `PIPE_REJECT_REMOTE_CLIENTS`, default DACL: only the user, administrators and SYSTEM may write) served by a thread
    blocked in `ConnectNamedPipe`; the second start opens it (`CreateFileW`, `WaitNamedPipeW` while busy), calls
    `AllowSetForegroundWindow` on the pipe server's process id (`GetNamedPipeServerProcessId`) and hands its command
    line over; the first comes forward through GPUI's `activate` (restores a minimised window, `SetForegroundWindow`);
  - `decibell://` arrives as an argument (`"%1"` in the registered command: the installer's keys are in §6); the app
    never writes the registry;
  - `--hotkey=` works as on Linux but isn't needed: press actions are `RegisterHotKey` system hotkeys in the core;
  - attention: `FlashWindowEx(FLASHW_TRAY | FLASHW_TIMERNOFG)` (until the window comes to the foreground);
  - taskbar progress: `ITaskbarList3` (`CoCreateInstance(TaskbarList)`, `HrInit`, `SetProgressState` normal / paused /
    none, `SetProgressValue` in thousandths) on the UI thread, which GPUI made an STA (`OleInitialize`);
  - window state: GPUI's `window_bounds` (`GetWindowPlacement`: the restored bounds of a maximised window), reopened on
    the monitor the saved place is on (`WindowOptions::display_id`: GPUI converts the bounds with that monitor's DPI
    and drops bounds whose centre is on another one); a place on no monitor is dropped;
  - the quit guard: `WM_CLOSE` (the title bar's close goes through GPUI's non-client handling to `WM_CLOSE`) asks
    while uploads run; downloads pause in `on_app_quit`, which GPUI also runs on `WM_ENDSESSION`;
  - screen lock → Away: `WM_WTSSESSION_CHANGE` (`WTSRegisterSessionNotification`) to a message-only window on a thread
    sleeping in `GetMessageW`; sleep / wake through GPUI's `WM_POWERBROADCAST`; idle time: `GetLastInputInfo`
    (`decibell_platform::idle`), read when `AutoAway` asks (once per threshold while active, every second while Away);
  - Copy image: GPUI writes the registered "PNG" format and, with patch `0019`, a `CF_DIB` (Paint, older Office);
  - the keyring: Credential Manager, generic credential `Decibell:url=decibell://e2ee-local-key/<profile>` (patch
    `0016`; GPUI persists it `CRED_PERSIST_LOCAL_MACHINE`, i.e. for this user on this machine).
  Most likely wrong on first contact: placement across monitors with different DPI scales (each monitor's logical
  bounds are in its own scale: the saved place is matched to a monitor by its centre in that space, GPUI checks
  again in device pixels), `SetForegroundWindow` refusals when the second start itself wasn't in the foreground
  (a start from a background process: the window then only flashes), and `ITaskbarList3` calls made before the
  taskbar button exists (a progress in the first seconds after start is lost until the next change). Known limit: the
  pipe's reads have no timeout, so a second start that connects and then hangs (not one that crashes or is killed:
  that breaks the pipe) holds later hand-overs until it exits.
  Checked without Windows: `win32.rs`, the Windows half of `instance.rs` and `idle.rs` type-check (and pass clippy)
  against `windows` 0.62.2 in a scratch crate whose copies of `windows` / `windows-core` / `windows-strings` /
  `windows-result` have their `cfg(windows)` gates removed (the recipe in the Electron hand-off's Windows notes;
  a planted type error is caught). The GPUI-facing callers (`attention`, `progress`, `session`, `window_state`) are
  read against `gpui_windows`' source only.
- **X11 (KWin) window geometry with client-side decorations is racy**: a window opened at a given size comes up with
  its visible size equal to it or 20 px smaller (the margin), depending on whether KWin learns `_GTK_FRAME_EXTENTS`
  before it maps the window (measured: 1300 → 1300, 1280 → 1260 on first starts; 1000 → 980 on some restores, also
  when coming back from a window opened maximised). `window_state` corrects a restored window once (at its first
  activation, and when it comes back from maximised), so saved sizes no longer shrink (`window-state` passes exactly,
  4 runs in a row); a first start without a saved state can still be 20 px off. The real fix belongs in `gpui_linux`
  (set the extents before mapping); not done.
- **The launcher progress is keyed to `decibell.desktop`**, the id of the window (`app_id`). On this machine that
  desktop file is the installed Electron client's, so a development build's progress (any self-test child on the
  session bus with a download running) would show on its taskbar entry; the platform's own tests use a private bus.

## 6. Needs owner

Things only the owner can do; nothing here blocks the rest.

- **Shipping the native client** (`PACKAGING.md` §11): the tag pattern (`native-v<VER>`), the update signing
  key (`UPDATE_PUBLIC_KEY` variable + `UPDATE_SIGNING_KEY` secret, or signing by hand), pinning the AppImage
  tools' hashes from the dry run's log, the project's licence (no `LICENSE` file; the LGPL needs one that
  allows relinking), the AUR name `decibell-native-bin`, Windows names and code signing while both clients
  ship, the re-check interval, Sentry debug-file uploads, and how Electron users move over. On Windows (never
  run here): install, `decibell://`, the shortcut's taskbar grouping, an update with its trial start and a
  rollback (`PACKAGING.md` §3, §6).

- **The desktop integration by hand** (`decibell-platform`, Linux): (1) after packaging installs
  `assets/linux/decibell.desktop`, click a `decibell://invite/…` link in a browser while Decibell runs: the window
  comes to the front (the forwarded activation token, patch `0018`, is only exercised by a real launcher; without a
  token KWin marks the window instead) and the join dialog shows; with Decibell closed it starts with the dialog after
  sign-in. (2) Paste a copied picture into GIMP, LibreOffice, a browser and Discord, on Wayland and under XWayland,
  and with Klipper's "Save images" on (it reads the picture at once). (3) Start a download and watch the task
  manager's progress on Decibell's entry. (4) Quit with an upload running: the "A file is still uploading" question
  (Enter keeps uploading). (5) Lock the screen: Away at once; unlock: back. (6) On Windows (the batch in §5),
  for `docs/windows-owner-checks.md`:
  - **Single instance**: start Decibell, minimise it, start it again from the Start menu: no second window, the first
    comes back to the front within a second. Start it again from a command prompt while another program is in front:
    it comes forward, or at least its taskbar button flashes. Two profiles (`set DECIBELL_USER_DATA_DIR=…` in one
    prompt): two windows.
  - **`decibell://`** (after the installer registers it): with Decibell running, open
    `decibell://invite/<a real code>` from Win+R or a browser: the window comes forward with the join dialog; with
    Decibell closed it starts and shows the dialog after sign-in. The installer's keys, under
    `HKCU\Software\Classes\decibell`: `(Default)` = `URL:Decibell`, `URL Protocol` = `` (empty string),
    `DefaultIcon\(Default)` = `"<install dir>\decibell.exe",0`, `shell\open\command\(Default)` =
    `"<install dir>\decibell.exe" "%1"`. The app never writes the registry.
  - **Attention**: have someone call you while Decibell is behind another window: its taskbar button flashes until you
    bring it to the front; not when your status is Do Not Disturb.
  - **Taskbar progress**: download a large attachment: the taskbar button shows the progress, yellow when you pause it,
    and clears when it ends.
  - **Quit guard**: close the window while a file uploads: "A file is still uploading" asks; Keep uploading / Enter /
    Escape keep the window, Quit anyway closes it. A download running at that moment resumes at the next start.
  - **Window state**: move the window to a second monitor (ideally one with another scale, e.g. 150 %), resize it,
    close, start: same monitor, place and size; maximise, close, start: maximised, and Restore gives the old size.
    Disconnect that monitor, start: the window opens on the remaining one.
  - **Screen lock and idle**: Win+L, wait, unlock: your status showed Away while locked (another account sees it).
    Set Settings → auto-away to 1 minute, leave the mouse and keyboard alone: Away after a minute; move the mouse:
    Online again within about a second.
  - **Copy image**: right-click a picture → Copy image, paste into Paint (a bitmap: patch `0019`), Word and a browser.
  - **Keyring**: Credential Manager → Windows Credentials shows a generic credential named
    `Decibell:url=decibell://e2ee-local-key/…`, nothing named `zed:`.

- **Auto-away on a real X11 session** (`decibell-platform/src/idle/x11.rs`; here it ran only in a private Xvfb, where
  input was faked with XTEST, and read-only against this desktop's Xwayland). On an X11 login (Plasma (X11), GNOME on
  Xorg, Xfce, i3, …), start the client from a terminal with `DECIBELL_LOG=info` and sign in:
  1. stderr shows `[idle] X11: SYNC IDLETIME alarms (<vendor> <release>): …` once auto-away is on (after sign-in).
     `ps -L -o tid,comm -p <pid>` lists one `x11-idle` thread; Settings → auto-away "Never" (or sign out) removes it
     within a second, choosing minutes again brings it back.
  2. Settings → auto-away 1 minute; leave mouse and keyboard alone: Away after 60 s (your own dot; another account sees
     Away once every device of yours is idle). Move the mouse: Online again within a second (not after a poll).
  3. Type in another application (not Decibell) for two minutes: you stay Online (the server counts every client's
     input). With a video playing in mpv / a browser that inhibits the screen saver you may stay Online: those players
     reset the server's idle time, as Electron's `powerMonitor` saw too.
  4. While idle at 1 minute, switch to 5 minutes: Online at once if you were idle under 5 minutes, Away at 5.
  5. Lock the screen, wait, unlock: Away at once, back at unlock (the session watcher, unchanged).
  6. Optional, on Plasma Wayland: `WAYLAND_DISPLAY= decibell` (an X11 client under XWayland). Xwayland's counter sees
     only input Xwayland receives (its own windows, plus what KWin forwards: System Settings → Applications →
     Legacy X11 App Support). Type in a native Wayland app for two minutes at a 1-minute threshold and report
     whether the client goes Away (if it does, typing elsewhere looks like idleness to an X11 client under XWayland:
     that run mode, not the X11 session, would need the compositor's reading instead).
- **Listen to attachment playback** (`docs/media-design.md` §4: every transition is a 10 ms ramp and each has a
  sample-exact test on what the device is handed; none of it has been heard by anyone). With headphones, at a
  normal level, on a music file and on a file with a loud steady tone (a pop hides in music), for a video and for
  an audio attachment. Each should be free of any click, pop or tick, and the sound should never jump in level:
  1. **start**: press play; and play a second attachment while the first plays (the first fades, the second starts);
  2. **pause** and **resume**, quickly and after a few seconds (after 250 ms the device stream has been closed
     and resume opens a new one: listen for a click at either end, and for the first syllable not being cut);
  3. **seek** while playing (click the scrub bar; then drag it back and forth for a few seconds) and while
     paused, then play;
  4. **the end** of a file that ends on sound (a clip cut mid-note);
  5. **volume**: drag the volume bar, use the wheel on it, mute and unmute while a tone plays (no zipper noise);
  6. **an underrun**: play a large attachment with Settings → Network's download cap set low enough that it
     stalls (the sound should fade out and come back, not stutter);
  7. **a device change**: unplug headphones / switch the system's default output while it plays, and plug back
     (the old device's last few milliseconds are the hardware's; the new one must start cleanly);
  8. **close**: switch channel while it plays; sign out while it plays; quit while it plays;
  9. **with voice**: in a voice channel, play an attachment: voice must sound exactly as before (the attachment is
     a separate output stream; nothing of the voice path was changed), and both are heard together.
  Also by eye, on a film or anything with speech: **lips and sound together** (the measured error of the clock is
  0.14 ms; what the display adds is up to one refresh), no judder on a 24 fps file, the first picture after a seek
  is the one asked for, a paused video resumes without a repeated or skipped frame.
- **Decide** `docs/electron-quirks.md` #17 (a channel switch stops playback: `FEATURES.md` says it survives, the
  renderer's code stops it; this client follows the code) and #21 (no last-frame poster yet).
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
- **The listening test for the click fixes** (`wip/core-audio`; two clients A and B in one voice channel, B
  talking or playing steady speech / music into its microphone, A listening on headphones; the Electron client
  built from this branch works as A or B, the fixes are in the shared core). A failure is a tick, a pop or a
  short buzz at the moment named; a pass is a quick fade or nothing.
  1. A switches its output device while B talks (wired ↔ wired, then wired → Bluetooth): B fades out, comes
     back fading in. Failure: a click at the switch or as B returns; on Bluetooth, crackle for up to 2 s (the
     open item in §5: say if you hear it).
  2. A deafens and undeafens while B talks, several times, slowly and fast: no tick either way.
  3. A drags B's volume slider up and down while B talks: no "zipper" noise.
  4. B switches its microphone mid-word: A hears B's word end softly and the new microphone start softly; no
     click. (A second of echo of A's own voice at B is expected: the echo canceller relearns.)
  5. B toggles echo cancellation, gain control and noise suppression mid-word, each on and off, and moves the
     noise-suppression level between two "on" values: A hears the level or the noise change over a moment, no
     click; the level change between "on" values changes nothing.
  6. B, with push to talk, releases the key in the middle of a loud word, and mutes in the middle of one: the
     word ends in a short fade. Failure: a tick right after the word stops. Then B presses the key mid-sound:
     the start fades in.
  7. A watches B's stream with audio (music), starts and stops watching, B stops and restarts the stream, and
     one of them stalls the network for half a second (pause the Wi-Fi, a `tc netem` delay): the stream's
     audio starts and returns without a click, a stall ends in a fade, the catch-up after it has no tick.
     The same stall while B talks: the voice comes back without a tick.
  8. A leaves the channel while B talks: B fades out (5 ms) instead of being cut; the leave sound plays whole.
  9. Anything new: with three people talking in turns for a few minutes, no crackle, word starts not clipped.
- **Interface sounds, side by side with Electron** (native client and Electron on the same account settings,
  same headphones): join and leave a voice channel, have someone else join and leave, mute / unmute, deafen /
  undeafen, start and stop a stream, call and be called (ring until answered, declined, timed out; with Do Not
  Disturb on, the incoming ring is silent in both), end a call. Each sound should be the same sound at the same
  level in both. Listen for: a click at a sound's start or end; the ring's loop point; the ring ending
  (the native client fades it over 5 ms where Electron cuts it); a sound played while people talk (it is mixed
  into the voice output: no dip, no click, the voices unchanged); the first sound after a silence outside
  voice arriving late (0.1–0.2 s: the output stream opens for it).
  **Decided** (owner, 2026-11-26): sounds play on the voice output device chosen in Settings → Audio, in voice
  and out of it, never the separate stream-output device (`crates/decibell-app/src/audio.rs`; Electron keeps the OS
  default: `docs/electron-quirks.md` #23); and the 0.1–0.2 s before a sound outside voice is fine for now.
- **The microphone test** (Settings → Audio, outside voice): the meter follows your voice, the OS's
  microphone indicator comes on with "Test mic" and goes out when the test or the tab ends, and joining voice
  ends the test. It meters the signal after your echo-cancellation / noise-suppression / gain-control
  switches (what others would hear); Electron's meters Chromium's own processing.
- **Devices**: with Settings → Audio or a device menu open, plug and unplug a headset: the list and a saved
  device's "Not connected" follow within a second. On Windows and macOS this code (and the default-device
  follower's switch from polling to notifications) has never run: check that changing the default device in
  the OS moves a session set to "Default" within a moment, once.
- **Stream playback on AMD and Intel, and by eye** (`docs/video-owner-checks.md`; §4a there is new): the decoder
  list in Settings → Codecs on those machines (it is the probe's whole answer: copy it), "Reconnecting…" with a
  real network drop, a 120 fps stream on a machine whose CPU is busy (it should stutter at worst, not freeze),
  and the cursor hiding in fullscreen under Wayland and XWayland (the self-test cannot move the real pointer).
  Windows: the cursor hide is upstream GPUI's own code path and the rest of this work has no Windows-only code;
  nothing of it was run there.
- **Decide: the 100 ms decoder queue** (`docs/electron-quirks.md` #14, decided by the architect for video
  quality). It costs up to 100 ms of extra latency during a hiccup and on a machine that cannot decode the
  stream in real time; say if the Electron client should get the same rule.
- **Run the encoder checks on AMD and Intel hardware** (`crates/decibell-core/tools/verify-encoders.md`: one
  command per backend on Linux and Windows) and a real Go Live through each.
- **Decide the active-tile glow's behaviour.** In Electron it breathes forever (2.4 s opacity cycle) and costs
  ~15 % of a core on Linux while the window is visible. This client breathes it only while the window is
  focused, at a capped frame rate, presenting without re-rendering; unfocused it rests. Say if it should
  breathe unfocused too (it then draws frames at idle) or not at all.
