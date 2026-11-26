#!/usr/bin/env bash
# Builds the preview page (runtime/ + pages/ + the real electron-client/src) into .work/dist.
# Uses electron-client's node_modules (vite, plugins, react, tailwind); installs nothing.
# Every path is absolute: a relative out dir plus a `cd` once wrote the bundle elsewhere and a
# stale one kept loading.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../../.." && pwd -P)"
EC="$REPO/electron-client"
WORK="$HERE/.work"

if [[ ! -d "$EC/node_modules/vite" ]]; then
  echo "build.sh: $EC/node_modules is missing; run npm ci in electron-client first" >&2
  exit 1
fi

mkdir -p "$WORK"
# Bare imports from runtime/ and pages/ resolve through this link (gitignored). The second
# link is for anything resolved from the work dir itself (app.css lives there).
ln -sfn "$EC/node_modules" "$HERE/node_modules"
ln -sfn "$EC/node_modules" "$WORK/node_modules"

# The app's stylesheet, as the app loads it, plus @source lines: without them Tailwind never
# sees the classes used in electron-client/src or in the pages and leaves them out.
cat > "$WORK/app.css" <<EOF
@import "$EC/src/styles/globals.css";
@source "$EC/src";
@source "$HERE/pages";
@source "$HERE/runtime";
EOF

# Vite through its JS API (configFile: false): a config file would be bundled into
# node_modules/.vite-temp, i.e. inside electron-client/node_modules.
if command -v node >/dev/null 2>&1; then
  node "$HERE/build.mjs" "$HERE" "$REPO"
else
  ELECTRON_RUN_AS_NODE=1 "$EC/node_modules/.bin/electron" "$HERE/build.mjs" "$HERE" "$REPO"
fi
