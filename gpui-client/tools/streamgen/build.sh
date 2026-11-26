#!/usr/bin/env bash
# Builds tools/streamgen/streamgen.c against the client's FFmpeg prefix (gpui-client/vendor/ffmpeg,
# tools/build-ffmpeg.sh) into gpui-client/vendor/bin/streamgen.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
PREFIX="${FFMPEG_PREFIX:-$ROOT/vendor/ffmpeg}"
[ -f "$PREFIX/lib/pkgconfig/libavcodec.pc" ] || { echo "streamgen: no FFmpeg at $PREFIX (run tools/build-ffmpeg.sh)" >&2; exit 1; }
mkdir -p "$ROOT/vendor/bin"
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"
# shellcheck disable=SC2046
cc -O2 -Wall "$HERE/streamgen.c" -o "$ROOT/vendor/bin/streamgen" \
  $(pkg-config --static --cflags --libs libavcodec libavutil) -lm
echo "$ROOT/vendor/bin/streamgen"
