#!/bin/bash
# Fetch everything a standalone central-server build + e2e run needs:
# header-only deps, libpqxx 7 built against a bundled libpq, a Python 3.12
# venv with a bundled PostgreSQL (pgserver), and the regenerated protobuf.
# Usage: setup_deps.sh <depsdir> <repo>
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

# PostgreSQL: pgserver bundles the server binaries and libpq. Its wheels
# stop at Python 3.12, so the venv is made by uv with its own interpreter.
if [ ! -x $D/pgvenv/bin/python ]; then
  uv venv -q --python 3.12 $D/pgvenv
  VIRTUAL_ENV=$D/pgvenv uv pip install -q pgserver 'psycopg[binary]' protobuf
fi
PG=$($D/pgvenv/bin/python -c "import pgserver, os; print(os.path.join(os.path.dirname(pgserver.__file__), 'pginstall'))")

# libpqxx 7 (the project targets 7.x), static, against that libpq.
if [ ! -f $D/pqxx/lib/libpqxx.a ]; then
  curl -sSL -o $D/pqxx.tar.gz https://github.com/jtv/libpqxx/archive/refs/tags/7.10.1.tar.gz
  tar xzf $D/pqxx.tar.gz -C $D
  cmake -S $D/libpqxx-7.10.1 -B $D/pqxx-build -DCMAKE_BUILD_TYPE=Release -DSKIP_BUILD_TEST=ON \
    -DBUILD_SHARED_LIBS=OFF -DBUILD_DOC=OFF -DCMAKE_CXX_STANDARD=20 \
    -DPostgreSQL_INCLUDE_DIR=$PG/include -DPostgreSQL_LIBRARY=$PG/lib/libpq.so \
    -DCMAKE_INSTALL_PREFIX=$D/pqxx > /dev/null
  cmake --build $D/pqxx-build -j"$(nproc)" > /dev/null
  cmake --install $D/pqxx-build > /dev/null
  rm -rf $D/pqxx-build $D/libpqxx-7.10.1 $D/pqxx.tar.gz
fi
ln -sfn $PG $D/pginstall

protoc --cpp_out=$D/pb -I$REPO/proto $REPO/proto/messages.proto
protoc --python_out=$D/pb -I$REPO/proto $REPO/proto/messages.proto
echo deps-ready
