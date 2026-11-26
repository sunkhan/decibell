# Client hooks for scenarios.sh: the GPUI client (gpui-client/), release build with the `dev-local` feature (it honours
# DECIBELL_CENTRAL and has `--login`, the stdout markers and the control pipe of gpui-client/crates/decibell-app/src/dev.rs;
# the shipped binary has none of them). clients/electron.sh documents the hook contract. Sourced by scenarios.sh; $PERF is
# tools/perf.
#
# Build:  (cd gpui-client && cargo build --release -p decibell-app --features dev-local)
# Same rules as the Electron baseline: DECIBELL_CENTRAL=127.0.0.1:18080, its own profile directory, never the live central.
# The profile signs in once as perf1 (--login, the stack's password) and keeps the saved sign-in, as Electron's does.
#
# The chat pane is still a placeholder in this client (the chat agent's): #general shows no messages yet, so the idle-channel
# rows measure the shell (server bar, channel list, member list, user panel) beside an empty pane, and `scroll` can't run.

GPUI_DIR="$(cd "$PERF/../../gpui-client" && pwd)"
GPUI_BIN="${GPUI_BIN:-$GPUI_DIR/target/release/decibell}"
GPUI_DEV_DIR="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}"
GPUI_PROFILE="$GPUI_DEV_DIR/gpui-profile"
GPUI_STATE="$GPUI_DEV_DIR/gpui-run"
GPUI_CENTRAL=127.0.0.1:18080
GPUI_ACCOUNT=perf1

_gpui_env() {
  printf '%s\0' env "DECIBELL_CENTRAL=$GPUI_CENTRAL" "DECIBELL_USER_DATA_DIR=$GPUI_PROFILE"
}

_gpui_password() {
  python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["password"])' "$GPUI_DEV_DIR/run/accounts.json"
}

# Wait until the log has a line matching $1 (extended regex), at most $2 seconds.
_gpui_wait_log() {
  local pattern="$1" limit="${2:-60}" i
  for ((i = 0; i < limit * 10; i++)); do
    grep -Eq -- "$pattern" "$GPUI_STATE/app.log" 2>/dev/null && return 0
    sleep 0.1
  done
  echo "gpui: timed out waiting for /$pattern/ in $GPUI_STATE/app.log" >&2
  tail -20 "$GPUI_STATE/app.log" >&2
  return 1
}

# Send one control command and wait for its acknowledgement.
_gpui_control() {
  local before; before="$(grep -c 'decibell: control' "$GPUI_STATE/app.log" 2>/dev/null || true)"
  timeout 5 sh -c 'printf "%s\n" "$1" >"$2"' _ "$1" "$GPUI_STATE/control" || { echo "gpui: control pipe not read" >&2; return 1; }
  local i
  for ((i = 0; i < 100; i++)); do
    (( $(grep -c 'decibell: control' "$GPUI_STATE/app.log") > before )) && break
    sleep 0.1
  done
  grep 'decibell: control' "$GPUI_STATE/app.log" | tail -1 | grep -q ' ok$'
}

client_describe() {
  echo "GPUI client @ $(git -C "$GPUI_DIR" rev-parse --short=10 HEAD 2>/dev/null)$(git -C "$GPUI_DIR" diff --quiet 2>/dev/null || echo '+dirty'), release (fat LTO) + dev-local; profile $GPUI_PROFILE; session keyring"
}

client_preflight() {
  [[ -x "$GPUI_BIN" ]] || { echo "gpui: not built: (cd gpui-client && cargo build --release -p decibell-app --features dev-local)" >&2; return 1; }
  local probe; probe="$(DECIBELL_CENTRAL=127.0.0.1:9 DECIBELL_DEV_PASSWORD=x "$GPUI_BIN" --login __probe__ 2>&1 || true)"
  grep -q 'not an account' <<<"$probe" || { echo "gpui: $GPUI_BIN was built without --features dev-local" >&2; return 1; }
  python3 -I -c 'import socket; socket.create_connection(("127.0.0.1",18080),timeout=2).close()' 2>/dev/null \
    || { echo "gpui: the local stack is down: tools/dev/local-stack.sh up" >&2; return 1; }
  mkdir -p "$GPUI_STATE" "$GPUI_PROFILE"
  # The saved sign-in every scenario starts from: one sign-in as perf1, then quit.
  if ! python3 -I -c 'import json,sys; c=json.load(open(sys.argv[1])); sys.exit(0 if c.get("credentials") else 1)' \
      "$GPUI_PROFILE/config.json" 2>/dev/null; then
    echo "gpui: signing the profile in as $GPUI_ACCOUNT (saved sign-in)" >&2
    client_launch --login >/dev/null && client_ready && client_stop || return 1
  fi
}

