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
the run dir), mints HS256 JWTs with the test secret, and exercises the
2026-08-21 fix batches 13 + 14 permissions v2 (batch 15), roster deltas (batch 16) and
server management + moderation (batch 17), relay / keep-alive / FTS (batch 18) and
Theme A — Ed25519 JWTs / uid bans (batch 19) (see `docs/reviews/2026-08-21-community-server-review.md`).
Requires `python3` with `protobuf`, and `protoc`, `g++`, `openssl`, `sqlite3`.
