#!/usr/bin/env bash
# Builds the GPUI client's FFmpeg into gpui-client/vendor/ffmpeg: a static, LGPL prefix that the
# whole client links once, for decode (crates/decibell-video: H.264 / HEVC / AV1 through Vulkan
# Video, NVDEC and VAAPI hwaccels, FFmpeg's software H.264 / HEVC, dav1d for AV1) and for encode
# (crates/decibell-core: NVENC, VAAPI with scale_vaapi for the GPU conversion, and SVT-AV1, the
# software encoder of a machine with no working hardware one). gpui-client/.cargo/config.toml points
# pkg-config at the prefix. Why static, why this version: gpui-client/docs/video-design.md §9; what
# the encoders need: crates/decibell-core/tools/ffmpeg-encoders.md.
#
#   tools/build-ffmpeg.sh              # builds what is missing; a no-op when the prefix is current
#   tools/build-ffmpeg.sh --force      # rebuilds everything
#   tools/build-ffmpeg.sh --stage vendor/ffmpeg.new   # build beside the prefix, then move it in place
#   JOBS=8 FFMPEG_PREFIX=... FFMPEG_STAGE=... FFMPEG_BUILD_DIR=... tools/build-ffmpeg.sh
#
# --prefix <dir> (FFMPEG_PREFIX; default vendor/ffmpeg) is where the prefix is used from: the path
# its pkg-config files name. --stage <dir> (FFMPEG_STAGE) builds into another directory, which must
# not exist yet, and writes the pkg-config files for --prefix: `mv <prefix> <old> && mv <stage>
# <prefix>` then swaps it in while builds that link the old prefix keep running until the moment
# of the swap. Without --stage the prefix is built in place.
#
# No sudo, nothing installed system-wide. Needs from the system: a C / C++ toolchain, make, cmake, curl,
# tar, sha256sum, python3 (for meson, in a venv), ninja, pkg-config and the libva + libdrm headers
# (VAAPI). Fetched and built here, each checked against a pinned SHA-256: nasm, the Vulkan
# headers, nv-codec-headers, dav1d, SVT-AV1, FFmpeg.
#
# LGPL only: the script refuses --enable-gpl / --enable-nonfree and fails if the configured tree
# is not LGPL (dav1d is BSD-2-Clause; SVT-AV1 is BSD-3-Clause-Clear with the Alliance for Open
# Media Patent License 1.0). Their licence files are copied to <prefix>/share/licenses.
set -euo pipefail

HERE="$(cd "$(dirname "$0")" && pwd)"
ROOT="$(cd "$HERE/.." && pwd)"                                  # gpui-client/
PREFIX="${FFMPEG_PREFIX:-$ROOT/vendor/ffmpeg}"
STAGE="${FFMPEG_STAGE:-}"
BUILD="${FFMPEG_BUILD_DIR:-$ROOT/vendor/ffmpeg-build}"
JOBS="${JOBS:-$(nproc)}"
FORCE=0
while [ $# -gt 0 ]; do
  case "$1" in
    --force) FORCE=1 ;;
    --prefix) [ $# -ge 2 ] || { echo "build-ffmpeg: --prefix needs a directory" >&2; exit 2; }; PREFIX="$2"; shift ;;
    --stage) [ $# -ge 2 ] || { echo "build-ffmpeg: --stage needs a directory" >&2; exit 2; }; STAGE="$2"; shift ;;
    -h|--help) sed -n '2,28p' "$0"; exit 0 ;;
    *) echo "build-ffmpeg: unknown argument $1" >&2; exit 2 ;;
  esac
  shift
done
# Absolute paths, symlinks resolved: they end up in the pkg-config files, and a worktree whose
# vendor/ffmpeg links the main tree's prefix then finds that prefix's stamp.
PREFIX="$(realpath -m "$PREFIX")"
STAGE="$(realpath -m "${STAGE:-$PREFIX}")"
BUILD="$(realpath -m "$BUILD")"