# Prints the app's pid. `--login` (only from preflight) signs in with the stack's password.
client_launch() {
  mkdir -p "$GPUI_STATE"
  rm -f "$GPUI_STATE/control"; mkfifo "$GPUI_STATE/control"
  : >"$GPUI_STATE/app.log"
  local env=(); mapfile -d '' env < <(_gpui_env)
  local extra=()
  if [[ "${1:-}" == --login ]]; then
    extra=(--login "$GPUI_ACCOUNT"); env+=("DECIBELL_DEV_PASSWORD=$(_gpui_password)")
  fi
  ( exec "${env[@]}" "DECIBELL_DEV_CONTROL=$GPUI_STATE/control" "$GPUI_BIN" "${extra[@]}" >>"$GPUI_STATE/app.log" 2>&1 ) &
  local pid=$!
  echo "$pid" >"$GPUI_STATE/pid"
  echo "$pid"
}

client_ready() {
  _gpui_wait_log '^decibell: signed-in ' 60 && _gpui_wait_log '^decibell: connected Daily Gaming$' 60 && client_check
}

client_goto_home() { _gpui_control home && echo '{"view":"home"}'; }

# The channel opens through the control pipe: nothing takes keyboard focus.
client_goto_channel() { _gpui_control "channel $1	$2" && printf '{"server":"%s","channel":"%s"}\n' "$1" "$2"; }

client_minimize() { _gpui_control minimize && echo '{"minimized":true}'; }

client_stop() {
  local pid; pid="$(cat "$GPUI_STATE/pid" 2>/dev/null)" || return 0
  [[ -n "$pid" ]] && kill -0 "$pid" 2>/dev/null || return 0
  timeout 3 sh -c 'printf "quit\n" >"$1"' _ "$GPUI_STATE/control" 2>/dev/null || true
  local i
  for ((i = 0; i < 100; i++)); do kill -0 "$pid" 2>/dev/null || return 0; sleep 0.1; done
  kill -TERM "$pid" 2>/dev/null; sleep 2; kill -KILL "$pid" 2>/dev/null || true
}

client_check() {
  local pid; pid="$(cat "$GPUI_STATE/pid")"
  local socks; socks="$(ss -tnpH 2>/dev/null | grep "pid=$pid," || true)"
  if grep -qF "178.104.131.247" <<<"$socks"; then echo "gpui: a socket to the live central!" >&2; kill -KILL "$pid"; return 1; fi
  grep -qF "$GPUI_CENTRAL" <<<"$socks" || { echo "gpui: no connection to $GPUI_CENTRAL: $socks" >&2; return 1; }
}

client_measure_args() { :; }

client_startup_argv() {
  local env=(); mapfile -d '' env < <(_gpui_env)
  printf '%s\0' "${env[@]}" "$GPUI_BIN" --perf
}

client_startup_args() { printf '%s\0' --ready-marker 'first-frame .* monotonic_ns=(?P<ns>\d+)'; }

# Every startup run begins from: saved sign-in, Home showing when it last quit.
client_startup_prepare() {
  client_launch >/dev/null
  client_ready >/dev/null
  client_goto_home >/dev/null
  sleep 1
  client_stop
}

client_install_paths() { printf 'gpui release (dev-local, stripped)\t%s\n' "$GPUI_BIN"; }

client_scroll() { echo '{"error":"the chat pane is not built yet in the GPUI client"}'; return 1; }

client_facts() { python3 -B "$PERF/linux/scenario_util.py" display-server --pid "$1"; }

# ---- voice (the watch scenarios) ------------------------------------------------------------------------------------
# MLS voice needs E2EE keys: the watch scenarios run as perf3 (keys on the stack, passphrase below) in a profile of
# their own, so perf1's profile (the idle rows) is untouched. The streams come from tools/perf/streambot.sh.

GPUI_VOICE_PASSPHRASE="${GPUI_VOICE_PASSPHRASE:-decibell-m3-test-passphrase}"

# Switch the hooks to the voice account and profile (called by scenarios.sh before client_preflight).
client_voice_profile() {
  GPUI_ACCOUNT=perf3
  GPUI_PROFILE="$GPUI_DEV_DIR/gpui-profile-voice"
  GPUI_STATE="$GPUI_DEV_DIR/gpui-run-voice"
}

# Unlock the E2EE keys and join a voice channel (muted and deafened first: nothing captured or played).
client_voice_join() {
  _gpui_control "unlock $GPUI_VOICE_PASSPHRASE" || return 1
  sleep 2
  _gpui_control "voice $1	$2" || return 1
  sleep 3
  printf '{"voice":{"server":"%s","channel":"%s"}}\n' "$1" "$2"
}

# Watch a stream large (focus): the first stream; `client_watch_too` adds one to the grid.
client_watch() { _gpui_control "watch $1" && printf '{"watch":%s}\n' "$1"; }
client_watch_too() { client_watch "$@"; }

# The hub's counters of a watched stream (one JSON line).
client_video_stats() {
  _gpui_control "video-stats $1" >/dev/null || return 1
  grep "^decibell: video-stats $1 " "$GPUI_STATE/app.log" | tail -1 | cut -d' ' -f4-
}

client_leave_voice() { _gpui_control leave; }
