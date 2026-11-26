# Local test stack

PostgreSQL, a **central** server and **three community servers** on loopback, seeded with test accounts
and a realistic amount of content, so a real client (the Electron client, the GPUI client) can sign in,
be exercised and be measured **without the owner's account and without the live central**. Nothing here
contacts 178.104.131.247.

```sh
tools/dev/local-stack.sh setup      # build deps + servers (first run ~1 min, then seconds)
tools/dev/local-stack.sh up         # postgres, central, Daily Gaming, Rustaceans, Design Lab
tools/dev/local-stack.sh seed       # accounts, friends, DMs, communities, 10k-message #general (~30 s)
tools/dev/local-stack.sh smoke      # sign in as alice the way a client does; exit 0 = all good
eval "$(tools/dev/local-stack.sh client-env)"   # DECIBELL_CENTRAL=127.0.0.1:18080
# then start a client built so it honours DECIBELL_CENTRAL (below) and sign in as alice / decibell-test-1
tools/dev/local-stack.sh down
```

## Commands

| Command | What it does |
|---|---|
| `setup` | Runs the two harness scripts (`src/community/tests/setup_deps.sh`, `src/server/tests/setup_deps.sh` + their `build.sh`) into the dev dir, builds the bind shim, adds Pillow to the venv. Idempotent: deps are redone only when `proto/messages.proto` or a harness script changed; a server binary is rebuilt when any of its sources (`src/server/*`, `src/community/*.{cpp,hpp}`, `src/common/*.hpp`, the generated pb) is newer than it. Parallel compile jobs are capped at 8 (`DECIBELL_DEV_JOBS`) by putting a wrapper `nproc` first on `PATH` for the harness scripts. |
| `up` | Starts what isn't running: PostgreSQL (unix socket only), central, then each community, waiting until each has registered with central through its heartbeat. Refuses with a clear message when a port it needs is taken. |
| `seed` | Fills the stack (below). Idempotent: every step checks the databases first, so a second run changes nothing (~10 s). |
| `status` | What runs, pids, each community's id / member count / listing / last heartbeat as central sees it, whether a binary was rebuilt since start, the client env. |
| `logs <name>` | `tail -F` a log: `central`, `postgres`, `gaming`, `rust`, `design` (or `daily-gaming`, …). |
| `down` | Stops everything (SIGTERM, then SIGKILL after 10 s; `pg_ctl stop -m fast`). **Data persists**: `up` brings the same seeded state back. |
| `reset` | `down`, then wipes `run/` except `run/keys/`, so `up && seed` starts from scratch and gives the same accounts, uids, server ids and content. |
| `client-env` | Prints `export DECIBELL_CENTRAL=127.0.0.1:18080` and what else a client developer needs to know. |
| `smoke` | Runs `smoke.py` (below). |

`DECIBELL_DEV_DIR` moves the whole thing (default `~/.cache/decibell-dev`; not `/tmp`, a tmpfs wiped at
reboot). `DECIBELL_DEV_CENTRAL_PORT` moves central (default 18080).

## Addresses

| Piece | Address | Notes |
|---|---|---|
| central | `127.0.0.1:18080` TLS | the client's `DECIBELL_CENTRAL` |
| Daily Gaming | `127.0.0.11`: 8082 TLS, 8083 voice UDP, 8084 media UDP, 8085 attachment HTTPS | server id 1, owner alice, public |
| Rustaceans | `127.0.0.12`: same ports | server id 2, owner bob, unlisted |
| Design Lab | `127.0.0.13`: same ports | server id 3, owner carol, public |
| PostgreSQL 16 | unix socket `run/pg/.s.PGSQL.54329`, database `decibell`, user `postgres`, no TCP | `psql -h ~/.cache/decibell-dev/run/pg -p 54329 -U postgres decibell` (psql is in `deps/central/pginstall/bin`) |

