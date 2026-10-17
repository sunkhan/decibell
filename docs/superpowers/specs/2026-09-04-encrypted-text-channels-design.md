# Encrypted text channels — Design

**Date:** 2026-09-04
**Author:** sunkhan (with Claude)
**Status:** Implemented on `main` (messages 2026-09-04, attachments 2026-09-04)

**Update 2026-10-15 (users by uid, stage 4):** Message bodies (header byte `0x04`) and key blobs (v2 DM envelopes) now bind the account uid instead of the name, so a rename changes nothing. See `2026-10-15-uid-identity-design.md` §Stage 4 for the v2 formats. v1 bodies (`0x02`) stay readable with `sealed_sender`.

## Problem

Community text channels are stored in the community server's SQLite in
plaintext. DMs, P2P calls and voice channels are end-to-end encrypted now; text
channels are the remaining plaintext content the host can read.

## Decisions

| Question | Decision |
|---|---|
| Opt-in or default | **Per-channel, default off** (user decision: people who never open settings must keep server-side search when it ships). **Chosen at creation and fixed** since 2026-10-14 (see below). A lock shows on encrypted channels everywhere. |
| Protocol | **Channel epoch keys, escrowed to members.** Not MLS: members are mostly offline, history must stay readable to late joiners and on every device, and one identity per user would pin a channel to one device. |
| Who holds keys | Members only. A member who has the keys seals them to each entitled member's static X25519 identity (the DM envelope, content tag `0x02`) and the server stores those blobs per recipient. The server never sees a key. |
| Who is entitled | The server decides (it already decides who can VIEW a channel); members decide whether an identity is real (central's key directory + pins). Same split as voice. |
| Rotation | A new epoch on every membership removal (kick / ban / leave), coalesced client-side. Joins don't rotate: late joiners read history by design. |
| Enforcement | The server enforces the wire format: an encrypted channel refuses plaintext messages and binds only sealed uploads; a plaintext channel refuses envelopes and sealed uploads. A sealed message can never be edited into plaintext. |
| Strict | No keys → placeholders and a prompt; no fallback. |

## Wire

```
ChannelInfo.encrypted = 12          ChannelUpdateRequest.encrypted = 9 (optional)
ChannelMessage.envelope = 13, reply_to_envelope = 14
MessageEditReq.envelope = 4         ChannelMessageEdited.envelope = 6
CHANNEL_KEYS_REQ = 142  {channel_id}
CHANNEL_KEYS_RES = 143  {channel_id, encrypted, current_epoch, blobs[{epoch, sender, blob}] (mine),
                         needs[{username, epochs[]}] (viewers missing blobs), members[] (viewers)}
CHANNEL_KEYS_PUBLISH_REQ = 144 {channel_id, epoch, blobs[{recipient, blob}]}
CHANNEL_KEYS_PUBLISH_RES = 145 {channel_id, success, message, current_epoch}
CHANNEL_KEYS_CHANGED = 146 {channel_id, current_epoch, filler, rotate}   (community→viewers)
```

- `PUBLISH` with `epoch == current + 1` creates the epoch (at least one blob);
  `epoch <= current` fills gaps (only missing `(epoch, recipient)` rows, never
  overwrites). Recipients who can't VIEW are dropped. Caps: 500 blobs, 2 KiB each.
- `CHANNEL_KEYS_CHANGED` names a **filler**: the lowest online username holding a
  current-epoch blob. Sent on a new epoch, when a member (re)joins and lacks
  blobs (`rotate = false`), and on removal (`rotate = true`, the removed
  member's blobs are deleted).
- Tables: `channel_key_epochs (channel_id, epoch, created_by, created_at)`,
  `channel_key_blobs (channel_id, epoch, recipient, sender, blob, created_at)`,
  `channels.encrypted`, `messages.envelope`.

### Escrow hardening (review S14, 2026-10-17)

The pre-1.0 review found the escrow open to any viewer: unlimited epochs, garbage
blobs a newcomer could never replace, and a reply that grew past the client's
2 MiB frame cap. The rules now:

- **Who may mint an epoch**: the first one, the rotation the server asked for after a
  removal (by a holder of the current epoch), or the next one when **nobody holds the
  current epoch** any more (`ChannelKeysRes.current_orphaned`: its holders left or
  dropped it, so nobody could share or rotate it), by any viewer. Encrypted channels
  only; one transaction per publish. Fills come only from holders of that epoch.
- **Key check**: a creation carries `key_check` = HMAC-SHA256(key_e,
  `"decibell-channel-key-check-v1" ‖ 0 ‖ channel_id ‖ 0 ‖ epoch u32 LE`), 32 bytes,
  stored on the epoch (`channel_key_epochs.key_check`, schema v17) and returned with
  every blob. A blob whose key fails it is wrong, whoever sealed it; epochs minted
  before it existed have none and open on the envelope alone.
- **Dropping a blob** (`CHANNEL_KEYS_REJECT = 165 {channel_id, epochs[], bad}`, no
  reply): the member's own rows go, they show up in `needs` again, and a filler is
  named. The client sends it for a blob that is
  - *stale*: sealed to a generation of their key older than their current one (they
    reset), nobody's fault; or
  - *bad*: it doesn't authenticate, binds another channel or epoch, or fails the key
    check. The server then records the sender (`channel_key_refusals`): their blobs to
    that member are dropped, they're never named filler on that member's behalf, and
    that member is left out of their `needs`.

  A blob that doesn't open for any other reason (locked, the sender's key couldn't be
  fetched, a key generation newer than this device holds) is retried on the next fetch,
  never dropped. A filler looks each recipient's current keys up again before sealing to
  them, so a member who reset gets a blob for their new keys even from a filler that
  cached the old ones (`E2EE_KEYS_CHANGED` reaches watchers only).
- **Owed rotations are stored** (`channel_key_rotations`): a removal marks every channel
  with an epoch, and the mark stays until a new epoch exists. When no holder was online
  at the removal (or the named filler never rotated), the next holder to sign in is sent
  `CHANNEL_KEYS_CHANGED{rotate}`. They used to live in memory, lost with a restart or
  an offline filler.
- **Paging**: `ChannelKeysReq.after_epoch` / `needs_after_uid`. The first page (both 0)
  carries the viewers (≤ 1000), the needs in uid order (≤ 200 viewers, ≤ 4096 epochs)
  and the caller's first 256 blobs; `more_blobs` / `more_needs` are continued by a
  request with that cursor alone (the last blob's epoch / the last need's uid), whose
  reply holds only that list. The reply echoes both cursors, which is how the client
  routes it.
- Residual: someone dropping a *stale* blob can be refilled with garbage by a holder
  who races the named filler (stale refuses nobody); a bad blob after that refuses the
  sender. Pinned identities and the key check mean a member never accepts a wrong key.

## Message envelope

```
key_e   = the channel's epoch key (32 B random, minted by whoever creates the epoch)
salt    = 32 random bytes per message
okm     = HKDF-Expand(HKDF-Extract(salt, key_e), "decibell-channel-v1" ‖ 0 ‖ channel_id ‖ 0 ‖ sender, 44)
header  = 0x02 ‖ epoch u32 LE ‖ salt
wire    = header ‖ AES-256-GCM(okm.key, okm.nonce, 0x01 ‖ utf8, aad = header ‖ channel_id ‖ sender) ‖ tag
```

Binding the server-stamped sender and the channel into the key and the AAD
means a message can't be relabelled or moved. Old epochs stay decryptable as
long as the member holds their blob — every member holds every epoch they are
entitled to.

## Client

- `e2ee/channel_keys.rs`: per `(server, channel)` keyring (`epoch → key`,
  current epoch, viewers). `ensure` fetches and unseals our blobs, creates epoch 1
  when the channel has none, fills gaps the server reports, and rotates when
  named as filler with `rotate`. Peer bundles come through the DM path
  (`resolve_peer_current`, pins, key-change notices), paced to central's key
  bucket.
- Channel packets go through an ordered crypto worker (like DMs); plaintext rows
  pass straight through it. `send_channel_message` / `edit_channel_message`
  take `encrypted` from the renderer's `ChannelInfo` and seal.
- Renderer: lock on encrypted channels in the sidebar, the toggle in channel
  settings (with the search caveat), placeholder rows + prompt when this device
  has no keys.

## Attachments

The server never processes attachment content (it stores bytes, serves Ranges,
and keeps client-made thumbnails), so encrypting attachments is a client-side
transform around the existing upload / fetch paths plus one flag.

- **Per-file key.** The renderer draws a random 32-byte key per upload
  (`features/chat/attachmentCrypto.ts`, WebCrypto). Content is sealed in 64 KiB
  chunks: chunk *i* = AES-256-GCM(key, nonce `u64le(i) ‖ 00 00 00 01`, plain,
  aad `"decibell-att-v1" ‖ 0x00 ‖ u64le(i)`) ‖ tag; the ciphertext is the
  concatenation, so plaintext byte *p* lives in sealed chunk ⌊p / 64 KiB⌋ and a
  `<video>` Range probe maps to one upstream Range of whole chunks. Thumbnails
  (still generated client-side) are sealed whole under nonce
  `u64le(sizePx) ‖ 00 00 00 02`, aad `"decibell-att-v1" ‖ 0x01 ‖ u64le(sizePx)`.
  Deterministic nonces are safe because the key is single-use; they keep a
  retried chunk byte-identical.
- **What the server sees.** `/attachments/init` takes `encrypted: true` plus
  the *kind* (retention is per kind) and the ciphertext size; filename is
  "encrypted", mime octet-stream, width / height / duration / placeholder are
  zeroed server-side whatever the client sent. The row carries
  `attachments.encrypted` and the wire `Attachment.encrypted` (field 17).
  `bind_attachments` binds only uploads whose flag matches the channel's, so a
  plaintext upload never lands in an encrypted channel and vice versa.
- **Where the metadata goes.** The real filename / mime / size / dimensions /
  duration / ThumbHash / chunk size / thumbnail set and the key ride the message
  envelope: content tag 0x03 = prost `EncryptedMessageBody { text, attachments:
  [EncryptedAttachmentMeta] }` (tag 0x01 stays the text-only body). Native opens
  it and emits `encryptedAttachments` on message / history / edit events; the
  renderer merges the metadata into the `Attachment` (`encrypted`, `keyB64`,
  `chunkBytes`) and registers the keys with main
  (`decibell:attachments:registerKeys`, per session, never persisted). An edit
  re-seals the body with the same metadata.
- **Fetch paths.** Main decrypts in every path that hands bytes to the renderer
  (`electron/main/attachmentFetch.ts`): the `decibell-attachment://` protocol
  (images, thumbnails), the loopback media server (`<video>` / `<audio>`, with
  real 200 / 206 semantics: the covering chunks are fetched, opened, trimmed)
  and `netFetch` (save-as). Plain attachments proxy exactly as before.
- **Not hidden:** attachment count, kind, ciphertext size and thumbnail
  presence / sizes (the server needs kind for retention and sizes for storage).

## Non-goals (v1)

Rotating on
permission-overwrite changes (a member who loses VIEW keeps the old epochs until
the next removal-driven rotation); search (no server search exists yet; encrypted
channels will need the local store); a batch key-fetch endpoint on central
(fills are paced instead).

**Creation-time only (2026-10-14, owner decision).** The flag used to be a
MANAGE_CHANNELS switch on existing channels (`ChannelUpdateRequest.encrypted`,
now `reserved 9`). Switching left a mixed history and broke in several ways:
- Switching off let an edit of a sealed message go out in plaintext. The server
  rewrote the envelope, which also erased the message's attachment keys forever.
- After switching off, escrow stopped filling keys for new members
  (`channel_keys_on_join` only walks encrypted channels), so they couldn't read
  the sealed history.
- Switching on kept the plaintext history under the lock badge and locked out
  members without encryption mid-conversation.
- At the moment of the switch, sends and uploads in flight bounced or were
  silently dropped.

Now `ChannelCreateRequest.encrypted` sets the flag for the channel's life (text
channels only; audited as `channel_create` with `encrypted`). Channel settings
shows it read-only. For channels toggled before this change, the server
refuses a plaintext edit of a row that has an envelope, and the client doesn't
offer Edit on such messages.

Accepted metadata: the channel name and, since 2026-10-14, its topic stay
in the clear on the server. This is an owner decision: they label the channel, not
the conversation. Sealing the topic would leave keyless members and an
encryption switch-on with a mixed state, and the name beside it would still be
readable. The channel-settings topic field says "Topics aren't encrypted. The
server host can read them." on encrypted channels.

## Verification

`cargo test --lib` (channel envelope round trip / tamper / wrong channel or
sender / epoch, tagged bodies; blob seal/unseal), community e2e (toggle + audit,
enforcement both ways, envelope through broadcast/history/edit/reply preview,
escrow: create / fetch / needs / gap fill / stale epoch / viewer gating / CHANGED
on join and removal; sealed uploads: init stand-ins + kind check, binding rule
both ways, `encrypted` on broadcast and history), a node cross-check that the
renderer's sealing and main's opening agree (sizes 0 … 3 chunks, ranges, tamper,
thumbnails), tsc (web + node), community build, Windows check.
