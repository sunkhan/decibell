#!/usr/bin/env bash
# Builds a development FFmpeg prefix for the core's encoder work: everything the encode paths open
# (NVENC, VAAPI + the VAAPI upload / scale filters, SVT-AV1 through libsvtav1) and the software
# decoders the encoder harness checks its output with (H.264, HEVC, dav1d for AV1).
#
#   crates/decibell-core/tools/build-ffmpeg-encoders.sh
#   cd crates/decibell-core && PKG_CONFIG_PATH=~/.cache/decibell-dev/ffmpeg-enc/prefix/lib/pkgconfig cargo test --lib
#
# This is NOT a prefix any client ships: electron-client/native/scripts/build-ffmpeg-linux.sh builds the
# addon's and gpui-client/tools/build-ffmpeg.sh the GPUI client's. tools/ffmpeg-encoders.md lists
# what each of those needs from here (the lines to fold in), with the same versions and hashes.
#
# No sudo, nothing installed system-wide. Needs: a C / C++ toolchain, make, cmake, ninja, curl,
# tar, sha256sum, python3 (meson in a venv, for dav1d), pkg-config, the libva + libdrm headers.
# Every archive is checked against a pinned SHA-256.
#
# Licences: FFmpeg stays LGPL-2.1+ (no --enable-gpl / --enable-nonfree; checked below). SVT-AV1 is
# BSD-3-Clause-Clear with the Alliance for Open Media Patent License 1.0; dav1d is BSD-2-Clause.
# Their licence files are copied to $PREFIX/share/licenses.
set -euo pipefail

ROOT="${DECIBELL_FFMPEG_ENC_DIR:-$HOME/.cache/decibell-dev/ffmpeg-enc}"
PREFIX="${FFMPEG_PREFIX:-$ROOT/prefix}"
BUILD="${FFMPEG_BUILD_DIR:-$ROOT/build}"
JOBS="${JOBS:-8}"

FFMPEG_VERSION=8.1.3      # as gpui-client/tools/build-ffmpeg.sh (ffmpeg-next 8.1 binds 8.0 and 8.1)
FFMPEG_URL="https://ffmpeg.org/releases/ffmpeg-$FFMPEG_VERSION.tar.xz"
FFMPEG_SHA256=7138d28c96d9d3e3af4ee3d8cad72741f8ffb40da90c1112235dea3ecd3178a3
SVTAV1_VERSION=v4.2.0     # FFmpeg 8.1's libsvtav1.c handles the 4.x API (SVT_AV1_CHECK_VERSION(4, 0, 0))
SVTAV1_URL="https://gitlab.com/AOMediaCodec/SVT-AV1/-/archive/$SVTAV1_VERSION/SVT-AV1-$SVTAV1_VERSION.tar.gz"
SVTAV1_SHA256=c7b13c4a84bd3751aa35fcc72be13e6875467e7c2216879251a486e5b1e4e740
DAV1D_VERSION=1.5.3
DAV1D_URL="https://code.videolan.org/videolan/dav1d/-/archive/$DAV1D_VERSION/dav1d-$DAV1D_VERSION.tar.bz2"
DAV1D_SHA256=e099f53253f6c247580c554d53a13f1040638f2066edc3c740e4c2f15174ce22
NASM_VERSION=2.16.03      # SVT-AV1's and FFmpeg's x86 SIMD: without it SVT-AV1 is not real-time
NASM_URL="https://www.nasm.us/pub/nasm/releasebuilds/$NASM_VERSION/nasm-$NASM_VERSION.tar.xz"
NASM_SHA256=1412a1c760bbd05db026b6c0d1657affd6631cd0a63cddb6f73cc6d4aa616148
NVHEADERS_VERSION=n13.0.19.0
NVHEADERS_URL="https://github.com/FFmpeg/nv-codec-headers/archive/refs/tags/$NVHEADERS_VERSION.tar.gz"
NVHEADERS_SHA256=86d15d1a7c0ac73a0eafdfc57bebfeba7da8264595bf531cf4d8db1c22940116
MESON_VERSION=1.6.1

CONFIGURE_FLAGS=(
  --pkg-config-flags=--static
  --enable-static --disable-shared --enable-pic
  --disable-programs --disable-doc --disable-network --disable-autodetect
  --disable-everything
  --enable-avcodec --enable-avutil --enable-swscale
  --enable-avformat --enable-swresample --enable-avfilter
  --enable-ffnvcodec --enable-nvenc --enable-vaapi --enable-libdrm
  --enable-libsvtav1 --enable-libdav1d
  --enable-encoder=h264_nvenc,hevc_nvenc,av1_nvenc
  --enable-encoder=h264_vaapi,hevc_vaapi,av1_vaapi
  --enable-encoder=libsvtav1
  --enable-decoder=h264,hevc,libdav1d
  --enable-parser=h264,hevc,av1
  --enable-filter=scale,hwupload,scale_vaapi
  --enable-protocol=file
)

say() { echo "[ffmpeg-encoders] $*"; }
die() { echo "ffmpeg-encoders: $*" >&2; exit 1; }

for tool in cc c++ make cmake ninja curl tar sha256sum python3 pkg-config; do
  command -v "$tool" >/dev/null 2>&1 || die "$tool is not installed"
