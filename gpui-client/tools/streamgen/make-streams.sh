#!/usr/bin/env bash
# Generates the standard test streams into gpui-client/vendor/streams (gitignored; about 600 MB):
#   1080p60 at 10 Mbit/s, 1440p120 at 20, 2160p60 at 30, each in H.264, HEVC and AV1, 30 s long,
#   and switch_hevc.dbv: 1080p -> 1440p -> 2160p HEVC at 60 fps, 4 s each (a mid-stream
#   resolution change).
# With --all also 2160p120 at 40 Mbit/s. Existing files are kept (delete one to regenerate it).
# Needs an NVIDIA GPU (NVENC), like the streamer's own encode path the settings come from.
# The self-tests (decibell --selftest video-*) and tools for measuring read them from here;
# DECIBELL_VIDEO_STREAMS points them elsewhere.
set -euo pipefail
HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/../.." && pwd)"
OUT="${DECIBELL_VIDEO_STREAMS:-$ROOT/vendor/streams}"
GEN="$ROOT/vendor/bin/streamgen"
[ -x "$GEN" ] || "$HERE/build.sh" >/dev/null
mkdir -p "$OUT"

gen() { # <name> <encoder> <w> <h> <fps> <kbit/s> <seconds>
  [ -s "$OUT/$1.dbv" ] || "$GEN" "$OUT/$1.dbv.part" "$2" "$3" "$4" "$5" "$6" "$7"
  [ ! -f "$OUT/$1.dbv.part" ] || mv "$OUT/$1.dbv.part" "$OUT/$1.dbv"
}
for codec in h264 hevc av1; do
  gen "1080p60_$codec"  "${codec}_nvenc" 1920 1080 60  10000 30
  gen "1440p120_$codec" "${codec}_nvenc" 2560 1440 120 20000 30
  gen "2160p60_$codec"  "${codec}_nvenc" 3840 2160 60  30000 30
  [ "${1:-}" != "--all" ] || gen "2160p120_$codec" "${codec}_nvenc" 3840 2160 120 40000 20
done
if [ ! -s "$OUT/switch_hevc.dbv" ]; then
  tmp="$(mktemp -d)"; trap 'rm -rf "$tmp"' EXIT
  "$GEN" "$tmp/a.dbv" hevc_nvenc 1920 1080 60 10000 4
  "$GEN" "$tmp/b.dbv" hevc_nvenc 2560 1440 60 20000 4
  "$GEN" "$tmp/c.dbv" hevc_nvenc 3840 2160 60 30000 4
  python3 "$HERE/dbvcat.py" "$OUT/switch_hevc.dbv" 4 "$tmp/a.dbv" "$tmp/b.dbv" "$tmp/c.dbv"
fi
ls -l "$OUT"
