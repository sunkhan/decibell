#!/usr/bin/env bash
# Builds the GPUI client's FFmpeg into gpui-client/vendor/ffmpeg: a static, LGPL prefix that the
# whole client links once, for decode (crates/decibell-video: H.264 / HEVC / AV1 through Vulkan
# Video, NVDEC and VAAPI hwaccels, FFmpeg's software H.264 / HEVC, dav1d for AV1) and for encode
# (crates/decibell-core: NVENC and VAAPI, the same set as the Electron addon's prefix,
# electron-client/native/scripts/build-ffmpeg-linux.sh). gpui-client/.cargo/config.toml points
# pkg-config at the prefix. Why static, why this version: gpui-client/docs/video-design.md §9.
#
#   tools/build-ffmpeg.sh              # builds what is missing; a no-op when the prefix is current
#   tools/build-ffmpeg.sh --force      # rebuilds everything
#   JOBS=8 FFMPEG_PREFIX=... FFMPEG_BUILD_DIR=... tools/build-ffmpeg.sh
#
# No sudo, nothing installed system-wide. Needs from the system: a C toolchain, make, curl, tar,
# sha256sum, python3 (for meson, in a venv), ninja, pkg-config and the libva + libdrm headers
# (VAAPI). Fetched and built here, each checked against a pinned SHA-256: nasm, the Vulkan
# headers, nv-codec-headers, dav1d, FFmpeg.
#
# LGPL only: the script refuses --enable-gpl / --enable-nonfree and fails if the configured tree
# is not LGPL (dav1d is BSD-2-Clause).
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"                                  # gpui-client/
PREFIX="${FFMPEG_PREFIX:-$ROOT/vendor/ffmpeg}"
BUILD="${FFMPEG_BUILD_DIR:-$ROOT/vendor/ffmpeg-build}"
JOBS="${JOBS:-$(nproc)}"
FORCE=0
for arg in "$@"; do
  case "$arg" in
    --force) FORCE=1 ;;
    -h|--help) sed -n '2,20p' "$0"; exit 0 ;;
    *) echo "build-ffmpeg: unknown argument $arg" >&2; exit 2 ;;
  esac
done

# name, version, URL, SHA-256 of the archive. Changing a version means changing its hash.
FFMPEG_VERSION=8.1.3      # ffmpeg-next / ffmpeg-sys-next 8.1 bind FFmpeg 8.0 and 8.1
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
FFMPEG_SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
DAV1D_VERSION=1.5.3
DAV1D_URL="https://code.videolan.org/videolan/dav1d/-/archive/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.bz2"
DAV1D_SHA256=e099f53253f6c247580c554d53a13f1040638f2066edc3c740e4c2f15174ce22
NASM_VERSION=2.16.03      # x86 SIMD of the software decoders; several times slower without
NASM_URL="https://www.nasm.us/pub/nasm/releasebuilds/$NASM_VERSION/nasm-$NASM_VERSION.tar.xz"
NASM_SHA256=1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148
NVHEADERS_VERSION=n13.0.19.0   # as the Electron prefix (pairs with FFmpeg 8.x)
NVHEADERS_URL="https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/$NVHEADERS_VERSION.tar.gz"
NVHEADERS_SHA256=86d15d1a7c0ac73a0eafdfc57bebfeba7da8264595bf531cf4d8db1c22940116
VKHEADERS_VERSION=v1.4.335     # FFmpeg 8.1's Vulkan Video code needs >= 1.4.317
VKHEADERS_URL="https://github.com/KhronosGroup/Vulkan-Headers/archive/refs/tags/$VKHEADERS_VERSION.tar.gz"
VKHEADERS_SHA256=8ee39bee575bdccc1ecacd0bdb26f181841de35d7409a3eca468c6b622daa2f1
MESON_VERSION=1.6.1            # build tool only (dav1d); nothing of it reaches the prefix

