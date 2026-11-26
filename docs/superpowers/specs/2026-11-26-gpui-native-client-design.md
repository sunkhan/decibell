# GPUI native client

2026-11-26. **Owner decision: the native client is built on GPUI, not Qt.** It lives in `gpui-client/`, on the
`gpui-client` branch (cut from `qt-spike`, which carries the Qt spike's report and tools), and goes for full
feature parity with the Electron client: every row of `docs/FEATURES.md` §1–§9. The Electron client keeps
shipping and defines every behaviour until then.

The point of the migration is a faster, leaner client: **far less RAM than Electron, and low CPU, GPU, wakeups,
startup time and install size.** Every choice below is judged by those, measured, on the same machine as the
Electron client it is compared with.

This record replaces the Qt design (`2026-11-07-qt-native-client-design.md`) as the plan of record. What of that
design still holds, unchanged, is listed in §1; the rest of this document is what GPUI changes.

Status: §2–§4 are decided. §5 (GPUI facts) and §7 (video) are filled in from the source survey and the video
prototype as they land (`gpui-client/docs/gpui-notes.md`, `gpui-client/docs/video-design.md`); the running state
of the work is `gpui-client/HANDOFF.md`, the checklist `gpui-client/PARITY.md`.

---

## 1. What carries over from the Qt design

| Qt design | Status here |
|---|---|
| §1 How the client is judged (memory, CPU, GPU, wakeups, startup, size; one harness for both clients) | Holds. The Linux harness is `tools/perf/measure.sh` + `startup.sh` (§3). |
| §2 Electron baseline | The Windows numbers stand for Windows. **This session is on Linux**, so the baseline is re-measured on this machine under Linux, against the local test stack (§3). |
| §3 Resource targets | Hold **as ratios** against the Electron baseline of the same machine and OS (§3 below). |
| §6 The core split (`crates/decibell-core` + a thin napi crate) | Holds, done first (§4). |
| §7 Threads and the runtime model | Holds with GPUI's names: one UI thread (GPUI's foreground executor; there is no separate render thread), a tokio runtime the client owns, the core's media threads, one decoder thread per watched stream. One wake of the UI thread per burst of core events. No polling. |
| §8 Video: decode on the GPU, convert at drawn size, newest-frame pick-up, one redraw per new frame, the persistent player | The model holds; the mechanism is GPUI's (§7). |
| §9.2 Message-list invariants (exact heights, rows keyed by identity, anchor-based positions, 0 px on a prepend, cross-row selection, Chromium-matched wheel) | Hold; implemented as a GPUI element (§6.3). |
| §10 What replaces Electron's main process and Chromium services | Holds row for row (§8). |
| §11 Fidelity: generated tokens, deterministic fixtures, captures of both clients side by side | Holds (§9). |
| The spike's findings and tools (`docs/reviews/2026-11-07-qt-client-spike-report.md`, `qt-client/HANDOFF.md` §2a–§2f, `tools/perf/`) | Used as reference: fidelity rules (line height, Inter `opsz`, emoji boxes, inline-code chips), the wheel curve, selection colours, the `.dbv` stream format, where memory went. |
| Qt Quick, cxx-qt, CMake, QML, `qt-client/` code | Set aside. Nothing is built on `qt-client/`. |
| D13 (Windows hotkeys: `RegisterHotKey` for press actions) | Already on `main`. |

Standing decisions in `CLAUDE.md` apply to this client without exception (uid identity, bound-keys-only hotkeys,
strict MLS voice, no recent searches, attachment images not cached across restarts, link previews unfurled by
the client, KLIPY, …). The one that does not carry over is "prefer Chromium APIs": there is no Chromium.

## 2. Rules of the build

