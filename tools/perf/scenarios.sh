#!/usr/bin/env bash
# scenarios.sh: the client baseline, scripted. The same scenarios and measurements for every client, so the GPUI
# client is judged against the Electron client measured the same way on the same machine (the Windows method of the
# native-client design §2: 2 min settle, 300 samples at 1 s, focus controlled and logged, the channel opened without
# giving anything keyboard focus).
#
#   scenarios.sh <client> <scenario|all> [--quick] [options]
#   scenarios.sh <client> table [--quick]      rebuild the table from the newest result of each scenario
#
#   client      clients/<client>.sh: the hooks (clients/electron.sh documents them; clients/gpui.sh is the stub)
#   scenarios   install-size startup idle-home idle-channel idle-channel-hidden scroll    (all: in that order)
#     install-size   the installed app (measured only, never run) and this build
#     startup        warm: 1 discarded + 5 runs, saved sign-in, launch -> first visible window -> settled
#     idle-home      signed in, Home, window focused; settle, then samples
#     idle-channel   signed in, Daily Gaming #general open (10k messages), another window focused (the headline)
#     idle-channel-hidden  the same with the window minimized: the page is hidden and stops drawing, which is what
#                    Windows' occlusion tracking does to a covered window (the Windows baseline's "another window
#                    focused" run had the client covered). On Linux a covered window keeps drawing.
#     scroll         #general scrolled into history at constant speed from requestAnimationFrame, window on top
#     watch-1        in Daily Gaming / Lounge watching one live stream large (streambot.sh: a .dbv recording streamed
#                    by a headless perf4 through the whole path: relay, receive, FEC, MLS, decode, present), desktop
#                    focused, client visible; the client's own presented / received counts recorded with the samples
#     watch-2        the same with two streams watched (two streamer accounts with E2EE keys: --streamers)
#                    (watch-*: not part of `all`; the client needs the hooks client_voice_join / client_watch)
#   --dbv FILE[,FILE]    the recordings for watch-* (default gpui-client/vendor/streams/1080p60_h264.dbv; watch-2:
#                        1440p120_h264.dbv twice)
#   --streamers a[,b]    the stack accounts streaming them (default perf4[,<none>]: a user streams one stream)
#   --voice-channel NAME (default Lounge)
#   --quick              validation lengths: settle 15 s, 30 samples, 2 startup runs (+1 discarded), 1 scroll run;
#                        results get a -quick suffix and the table goes to results/<client>-baseline-quick.md
#   --settle S --duration S --interval S --startup-runs N --scroll-runs N --scroll-seconds S --scroll-speed PX
#   --server NAME --channel NAME (default Daily Gaming / general)
#
# Writes results/<client>-<scenario>-<YYYYmmdd-HHMMSS>[-quick]/ (the tool's own output + scenario.json with what the
# runner did: placement, focus, the client's display server, machine facts and load) and prints one table, also
# written to results/<client>-baseline[-quick].md. Run it on a quiet machine: see tools/perf/README.md.
set -euo pipefail

PERF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
RESULTS="$PERF/results"
UTIL=(python3 -B "$PERF/linux/scenario_util.py")
die() { echo "scenarios: $*" >&2; exit 1; }
say() { echo "[scenarios $(date +%H:%M:%S)] $*" >&2; }

client="${1:-}"; what="${2:-}"
[[ -n "$client" && -n "$what" ]] || { sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 2; }
shift 2
[[ -f "$PERF/clients/$client.sh" ]] || die "no clients/$client.sh"

quick=0 settle="" duration="" interval=1 srun="" scroll_runs="" scroll_secs=6 scroll_speed=1000
server="Daily Gaming" channel="general"
dbv="" streamers="perf4" voice_channel="Lounge"
while (($#)); do
  case "$1" in
    --quick) quick=1; shift ;;
    --settle) settle="$2"; shift 2 ;;
    --duration) duration="$2"; shift 2 ;;
    --interval) interval="$2"; shift 2 ;;
    --startup-runs) srun="$2"; shift 2 ;;
    --scroll-runs) scroll_runs="$2"; shift 2 ;;
    --scroll-seconds) scroll_secs="$2"; shift 2 ;;
    --scroll-speed) scroll_speed="$2"; shift 2 ;;
    --server) server="$2"; shift 2 ;;
    --channel) channel="$2"; shift 2 ;;
    --dbv) dbv="$2"; shift 2 ;;
    --streamers) streamers="$2"; shift 2 ;;
    --voice-channel) voice_channel="$2"; shift 2 ;;
    *) die "unknown option $1" ;;
  esac
