#!/usr/bin/env bash
# The build-level half of verifying a tree before it is pushed (HANDOFF "Resume here"): the Windows type-check
# with shaders, the unit tests, the development and ship release builds, the ship check, and the release
# tests against their loopback fakes. Each heavy step takes a slot (tools/heavy.sh); the release tests open
# windows, so they run under the self-test lock. Run the self-tests afterwards with tools/selftest.sh.
#
#   tools/verify-tree.sh <log>          (in the tree to verify: the main checkout or a landing worktree)
#
# SHIP names the ship build's target directory (default ~/.cache/decibell-dev/target-ship; give a landing
# tree its own, e.g. SHIP=~/.cache/decibell-dev/target-ship-land).
set -uo pipefail
LOG=${1:?a log file}
cd "$(dirname "$0")/.."
H=tools/heavy.sh
SHIP=${SHIP:-$HOME/.cache/decibell-dev/target-ship}
{
echo "== head $(git log --oneline -1 | cut -c1-70)  $(date +%H:%M)"
echo "== win-check";  $H tools/win-check.sh --shaders 2>&1 | grep -E "^error|Finished|could not compile" | tail -4; echo "rc=${PIPESTATUS[0]}"
echo "== unit tests"
$H cargo test --locked --workspace --lib --bins 2>&1 | grep -E "^test result|FAILED|failed|panicked|^error" | awk '/^test result/{p+=$4; f+=$6; i+=$8; next}{print} END{print "passed="p" failed="f" ignored="i}'
echo "== release build (dev-local)"
$H cargo build --locked --release -p decibell-app --features dev-local 2>&1 | tail -1
$H cargo build --locked --release -p decibell-model --features local-central --bin decibell-headless 2>&1 | tail -1
ls -l target/release/decibell | awk '{print "dev-local binary bytes: "$5}'
echo "== ship build  $(date +%H:%M)"
CARGO_TARGET_DIR=$SHIP $H cargo build --locked --release -p decibell-app --no-default-features --features ship 2>&1 | tail -1
V=$(python3 -I -c 'import json; print(json.load(open("../electron-client/package.json"))["version"])')
cp "$SHIP/release/decibell" "$SHIP/decibell-stripped" && strip "$SHIP/decibell-stripped" && ls -l "$SHIP/decibell-stripped" | awk '{print "ship binary bytes (stripped): "$5}'
echo "== check-ship"
packaging/check-ship.sh "$SHIP/decibell-stripped" "$V" 2>&1 | tail -4; echo "rc=${PIPESTATUS[0]}"
echo "-- and it refuses the development binary:"
packaging/check-ship.sh target/release/decibell "$V" 2>&1 | tail -2; echo "rc=${PIPESTATUS[0]} (non-zero expected)"
echo "== release tests (loopback fakes)  $(date +%H:%M)"
flock --close "${XDG_CACHE_HOME:-$HOME/.cache}/decibell-dev/selftest.lock" timeout 1200 python3 -I packaging/tests/release_e2e.py --binary target/release/decibell 2>&1 | grep -E "^PASS|^FAIL|passed|failed|Traceback|Error" | cut -c1-200
echo "== done $(date +%H:%M)"
} > "$LOG" 2>&1
