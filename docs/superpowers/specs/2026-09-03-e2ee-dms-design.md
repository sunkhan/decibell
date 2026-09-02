# End-to-end encrypted DMs — Design

**Date:** 2026-09-03
**Author:** sunkhan (with Claude)
**Status:** Implemented on `main` (client + central); central rebuild on the Hetzner box pending

## Problem

Every DM is stored in central's Postgres in plaintext (`dm_messages.content`) and
travels central→client in plaintext inside TLS. Whoever operates a central — or
reads its disk, its backups, or its logs — can read every private conversation.
Decibell is self-hostable, so "trust the operator" is not a story we can tell.
This is the first step of a longer programme: DM *messages* first, then more
(calls already seal their media plane; channels, attachments and call-key
authentication come later on the same identity keys).

## Threat model

Protects DM **content** from:

- the central server and its operator, its database, its backups, its logs;
- anyone who compromises central after the fact (stored ciphertext only);
- a network attacker who defeats TLS.

Does **not** protect against:

- a compromised endpoint (the plaintext is on screen and the keys are on disk);
- **metadata** — who talks to whom, when, how much, message ids, read cursors;
- a central that substitutes public keys (MITM). Mitigation: identity fingerprints
  and a per-conversation safety number are shown in the profile popup for
  out-of-band comparison, key changes are surfaced, and the client pins the first
  identity it sees. Verification *state* ("I checked this") is a follow-up;
- a central that withholds keys to force plaintext — the client **refuses to
  downgrade** once it has pinned a peer's identity;
- **forward secrecy.** v1 uses static identity keys (see *Why not a ratchet*).

## Decisions

| Question | Decision |
|---|---|
| Protocol | **Static–static X25519 + per-message HKDF + AES-256-GCM.** One long-term identity per user; a conversation root from X25519(my identity, peer identity); every message gets a fresh key from a 32-byte random salt. Versioned envelope so a ratchet can replace it later. |
| Identity | Per **user**, not per device. One X25519 agreement key + one Ed25519 signing key, published to central as a self-signed bundle with a central-assigned monotonic `key_id`. |
| Multi-device | The private keys are backed up **on central, encrypted under a user passphrase** (Argon2id → AES-256-GCM). A new device enters the passphrase once; central never learns it (the passphrase is not the login password — central sees that one at login, so it cannot be the KEK). |
| Where the crypto runs | **Native (Rust).** The renderer never sees a key or a ciphertext: DM events arrive already decrypted with an `encrypted` flag and a `decryptError` string; sends are sealed inside `send_private_message` / `edit_dm_message`. |
| Rollout policy | **Encrypt whenever both sides have keys; plaintext otherwise, visibly.** A peer that has never set up encryption gets plaintext with an open-lock indicator. A peer whose identity we have pinned can *never* be downgraded. |
| Old clients | The sender puts a fixed placeholder in `content` next to the envelope, so a pre-E2EE client shows "🔒 This message is end-to-end encrypted. Update Decibell to read it." A new client ignores `content` when an envelope is present. |
| Old centrals | `LoginResponse.e2ee_keys` gates the whole feature. On a central without the key endpoints the client behaves exactly as before and the Privacy tab says so. |
| History | Server-side history stays the source of truth (there is no local message store). Old envelopes are decryptable forever because the identity keys are static and every old key is kept (locally and in the backup) after a rotation; the envelope names the `key_id`s it was sealed under. |

### Why not a ratchet (Signal / Olm / Megolm)

Forward secrecy means deleting keys, which means the client must keep a local copy
of every decrypted message (or every message key). Decibell has no local message
store — every login re-fetches history from central — so a ratchet would turn
"your history" into "whatever this device has seen since it joined", plus the
whole device-verification and key-backup UX that comes with it. That is the right
long-term destination and the envelope is versioned for it, but it is not the
first step. Static keys give the operator-can't-read property today with a
one-passphrase-per-device UX and no unreadable history.

## Cryptography

All primitives come from crates already in the tree or standard RustCrypto/dalek
ones: `x25519-dalek` (static secrets), `ed25519-dalek`, `ring` (HKDF, random),
`aes-gcm`, `argon2`, `zeroize`.

### Identity bundle

```
dh_priv, dh_pub     = X25519 keypair (32 B each)
sign_priv, sign_pub = Ed25519 keypair (32 B seed / 32 B public)
signature           = Ed25519(sign_priv, "decibell-e2ee-bundle-v1" ‖ username ‖ dh_pub ‖ sign_pub)
bundle              = { username, key_id, dh_pub, sign_pub, signature, created_at }
```