# The configure line of docs/video-design.md §9. One element per line so the stamp below can
# hash it. libdrm (hwcontext_drm: VAAPI -> DRM PRIME) is required, not probed: a prefix that
# silently lacks it would differ between machines.
CONFIGURE_FLAGS=(
  --pkg-config-flags=--static
  --enable-static --disable-shared --enable-pic
  --disable-programs --disable-doc --disable-network --disable-autodetect
  --disable-everything
  --enable-avcodec --enable-avutil --enable-swscale
  --enable-avformat --enable-swresample
  --enable-vulkan --enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-vaapi --enable-libdrm
  --enable-libdav1d
  --enable-decoder=h264,hevc,av1,libdav1d
  --enable-parser=h264,hevc,av1
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,av1_frame_split,av1_frame_merge,extract_extradata
  --enable-hwaccel=h264_vulkan,hevc_vulkan,av1_vulkan
  --enable-hwaccel=h264_nvdec,hevc_nvdec,av1_nvdec
  --enable-hwaccel=h264_vaapi,hevc_vaapi,av1_vaapi
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc
  --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi
  --enable-filter=scale
  --enable-protocol=file
)
for flag in "${CONFIGURE_FLAGS[@]}"; do
  case "$flag" in
    --enable-gpl|--enable-nonfree|--enable-version3)
      echo "build-ffmpeg: $flag is not allowed: the client ships an LGPL-2.1+ FFmpeg" >&2; exit 1 ;;
  esac
done

say() { echo "[build-ffmpeg] $*"; }
die() { echo "build-ffmpeg: $*" >&2; exit 1; }

for tool in cc make curl tar sha256sum python3 ninja pkg-config; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed"
done
pkg-config --exists libva libva-drm libdrm || die "the libva and libdrm development files are not installed"

# What this script would build: when the prefix's stamp says the same, there is nothing to do.
STAMP="$PREFIX/.decibell-ffmpeg-stamp"
WANT="$(printf '%s\n' "ffmpeg $FFMPEG_VERSION $FFMPEG_SHA256" "dav1d $DAV1D_VERSION $DAV1D_SHA256" \
  "nasm $NASM_VERSION $NASM_SHA256" "nv-codec-headers $NVHEADERS_VERSION $NVHEADERS_SHA256" \
  "vulkan-headers $VKHEADERS_VERSION $VKHEADERS_SHA256" "prefix $PREFIX" "${CONFIGURE_FLAGS[@]}")"
