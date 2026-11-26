#!/usr/bin/env bash
# Regenerates (or with --check, verifies) the static font faces under assets/fonts/static and
# crates/decibell-ui/src/fonts_generated.rs. See gen-fonts.py for what and why.
#
# Needs a Python 3 with fontTools: the system one, else a virtualenv under
# ~/.cache/decibell-dev/fonttools-venv (created here on first use; nothing is installed
# system-wide).
set -euo pipefail

here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
venv="${XDG_CACHE_HOME:-$HOME/.cache}/decibell-dev/fonttools-venv"

python=""
for candidate in python3 python "$venv/bin/python" "$venv/Scripts/python.exe"; do
    if command -v "$candidate" >/dev/null 2>&1 && "$candidate" -c 'import fontTools' 2>/dev/null; then
        python="$candidate"
        break
    fi
done
if [ -z "$python" ]; then
    echo "gen-fonts: no Python with fontTools; creating $venv" >&2
    python3 -m venv "$venv"
    "$venv/bin/python" -m pip install --quiet fonttools
    python="$venv/bin/python"
fi

exec "$python" "$here/gen-fonts.py" "$@"
