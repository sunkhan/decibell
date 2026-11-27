#!/usr/bin/env bash
# media-clips.sh: the test clips of the attachment-playback scenarios (scenarios.sh <client> media-play-*), made with
# the system's ffmpeg from synthetic sources (testsrc2 + a quiet 220 Hz sine at -40 dB). Nothing is downloaded and
# nothing is committed: the clips live in $DECIBELL_DEV_DIR/perf-media (default ~/.cache/decibell-dev/perf-media).
#
#   media-clips.sh              make the clips that are missing; prints one line per clip: <name>\t<path>
#   media-clips.sh path <name>  the path of one clip (made if missing): h264 | vp9
#   media-clips.sh info <name>  ffprobe's facts of it as JSON (codec, size, frame rate, frames, duration, bit rate)
#   media-clips.sh --force      make them again
#
#   h264   1920x1080, 60 fps, H.264 High (libx264, ~10 Mbit/s, a keyframe every 2 s) + AAC 128 kbit/s, MP4 (faststart)
#   vp9    1920x1080, 30 fps, VP9 profile 0 (libvpx-vp9, ~6 Mbit/s, a keyframe every 2 s) + Opus 96 kbit/s, WebM
# Both are 34 s: a 20 s measurement window fits behind a seek to the start with room on both sides.
set -euo pipefail

DIR="${DECIBELL_DEV_DIR:-$HOME/.cache/decibell-dev}/perf-media"
SECONDS_LONG=34
die() { echo "media-clips: $*" >&2; exit 1; }

clip_path() {
  case "$1" in
    h264) echo "$DIR/perf-1080p60-h264.mp4" ;;
    vp9) echo "$DIR/perf-1080p30-vp9.webm" ;;
    *) die "no clip named $1 (h264, vp9)" ;;
  esac
}

make_clip() {
  local name="$1" out tmp
  out="$(clip_path "$name")"
  [[ -s "$out" ]] && return 0
  command -v ffmpeg >/dev/null || die "ffmpeg is not installed"
  mkdir -p "$DIR"
  local audio=(-f lavfi -i "sine=frequency=220:sample_rate=48000:duration=$SECONDS_LONG" -af volume=-40dB -ac 2)
  echo "media-clips: making $out" >&2
  case "$name" in
    h264)
      tmp="$DIR/.tmp-$$.mp4"
      ffmpeg -nostdin -hide_banner -loglevel error -y \
        -f lavfi -i "testsrc2=size=1920x1080:rate=60:duration=$SECONDS_LONG" "${audio[@]}" \
        -c:v libx264 -preset medium -profile:v high -pix_fmt yuv420p -b:v 10M -maxrate 12M -bufsize 20M -g 120 -bf 2 \
        -c:a aac -b:a 128k -movflags +faststart -shortest "$tmp"
      ;;
    vp9)
      tmp="$DIR/.tmp-$$.webm"
      ffmpeg -nostdin -hide_banner -loglevel error -y \
        -f lavfi -i "testsrc2=size=1920x1080:rate=30:duration=$SECONDS_LONG" "${audio[@]}" \
        -c:v libvpx-vp9 -pix_fmt yuv420p -b:v 6M -deadline good -cpu-used 4 -row-mt 1 -g 60 \
        -c:a libopus -b:a 96k -shortest "$tmp"
      ;;
  esac
  mv "$tmp" "$out"
}

info() {
  local f; f="$(clip_path "$1")"
  ffprobe -v error -select_streams v:0 -count_packets \
    -show_entries stream=codec_name,profile,width,height,avg_frame_rate,nb_read_packets,pix_fmt:format=duration,bit_rate,size \
    -of json "$f" | python3 -I -c '
import json, sys
j = json.load(sys.stdin); s = j["streams"][0]; f = j["format"]
n, d = (s["avg_frame_rate"].split("/") + ["1"])[:2]
print(json.dumps({"file": sys.argv[1], "codec": s["codec_name"], "profile": s.get("profile"), "pix_fmt": s.get("pix_fmt"),
                  "width": s["width"], "height": s["height"], "fps": round(float(n) / float(d), 3),
                  "frames": int(s["nb_read_packets"]), "duration_s": round(float(f["duration"]), 3),
                  "mbit_s": round(float(f["bit_rate"]) / 1e6, 2), "bytes": int(f["size"])}))' "$f"
}

case "${1:-}" in
  path) make_clip "${2:?name}"; clip_path "$2" ;;
  info) make_clip "${2:?name}"; info "$2" ;;
  --force) rm -f "$(clip_path h264)" "$(clip_path vp9)"; "$0" ;;
  "") for n in h264 vp9; do make_clip "$n"; printf '%s\t%s\n' "$n" "$(clip_path "$n")"; done ;;
  -h|--help) sed -n '2,/^set -euo/p' "$0" | sed '$d; s/^# \{0,1\}//' ;;
  *) die "unknown argument $1" ;;
esac
