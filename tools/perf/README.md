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

Bash entry points over Python 3 (stdlib + `/proc`; `jeepney` for D-Bus, `python-xlib` for the X11 fallback). No root,
no sysstat (`pidstat`), no `perf`, no xdotool. Built and checked on KDE Plasma 6.8 Wayland, kernel 7.3, RTX 4080 with
the proprietary driver (620.73). Every script takes `--help`, runs from any cwd and writes under `tools/perf/results/`.

```sh
# Resource use of a process tree: 2 min settle, then 300 samples at 1 s (same method as the Windows baseline)
tools/perf/measure.sh --pid <root-pid> --settle 120 --duration 300 --focus-log --label "gpui idle channel"
tools/perf/measure.sh --match '<regex over /proc/*/cmdline>' [--also-match '<regex>'] ...   # every match + descendants

# Startup: 1 discarded + 5 runs, launch -> first visible window -> settled
tools/perf/startup.sh --runs 5 -- <command> [args...]

# Install size (measures only, runs nothing)
tools/perf/install-size.sh /opt/Decibell <gpui install dir or binary>

# Windows through KWin: list | activate <pid> | unfocus <pid> | geometry <pid> <x> <y> <w> <h> | close <pid> | cleanup
tools/perf/winctl.sh list
```

### `measure.sh`

