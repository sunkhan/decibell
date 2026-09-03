# MLS-encrypted voice channels and streams — Design

**Date:** 2026-09-03
**Author:** sunkhan (with Claude)
**Status:** Implemented on `main` in two steps (M1 group + voice, M2 streams); needs a community-server release and a multi-client live test

## Problem

Community voice channels and screen streams are relayed by the community server
as plaintext UDP: Opus frames and encoded video in the clear, a JWT-tail sender
id that is never re-verified. Whoever runs a community, or reaches its relay
port, can listen to every channel and watch every stream. DMs and P2P calls are
end-to-end encrypted now; the relay path is the last plaintext media plane.

## Decisions

| Question | Decision |
|---|---|
| Group key agreement | **MLS (RFC 9420) via OpenMLS 0.9**, one group per voice-channel session. Members join by external commit against a GroupInfo the server holds; membership changes are commits; every epoch's exporter secret derives the media keys. Chosen over pairwise sender-key distribution for logarithmic membership changes, forward secrecy across epochs, post-compromise security, and a standardized, analyzed protocol that group text channels can reuse. |
| Identity | The E2EE identity from the DM work: the Ed25519 signing key is the MLS credential's signature key, the username is the BasicCredential identity. Ciphersuite `MLS_128_DHKEMX25519_AES128GCM_SHA256_Ed25519`. |
| Who is trusted with the roster | The community server (it always knew who is in which channel). It is the MLS *delivery service*: sequences commits, stores the current GroupInfo, forwards to channel participants. It never sees a key. |
| Identity binding | Every member verifies every leaf's `(identity, signature_key)` against central's key directory after each merge. An unverified leaf **quarantines** the epoch (nothing is sealed under it) and the elected member commits its removal. A server can therefore add a participant to the roster but not a fake identity to the group. |
| Policy | **Strict.** A device without E2EE keys cannot join a voice channel (client-side refusal with a set-up prompt). Receivers ignore plaintext media types in a channel session. No fallback. |
| What stays in the clear | Every existing packet header (type, sender id, sequence, frame id, packet index, keyframe flag, codec) — the relay needs them. Control packets (PING, KEYFRAME_REQUEST, NACK, FEC) are unchanged. Stream thumbnails stay plaintext JPEG so non-members can see what a stream is about (user decision). |
| Where the crypto runs | Native. The renderer sees a per-channel encryption state and nothing else. |

## MLS group lifecycle

```
join voice ─► VOICE_PRESENCE_UPDATE ─► MLS_GROUP_INFO_REQ
   none  ─► MlsGroup::new (epoch 0) ─► MLS_GROUP_CREATE_REQ {group_info}
   some  ─► external commit against group_info ─► MLS_COMMIT_REQ {epoch+1, commit, group_info}
              ok       ─► merge_pending_commit ─► export media keys
              rejected ─► discard, re-fetch GroupInfo, retry (bounded)
MLS_COMMIT_BROADCAST (another member's commit) ─► process_message ─► verify leaves ─► merge ─► keys
   any processing error ─► resync: rejoin by external commit
roster change (presence): MLS members absent from the roster ─► the elected member
   (lowest username among online members) commits Remove after a 3 s grace
leave voice ─► drop the group; the others remove our leaf
```

- The server enforces `commit.epoch == current + 1`, forwards an accepted commit to
  every channel participant except the committer, and stores the committer's
  GroupInfo (the commit bundle's GroupInfo carries the ratchet tree and the
  external-init key when `use_ratchet_tree_extension` is on). It drops the group
  when the channel empties.
- Wire format policy is pure plaintext: MLS handshake messages carry no secrets
  and travel inside TLS; the server already knows the roster.
- Group state lives in memory for the session (OpenMLS memory storage). Rejoin is
  the universal recovery; there is nothing to persist.
- Two devices with one identity cannot both be in a group (MLS requires unique
  signature keys per leaf; an external join with the same identity removes the
  other leaf). One device per user per voice channel — the relay could never tell
  them apart anyway.

## Media keys and framing

```
exporter = MlsGroup::export_secret("decibell-media-v1", context = channel_id, 32)   # per epoch
K(member, kind) = HKDF-Expand(exporter, "decibell-media-key" ‖ 0 ‖ username ‖ 0 ‖ kind, 32)
kinds: "voice" | "stream-audio" | "video"
```