if [ "$FORCE" = 0 ] && [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$WANT" ] && [ -f "$PREFIX/lib/libavcodec.a" ]; then
  say "$PREFIX is current (FFmpeg $FFMPEG_VERSION); nothing to do"
  exit 0
fi
if [ "$FORCE" = 1 ]; then
  rm -rf "$BUILD/ffmpeg-$FFMPEG_VERSION" "$BUILD/dav1d-$DAV1D_VERSION" \
         "$BUILD/nv-codec-headers-${NVHEADERS_VERSION}" "$BUILD/Vulkan-Headers-${VKHEADERS_VERSION#v}"
  rm -f "$STAMP" "$PREFIX/lib/libdav1d.a" "$PREFIX/lib/pkgconfig/ffnvcodec.pc" "$PREFIX/include/vulkan/vulkan.h"
fi
rm -f "$STAMP"

mkdir -p "$BUILD/dl" "$PREFIX/bin" "$PREFIX/lib/pkgconfig" "$PREFIX/include"
export PATH="$PREFIX/bin:$PATH"
# Only the prefix's own .pc files and the system's (libva, libdrm): never another FFmpeg.
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"

# fetch <url> <sha256>: downloads into $BUILD/dl (once) and prints the file's path. A file whose
# hash is wrong is deleted and the build stops.
fetch() {
  local url="$1" sha="$2" file="$BUILD/dl/${1##*/}"
  if [ ! -f "$file" ]; then
    say "fetching $url" >&2
    curl -fSL --retry 3 -o "$file.part" "$url" && mv "$file.part" "$file"
  fi
  if ! echo "$sha  $file" | sha256sum --check --status; then
    local got; got="$(sha256sum "$file" | cut -d' ' -f1)"
    rm -f "$file"
    die "${url##*/}: SHA-256 is $got, expected $sha (the file was deleted)"
  fi
  echo "$file"
}

# unpack <archive> <directory it creates>: into $BUILD, once.
unpack() {
  [ -d "$BUILD/$2" ] || tar -xf "$1" -C "$BUILD"
  [ -d "$BUILD/$2" ] || die "$1 did not unpack to $2"
}

if ! command -v nasm >/dev/null 2>&1; then
  say "building nasm $NASM_VERSION"
  unpack "$(fetch "$NASM_URL" "$NASM_SHA256")" "nasm-$NASM_VERSION"
  (cd "$BUILD/nasm-$NASM_VERSION" && ./configure --prefix="$PREFIX" >/dev/null && make -j"$JOBS" >/dev/null \
    && install -m755 nasm ndisasm "$PREFIX/bin/")
fi

# Vulkan headers (header-only; FFmpeg dlopens libvulkan.so.1 at run time, nothing links it).
if [ ! -f "$PREFIX/include/vulkan/vulkan.h" ]; then
  say "installing Vulkan-Headers $VKHEADERS_VERSION"
  unpack "$(fetch "$VKHEADERS_URL" "$VKHEADERS_SHA256")" "Vulkan-Headers-${VKHEADERS_VERSION#v}"
  rm -rf "$PREFIX/include/vulkan" "$PREFIX/include/vk_video"
  cp -r "$BUILD/Vulkan-Headers-${VKHEADERS_VERSION#v}/include/vulkan" \
        "$BUILD/Vulkan-Headers-${VKHEADERS_VERSION#v}/include/vk_video" "$PREFIX/include/"
fi

# nv-codec-headers -> ffnvcodec.pc (NVENC, NVDEC).
if [ ! -f "$PREFIX/lib/pkgconfig/ffnvcodec.pc" ]; then
  say "installing nv-codec-headers $NVHEADERS_VERSION"
  unpack "$(fetch "$NVHEADERS_URL" "$NVHEADERS_SHA256")" "nv-codec-headers-$NVHEADERS_VERSION"
  make -s -C "$BUILD/nv-codec-headers-$NVHEADERS_VERSION" PREFIX="$PREFIX" install
fi

# dav1d, static: the AV1 software decoder (FFmpeg's own av1 decoder is hwaccel-only).
if [ ! -f "$PREFIX/lib/libdav1d.a" ]; then
  say "building dav1d $DAV1D_VERSION"
  if ! command -v meson >/dev/null 2>&1; then
    [ -x "$BUILD/venv/bin/meson" ] || { python3 -m venv "$BUILD/venv" && "$BUILD/venv/bin/pip" -q install "meson==$MESON_VERSION"; }
    export PATH="$BUILD/venv/bin:$PATH"
  fi
  unpack "$(fetch "$DAV1D_URL" "$DAV1D_SHA256")" "dav1d-$DAV1D_VERSION"
  (cd "$BUILD/dav1d-$DAV1D_VERSION" && rm -rf build && meson setup build --prefix="$PREFIX" --libdir=lib \
      --default-library=static --buildtype=release -Denable_tools=false -Denable_tests=false \
      -Db_pie=true -Db_staticpic=true >/dev/null \
    && ninja -C build -j"$JOBS" >/dev/null && ninja -C build install >/dev/null)
fi

say "building FFmpeg $FFMPEG_VERSION (-j$JOBS)"
unpack "$(fetch "$FFMPEG_URL" "$FFMPEG_SHA256")" "ffmpeg-$FFMPEG_VERSION"
cd "$BUILD/ffmpeg-$FFMPEG_VERSION"
./configure --prefix="$PREFIX" --extra-cflags="-I$PREFIX/include" --extra-ldflags="-L$PREFIX/lib" \
  "${CONFIGURE_FLAGS[@]}"

# The licence is what configure decided, not what we meant to ask for.
grep -q '^#define CONFIG_GPL 0$' config.h || die "the configured FFmpeg is GPL"
grep -q '^#define CONFIG_NONFREE 0$' config.h || die "the configured FFmpeg is nonfree"
grep -q '^#define CONFIG_VERSION3 0$' config.h || die "the configured FFmpeg is (L)GPL version 3"
grep -q '^#define FFMPEG_LICENSE "LGPL version 2.1 or later"$' config.h \
  || die "unexpected licence: $(grep FFMPEG_LICENSE config.h)"
# And it has what both halves of the client open by name.
for component in H264_DECODER HEVC_DECODER AV1_DECODER LIBDAV1D_DECODER \
    H264_VULKAN_HWACCEL HEVC_VULKAN_HWACCEL AV1_VULKAN_HWACCEL \
    H264_NVENC_ENCODER HEVC_NVENC_ENCODER AV1_NVENC_ENCODER \
    H264_VAAPI_ENCODER HEVC_VAAPI_ENCODER AV1_VAAPI_ENCODER SCALE_FILTER; do
  grep -q "^#define CONFIG_$component 1$" config_components.h || die "configure left out $component"
done

make -j"$JOBS"
make install
printf '%s\n' "$WANT" > "$STAMP"
say "static FFmpeg $FFMPEG_VERSION (LGPL-2.1+) installed to $PREFIX"
