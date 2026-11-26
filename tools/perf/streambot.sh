#!/usr/bin/env bash
# streambot.sh: real screen streams on the local test stack, without capture or an encoder. Each streamer is a
# `decibell-headless` process (gpui-client's state model + the shared core) that signs in as a stack account with E2EE
# keys, joins a voice channel muted and deafened (MLS ready), and streams a `.dbv` recording through the core's
# `send_video_frame` (the renderer's WebCodecs path: announced, packetised, MLS-sealed and relayed like any stream),
# looping at the file's timing and answering keyframe requests with the file's next keyframe. Any client (the GPUI
# client, the Electron client) can then watch the same streams through the whole path.
#
#   streambot.sh start [options] FILE.dbv [FILE.dbv ...]   one streamer per file; waits until each is live
#   streambot.sh stop                                      stop every streamer (announced as ended), no process left
#   streambot.sh status                                    the running streamers and their logs' last lines
#
# Options (start):
#   --community NAME     default "Daily Gaming"
#   --channel NAME       the voice channel, default Lounge
#   --accounts a,b,...   the stack accounts that stream, one per file, in order (default perf4). Each must have E2EE
#                        keys on the stack (MLS voice needs them); the stack's perf3 and perf4 do (passphrase below).
#                        A user streams one stream: two streams need two accounts.
#   --passphrase TEXT    their E2EE passphrase (default decibell-m3-test-passphrase)
#   --headless PATH      default gpui-client/target/release/decibell-headless
#                        (cargo build --release -p decibell-model --features local-central --bin decibell-headless)
#
# Against the local stack only: DECIBELL_CENTRAL must be a loopback address (the headless binary refuses anything else),
# taken from tools/dev/local-stack.sh client-env when unset. State: $DECIBELL_DEV_DIR/streambot/ (profiles, logs, pids).
# Nothing plays or captures sound: the streamers are muted and deafened and send video only.
set -euo pipefail

PERF="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
ROOT="$(cd "$PERF/../.." && pwd)"
DEV_DIR="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}"
STATE="$DEV_DIR/streambot"
die() { echo "streambot: $*" >&2; exit 1; }
say() { echo "[streambot $(date +%H:%M:%S)] $*" >&2; }

usage() { sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; }

central() {
  if [[ -z "${DECIBELL_CENTRAL:-}" ]]; then
    eval "$("$ROOT/tools/dev/local-stack.sh" client-env)"
  fi
  local host="${DECIBELL_CENTRAL%:*}"
  case "$host" in
    127.*|localhost|\[::1\]|::1) ;;
    *) die "DECIBELL_CENTRAL=$DECIBELL_CENTRAL is not loopback: streambot runs against the local stack only" ;;
  esac
  export DECIBELL_CENTRAL
}

