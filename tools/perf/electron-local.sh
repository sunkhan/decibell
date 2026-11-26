#!/usr/bin/env bash
# electron-local.sh: the Electron client built from an isolated checkout with the `local-central` cargo feature,
# run against the local test stack (tools/dev/local-stack.sh) in an isolated profile, driven over CDP. It is the
# Electron side of the client baseline (tools/perf/scenarios.sh electron ...). It never talks to the live central
# and never touches the owner's real profile (~/.config/decibell-electron-client), saved sign-in or keyrings.
#
#   electron-local.sh build [--ref <commit>] [--force] [--unpackaged]
#        worktree $DECIBELL_DEV_DIR/electron-baseline (git worktree, detached; node_modules + the static FFmpeg are
#        symlinked from the main tree), production renderer + main, addon in release with --features local-central
#        (own CARGO_TARGET_DIR), then electron-builder --linux dir (the shipped layout: asar + unpacked addon, offline
#        from node_modules/electron/dist). --ref moves the worktree to that commit first (default: stays; the first
#        build takes the main tree's HEAD). Idempotent; --force redoes the JS and packaging steps.
#   electron-local.sh run [--cdp-port N | --no-cdp] [-- extra electron args]
#        checks the stack, proves the addon honours DECIBELL_CENTRAL (probe), starts the private session bus +
#        throwaway keyring, launches, verifies (addon mapped, sockets), prints the root pid.
#   electron-local.sh signin [--user alice]   first run: types the sign-in over CDP, waits for the main layout,
#        verifies central = 127.0.0.1:18080, then restarts once to prove the saved sign-in signs in by itself
#   electron-local.sh open <server> <channel>  open a text channel through CDP (open-channel.mjs)
#   electron-local.sh home | state | blur | minimize | dismiss-banner | wait-main [s] | screenshot <out.png>
#                               (electron-cdp.mjs)
#   electron-local.sh check     verify the running instance: probed addon mapped, central socket, no live socket
#   electron-local.sh stop      SIGTERM (clean quit), SIGKILL after 10 s; stops the private bus; checks nothing is
#                               left and that the owner's profile / keyrings / mimeapps.list were not written
#   electron-local.sh probe | pid | pids | port | cmd [port] | paths
#
# Isolation (what the real client keeps where, and where this one keeps it instead):
#   - Chromium profile, config.json (saved sign-in, settings, device id), cert_pins.json, e2ee/ keystore + local.key:
#     all under app.getPath('userData') = --user-data-dir=$PROFILE_ROOT/user-data.
#   - The safeStorage key that wraps e2ee/local.key (the saved sign-in's key): Chromium asks the OS keyring over the
#     session bus. The client runs on a PRIVATE session bus ($PROFILE_ROOT/session-bus, no service activation) with a
#     throwaway gnome-keyring whose files are in $PROFILE_ROOT/keyring (--password-store=gnome-libsecret), so it can
#     never reach the owner's KWallet / gnome-keyring. (--password-store=basic would be simpler but the client cannot
#     run with it: safeStorage reports no encryption, main passes e2eeLocalKey: null to the addon's init, napi-rs
#     refuses null for Option<String> and the addon never initialises.)
#   - xdg-settings is shimmed: app.setAsDefaultProtocolClient runs `xdg-settings set default-url-scheme-handler
#     decibell decibell-electron-client.desktop` on every start, which would re-point the desktop's decibell://
#     handler. The shim runs the read-only `xdg-settings check` instead (same cost at startup) and logs the call.
#   - cert_pins.json gets a deliberately wrong pin for the live central (178.104.131.247:8080): if this client ever
#     reached it, the TLS handshake would fail before a sign-in is sent. Belt and braces over the probe.
#
# Environment: DECIBELL_DEV_DIR (default ~/.cache/decibell-dev), DECIBELL_EB_JOBS (cargo jobs, default 8), NODE.
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$here/../.." && pwd)"
DEV_DIR="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}"
TREE="$DEV_DIR/electron-baseline"            # git worktree (detached) of this repo
TARGET="$DEV_DIR/electron-baseline-target"   # its own cargo target dir
PROFILE_ROOT="$DEV_DIR/electron-profile"
PROFILE="$PROFILE_ROOT/user-data"            # --user-data-dir
KEYRING="$PROFILE_ROOT/keyring"              # the throwaway gnome-keyring's XDG_DATA_HOME + control dir
BUS="$PROFILE_ROOT/session-bus"              # private session bus socket
STATE="$DEV_DIR/electron-local"              # pid, logs, build + probe stamps, shim, bus config
CENTRAL="127.0.0.1:18080"
LIVE_IP="178.104.131.247"
LIVE_KEY="$LIVE_IP:8080"
KEYRING_PASSWORD="decibell-perf"
REAL_PROFILE="$HOME/.config/decibell-electron-client"
REAL_KEYRINGS=("$HOME/.local/share/keyrings" "$HOME/.local/share/kwalletd" "$HOME/.config/mimeapps.list")
DEFAULT_CDP_PORT=9333
NODE="${NODE:-node}"
mkdir -p "$STATE"

