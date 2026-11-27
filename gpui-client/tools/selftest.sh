#!/usr/bin/env bash
# Run self-tests one at a time on this machine, each in its own process with a fresh profile.
#
#   tools/selftest.sh [--bin <decibell>] [--keep-logs <dir>] <name>...        (names as `--selftest list` prints)
#
# Why a wrapper: a self-test drives a real window on the desktop. Two test windows open at once take each
# other's focus and pointer, and frame counts, clicks, typing and drags then fail at random (2026-11-27:
# `input-typing`, `kit-idle`, `idle-frames`, `themes` each failed and passed on alternate runs while five
# agents tested side by side). So every self-test on the machine goes through one lock, taken per test: other
# sessions' tests run between yours. A test's failure here is then worth reading.
#
# The binary defaults to this tree's `target/release/decibell` (a `dev-local` release build; the headless
# client beside it must be current: HANDOFF "Self-tests need the headless client"). DECIBELL_CENTRAL defaults
# to the local stack. Exit status: 0 when every test passed; a test that printed SKIPPED (a missing fixture or
# helper) is reported as SKIP and fails the run.
set -uo pipefail

here=$(cd "$(dirname "$0")" && pwd)
bin="$here/../target/release/decibell"
logs=
while [ $# -gt 0 ]; do
  case "$1" in
    --bin) bin=$2; shift 2 ;;
    --keep-logs) logs=$2; shift 2 ;;
    -h|--help) sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//'; exit 0 ;;
    *) break ;;
  esac
done
[ $# -gt 0 ] || { sed -n '2,5p' "$0" | sed 's/^# \{0,1\}//' >&2; exit 1; }
[ -x "$bin" ] || { echo "selftest.sh: no binary at $bin" >&2; exit 1; }
bin=$(cd "$(dirname "$bin")" && pwd)/$(basename "$bin")

cache=${XDG_CACHE_HOME:-$HOME/.cache}/decibell-dev
lock=$cache/selftest.lock
work=$cache/scratch/selftest-$$
mkdir -p "$work"
[ -z "$logs" ] || mkdir -p "$logs"
export DECIBELL_CENTRAL=${DECIBELL_CENTRAL:-127.0.0.1:18080}

pass=0; failed=(); skipped=()
for name in "$@"; do
  profile=$work/profile; rm -rf "$profile"; mkdir -p "$profile"
  log=$work/$name.log
  start=$(date +%s)
  # --close: the lock is held by flock itself, not inherited by the app's children, so a child that
  # outlives its test can't hold every other session's tests up.
  DECIBELL_USER_DATA_DIR=$profile flock --close "$lock" \
    timeout "${DECIBELL_SELFTEST_TIMEOUT:-420}" "$bin" --selftest "$name" >"$log" 2>&1
  rc=$?
  took=$(( $(date +%s) - start ))
  if [ $rc = 0 ] && grep -aq 'SKIPPED' "$log"; then
    # A test that could not run (no fixture, no KWin helper, the stack down) exits 0: never count it as a pass.
    skipped+=("$name"); echo "SKIP $name: $(grep -a 'SKIPPED' "$log" | head -2 | cut -c1-300 | tr '\n' '|')"
  elif [ $rc = 0 ]; then
    pass=$((pass + 1)); echo "PASS $name (${took}s, with the wait for the lock)"
  else
    failed+=("$name")
    echo "FAIL $name rc=$rc (${took}s): $(grep -aE 'FAIL|panicked|SKIPPED' "$log" | head -3 | cut -c1-300 | tr '\n' '|')"
  fi
  [ -z "$logs" ] || cp "$log" "$logs/$name.log"
done
rm -rf "$work"
echo "== $pass passed, ${#failed[@]} failed${failed:+: ${failed[*]}}, ${#skipped[@]} skipped${skipped:+: ${skipped[*]}}"
[ ${#failed[@]} = 0 ] && [ ${#skipped[@]} = 0 ]
