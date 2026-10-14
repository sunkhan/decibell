# Central server e2e harness

Standalone build + black-box tests for `src/server/`, against a real PostgreSQL, with no system
libpqxx / boost / jwt-cpp / Postgres needed. Everything is fetched into a scratch dir:

- the same header-only deps as the community harness;
- a Python 3.12 venv (made by `uv`) with `pgserver`, which bundles the PostgreSQL 16 server and libpq,
  plus `psycopg` and `protobuf`;
- libpqxx 7.10.1 (the project targets 7.x), built statically against that libpq.

```sh
D=/tmp/decibell-central-deps          # its own dir: don't share the community harness's pb
./setup_deps.sh "$D" ../../..         # fetch deps, build libpqxx, regen C++ + Python protobuf
./build.sh "$D" ../../.. "$D/central_server"
DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/central_server" DECIBELL_E2E_RUN="$D/run" \
  "$D/pgvenv/bin/python" -I e2e.py
```

`e2e.py` starts PostgreSQL (data in `$RUN/pg`), recreates the `central_e2e` database, and runs central
on 8080 (hard-coded in `main.cpp`) with a self-signed cert generated into the run dir. It drives it as
clients (register, login, friends, DMs, presence, avatars, calls, E2EE keys) and as a community server
(heartbeat, membership sync with the shared secret).

**Migration check.** Set `DECIBELL_E2E_SERVER_OLD` to a central binary from before a schema migration
(built from that commit with the same `build.sh`). The check seeds a fresh database with it, starts the
new binary on the same database, and verifies that every row survived, that the migration logged its
counts, and that a second start changes nothing. For migration 1 (users by uid) that is `main` at
2081fb0.

Requires `uv`, `protoc`, `g++`, `cmake`, `openssl` and `pkg-config` with protobuf.
