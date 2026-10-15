# Central: hardening for scale

2026-10-15. Owner asked whether central was ready for hundreds of thousands of users, then to fix what
would break first and to measure it. Follows `2026-10-15-central-scaling-design.md` (threads, the
Postgres pool, presence by subscription).

## What would break first

1. **Reconnects signed in with the password.** After a restart or a network blip every client re-sent its
   stored password: one bcrypt (cost 12, ~0.2 s of CPU) per online user. 30,000 online meant about two
   CPU-hours of hashing, roughly 40 minutes on the production box before everyone was back.
2. **No abuse limits.** Sign-in, sign-up, DMs and DM history had no rate limit, and nothing was per IP
   address. A sign-in burns ~0.2 s of CPU and a fresh connection per attempt got round any per-connection
   limit: one machine could keep central's CPU busy.
3. **The open-file limit.** Nothing raised it from the usual soft 1024, so central refused connections
   at about a thousand users. And when accept() failed with EMFILE the loop retried at once, spinning a
   core.
4. **Every packet re-verified its token from scratch:** it re-parsed the PEM key and checked the Ed25519
   signature each time.

## Design

### Resume tokens

- A password sign-in returns `LoginResponse.resume_token` next to the session token. On a dropped
  connection the client sends `LoginRequest.resume_token` instead of the password; central checks a
  signature and reads the account, with no bcrypt.
- **Central-only, by audience.** Communities receive every member's session token (that is how they
  verify people), so a session token must never sign anyone into central. Resume tokens carry
  `aud: decibell_central_resume`; session tokens carry no audience. Central's per-packet check refuses a
  token with an audience, its resume check requires that one, and communities refuse any token with an
  audience (defence in depth: the client never sends a resume token to a community).
- **Lifetime: 30 days from the password sign-in.** A resume echoes the same token; it is not extended, so
  at most 30 days pass between password checks. A refused resume (expired, or a new signing key) falls
  back to the password silently; the client keeps its credentials as before (the "remember me" store).
- The resume answer is a normal LoginResponse: a fresh session token naming the current username (a
  rename in between is picked up), memberships, statuses.
- Reconnect delays are jittered (50–150% of each step), so a restart doesn't bring every client back in
  the same instant.

### Abuse limits (`abuse_limits.hpp`)

Across connections, per IP address (shared, thread-safe, swept every 30 s, tables capped at 500,000
entries):

| What | Limit |
|---|---|
| Open connections per address | 256 (a household or office behind NAT) |
| New connections per address | 60 at once, then 10 a second |
| Password sign-ins per address | 30 at once, then one every 2 s (resumes don't count) |
| Sign-ups per address | 10 at once, then one every 2 minutes |
| Failed sign-ins per account | 10, then one a minute; refused before any hashing, a success forgets them; a resume still works |
| Connections in total | open-file limit minus a reserve (threads' DB connections + 64), or `DECIBELL_MAX_CONNECTIONS` |

Per connection, **paced** (an over-rate request waits and reading pauses until the bucket refills: a
burst is delayed, never dropped, and the connection's packets keep their order):

| Requests | Rate |
|---|---|
| DIRECT_MSG, DM edit, DM delete | 10 at once, 3 a second (the community's message rate) |
| DM history, conversations, read cursors, avatar and server-picture fetches, friend and server lists, invite lookups | 60 at once, 20 a second |
| Friend actions, avatar uploads | 20 at once, 1 a second |
| Sign-in / sign-up attempts | 5 at once, one every 2 s |

The existing refused-over-rate buckets (calls, keys, renames, renewals, statuses, subscriptions) stay.

### File descriptors

- At startup central raises its soft open-file limit to the hard one (systemd's `LimitNOFILE=` sets the
  hard one) and logs both the limit and the connection cap derived from it.
- accept() failing with EMFILE / ENFILE / ENOBUFS / ENOMEM waits 100 ms before the next accept (logged at
  most every 10 s); connections already open keep being served. Refused connections are closed at once,
  logged as a count at most every 10 s.

### Token checks

- AuthManager parses the keys once: one signer, one session verifier, one resume verifier, shared by all
  threads (const use only).
- A session remembers the last token that verified and its expiry; a packet carrying the same token only
  has its expiry checked.

## Verification

- Central e2e (resume, refusals of session tokens / junk / expired / foreign-audience tokens, resume after
  a rename, the per-account and per-address limits, DM pacing, the connection cap, and running out of
  descriptors without spinning), under ThreadSanitizer and AddressSanitizer + UBSan + LeakSanitizer.
- Community e2e (the audience check), native unit tests, renderer typecheck.
- `src/server/tests/load.py`: the load test, with the results in the review log.
