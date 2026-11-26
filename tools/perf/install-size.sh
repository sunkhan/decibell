#!/usr/bin/env bash
# install-size.sh: apparent size, size on disk, file count and the largest files of an install (measures only;
# runs nothing from it). MB = 2^20 bytes, like the Windows scripts.
#   install-size.sh [--top 15] [--label L] [--out DIR] <dir-or-file>...
set -euo pipefail
here="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
exec python3 -B "$here/linux/install_size.py" "$@"