die() { echo "electron-local: $*" >&2; exit 1; }
log() { echo "[electron-local] $*" >&2; }

node_cmd() {
  # Node >= 22 has a global WebSocket; older ones need the flag (the Windows scripts pass it).
  if "$NODE" -e 'process.exit(typeof WebSocket === "function" ? 0 : 1)' 2>/dev/null; then
    "$NODE" "$@"
  else
    "$NODE" --experimental-websocket "$@"
  fi
}

stamp_get() { [[ -f "$STATE/build.env" ]] && sed -n "s/^$1=//p" "$STATE/build.env" | tail -1 || true; }

# ---------------------------------------------------------------------------------------------------- build

cmd_build() {
  local ref="" force=0 unpackaged=0
  while (($#)); do
    case "$1" in
      --ref) ref="$2"; shift 2 ;;
      --force) force=1; shift ;;
      --unpackaged) unpackaged=1; shift ;;
      *) die "build: unknown option $1" ;;
    esac
  done
  local t0=$SECONDS
  if [[ ! -e "$TREE/.git" ]]; then
    local c; c="$(git -C "$REPO" rev-parse "${ref:-HEAD}")"
    log "worktree $TREE at $c (git worktree add --detach)"
    git -C "$REPO" worktree add --detach "$TREE" "$c" >&2
  elif [[ -n "$ref" ]]; then
    local c; c="$(git -C "$REPO" rev-parse "$ref")"
    log "moving the worktree to $c (anything changed inside it is discarded)"
    git -C "$TREE" checkout --detach --force "$c" >&2
  fi
  local commit; commit="$(git -C "$TREE" rev-parse HEAD)"
  local ec="$TREE/electron-client"
  # Not in git: node_modules (castlabs Electron is already downloaded there) and the static FFmpeg.
  for l in "electron-client/node_modules" "electron-client/native/node_modules" "electron-client/native/vendor"; do
    [[ -d "$REPO/$l" ]] || die "missing $REPO/$l (npm ci / build the FFmpeg there first)"
    if [[ ! -L "$TREE/$l" ]]; then
      [[ -e "$TREE/$l" ]] && die "$TREE/$l exists and is not a symlink"
      ln -s "$REPO/$l" "$TREE/$l"
    fi
  done
  # A release build bakes resources/sentry.json (crash reports) and gifs.json; this one must have neither.
  rm -f "$ec/resources/sentry.json" "$ec/resources/gifs.json"

  local js_done=0
  if ((force)) || [[ "$(stamp_get EB_COMMIT)" != "$commit" ]] || [[ ! -f "$ec/dist/renderer/index.html" ]]; then
    log "renderer + main (production) at ${commit:0:10}"
    (cd "$ec" && npm run --silent build:twemoji && npm run --silent build:renderer && npm run --silent build:tsc) >&2
    js_done=1
  else
    log "renderer + main up to date for ${commit:0:10} (--force to redo)"
  fi

  log "addon: release, --features local-central (CARGO_TARGET_DIR=$TARGET, jobs ${DECIBELL_EB_JOBS:-8})"
  (
    cd "$ec/native"
    export PKG_CONFIG_PATH="$REPO/electron-client/native/vendor/ffmpeg/lib/pkgconfig"
    export CARGO_TARGET_DIR="$TARGET" CARGO_BUILD_JOBS="${DECIBELL_EB_JOBS:-8}"
    npx --no-install napi build --platform --release --features local-central --js index.js --dts index.d.ts
    node copy-dlls.cjs
  ) >&2
  local addon_src="$ec/native/index.linux-x64-gnu.node"
  [[ -f "$addon_src" ]] || die "no addon at $addon_src"
  grep -q -a "DECIBELL_CENTRAL" "$addon_src" || die "the addon has no DECIBELL_CENTRAL override: built without local-central?"

  local mode bin appdir="" addon
  if ((unpackaged)); then
    mode=unpackaged
    bin="$ec/node_modules/electron/dist/electron"; appdir="$ec"; addon="$addon_src"
  else
    mode=packaged
    local out="$ec/release/linux-unpacked"
    if ((force || js_done)) || [[ ! -x "$out/decibell" ]] || \
        ! cmp -s "$addon_src" "$out/resources/app.asar.unpacked/native/index.linux-x64-gnu.node"; then
      log "electron-builder --linux dir (offline: electronDist = node_modules/electron/dist)"
      rm -rf "$out"
      (cd "$ec" && npx --no-install electron-builder --linux dir --publish never \
          -c.electronDist=node_modules/electron/dist -c.npmRebuild=false) >&2
    else
      log "package up to date"
    fi
    bin="$out/decibell"; addon="$out/resources/app.asar.unpacked/native/index.linux-x64-gnu.node"
    [[ -x "$bin" && -f "$addon" ]] || die "electron-builder produced no $bin / $addon"
    [[ ! -e "$out/resources/sentry.json" ]] || die "$out/resources/sentry.json exists: crash reporting would be on"
    [[ ! -e "$out/resources/package-type" ]] || die "$out/resources/package-type exists: electron-updater would check GitHub"
  fi
  cmp -s "$addon_src" "$addon" || die "packaged addon differs from the one just built"
  {
    echo "EB_COMMIT=$commit"
    echo "EB_MODE=$mode"
    echo "EB_BIN=$bin"
    echo "EB_APPDIR=$appdir"
    echo "EB_ADDON=$addon"
    echo "EB_ADDON_SHA256=$(sha256sum "$addon" | cut -d' ' -f1)"
    echo "EB_ELECTRON=$(cat "$ec/node_modules/electron/dist/version")"
    echo "EB_BUILT=$(date -Iseconds)"
  } >"$STATE/build.env"
  rm -f "$STATE/probe.ok"
  cmd_probe
  log "built ${commit:0:10} ($mode) in $((SECONDS - t0)) s: $bin"
}