Samples the root and every descendant (re-discovered every sample: Electron spawns helpers), plus `--also-match`
processes and their descendants (a crashpad handler that was reparented away). Membership is sticky: a process seen
as a member stays one after reparenting. Processes and threads that appear between samples are counted from birth; the
CPU and wakeups a thread spends between the last sample and its exit are lost (`cpu_ticks_core_pct` keeps exited
threads' CPU). Outputs: `samples.csv` (every sample), `threads.csv` (every thread: totals and rates over the window,
kind, `wchan` at the end), `summary.json` (mean / p50 / p95 / max per metric, whole-window totals, per-process table,
top threads, windows counted for frames, machine facts with kernel / CPU / GPU + driver / displays and refresh rates
from `kscreen-doctor -j` / session type, the root's command line, sampler overhead) and `focus.csv` with `--focus-log`.

| Column | Meaning | Windows (`measure.ps1`) |
|---|---|---|
| `cpu_core_pct` / `cpu_machine_pct` | CPU as % of one core / of the machine: per-thread `schedstat` (ns) summed | same |
| `cpu_ticks_core_pct` | Same from `/proc/<pid>/stat` utime+stime: what top/ps show; 10 ms ticks, so per-sample values are quantised (0 / 1 / 2 %), the window mean is fine | — |
| `private_mb` | `smaps_rollup` Private_Clean + Private_Dirty: the headline memory figure | `private_ws_mb` |
| `private_dirty_mb` | Private_Dirty alone (≈ private anonymous memory; see caveats) | closest to `private_ws_mb` |
| `pss_mb`, `rss_mb`, `anon_mb`, `swap_mb` | `smaps_rollup` Pss / Rss / Anonymous / Swap | `ws_mb` ≈ `rss_mb`; no commit-charge twin (Linux overcommits) |
| `vmrss_mb`, `shmem_mb` | `/proc/<pid>/status` VmRSS (= ps / top RSS) and RssShmem | — |
| `ctx_switches_s`, `ctxsw_vol_s`, `ctxsw_invol_s` | Context switches / s over every thread (`/proc/<pid>/task/<tid>/status`; voluntary = the thread blocked and woke: the wakeup proxy) | `ctx_switches_s` |
| `own_` / `driver_` / `unnamed_ctx_switches_s` | The same split by thread kind (below) | `threads.ps1` by hand |
| `own_` / `driver_` / `unnamed_wakeups_s` | Voluntary switches only, by kind: **wakeups** (the GPUI design's D12 count is `own_wakeups_s`) | — |
| `gpu_sm_pct`, `gpu_mem_pct`, `gpu_enc_pct`, `gpu_dec_pct`, `gpu_fb_mb` | NVIDIA per process: SM (3D + compute), memory controller, NVENC, NVDEC busy %, framebuffer MB | `gpu_3D_pct`, —, `gpu_VideoEncode_pct`, `gpu_VideoDecode_pct`, `gpu_dedicated_mb` |
| `gpu_<engine>_pct`, `gpu_<region>_mb` | AMD / Intel (`--gpu fdinfo`): DRM fdinfo `drm-engine-*` busy %, `drm-resident-*` MB | `gpu_<engtype>_pct` |
| `frames_s` | Frames the tree's windows handed the compositor per second (KWin `damaged` events, timestamped in KWin) | — (`framecount` measures presented stream frames) |
| `focused` | 1 when the active window belongs to the tree (`--focus-log`; also `focus.csv` every 5 s) | the baseline's focus log |
| `compositor_cpu_core_pct`, `compositor_gpu_sm_pct` | `--include-compositor`: kwin_wayland + Xwayland CPU and SM %, the composition cost both clients pay | `dwm_gpu_3D_pct` |
| `procs`, `threads`, `fds` | Counts | `procs`, `threads`, `handles` |

**Thread kinds** (the `threads.ps1` question, answered per sample): `main` (tid = pid), `driver` (name matches
`--driver-threads`, default NVIDIA's `[vkps] Update`, `[vkcf] Analysis`, `[vkrt] Analysis`, `cuda0000…`), `unnamed`
(a non-main thread still carrying the process name), `own` (every other named thread). `own_wakeups_s` counts the
voluntary switches of `main` + `own`: the figure for the "≤ 30 wakeups / s from our own threads" target (D12);
`own_ctx_switches_s` adds preemptions, as the Windows counter does. NVIDIA's **GL** driver threads
have no name: Electron's GPU process holds 12–14 of them, each waking ~10 times a second on a futex (~120 / s in total;
gone with `--disable-gpu`, present only when `libnvidia-glcore` is loaded): the Linux counterpart of the Windows
`nvwgf2umx.dll` thread. They land in `unnamed`, so **name every thread the native client spawns**
(`std::thread::Builder::name`; tokio names its workers, rayon only with `thread_name`), or its own unnamed threads are
reported beside the driver's.

**GPU route**: `--gpu auto` uses NVML in-process (ctypes on `libnvidia-ml.so.1`: `nvmlDeviceGetProcessUtilization` +
`nvmlDeviceGet{Graphics,Compute}RunningProcesses_v3`, the calls behind `nvidia-smi pmon`), else DRM fdinfo. `--gpu pmon`
runs one long-lived `nvidia-smi pmon -d <interval> -s um` stream instead (same numbers, but `nvidia-smi` costs 2–3 % of
a core at 1 Hz against ~0.3 % for the NVML calls: 2.5–3.5 ms a sample). `--gpu fdinfo` reads `/proc/<pid>/fdinfo/*` of `/dev/dri` fds (`drm-engine-*`
ns or `drm-cycles-*` / `drm-total-cycles-*`, `drm-engine-capacity-*`, `drm-resident-*`, older amdgpu `drm-memory-*`),
counting each DRM client once by (pdev, client id): AMD, Intel i915 / xe and other DRM drivers (`amdgpu_top` and
`intel_gpu_top` read the same counters). The proprietary NVIDIA driver exposes no `drm-*` keys, so fdinfo gives nothing
there (checked); the fdinfo parser is checked against synthetic amdgpu and xe text only (no AMD / Intel hardware here).

**Frames**: a KWin script attaches to the tree's windows (membership is asked back over D-Bus) and sends the KWin-side
time of every `damaged` event, batched by a 250 ms single-shot `QTimer` that only runs while frames arrive (a
continuously rendering window costs KWin 4 D-Bus calls a second; an idle one nothing). The tool buckets the times into
the sample intervals after the run. Checked on a 280 Hz panel: es2gears_wayland and vkcube 280 / s in every sample,
glxgears through XWayland 279.7 / s against its own 279.9 FPS. Idle Chromium commits nothing; a blinking caret or a
focus change (client-side decorations) shows up as frames.

### `startup.sh`

`startup.sh [--runs 5] [--discard 1] [--settle-cpu 2 --settle-secs 2] [--timeout 60] [--ready-marker RE] -- cmd…`.
Each run starts the command in a new session / process group and records:

- `visible_ms`: launch → KWin's `workspace.windowAdded` for a normal / dialog / splash / utility window whose pid is in
  the launched tree. On KWin 6 that signal fires when the window has content: a Wayland client's first committed
  buffer, an X11 client's map with its first damage 1 ms later. The timestamp is KWin's own `Date.now()` (the D-Bus
  delivery adds ~0.1–1 ms, kept as `visible_rx_ms`); `first_damage_ms` is the window's first `damaged` after that.
  Checked against the X11 route on the same windows (kcalc through XWayland, both watchers at once): within 0.1–1.5 ms.
- `marker_ms` / `first_frame_ms` with `--ready-marker`: the first stdout / stderr line matching the regex. A named group
  `ns` / `us` / `ms` / `s` is read as a `CLOCK_MONOTONIC` timestamp printed by the app (exact even with buffered
  output), e.g. `--ready-marker 'first frame presented monotonic_ns=(?P<ns>\d+)'`. The output of every run is kept in
  `run-N.log`.
- `settled_ms`: from `visible`, the start of the first `--settle-secs` window in which the tree used under
  `--settle-cpu` % of one core (per-thread schedstat, polled every 250 ms, as `startup.ps1`), plus `cpu_ms_at_visible`
  and `cpu_ms_quiet_window`.

Closing: SIGTERM to the root process, then SIGTERM to the group and the rest of the tree after half of `--grace`, then
SIGKILL; the next run starts only when every process of the tree is gone. (Electron exits on a SIGTERM to its main
process in ~70 ms; a SIGTERM to the whole group kills the helpers first and leaves the main process spinning.)
`--absent RE` refuses to start while a matching process runs: a single-instance app hands the launch to the running
copy. Window backends: `kwin` (default when KWin scripting is on the session bus), `x11` (`_NET_CLIENT_LIST` +
`_NET_WM_PID` / X-Resource pid on the X root: other X11 window managers; under Wayland it sees XWayland windows only),
`none` (marker and settle only).

### KWin bridge (window detection, frames, focus, `winctl.sh`)

`linux/kwin.py` writes a JavaScript KWin script to a temp file, loads it with
`org.kde.kwin.Scripting.loadScript(path, name)`, starts it (`/Scripting/Script<id>` `run`), and hosts a D-Bus
service (a unique name per tool instance) the script reports to with `callDBus` (asynchronous: KWin never waits on the
tool). On exit, error or signal the script is unloaded and `isScriptLoaded` must say false. Every loaded script is
recorded with its owner in `$XDG_RUNTIME_DIR/decibell-perf-kwin-scripts.json`; each tool unloads scripts whose owner
died on start, and `winctl.sh cleanup` does it by hand (checked by SIGKILLing a measure mid-run). Each load makes KWin
log a few harmless `Skipped method "borderActivated"` lines in the journal. Not used: script `print()` (it does reach
the journal, but reading it back means following everything KWin logs), `dbus-monitor` (eavesdropping on our own
calls), polling `xprop` (X11 windows only).

`winctl.sh` verbs run as one-shot scripts on the topmost window of `<pid>`'s tree (or of every process matching a
regex given instead of a pid): `activate` (unminimizes), `unfocus [--to desktop|<pid>]` (if the tree's window is
active, activates the desktop window of its output, or the given app), `geometry` (frame geometry in logical px;
unmaximizes first; the client applies it asynchronously, the result is listed 300 ms later), `close` (like the
title-bar ✕). All verbs work on KWin 6.8; other compositors are not supported by `winctl.sh`.

### Caveats

- `smaps_rollup` walks every mapping under the target's mmap read lock: ~0.5 ms for a small process, 5–10 ms per
  sample for a 7-process Electron tree. `--mem-interval 5` reads it every 5 s (the rest stays at `--interval`).
- **Private vs the Windows private working set.** Private_Clean counts file pages mapped by this process only: a
  single-process client pays for its own binary and libraries there, while Electron's helpers share one binary
  (Shared_Clean). Windows counts image pages as shareable, never private. `private_dirty_mb` (≈ private anonymous
  memory) is the closer match to Windows' figure: quote both. PSS splits shared pages between the processes that map
  them: the fairest total footprint when comparing a multi-process app with a single-process one.
- `vmrss_mb` (ps / top) is above `rss_mb` by `shmem_mb`: shared-memory pages reached through special mappings the smaps
  walk skips (Wayland shm buffers, GPU driver mappings). vkcube: Rss 85.8 + RssShmem 9.9 = VmRSS 95.7 MB.
- NVIDIA per-process utilisation is the driver's latest sample per process, integer %, and absent for a process with
  no load in that period: fine for a stream decode, coarse for a light GUI (glxgears read 0–33 % from sample to
  sample at a steady 280 fps; es2gears_wayland showed no samples at all). `gpu_fb_mb` is the process's own
  allocations; memory the compositor shares with it (Windows' `gpu_shared_mb`) is not visible.
- `frames_s` counts damage events. XWayland clients reach KWin as X damage reports, which can coalesce while KWin is
  stalled.
- Wakeups: `/proc/<pid>/status` counts only the main thread, which is why every thread is read. Exited threads lose
  their last interval.
- `--match` never matches processes in the tool's own process group (a `| grep foo` it is piped into would match
  `foo`), nor its ancestors; it prints what it picked up. A script that starts the app with `&` and no job control
  shares that group: use `setsid` or `--pid`.
- Sampler overhead (measured by every run, in `summary.json`): a single process 0.16 % of one core without GPU
  sampling, 0.46–0.52 % with NVML (2.5–3.5 ms of NVML calls a sample); the 7-process Electron stand-in 0.95–1.34 %
  (`smaps_rollup` 4.7–6.6 ms + NVML ~3.5 ms a sample); `--gpu pmon` adds `nvidia-smi` at ~2 %. 2–15 wakeups / s of
  its own; nothing runs inside the measured app.

### Measuring the two clients

**Electron client**: built from an isolated checkout with the `local-central` feature, run against the local test
stack in an isolated profile (GPUI design §3), never `/opt/Decibell`, which signs in to the live account.
`tools/perf/electron-local.sh` (`build` / `run` / `open` / `blur` / `pid` / `stop`) does that; every process of its
instance, crashpad included (`--database=<profile>/…`), carries the profile path, so match on it:

```sh
P="$HOME/.cache/decibell-dev/electron-profile/user-data"   # electron-local.sh's --user-data-dir (not the profile
                                                         # root: its throwaway keyring daemon carries that path)
tools/perf/electron-local.sh run && tools/perf/electron-local.sh open '<server>' '<channel>'
tools/perf/winctl.sh unfocus "$(tools/perf/electron-local.sh pid)"
tools/perf/measure.sh --match "$P" --settle 120 --duration 300 --focus-log --label "electron <ver> idle channel, linux"
```

By hand: start Electron with `setsid … --user-data-dir=<dir> --remote-debugging-port=<port>` and `--match` on
`<dir>` (zygotes and renderers are descendants of the main process either way). Startup: `startup.sh` must launch the
client process itself (not a wrapper that backgrounds it), with `--absent "$P"` (the single-instance lock would hand
the launch to a running copy); like `startup.ps1` it signs in on every run, so keep `--runs` small. Expect `unnamed`
≈ 120–130 wakeups / s from NVIDIA's GL threads in the GPU process.

**GPUI client**: `measure.sh --pid <pid>` (one process); `startup.sh --ready-marker '<its first-frame line with
(?P<ns>\d+)>' -- <binary>` so `first_frame_ms` (presented frame) sits beside `visible_ms` (first buffer at KWin).
`frames_s` at idle should be 0. GPUI on Linux renders through wgpu (Vulkan on this box): NVIDIA's `[vk…]` threads
land in `driver`; name the client's own threads so `own_wakeups_s` is the D12 figure. `install-size.sh` on the release
binary plus whatever it ships beside it.

Stand-in numbers from the validation runs on this box (2026-11-26; not Decibell): a minimal Electron 42 app (7
processes, 80 threads, a page that updates a counter once a second), unfocused: 0.49 % of one core, private 181 MB
(private dirty 89, PSS 283, VmRSS 709 of which shmem 37), 188 context switches / s, wakeups own 51 / s + NVIDIA
unnamed 130 + `[vk…]` 4, frames 1.00 / s; startup visible 657 ms median (639–678). kcalc visible 55.6 ms (Wayland),
glxgears 47.6 ms (XWayland). `/opt/Decibell` 416.8 MB apparent, 80 files.

### Taking the client baseline (`scenarios.sh`)

```sh
tools/dev/local-stack.sh up                       # set up + seeded once (tools/dev/README.md); `status` to check
tools/perf/electron-local.sh build                # first time ~4 min (release addon), then seconds
tools/perf/electron-local.sh run && tools/perf/electron-local.sh signin    # first time only (saved sign-in)
tools/perf/scenarios.sh electron all              # ~24 min; results/electron-baseline.md
tools/dev/local-stack.sh down
```

`scenarios.sh <client> <scenario|all> [--quick]` runs, for any client with a `clients/<client>.sh` (the hooks:
launch, ready, goto-home, goto-channel, minimize, stop, …; `clients/electron.sh` documents them, `clients/gpui.sh` is the
stub): `install-size` (the installed app, measured only, and the build), `startup` (1 discarded + 5 warm runs with
the saved sign-in, from Home), `idle-home` (window focused), `idle-channel` (Daily Gaming #general, the 10k-message
fixture, opened through CDP so nothing has keyboard focus; the desktop focused with `winctl unfocus`, the client
visible: the headline), `idle-channel-hidden` (the same, minimized), `scroll` (`electron-scroll.mjs`: 1000 px/s for
6 s, ×3, window activated, missed vsyncs against the refresh of the output the window is on). Each idle scenario
places the window at 1280×800 in the centre of the primary output, settles 120 s and takes 300 samples at 1 s with
`--focus-log --include-compositor`. Every scenario writes `results/<client>-<scenario>-<time>/` (the tool's output +
`scenario.json`: placement, focus mode, the client's display server, machine facts, the load at the start); the
table goes to `results/<client>-baseline.md`. `--quick` (15 s settle, 30 samples, 2 startup runs, 1 scroll run; ~4
min, `-quick` suffixes, `results/<client>-baseline-quick.md`) is for checking the pipeline, not for numbers;
`scenarios.sh <client> table` rebuilds the table from the newest results.

**Quiet machine**: no compile, download, update, video, game or animated page running; nothing that opens windows
(a test that spawns windows takes focus and covers the client; the focus log shows it as `focused` < 100 % on
idle-home or as another `active_class`); don't use the mouse or keyboard while it runs (idle-home and scroll activate
the client, idle-channel focuses the desktop). The runner warns when the 1-minute load average is above 2 at the
start of a scenario and records the busiest processes. Rough durations: startup 1 min, each idle scenario 7.5 min,
scroll 1 min.

**What `electron-local.sh` isolates** (header of the script for details): a git worktree of the current commit at
`~/.cache/decibell-dev/electron-baseline` (`build --ref <commit>` moves it; node_modules and the static FFmpeg are
symlinked from the main tree; own `CARGO_TARGET_DIR`), packaged with `electron-builder --linux dir` exactly as shipped
(asar, unpacked addon) minus `sentry.json` / `gifs.json` / `package-type`; the profile in
`~/.cache/decibell-dev/electron-profile/` (`user-data/` = `--user-data-dir`: Chromium profile, `config.json` with the
saved sign-in, `cert_pins.json`, `e2ee/`; `keyring/`: a throwaway gnome-keyring on a private session bus, so
safeStorage never reaches the desktop's KWallet / keyring); `xdg-settings` shimmed (the app re-registers the
`decibell://` handler on every start). Before every launch it proves the addon honours `DECIBELL_CENTRAL` (loads it
in an empty network namespace with a listener on 127.0.0.1:18080), and after it checks the sockets (central
127.0.0.1:18080, nothing to the live address; a wrong pin for the live central in `cert_pins.json` would stop a
sign-in there anyway). `stop` reports whether the owner's profile, keyrings or `mimeapps.list` changed.

**Not comparable to the installed app, by construction**: the installed `/opt/Decibell` also runs Sentry (DSN baked:
main + renderer SDKs), checks GitHub for updates 5 s after load (`resources/package-type` = deb), and keeps its
safeStorage key in KWallet; this build has none of that and uses libsecret on a private bus (no portal, a11y or
notification services there). The idle and scroll scenarios run with a CDP port open (no client attached while
measuring); startup runs without. Electron 33 runs through **XWayland** (the app's `ozone-platform-hint=auto` switch
comes too late; the shipped `.desktop` passes no flags), so composition goes through Xwayland and Chromium sees no
occlusion: a covered window keeps drawing. Electron itself contacts Google: the component updater
(`update.googleapis.com`, castlabs' Widevine component) within the first second of every launch, and the
spellchecker downloads `en-US-10-1.bdic` once per new profile.

**The breathing glow**: the active server tile's `ActiveTileGlow` is an infinite opacity animation, composited but
still one frame per vsync, so with a channel open and the window visible the client draws at the panel's refresh
(quick run: ~275 frames / s, ~15 % of one core at 280 Hz). On Windows a covered window is hidden by Chromium's
occlusion tracking and stops drawing. The Windows baseline's idle-channel run (0.58 %, "another window focused")
can only have had the client covered (a visible client would have drawn the glow at 279 Hz), so its Linux
counterpart is `idle-channel-hidden`; `idle-channel` is what a visible, unfocused client costs on Linux.

### Wheel and key scrolling (`electron-wheel-linux.py`)

How Chromium animates the message list on a wheel notch or a scroll key, measured on the Electron client so the GPUI
client can match it. Results and the derived spec: `gpui-client/docs/wheel-scroll.md`.

```sh
tools/perf/electron-local.sh run && tools/perf/electron-local.sh open "Daily Gaming" general
tools/perf/electron-wheel-linux.py --notches 1 --direction down --start middle --runs 8 --trace --label notch
tools/perf/electron-wheel-linux.py --notches 3 --spacing-ms 30 --start middle --runs 3 --trace     # retargets
tools/perf/electron-wheel-linux.py --pattern 'd@0,u@60' --start middle --runs 3 --trace            # reversal
tools/perf/electron-wheel-linux.py --key PageDown --start middle --runs 3                         # keys (focus!)
tools/perf/electron-wheel-linux.py --key ArrowDown --hold-ms 1500 --start middle                  # auto-repeat
tools/perf/electron-wheel-linux.py --inject cdp --notches 1 --start middle                         # precise deltas
tools/perf/electron-wheel-linux.py --reanalyze tools/perf/results/wheel-linux-<…>                  # no input
```

- **Input is XSendEvent, not XTest.** Under KWin, Xwayland forwards XTest through libei and KWin pops a "Remote
  Control: python3 is asking to control input devices" prompt (`kwin_eis_prompter`) for the owner; XTest also moves
  the real pointer. The tool sends core ButtonPress 4 / 5 and KeyPress / KeyRelease straight to the client's X window
  with `CLOCK_MONOTONIC` ms timestamps: Chromium turns a core wheel button into the same 120 px, non-precise wheel
  event a real notch makes (the curve code from there on is the real one), no pointer moves, and the wheel needs no
  focus. Keys do: key runs activate the window through KWin (and hand focus back after), and focus the list
  programmatically (Blink's key scrolling starts at the focused / last-pressed node). A run whose keys arrive while
  another window took focus prints `no input events reached the page`. CDP `Input.dispatchMouseEvent` wheels are
  precise-pixel and never animate (`--inject cdp` shows exactly that).
- **Window**: placed at 1280×800 in the centre of the primary output and kept above for the burst, then put back
  (`--no-place` leaves it, e.g. when another tool's window sits there). Each invocation is a few seconds per run.
- **Clocks**: rAF timestamps count from navigation start, `performance.now()` / `event.timeStamp` from
  `performance.timeOrigin`, 52 ms later in this page; every invocation calibrates the offset exactly with a short trace
  (rAF times snapped onto `Scheduler::BeginImplFrame` frame_times). Uncorrected, every onset looks ~52 ms late.
- **Output**: `results/wheel-linux-<time>[-label]/run-N.json` (frames, events, sent events, trace subset, analysis)
  and `summary.json`; one line per run: distance, onset, duration, dropped frames, free fits (ease-in-out bezier,
  smoothstep, exponential) for single moves, and the error of a re-implementation of cc's
  `ScrollOffsetAnimationCurve` driven by the run's own timing (with `--trace`: Chromium's traced processing times and
  frame receipts, 0.13–0.23 px rms). The content position is scrollTop corrected by an anchor row, so history pages
  prepended mid-animation do not read as jumps.

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

## Live streams on the local stack (`streambot.sh`)

`streambot.sh` puts real screen streams on the local test stack without capture or an encoder, so any client watches
**the same live streams through the whole path** (community relay → the client's core: receive, FEC, MLS decrypt →
its decoder → its screen): the GPUI client and the Electron client measured the same way.

```sh
tools/perf/streambot.sh start gpui-client/vendor/streams/1080p60_h264.dbv            # perf4 streams into Daily Gaming / Lounge
tools/perf/streambot.sh start --accounts perf4,<keyed account> a.dbv b.dbv           # two streams: two accounts
tools/perf/streambot.sh status
tools/perf/streambot.sh stop                                                          # announced as ended, no process left
```

Each streamer is `decibell-headless` (`cargo build --release -p decibell-model --features local-central --bin
decibell-headless` in `gpui-client/`): it signs in as a stack account **with E2EE keys** (MLS voice needs them; the
stack's perf3 and perf4 have them, passphrase `decibell-m3-test-passphrase`), joins the channel muted and deafened, waits
for MLS, then runs the headless `stream-file` step: `start_screen_share` with `native_encode: false` (the renderer's
WebCodecs path: the core announces the stream and makes its sender, MLS-sealed like any stream) and the recording's
frames through `api::streaming::send_video_frame` at its timing (DBV1 at its fps, DBV2 on its arrival times), looped. A
watcher's keyframe request (the core's `keyframe_requested`) makes it jump to the recording's next keyframe, as the
renderer forced one. A user streams one stream: N streams need N keyed accounts. State, logs and profiles live in
`$DECIBELL_DEV_DIR/streambot/`. Loopback only; nothing plays or captures sound.

The watch scenarios run it for you: `scenarios.sh gpui watch-1` (one 1080p60 H.264 stream, or `--dbv`) and `watch-2`
(two streams, `--streamers a,b`): the client joins Lounge as perf3 (its own profile), unlocks its keys, watches the
stream(s) large, the desktop takes the focus with the client visible, and `measure.sh --gpu pmon` samples it (CPU,
private memory, wakeups per thread, GPU sm / dec). The GPUI client's hub counters before and after the window
(`decibell: video-stats` over the control pipe) give `video-rates.json`: received / decoded / presented / replaced per
second and frames dropped. `clients/electron.sh` has no voice hooks yet (see the notes in that file).

## Chat scrolling and selection against the live client

- `scroll-compare.ps1 [-Runs 3] [-Seconds 6] [-Speed 1000]`: starts the installed client with a debugging port,
  opens DailyGaming #general (open-channel.mjs), then alternates the Qt client's `--bench-scroll` and
  `electron-scroll.mjs` (the client kept topmost while it is measured). It signs the client in: run it with the
  owner's go-ahead, and restart the client normally afterwards (the debugging port stays open while it runs).
- `electron-selection.mjs <port> <out.png>`: selects text across two messages in the open channel and saves a
  screenshot with Chromium's selection colours (#0539A4 with white text in Nocturne).
