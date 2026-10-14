#!/bin/bash
# Usage: build.sh <depsdir> <repo> <out>
D="$1"; REPO="$2"; OUT="$3"
g++ -std=c++20 -O1 -w $REPO/src/server/main.cpp $REPO/src/server/auth_manager.cpp $D/pb/messages.pb.cc \
  -I$D/pb -I$D/include -I$D/boost/usr/include -I$D/pqxx/include -I$D/pginstall/include \
  $(pkg-config --cflags --libs protobuf) \
  $D/pqxx/lib/libpqxx.a -L$D/pginstall/lib -Wl,-rpath,$D/pginstall/lib -lpq \
  -lssl -lcrypto -lpthread -o "$OUT"
