# Community server e2e harness

Standalone build + black-box tests for `src/community/` — no CMake, no system
boost/jwt-cpp/nlohmann needed (headers are fetched into a scratch dir).

```sh
D=/tmp/decibell-deps            # anywhere writable
./setup_deps.sh "$D" ../../..   # fetch headers, regen C++ + Python protobuf
./build.sh "$D" ../../.. "$D/community_server"
DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/community_server" DECIBELL_E2E_RUN="$D/run" python3 e2e.py
```

`e2e.py` starts its own server on 8082–8085 (and short-lived extra instances
with `DECIBELL_AUTH_TIMEOUT_SECONDS` / `DECIBELL_IDLE_TIMEOUT_SECONDS` /
`DECIBELL_RETENTION_INTERVAL_SECONDS` for the deadline and sweep checks) (self-signed cert generated into
the run dir), mints HS256 JWTs with the test secret, and exercises the
2026-08-21 fix batches 13 + 14 (see `docs/reviews/2026-08-21-community-server-review.md`).
Requires `python3` with `protobuf`, and `protoc`, `g++`, `openssl`, `sqlite3`.
