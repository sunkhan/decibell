#!/usr/bin/env bash
# Builds the Qt client on Linux: tools/build-linux.sh [Release|Debug] [build dir]
#
# Needs CMake >= 3.24, Ninja, a C++20 compiler, Rust (rustup), Node 20, and Qt 6.12 with Shader Tools and Svg.
# Qt from download.qt.io without an account (as on Windows, tools/qt/fetch_qt.py):
#   uv run --no-project --python 3.12 --with py7zr --with truststore python -I tools/qt/fetch_qt.py \
#       --host linux_x64 --version 6.12.0 --arch linux_gcc_64 --dest ~/Qt --addons qtshadertools qtimageformats
#   QT_PREFIX=~/Qt/6.12.0/linux_gcc_64 qt-client/tools/build-linux.sh
# Not built on Linux yet (first run is the Linux half of the spike, qt-client/HANDOFF.md §4): watched streams
# (src/video is D3D11VA; VA-API / Vulkan Video comes then).
set -euo pipefail
cfg=${1:-Release}
build=${2:-$HOME/b/qt}
here=$(cd "$(dirname "$0")" && pwd)
: "${QT_PREFIX:?set QT_PREFIX to the Qt install, e.g. ~/Qt/6.12.0/linux_gcc_64}"
export QMAKE="$QT_PREFIX/bin/qmake" # cxx-qt-build finds Qt through qmake
node "$here/fetch-assets.mjs"
node "$here/gen-theme/gen-theme.mjs" --check
cmake -S "$here/.." -B "$build" -G Ninja -DCMAKE_BUILD_TYPE="$cfg" -DCMAKE_PREFIX_PATH="$QT_PREFIX"
cmake --build "$build"
echo "QT_CLIENT_BUILD_OK $build/decibell-qt"
