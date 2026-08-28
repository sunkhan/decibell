# P2P DM Calls — Design

**Date:** 2026-08-28
**Author:** sunkhan (with Claude)
**Status:** Implemented (M1–M4 on `main`); live two-machine test pending

## Problem

Voice and screen streaming only existed inside community voice channels: the
native `VoiceEngine` binds two UDP sockets `connect()`ed to a community server
(`port+1` voice, `port+2` media), identifies itself with the last 31 chars of
the community JWT, and the C++ community server is a plain relay. Two people
in a DM had no way to talk or share a screen without one of them hosting a
community. Central (TCP/TLS 8080, single io thread, synchronous Postgres) has
no UDP plane at all, and nothing in the tree did NAT traversal or media
encryption.

## Decisions

| Question | Decision |
|---|---|
| Transport | **Extend the native UDP engine** — keep the cpal / RNNoise / AEC3 / Opus pipeline, the WebCodecs + native-NVENC stream encoder and the `StreamVideoPlayer` receive path; add NAT traversal + AEAD in Rust. (Chromium WebRTC was the alternative: it would have meant a second audio pipeline and Chromium's encoder for calls.) |
| Relay | **STUN-only v1.** No TURN. A pair that can't hole-punch gets a clear "couldn't connect" error. The STUN list comes from central (`DECIBELL_STUN_SERVERS`) with a client default, so adding a relay later is a config change. |
| Concurrency | **Mutually exclusive** with community voice: one native engine, one mic. Starting/accepting a call leaves the channel; joining a channel hangs up. |
| Who can call | **DM rules** — central's `check_dm_allowed` (blocked → refused; callee friends-only → friends only; otherwise anyone online). |

## Non-goals (v1)

- TURN / relayed media; IPv6 candidates; group calls; webcam video;
  missed-call persistence (an INVITE to an offline user just fails); a
  safety-number display for the key exchange.

## Architecture

```
 caller                      central (TCP/TLS, relay only)                 callee
 ──────                      ──────────────────────────────                ──────
 call_prepare                                                              
   bind voice+media sockets                                                
   STUN both (parallel) + host IPs                                         
   X25519 keygen                                                           
 CALL_SIGNAL INVITE {pub_key, candidates} ──▶ stamp from, bucket,          
                                              check_dm_allowed,           
                                              send_private ─────────────▶ setIncoming, ring
                                    ◀──────── RINGING ◀────────────────── 
                                                                           accept: call_prepare
                                    ◀──────── ACCEPT {pub_key, candidates} call_connect
 call_connect                                                              
   HKDF(X25519) → 4 keys; seal both sockets                                
   punch: sealed PINGs at every candidate ◀═══ sealed UDP, both sockets ═══▶ punch
   first datagram that OPENS = live path; 300 ms grace prefers LAN         
 VoiceEngine::start_p2p ── call_connected                                  call_connected
        ◀═══════════ AUDIO / STREAM_AUDIO / PING · VIDEO / FEC / PLI / NACK ═══════════▶
 STREAM_START / STREAM_STOP over central announce an in-call screen share  
 HANGUP / CANCEL / REJECT / BUSY end it; PEER_OFFLINE / NOT_ALLOWED come from central
```

### Signaling (central)

`Packet.Type CALL_SIGNAL = 128`, payload `CallSignal call_signal = 130`
(`proto/messages.proto`): `kind` (INVITE, RINGING, ACCEPT, REJECT, BUSY,
CANCEL, HANGUP, PEER_OFFLINE, NOT_ALLOWED, STREAM_START, STREAM_STOP),
`call_id` (caller-minted UUID), `from` (central-stamped), `to`, `pub_key`
(32-byte X25519, INVITE/ACCEPT), `repeated CallCandidate candidates`
(`{socket: VOICE|MEDIA, kind: HOST|SRFLX, ip, port}`, ≤16), `CallStreamMeta
stream` (STREAM_START), `timestamp`. `LoginResponse` gains
`repeated string stun_servers = 5` and `bool call_signaling = 6`.

Central (`src/server/main.cpp`): authenticated only; per-session token bucket
(20 burst / 5 per s — central had no rate limiting and one io thread with
synchronous Postgres); size caps; `set_from(username_)`; on **INVITE only**
`check_dm_allowed` (the one DB touch); `send_private(routed, to)`, and when
that returns false a `PEER_OFFLINE` reply. Replies are written from the
recipient's POV (`from = peer, to = us`) like `DM_MESSAGE_DELETED`. Nothing is
persisted. No candidates trickle: they ride INVITE and ACCEPT (gathering takes
≤2.5 s; the callee gathers only after clicking Accept).

### Transport (`native/src/media/media_socket.rs`)

`MediaSocket` wraps each UDP socket. `Plain` is a byte-identical passthrough
used by the community path. `Sealed` is the P2P transport:

- Outer envelope `[0xE5][u64 LE counter][ciphertext][16 B GCM tag]` — 25 B
  overhead; nonce = 4-byte direction salt ‖ 8-byte counter; AAD = the 9-byte
  header; 1024-bit replay window checked only after authentication. `0xE5`
  has its top bits set so a late STUN response (top bits `00`) never matches.
- The inner plaintext is the **unchanged** existing datagram, so
  `pipeline.rs`, `VideoReceiver`, PING/RTT, NACK, PLI and FEC work untouched.
  Worst-case sealed datagram is 45 + 1200 + 25 = 1270 B for video and
  37 + 1200 + 25 = 1262 B for audio (≤ 1280).
- Peer address is *learned* from the source of the last authenticated
  datagram (the community relay's endpoint-learning, made safe by AEAD) and
  migrates after 2 s of silence — no `connect()`, so the two sides can never
  wedge on different candidate choices.
- Peer PINGs are reflected re-sealed and never surfaced; our own echo is
  returned — the pipeline's RTT and 3 s keepalive logic is unchanged and the
  reflection doubles as the punch responder.
- `sender_id` in P2P mode is the **own username** (no relay rewrite; usernames
  are ≤32 chars = `SENDER_ID_SIZE`, the same slot the relay stamps).

Keys (`call_crypto.rs`): ephemeral X25519 per call → `HKDF-SHA256(salt =
call_id, ikm = shared, info = "decibell-call-v1|<lower>|<higher>")` → four
(key, salt) pairs (voice/media × a→b/b→a; A = lexicographically lower
username). Both sides derive mirrored material; the first datagram that opens
is the confirmation. `ring` (already in the tree via rustls) provides
X25519/HKDF/random.

### NAT traversal (`stun.rs`, `punch.rs`)

Minimal RFC 5389 Binding client (XOR-MAPPED-ADDRESS, MAPPED-ADDRESS
fallback, RFC 5769 vectors in tests), sent from the socket that will carry
media, retransmits at 0/300/900/2100 ms, 2.5 s budget, two sockets queried in
parallel. Host candidates from `if-addrs` (LAN + VPN + bridges, loopback and
link-local dropped, cap 6) with the default-route trick as fallback.

Punch, per socket, on the blocking pool: sealed PINGs sprayed at every remote
candidate at 50 ms ×10 → 200 ms ×10 → 500 ms; any datagram that opens
validates its source; after the first validation a 300 ms grace window lets a
private/host path displace a reflexive one; 10 s → `NoPath`. Both sockets
must succeed. Cone NATs work because the reflexive mapping belongs to this
socket and is reused for every destination; symmetric NATs allocate a new
port per destination, so the STUN-seen port is wrong — that pair fails with
the documented copy.

### Native commands (`native/src/commands/call.rs`)

`get_call_config`, `send_call_signal`, `call_prepare({callId, peer}) →
{pubKey, candidates}`, `call_connect({callId, peer, remotePubKey,
remoteCandidates})` (returns immediately; `call_connected` / `call_failed`
follow; a 1 s watchdog emits `call_dropped` after 15 s without an
authenticated datagram), `call_end`, `call_watch_stream({watch})`.
`AppState.pending_call` / `active_call`. `start_screen_share` /
`stop_screen_share` take optional community ids and skip the community
announcement inside a call; `join_voice_channel` refuses while a call is
active; logout clears the call.

### Renderer

`callStore` (idle → outgoing / incoming → connecting → active) +
`voiceStore.callPeer` (the media session runs with no channel; every
component that used to gate on `connectedChannelId` gates on "in a session").
`features/call/`: `callActions.ts` (start / accept / decline / end, ringtone
loop, 45 s timeout, `leaveCommunityVoiceIfAny`, stream announce/watch),
`useCallEvents.ts` (signal state machine incl. BUSY and glare — the lower
username's INVITE wins), `IncomingCallModal`, `CallPanel` (inside the DM:
tiles with speaking rings, duration · RTT · LAN/direct, mute / deafen /
share / hang up, "Watch <peer>'s screen"). In-call streams reuse the whole
player stack; `DmChatPanel` overlays `StreamViewPanel` while one is focused
and `MiniStreamPlayer` hides behind it / expands back into the DM.

## Security notes

- Media is end-to-end sealed between the two clients. Central authenticates
  both identities and relays the ephemeral public keys, so a malicious central
  could substitute keys (MITM); a safety number (hash of both public keys)
  shown in the call panel is the planned mitigation.
- Only key holders can produce a datagram that opens; the learned-peer rule
  therefore can't be hijacked by a spoofed source, and replays are dropped.
- `MediaSocket::Sealed` is the seam for closing the tracked HIGH item on the
  community plane (plaintext, unauthenticated relay): negotiate a per-session
  key over the TLS control channel and wrap the community sockets too.

## Verification

- `cargo test --lib` in `electron-client/native`: seal/open round trip,
  tamper, replay window, PING reflection, peer learning, mirrored key
  derivation, STUN vectors, loopback two-socket punch, timeout, abort.
- `tsc` 0 errors; `napi build`; community e2e unchanged (proto regen only);
  central `g++ -fsyntax-only` against the regenerated pb with a stub pqxx.
- **Pending live:** two machines on one LAN (expect `path: host`), two
  networks (expect `srflx`), a symmetric-NAT pair (clean failure), community
  voice regression, mutual exclusion both ways, central reconnect mid-call
  (media keeps flowing), in-call screen share both directions incl. Linux
  PipeWire tap / Windows WASAPI stream audio, PLI round trip, mini player.

## Rollout

Client-first is safe: an old central ignores `CALL_SIGNAL` and leaves
`call_signaling` false, which keeps the Call button disabled ("Calls aren't
available on this server yet"). Rebuild central on the Hetzner box (protoc
regen) and optionally set `DECIBELL_STUN_SERVERS`.
