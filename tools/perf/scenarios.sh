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
#               watch-1 watch-2 media-play-h264 media-play-vp9                            (on their own)
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
#     media-play-h264  a video attachment played in Daily Gaming #off-topic: a 1080p60 H.264 + AAC clip (media-clips.sh)
#                    uploaded by the GPUI client's `--check media-perf` (perf2; deleted again at the end), its card
#                    clicked in the client's main window, MUTED (gpui: the null sink; electron: --mute-audio and the
#                    element muted). Measured before playing, playing with the window focused, playing with the
#                    desktop focused (the client visible), and after the player was stopped; the client's own frame
#                    counts over both windows (pictures presented against the file's rate, pictures dropped)
#     media-play-vp9   the same with a 1080p30 VP9 + Opus WebM
#                    (media-play-*: not part of `all`; `media-table` rebuilds results/media-play.md from every run of
#                    every client)
#   --media-window S     seconds of each playing window (default 20; --quick 8)
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
dbv="" streamers="perf4" voice_channel="Lounge" media_window=""
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
    --media-window) media_window="$2"; shift 2 ;;
    *) die "unknown option $1" ;;
  esac
done
if ((quick)); then
  : "${settle:=15}" "${duration:=30}" "${srun:=2}" "${scroll_runs:=1}" "${media_window:=8}"
else
  : "${settle:=120}" "${duration:=300}" "${srun:=5}" "${scroll_runs:=3}" "${media_window:=20}"
fi
suffix=""; ((quick)) && suffix="-quick"

# shellcheck source=clients/electron.sh
source "$PERF/clients/$client.sh"
# shellcheck source=media-host.sh
source "$PERF/media-host.sh"
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
  [[ -z "${MEDIA_GPU_PID:-}" ]] || kill "$MEDIA_GPU_PID" 2>/dev/null || true
  if [[ -f "$MH_STATE/pid" ]]; then
    say "stopping the clip's poster (its message is deleted): $(media_host_stop 2>&1 || true)"
  fi
  [[ -z "${MEDIA_STAGE:-}" ]] || rm -rf "$MEDIA_STAGE"
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


# media-play-h264 / media-play-vp9: an uploaded clip played from its card (README, "Attachment playback").
MEDIA_STAGE="" MEDIA_GPU_PID=""
media_measure() { # DIR PID PHASE SECONDS [measure args]: one measure.sh run into DIR/PHASE
  local d="$1" pid="$2" phase="$3" secs="$4"; shift 4
  local margs=()
  if has client_measure_args; then mapfile -d '' margs < <(client_measure_args); fi
  "$PERF/measure.sh" --pid "$pid" "${margs[@]}" --settle 0 --duration "$secs" --interval 1 --focus-log --include-compositor \
    --label "$client $phase" --out "$d/$phase" --quiet "$@" 2>&1 | tail -40 >"$d/$phase.txt"
  [[ -f "$d/$phase/summary.json" ]] || { cat "$d/$phase.txt" >&2; die "measure.sh wrote no summary for $phase"; }
}

