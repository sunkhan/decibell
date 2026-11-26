#!/usr/bin/env bash
# startup.sh: times a desktop client's startup on Linux (launch -> first visible window of its process tree via KWin
# scripting or X11, launch -> settled, optional --ready-marker on stdout), over warm runs; writes startup.json.
#   startup.sh [--runs 5] [--discard 1] [--settle-cpu 2 --settle-secs 2] [--timeout 60] [--ready-marker RE] -- cmd...
# Run with --help for every option. Implementation: linux/startup.py.
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -B "$here/linux/startup.py" "$@"
