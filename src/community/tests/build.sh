#!/bin/bash
# Usage: build.sh <depsdir> <repo> <out>
D="$1"; REPO="$2"; OUT="$3"
g++ -std=c++20 -O1 -w $REPO/src/community/main.cpp $REPO/src/community/db.cpp $REPO/src/community/attachment_http.cpp $D/pb/messages.pb.cc -I$D/pb -I$D/include -I$D/boost/usr/include $(pkg-config --cflags --libs protobuf) -lssl -lcrypto -lsqlite3 -lpthread -o "$OUT"