# name, version, URL, SHA-256 of the archive. Changing a version means changing its hash.
FFMPEG_VERSION=8.1.3      # ffmpeg-next / ffmpeg-sys-next 8.1 bind FFmpeg 8.0 and 8.1
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
FFMPEG_SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
DAV1D_VERSION=1.5.3
DAV1D_URL="https://code.videolan.org/videolan/dav1d/-/archive/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.bz2"
DAV1D_SHA256=e099f53253f6c247580c554d53a13f1040638f2066edc3c740e4c2f15174ce22
# The software encoder. 4.0+ forces a key frame on request in low-delay mode (earlier versions
# did not, FFmpeg's libsvtav1.c notes); FFmpeg 8.1's libsvtav1.c handles the 4.x API.
SVTAV1_VERSION=v4.2.0
SVTAV1_URL="https://gitlab.com/AOMediaCodec/SVT-AV1/-/archive/$SVTAV1_VERSION/SVT-AV1-$SVTAV1_VERSION.tar.gz"
SVTAV1_SHA256=c7b13c4a84bd3751aa35fcc72be13e6875467e7c2216879251a486e5b1e4e740
NASM_VERSION=2.16.03      # x86 SIMD of the software decoders and of SVT-AV1; several times slower without
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
# silently lacks it would differ between machines. libavfilter carries scale_vaapi, the VAAPI
# encoders' GPU conversion (encoder_vaapi.rs: buffer -> scale_vaapi -> buffersink).
CONFIGURE_FLAGS=(
  --pkg-config-flags=--static
  --enable-static --disable-shared --enable-pic
  --disable-programs --disable-doc --disable-network --disable-autodetect
  --disable-everything
  --enable-avcodec --enable-avutil --enable-swscale
  --enable-avformat --enable-swresample --enable-avfilter
  --enable-vulkan --enable-ffnvcodec --enable-nvenc --enable-nvdec --enable-vaapi --enable-libdrm
  --enable-libdav1d --enable-libsvtav1
  --enable-decoder=h264,hevc,av1,libdav1d
  --enable-parser=h264,hevc,av1
  --enable-bsf=h264_mp4toannexb,hevc_mp4toannexb,av1_frame_split,av1_frame_merge,extract_extradata
  --enable-hwaccel=h264_vulkan,hevc_vulkan,av1_vulkan
  --enable-hwaccel=h264_nvdec,hevc_nvdec,av1_nvdec
  --enable-hwaccel=h264_vaapi,hevc_vaapi,av1_vaapi
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc
  --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi
  --enable-encoder=libsvtav1
  --enable-filter=scale,scale_vaapi
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

for tool in cc c++ make cmake curl tar sha256sum python3 ninja pkg-config; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed"
done
pkg-config --exists libva libva-drm libdrm || die "the libva and libdrm development files are not installed"

# What this script would build: when the prefix's stamp says the same, there is nothing to do.
STAMP="$STAGE/.decibell-ffmpeg-stamp"
WANT="$(printf '%s\n' "ffmpeg $FFMPEG_VERSION $FFMPEG_SHA256" "dav1d $DAV1D_VERSION $DAV1D_SHA256" \
  "svt-av1 $SVTAV1_VERSION $SVTAV1_SHA256" \
  "nasm $NASM_VERSION $NASM_SHA256" "nv-codec-headers $NVHEADERS_VERSION $NVHEADERS_SHA256" \
  "vulkan-headers $VKHEADERS_VERSION $VKHEADERS_SHA256" "prefix $PREFIX" "${CONFIGURE_FLAGS[@]}")"
if [ "$FORCE" = 0 ] && [ -f "$STAMP" ] && [ "$(cat "$STAMP")" = "$WANT" ] && [ -f "$STAGE/lib/libavcodec.a" ]; then
  say "$STAGE is current (FFmpeg $FFMPEG_VERSION for $PREFIX); nothing to do"
  exit 0
fi
if [ "$STAGE" != "$PREFIX" ] && [ -e "$STAGE" ]; then
  # A stage's pkg-config files already name the prefix: building on top of it would link the
  # prefix's libraries, not the stage's.
  die "$STAGE exists and is not a current build for $PREFIX: remove it, or stage elsewhere"
fi
if [ "$FORCE" = 1 ]; then
  rm -rf "$BUILD/ffmpeg-$FFMPEG_VERSION" "$BUILD/dav1d-$DAV1D_VERSION" "$BUILD/SVT-AV1-$SVTAV1_VERSION" \
         "$BUILD/nv-codec-headers-${NVHEADERS_VERSION}" "$BUILD/Vulkan-Headers-${VKHEADERS_VERSION#v}"
  rm -f "$STAMP" "$STAGE/lib/libdav1d.a" "$STAGE/lib/libSvtAv1Enc.a" "$STAGE/lib/pkgconfig/ffnvcodec.pc" \
        "$STAGE/include/vulkan/vulkan.h"
fi
rm -f "$STAMP"

mkdir -p "$BUILD/dl" "$STAGE/bin" "$STAGE/lib/pkgconfig" "$STAGE/include" "$STAGE/share/licenses"
export PATH="$STAGE/bin:$PATH"
# Only the stage's own .pc files and the system's (libva, libdrm): never another FFmpeg or SVT-AV1.
export PKG_CONFIG_PATH="$STAGE/lib/pkgconfig"

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
  (cd "$BUILD/nasm-$NASM_VERSION" && ./configure --prefix="$STAGE" >/dev/null && make -j"$JOBS" >/dev/null \
    && install -m755 nasm ndisasm "$STAGE/bin/")
fi

# Vulkan headers (header-only; FFmpeg dlopens libvulkan.so.1 at run time, nothing links it).
if [ ! -f "$STAGE/include/vulkan/vulkan.h" ]; then
  say "installing Vulkan-Headers $VKHEADERS_VERSION"
  unpack "$(fetch "$VKHEADERS_URL" "$VKHEADERS_SHA256")" "Vulkan-Headers-${VKHEADERS_VERSION#v}"
  rm -rf "$STAGE/include/vulkan" "$STAGE/include/vk_video"
  cp -r "$BUILD/Vulkan-Headers-${VKHEADERS_VERSION#v}/include/vulkan" \
        "$BUILD/Vulkan-Headers-${VKHEADERS_VERSION#v}/include/vk_video" "$STAGE/include/"
fi

# nv-codec-headers -> ffnvcodec.pc (NVENC, NVDEC).
if [ ! -f "$STAGE/lib/pkgconfig/ffnvcodec.pc" ]; then
  say "installing nv-codec-headers $NVHEADERS_VERSION"
  unpack "$(fetch "$NVHEADERS_URL" "$NVHEADERS_SHA256")" "nv-codec-headers-$NVHEADERS_VERSION"
  make -s -C "$BUILD/nv-codec-headers-$NVHEADERS_VERSION" PREFIX="$STAGE" install
fi

# dav1d, static: the AV1 software decoder (FFmpeg's own av1 decoder is hwaccel-only).
if [ ! -f "$STAGE/lib/libdav1d.a" ]; then
  say "building dav1d $DAV1D_VERSION"
  if ! command -v meson >/dev/null 2>&1; then
    [ -x "$BUILD/venv/bin/meson" ] || { python3 -m venv "$BUILD/venv" && "$BUILD/venv/bin/pip" -q install "meson==$MESON_VERSION"; }
    export PATH="$BUILD/venv/bin:$PATH"
  fi
  unpack "$(fetch "$DAV1D_URL" "$DAV1D_SHA256")" "dav1d-$DAV1D_VERSION"
  (cd "$BUILD/dav1d-$DAV1D_VERSION" && rm -rf build && meson setup build --prefix="$STAGE" --libdir=lib \
      --default-library=static --buildtype=release -Denable_tools=false -Denable_tests=false \
      -Db_pie=true -Db_staticpic=true >/dev/null \
    && ninja -C build -j"$JOBS" >/dev/null && ninja -C build install >/dev/null)
  mkdir -p "$STAGE/share/licenses/dav1d"
  cp "$BUILD/dav1d-$DAV1D_VERSION/COPYING" "$STAGE/share/licenses/dav1d/"
fi

# SVT-AV1, static, encoder only: the software encoder. LTO off: a static archive of LTO objects
# only links with the compiler that made it. PIC: it ends up in a PIE. nasm must be on PATH (it
# is: $STAGE/bin or the system's); without it SVT-AV1 builds C-only and is not real-time.
if [ ! -f "$STAGE/lib/libSvtAv1Enc.a" ]; then
  say "building SVT-AV1 $SVTAV1_VERSION"
  unpack "$(fetch "$SVTAV1_URL" "$SVTAV1_SHA256")" "SVT-AV1-$SVTAV1_VERSION"
  (cd "$BUILD/SVT-AV1-$SVTAV1_VERSION" && rm -rf build-static && cmake -S . -B build-static -G Ninja \
      -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DBUILD_APPS=OFF -DBUILD_TESTING=OFF \
      -DSVT_AV1_LTO=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_INSTALL_PREFIX="$STAGE" -DCMAKE_INSTALL_LIBDIR=lib >/dev/null \
    && ninja -C build-static -j"$JOBS" >/dev/null && ninja -C build-static install >/dev/null)
  mkdir -p "$STAGE/share/licenses/SVT-AV1"
  cp "$BUILD/SVT-AV1-$SVTAV1_VERSION/LICENSE.md" "$BUILD/SVT-AV1-$SVTAV1_VERSION/PATENTS.md" \
     "$STAGE/share/licenses/SVT-AV1/"
fi

say "building FFmpeg $FFMPEG_VERSION (-j$JOBS)"
unpack "$(fetch "$FFMPEG_URL" "$FFMPEG_SHA256")" "ffmpeg-$FFMPEG_VERSION"
cd "$BUILD/ffmpeg-$FFMPEG_VERSION"
./configure --prefix="$STAGE" --extra-cflags="-I$STAGE/include" --extra-ldflags="-L$STAGE/lib" \
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
    H264_VAAPI_ENCODER HEVC_VAAPI_ENCODER AV1_VAAPI_ENCODER LIBSVTAV1_ENCODER \
    SCALE_FILTER SCALE_VAAPI_FILTER; do
  grep -q "^#define CONFIG_$component 1$" config_components.h || die "configure left out $component"
done

make -j"$JOBS"
make install
mkdir -p "$STAGE/share/licenses/ffmpeg"
cp COPYING.LGPLv2.1 "$STAGE/share/licenses/ffmpeg/"

# A stage: point every text file that names it (pkg-config files, SVT-AV1's CMake config) at the
# prefix it will be moved to.
if [ "$STAGE" != "$PREFIX" ]; then
  grep -rlIF -- "$STAGE" "$STAGE" | while IFS= read -r file; do
    sed -i "s|$STAGE|$PREFIX|g" "$file"
  done
  if grep -rqIF -- "$STAGE" "$STAGE/lib" "$STAGE/include"; then
    die "$STAGE still names itself after the rewrite"
  fi
fi
printf '%s\n' "$WANT" > "$STAMP"
if [ "$STAGE" != "$PREFIX" ]; then
  say "static FFmpeg $FFMPEG_VERSION (LGPL-2.1+) built in $STAGE for $PREFIX: move it there to use it"
else
  say "static FFmpeg $FFMPEG_VERSION (LGPL-2.1+) installed to $PREFIX"
fi
