#!/usr/bin/env node
// Pre-build guard: on Linux, build the static FFmpeg (build-ffmpeg-linux.sh
// → vendor/ffmpeg) if it isn't present, so the addon links our
// interposition-proof static libav* rather than silently falling back to
// the system shared libs (which Electron's bundled libffmpeg.so would then
// shadow, leaving the encoder probe empty). No-op on Windows (vcpkg) and
// macOS (no FFmpeg dep).
const { existsSync } = require("node:fs");
const { execFileSync } = require("node:child_process");
const path = require("node:path");

if (process.platform !== "linux") process.exit(0);

const lib = path.join(__dirname, "..", "vendor", "ffmpeg", "lib", "libavcodec.a");
if (existsSync(lib)) process.exit(0);

console.log(
  "[ensure-ffmpeg] static FFmpeg not found — building it (one-time, a few minutes)…",
);
execFileSync("bash", [path.join(__dirname, "build-ffmpeg-linux.sh")], {
  stdio: "inherit",
});
