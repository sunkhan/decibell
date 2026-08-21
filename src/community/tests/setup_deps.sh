#!/bin/bash
# Fetch header-only deps + regen protobuf for a standalone community-server build.
set -e
D="$1"; REPO="$2"
mkdir -p $D/include/nlohmann $D/boost $D/pb
[ -f $D/include/nlohmann/json.hpp ] || curl -sSL -o $D/include/nlohmann/json.hpp https://raw.githubusercontent.com/nlohmann/json/v3.11.3/single_include/nlohmann/json.hpp
if [ ! -d $D/include/jwt-cpp ]; then
  curl -sSL -o $D/jwt.tar.gz https://github.com/Thalhammer/jwt-cpp/archive/refs/tags/v0.7.1.tar.gz
  tar xzf $D/jwt.tar.gz -C $D && cp -r $D/jwt-cpp-0.7.1/include/jwt-cpp $D/include/ && rm -rf $D/jwt-cpp-0.7.1 $D/jwt.tar.gz
fi
if [ ! -d $D/boost/usr/include/boost ]; then
  BOOST=$(curl -sSL https://geo.mirror.pkgbuild.com/extra/os/x86_64/ | grep -o 'boost-[0-9][^"]*x86_64.pkg.tar.zst' | grep -v libs | sort -u | tail -1)
  curl -sSL https://geo.mirror.pkgbuild.com/extra/os/x86_64/$BOOST | tar --zstd -x -C $D/boost usr/include
fi
protoc --cpp_out=$D/pb -I$REPO/proto $REPO/proto/messages.proto
protoc --python_out=$D/pb -I$REPO/proto $REPO/proto/messages.proto
echo deps-ready
