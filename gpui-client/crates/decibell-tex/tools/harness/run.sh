#!/usr/bin/env bash
# Reference harness: lays out every corpus formula with KaTeX in a hidden Electron window
# (Chromium) and records glyph / rule / SVG positions.
#   run.sh <corpus.jsonl> <out.jsonl[.gz]> [--dsf N] [--filter id-regex] [--batch N]
# Electron is the repo's (electron-client/node_modules/electron); the profile is a throwaway
# directory removed on exit; the page may load file:// URLs only.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
repo="$(cd "$here/../../../../.." && pwd)"
electron="${ELECTRON:-$repo/electron-client/node_modules/electron/dist/electron}"
[ $# -ge 2 ] || { sed -n '2,6p' "$0"; exit 2; }
export HARNESS_CORPUS="$(realpath "$1")"
export HARNESS_OUT="$(realpath -m "$2")"
shift 2
export HARNESS_DSF=2
while [ $# -gt 0 ]; do
    case "$1" in
        --dsf) export HARNESS_DSF="$2"; shift 2 ;;
        --filter) export HARNESS_FILTER="$2"; shift 2 ;;
        --batch) export HARNESS_BATCH="$2"; shift 2 ;;
        *) echo "unknown option $1" >&2; exit 2 ;;
    esac
done
profile="$(mktemp -d "${HARNESS_TMP:-${TMPDIR:-/tmp}}/decibell-tex-harness.XXXXXX")"
trap 'rm -rf "$profile"' EXIT
# --disable-gpu: no GL context is needed for layout (and GLX fails in hidden windows here).
"$electron" --no-sandbox --disable-gpu --user-data-dir="$profile" "$here/main.cjs" 2>&1 \
    | grep -v -E '^\[[0-9:/.]+ ?[A-Z]*:|Fontconfig warning' >&2 || true
test -s "$HARNESS_OUT"