done
pkg-config --exists libva libva-drm libdrm || die "the libva and libdrm development files are not installed"

mkdir -p "$BUILD/dl" "$PREFIX/bin" "$PREFIX/lib/pkgconfig" "$PREFIX/include" "$PREFIX/share/licenses"
export PATH="$PREFIX/bin:$PATH"
# The prefix's own .pc files and the system's (libva, libdrm): never another FFmpeg or SVT-AV1.
export PKG_CONFIG_PATH="$PREFIX/lib/pkgconfig"

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

unpack() {
  [ -d "$BUILD/$2" ] || tar -xf "$1" -C "$BUILD"
  [ -d "$BUILD/$2" ] || die "$1 did not unpack to $2"
}

if [ ! -x "$PREFIX/bin/nasm" ] && ! command -v nasm >/dev/null 2>&1; then
  say "building nasm $NASM_VERSION"
  unpack "$(fetch "$NASM_URL" "$NASM_SHA256")" "nasm-$NASM_VERSION"
  (cd "$BUILD/nasm-$NASM_VERSION" && ./configure --prefix="$PREFIX" >/dev/null && make -j"$JOBS" >/dev/null \
    && install -m755 nasm ndisasm "$PREFIX/bin/")
fi

if [ ! -f "$PREFIX/lib/pkgconfig/ffnvcodec.pc" ]; then
  say "installing nv-codec-headers $NVHEADERS_VERSION"
  unpack "$(fetch "$NVHEADERS_URL" "$NVHEADERS_SHA256")" "nv-codec-headers-$NVHEADERS_VERSION"
  make -s -C "$BUILD/nv-codec-headers-$NVHEADERS_VERSION" PREFIX="$PREFIX" install
fi

# SVT-AV1, static, encoder only. LTO off: a static archive of LTO objects only links with the
# compiler that made it. PIC: it ends up in a cdylib (the addon) or a PIE.
if [ ! -f "$PREFIX/lib/libSvtAv1Enc.a" ]; then
  say "building SVT-AV1 $SVTAV1_VERSION"
  unpack "$(fetch "$SVTAV1_URL" "$SVTAV1_SHA256")" "SVT-AV1-$SVTAV1_VERSION"
  (cd "$BUILD/SVT-AV1-$SVTAV1_VERSION" && rm -rf build-static && cmake -S . -B build-static -G Ninja \
      -DCMAKE_BUILD_TYPE=Release -DBUILD_SHARED_LIBS=OFF -DBUILD_APPS=OFF -DBUILD_TESTING=OFF \
      -DSVT_AV1_LTO=OFF -DCMAKE_POSITION_INDEPENDENT_CODE=ON \
      -DCMAKE_INSTALL_PREFIX="$PREFIX" -DCMAKE_INSTALL_LIBDIR=lib >/dev/null \
    && ninja -C build-static -j"$JOBS" >/dev/null && ninja -C build-static install >/dev/null)
  mkdir -p "$PREFIX/share/licenses/SVT-AV1"
  cp "$BUILD/SVT-AV1-$SVTAV1_VERSION/LICENSE.md" "$BUILD/SVT-AV1-$SVTAV1_VERSION/PATENTS.md" "$PREFIX/share/licenses/SVT-AV1/"
fi

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
  mkdir -p "$PREFIX/share/licenses/dav1d"
  cp "$BUILD/dav1d-$DAV1D_VERSION/COPYING" "$PREFIX/share/licenses/dav1d/"
fi

say "building FFmpeg $FFMPEG_VERSION (-j$JOBS)"
unpack "$(fetch "$FFMPEG_URL" "$FFMPEG_SHA256")" "ffmpeg-$FFMPEG_VERSION"
cd "$BUILD/ffmpeg-$FFMPEG_VERSION"
./configure --prefix="$PREFIX" --extra-cflags="-I$PREFIX/include" --extra-ldflags="-L$PREFIX/lib" \
  "${CONFIGURE_FLAGS[@]}"

grep -q '^#define CONFIG_GPL 0$' config.h || die "the configured FFmpeg is GPL"
grep -q '^#define CONFIG_NONFREE 0$' config.h || die "the configured FFmpeg is nonfree"
grep -q '^#define FFMPEG_LICENSE "LGPL version 2.1 or later"$' config.h \
  || die "unexpected licence: $(grep FFMPEG_LICENSE config.h)"
for component in H264_NVENC_ENCODER HEVC_NVENC_ENCODER AV1_NVENC_ENCODER \
    H264_VAAPI_ENCODER HEVC_VAAPI_ENCODER AV1_VAAPI_ENCODER LIBSVTAV1_ENCODER \
    H264_DECODER HEVC_DECODER LIBDAV1D_DECODER HWUPLOAD_FILTER SCALE_VAAPI_FILTER; do
  grep -q "^#define CONFIG_$component 1$" config_components.h || die "configure left out $component"
done

make -j"$JOBS"
make install
mkdir -p "$PREFIX/share/licenses/ffmpeg"
cp COPYING.LGPLv2.1 "$PREFIX/share/licenses/ffmpeg/"
say "static FFmpeg $FFMPEG_VERSION (LGPL-2.1+) with libsvtav1 installed to $PREFIX"
say "use it with PKG_CONFIG_PATH=$PREFIX/lib/pkgconfig"
