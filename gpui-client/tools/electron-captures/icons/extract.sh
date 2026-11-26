#!/usr/bin/env bash
# Re-extracts the renderer's icon components into gpui-client/assets/icons/ (see extract.mjs).
set -euo pipefail
here="$(cd "$(dirname "$0")" && pwd)"
out="$here/../.work/icons-dump"
"$here/../run.sh" --pages icons-dump --themes nocturne --out "$out"
node "$here/extract.mjs" "$out/icons-dump-nocturne.json"