Every member derives every sender's keys locally; nothing is distributed. Keys
for the previous epoch are kept for 2 s so in-flight packets still open.

Sealed packet types (`udp_packet.hpp`, relayed exactly like their plaintext twins):

| Type | Plain twin | Payload |
|---|---|---|
| `AUDIO_SEALED` = 7 | AUDIO 0 | `[epoch u32][counter u64][AES-256-GCM(ct)][tag]` — 28 B overhead |
| `STREAM_AUDIO_SEALED` = 8 | STREAM_AUDIO 6 | same framing, kind "stream-audio" |
| `VIDEO_SEALED` = 9 | VIDEO 1 | chunks of one **sealed frame** `[epoch u32][stream_salt u32][ct][tag]` — 24 B per frame |

- Audio: nonce = counter (per sender, per epoch, per kind), AAD = `type ‖ username ‖ sequence`.
- Video: sealed once per encoded frame before chunking (like Discord's DAVE), nonce =
  `stream_salt ‖ frame_id`; `stream_salt` is random per stream start so a restarted
  frame counter never reuses a nonce within an epoch. AAD = `type ‖ username ‖
  frame_id ‖ is_keyframe ‖ codec`. The HEVC/AV1 decoder-description prefix is
  inside the sealed frame. FEC XORs the sealed chunks and reconstructs a sealed
  chunk; NACK retransmits sealed chunks — both untouched.
- The relay rewrites the sender id from the JWT tail to the username; the AAD uses
  the username on both ends, so the rewrite is part of what is authenticated.

## Wire (proto)

```
MLS_GROUP_INFO_REQ    = 136  client→community {channel_id}
MLS_GROUP_INFO_RES    = 137  community→client {channel_id, exists, epoch, group_info}
MLS_GROUP_CREATE_REQ  = 138  client→community {channel_id, group_info}         → MLS_COMMIT_RES
MLS_COMMIT_REQ        = 139  client→community {channel_id, epoch, commit, group_info}
MLS_COMMIT_RES        = 140  community→client {channel_id, success, message, epoch}
MLS_COMMIT_BROADCAST  = 141  community→participants except sender {channel_id, epoch, sender, commit}
```

Server: `mls_groups_[channel_id] = {epoch, group_info}`; handlers require the
sender to be in that voice channel; a new `mls` token bucket (20 burst / 5 per s);
blobs capped at 256 KiB. Voice-socket relay treats 7 like 0 and 8 like 6 (endpoint
learning, server-mute, deafen skip, watcher fan-out); media-socket relay treats 9
like 1.

## Client

- `e2ee/group.rs`: the MLS session driver (a tokio task per voice session) — owns
  the OpenMLS provider and group, reacts to presence / server replies / broadcasts,
  verifies leaves, elects the remover, exports keys into a lock-free `KeyRing`
  (`media/frame_crypto.rs`) the pipelines read.
- `media/frame_crypto.rs`: `KeyRing` (current + previous epoch, per-member keys)
  and seal/open for the three kinds; the audio pipeline, stream-audio pipeline,
  video sender and video receive thread call it. Community sessions send nothing
  until the ring has an epoch and drop plaintext media types.
- `join_voice_channel` refuses when E2EE isn't ready; the renderer checks first
  and opens the passphrase modal.
- Event `voice_e2ee_state {serverId, channelId, state: joining|ready|resyncing|quarantine, epoch, unverified[]}`
  drives a lock badge in the voice panel header and a warning toast on quarantine.

## Non-goals

Verifying leaves on the server (would need a community→central key lookup —
follow-up); encrypted thumbnails (user decision); the UDP endpoint-hijack item
from the stream-watch audit (a stolen JWT tail can still rebind a session's
endpoint; it can no longer listen); persisting group state across restarts.

## Verification

`cargo test --lib`: frame_crypto round trips per kind, tamper, epoch mismatch,
previous-epoch grace; an in-process two- and three-member MLS session test that
simulates the delivery service (create, external join, remove, key agreement,
stale-epoch rejection). Community e2e: group create/info, epoch rule, broadcast
excludes the committer, participant gating, sealed relay types, group dropped on
empty. tsc, community build, Windows Native Check. Live: three clients in one
channel, leave/rejoin, stream start mid-session, a client without keys refused.