sc_media() {
  local name="$1" clipname="$2" d pid w
  has client_media_play || die "clients/$client.sh has no media hooks (client_media_play, client_media_stats, ...)"
  d="$(newdir "$name")"; begin "$d" "$name"
  local src; src="$("$PERF/media-clips.sh" path "$clipname")"
  "$PERF/media-clips.sh" info "$clipname" >"$d/clip.json"
  # The upload gets a name of its own, so its card is found by it whatever else the channel holds.
  MEDIA_STAGE="$(mktemp -d "${TMPDIR:-/tmp}/decibell-media-clip.XXXXXX")"
  local file; file="$MEDIA_STAGE/perf-$(date +%H%M%S)-$$-${src##*/}"
  cp --reflink=auto "$src" "$file"
  local posted="${file##*/}"
  if ! has client_media_is_host; then client_preflight; fi
  say "$name: posting $posted to $server #off-topic (the GPUI client's --check media-perf, as $MH_ACCOUNT)"
  media_host_start "$file" 2>>"$d/hooks.log" || die "the clip was not posted (see $d/hooks.log)"
  media_host_ready >"$d/posted.json"
  if has client_media_is_host; then
    pid="$(media_host_pid)"
  else
    pid="$(client_media_open "$server" off-topic "$posted" 2>>"$d/hooks.log" | tail -1)"
    [[ "$pid" =~ ^[0-9]+$ ]] || die "the client did not open the clip's channel (see $d/hooks.log)"
  fi
  "${UTIL[@]}" place --pid "$pid" >"$d/place.json"
  if has client_facts; then client_facts "$pid" >"$d/client-facts.json" 2>>"$d/hooks.log" || true; fi
  "$PERF/winctl.sh" activate "$pid" >>"$d/hooks.log" 2>&1
  sleep 3
  say "$name: pid $pid; before playing (5 s)"
  uptime >"$d/uptime-before.txt"
  media_measure "$d" "$pid" before 5
  client_media_play "$posted" >"$d/play.json" 2>>"$d/hooks.log" || die "the clip did not play (see $d/hooks.log)"
  say "$name: playing: $(cut -c1-300 "$d/play.json")"
  sleep 2
  for w in focused unfocused; do
    if [[ $w == focused ]]; then
      "$PERF/winctl.sh" activate "$pid" >>"$d/hooks.log" 2>&1
    else
      "$PERF/winctl.sh" unfocus "$pid" >>"$d/hooks.log" 2>&1
    fi
    client_media_restart >>"$d/hooks.log" 2>&1 || die "the seek to the start failed (see $d/hooks.log)"
    sleep 1
    "${UTIL[@]}" load >"$d/$w-load.json"; uptime >"$d/uptime-$w.txt"
    # The whole GPU's decoder and 3D utilisation beside the per-process figures (other programs' work included).
    if command -v nvidia-smi >/dev/null; then
      nvidia-smi --query-gpu=utilization.decoder,utilization.gpu --format=csv,noheader,nounits -lms 500 >"$d/$w-gpu.csv" 2>/dev/null &
      MEDIA_GPU_PID=$!
    fi
    client_media_stats "$w-start" >"$d/$w-start.json" 2>>"$d/hooks.log"
    say "$name: playing, $w: $media_window samples at 1 s (load $(cut -d' ' -f1-3 /proc/loadavg))"
    media_measure "$d" "$pid" "$w" "$media_window" --gpu pmon
    client_media_stats "$w-end" >"$d/$w-end.json" 2>>"$d/hooks.log"
    if [[ -n "$MEDIA_GPU_PID" ]]; then kill "$MEDIA_GPU_PID" 2>/dev/null || true; wait "$MEDIA_GPU_PID" 2>/dev/null || true; MEDIA_GPU_PID=""; fi
  done
  client_media_halt >"$d/halt.json" 2>>"$d/hooks.log"
  if has client_media_gone; then
    media_host_stop >"$d/host-stop.json" 2>>"$d/hooks.log" || die "the poster did not delete its message (see $d/hooks.log, $MH_STATE/app.log)"
    client_media_gone "$posted" >"$d/gone.json" 2>>"$d/hooks.log" || true
  fi
  sleep 3
  say "$name: after stopping (5 s)"
  uptime >"$d/uptime-after.txt"
  media_measure "$d" "$pid" after 5
  if has client_check && ! has client_media_is_host; then client_check >>"$d/hooks.log" 2>&1 || die "the client's sockets were wrong after the run"; fi
  if has client_media_is_host; then
    media_host_stop >"$d/host-stop.json" 2>>"$d/hooks.log" || die "the client did not delete its message (see $d/hooks.log, $MH_STATE/app.log)"
  else
    client_stop >>"$d/hooks.log" 2>&1
  fi
  cp "$MH_STATE/app.log" "$d/media-host.log" 2>/dev/null || true
  rm -rf "$MEDIA_STAGE"; MEDIA_STAGE=""
  meta "$d" --set "focus_mode=before / focused: winctl activate; unfocused: winctl unfocus (desktop focused, client visible)" \
    --json-file "placement=$d/place.json" --json-file "client_facts=$d/client-facts.json" --json-file "clip=$d/clip.json" \
    --set "media_window_s=$media_window" --set "media=$(client_media_describe)" --json-file "host_stop=$d/host-stop.json"
  python3 -I "$PERF/linux/media_summary.py" run "$d" >&2
  finish "$d"; DIRS+=("$d")
}

