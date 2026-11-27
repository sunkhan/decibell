#!/usr/bin/env bash
# check-ship.sh: refuses a `decibell` binary that is not a shipped build: one that would honour
# DECIBELL_CENTRAL (reach a test central), carries development code (screens, self-tests, fixtures,
# test hooks, the local stack's accounts), says another version, or (Linux) links an unexpected
# shared library. Run by the packaging scripts before anything is packed; looks only, runs the
# binary only for `--version`.
#
#   packaging/check-ship.sh <decibell binary> <expected version>
set -euo pipefail

bin="$1"
want="$2"
fail() { echo "check-ship: $*" >&2; exit 1; }
[ -f "$bin" ] || fail "no binary at $bin"

# Strings that exist only in development code. The first is the one the brief names: the core's
# `local-central` reads DECIBELL_CENTRAL; nothing else in a shipped build may name it.
forbidden=(
  DECIBELL_CENTRAL            # decibell-core's local-central, the self-tests' stack address
  DECIBELL_TEST_              # decibell-release's test hooks (feed, DSN, install kind, panic)
  DECIBELL_TEST_PROBES        # self-test probes
  DECIBELL_DEV_PASSWORD       # --login (dev-local)
  DECIBELL_DEV_CONTROL        # the perf control pipe (dev-local)
  decibell-test-1             # the local stack's test password
  accounts.json               # the local stack's account list
  idle-frames                 # a self-test's name
  shell-signin                # another
  --selftest                  # the self-test switch
  --bench-scroll              # the scroll benchmark
)
strings_out="$(mktemp)"
trap 'rm -f "$strings_out"' EXIT
LC_ALL=C grep -a -o -E '[[:print:]]{6,}' "$bin" > "$strings_out" || true
for s in "${forbidden[@]}"; do
  if grep -q -F -- "$s" "$strings_out"; then
    fail "the binary contains \"$s\": a development build (build with --no-default-features --features ship)"
  fi
done

# The version it reports is the one being packaged (electron-client/package.json).
case "$(uname -s)" in
  MINGW*|MSYS*|CYGWIN*) got="$("$bin" --version | tr -d '\r')" ;;
  *) got="$("$bin" --version)" ;;
esac
[ "$got" = "decibell $want" ] || fail "--version says \"$got\", expected \"decibell $want\""

if [ "$(uname -s)" = Linux ]; then
  command -v readelf >/dev/null || fail "readelf not found"
  readelf -n "$bin" | grep -q 'Build ID' || fail "no GNU build id (crash reports could not be symbolicated)"
  if readelf -d "$bin" | grep -q -E 'RPATH|RUNPATH'; then fail "the binary has an RPATH / RUNPATH"; fi
  # The shared libraries it may need: the system's, never something from the build machine.
  allowed='^(libc|libm|libgcc_s|libxkbcommon|libxkbcommon-x11|libxcb|libxcb-xkb|libva|libva-drm|libdrm|libopus|libpipewire-0\.3|libasound|ld-linux-x86-64)\.so'
  while read -r lib; do
    [[ "$lib" =~ $allowed ]] || fail "unexpected shared library $lib (add it to the packages' dependencies and here, or remove it)"
  done < <(readelf -d "$bin" | sed -n 's/.*Shared library: \[\(.*\)\]/\1/p')
fi
echo "check-ship: $bin is a ship build of $want"
