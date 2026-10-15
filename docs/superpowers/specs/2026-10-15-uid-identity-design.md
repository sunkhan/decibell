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
   credentials bind the uid. v1 stays readable for history. Fingerprints and safety numbers change once.
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

## Stage 3: client ✅

- **Native.** Every event payload that names a user carries its uid, and `login_succeeded` carries ours
  (also kept in `AppState.uid`). Every command that targets someone takes an optional uid and sends it,
  so the server falls back to the name when it is absent.
- **Renderer.** The payload types mirror the uids. `features/users/userDirectory.ts` keeps the latest
  uid seen for each name, fed by every event that carries both. A request that targets someone sends
  `uidOf(name)`: moderation, roles, nicknames, voice moderation, ownership, overwrites, invites, DMs,
  friends, calls, streams and search senders.
- **Stores stay keyed by name** (see "Why the client keeps name-keyed stores").
- **What stays on names:**
  - E2EE pins and the keystore move to uids in stage 4, with the binding.
  - Voice volumes and local mutes follow the media plane, which names speakers. Stage 5 re-keys them on
    a rename.
  - Avatar fetches match the reply by name. Stage 5 refreshes them on a rename.
  - Nothing else user-keyed is stored on the device.

## Stage 4: encryption bound to the uid ✅

Done on the branch (see the review log). Verified by native unit tests for every v2 format, opening v1
history, legacy-pin adoption and the keystore migration; by central e2e checks for the rebind; and by a
community e2e check for v1 / v2 edits after a rename.

The goal: a rename changes nothing in encryption. The identity keys stay the same. What changes is what
they sign and what the key schedules bind.

### Formats (v2)

uid = int64, little-endian (8 bytes). Salts, key ids, tags and inner content are unchanged.

| Item | v1 (name) | v2 (uid) |
|---|---|---|
| Bundle signature | `"decibell-e2ee-bundle-v1" ‖ 0 ‖ name ‖ 0 ‖ dh_pub ‖ sign_pub` | `"decibell-e2ee-bundle-v2" ‖ 0 ‖ uid ‖ dh_pub ‖ sign_pub` |
| Fingerprint | SHA-256(fp-v1 ‖ name ‖ sign_pub) | SHA-256(`"decibell-e2ee-fp-v2" ‖ 0 ‖ uid ‖ sign_pub`) |
| Safety number | both fingerprints in name order | both fingerprints in uid order |
| DM envelope | header byte `0x01`. The HKDF info binds both names; AAD = header ‖ sender name | header byte `0x03`. Info = `"decibell-dm-v2" ‖ 0 ‖ lo_uid ‖ hi_uid ‖ lo_kid ‖ hi_kid` (uid order); AAD = header ‖ sender uid |
| Channel envelope | header byte `0x02`. Info and AAD bind channel ‖ sender name | header byte `0x04`. The same, with the sender uid |
| Key blob | a v1 DM envelope | a v2 DM envelope (filler uid → recipient uid) |
| Call auth | `"decibell-call-auth-v1" ‖ call ‖ from ‖ to ‖ key` | `"decibell-call-auth-v2" ‖ 0 ‖ call_id ‖ 0 ‖ from_uid ‖ to_uid ‖ pub_key` |
| MLS credential | `name` | `"<uid>:<name>"` |

- **The header byte tells a reader which binding to use,** so v1 and v2 rows mix freely in one history.
  Sealing is always v2. Opening accepts both: v1 with the sealed-under names (rule 4), v2 with the uids
  the server stamps (`sender_uid`, `recipient_uid`).
- **Calls are v2 only.** Nothing about a call is stored, so there is nothing old to read.
- **MLS** credentials carry the uid for verification against the uid-bound bundle. They also carry the
  name for the media plane, which still names senders; per-sender media keys derive from that name.
  Election and ghost removal compare uids from `VoicePresenceUpdate.active_uids`.

### Bundles: re-signed, not re-keyed

- `E2eeKeyBundle.binding` is 1 = name or 2 = uid. Central stores it as `e2ee_keys.binding` (migration 2;
  existing rows get 1). New identities (setup, reset) publish binding 2.
- **An existing identity is rebound.** After login, a device that holds the current key and finds binding
  1 on central re-signs the same keys over the uid. It sends `E2eePublishKeysReq{bundle, rebind: true}`.
  - Central replaces the signature and binding of the current row in place, and only when `key_id`,
    `dh_pub` and `sign_pub` match that row.
  - No new key id, so history and the user's other devices are untouched.
  - Central broadcasts `E2EE_KEYS_CHANGED` with the same key id, so peers drop cached bundles.
