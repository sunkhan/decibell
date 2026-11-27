# media-host.sh: the GPUI client's `--check media-perf` as a process a script drives (sourced by scenarios.sh for the
# media-play-* scenarios; gpui-client/crates/decibell-chat-content/src/media_perf.rs is the other end).
#
# It is the release build with `dev-local`, signed in on the local stack as perf2 in a throwaway profile (a temp
# directory, removed at the end) on a private, empty session bus (no keyring, tray, portal or URL-handler registration
# of the desktop is touched), with the null audio sink (DECIBELL_MEDIA_OUTPUT=null: nothing audible). It uploads the
# clip to Daily Gaming #off-topic and deletes the message when it is told to quit. For the GPUI scenario it is the
# measured client; for another client it only keeps the clip posted while that client plays it.
#
#   media_host_start CLIP     start, wait for `media-perf: ready` (signed in, #off-topic open, the clip posted)
#   media_host_pid            the app's pid
#   media_host_cmd CMD ANSWER [SECONDS]   write CMD to the control FIFO, wait for `media-perf: ANSWER {json}`: the JSON
#   media_host_ready          the `ready` line's JSON (message, attachment, filename)
#   media_host_stop           quit (the message is deleted), wait for the exit, stop the bus, remove the profile;
#                             prints {"deleted": true|false, ...}
# State: $DECIBELL_DEV_DIR/gpui-run-media/ (pid, the app's log, the temp directory's path).

MH_GPUI_DIR="$(cd "$PERF/../../gpui-client" && pwd)"
MH_BIN="${GPUI_BIN:-$MH_GPUI_DIR/target/release/decibell}"
MH_DEV_DIR="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}"
MH_STATE="$MH_DEV_DIR/gpui-run-media"
MH_CENTRAL=127.0.0.1:18080
MH_ACCOUNT="${MEDIA_HOST_ACCOUNT:-perf2}"

