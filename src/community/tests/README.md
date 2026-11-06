# Community server e2e harness

Standalone build + black-box tests for `src/community/` — no CMake, no system
boost/jwt-cpp/nlohmann needed (headers are fetched into a scratch dir).

```sh
D=/tmp/decibell-deps            # anywhere writable
./setup_deps.sh "$D" ../../..   # fetch headers, regen C++ + Python protobuf
./build.sh "$D" ../../.. "$D/community_server"
DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/community_server" DECIBELL_E2E_RUN="$D/run" python3 e2e.py
```

**Migration check.** Set `DECIBELL_E2E_SERVER_OLD` to a community binary from before a schema migration
(`./build.sh "$D" <that checkout> "$D/community_server_old"`). The run then starts by filling a fresh database
with it and migrating with the new binary. For v15 (users by uid) that is any build before 2026-10-15.

`e2e.py` starts its own server on 8082–8085 (and short-lived extra instances
with `DECIBELL_AUTH_TIMEOUT_SECONDS` / `DECIBELL_IDLE_TIMEOUT_SECONDS` /
`DECIBELL_RETENTION_INTERVAL_SECONDS` for the deadline and sweep checks) (self-signed cert generated into
the run dir), mints Ed25519 JWTs with a test key, and drives it as clients over TLS, UDP and the
attachment HTTPS listener: auth and memberships, channels and messages (history paging, edits, replies,
nonces, rejections, retention, wipes), roles and permissions v3, roster paging and deltas, moderation
and the audit log, invites (personal ones too), search, the UDP relay with its bind challenge,
MLS delivery and channel key escrow, uid identity and renames, device revocations, request pacing, and
the deadlines. Each fix batch in `docs/reviews/2026-08-21-community-server-review.md` and each feature
in the running log `docs/reviews/2026-08-23-community-server-review.md` added its checks (567 at the
2026-10-17 review). It builds against the system `-lsqlite3`, not the pinned amalgamation production
links (`cmake/DecibellSQLite.cmake`); the system SQLite must be ≥ 3.43 with FTS5.
Requires `python3` with `protobuf`, and `protoc`, `g++`, `openssl`, `sqlite3`.

**Sanitizers.** The community server has threads (the search worker, the central-sync worker), so a
change that touches them is worth a run under AddressSanitizer + UBSan. protobuf moves its message globals
under ASan and the system libprotobuf wasn't built that way, so the flag below is required (without it the
binary crashes at startup), and container-overflow is a known false positive with an uninstrumented
libprotobuf:

```sh
g++ -std=c++20 -O1 -g -fsanitize=address,undefined -fno-omit-frame-pointer \
  -DPROTOBUF_MESSAGE_GLOBALS_TEMPORARY_OPTOUT -w \
  ../main.cpp ../db.cpp ../attachment_http.cpp ../search.cpp "$D/pb/messages.pb.cc" \
  -I"$D/pb" -I"$D/include" -I"$D/boost/usr/include" $(pkg-config --cflags --libs protobuf) \
  -lssl -lcrypto -lsqlite3 -lpthread -o "$D/community_server_asan"
ASAN_OPTIONS=detect_container_overflow=0 DECIBELL_E2E_SERVER="$D/community_server_asan" ...  # as above
grep -n "ERROR: AddressSanitizer\|runtime error" "$D/run"/*.log
```

The suite uses fixed ports (8082–8085) and heartbeats a central at 127.0.0.1:8080, so don't run it next to
the central suite or a dev server.