- **The feature is gated** on `LoginResponse.e2ee_uid_binding`. Against a central without it, clients
  publish v1 bundles and never rebind. An old central would take a rebind for a fresh publish and fork
  the key ids.
- **Verification:**
  - A binding-2 bundle must name the uid that was asked for.
  - A binding-1 bundle verifies over `signed_name`. A *current* v1 bundle seals only while `signed_name`
    is still the account's name: after a rename, a v1 signature says nothing about who holds the key.
  - Historical v1 bundles open history whatever name they carry.

### What users see

- **Fingerprints and safety numbers change once, for everyone,** because they now hash the uid. TOFU pins
  hold the signing key, which doesn't change, so no "keys changed" warning fires. Nothing records a past
  verification, so nothing else is invalidated. People who compared safety numbers should compare again.
- **Old messages stay readable.**
- **Older clients break.** A client from before this release can't open v2 messages, verify v2 calls or
  join v2 voice groups. Release to everyone at once (pre-1.0 rule), after central is deployed.

### Device storage

- **The keystore** is `e2ee/u<uid>.json`, with its AAD bound to the uid (outer file version 2). On first
  load the client migrates the name-keyed file, then deletes it.
- **Pins key on the uid.** Pins from the old store move to `legacy_peers`, still keyed by name. A legacy
  pin is adopted by uid the first time that peer's current bundle resolves under the same name. Names
  can't change before stage 5, so the name still means the same account.

### Server changes

- **Central:** migration 2 (`binding`), the rebind, and `LoginResponse.e2ee_uid_binding`.
- **Community:** the refusal to edit an encrypted message sent under a previous name now applies to v1
  envelopes (`0x02`) only. A v2 edit binds the uid and stays readable.

## Stage 5: the rename ✅

Done on the branch (see the review log). Verified by central e2e checks for every rule below, a
community e2e check for the token guard, and a scratch test of the client's re-keying.

### Central

- **`RENAME_REQ {new_username, password}`** → `RENAME_RES {success, message, username, jwt_token}`.
  - The password is checked against the session's account.
  - The name follows the registration rules (`AuthManager::validateUsername`, now shared).
  - Refused when taken, when another account's reservation holds it, or when it is the current name.
  - At most **two renames per 24 hours**. The rule allows a typo fix, and limits how many names one
    account can hold in reservation.
  - A per-session bucket (10 burst / 1 per 30 s) keeps a stolen token from guessing the password.
  - On success the session's name changes and a fresh token naming it is returned. Central allows one
    session per account, so there is no other session to update.
- **`name_changes(uid, old_username, new_username, changed_at)`** (migration 3) records each rename.
  - **Reservation:** an old name renamed away from in the last 14 days is closed to registration and to
    other accounts' renames. The account that left it may rename back.
  - **Sign-in with the old name** still works during the reservation (a device that saved it). The
    response and token carry the current name, and the client updates its saved credentials.
  - Rows are kept as an audit trail; the last day's rows rate-limit renames.
- **`USER_RENAMED {uid, old_username, username}`** goes to every session, but only when the renamer is
  visible. An invisible user's rename would reveal activity, so others see it at their next fetch.
- **`LoginResponse.renames`** advertises the feature.

### Community

Members are renamed at auth from the token (stage 2). Schema v16 adds `members.name_iat`: a token
issued before the one that last named the member can't rename them back, and that session runs under
the newer name. A device can hold a token across a rename for up to 24 h.

### Client

- **Native `rename_account`.**
  - Refused while in a community voice channel (the reconnect would drop it).
  - On success it swaps the token, moves the saved credentials to the new name, emits
    `user_renamed {self: true}` and reconnects every community with the new token.
  - Central's `USER_RENAMED` for anyone else becomes `user_renamed {self: false}`.
- **`features/users/applyRename.ts`** moves everything held under the old name to the new one, in place.
  Untouched stores keep their references. It covers:
  - DM threads (key, peer, senders), the active DM, pending deletions;
  - channel message senders, members, owners;
  - friends, presence, avatars, DM drafts, E2EE notices, call peer, profile / menu targets;
  - saved per-user volume and local mute (saved again and re-applied).
  Live voice state is left alone: the renamed user's session reconnects under the new name.
- **Settings → Account → "Change username"** (`RenameUsernameModal`) asks for the new name and the
  password.
