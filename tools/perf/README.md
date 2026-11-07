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
