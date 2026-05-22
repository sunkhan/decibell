#!/usr/bin/env bash
# Build a minimal STATIC, LGPL FFmpeg (NVENC + VAAPI encoders) for the
# Linux native addon.
#
# Why: Electron bundles Chromium's libffmpeg.so, which exports FFmpeg
# symbols (avcodec_find_encoder_by_name, …) into the global scope —
# decode-only, no encoders. A dynamically-linked addon's avcodec_* calls
# bind to *those* symbols instead of the system libavcodec, so the encoder
# probe finds nothing. RTLD_DEEPBIND fixes the symbol resolution but breaks
# CUDA (cuInit → OUT_OF_MEMORY). Static-linking libav* makes the symbols
# internal to the .node — nothing for Electron's libffmpeg to shadow, and
# CUDA loads normally. This is the Linux analogue of the Windows vcpkg
# FFmpeg (immune because DLL imports bind by name).
#
# Output: $PREFIX (default native/vendor/ffmpeg, gitignored). Point the
# cargo build at it via FFMPEG_DIR.
set -euo pipefail

PREFIX="${FFMPEG_PREFIX:-$(cd "$(dirname "$0")/.." && pwd)/vendor/ffmpeg}"
BUILD="${FFMPEG_BUILD_DIR:-${TMPDIR:-/tmp}/decibell-ffmpeg-build}"
FFMPEG_VERSION="${FFMPEG_VERSION:-8.0}"
# nv-codec-headers must be recent enough for the installed driver's NVENC
# API. n13.x pairs with FFmpeg 8.x.
NVHEADERS_TAG="${NVHEADERS_TAG:-n13.0.19.0}"

mkdir -p "$BUILD" "$PREFIX"

# nv-codec-headers → ffnvcodec.pc (required by --enable-nvenc).
if [ ! -f "$PREFIX/lib/pkgconfig/ffnvcodec.pc" ]; then
  echo "[build-ffmpeg] installing nv-codec-headers $NVHEADERS_TAG"
  rm -rf "$BUILD/nv-codec-headers"
  git clone --depth 1 --branch "$NVHEADERS_TAG" \
    https://github.com/FFmpeg/nv-codec-headers.git "$BUILD/nv-codec-headers"
  make -C "$BUILD/nv-codec-headers" PREFIX="$PREFIX" install
fi

if [ ! -d "$BUILD/ffmpeg-$FFMPEG_VERSION" ]; then
  echo "[build-ffmpeg] fetching FFmpeg $FFMPEG_VERSION"
  curl -fSL "https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz" \
    | tar xJ -C "$BUILD"
fi
cd "$BUILD/ffmpeg-$FFMPEG_VERSION"

# x86 SIMD needs nasm. Without it FFmpeg still builds (C fallbacks), just
# slower — fine for validation. CI/release builds should install nasm.
ASM_FLAG=""
if ! command -v nasm >/dev/null 2>&1; then
  echo "[build-ffmpeg] WARNING: nasm not found — building without x86 asm (slower). Install nasm for an optimized build."
  ASM_FLAG="--disable-x86asm"
fi

# Minimal LGPL build: only the encoders + scaler we use. No --enable-gpl
# (no libx264), so the resulting binary stays LGPL — there is no software
# encode fallback on Linux (hardware-only: NVENC/VAAPI).
PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig:${PKG_CONFIG_PATH:-}" \
./configure \
  --prefix="$PREFIX" \
  --pkg-config-flags="--static" \
  --extra-cflags="-I$PREFIX/include" \
  --extra-ldflags="-L$PREFIX/lib" \
  $ASM_FLAG \
  --enable-static --disable-shared --enable-pic \
  --disable-programs --disable-doc --disable-network \
  --disable-everything \
  --enable-avcodec --enable-avutil --enable-swscale \
  --enable-avformat --enable-swresample \
  --enable-ffnvcodec --enable-nvenc \
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc \
  --enable-vaapi \
  --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi \
  --enable-hwaccel=h264_vaapi,hevc_vaapi,av1_vaapi \
  --enable-filter=scale \
  --enable-protocol=file

make -j"$(nproc)"
make install
echo "[build-ffmpeg] static FFmpeg $FFMPEG_VERSION installed to $PREFIX"
echo "[build-ffmpeg] set FFMPEG_DIR=$PREFIX for the cargo build"
