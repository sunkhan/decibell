# Users by uid: design

2026-10-15. Implements the plan in `2026-10-14-uid-identity-audit.md` (the audit lists every username-keyed
spot this replaces). Owner decisions: encryption moves to a uid binding (v2), old names are reserved for
14 days, and the two present-day central bugs went first (2081fb0). Work happens on the `uid-identity`
branch, which merges to `main` at the end of each stage that can ship alone.

## Rules

1. **Rows key on the uid.** Every stored reference to a user, on central and on the community server, is
   the account uid. Ownership checks, filters, purges and session lookups compare uids.
2. **Names are labels.** A name is read at display time: central joins `users`, and the community keeps
   the latest name it knows for each uid in `members`. The one exception is the name a message was sent
   under: rows keep it, because v1 encryption binds it (see rule 4).
3. **The wire carries both.** Every user reference gains an `int64 *_uid` field next to its name.
   - Requests target by uid. The name stays as the fallback, for names a person typed (add friend,
     message someone) and for clients that don't send the uid yet. Servers resolve that name to a uid at
     the request edge, before anything is written.
   - Payloads carry the uid and the **current** name.
4. **Rows sealed under a name say so.** A v1 envelope binds the names it was sealed under: a DM binds both
   names (key derivation and AAD), a channel message binds the sender. After a rename those can differ
   from the current names, so a row with an envelope also carries `sealed_sender` (and, for DMs,
   `sealed_recipient`): the names stored with it. Clients open v1 envelopes with those. v2 envelopes bind
   uids and need no names.

## Stages

Each stage leaves every tier working with every other, so they can deploy one at a time.

1. **Central:** storage, sessions and wire by uid. It accepts names where a uid is missing, so today's
   clients and communities keep working.
2. **Community:** the same. Membership sync to central carries the uid.
3. **Client:** carries uids from payloads into its stores and sends them in requests. Anything stored on
   the device is keyed by uid. In-memory stores stay keyed by name, and a rename re-keys them (stage 5).
4. **Encryption v2:** bundles, fingerprints, safety numbers, DM and channel envelopes, call auth and MLS
   credentials bind the uid. v1 stays readable for history. Everyone sees one "keys changed" notice.
5. **Rename:** a password-confirmed username change on central. The old name is reserved for 14 days.
   `USER_RENAMED {uid, name}` goes to online sessions, communities learn the new name, and the client
   re-keys its stores.

### Why the client keeps name-keyed stores

The renderer has about 800 username references and native about 1,300. Re-keying all of them by uid would
be the largest and riskiest part of the work, for an event (a rename) that is rare. With rules 3 and 4 in
place, every fetched list and history already shows current names, so the client only has to patch live
state when a rename arrives (or refetch it). What must not split across a rename is what lives on the
device: E2EE pins and the keystore, the avatar cache and drafts. Those move to uid keys in stage 3.

## Stage 1: central ✅

Done on the branch (see the review log). Verified by the new central harness (`src/server/tests/`):
123 checks against a real PostgreSQL. They cover every request addressed by uid and by name, forged
uids, a simulated rename (the renamed account keeps friends, DMs, keys, backup and memberships; a
newcomer who registers the old name inherits none of it), and migration 1 run on a database the
pre-uid binary filled.

### Schema (migration 1, one transaction)

Central gains a `central_schema` version row. Migration 1 runs once, inside one transaction, and logs
the row counts it moved.

| Table | Change |
|---|---|
| `users` | Primary key moves from `username` to `uid` (reusing `users_uid_idx`); `username` becomes UNIQUE. |
| `friends` → `friendships` | `(uid1, uid2)` primary key with `uid1 < uid2`, `action_uid`, foreign keys to `users`, index on `uid2`. |
| `dm_messages` | Adds `sender_uid`, `recipient_uid` (NOT NULL, foreign keys); pair and recipient indexes move to the uid columns. `sender` and `recipient` stay as the names each row was sent under. |
| `dm_read_state` → `dm_read_cursors` | `(reader_uid, peer_uid)` primary key. |
| `user_e2ee_keys` → `e2ee_keys` | `(uid, key_id)` primary key; `signed_name` keeps the name a v1 bundle's signature covers. |
| `user_e2ee_backup` → `e2ee_backups` | `uid` primary key. |
| `user_communities` → `community_memberships` | `(uid, server_id)` primary key, index on `server_id`. |

- Old tables are renamed `legacy_*`, not dropped, until the deploy is verified. Take a `pg_dump` before
  the rebuild anyway.
- DM rows whose names match no account are deleted, with a count in the log. These are the messages left
  for unregistered names (fixed in 2081fb0).

### Sessions

- Login reads the uid with the password hash. The session keeps `uid_` and the stored username.
- Every SessionManager lookup (private delivery, visibility, kicks, presence, broadcasts to members)
  matches on the uid.
- The DM gate compares uids, and friendship is one query instead of loading the recipient's friend list.

### Wire (all additions, field numbers after the current last)

| Message | New fields |
|---|---|
| LoginResponse | `uid`, `username` (as stored) |
| DirectMessage | `sender_uid`, `recipient_uid`, `reply_to_sender_uid`, `reply_to_sealed_sender`, `reply_to_sealed_recipient` |
| UserPresence | `uid` |
| FriendActionReq | `target_uid` |
| FriendInfo | `uid` |
| FetchAvatarReq / FetchAvatarRes / AvatarChanged | `uid` |
| DmConversationPreview | `peer_uid`, `last_message_sender_uid`, `sealed_sender`, `sealed_recipient` |
| DmHistoryReq / DmHistoryRes | `peer_uid` |
| DmHistoryMessage | `sender_uid`, `reply_to_sender_uid`, `sealed_sender`, `sealed_recipient`, `reply_to_sealed_sender`, `reply_to_sealed_recipient` |
| DmMarkReadReq | `peer_uid` |
| DmDeleteReq / DmDeleteRes / DmMessageDeleted | `peer_uid` |
| DmEditReq / DmEditRes / DmMessageEdited | `peer_uid` (+ `sender_uid` on DmMessageEdited) |
| CallSignal | `from_uid`, `to_uid` |
| E2eeKeyBundle | `uid`, `signed_name` |
| E2eeFetchKeysReq / E2eeFetchKeysRes / E2eeKeysChanged | `uid` |
| MembershipRegisterReq / MembershipRevokeReq | `uid` |

- Name fields in payloads carry current names (DM history joins `users`).
- A DM row's `sealed_*` fields are set when the row has an envelope.

## Stages 2–5

Specified when each starts, in this file. The audit's community table is the checklist for stage 2.