# ---------------------------------------------------------------------------------------------------- probe
# Proves the exact addon file that will be loaded honours DECIBELL_CENTRAL: loads it in Node inside an empty network
# namespace (unshare -rn: only loopback exists, so the live central is unreachable whatever the build does) with a
# listener on 127.0.0.1:18080, and asks it to sign in as a junk user. Pass = the listener saw the connection.
# (Without the variable the same addon tries 178.104.131.247:8080 and gets "Network is unreachable" in there.)
cmd_probe() {
  [[ -f "$STATE/build.env" ]] || die "not built (electron-local.sh build)"
  local addon sha; addon="$(stamp_get EB_ADDON)"; sha="$(stamp_get EB_ADDON_SHA256)"
  [[ -f "$addon" ]] || die "addon missing: $addon"
  [[ "$(sha256sum "$addon" | cut -d' ' -f1)" == "$sha" ]] || die "addon changed since the build stamp; rebuild"
  if [[ -f "$STATE/probe.ok" ]] && [[ "$(cat "$STATE/probe.ok")" == "$sha" ]]; then return 0; fi
  log "probe: does $addon connect to \$DECIBELL_CENTRAL? (empty netns, nothing can leave the machine)"
  local tmp; tmp="$(mktemp -d "$STATE/probe.XXXXXX")"
  local out rc=0
  out="$(unshare -rn sh -c '
    ip link set lo up 2>/dev/null || exit 90
    python3 -I -c "
import socket,sys
s=socket.socket(); s.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1); s.bind((\"127.0.0.1\",18080)); s.listen(1)
s.settimeout(20)
open(sys.argv[1]+\"/listening\",\"w\").close()
try:
    c,a=s.accept(); print(\"probe: connection from\", a[0]); c.close()
except Exception as e:
    print(\"probe: no connection:\", e); sys.exit(3)
" "$1" & lp=$!
    for i in $(seq 50); do [ -e "$1/listening" ] && break; sleep 0.1; done
    DECIBELL_CENTRAL=127.0.0.1:18080 node "$2" "$3" "$1" >"$1/node.log" 2>&1 &
    np=$!
    wait $lp; r=$?
    kill $np 2>/dev/null; wait $np 2>/dev/null
    tail -5 "$1/node.log"
    exit $r
  ' sh "$tmp" "$here/linux/addon-probe.cjs" "$addon" 2>&1)" || rc=$?
  rm -rf "$tmp"
  echo "$out" | sed 's/^/  /' >&2
  if ((rc == 0)) && grep -q "probe: connection from 127.0.0.1" <<<"$out"; then
    echo "$sha" >"$STATE/probe.ok"
    log "probe passed: the addon connects to 127.0.0.1:18080 when told to"
  else
    die "probe FAILED (rc $rc): this addon does not honour DECIBELL_CENTRAL; it would sign in to the live central"
  fi
}

# ---------------------------------------------------------------------------------------------------- private bus

bus_pid() { [[ -f "$STATE/bus.pid" ]] && kill -0 "$(cat "$STATE/bus.pid")" 2>/dev/null && cat "$STATE/bus.pid"; }
keyring_pid() { [[ -f "$STATE/keyring.pid" ]] && kill -0 "$(cat "$STATE/keyring.pid")" 2>/dev/null && cat "$STATE/keyring.pid"; }

# Starts a process detached (own session), stdout/stderr appended to $1, and prints its pid (the real one: setsid(1)
# forks when the caller leads its process group, so $! would be setsid's own pid). $2: stdin text ("" = /dev/null).
spawn() {
  local logf="$1" input="$2"; shift 2
  python3 -I -c '
import os, sys
logf, inp, argv = sys.argv[1], sys.argv[2], sys.argv[3:]
r, w = os.pipe()
pid = os.fork()
if pid == 0:
    os.setsid()
    fd = os.open(logf, os.O_WRONLY | os.O_APPEND | os.O_CREAT, 0o644)
    os.dup2(fd, 1); os.dup2(fd, 2)
    os.close(w); os.dup2(r, 0)
    if r != 0: os.close(r)
    os.close(fd)
    try:
        os.execvp(argv[0], argv)
    finally:
        os._exit(127)
os.close(r)
if inp:
    os.write(w, inp.encode())
os.close(w)
print(pid)' "$logf" "$input" "$@"
}

bus_up() {
  if bus_pid >/dev/null && keyring_pid >/dev/null && [[ -S "$BUS" ]]; then return 0; fi
  bus_down quiet
  mkdir -p "$PROFILE_ROOT" "$KEYRING/run"
  chmod 700 "$PROFILE_ROOT" "$KEYRING" "$KEYRING/run"
  cat >"$STATE/bus.conf" <<EOF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN"
 "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<!-- electron-local.sh: the test client's private session bus. No servicedir: nothing is ever activated on it. -->
<busconfig>
  <type>session</type>
  <listen>unix:path=$BUS</listen>
  <auth>EXTERNAL</auth>
  <policy context="default">
    <allow send_destination="*" eavesdrop="true"/>
    <allow eavesdrop="true"/>
    <allow own="*"/>
  </policy>
</busconfig>
EOF
  rm -f "$BUS"
  spawn "$STATE/bus.log" "" dbus-daemon --config-file="$STATE/bus.conf" --nofork >"$STATE/bus.pid"
  local i
  for i in $(seq 50); do [[ -S "$BUS" ]] && break; sleep 0.1; done
  [[ -S "$BUS" ]] || die "private session bus did not start (see $STATE/bus.log)"
  # The keyring daemon: its own data dir and control dir (the owner's runs with /run/user/<uid>/keyring).
  spawn "$STATE/keyring.log" "$KEYRING_PASSWORD" env "DBUS_SESSION_BUS_ADDRESS=unix:path=$BUS" \
      "XDG_DATA_HOME=$KEYRING" "XDG_RUNTIME_DIR=$KEYRING/run" \
      gnome-keyring-daemon --foreground --components=secrets --unlock --control-directory="$KEYRING/run" \
      >"$STATE/keyring.pid"
  for i in $(seq 50); do
    busctl --address="unix:path=$BUS" list --no-pager 2>/dev/null | grep -q '^org.freedesktop.secrets ' && break
    sleep 0.1
  done
  busctl --address="unix:path=$BUS" list --no-pager 2>/dev/null | grep -q '^org.freedesktop.secrets ' \
    || die "throwaway keyring did not come up on the private bus (see $STATE/keyring.log)"
}

bus_down() {
  local p
  for p in "$(keyring_pid || true)" "$(bus_pid || true)"; do
    [[ -n "$p" ]] && kill -TERM "$p" 2>/dev/null || true
  done
  sleep 0.2
  for p in "$(keyring_pid || true)" "$(bus_pid || true)"; do
    [[ -n "$p" ]] && kill -KILL "$p" 2>/dev/null || true
  done
  rm -f "$STATE/bus.pid" "$STATE/keyring.pid" "$BUS"
  [[ "${1:-}" == quiet ]] || true
}

# ---------------------------------------------------------------------------------------------------- process helpers

# Every process of the instance: the client's own binary (or crashpad beside it) with this profile on its command line.
tree_pids() {
  local bin dir p exe
  # argv[0], not /proc/<pid>/exe: the sandboxed helpers are not dumpable, so their exe link cannot be read.
  bin="$(stamp_get EB_BIN)"; [[ -n "$bin" ]] || return 0
  local d1 d2 cmd; d1="$(dirname "$bin")"; d2="$(dirname "$(realpath "$bin")")"
  for p in /proc/[0-9]*; do
    cmd="$( { tr '\0' '\n' <"$p/cmdline"; } 2>/dev/null )" || continue
    exe="${cmd%%$'\n'*}"
    [[ "$exe" == "$d1/"* || "$exe" == "$d2/"* ]] || continue
    grep -qF -- "$PROFILE" <<<"$cmd" && echo "${p#/proc/}"
  done
  return 0
}

root_pid() {
  local p
  [[ -f "$STATE/client.pid" ]] || return 1
  p="$(cat "$STATE/client.pid")"
  [[ -d "/proc/$p" ]] && [[ "$(readlink "/proc/$p/exe" 2>/dev/null)" == "$(realpath "$(stamp_get EB_BIN)")" ]] || return 1
  echo "$p"
}

cdp_port() { [[ -f "$STATE/cdp.port" ]] && cat "$STATE/cdp.port" || echo ""; }

# Sockets of the instance (TCP + UDP): "proto state local peer" per line.
tree_sockets() {
  local pids; pids="$(tree_pids | tr '\n' '|' | sed 's/|$//')"
  [[ -n "$pids" ]] || return 0
  ss -tunapH 2>/dev/null | grep -E "pid=($pids)," | awk '{print $1, $2, $5, $6}' || true
}

check_sockets() { # $1: require-central (1/0); returns 0 ok, 1 central missing, 2 live address seen
  local s; s="$(tree_sockets)"
  if grep -qF "$LIVE_IP" <<<"$s"; then
    echo "$s" >&2
    return 2
  fi
  if [[ "${1:-0}" == 1 ]] && ! grep -E "ESTAB" <<<"$s" | grep -qF " $CENTRAL"; then return 1; fi
  return 0
}

kill_now() {
  local p
  for p in $(tree_pids); do kill -KILL "$p" 2>/dev/null || true; done
}

verify_process() { # static checks of the running main process, before anything is typed
  local pid="$1" addon i; addon="$(stamp_get EB_ADDON)"
  # (Its environment cannot be read back: Chromium writes its process title over the original argv + environ block,
  # so /proc/<pid>/environ is blank. The launch command is built here with DECIBELL_CENTRAL=127.0.0.1:18080.)
  grep -qx "DECIBELL_CENTRAL=$CENTRAL" <(tr ' ' '\n' <"$STATE/client.cmd") \
    || { kill_now; die "ABORT: the launch command has no DECIBELL_CENTRAL=$CENTRAL"; }
  for i in $(seq 150); do
    grep -qF "$addon" "/proc/$pid/maps" 2>/dev/null && break
    sleep 0.1
  done
  grep -qF "$addon" "/proc/$pid/maps" 2>/dev/null || { kill_now; die "ABORT: the probed addon ($addon) is not loaded"; }
  [[ "$(sha256sum "$addon" | cut -d' ' -f1)" == "$(cat "$STATE/probe.ok" 2>/dev/null)" ]] \
    || { kill_now; die "ABORT: the loaded addon was not probed"; }
}

wait_central() { # $1 timeout s: ESTAB to the local central, never the live address
  local i r
  for i in $(seq $(( ${1:-30} * 4 ))); do
    r=0; check_sockets 1 || r=$?
    ((r == 2)) && { kill_now; die "ABORT: a socket to the live central $LIVE_IP was opened; client killed"; }
    ((r == 0)) && return 0
    sleep 0.25
  done
  return 1
}

has_saved_signin() {
  [[ -f "$PROFILE/config.json" ]] && python3 -I -c '
import json,sys
c=json.load(open(sys.argv[1])); sys.exit(0 if c.get("saved_login") or c.get("credentials") else 1)' "$PROFILE/config.json" 2>/dev/null
}

stack_up() {
  python3 -I -c 'import socket; socket.create_connection(("127.0.0.1",18080),timeout=2).close()' 2>/dev/null \
    && [[ -f "$DEV_DIR/run/accounts.json" ]]
}

poison_live_pin() {
  mkdir -p "$PROFILE"
  python3 -I -c '
import json, os, sys
p, key = sys.argv[1], sys.argv[2]
d = {}
if os.path.exists(p):
    try: d = json.load(open(p))
    except ValueError: d = {}
if d.get(key) != "0" * 64:
    d[key] = "0" * 64
    tmp = p + ".tmp"
    json.dump(d, open(tmp, "w"), indent=2)
    os.replace(tmp, p)' "$PROFILE/cert_pins.json" "$LIVE_KEY"
}

# The launch, as one command line (also what startup.sh runs; see the header for each piece).
launch_argv() { # $1 cdp port ("" = none), rest: extra args
  local port="$1"; shift
  local bin appdir; bin="$(stamp_get EB_BIN)"; appdir="$(stamp_get EB_APPDIR)"
  mkdir -p "$STATE/shim"
  cat >"$STATE/shim/xdg-settings" <<EOF
#!/bin/sh
# electron-local.sh shim: never write the desktop's URL-scheme registration from the test client.
echo "\$(date -Iseconds) xdg-settings \$*" >>"$STATE/xdg-settings.log"
if [ "\$1" = set ]; then shift; exec /usr/bin/xdg-settings check "\$@" >/dev/null 2>&1; fi
exec /usr/bin/xdg-settings "\$@"
EOF
  chmod +x "$STATE/shim/xdg-settings"
  local a=(env -u ELECTRON_RUN_AS_NODE -u ELECTRON_ENABLE_LOGGING -u ELECTRON_ENABLE_STACK_DUMPING -u NODE_OPTIONS
           -u ELECTRON_OZONE_PLATFORM_HINT -u VITE_DEV_SERVER_URL -u DECIBELL_GPU_SANDBOX_OFF -u DECIBELL_STREAM_DUMP
           -u GNOME_KEYRING_CONTROL -u SSH_AUTH_SOCK
           "PATH=$STATE/shim:$PATH" "DBUS_SESSION_BUS_ADDRESS=unix:path=$BUS" "DECIBELL_CENTRAL=$CENTRAL" "$bin")
  [[ -n "$appdir" ]] && a+=("$appdir")
  a+=("--user-data-dir=$PROFILE" "--password-store=gnome-libsecret")
  [[ -n "$port" ]] && a+=("--remote-debugging-port=$port")
  a+=("$@")
  printf '%s\0' "${a[@]}"
}

# Everything a launch needs besides the binary: the bus + keyring, the pin. (startup.sh launches the command itself.)
prepare_launch() {
  stack_up || die "the local stack is not up (tools/dev/local-stack.sh up; seed once)"
  [[ -f "$STATE/build.env" ]] || die "not built (electron-local.sh build)"
  cmd_probe
  bus_up
  poison_live_pin
  [[ -f "$STATE/real.stamp" ]] || touch "$STATE/real.stamp"
}

# ---------------------------------------------------------------------------------------------------- commands

cmd_run() {
  local port="$DEFAULT_CDP_PORT" extra=()
  while (($#)); do
    case "$1" in
      --cdp-port) port="$2"; shift 2 ;;
      --no-cdp) port=""; shift ;;
      --) shift; extra=("$@"); break ;;
      *) die "run: unknown option $1" ;;
    esac
  done
  if root_pid >/dev/null; then die "already running (pid $(root_pid)); electron-local.sh stop"; fi
  [[ -z "$(tree_pids)" ]] || die "processes with this profile are still running: $(tree_pids | tr '\n' ' ')"
  prepare_launch
  local argv=()
  mapfile -d '' argv < <(launch_argv "$port" "${extra[@]}")
  printf '%s ' "${argv[@]}" >"$STATE/client.cmd"; echo >>"$STATE/client.cmd"
  echo "$port" >"$STATE/cdp.port"
  echo "=== $(date -Iseconds) launch" >>"$STATE/client.log"
  local pid; pid="$(spawn "$STATE/client.log" "" "${argv[@]}")"
  echo "$pid" >"$STATE/client.pid"
  sleep 0.3
  [[ -d "/proc/$pid" ]] || { tail -20 "$STATE/client.log" >&2; die "the client exited at once"; }
  verify_process "$pid"
  log "pid $pid: probed addon loaded, DECIBELL_CENTRAL=$CENTRAL; profile $PROFILE; log $STATE/client.log"
  if has_saved_signin; then
    if wait_central 45; then
      log "saved sign-in: connected to central $CENTRAL; no socket to $LIVE_IP"
    else
      log "WARNING: no connection to $CENTRAL within 45 s (saved sign-in refused? see the log)"
    fi
  else
    log "no saved sign-in yet: electron-local.sh signin"
  fi
  echo "$pid"
}

cmd_check() {
  local pid; pid="$(root_pid)" || die "not running"
  verify_process "$pid"
  local r=0; check_sockets 1 || r=$?
  case $r in
    0) log "ok: pid $pid, central $CENTRAL connected, no socket to $LIVE_IP" ;;
    1) log "pid $pid: no connection to $CENTRAL (signed out?), no socket to $LIVE_IP" ;;
    2) kill_now; die "ABORT: socket to the live central $LIVE_IP; client killed" ;;
  esac
  tree_sockets | sed 's/^/  /' >&2
  return $r
}

