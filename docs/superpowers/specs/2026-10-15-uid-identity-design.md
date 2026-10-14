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

## Stage 2: community ✅

Done on the branch (see the review log). Verified by the community harness, 570 checks:
- every pre-existing check, updated where it encoded the old name semantics;
- migration v15 run on a database the previous build filled, plus rows from before uids reached the
  server, with each placeholder then claimed by its name's token;
- requests addressed by uid, and a uid-less token refused;
- current names in history, and a rename retiring the old roster row.

Still name-based on purpose until the client stage, because old clients parse them:
- the media plane, where the relay stamps the sender's name into `sender_id` and keyframe / NACK
  requests name the streamer (resolved to a uid among online sessions);
- `VideoStreamInfo.stream_id` (`<name>_screen`);
- member overwrites' `target_id`, which carries the current name next to `target_uid`.

### Identity at auth

- **Tokens must carry a uid.** Central has signed one into every token since 2026-08-22 (never 0 since
  2081fb0), and tokens last 24 h, so a token without one is refused ("Sign in again").
- The session keeps the uid and the token's name, which is the current name. All session state is
  keyed by the uid.
- **A changed name is learned at auth.** If the member row for the uid holds another name, the row is
  updated. The roster gets a `MemberRemove` for the old name (older clients key rows by name) and an
  upsert for the new one.

### Schema (v15, one transaction)

- Every user reference becomes a uid: `members` (uid primary key; `username` = the latest name seen),
  `member_roles`, member overwrites (`target_id` = the uid in decimal, like role ids), `bans`,
  `invites` (`created_by_uid`, `for_uid`), `attachments.uploader_uid`, `channel_key_epochs.created_by_uid`,
  `channel_key_blobs` (`recipient_uid`, `sender_uid`) and `server_meta.owner_uid`.
- `messages.sender_uid` already existed (v14). Rows still at 0 get mapped.
- Names stay where they mean "the name at the time":
  - message `sender` and key-blob `sender` / `recipient`, which a v1 envelope binds;
  - `audit_log.actor` / `target`, as history. `actor_uid` / `target_uid` are added next to them.
- **Legacy rows.** Members who haven't connected since 2026-08-22 have no uid, and neither do name-only
  bans, the invites, uploads and key blobs they made, or a fresh server's configured owner.
  - Each such name gets a negative placeholder uid, recorded in `legacy_claims (username, uid)`.
  - When a token with that name first authenticates, the placeholder is rewritten to the real uid in
    every table at once, and the claim row goes. That's the same trust today's name backfill uses.
  - The owner of a fresh server (`DECIBELL_OWNER` is a name) is seeded the same way.
  - Leftover risk: a placeholder that is never claimed could be claimed by whoever holds the name after
    a rename (stage 5) and the 14-day reservation. Members claim theirs on their first connect after this
    deploy, and the client connects to every membership at login, so that window is small.

### Wire (additive)

- Requests carry the target's uid, with the name as the fallback (resolved against `members.username`).
- Payloads carry uids next to current names.
- `ChannelMessage` gains `sender_uid`, `reply_to_sender_uid`, `sealed_sender` and
  `reply_to_sealed_sender` (rule 4).
- Key blobs keep the names they were sealed under, and carry the uids.
- Membership sync to central sends the uid.

## Stages 3–5

Specified when each starts, in this file.