`key_id` is assigned by central (`MAX(key_id)+1` per user, single io thread, no
race) and is bookkeeping, not a security property: a peer's *identity* is its
`sign_pub`. The self-signature binds the two public keys to the username so central
cannot serve a mix-and-match bundle. A fetched bundle whose signature does not
verify is rejected.

Fingerprint of a user = `SHA-256("decibell-e2ee-fp-v1" ‖ username ‖ sign_pub)`,
shown as 12 groups of 4 base32 characters (first 30 bytes). Safety number of a
conversation = `SHA-256(fp(lower) ‖ fp(higher))` formatted the same way, so both
parties see the same string.

### Message envelope (v1)

```
root   = X25519(my_dh_priv[my_kid], peer_dh_pub[peer_kid])
salt   = 32 random bytes (per message)
prk    = HKDF-Extract(SHA-256, salt, root)
okm    = HKDF-Expand(prk, "decibell-dm-v1" ‖ 0 ‖ lower ‖ 0 ‖ higher ‖ 0 ‖ lower_kid ‖ higher_kid, 44 B)
key    = okm[0..32]      nonce = okm[32..44]
header = 0x01 ‖ sender_kid (u32 LE) ‖ recipient_kid (u32 LE) ‖ salt         (41 B)
aad    = header ‖ sender_username
body   = AES-256-GCM(key, nonce, plaintext = 0x01 ‖ utf8(text), aad)
wire   = header ‖ body ‖ tag(16)                                          (57 B + text)
```

`lower`/`higher` are the two usernames in byte order, `lower_kid`/`higher_kid` the
matching key ids. Binding both names and both key ids into the schedule means a
message can never be opened under another pair or another key generation; the
sender's name in the AAD (checked against the server-stamped `sender`) defeats
reflection (a peer bouncing your own ciphertext back as theirs). A fresh key per
message from a 256-bit salt makes nonce reuse a non-issue. The leading byte of the
plaintext is a content tag (`0x01` = text) so attachments/reactions can ride the same
envelope later.

Decrypting our own sent messages (echo, history) uses the same root — X25519 is
symmetric — with the roles read off the server-stamped sender.

### Passphrase backup

```
kek     = Argon2id(passphrase, kdf_salt[16], m = 64 MiB, t = 3, p = 1) → 32 B
header  = 0x01 ‖ m_kib (u32) ‖ t (u32) ‖ p (u32) ‖ kdf_salt ‖ nonce[12]
payload = JSON { current_key_id, keys: [{ key_id, dh_priv, sign_priv, created_at }] }
blob    = header ‖ AES-256-GCM(kek, nonce, payload, aad = header) ‖ tag
```

Stored on central in `user_e2ee_backup` (≤ 8 KiB), returned only to its owner. The
parameters ride in the header so they can be raised later without breaking old
blobs. Passphrase minimum is 10 characters. "Change passphrase" re-wraps the same
keys; "Reset" mints a new identity (new `key_id`), publishes it, and re-uploads a
backup containing the old keys too — old history stays readable on this device and
on any device that unlocks with the new passphrase. Reset exists for the
forgot-my-passphrase case, in which the old keys are simply gone and old messages
render as undecryptable.

### Local key store

`<userData>/e2ee/<username>.json`: `{ version, current_key_id, keys[], peers{} }`,
AES-256-GCM at rest. The at-rest key comes from Electron `safeStorage` (a random
32-byte key wrapped by the OS keychain in `<userData>/e2ee/local.key`) when
`safeStorage.isEncryptionAvailable()`, otherwise the `config.rs` hostname+user
derivation — i.e. the same local-disk posture as the stored login credentials. The
store is a cache: if it cannot be opened, the account shows as *locked* and the
passphrase restores it from the backup.

`peers` holds the **pinned identity** per peer (`sign_pub`, current `dh_pub`,
`key_id`, `first_seen`, `changed_at`) plus every historical `dh_pub` by `key_id`
seen while decrypting history, so old envelopes don't need a fetch each session.

## Wire protocol