_mh_alive() { local pid; pid="$(cat "$MH_STATE/pid" 2>/dev/null)" && [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; }

# Wait until the log has more than $2 lines starting with `media-perf: $1 `, at most $3 seconds.
_mh_wait() {
  local what="$1" before="$2" limit="${3:-60}" i
  for ((i = 0; i < limit * 10; i++)); do
    (( $(grep -c "^media-perf: $what " "$MH_STATE/app.log" 2>/dev/null || true) > before )) && return 0
    _mh_alive || { echo "media-host: the app exited while waiting for '$what':" >&2; tail -25 "$MH_STATE/app.log" >&2; return 1; }
    sleep 0.1
  done
  echo "media-host: timed out after $limit s waiting for '$what':" >&2; tail -25 "$MH_STATE/app.log" >&2
  return 1
}

media_host_pid() { cat "$MH_STATE/pid"; }

media_host_start() {
  local clip="$1" tmp password address buspid
  [[ -x "$MH_BIN" ]] || { echo "media-host: not built: (cd gpui-client && cargo build --release -p decibell-app --features dev-local)" >&2; return 1; }
  [[ -s "$clip" ]] || { echo "media-host: no clip $clip (tools/perf/media-clips.sh)" >&2; return 1; }
  python3 -I -c 'import socket; socket.create_connection(("127.0.0.1",18080),timeout=2).close()' 2>/dev/null \
    || { echo "media-host: the local stack is down: tools/dev/local-stack.sh up" >&2; return 1; }
  mkdir -p "$MH_STATE"
  _mh_alive && { echo "media-host: already running (pid $(media_host_pid))" >&2; return 1; }
  password="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["password"])' "$MH_DEV_DIR/run/accounts.json")"
  tmp="$(mktemp -d "${TMPDIR:-/tmp}/decibell-media-perf.XXXXXX")"
  echo "$tmp" >"$MH_STATE/tmp"
  mkdir -p "$tmp/profile" "$tmp/bus"
  cat >"$tmp/bus/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path=$tmp/bus/bus</listen>
<policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
EOF
  { read -r address; read -r buspid; } < <(dbus-daemon --config-file="$tmp/bus/bus.conf" --fork --print-pid=1 --print-address=1)
  [[ "$buspid" =~ ^[0-9]+$ ]] || { echo "media-host: dbus-daemon did not start" >&2; rm -rf "$tmp"; return 1; }
  echo "$buspid" >"$MH_STATE/bus.pid"
  mkfifo "$tmp/control"
  : >"$MH_STATE/app.log"
  ( exec env -u DECIBELL_PERF "DECIBELL_CENTRAL=$MH_CENTRAL" "DECIBELL_DEV_PASSWORD=$password" \
      "DECIBELL_USER_DATA_DIR=$tmp/profile" "DBUS_SESSION_BUS_ADDRESS=$address" DECIBELL_MEDIA_OUTPUT=null \
      "DECIBELL_MEDIA_CLIP=$clip" "DECIBELL_MEDIA_PERF_CONTROL=$tmp/control" \
      "$MH_BIN" --check media-perf --login "$MH_ACCOUNT" >>"$MH_STATE/app.log" 2>&1 ) &
  echo $! >"$MH_STATE/pid"
  _mh_wait ready 0 240 || { media_host_stop >/dev/null 2>&1; return 1; }
  if ss -tnpH 2>/dev/null | grep "pid=$(media_host_pid)," | grep -qF "178.104.131.247"; then
    echo "media-host: a socket to the live central!" >&2; kill -KILL "$(media_host_pid)"; return 1
  fi
}

media_host_ready() { grep '^media-perf: ready ' "$MH_STATE/app.log" | tail -1 | cut -d' ' -f3-; }

media_host_cmd() {
  local cmd="$1" answer="$2" limit="${3:-60}" before tmp
  tmp="$(cat "$MH_STATE/tmp")"
  _mh_alive || { echo "media-host: not running" >&2; return 1; }
  before="$(grep -c "^media-perf: $answer " "$MH_STATE/app.log" 2>/dev/null || true)"
  timeout 5 sh -c 'printf "%s\n" "$1" >"$2"' _ "$cmd" "$tmp/control" || { echo "media-host: the control FIFO is not read" >&2; return 1; }
  _mh_wait "$answer" "$before" "$limit" || return 1
  grep "^media-perf: $answer " "$MH_STATE/app.log" | tail -1 | cut -d' ' -f3-
}

media_host_stop() {
  local pid tmp buspid i deleted=false
  pid="$(cat "$MH_STATE/pid" 2>/dev/null || true)"; tmp="$(cat "$MH_STATE/tmp" 2>/dev/null || true)"
  if [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null; then
    [[ -p "$tmp/control" ]] && timeout 5 sh -c 'printf "quit\n" >"$1"' _ "$tmp/control" 2>/dev/null || true
    for ((i = 0; i < 400; i++)); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$pid" 2>/dev/null; then
      echo "media-host: still running 40 s after quit: SIGTERM" >&2
      kill -TERM "$pid" 2>/dev/null || true; sleep 2; kill -KILL "$pid" 2>/dev/null || true
    fi
  fi
  grep -q '^media-perf: deleted ' "$MH_STATE/app.log" 2>/dev/null && deleted=true
  buspid="$(cat "$MH_STATE/bus.pid" 2>/dev/null || true)"
  [[ -n "$buspid" ]] && kill "$buspid" 2>/dev/null || true
  [[ -n "$tmp" && -d "$tmp" && "$tmp" == */decibell-media-perf.* ]] && rm -rf "$tmp"
  rm -f "$MH_STATE/pid" "$MH_STATE/bus.pid" "$MH_STATE/tmp"
  printf '{"deleted": %s, "message": %s}\n' "$deleted" "$(grep '^media-perf: ready ' "$MH_STATE/app.log" 2>/dev/null | tail -1 | python3 -I -c 'import json,sys
try: print(json.loads(sys.stdin.read().split(" ", 2)[2])["message"])
except Exception: print("null")')"
  [[ "$deleted" == true ]]
}
