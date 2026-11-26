#!/usr/bin/env bash
# Build the preview and capture it:  run.sh [--pages a,b] [--themes t1,t2] [--out <dir>]
#                                           [--query k=v]... [--no-build]
# Defaults: every page in pages/, all seven themes, out = gpui-client/target/electron-captures.
# Env: CAPTURE_DPR (default 1), CAPTURE_TIMEOUT_S (default 300), CAPTURE_GRAYSCALE_TEXT=1.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../../.." && pwd -P)"
ELECTRON="$REPO/electron-client/node_modules/.bin/electron"

build=1
args=()
while [[ $# -gt 0 ]]; do
  case "$1" in
    --no-build) build=0; shift ;;
    --pages|--themes|--query) args+=("$1=$2"); shift 2 ;;
    --out) args+=("--out=$(realpath -m "$2")"); shift 2 ;;
    --pages=*|--themes=*|--query=*) args+=("$1"); shift ;;
    --out=*) args+=("--out=$(realpath -m "${1#--out=}")"); shift ;;
    -h|--help) sed -n 2,5p "$0"; exit 0 ;;
    *) echo "run.sh: unknown option $1" >&2; exit 2 ;;
  esac
done

[[ $build == 1 ]] && "$HERE/build.sh"

# A throwaway profile under the gitignored work dir (shoot.cjs sets the same path).
exec "$ELECTRON" "$HERE/shoot.cjs" --user-data-dir="$HERE/.work/profile" "${args[@]}"
