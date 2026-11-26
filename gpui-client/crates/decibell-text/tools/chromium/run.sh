#!/usr/bin/env bash
# Runs the Chromium side of the fidelity harness: lays a corpus out in Electron (the renderer's
# own Chromium) and writes its line breaks and metrics.
#
#   run.sh <corpus.json> <out.json> [--dsf 1|2] [--shots <dir>]
#
# Uses electron-client/node_modules/electron with a throwaway profile; no network, and nothing
# of the Decibell app or its profile is read. --shots shows a window for a moment (a hidden
# window is not painted).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../../.." && pwd)"
electron="$repo/electron-client/node_modules/electron/dist/electron"
[ -x "$electron" ] || { echo "run.sh: $electron is missing (npm ci in electron-client)" >&2; exit 1; }
corpus="$(realpath "$1")"; out="$(realpath -m "$2")"; shift 2
args=()
while [ $# -gt 0 ]; do
  case "$1" in
    --shots) args+=(--shots "$(realpath -m "$2")"); shift 2 ;;
    *) args+=("$1"); shift ;;
  esac
done
profile="$(mktemp -d)"
trap 'rm -rf "$profile"' EXIT
"$electron" --user-data-dir="$profile" --no-sandbox "$here/main.js" \
  --corpus "$corpus" --out "$out" "${args[@]}" 2> >(grep -a -e '^page: harness' -e '^usage' -e 'Error:.*harness' >&2 || true)