# Every media-play run of every client, one table: $RESULTS/media-play[-quick].md.
media_table() {
  local out="$RESULTS/media-play$suffix.md" all=()
  mapfile -t all < <(ls -d "$RESULTS"/*-media-play-* 2>/dev/null | grep -E -- "-media-play-(h264|vp9)-[0-9]{8}-[0-9]{6}$suffix\$" | sort)
  ((${#all[@]})) || die "no media-play results$suffix"
  python3 -I "$PERF/linux/media_summary.py" table "$out" "${all[@]}"
  say "media table: $out"
}

table() {
  local out="$RESULTS/$client-baseline$suffix.md"
  local args=("$client" --out "$out")
  ((quick)) && args+=(--quick)
  "${UTIL[@]}" table "${args[@]}" "${DIRS[@]}"
  say "table written to $out"
}

# The watch scenarios' own table (the baseline table has no rows for them): $RESULTS/<client>-watch.md, one row per
# run, every watch run in $RESULTS (each run is taken twice: both rows stay).
watch_table() {
  local out="$RESULTS/$client-watch$suffix.md" all=()
  mapfile -t all < <(ls -d "$RESULTS/$client-watch-"* 2>/dev/null | grep -E -- "/$client-watch-[12]-[0-9]{8}-[0-9]{6}$suffix\$" | sort)
  python3 -I - "$out" "$client" "${all[@]}" <<'PY'
import json, os, sys
out, client, dirs = sys.argv[1], sys.argv[2], sys.argv[3:]
rows = []
for d in dirs:
    try:
        j = json.load(open(os.path.join(d, "summary.json"))); m = json.load(open(os.path.join(d, "scenario.json")))
    except (OSError, ValueError):
        continue
    s = j["summary"]; f = lambda k, st="mean", nd=1: "%.*f" % (nd, s[k][st]) if k in s else "–"
    try:
        v = json.load(open(os.path.join(d, "video-rates.json")))
    except (OSError, ValueError):
        v = []
    vid = "; ".join("%s %s %s: %.1f / %.1f / %.1f, %d dropped" % (r["codec"], r["size"], r["path"], r["received"],
                    r["decoded"], r["presented"], r["dropped"]) for r in v) or "–"
    load = (m.get("load_at_start") or {}).get("loadavg") or ["?"]
    rows.append("| %s | %s | %s (%s); swap %s | %s / %s | %s / %s / %s | %s / %s | %s | %s | %s | %s |" % (
        m["scenario"], ", ".join(os.path.basename(x) for x in (m.get("dbv") or "").split()) or "–",
        f("private_mb"), f("private_mb", "p95"), f("swap_mb"), f("cpu_core_pct", "mean", 2), f("cpu_core_pct", "p95", 2),
        f("own_wakeups_s", "p50", 0), f("driver_wakeups_s", "p50", 0), f("ctxsw_vol_s", "p50", 0),
        f("gpu_dec_pct"), f("gpu_sm_pct"), f("frames_s", "mean", 1), vid, load[0], os.path.basename(d)))
head = ["# %s watching live streams (Linux)" % client, "",
        "From `tools/perf/scenarios.sh " + client + " watch-1 | watch-2` (streambot.sh streams; the client in voice, the stream(s) "
        "watched large, the desktop focused, the client visible). Video: the client's own hub counters over the "
        "measured window, received / decoded / presented per second.", "",
        "| Scenario | Recording(s) | Private MB mean (p95); swapped out, mean | CPU %% core mean / p95 | Wakeups/s own / driver / all "
        "voluntary (p50) | GPU dec %% / SM %% | Frames to the compositor /s | Video | Load avg at start | Result |"
        .replace("%%", "%"), "|---|---|---|---|---|---|---|---|---|---|"]
open(out, "w").write("\n".join(head + rows) + "\n")
print("\n".join(head + rows))
PY
  say "watch table: $out"
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
  watch-1) sc_watch watch-1 1; watch_table ;;
  watch-2) sc_watch watch-2 2; watch_table ;;
  watch-table) watch_table ;;
  media-play-h264) sc_media media-play-h264 h264; media_table ;;
  media-play-vp9) sc_media media-play-vp9 vp9; media_table ;;
  media-table) media_table ;;
  table)
    for s in install-size startup idle-home idle-channel idle-channel-hidden scroll; do
      n="$(newest "$s")"; [[ -n "$n" ]] && DIRS+=("$n")
    done
    ((${#DIRS[@]})) || die "no results for $client$suffix"
    table
    ;;
  *) die "unknown scenario $what (install-size startup idle-home idle-channel idle-channel-hidden scroll watch-1 watch-2 watch-table media-play-h264 media-play-vp9 media-table all table)" ;;
esac