**Why loopback addresses instead of port sets.** Both servers hard-code their ports (central 8080,
`src/server/main.cpp`; a community 8082 and the ports derived from it, `src/community/main.cpp`), bind the
IPv4 wildcard, and a community *advertises* 8082 to central (heartbeat, invites, memberships) and 8085 to
clients (`COMMUNITY_AUTH_RES.attachment_port`); clients derive the UDP ports as port + 1 / + 2. So three
communities can only differ by address. `bindshim.c`, preloaded into the server processes
(`LD_PRELOAD`), rewrites their wildcard `bind()` to the community's own loopback address
(`DECIBELL_DEV_BIND_IP`) and central's 8080 to 18080 on 127.0.0.1 (`DECIBELL_DEV_PORT_MAP`); nothing else
is touched, and the server code is unchanged. Linux routes all of 127.0.0.0/8 to `lo` without setup. A
`DECIBELL_PORT` / listen-address setting in the servers would make the shim unnecessary.

## Layout (`$DECIBELL_DEV_DIR`)

```
deps/community/        community harness deps (headers, boost, C++/Python pb)
deps/central/          central harness deps: pgvenv/ (uv venv, Python 3.12, pgserver's PostgreSQL 16,
                       psycopg, protobuf, cryptography, Pillow), pqxx/, pginstall -> PostgreSQL, pb/
bin/                   central_server, community_server, bindshim.so
cache/fixture-v1/      drawn pictures + thumbnails + ThumbHash, reused by every seed (survives reset)
run/                   all state; `reset` wipes it except keys/
  keys/                community_secret; central/{server.crt,server.key,jwt_ed25519.pem(.pub)};
                       gaming|rust|design/{server.crt,server.key}   (10-year self-signed certs)
  pg/data/             PostgreSQL cluster (fsync off: a dev stack, not a durability test)
  central/             central's cwd (cert symlinks)
  communities/<slug>/  community cwd: community.db (+WAL), attachments/, cert symlinks
  logs/                central.log, postgres.log, gaming.log, rust.log, design.log
  pids/                <name>.pid
  stack.json           addresses, server ids, cert fingerprints, pids (written by `up`)
  accounts.json        the seeded accounts (written by `seed`)
```

## How the pieces trust each other

- **Session tokens.** Central signs Ed25519 JWTs with `run/keys/central/jwt_ed25519.pem`, which it
  generates on its first boot (writing the `.pub` beside it). Each community gets the `.pub` as
  `DECIBELL_JWT_PUBLIC_KEY_FILE` and only verifies. Tools never mint tokens: `seed` and `smoke` sign in by
  password like a client.
- **Community secret.** One random value in `run/keys/community_secret`, given to central and to every
  community as `DECIBELL_COMMUNITY_SECRET`; it authenticates heartbeats, membership and invite sync.
- **Registration.** Each community heartbeats central (every 30 s here, `DECIBELL_HEARTBEAT_INTERVAL_SECONDS`)
  with its name, `DECIBELL_PUBLIC_IP` (its loopback address), port 8082, member count, public listing and
  its certificate's sha256. Central answers with the community's id, which the community caches
  (`server_meta.central_server_id`) and puts on membership registrations: joining a community registers
  the membership with central, which is what puts it in the next `LOGIN_RES`. `up` waits for this.
- **Central's certificate** is pinned by each community explicitly (`DECIBELL_CENTRAL_CERT_FINGERPRINT`,
  computed from `run/keys/central/server.crt`), not trust-on-first-use.
- **Clients** trust central on first use (the client stores sha256 per `host:port` in its
  `cert_pins.json`) and pin each community to the fingerprint central reports in memberships, the
  directory and invite resolution. `reset` keeps `run/keys/`, so a client's pins stay valid across resets.
  Deleting `run/keys/` re-keys everything: clients then show their certificate-changed prompt for
  127.0.0.1:18080 and the communities (re-trust there, or delete those entries from `cert_pins.json`).

## Signing a client in

- The client goes to the live central unless told otherwise. `DECIBELL_CENTRAL=host:port` is honoured by
  **debug builds** (`npm run dev`, `napi build` without `--release`) and by release builds with the cargo
  feature **`local-central`** (`crates/decibell-core/src/net/central.rs`). A release build without it, or a
  value without `:port`, silently uses the live central: check that the client's log or UI shows
  127.0.0.1:18080 before typing a password.
