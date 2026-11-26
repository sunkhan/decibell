#!/usr/bin/env bash
# measure.sh: samples a desktop client's whole process tree on Linux (CPU, private memory, per-thread wakeups,
# GPU, frames handed to the compositor, focus) and writes samples.csv / threads.csv / summary.json.
#   measure.sh --pid <root-pid> | --match <regex> [--settle 120] [--duration 300] [--interval 1] [--label ...]
# Run with --help for every option. Implementation: linux/measure.py (stdlib + /proc; jeepney for KWin).
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -B "$here/linux/measure.py" "$@"
