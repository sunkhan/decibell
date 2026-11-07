# Client resource measurement

Client-agnostic scripts that measure the Electron client and the Qt client the same way, so every phase of the
Qt client is judged against the same baseline (`docs/superpowers/specs/2026-11-07-qt-native-client-design.md` §2).
Results land in `tools/perf/results/` (gitignored: raw samples are machine-specific); summaries go into the design
doc or the review log.

## Windows

```powershell
# Resource use of every process whose name starts with Decibell, 300 samples at 1 s after a 2 min settle
.\tools\perf\measure.ps1 -ProcessName Decibell -WarmupSeconds 120 -Seconds 300 -IncludeDwm `
    -Label "electron 0.9.4 idle, busy channel" -Out tools\perf\results\electron-idle-channel

# Startup: 1 discarded + 5 warm runs, launch -> visible window -> settled
.\tools\perf\startup.ps1 -Exe "$env:LOCALAPPDATA\Programs\decibell-electron-client\Decibell.exe" `
    -Out tools\perf\results\electron-startup
```

What `measure.ps1` reports (summed over the process tree; mean / p50 / p95 / max):

| Column | Meaning |
|---|---|
| `cpu_core_pct` / `cpu_machine_pct` | CPU as % of one logical core / of the whole machine |
| `private_ws_mb` | Private working set (Task Manager's "Memory") |
| `private_bytes_mb` / `ws_mb` | Commit charge / working set incl. shared pages |
| `ctx_switches_s` | Context switches per second over all threads: the wakeup proxy on Windows |
| `threads`, `handles`, `procs` | Counts |
| `gpu_3D_pct`, `gpu_VideoDecode_pct`, … | GPU engine utilisation by engine type |
| `gpu_dedicated_mb` / `gpu_shared_mb` | GPU memory |
| `dwm_gpu_3D_pct` | DWM's 3D time (`-IncludeDwm`), the composition cost both clients pay |

Everything comes from one `Get-Counter` stream (PDH computes the rates over the interval); the scripts add no
polling inside the measured app. Measure on a quiet machine: no build, download or update running.

`startup.ps1` signs in on every run when the client has a saved sign-in; keep `-Runs` small (central allows 30
password sign-ins per address, then one per 2 s).

## Linux

`measure.sh` (to come with the Linux half of the spike): `/proc/<pid>/status` + `smaps_rollup`, `pidstat -w`
(voluntary + involuntary context switches), and the GPU tool for the vendor (`nvidia-smi pmon`, `amdgpu_top`,
`intel_gpu_top`).

## Who wakes up

`threads.ps1 -ProcessName <name> [-Seconds 10] [-Top 10]` ranks every thread of the client by context switches per
second, with its name (`SetThreadDescription`) and the module + offset of its start address (Electron processes are
labelled by `--type`). It found Qt's `QDxgiVSyncThread` (279 wakeups/s on a 279 Hz panel, off with
`QT_D3D_NO_VBLANK_THREAD=1`) and NVIDIA's D3D11 driver thread (`nvwgf2umx.dll`, ~80–110/s in every process with a
D3D11 swap chain). Rust `std` threads all start at the same trampoline, so tell them apart by name.

## Watched streams (Qt client spike (a))

`video-compare.ps1 -Streams <a.dbv>[,<b.dbv>] [-Runs 2] [-Seconds 10] [-Resources] [-Maximized]` plays the same test
streams in the Qt client (`--video`) and in `electron-video/` (the client's Electron and Chromium switches,
StreamVideoPlayer's WebCodecs + canvas path, each frame sent from main over IPC), alternating, and measures every run
with the app's own report, `measure.ps1` (`-Resources`) and `framecount`. Results append as JSON lines to `-Out`
(default `results/video-compare.jsonl`). It refuses to start while a `decibell-qt` or `electron` process runs
(`measure.ps1` would sum it in). Each run puts a window on top for ~15 s: measure on a quiet machine.

- Test streams: `qt-client/tools/streamgen` (synthetic, the streamer's NVENC settings, the frame number as a
  barcode band). Build it and `framecount/` with CMake + Ninja inside vcvars64 (see each `CMakeLists.txt`).
- `framecount <window title part> <seconds> [--fps=120] [--tiles=N]` reads the barcode off the screen through DXGI
  Desktop Duplication on every DWM composition: frames displayed per second, frames never displayed, and the time
  between new frames, per tile. It is the only number that compares the two clients: Electron's own counter counts
  `drawImage` calls, and Chromium shows only the newest canvas per frame.
- Electron's main-process (Node) timers tick at Windows' 15.6 ms granularity (77 frames/s for a 120 fps stream), so
  the harness paces in the renderer (Chromium's timers are high-resolution) and pulls each frame from main.

### Real streams

`video-compare.ps1` also plays streams recorded from a live session (DBV2: the client's own frames with their arrival
times; both clients replay them on those times). Recording: build the native addon with the `stream-dump` cargo
feature (`npx napi build --platform --release --features stream-dump --js index.js --dts index.d.ts`, then
`node copy-dlls.cjs`; restore the regenerated `index.js` / `index.d.ts`), build the production renderer
(`npm run build:renderer && npm run build:tsc`), close the installed client (same profile) and run
`node_modules\electron\dist\electron.exe .` from `electron-client` with `DECIBELL_STREAM_DUMP=<dir>`; watch the stream.
`node dbv-cut.mjs <recording> <out.dbv> [seconds]` cuts a clean piece from the first keyframe and prints the
stream's rate, keyframes, bitrate and arrival spacing. No barcode in a real stream, so framecount reads nothing
there: compare resources and the apps' own counters.

## Chat scrolling and selection against the live client

- `scroll-compare.ps1 [-Runs 3] [-Seconds 6] [-Speed 1000]`: starts the installed client with a debugging port,
  opens DailyGaming #general (open-channel.mjs), then alternates the Qt client's `--bench-scroll` and
  `electron-scroll.mjs` (the client kept topmost while it is measured). It signs the client in: run it with the
  owner's go-ahead, and restart the client normally afterwards (the debugging port stays open while it runs).
- `electron-selection.mjs <port> <out.png>`: selects text across two messages in the open channel and saves a
  screenshot with Chromium's selection colours (#0539A4 with white text in Nocturne).