- **Branch**: `gpui-client` only. Nothing is pushed to `main`, tagged, released or deployed from this work.
- **No wire or server changes.** The client speaks today's protocol to today's servers.
- **GPUI is pinned** to one Zed commit, recorded in `gpui-client/GPUI_PIN`
  (`a3ce8d5e88e35697b2ba1cf9cb3c6816089b8281`, 2026-11-26). Its source is read, not remembered. GPUI itself is
  Apache-2.0; most other Zed crates are GPL and are neither depended on nor copied from. Changes GPUI needs are
  kept as patch files against the pin (`gpui-client/patches/`), applied by `gpui-client/tools/fetch-gpui.sh`.
- **Electron defines behaviour.** A feature's renderer code is read before it is ported. Protocol, media and
  crypto stay in `crates/decibell-core`; the client never re-implements a packet path.
- **Testing** runs against the local stack (`tools/dev/local-stack.sh`: central + three communities + seeded
  test accounts), never the owner's account or the live servers.
- **Linux is finished here**; Windows code must compile (the Windows Native Check workflow builds the core, the
  addon and this client on the branch); its hardware checks are listed under "Needs owner" in
  `gpui-client/HANDOFF.md`, with everything else only the owner can do (two-machine tests, visual sign-off,
  other GPUs).
- A parity row is **done only when it was run**, with a self-test where one is possible.

## 3. Measuring, and the targets

