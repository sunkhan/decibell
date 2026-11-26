#!/usr/bin/env bash
# Typechecks runtime/ and pages/ against the real electron-client/src, with the renderer's own
# compiler options (strict, noUnused*). The config is written under .work/ with a non-default name,
# so Vite never picks it up.
set -euo pipefail

HERE="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd -P)"
REPO="$(cd "$HERE/../../.." && pwd -P)"
EC="$REPO/electron-client"
mkdir -p "$HERE/.work"
ln -sfn "$EC/node_modules" "$HERE/node_modules"

cat > "$HERE/.work/tsconfig.typecheck.json" <<EOF
{
  "extends": "$EC/tsconfig.web.json",
  "compilerOptions": {
    "noUncheckedSideEffectImports": false,
    "types": [],
    "baseUrl": "$HERE",
    "paths": { "@app/*": ["$EC/src/*"] }
  },
  "include": ["$HERE/runtime", "$HERE/pages", "$EC/src/types/global.d.ts", "$EC/node_modules/vite/client.d.ts"]
}
EOF
"$EC/node_modules/.bin/tsc" -p "$HERE/.work/tsconfig.typecheck.json" --noEmit
echo "typecheck: ok"