```proto
// Packet.Type
E2EE_PUBLISH_KEYS_REQ = 129;   // client→central (JWT): own bundle and/or backup
E2EE_PUBLISH_KEYS_RES = 130;   // central→client
E2EE_FETCH_KEYS_REQ   = 131;   // client→central: a user's bundle (current or by key_id)
E2EE_FETCH_KEYS_RES   = 132;   // central→client
E2EE_FETCH_BACKUP_REQ = 133;   // client→central: own backup blob
E2EE_FETCH_BACKUP_RES = 134;   // central→client
E2EE_KEYS_CHANGED     = 135;   // central→every session: a user published a new bundle

message E2eeKeyBundle { string username; uint32 key_id; bytes dh_pub; bytes sign_pub; bytes signature; int64 created_at; }
message E2eePublishKeysReq { E2eeKeyBundle bundle; bytes backup; }   // either may be absent
message E2eePublishKeysRes { bool success; string message; uint32 key_id; }
message E2eeFetchKeysReq   { string username; uint32 key_id; }        // 0 = current
message E2eeFetchKeysRes   { string username; uint32 key_id; bool found; E2eeKeyBundle bundle; }
message E2eeFetchBackupReq {}
message E2eeFetchBackupRes { bool found; uint32 key_id; bytes backup; }
message E2eeKeysChanged    { string username; uint32 key_id; }

DirectMessage        += bytes envelope = 11, bytes reply_to_envelope = 12;
DmHistoryMessage     += bytes envelope = 9,  bytes reply_to_envelope = 10;
DmConversationPreview+= bytes last_message_envelope = 7;
DmEditReq            += bytes envelope = 4;
DmMessageEdited      += bytes envelope = 5;
LoginResponse        += bool e2ee_keys = 7;
```

Central: `user_e2ee_keys (username, key_id, dh_pub, sign_pub, signature, created_at)`,
`user_e2ee_backup (username PK, key_id, blob, updated_at)`,
`dm_messages.envelope BYTEA` (NULL = plaintext row). The DM / edit handlers pass the
envelope through untouched (cap 64 KiB + 64), the reply-preview join returns the
parent's envelope, the sidebar preview returns the last message's envelope. Key
fetches sit behind a per-session bucket (20 burst / 5 per s, the CALL_SIGNAL one
generalised). Publishing validates sizes (32/32/64 bytes) and broadcasts
`E2EE_KEYS_CHANGED` the way `AVATAR_CHANGED` is broadcast.

## Client flow

```
login_succeeded
  └─ native: LoginRes.e2ee_keys?  no → status "unavailable" (plaintext, no prompts)
       yes → local store for <username> opens?  yes → "ready"
             no → E2EE_FETCH_BACKUP_REQ → found → "locked"   (renderer: unlock prompt)
                                        → none  → "not_set_up" (renderer: set-up nudge)
send_private_message / edit_dm_message
  └─ status ready?  no → plaintext (unless peer pinned → error)
     yes → peer bundle (session cache → E2EE_FETCH_KEYS_REQ) → none & not pinned → plaintext
                                                            → none & pinned → refuse
                                                            → found → pin/compare → seal
DIRECT_MSG / DM_HISTORY_RES / DM_CONVERSATIONS_RES / DM_MESSAGE_EDITED
  └─ router hands the packet to the DM crypto worker (ordered mpsc) — it never
     awaits inside route_packets, because E2EE_FETCH_KEYS_RES arrives through the
     same loop. The worker opens each envelope (fetching a peer's historical key
     by key_id when needed, 5 s timeout) and emits the existing events with
     `encrypted` + `decryptError` ("", "locked", "no_key", "peer_key", "bad").
```

`e2ee_status_changed { status, keyId, fingerprint }` and
`e2ee_peer_changed { username }` are the two new events. After an unlock/setup the
renderer re-pulls conversations and invalidates loaded DM history so "locked"
placeholders are replaced by plaintext.

Renderer surface: Privacy tab section (state-driven: set up / unlock / change
passphrase / reset / fingerprint), a passphrase modal, a lock in the DM header with
a tooltip, a slim banner in the DM panel for *locked* / *not set up* / *peer has no
keys* / *peer key changed*, undecryptable rows rendered as a muted placeholder, a
lock glyph on encrypted bubbles, and the safety number in the profile popup.

## Non-goals (v1)

Forward secrecy; per-device keys; verified-contact state; encrypted attachments
(DMs have none); channel encryption; signed call keys (the identity keys make that
a small follow-up); metadata protection; deleting keys from central.

## Verification

- `cargo test --lib` — envelope round trip, tamper, reflection, wrong pair / wrong
  key id, bundle signature, backup round trip + wrong passphrase, keystore at-rest
  round trip, fingerprint/safety-number symmetry.
- `npx napi build`, renderer `tsc` — 0 errors.
- Community server rebuilds against the new proto (additive; no behaviour change);
  central verified by inspection + the mocked-pqxx g++ syntax check.
- Live: two accounts, set up on A and B, DM both ways, edit, reply, restart (store
  reopens), second device unlock with the passphrase, reset on one side → the
  other sees the key-change banner, old central → feature hidden.