done
if ((quick)); then
  : "${settle:=15}" "${duration:=30}" "${srun:=2}" "${scroll_runs:=1}"
else
  : "${settle:=120}" "${duration:=300}" "${srun:=5}" "${scroll_runs:=3}"
fi
suffix=""; ((quick)) && suffix="-quick"

# shellcheck source=clients/electron.sh
source "$PERF/clients/$client.sh"
has() { declare -F "$1" >/dev/null; }

newdir() { local d="$RESULTS/$client-$1-$(date +%Y%m%d-%H%M%S)$suffix"; mkdir -p "$d"; echo "$d"; }

meta() { "${UTIL[@]}" meta "$@"; }

begin() { # $1 dir $2 scenario: common metadata
  meta "$1" --facts --set "client=$client" --set "scenario=$2" --json "quick=$([[ $quick == 1 ]] && echo true || echo false)" \
    --set "client_describe=$(client_describe)" --set "started=$(date -Iseconds)" \
    --set "command=scenarios.sh $client $what$([[ $quick == 1 ]] && echo ' --quick')"
  local la; la="$("${UTIL[@]}" load)"
  local l1; l1="$(python3 -c 'import json,sys; print(json.loads(sys.argv[1])["loadavg"][0])' "$la")"
  if python3 -c 'import sys; sys.exit(0 if float(sys.argv[1]) > 2.0 else 1)' "$l1"; then
    say "WARNING: load average $l1: the machine is not quiet (busiest: $(python3 -c 'import json,sys; print(", ".join("%s %s%%" % (b["comm"], b["cpu_core_pct"]) for b in json.loads(sys.argv[1])["busiest"][:4]))' "$la"))"
    meta "$1" --note "load average $l1 at the start: not a quiet machine"
  fi
}

finish() { meta "$1" --set "ended=$(date -Iseconds)"; }

cleanup() {
  local rc=$?
  if has client_guard_stop; then client_guard_stop || true; fi
  if ((rc != 0)); then
    say "failed (exit $rc): stopping the client"
    client_stop >/dev/null 2>&1 || true
  fi
  "$PERF/winctl.sh" cleanup >/dev/null 2>&1 || true
  exit $rc
}
trap cleanup EXIT
trap 'exit 130' INT TERM

# ---------------------------------------------------------------------------------------------------- scenarios

sc_install_size() {
  local d; d="$(newdir install-size)"; begin "$d" install-size
  local labels=() paths=() label path
  while IFS=$'\t' read -r label path; do labels+=("$label"); paths+=("$path"); done < <(client_install_paths)
  say "install-size: ${paths[*]}"
  "$PERF/install-size.sh" --label "$client" --out "$d" "${paths[@]}" | grep -E '^/|apparent' >&2
  meta "$d" --json "labels=$(python3 -c 'import json,sys; print(json.dumps(sys.argv[1:]))' "${labels[@]}")"
  finish "$d"; DIRS+=("$d")
}