`tools/perf/measure.sh` samples a client's whole process tree from `/proc`: CPU as % of one core, private memory
(`Private_Clean + Private_Dirty` of `smaps_rollup`, the analogue of Windows' private working set), PSS, RSS,
threads, **voluntary context switches per second per thread** (the wakeup count, by thread name, so our own
threads can be told from the GPU driver's), GPU engine use and memory per process (`nvidia-smi pmon`, DRM
fdinfo). `startup.sh` times launch → first visible window (seen by the compositor) → settled. `install-size.sh`
measures a directory. The Electron client is measured first, on this machine, from a checkout built with the
`local-central` feature against the same local stack and fixtures the GPUI client uses.

Targets (the Qt design's §3 ratios; the absolute figure is the ratio applied to the Linux baseline, recorded in
`gpui-client/HANDOFF.md` once measured):

| Metric | Target |
|---|---|
| Idle private memory, busy channel open | ≤ 1/3 of Electron |
| Idle CPU | ≤ 1/10 of Electron; no timer of ours firing at idle (only the 30 s server pings) |
| Idle wakeups | ≤ 30 / s from our own threads; GPU driver threads reported beside it |
| Frames at idle | 0 rendered while nothing on screen changes |
| Startup to a visible window (warm) | ≤ 1/3 of Electron |
| Install size (with FFmpeg) | ≤ 1/4 of Electron |
| Watching 2 streams (1440p120 where the display allows) | CPU ≤ 1/3 of Electron, GPU ≤ Electron, no frame dropped or duplicated beyond the source's, presented-frame jitter p99 ≤ one refresh interval |
| Scrolling a 10k-message channel | every frame inside the refresh interval; a history page moves what is being read by 0 px |

Every phase logs its numbers against these. A feature that breaks one isn't done.

What the client does to meet them, as rules of the code:

- **No polling.** A timer exists only for a named, live reason (slowmode countdown, 45 s ring, auto-away
  threshold, a toast's lifetime) and is dropped when the reason ends.
- **No frames at idle.** Nothing animates off screen or while its state is not live; spinners, cursors and
  speaking rings stop with their cause. The client counts presented frames; the self-tests assert the counter
  stands still on an idle screen.
- **No per-frame allocation or copy on hot paths**: video (pooled packets from the socket to the decoder, the
  picture stays on the GPU), scrolling (row layouts cached by message identity and width; nothing is shaped
  again while only the offset changes), audio (the core's existing rings).
- **One UI wake per burst** of core events (§6.1), and stores notify only the views that read what changed.
- **Memory**: state is plain Rust keyed by uid / id; the message model keeps a window, not a channel's history;
  pictures are decoded at drawn size into a byte-bounded cache; fonts and assets are mapped from the binary,
  not copied to the heap; nothing is kept across restarts that Electron doesn't keep.

## 4. The core split (first, before any UI)

As the Qt design's §6, in steps; Electron passes its whole verification bar after each (renderer typecheck,
`npx napi build`, `cargo test --lib`, Windows Native Check on the branch):

1. **Move** `net/`, `media/`, `e2ee/`, `hotkeys/`, `config.rs`, `presence.rs`, `system_idle.rs`, `state.rs` and the
   prost build into `crates/decibell-core`; the four call-outs from `net/` into `commands/` become core functions;
   events go through an installed `EventSink` (untyped at this step: name + JSON, plus the two binary
   callbacks), the napi crate keeping its three ThreadsafeFunctions and the stream-frame shedding.
2. **Typed events**: `CoreEvent`, one variant per event with today's payload struct; `to_json()` gives the napi
   sink today's exact envelope, the GPUI sink matches on the variant with no JSON in between.
3. **Commands**: the bodies of the 132 `#[napi]` functions move to `decibell_core::api::<area>` as plain Rust
   returning `Result<T, CoreError>`; argument structs live in core (`#[cfg_attr(feature = "napi", napi(object))]`);
   the napi commands become one-line wrappers.
4. **Runtime**: core spawns through a `tokio::runtime::Handle` given to `init`, never a bare `tokio::spawn`, so
   calls from GPUI's threads work. The addon passes napi's runtime; this client owns a small one.
5. **Receive path without per-frame allocation**: reassembled frames and decrypt buffers come from a pool; the
   decoder hands the same buffer to FFmpeg refcounted.

No root Cargo workspace: `electron-client/native` and `gpui-client` each keep their own lockfile and target
directory and depend on the core by path.

## 5. GPUI at the pin

Filled from the source survey (`gpui-client/docs/gpui-notes.md`, every claim with a path and line at the pin).
The questions that shape the design, each answered there and summarised here once known:

- what draws a frame and what wakes at idle on Wayland, X11 and Windows;
- the text system's reach: variable-font axes (Inter `opsz`, Mona Sans `wght` / `wdth`), fractional sizes,
  hit-testing and ranges for selection, inline images for Twemoji;
- lists: measured heights and what a prepend does to the viewport (we expect to write our own list element);
- text input: GPUI has an input-handler protocol but no text field of its own; ours or a non-GPL crate;
- capture: rendering a window to a PNG without showing it;
- which default features and in-tree crates can be left out.

## 6. Architecture of the client

```
crates/decibell-core/            the shared core (§4)
gpui-client/
  Cargo.toml                     the client's workspace (own Cargo.lock, own target/)
  GPUI_PIN, patches/, tools/     the pin, patches against it, fetch-gpui.sh, gen-theme, fixtures, capture
  crates/
    decibell-theme/              tokens generated from electron-client/src/styles/globals.css (7 themes)
    decibell-ui/                 the design system as GPUI components (Button, Switch, Select, Modal, the
                                 settings frame, menus, tooltips, text field, icons, avatars, …)
    decibell-text/               rich text: the richText.ts subset → styled runs → layout, hit-testing,
                                 selection ranges, Twemoji, code highlighting, TeX
    decibell-video/              hardware decode and the external-texture element (§7)
    decibell-services/           what Electron's main process did (§8); no GPUI in it
    decibell-app/                the binary: stores, the event pump, features (auth, shell, servers,
                                 channels, chat, dm, friends, members, voice, call, streams, search,
                                 transfers, settings, profile, presence, hotkeys, e2ee)
  HANDOFF.md, PARITY.md, docs/
```

### 6.1 State and events

- **Stores are GPUI entities**, one per concern, mirroring the renderer's zustand stores so a ported feature
  reads like its original (`UsersStore` is the only source of names, keyed by uid; `ServersStore`,
  `ChannelsStore`, `MessagesStore`, `DmStore`, `VoiceStore`, `PresenceStore`, …). They hold plain Rust; views
  observe the store they read and are re-rendered only on its notification. Where the renderer selected a
  slice of a store to avoid re-renders, the store here notifies per key (a per-channel or per-user entity), so
  a message in one channel never re-lays-out another view.
- **Core → UI**: the client's `EventSink` pushes each `CoreEvent` onto a queue and wakes the UI thread only
  when the queue goes from empty to non-empty; the pump drains the whole queue in one turn and applies it to
  the stores, then GPUI draws once. Stream frames and thumbnails never go through the UI thread: frames go
  straight to the stream's decoder queue.
- **UI → core**: views call `decibell_core::api` through a thin `Core` handle that spawns on the client's tokio
  runtime (2 workers) and brings the result back to the UI thread; the UI thread never blocks on the runtime.
- **Settings** live in the core's `config.json`; the ~18 `localStorage` keys of the renderer move into it.

### 6.2 Theme, fonts, icons

- `gpui-client/tools/gen-theme` parses `globals.css` (the `@theme` block, `:root`, every `[data-theme]` block
  including shared ones, `var()` chains, `calc()` over the type scale) and writes `decibell-theme/src/generated.rs`:
  seven `static` palettes with every role resolved, plus radii, shadows, font roles and the type scale. The file
  is committed; the verification bar regenerates it and fails on a diff, so tokens can't drift. A theme switch
  swaps one pointer and repaints. Per-theme component rules in the CSS (console overrides, `.chrome-scope`,
  the image viewer's light stages) are ported by hand into the components, each citing its CSS line.
- Fonts are the renderer's files (Inter Variable, Mona Sans Variable, JetBrains Mono), embedded.
- Icons are the lucide SVGs the renderer uses, at its stroke widths, rasterised at device-pixel size and tinted
  at draw; Twemoji are the same SVG set as the renderer.

### 6.3 Chat

- **The message list is our own element.** The model holds the loaded window of a channel (paged around /
  before / after like the renderer), rows keyed by message identity (id, else nonce). Each row's height is its
  laid-out height for the current width, computed once and cached by (identity, content version, width); the
  scroll position is `{anchor row, offset, at bottom}`, so a page of history arriving above changes no pixel
  of what is being read, by construction. Only rows that intersect the viewport (plus overdraw) are painted.
- **Rich text** is parsed once per message (a port of `richText.ts`) into runs; `decibell-text` lays it out with
  GPUI's text system and paints text, inline-code chips, spoilers, emoji and selection itself; hover, links and
  selection are hit-tested inside the element. Code blocks are highlighted by a Rust highlighter covering the
  renderer's 27 grammars; `$TeX$` by a Rust TeX layout.
- **Selection spans rows**: a controller holds (row identity, offset) endpoints; rows paint their part; copy
  builds the text from the model, so rows scrolled away still copy.
- **Wheel scrolling** reproduces Chromium's on the same OS (distance per notch and the animation curve,
  measured from the Electron client on this machine), on wall time, one tick per frame while it runs and none
  after; touchpads pass through.
- **The composer** is our own multi-line input over GPUI's input-handler protocol (IME, selection, undo),
  with the renderer's live formatting preview, emoji / GIF picker, attachments, slowmode and pacing.

## 7. Video

The model of the Qt design's §8: the core's receive thread hands whole, decrypted, encoded frames to the
stream's decoder thread (a 3-frame queue; on overflow shed to the next keyframe and ask for one); FFmpeg decodes
on the GPU; the picture is converted and scaled on the GPU at the size it is drawn; the UI takes the newest
completed picture when it draws; a new picture asks for exactly one redraw; nothing redraws when no frame
arrives. One decoder per stream, however many places show it (grid tile, focus, mini-player); self-preview
decodes our own encoded frames like any other stream. Software decode only as a fallback.

How a decoded picture reaches a GPUI frame without a CPU copy (GPUI on Linux renders through wgpu), which
FFmpeg hwaccel feeds it per vendor and OS, the patch to GPUI and the measured numbers are in
`gpui-client/docs/video-design.md`.

## 8. What replaces Electron's main process and Chromium services

As the Qt design's §10, in `decibell-services` (plain Rust, shared by nothing else, no GPUI types) unless noted:

| Responsibility | Here |
|---|---|
| Video decode + render | §7 |
| Capture source list | Linux: the portal dialog, as today. Windows: DXGI outputs + `EnumWindows` producing the ids `source_id.rs` parses |
| Attachment upload / download / Range playback / save-as / Transfers / pacing | One HTTPS client (hyper + rustls, pinned to the community's fingerprint): resumable upload, downloads over Range, speed caps |
| Attachment encryption | Rust, byte-for-byte the format of `attachmentCrypto.ts` (tested against vectors from both TS modules) |
| Upload thumbnails, ThumbHash, EXIF orientation | Rust, off the UI thread |
| Inline video / audio attachments | Our FFmpeg player on the §7 path + the core's audio output |
| Link previews (with the private-network guard), GIF search | Ports of `linkPreview.ts`, `publicNet.ts`, `gifs.ts`: same caps, cache and Privacy toggles |
| E2EE at-rest key, saved sign-in | The OS keyring (Secret Service on Linux, Credential Manager / DPAPI on Windows) |
| UI sounds, mic test outside a call | The core's audio output, preloaded PCM |
| Attention, taskbar progress | The platform's urgency hint / `FlashWindowEx`, `ITaskbarList3` |
| Deep links, single instance | `decibell://` registration per OS, forwarded to the running instance |
| Global hotkeys | The core's listeners as today; the recorder maps GPUI key events to the `KeyboardEvent.code` names bindings use |
| Auto-away | The core's `system_idle.rs` on Wayland; one timer at the threshold elsewhere |
| Auto-update, crash reports | Last phase: an own updater against GitHub releases; the `sentry` crate, opt-in as today |

## 9. Fidelity and captures

Tokens are generated (§6.2). Both clients are fed the same deterministic fixtures through the local stack
(`tools/dev/local-stack.sh seed`). The GPUI client captures itself (`decibell --capture <png> --screen <name>
--theme <name>`), the Electron client is captured through its UI preview harness / CDP; a compare page shows
each pair. The owner judges the feel (scrolling, motion, DPI) and signs off the look.

## 10. Order of work

1. The core split (§4), Electron's bar after each step.
2. The skeleton: pin + patches, workspace, theme generator, fonts, window frame, the design-system components,
   the capture and self-test harness, the frame and wakeup counters.
3. Stream playback (§7).
4. Chat (§6.3).
5. Sign-in, servers, channels, DMs with E2EE, voice with MLS; then every other row of `docs/FEATURES.md` §1–§9,
   tracked in `gpui-client/PARITY.md` (per row: status, evidence, the Electron file it mirrors).
6. Packaging, auto-update, crash reports.

Each phase logs its numbers against §3 in `gpui-client/HANDOFF.md`.

## 11. Verification bar for the client

Before committing a change under `gpui-client/` (and Electron's full bar for anything under
`crates/decibell-core`):

```sh
cd crates/decibell-core && cargo test --lib
cd gpui-client && cargo test --workspace
cd gpui-client && cargo run --release -p decibell-app -- --selftest all     # hidden window, the local stack up
cd gpui-client && tools/gen-theme/check.sh                                    # regenerating changes nothing
```

and, for a change that could move a target, `tools/perf/measure.sh` on the affected scenario.

## 12. Risks

- **Parity is large**: the renderer is ~290 files / 62k lines of TS + TSX and Electron main 5.8k. The checklist
  keeps it honest; the shared core removes the protocol risk.
- **GPUI is a moving, unversioned target** with gaps a chat client meets early (text input, inline images in
  text, external textures, capture). The pin plus a small patch set contains it; each patch is written to be
  upstreamable.
- **Text fidelity**: Chromium and GPUI's text stack shape and rasterise the same fonts differently; variable
  axes may need a patch or static instances. The owner's eye decides.
- **Only NVIDIA hardware on hand**: AMD / Intel decode paths are reasoned and compile-checked until someone
  runs them ("Needs owner").
- **Windows is compile-checked only** in this session.