- Accounts: `alice`, `bob`, `carol`, `dave`, `erin`, `perf1`..`perf4`, all with password `decibell-test-1`,
  emails `<name>@example.test`. Uids 1..9 in that order after `reset`. `run/accounts.json`:
  ```json
  {"central": "127.0.0.1:18080", "central_cert_sha256": "…", "password": "decibell-test-1",
   "accounts": [{"name": "alice", "uid": 1, "password": "…", "email": "…", "communities": ["Daily Gaming", "Rustaceans"]}, …],
   "communities": [{"name": "Daily Gaming", "slug": "gaming", "owner": "alice", "host": "127.0.0.11", "port": 8082,
                    "attachment_port": 8085, "server_id": 1, "cert_sha256": "…", "invite": "<an open invite code>"}, …]}
  ```
- Use a separate client profile (user-data dir) for the stack, so a saved sign-in for a real account is
  never offered to it and test pins / device ids stay out of the real profile.
- No account has E2EE keys: a client sets them up itself (passphrase prompt) the first time it needs them.
- `seed` and `smoke` sign in from one fixed tool device per account ("Decibell dev stack", linux), so
  each account's device list shows that device too.
- DM calls: central hands out no STUN list (`DECIBELL_STUN_SERVERS` unset), so clients use their built-in
  public STUN servers for P2P call candidates.

## What `seed` produces

Timestamps are anchored to the moment `seed` runs (the fixture's newest #general message lands 4 minutes
before it; the live messages follow); ids, contents, people and structure are the same on every
`reset && up && seed`.