sc_startup() {
  local d; d="$(newdir startup)"; begin "$d" startup
  client_preflight
  if has client_startup_prepare; then say "startup: preparing (saved sign-in, Home)"; client_startup_prepare >>"$d/hooks.log" 2>&1; fi
  local argv=() sargs=()
  mapfile -d '' argv < <(client_startup_argv)
  mapfile -d '' sargs < <(client_startup_args)
  ((${#argv[@]})) || die "client_startup_argv gave nothing"
  if has client_guard_start; then client_guard_start "$d"; fi
  say "startup: $((srun + 1)) launches (1 discarded)"
  "$PERF/startup.sh" --runs "$srun" --discard 1 --label "$client startup" --out "$d" "${sargs[@]}" -- "${argv[@]}" \
    2>&1 | tail -6 >&2
  if has client_guard_stop; then client_guard_stop; fi
  [[ ! -s "$d/guard.log" ]] || die "the guard saw a live-central socket during startup: $(cat "$d/guard.log")"
  client_stop >>"$d/hooks.log" 2>&1
  meta "$d" --set "argv=${argv[*]}"
  finish "$d"; DIRS+=("$d")
}

# idle-home / idle-channel
sc_idle() {
  local name="$1" d pid
  d="$(newdir "$name")"; begin "$d" "$name"
  client_preflight
  say "$name: launching"
  pid="$(client_launch 2>>"$d/hooks.log")"
  [[ "$pid" =~ ^[0-9]+$ ]] || die "client_launch gave no pid ($pid)"
  client_ready >>"$d/hooks.log" 2>&1
  if [[ "$name" == idle-home ]]; then
    client_goto_home >"$d/goto.json" 2>>"$d/hooks.log"
  else
    client_goto_channel "$server" "$channel" >"$d/goto.json" 2>>"$d/hooks.log"
  fi
  "${UTIL[@]}" place --pid "$pid" >"$d/place.json"
  sleep 1
  if [[ "$name" == idle-channel ]] && has client_screenshot; then
    client_screenshot "$d/$client-$channel.png" >"$d/screenshot.json" 2>>"$d/hooks.log" || true
  fi
  local focus
  if [[ "$name" == idle-home ]]; then
    "$PERF/winctl.sh" activate "$pid" >>"$d/hooks.log" 2>&1; focus="client window focused (winctl activate)"
  elif [[ "$name" == idle-channel-hidden ]]; then
    client_minimize >"$d/minimize.json" 2>>"$d/hooks.log"
    "${UTIL[@]}" refresh --pid "$pid" >"$d/window.json" || true
    grep -q '"minimized": true' "$d/window.json" || die "the client's window is not minimized: $(cat "$d/window.json")"
    focus="client window minimized (its own title-bar button): page hidden"
  else
    "$PERF/winctl.sh" unfocus "$pid" >>"$d/hooks.log" 2>&1; focus="desktop focused (winctl unfocus), client visible"
  fi
  if has client_facts; then client_facts "$pid" >"$d/client-facts.json" 2>>"$d/hooks.log" || true; fi
  if has client_check; then client_check >>"$d/hooks.log" 2>&1; fi
  local margs=()
  if has client_measure_args; then mapfile -d '' margs < <(client_measure_args); fi
  say "$name: pid $pid; settle ${settle} s, then ${duration} samples at ${interval} s ($focus)"
  "$PERF/measure.sh" --pid "$pid" "${margs[@]}" --settle "$settle" --duration "$duration" --interval "$interval" \
    --focus-log --include-compositor --label "$client $name" --out "$d" --quiet 2>&1 | tail -40 >"$d/measure.txt"
  [[ -f "$d/summary.json" ]] || { cat "$d/measure.txt" >&2; die "measure.sh wrote no summary"; }
  if has client_check; then client_check >>"$d/hooks.log" 2>&1 || die "the client's sockets were wrong after the run"; fi
  ss -tunapH 2>/dev/null >"$d/sockets-at-end.txt" || true
  client_stop >>"$d/hooks.log" 2>&1
  meta "$d" --set "focus_mode=$focus" --json-file "placement=$d/place.json" --json-file "goto=$d/goto.json" \
    --json-file "client_facts=$d/client-facts.json" --set "settle_s=$settle" --set "duration_s=$duration"
  finish "$d"; DIRS+=("$d")
}

sc_scroll() {
  local d pid r
  d="$(newdir scroll)"; begin "$d" scroll
  client_preflight
  say "scroll: launching"
  pid="$(client_launch 2>>"$d/hooks.log")"
  [[ "$pid" =~ ^[0-9]+$ ]] || die "client_launch gave no pid ($pid)"
  client_ready >>"$d/hooks.log" 2>&1
  client_goto_channel "$server" "$channel" >"$d/goto.json" 2>>"$d/hooks.log"
  "${UTIL[@]}" place --pid "$pid" >"$d/place.json"
  sleep 8   # history loaded, pictures decoded (scroll-compare.ps1 waits the same)
  local hz; hz="$("${UTIL[@]}" refresh --pid "$pid" | python3 -c 'import json,sys; print(json.load(sys.stdin)["refresh_hz"])')"
  : >"$d/scroll.jsonl"
  for r in $(seq "$scroll_runs"); do
    "$PERF/winctl.sh" activate "$pid" >>"$d/hooks.log" 2>&1
    sleep 1
    "${UTIL[@]}" refresh --pid "$pid" >>"$d/window-before-run.jsonl"
    client_scroll "$scroll_secs" "$scroll_speed" "$hz" | tail -1 >>"$d/scroll.jsonl"
    say "scroll run $r: $(tail -1 "$d/scroll.jsonl" | cut -c1-200)"
  done
  "${UTIL[@]}" scroll-summary "$d" >/dev/null
  if has client_facts; then client_facts "$pid" >"$d/client-facts.json" 2>>"$d/hooks.log" || true; fi
  client_stop >>"$d/hooks.log" 2>&1
  meta "$d" --set "speed=$scroll_speed" --set "seconds=$scroll_secs" --set "refresh_hz=$hz" \
    --json-file "placement=$d/place.json" --json-file "client_facts=$d/client-facts.json"
  finish "$d"; DIRS+=("$d")
}

# watch-1 / watch-2: live streams from streambot.sh, watched through the whole path.
sc_watch() {
  local name="$1" count="$2" d pid
  has client_voice_join && has client_watch || die "clients/$client.sh has no voice hooks (client_voice_join, client_watch)"
  d="$(newdir "$name")"; begin "$d" "$name"
  local streams="$PERF/../../gpui-client/vendor/streams" files=()
  if [[ -n "$dbv" ]]; then IFS=, read -r -a files <<<"$dbv"; fi
  if ((${#files[@]} == 0)); then
    if ((count == 1)); then files=("$streams/1080p60_h264.dbv"); else files=("$streams/1440p120_h264.dbv" "$streams/1440p120_h264.dbv"); fi
  fi
  ((${#files[@]} == count)) || die "$name needs $count .dbv files (--dbv)"
  local accts=(); IFS=, read -r -a accts <<<"$streamers"
  ((${#accts[@]} >= count)) || die "$name needs $count streamer accounts with E2EE keys (--streamers a,b): the stack has perf3 and perf4, and perf3 is the watching client"
  if has client_voice_profile; then client_voice_profile; fi
  client_preflight
  say "$name: streambot: ${files[*]##*/} as ${accts[*]:0:$count}"
  "$PERF/streambot.sh" start --community "$server" --channel "$voice_channel" --accounts "$(IFS=,; echo "${accts[*]:0:$count}")" \
    "${files[@]}" 2>>"$d/hooks.log" || die "streambot did not start (see $d/hooks.log)"
  pid="$(client_launch 2>>"$d/hooks.log")"
  [[ "$pid" =~ ^[0-9]+$ ]] || { "$PERF/streambot.sh" stop; die "client_launch gave no pid ($pid)"; }
  client_ready >>"$d/hooks.log" 2>&1
  client_voice_join "$server" "$voice_channel" >"$d/goto.json" 2>>"$d/hooks.log"
  local uids=() a
  for a in "${accts[@]:0:$count}"; do
    uids+=("$(python3 -I -c 'import json,sys; print(next(x["uid"] for x in json.load(open(sys.argv[1]))["accounts"] if x["name"]==sys.argv[2]))' \
      "${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}/run/accounts.json" "$a")")
  done
  client_watch "${uids[0]}" >>"$d/goto.json" 2>>"$d/hooks.log"
  local u; for u in "${uids[@]:1}"; do client_watch_too "$u" >>"$d/goto.json" 2>>"$d/hooks.log"; done
  "${UTIL[@]}" place --pid "$pid" >"$d/place.json"
  sleep 6   # the first keyframe (the streamer answers the request with its next keyframe)
  "$PERF/winctl.sh" unfocus "$pid" >>"$d/hooks.log" 2>&1
  local focus="desktop focused (winctl unfocus), client visible, ${count} stream(s) watched"
  if has client_facts; then client_facts "$pid" >"$d/client-facts.json" 2>>"$d/hooks.log" || true; fi
  if has client_check; then client_check >>"$d/hooks.log" 2>&1; fi
  local margs=()
  if has client_measure_args; then mapfile -d '' margs < <(client_measure_args); fi
  say "$name: pid $pid; settle ${settle} s, then ${duration} samples at ${interval} s ($focus)"
  ( sleep "$settle"; for u in "${uids[@]}"; do has client_video_stats && client_video_stats "$u" >>"$d/video-stats-start.jsonl" 2>/dev/null; done ) &
  local stats_pid=$!
  "$PERF/measure.sh" --pid "$pid" "${margs[@]}" --settle "$settle" --duration "$duration" --interval "$interval" \
    --gpu pmon --focus-log --include-compositor --label "$client $name" --out "$d" --quiet 2>&1 | tail -40 >"$d/measure.txt"
  wait "$stats_pid" 2>/dev/null || true
  for u in "${uids[@]}"; do has client_video_stats && client_video_stats "$u" >>"$d/video-stats-end.jsonl" 2>/dev/null || true; done
  [[ -f "$d/summary.json" ]] || { cat "$d/measure.txt" >&2; client_stop; "$PERF/streambot.sh" stop; die "measure.sh wrote no summary"; }
  if [[ -s "$d/video-stats-start.jsonl" && -s "$d/video-stats-end.jsonl" ]]; then
    python3 -I - "$d" "$duration" <<'PY' >&2
import json, sys
d, dur = sys.argv[1], float(sys.argv[2])
a = [json.loads(l) for l in open(d + "/video-stats-start.jsonl")]
b = [json.loads(l) for l in open(d + "/video-stats-end.jsonl")]
rows = []
for s, e in zip(a, b):
    secs = (e["monotonicMs"] - s["monotonicMs"]) / 1000 or dur
    rows.append({k: round((e[k] - s[k]) / secs, 2) for k in ("received", "decoded", "presented", "replaced")} |
                {"dropped": e["dropped"] - s["dropped"], "codec": e["codec"], "path": e["path"], "size": f'{e["width"]}x{e["height"]}', "seconds": secs})
json.dump(rows, open(d + "/video-rates.json", "w"), indent=2)
for r in rows: print("[scenarios] video:", json.dumps(r))
PY
  fi
  client_stop >>"$d/hooks.log" 2>&1
  "$PERF/streambot.sh" stop 2>>"$d/hooks.log"
  meta "$d" --set "focus_mode=$focus" --json-file "placement=$d/place.json" \
    --json-file "client_facts=$d/client-facts.json" --set "settle_s=$settle" --set "duration_s=$duration" \
    --set "dbv=${files[*]}"
  finish "$d"; DIRS+=("$d")
}

table() {
  local out="$RESULTS/$client-baseline$suffix.md"
  local args=("$client" --out "$out")
  ((quick)) && args+=(--quick)
  "${UTIL[@]}" table "${args[@]}" "${DIRS[@]}"
  say "table written to $out"
}

newest() { # newest result dir of a scenario (quick or not, as asked)
  local s="$1"
  ls -d "$RESULTS/$client-$s-"* 2>/dev/null | grep -E -- "/$client-$s-[0-9]{8}-[0-9]{6}$suffix\$" | sort | tail -1
}

DIRS=()
case "$what" in
  all)
    t0=$SECONDS
    say "client $client: $(client_describe)"
    say "all scenarios$([[ $quick == 1 ]] && echo ' (quick)'): settle $settle s, $duration samples, startup $srun+1, scroll $scroll_runs x $scroll_secs s"
    sc_install_size; sc_startup; sc_idle idle-home; sc_idle idle-channel; sc_idle idle-channel-hidden; sc_scroll
    table
    say "done in $(( (SECONDS - t0) / 60 )) min $(( (SECONDS - t0) % 60 )) s"
    ;;
  install-size) sc_install_size; table ;;
  startup) sc_startup; table ;;
  idle-home|idle-channel|idle-channel-hidden) sc_idle "$what"; table ;;
  scroll) sc_scroll; table ;;
  watch-1) sc_watch watch-1 1; table ;;
  watch-2) sc_watch watch-2 2; table ;;
  table)
    for s in install-size startup idle-home idle-channel idle-channel-hidden scroll; do
      n="$(newest "$s")"; [[ -n "$n" ]] && DIRS+=("$n")
    done
    ((${#DIRS[@]})) || die "no results for $client$suffix"
    table
    ;;
  *) die "unknown scenario $what (install-size startup idle-home idle-channel idle-channel-hidden scroll watch-1 watch-2 all table)" ;;
esac
