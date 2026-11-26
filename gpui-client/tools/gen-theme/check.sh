#!/bin/sh
# Fails (exit 1) when gpui-client/crates/decibell-theme/src/generated*.rs or gpui-client/docs/css-component-rules.md
# no longer match electron-client/src/styles/globals.css. Runs from any working directory.
# Needs electron-client/node_modules (Tailwind's theme.css is an input): `npm ci` in electron-client.
set -eu
exec node "$(dirname "$0")/gen-theme.mjs" --check
