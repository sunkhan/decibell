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
clients (register, login, friends, DMs, presence subscriptions, avatars, calls, E2EE keys, a login storm)
and as a community server
(heartbeat, membership sync with the shared secret).

**Migration check.** Set `DECIBELL_E2E_SERVER_OLD` to a central binary from before a schema migration
(built from that commit with the same `build.sh`). The check seeds a fresh database with it, starts the
new binary on the same database, and verifies that every row survived, that the migration logged its
counts, and that a second start changes nothing. For migration 1 (users by uid) that is `main` at
2081fb0.

**Sanitizers.** Central runs on several threads (`docs/superpowers/specs/2026-10-15-central-scaling-
design.md`), so a change to `src/server` also runs the suite under ThreadSanitizer and under
AddressSanitizer + UBSan; both must report nothing in `$RUN/central.log`. Central shuts down cleanly on
SIGTERM, so LeakSanitizer reports leaks at exit too.

```sh
EXTRA_CXXFLAGS="-fsanitize=thread -g -O1" ./build.sh "$D" ../../.. "$D/central_server_tsan"
DECIBELL_E2E_SERVER="$D/central_server_tsan" ...  # as above
# protobuf moves its message globals under ASan; the system libprotobuf wasn't built that way
EXTRA_CXXFLAGS="-fsanitize=address,undefined -fno-omit-frame-pointer -g -DPROTOBUF_MESSAGE_GLOBALS_TEMPORARY_OPTOUT" \
  ./build.sh "$D" ../../.. "$D/central_server_asan"
# container-overflow is a known false positive with an uninstrumented libprotobuf
ASAN_OPTIONS=detect_leaks=1:detect_container_overflow=0 DECIBELL_E2E_SERVER="$D/central_server_asan" ...
grep -n "WARNING: ThreadSanitizer\|ERROR: AddressSanitizer\|runtime error\|SUMMARY" "$D/run/central.log"
```

**Addresses.** Central limits connections, sign-ins and sign-ups per IP address
(`docs/superpowers/specs/2026-10-15-central-hardening-design.md`), so every test connection binds a fresh
loopback source address (`127.0.x.y`); the limit checks use fixed ones. The connection-cap and
out-of-descriptors checks start their own central instances after the main suite.

## Load test

`load.py` measures central the way production runs it: central and its PostgreSQL pinned to a CPU set
(default: one thread of each of six performance cores, `--server-cpus 0,2,4,6,8,10`, with 6 io and 3 CPU
threads), accounts seeded straight into the database, and client worker processes on the other CPUs
(`--worker-cpus`, `--workers`) holding real TLS connections from their own loopback ranges. Clients sign
in with resume tokens minted with central's key, as after a restart.

```sh
DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/central_server" DECIBELL_E2E_RUN="$D/load" \
  "$D/pgvenv/bin/python" -I load.py --steps 5000,10000,20000,30000,40000 --rates 250,500,1000,2000,4000
```

Phases: password sign-ins (bcrypt throughput); a ramp of connected clients, each subscribed to 50 others
and pinging (central memory and CPU, presence and DM latency at each step); traffic at full population
(DMs at rising rates, auto-away flips, re-subscriptions; delivery latency of every DM); and a restart of
central with everyone connected (time until all have resumed). The report lands in
`$RUN/load-report.md` and `.json`. **Put PostgreSQL on a real disk** (`--pg-dir`, or a `$RUN` that isn't
tmpfs; the script refuses tmpfs): every DM commits and waits for the disk to flush, and that wait, not
CPU, sets the DM rate. On tmpfs the first runs reported ~15,000 DMs/s; on an NVMe it was ~1,500. It needs `cryptography` in the venv (`setup_deps.sh` installs it) and a
few GB of memory for 40,000 clients.

Requires `uv`, `protoc`, `g++`, `cmake`, `openssl` and `pkg-config` with protobuf.
