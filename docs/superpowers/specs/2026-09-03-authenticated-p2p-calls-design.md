# Authenticated P2P calls — Design

**Date:** 2026-09-03
**Author:** sunkhan (with Claude)
**Status:** Implemented on `main`; needs the Hetzner central rebuild (same one as E2EE DMs) and a two-account live test

## Problem

P2P DM calls (`2026-08-28-p2p-dm-calls-design.md`) already seal every voice and
video datagram with AES-256-GCM under keys from a per-call ephemeral X25519
exchange — forward secret by construction. What they lacked was
**authentication of that exchange**: the ephemeral public keys ride through
central inside INVITE / ACCEPT, and central stamps `from`. A malicious central
(or anyone who owns it) could substitute its own keys in both directions and
decrypt the media in the middle, and neither side would notice. The calls spec
listed a safety number as the planned mitigation.

The E2EE DM identities (`2026-09-03-e2ee-dms-design.md`) are exactly the
missing ingredient: a long-term Ed25519 signing key per user, published on
central, TOFU-pinned by every peer, comparable out of band via fingerprints and
safety numbers.

## Decision

Each side **signs its ephemeral call key with its identity key**; the other side
**verifies against the peer's current, pinned identity before deriving any
media key**. Nothing changes about the media plane, the sockets, STUN, punching
or the key schedule.

```
sig = Ed25519(sign_priv, "decibell-call-auth-v1" ‖ 0 ‖ call_id ‖ 0 ‖ from ‖ 0 ‖ to ‖ 0 ‖ pub_key)
```

`call_id`, both usernames and the key are in the message, so a signature can't
be replayed into another call, reflected, or moved between pairs. `from` is what
central will stamp on the relayed packet; a central that rewrites either name
or the key breaks the signature.

### Policy (fail closed where it matters)

| Peer state (from central, pin-checked) | Their signal | Result |
|---|---|---|
| has an identity | valid signature by that identity | **Verified** |
| has an identity | no signature / wrong key_id / stale identity / bad signature | **Rejected** — `call_connect` fails, the call ends with "Couldn't verify X" |
| no identity | no signature | **Unverified** — allowed, shown as "Not verified" |
| no identity | any signature | Rejected — the server is lying about one of the two |
| central has no key endpoints at all | — | Unverified (feature-less central) |

"Current identity" goes through the same `resolve_peer_current` the DM path
uses, so a peer who reset their keys shows the same key-change banner and
re-pins; a signature by a *previous* generation is refused even if it verifies.
Verification only needs the peer's public key, so a device that hasn't set up
encryption itself still verifies signed calls (and sends unsigned ones — which
a peer who knows it has keys will refuse: set up encryption on every device).

## Wire

`CallSignal` gains `bytes pub_key_sig = 9` (64 B) and `uint32 key_id = 10`,
INVITE / ACCEPT only. Central relays them unchanged (it already copies the
packet) and caps the signature at 64 bytes like the key. No new packet types,
no schema change.

## Client

- Native `send_call_signal` signs when the device is *ready*
  (`session::sign_own_call_key`), otherwise sends unsigned.
- The router passes `pub_key_sig` / `key_id` through `call_signal` opaquely; the
  renderer keeps them on the pending call and hands them back to
  `call_connect` (`remotePubKeySig`, `remoteKeyId`).
- `call_connect` runs `session::verify_peer_call_key` before
  `call_crypto::derive`; a rejection returns an error (the renderer's existing
  "Couldn't connect the call" path ends the call), otherwise the outcome rides
  `call_connected.verified`.
- The call stage shows a **Verified** / **Not verified** pill next to the status
  once the call is live. The safety number in the profile popup is the same one
  DMs use — one identity, one number to compare.

## Non-goals

Verifying calls on a central without the E2EE endpoints (there is nothing to
verify against); a verified-contact state (follow-up shared with DMs);
community voice channels and stream watching (still the plaintext relay —
tracked as the HIGH item in the stream-watch audit; `MediaSocket::Sealed` is
the seam).

## Verification

`cargo test --lib` (call_auth sign/verify round trip; every bound field and a
foreign identity fail); tsc; community build + e2e on the new proto; central
syntax check. Live: A and B both set up → "Verified"; B without keys → "Not
verified"; B resets keys mid-day → next call from B shows the key-change
banner and still verifies under the new identity.