**Central.** 9 accounts. Friends: alice↔bob and alice↔carol accepted, dave → alice pending, alice blocked
erin. DMs: alice↔bob 599 (594 by SQL over 45 days in conversation bursts, 16 edited, 24 replies; then 5
sent for real, the last two from bob unread for alice), alice↔carol 30 (29 + 1; alice hasn't read them).

**Daily Gaming** (owner alice, public, 298 members):
- Categories / channels: *Information* (`welcome`, empty: the welcome banner; `announcements`, read-only
  for @everyone, Moderators may post, topic), *Text Channels* (`general`, `clips`, `off-topic` with a topic
  and a member overwrite giving dave Manage Messages, `slow-lane` with 30 s slowmode, `vault`
  end-to-end encrypted and empty), *Staff* (`mod-chat`, `mod-log`; category sync on, hidden from
  @everyone, visible to Moderators, both channels synced), *Voice Channels* (`Lounge`, `Squad`, `AFK`).
- `#general`: the **10,000-message fixture** of `qt-client/tools/gen-fixture.mjs` (seed 7: same contents,
  spacing, replies and image attachments bit for bit; `fixture.py` is the port) over 90 days from 40
  senders (alice, bob, carol, dave talk most; erin + 3 regulars; 32 occasional), 4 % edited (a second
  random stream), plus 5 live messages (a reply, an edit, an image upload). 1,052 fixture image
  attachments at the fixture's 18 sizes, each with 320/640/1280 JPEG thumbnails and a ThumbHash
  (`media.py`, a port of the `thumbhash` npm encoder, checked byte-identical), files hard-linked from
  the picture cache. `#off-topic`: 300 more fixture-style messages.
- Real uploads over the HTTPS API (init → PATCH → complete → thumbnails): a 3-image gallery, a phone
  screenshot, a PNG, a text file, a zip, a WAV in `#clips`; an image in `#general`.
- Roles: Admin (bob), Moderator (carol + 3 placeholders), Member (dave, erin, perf1-4 + 198 placeholders).
- Moderation done for real, so the audit log has it: griefer_9000 banned (reason given), drive_by_dan
  kicked, loud_larry timed out for a day, a moderator deleted a message, 5 nicknames (bob is "Bobby 🛠").
  Invites: an open one, a 5-use one, one expiring in a week.

**Rustaceans** (owner bob, unlisted, 29 members): @everyone without Create Invite, Change Nickname and
Stream; alice (role Rustacean) can't see `core-team`, can't post in `announcements`, can't attach in
`showcase`. carol has Core. 506 messages (`#general` 402, `#help` 102, 66 images). A personal invite for erin.

**Design Lab** (owner carol, public, 44 members; **alice is not a member**: join it from Browse or with
the invite in `accounts.json`): `critique` (topic), `inspiration`, `type-nerds`; Curator role (bob); 155
messages, 3 real image uploads.

Placeholder members are community-only rows with uids from 100001 (no central account has them), so
central knows nothing about them and clients name them from the member rows. They never come online.

## `smoke.py`

Against the running stack: TLS to central (TOFU, compared with the stack's cert), password sign-in as
alice, the memberships in `LOGIN_RES` (Daily Gaming, Rustaceans) with host / port / fingerprint, the
public directory (Daily Gaming, Design Lab), DM conversations; TLS to Daily Gaming at the host:port central
reported **pinned to the fingerprint central reported**, `COMMUNITY_AUTH_REQ` with the session token, the
newest page of `#general` (messages, `before_id` echo 0, `has_more`), then a thumbnail
(`?variant=thumb&size=320`, a JPEG ≤ 320 px) and a Range request over the attachment HTTPS API with the same
pin and the Bearer token. Exit status 0 when everything passed.

## Adding fixtures

- Anything with behaviour (channels, roles, overwrites, invites, joins, moderation, real messages and
  uploads) goes through the protocol in `seed.py`, using the helpers there (`ensure_channel`,
  `ensure_role`, `overwrite`, `say_once`, `upload_picture`, …). Each helper looks at the database first,
  which is what keeps `seed` idempotent; keep new steps that way.
- Bulk rows go through SQL inside `fill_*` (run by `bulk()`, which stops that community, writes in one
  transaction and starts it again so its caches load what's on disk). Bulk runs once per database,
  marked by `server_meta.devstack_bulk`; after changing a `fill_*`, `reset` (or remove the marker and the
  rows). Insert bulk messages before live ones and in time order across channels (`insert_messages` does
  this): message ids must rise with time.
- Respect the community's per-session buckets (messages 10 burst / 3 per s: `send_msg` paces at 2.5/s;
  uploads 20 inits then 1 per 2 s per account) and central's per-address limits (tools bind source
  addresses in 127.0.200.0/23 for that reason).
- New pictures: `picture(key, w, h, label)` draws and caches one (`cache/fixture-v1`); bump the directory
  name if you change the drawing.

## The e2e suites next to the stack

The stack uses its own directory and never the harnesses' defaults (`/tmp/decibell-deps`,
`/tmp/decibell-central-deps`, their `run/` dirs, ports 8080 and 8082–8085 on the wildcard address).

- **Central e2e / load test**: run as their READMEs say, stack up or not. They bind `0.0.0.0:8080` and
  their own pgserver PostgreSQL; the stack holds `127.0.0.1:18080` and a unix-socket-only PostgreSQL.
- **Community e2e** binds `0.0.0.0:8082–8085`, which the stack's communities occupy on their addresses.
  Either `local-stack.sh down` first, or run the suite in its own network namespace (unprivileged; it
  only talks to loopback):
  ```sh
  cd src/community/tests
  unshare -rn sh -c 'ip link set lo up && DECIBELL_E2E_PB="$D/pb" DECIBELL_E2E_SERVER="$D/community_server" \
    DECIBELL_E2E_RUN="$D/run" python3 e2e.py'
  ```
  (`setup_deps.sh` / `build.sh` need the network: run them outside.) Run that way on 2026-11-26 with
  the stack up (and being reset and re-seeded meanwhile): 780 passed, 0 failed. Conversely, `up` refuses
  to start while a community e2e run holds those ports on the host.

## Files

`local-stack.sh` (entry point, `setup`), `stack.py` (processes), `seed.py`, `smoke.py`, `devlib.py`
(paths, addresses, wire clients for central / community / attachment HTTPS), `fixture.py` (the
gen-fixture.mjs port), `media.py` (pictures, thumbnails, ThumbHash, WAV, zip), `bindshim.c`. The Python
runs on the central harness's venv with `-I`.