cmd_start() {
  local community="Daily Gaming" channel="Lounge" accounts="perf4" passphrase="decibell-m3-test-passphrase"
  local headless="$ROOT/gpui-client/target/release/decibell-headless" files=()
  while (($#)); do
    case "$1" in
      --community) community="$2"; shift 2 ;;
      --channel) channel="$2"; shift 2 ;;
      --accounts) accounts="$2"; shift 2 ;;
      --passphrase) passphrase="$2"; shift 2 ;;
      --headless) headless="$2"; shift 2 ;;
      -h|--help) usage; exit 0 ;;
      -*) die "unknown option $1" ;;
      *) files+=("$1"); shift ;;
    esac
  done
  ((${#files[@]})) || die "no .dbv file given"
  [[ -x "$headless" ]] || die "$headless is not built (cargo build --release -p decibell-model --features local-central --bin decibell-headless)"
  IFS=, read -r -a acct <<<"$accounts"
  ((${#acct[@]} >= ${#files[@]})) || die "${#files[@]} streams need ${#files[@]} accounts with E2EE keys (--accounts a,b): a user streams one stream"
  central
  python3 -I -c 'import socket,sys; h,p=sys.argv[1].rsplit(":",1); socket.create_connection((h,int(p)),timeout=2).close()' \
    "$DECIBELL_CENTRAL" 2>/dev/null || die "the local stack is down (tools/dev/local-stack.sh up)"
  if [[ -d "$STATE" ]] && ls "$STATE"/*.pid >/dev/null 2>&1; then die "already running (streambot.sh stop first)"; fi
  mkdir -p "$STATE"
  local i
  for i in "${!files[@]}"; do
    local file; file="$(realpath "${files[$i]}")"
    [[ -f "$file" ]] || die "no such file: ${files[$i]}"
    local name="s$i-${acct[$i]}" profile="$STATE/s$i-profile"
    rm -rf "$profile"; mkdir -p "$profile"
    rm -f "$STATE/$name.stop"
    # The steps: sign in, unlock, muted + deafened into the channel with MLS ready, stream until told to stop.
    ( exec "$headless" --user-data-dir "$profile" --timeout 86400 \
        login "${acct[$i]}" wait connected "$community" open-server "$community" e2ee-ready "$passphrase" \
        mute deafen voice-join "$channel" wait mls ready \
        stream-file "$file" print streaming wait-file "$STATE/$name.stop" \
        stream-stop voice-leave logout wait logged-out >"$STATE/$name.log" 2>"$STATE/$name.err" ) &
    echo $! >"$STATE/$name.pid"
    echo "$file" >"$STATE/$name.file"
    say "$name: ${acct[$i]} streams $(basename "$file") into $community / $channel (pid $!)"
  done
  # Wait until every one is live (or failed).
  local deadline=$((SECONDS + 90)) pidfile
  for pidfile in "$STATE"/*.pid; do
    local n; n="$(basename "$pidfile" .pid)"
    until grep -q '^MARK streaming' "$STATE/$n.log" 2>/dev/null; do
      if grep -q '^FAILED' "$STATE/$n.log" 2>/dev/null || ! kill -0 "$(cat "$pidfile")" 2>/dev/null; then
        tail -5 "$STATE/$n.log" >&2; cmd_stop; die "$n did not start"
      fi
      ((SECONDS < deadline)) || { tail -5 "$STATE/$n.log" >&2; cmd_stop; die "$n not live after 90 s"; }
      sleep 0.2
    done
    say "$n: $(grep '^STREAMING' "$STATE/$n.log" | tail -1)"
  done
}

cmd_stop() {
  [[ -d "$STATE" ]] || return 0
  local pidfile
  for pidfile in "$STATE"/*.pid; do
    [[ -e "$pidfile" ]] || continue
    local n; n="$(basename "$pidfile" .pid)"
    : >"$STATE/$n.stop"
  done
  for pidfile in "$STATE"/*.pid; do
    [[ -e "$pidfile" ]] || continue
    local n pid i; n="$(basename "$pidfile" .pid)"; pid="$(cat "$pidfile")"
    for ((i = 0; i < 150; i++)); do kill -0 "$pid" 2>/dev/null || break; sleep 0.1; done
    if kill -0 "$pid" 2>/dev/null; then kill -TERM "$pid" 2>/dev/null; sleep 1; kill -KILL "$pid" 2>/dev/null || true; fi
    say "$n: $(grep -E '^STREAM-STOPPED|^DONE|^FAILED' "$STATE/$n.log" 2>/dev/null | tr '\n' ' ')"
    rm -f "$pidfile" "$STATE/$n.stop"
    rm -rf "$STATE/${n%%-*}-profile"
  done
}

cmd_status() {
  [[ -d "$STATE" ]] || { echo "not running"; return 0; }
  local pidfile any=0
  for pidfile in "$STATE"/*.pid; do
    [[ -e "$pidfile" ]] || continue
    any=1
    local n pid; n="$(basename "$pidfile" .pid)"; pid="$(cat "$pidfile")"
    if kill -0 "$pid" 2>/dev/null; then echo "$n: pid $pid, $(cat "$STATE/$n.file")"; else echo "$n: exited"; fi
    tail -2 "$STATE/$n.log" | sed 's/^/    /'
  done
  ((any)) || echo "not running"
}

case "${1:-}" in
  start) shift; cmd_start "$@" ;;
  stop) cmd_stop ;;
  status) cmd_status ;;
  -h|--help|"") usage ;;
  *) die "unknown command $1 (start | stop | status)" ;;
esac
