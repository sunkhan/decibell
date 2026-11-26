#!/usr/bin/env bash
# Records what Chromium's editing does with real key events (input-keys.js) in the renderer's own
# Electron, with a throwaway profile: no network, no clipboard, a hidden window.
#
#   input-keys.sh <out.json>        (ELECTRON=<binary> to use another Electron)
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
repo="$(cd "$here/../../../../.." && pwd)"
electron="${ELECTRON:-$repo/electron-client/node_modules/electron/dist/electron}"
[ -x "$electron" ] || { echo "input-keys.sh: $electron is missing (npm ci in electron-client)" >&2; exit 1; }
out="$(realpath -m "$1")"
profile="$(mktemp -d)"
trap 'rm -rf "$profile"' EXIT
"$electron" --user-data-dir="$profile" --no-sandbox "$here/input-keys.js" --out "$out" 2>/dev/null
