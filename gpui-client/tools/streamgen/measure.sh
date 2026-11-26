#!/usr/bin/env bash
# Measures stream playback: starts `decibell --screen video-test` on the given test streams, lets
# it settle, samples it with tools/perf/measure.sh and prints one summary block (CPU, memory,
# wakeups per thread, GPU, the hub's own counters). The numbers of gpui-client/HANDOFF.md §4
# "Stream playback" are this script's output.
#
#   tools/streamgen/measure.sh <label> <seconds> <a.dbv[,b.dbv]> [--mirror]
#   env: DECIBELL_BIN (default target/release/decibell), SETTLE (default 5),
#        WAYLAND_DISPLAY= (empty) for XWayland, DECIBELL_VIDEO=software for software decode
#
# Run it on a quiet machine: the load average at the start of the run is printed with the result.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
REPO="$(cd "$ROOT/.." && pwd)"
label="$1"; seconds="$2"; files="$3"; shift 3
BIN="${DECIBELL_BIN:-$ROOT/target/release/decibell}"
out="$REPO/tools/perf/results/video-$label"
mkdir -p "$out"
load="$(cut -d' ' -f1-3 /proc/loadavg)"
DECIBELL_VIDEO_STATS=1 "$BIN" --screen video-test --dbv "$files" "$@" \
  > "$out/app.log" 2>&1 &
pid=$!
trap 'kill $pid 2>/dev/null || true' EXIT
"$REPO/tools/perf/measure.sh" --pid "$pid" --settle "${SETTLE:-5}" --duration "$seconds" --label "$label" \
  --out "$out" --quiet >/dev/null
# The hub's counters at the start and the end of the sampled window (one line per stream per second).
python3 - "$out" "$label" "$load" "${SETTLE:-5}" "$seconds" <<'PY'
import csv, json, re, sys
out, label, load, settle, seconds = sys.argv[1], sys.argv[2], sys.argv[3], float(sys.argv[4]), float(sys.argv[5])
s = json.load(open(f"{out}/summary.json"))
m = s["summary"]
def mean(key):
    v = m.get(key)
    return v.get("mean") if isinstance(v, dict) else v
print(f"== {label}  (load average at start {load})")
print(f"CPU {mean('cpu_core_pct'):.2f} % of a core   private {mean('private_mb'):.1f} MB  pss {mean('pss_mb'):.1f} MB  rss {mean('rss_mb'):.1f} MB")
print(f"wakeups/s own {mean('own_wakeups_s'):.0f}  driver {mean('driver_wakeups_s'):.0f}  unnamed {mean('unnamed_wakeups_s'):.0f}   frames/s (KWin) {mean('frames_s')}  private dirty {mean('private_dirty_mb'):.1f} MB")
print(f"GPU sm {mean('gpu_sm_pct')} %  dec {mean('gpu_dec_pct')} %  fb {mean('gpu_fb_mb')} MB")
rows = list(csv.DictReader(open(f"{out}/threads.csv")))
def num(row, *keys):
    for k in keys:
        if k in row and row[k] not in ("", None):
            return float(row[k])
    return 0.0
groups = {}
for r in rows:
    name = re.sub(r"\d+$", "N", r.get("name", "?"))
    g = groups.setdefault(name, [0, 0.0, 0.0])
    g[0] += 1
    g[1] += num(r, "vol_s", "wakeups_s", "ctxsw_vol_s")
    g[2] += num(r, "cpu_core_pct", "cpu_pct")
for name, g in sorted(groups.items(), key=lambda kv: -kv[1][1]):
    if g[1] >= 0.5 or g[2] >= 0.05:
        print(f"  {name:<26} x{g[0]:<2} {g[1]:8.1f} wakeups/s {g[2]:6.2f} % cpu")
streams = {}
for line in open(f"{out}/app.log", errors="replace"):
    if line.startswith("video-stats"):
        f = dict(p.split("=", 1) for p in line.split()[3:] if "=" in p)
        t = float(line.split()[1][2:])
        streams.setdefault(line.split()[2], []).append((t, f))
for key, samples in streams.items():
    inside = [x for x in samples if settle <= x[0] <= settle + seconds + 0.5] or samples
    (t0, a), (t1, b) = inside[0], inside[-1]
    d = lambda k: int(b[k]) - int(a[k])
    span = max(t1 - t0, 1e-9)
    print(f"  {key}: {b['codec']} {b['size']} {b['path']}: in {span:.0f} s received {d('received')} decoded {d('decoded')} "
          f"presented {d('presented')} replaced {d('replaced')} dropped {d('dropped')} errors {b['errors']}; "
          f"decode p50/p99 {b['decode_ms_p50']}/{b['decode_ms_p99']} ms, arrival->publish {b['latency_ms_p50']}/{b['latency_ms_p99']} ms, "
          f"presented every {b['present_interval_ms_p50']}/{b['present_interval_ms_p99']} ms (source {1000 * span / max(d('received'), 1):.2f})")
PY
kill "$pid" 2>/dev/null || true
wait "$pid" 2>/dev/null || true
trap - EXIT
