#!/usr/bin/env bash
# Run a build so that agents working side by side can't run the machine out of memory (HANDOFF "Resume here").
#
#   tools/heavy.sh <command…>           a heavy build: takes one of 3 slots, waits its turn when all are taken
#   tools/heavy.sh --light <command…>   anything else that compiles (cargo check, one crate's tests): no slot
#
# Heavy = a release build (ship, development, decibell-headless), `cargo test --workspace`, any build or test
# of decibell-app, tools/win-check.sh.
#
# Both forms run the command in one systemd user slice (`decibell-builds.slice`) with a memory ceiling shared
# by every build on the machine: a build that outgrows it is killed alone (the build fails, rerun it), never
# the desktop or the terminal. That ceiling is what makes three builds at once safe; the slots only keep the
# machine responsive. Why: swap here is zram (compressed RAM, not disk) and /tmp is tmpfs, so "31 GB RAM +
# 31 GB swap" is 31 GB; on 2026-11-26 some twenty rustc processes (25 GB) took the session down three times.
#
# Tunables: DECIBELL_HEAVY_SLOTS (3), CARGO_BUILD_JOBS (8), DECIBELL_BUILD_MEM_HIGH (18G: above it the builds
# are slowed and pushed to swap), DECIBELL_BUILD_MEM_MAX (21G: above it the largest is killed),
# DECIBELL_BUILD_SWAP_MAX (4G).
set -euo pipefail

light=0
[ "${1:-}" = --light ] && { light=1; shift; }
[ $# -gt 0 ] || { sed -n '2,7p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 1; }

export CARGO_BUILD_JOBS=${CARGO_BUILD_JOBS:-8}

if [ "$light" = 0 ]; then
  dir=${XDG_CACHE_HOME:-$HOME/.cache}/decibell-dev/heavy-slots
  mkdir -p "$dir"
  told=0
  while :; do
    for i in $(seq 1 "${DECIBELL_HEAVY_SLOTS:-3}"); do
      exec {slot}>"$dir/$i"
      # The descriptor stays open in the command we exec, so the slot is held until the build ends.
      flock -n "$slot" && break 2
      exec {slot}>&-
    done
    [ "$told" = 1 ] || { echo "heavy.sh: every slot is busy, waiting for one" >&2; told=1; }
    sleep 2
  done
fi

slice=decibell-builds.slice
if command -v systemd-run >/dev/null 2>&1 \
  && systemctl --user set-property --runtime "$slice" \
       MemoryHigh="${DECIBELL_BUILD_MEM_HIGH:-18G}" MemoryMax="${DECIBELL_BUILD_MEM_MAX:-21G}" \
       MemorySwapMax="${DECIBELL_BUILD_SWAP_MAX:-4G}" 2>/dev/null; then
  exec systemd-run --user --scope --quiet --collect --slice="$slice" -- "$@"
fi
echo "heavy.sh: no systemd user session, running without the memory ceiling" >&2
exec "$@"
