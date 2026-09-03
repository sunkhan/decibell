# Encrypted text channels — Design

**Date:** 2026-09-04
**Author:** sunkhan (with Claude)
**Status:** Implemented on `main` (messages); attachments in encrypted channels are a follow-up

## Problem

Community text channels are stored in the community server's SQLite in
plaintext. DMs, P2P calls and voice channels are end-to-end encrypted now; text
channels are the remaining plaintext content the host can read.

## Decisions

| Question | Decision |
|---|---|
| Opt-in or default | **Per-channel toggle, default off** (user decision: people who never open settings must keep server-side search when it ships). MANAGE_CHANNELS flips it; a lock shows on encrypted channels everywhere. |
| Protocol | **Channel epoch keys, escrowed to members.** Not MLS: members are mostly offline, history must stay readable to late joiners and on every device, and one identity per user would pin a channel to one device. |
| Who holds keys | Members only. A member who has the keys seals them to each entitled member's static X25519 identity (the DM envelope, content tag `0x02`) and the server stores those blobs per recipient. The server never sees a key. |
| Who is entitled | The server decides (it already decides who can VIEW a channel); members decide whether an identity is real (central's key directory + pins). Same split as voice. |
| Rotation | A new epoch on every membership removal (kick / ban / leave), coalesced client-side. Joins don't rotate: late joiners read history by design. |
| Enforcement | The server enforces the wire format: an encrypted channel refuses plaintext messages and attachments; a plaintext channel refuses envelopes. Switching on seals from that message on; switching off leaves sealed history sealed forever. |
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
  settings (with the search/attachment caveat), attach disabled in encrypted
  channels, placeholder rows + prompt when this device has no keys.

## Non-goals (v1)

Attachments in encrypted channels (client-side file encryption + client-side
thumbnails — next step; the server refuses them meanwhile); rotating on
permission-overwrite changes (a member who loses VIEW keeps the old epochs until
the next removal-driven rotation); search (no server search exists yet; encrypted
channels will need the local store); a batch key-fetch endpoint on central
(fills are paced instead).

## Verification

`cargo test --lib` (channel envelope round trip / tamper / wrong channel or
sender / epoch; blob seal/unseal), community e2e (toggle + audit, enforcement
both ways, envelope through broadcast/history/edit/reply preview, escrow:
create / fetch / needs / gap fill / stale epoch / viewer gating / CHANGED on
join and removal), tsc, community build, Windows check.
