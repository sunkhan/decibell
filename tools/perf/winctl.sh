#!/usr/bin/env bash
# winctl.sh: list and drive windows on KDE Plasma through a one-shot KWin script (Wayland or X11, no root).
#   winctl.sh list | activate <pid> | unfocus <pid> | geometry <pid> <x> <y> <w> <h> | close <pid> | cleanup
# Run with --help for details. Implementation: linux/winctl.py (+ linux/kwin.py).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -B "$here/linux/winctl.py" "$@"