cmd_signin() {
  local user=alice
  while (($#)); do
    case "$1" in
      --user) user="$2"; shift 2 ;;
      *) die "signin: unknown option $1" ;;
    esac
  done
  local pid; pid="$(root_pid)" || die "not running (electron-local.sh run)"
  local port; port="$(cdp_port)"; [[ -n "$port" ]] || die "started without a CDP port"
  local pw; pw="$(python3 -I -c 'import json,sys; print(json.load(open(sys.argv[1]))["password"])' "$DEV_DIR/run/accounts.json")"
  # Before anything is typed: the process must be the verified one and hold no socket to the live address.
  verify_process "$pid"
  local r=0; check_sockets 0 || r=$?
  ((r == 2)) && { kill_now; die "ABORT: socket to $LIVE_IP before sign-in; client killed"; }
  log "signing in as $user over CDP (port $port)"
  node_cmd "$here/electron-cdp.mjs" "$port" signin "$user" "$pw"
  wait_central 30 || { kill_now; die "ABORT: signed in but no connection to $CENTRAL; client killed"; }
  log "connected to central $CENTRAL; no socket to $LIVE_IP"
  sleep 2
  has_saved_signin || die "the sign-in was not saved in $PROFILE/config.json"
  log "restarting once to prove the saved sign-in"
  cmd_stop
  cmd_run --cdp-port "$port" >/dev/null
  node_cmd "$here/electron-cdp.mjs" "$port" wait-main 60 >/dev/null \
    || die "the saved sign-in did not reach the main layout after a restart"
  log "saved sign-in works: signed in by itself after a restart (pid $(root_pid))"
}

