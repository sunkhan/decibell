# Central: ready to scale

2026-10-15. Owner request: do now what keeps a later scale-out cheap ("optimized, clean and scalable,
no bugs or memory leaks"). Background: at millions of users central becomes several tiers (connection
nodes, a stateless API tier, a message bus, presence by subscription); communities already carry the
heavy traffic. This change does the parts that are cheap today and would be expensive to retrofit.

## What was wrong

- **One thread, blocking.** One `io_context` thread ran every session, and every handler waited on
  synchronous Postgres. A login's bcrypt (cost 12, about 0.25 s of CPU) stalled every user; a reconnect
  storm after a restart froze central for seconds per hundred logins.
- **A new Postgres connection per query** (46 call sites): a TCP + auth handshake, and a forked
  backend on the database side, per query.
- **Presence to everyone.** Every status change went to every signed-in session, and every login got a
  snapshot of every visible user. Work grows with the square of the online count; at a million online,
  one change is a million packets. Renames, avatar changes and key changes fanned out the same way.
- **Sessions read each other.** Fan-out code walked all sessions reading their fields, which only
  worked because there was one thread.

## Design

### Threads and strands

- `io_context` runs on `DECIBELL_IO_THREADS` threads (default: hardware concurrency, at least 2).
- Each session lives on its own strand: the acceptor hands every socket a fresh strand, so its reads,
  writes and packet handling are serialized without locks, and different sessions run in parallel.
- A handler may still block on Postgres; it holds one of N threads, not all of them.
- **Password hashing runs on a separate pool** (`DECIBELL_CPU_THREADS`, default half the cores, at
  least 1): login, registration and rename. The session stops reading while one runs and resumes from
  the continuation, so packets stay in order, a login storm queues there, and the io threads keep
  serving everyone else.

### The connection directory (SessionManager)

All state shared between sessions lives in `SessionManager`, behind its mutex. A session never reads
another session's fields; the directory holds what others need:

- `sessions_`: every connection (for the stale sweep).
- `accounts_`: signed-in accounts by uid → `{session, presence, watching}`. One session per account (a login
  replaces the account's previous connection, which is then closed).
- Presence: chosen status, idle, custom message + expiry, avatar version, display name, friends-only
  DMs. Session handlers update it through the directory and act on whether others' view changed.
- The watch index: `watchers_` (uid → accounts watching it), the reverse of each account's `watching`
  set. Both are pruned when an account disconnects or replaces its set; empty entries are erased.

Delivery: `deliver_to_user(uid, frame)` is the one way to reach an account; `deliver_to_users`,
`deliver_to_watchers` and the profile pushes are built on it. `Session::deliver` posts onto the target's
strand, so any thread may call it. In a multi-node deployment `deliver_to_user` is where a message bus
plugs in (publish to the node that holds the account); nothing else needs to know. Targets are collected
under the lock and delivered after releasing it. Closing a connection is likewise posted to its strand.

The stale sweep reads an atomic activity timestamp. Expired status messages are found in the directory,
cleared there, saved, and announced from the sweep thread.

### Presence by subscription

- `PRESENCE_SUBSCRIBE { repeated int64 uids }` (client → central): the full set of accounts the client
  is showing; each request replaces the previous set. At most 5,000 uids (the rest are ignored); a
  per-session bucket (10 burst, 2 per second) bounds the rate.
- Central answers with `PRESENCE_UPDATE { users, covered_uids }` for the accounts **newly** added:
  `covered_uids` lists them, `users` holds those visible right now; a covered uid without an entry is
  offline.
- `PRESENCE_DELTA`, `USER_RENAMED`, `AVATAR_CHANGED` and `E2EE_KEYS_CHANGED` go to the account's
  watchers only. Statuses stay visible to every signed-in user (owner decision 2026-10-11): anyone may
  watch anyone; only delivery changed.
- No snapshot at login any more; `LoginRequest.presence_deltas` is reserved (every client takes deltas).
- Friend lists keep their online/offline flag (`is_user_visible`), and invisible users are still never on
  the wire.
- **Client:** every hook that shows a user's status (`usePresence`, `useStatusMessage`) registers
  interest in that uid while mounted (ref-counted). A throttled flush (at most every 600 ms) sends the
  set; native keeps the last set and re-sends it after every central login. The presence store tracks
  which uids central has answered for: one of those without a status is offline; one it hasn't is drawn
  from the community roster's own flag until the answer arrives.
- A key change missed while not watching someone is caught when watching starts: native forgets that
  account's cached bundle on a subscription answer, so the next send fetches it fresh.

### Postgres pool

`PgPool` keeps idle connections for reuse (at most io + cpu threads + 2 idle) and opens a new one when none
is idle, so nested acquisitions can't deadlock. A connection goes back only if it is still open; a broken
one is dropped and replaced by the next acquire. `AuthManager` takes a lease per operation, as before it
opened a connection.

### Decided for later features

- **DM attachments**, when they come, go to object storage (S3-compatible) as end-to-end encrypted blobs;
  central stores and serves no file bytes.
- **Message ids** (DM and channel) are 64-bit and increase over time; that is all clients may assume (no
  density, no arithmetic). It leaves room for ids generated without a central counter (Snowflake-style).

## Verification

- Central e2e, including new checks: a login storm doesn't stall a DM round trip; presence and profile
  pushes reach watchers only; a replaced set stops deltas; the cap; a watcher's disconnect cleans up.
- The same suite under AddressSanitizer + UndefinedBehaviorSanitizer (leaks included) and under
  ThreadSanitizer (`build.sh` takes `EXTRA_CXXFLAGS`).
- Native unit tests, renderer typecheck, community e2e (proto change).
