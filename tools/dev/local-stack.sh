#!/usr/bin/env bash
# Decibell local test stack: PostgreSQL + central + three community servers on
# loopback, seeded with test accounts, for real clients to sign in to.
# Never talks to the live central. See tools/dev/README.md.
#
#   local-stack.sh setup        build deps + both servers (idempotent)
#   local-stack.sh up           start postgres, central, the communities
#   local-stack.sh seed         fill the stack with the test fixtures (idempotent)
#   local-stack.sh status       what runs, ports, the client env
#   local-stack.sh down         stop everything
#   local-stack.sh logs <name>  tail a log (central, postgres, gaming, rust, design)
#   local-stack.sh reset        stop and wipe all data (keeps TLS certs + JWT key)
#   local-stack.sh client-env   print the env a client needs
#   local-stack.sh smoke        run tools/dev/smoke.py against the running stack
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
REPO="$(cd "$HERE/../.." && pwd)"
DEV="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}"
JOBS="${DECIBELL_DEV_JOBS:-8}"
CDEPS="$DEV/deps/community"
ZDEPS="$DEV/deps/central"
BIN="$DEV/bin"
PY="$ZDEPS/pgvenv/bin/python"
export DECIBELL_DEV_DIR="$DEV" DECIBELL_DEV_REPO="$REPO"

die() { echo "local-stack: $*" >&2; exit 1; }
log() { echo "[setup] $*"; }

# True when $1 is missing or any later argument (file or directory, searched
# recursively) is newer than it.
stale() {
  local target="$1"; shift
  [ -e "$target" ] || return 0
  local src
  for src in "$@"; do
    [ -e "$src" ] || continue
    if [ -n "$(find "$src" -newer "$target" -type f -print -quit 2>/dev/null)" ]; then return 0; fi
  done
  return 1
}

need_tools() {
  local missing=() t
  for t in "$@"; do command -v "$t" >/dev/null 2>&1 || missing+=("$t"); done
  [ ${#missing[@]} -eq 0 ] || die "missing tools: ${missing[*]}"
}

# The harness scripts build with -j"$(nproc)"; other builds share this
# machine, so a wrapper nproc caps them without editing the scripts.
cap_jobs() {
  local real; real="$(command nproc)"
  [ "$real" -lt "$JOBS" ] && JOBS="$real"
  mkdir -p "$DEV/toolbin"
  printf '#!/bin/sh\necho %s\n' "$JOBS" > "$DEV/toolbin/nproc"
  chmod +x "$DEV/toolbin/nproc"
  export PATH="$DEV/toolbin:$PATH"
}

build_server() {  # name script-dir deps out sources...
  local name="$1" tests="$2" deps="$3" out="$4"; shift 4
  if stale "$out" "$tests/build.sh" "$deps/pb/messages.pb.cc" "$@"; then
    log "building $name (g++, one job) ..."
    local t0=$SECONDS
    if bash "$tests/build.sh" "$deps" "$REPO" "$out.tmp" > "$DEV/build-$name.log" 2>&1; then
      mv -f "$out.tmp" "$out"
      log "$name built in $((SECONDS - t0)) s -> $out"
    else
      rm -f "$out.tmp"
      echo "---- $DEV/build-$name.log ----" >&2; tail -40 "$DEV/build-$name.log" >&2
      return 1
    fi
  else
    log "$name is up to date ($out)"
  fi
}

cmd_setup() {
  need_tools curl tar zstd protoc g++ gcc cmake openssl pkg-config uv sqlite3
  cap_jobs
  mkdir -p "$CDEPS" "$ZDEPS" "$BIN"
  local t0=$SECONDS

  # Community harness deps (headers + C++/Python protobuf).
  if stale "$CDEPS/.deps-stamp" "$REPO/proto/messages.proto" "$REPO/src/community/tests/setup_deps.sh"; then
    log "community deps -> $CDEPS"
    bash "$REPO/src/community/tests/setup_deps.sh" "$CDEPS" "$REPO" > "$DEV/setup-community.log" 2>&1 \
      || { tail -30 "$DEV/setup-community.log" >&2; die "community setup_deps.sh failed"; }
    touch "$CDEPS/.deps-stamp"
  else
    log "community deps are up to date"
  fi

  # Central harness deps: the same headers (boost shared with the community
  # copy), a uv venv with pgserver's bundled PostgreSQL 16, libpqxx 7.
  if [ ! -e "$ZDEPS/boost" ] && [ -d "$CDEPS/boost/usr/include/boost" ]; then
    ln -s "$CDEPS/boost" "$ZDEPS/boost"
  fi
  if stale "$ZDEPS/.deps-stamp" "$REPO/proto/messages.proto" "$REPO/src/server/tests/setup_deps.sh"; then
    log "central deps -> $ZDEPS (venv, libpqxx with -j$JOBS) ..."
    bash "$REPO/src/server/tests/setup_deps.sh" "$ZDEPS" "$REPO" > "$DEV/setup-central.log" 2>&1 \
      || { tail -30 "$DEV/setup-central.log" >&2; die "central setup_deps.sh failed"; }
    touch "$ZDEPS/.deps-stamp"
  else
    log "central deps are up to date"
  fi
  [ -x "$PY" ] || die "no venv python at $PY"
  [ -x "$ZDEPS/pginstall/bin/pg_ctl" ] || die "no pg_ctl in $ZDEPS/pginstall/bin"
  # seed draws its attachment pictures + JPEG thumbnails with Pillow.
  if ! "$PY" -I -c 'import PIL' 2>/dev/null; then
    log "adding Pillow to the venv"
    VIRTUAL_ENV="$ZDEPS/pgvenv" uv pip install -q pillow
  fi

  # The bind shim (see bindshim.c).
  if stale "$BIN/bindshim.so" "$HERE/bindshim.c"; then
    gcc -shared -fPIC -O2 -Wall -o "$BIN/bindshim.so" "$HERE/bindshim.c" -ldl
    log "bindshim.so built"
  fi

  # Both servers, side by side (one g++ each).
  local common=("$REPO/src/common/"*.hpp)
  build_server community "$REPO/src/community/tests" "$CDEPS" "$BIN/community_server" \
    "$REPO/src/community/"*.cpp "$REPO/src/community/"*.hpp "${common[@]}" &
  local cpid=$!
  build_server central "$REPO/src/server/tests" "$ZDEPS" "$BIN/central_server" \
    "$REPO/src/server/main.cpp" "$REPO/src/server/auth_manager.cpp" "$REPO/src/server/"*.hpp \
    "$REPO/src/server/bcrypt.h" "${common[@]}" &
  local zpid=$!
  local rc=0
  wait "$cpid" || rc=1
  wait "$zpid" || rc=1
  [ $rc -eq 0 ] || die "a server build failed (logs: $DEV/build-*.log)"
  log "done in $((SECONDS - t0)) s. Next: $0 up && $0 seed"
}

case "${1:-}" in
  setup) cmd_setup ;;
  up|down|status|logs|reset|seed|client-env)
    [ -x "$PY" ] || die "not set up yet: run '$0 setup' first"
    exec "$PY" -I -B "$HERE/stack.py" "$@" ;;
  smoke)
    [ -x "$PY" ] || die "not set up yet: run '$0 setup' first"
    shift; exec "$PY" -I -B "$HERE/smoke.py" "$@" ;;
  ""|-h|--help|help)
    sed -n '2,14p' "$0" | sed 's/^# \{0,1\}//' ;;
  *) die "unknown command '$1' (try --help)" ;;
esac