cmd_cdp() { # passthrough to electron-cdp.mjs on the running instance
  local port; port="$(cdp_port)"; [[ -n "$port" ]] || die "started without a CDP port"
  root_pid >/dev/null || die "not running"
  node_cmd "$here/electron-cdp.mjs" "$port" "$@"
}

cmd_open() {
  (($# >= 1)) || die "open <server> [channel]"
  local port; port="$(cdp_port)"; [[ -n "$port" ]] || die "started without a CDP port"
  root_pid >/dev/null || die "not running"
  node_cmd "$here/open-channel.mjs" "$port" "$@"
}

untouched_report() {
  local touched
  touched="$(find "$REAL_PROFILE" "${REAL_KEYRINGS[@]}" -newer "$STATE/real.stamp" 2>/dev/null | head -5)"
  if [[ -n "$touched" ]]; then
    log "WARNING: written since the first launch of this session (the real client or another keyring user?):"
    echo "$touched" >&2
    return 1
  fi
  log "owner's profile ($REAL_PROFILE), keyrings (~/.local/share/keyrings, kwalletd) and mimeapps.list: untouched" \
      "since $(date -r "$STATE/real.stamp" -Iseconds)"
}

cmd_stop() {
  local pid
  if pid="$(root_pid)"; then
    kill -TERM "$pid" 2>/dev/null || true
    local i
    for i in $(seq 100); do [[ -z "$(tree_pids)" ]] && break; sleep 0.1; done
    if [[ -n "$(tree_pids)" ]]; then
      log "still running after 10 s: SIGKILL"
      kill_now; sleep 0.5
    fi
  elif [[ -n "$(tree_pids)" ]]; then
    log "stray processes with this profile: SIGKILL"
    kill_now; sleep 0.5
  fi
  rm -f "$STATE/client.pid"
  [[ -z "$(tree_pids)" ]] || die "processes left: $(tree_pids | tr '\n' ' ')"
  bus_down
  if [[ -f "$STATE/real.stamp" ]]; then
    untouched_report || true
    rm -f "$STATE/real.stamp"
  fi
  log "stopped"
}

cmd_paths() {
  cat <<EOF
repo        $REPO
worktree    $TREE ($(git -C "$TREE" rev-parse --short HEAD 2>/dev/null || echo none))
target      $TARGET
profile     $PROFILE_ROOT (user-data/ = --user-data-dir, keyring/ = throwaway gnome-keyring, session-bus = private bus)
state       $STATE (client.log, client.cmd, build.env, probe.ok, xdg-settings.log, bus.conf)
binary      $(stamp_get EB_BIN) ($(stamp_get EB_MODE), $(stamp_get EB_COMMIT | cut -c1-10))
addon       $(stamp_get EB_ADDON)
central     $CENTRAL
EOF
}

case "${1:-}" in
  build) shift; cmd_build "$@" ;;
  probe) shift; cmd_probe ;;
  reprobe) shift; rm -f "$STATE/probe.ok"; cmd_probe ;;
  run) shift; cmd_run "$@" ;;
  signin) shift; cmd_signin "$@" ;;
  open) shift; cmd_open "$@" ;;
  home) shift; cmd_cdp home "$@" ;;
  state) shift; cmd_cdp state "$@" ;;
  blur) shift; cmd_cdp blur "$@" ;;
  dismiss-banner) shift; cmd_cdp dismiss-banner "$@" ;;
  minimize) shift; cmd_cdp minimize "$@" ;;
  screenshot) shift; cmd_cdp screenshot "$@" ;;
  wait-main) shift; cmd_cdp wait-main "$@" ;;
  check) shift; cmd_check ;;
  stop) shift; cmd_stop ;;
  pid) root_pid || exit 1 ;;
  pids) tree_pids ;;
  port) cdp_port ;;
  cmd) shift; prepare_launch; argv=(); mapfile -d '' argv < <(launch_argv "${1:-}")
       printf '%s ' "${argv[@]}" >"$STATE/client.cmd"; echo >>"$STATE/client.cmd"; printf '%q ' "${argv[@]}"; echo ;;
  paths) cmd_paths ;;
  *) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//'; exit 2 ;;
esac
