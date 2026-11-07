# Qt client spike: report for the go / no-go (2026-11-07)

The spike of `docs/superpowers/specs/2026-11-07-qt-native-client-design.md` §12, Windows half: Qt 6.12 Quick +
Rust (cxx-qt) on the `qt-spike` branch, measured against Electron 0.9.4 on the owner's machine (i7-12700K, RTX 4080,
2560 × 1440 at 279 Hz). Every number below is from a **controlled session** (the GPU idle at P8 before each run,
nothing else running) unless marked. Details, method and every gotcha: `qt-client/HANDOFF.md` §2b–2f; raw results:
`tools/perf/results/` (local, gitignored).

**The owner's verdict is the next step. Recommendation: go**, with the Linux half (below) still to run.

## (a) Watching streams

Decode with FFmpeg's D3D11VA on the window's own device, the D3D11 video processor converting and scaling into a
texture the size it's drawn at, one image node: pictures never leave the GPU. Electron for comparison: a harness
running the client's exact player path (Electron 33 castlabs with the client's switches, WebCodecs prefer-hardware,
frames over IPC, drawImage into a capped canvas) on the same files. "Shown" is what reached the screen, read through
Desktop Duplication from a frame-number barcode in the test streams.

| Synthetic streams (the streamer's NVENC settings), 3 paired runs | Shown/s, skipped per 10 s: Qt / Electron | CPU, % of a core | Private memory, MB | GPU 3D % |
|---|---|---|---|---|
| 1440p120 H.264 | 119.9, 0.3 / 120.0, 0.7 | **7.3** / 32.7 | **69** / 169 | 2.9 / 4.1 |
| 1440p120 HEVC | 120.0, 0.3 / 119.8, 1.3 | **5.7** / 33.1 | **71** / 164 | 3.0 / 4.1 |
| 1440p120 AV1 | 120.0, 0.3 / 119.8, 1.7 | **7.8** / 31.5 | **57** / 174 | 3.1 / 4.3 |
| 2160p120 HEVC | 119.9, 0 / 119.9, 0 | **7.3** / 32.1 | **107** / 172 | 5.5 / 15.8 |
| 2 × 1440p120 | 120.0, 0.7 / 119.8, 2.7 | **18.5** / 52.6 | **98** / 203 | 3.5 / 4.5 |
| 1440p60 H.264 | 60 / 60 | **3.0** / 16.5 | **68** / 161 | 2.1 / 2.6 |
| 1440p120, maximized | 119.9, 0.7 / 119.9, 0 | **3.3** / 29.6 | **71** / 154 | 3.8 / 5.5 |
| 2160p120, maximized | 120.0, 0.7 / 119.8, 0.7 | **6.4** / 34.7 | **104** / 173 | 8.1 / 12.5 |

**The owner's real stream** (1440p120 AV1 at 20 Mbit/s from another machine, recorded at the receiver with the new
`stream-dump` build of the client and replayed on its recorded arrival times): Qt shows 1680 of 1680 frames, **5.0 %
CPU windowed / 5.7 % maximized, 58 MB**; Electron's player path 31.9 % / 26.7 %, 168 / 137 MB, all frames drawn.
**The installed Electron client watching that stream live: 62.6 % CPU (p95 85 %), 352 MB, 7 processes, 257
threads** — the new baseline row for the design's §2.

Against §3's video target (2 × 1440p120):
- **CPU ≤ 1/3 of Electron**: 18.5 % vs 52.6 % = 0.35 against Electron's player path alone; against the whole
  client the ratio is far lower (one live stream: 5–6 % vs 62.6 %), but Qt's figure doesn't yet include the shared
  core's receive and decrypt work (Electron's `decibell-video-recv` thread wakes ~4,100/s on its own), so the honest
  like-for-like waits for the core split. Met in substance, not yet provable.
- **GPU 3D ≤ Electron**: met everywhere (4K: 5.5 % vs 15.8 %).
- **No frame dropped beyond the source's**: met (Qt presented 1679–1681 of 1680 decoded per run; 0–2 skipped).
- **Jitter p99 ≤ one refresh**: met (p99 interval ~11.1 ms for an 8.33 ms frame on a 3.6 ms refresh).
- **Linux**: not run yet (VA-API / Vulkan Video, the dual-boot session).

Worse than Electron: **wakeups while playing** — Qt's process ~1.75× the context switches of Electron's player path
(GUI thread ~6 per video frame: the threaded render loop's GUI ↔ render handoff). Options in HANDOFF §4 (a video layer
outside the scene graph). **GPU memory at 4K** 358 vs 277 MB (spare decoder surfaces, tunable).

Under a game holding 85 % of the GPU (the first, uncontrolled runs) both clients fell short of 120 shown frames a
second, Qt 100–111, Electron 84–120.

## (b) The chat

A busy channel's panel in the app frame (title bar, horizontal server bar, sidebar, header, message list, composer,
member list), on a 10k-message fixture: markdown, inline code chips, fenced blocks, twemoji, replies, image grids.

- **Scrolling at 279 Hz** (3 pairs, 1000 px/s, 6 s; Electron on the real #general): Qt **1 missed vsync per run**,
  p50 3.57 ms; Electron 14–29 missed. Per frame: sync 0.03 ms, render 0.6–0.7 ms, Present 0.65 ms.
- **History that doesn't jump** (`--selftest history`): an older page arriving while reading moves the row being
  read 0 px; a new message while reading history 0 px; a new message at the bottom shows; paging triggers itself.
- **Wheel** matches Chromium's curve within ~1 px (100 px per notch, smoothstep over ~175 ms, retargets keep speed);
  keys use Blink's steps.
- **Selection** across messages with autoscroll, double-click word, Ctrl+C as displayed, link clicks; Chromium's
  colours (#0539A4, white text). Not yet: sender name and time selectable.
- **Idle**: own threads ~8 wakeups/s (target ≤ 30), 0 frames rendered while nothing changes; startup to a visible
  window 257 ms (target ≤ 300, Electron 904).
- **Memory**: UI 63–70 MB visible; with the core's upper bound (≤ 19 MB) **82–89 MB against the 82 MB target:
  not met yet, decided by the core's real share after the split**. Where it goes: NVIDIA's driver ~11 MB, Qt + QML +
  fonts + theme ~23 MB before any screen, the chat ~15 MB; pictures only ~2 MB.
- **Screenshots**: Qt in Nocturne and Matinee (`decibell-qt --theme <name> --capture f.png`); the live client in
  Nocturne with a selection (`tools/perf/electron-selection.mjs`). Local files, not committed (they show the
  community's real names and pictures): `C:\b\qt-nocturne.png`, `C:\b\qt-matinee.png`, `C:\b\electron-selection.png`.
  The owner reviewed the Nocturne frame earlier ("looks alright", the server bar horizontal as in the client).

Against §12's go criterion for (b): every frame at 279 Hz — 1 miss in ~1,680 frames per run (Electron 14–29);
0 px prepend movement — met; screenshots — the owner's call.

## Found and fixed along the way (Qt-side, all in HANDOFF)

- A crash on any scene-graph re-creation (a lost device): shared textures outlived their QRhi. Per-QRhi caches now.
- Qt's curve-rendered Shapes never draw again after a re-creation (Qt 6.12): icons, avatars and status dots are
  rasterised SVG now, identical at 4×.
- `ListView.itemAt` is wrong on a bottom-to-top list; `QSGTextNode`'s selection argument is an inclusive end, not a
  count.

## Not done in the spike

Linux (both halves); the shared-core split and with it the true memory and wakeup totals; install size; parity
beyond the chat panel and the video item.
