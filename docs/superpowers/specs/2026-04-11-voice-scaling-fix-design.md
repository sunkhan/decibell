# Voice Pipeline Scaling Fix — Design Spec

**Date:** 2026-04-11
**Problem:** Voice chat degrades catastrophically (robotic, cutting off) when 3+ users are in a channel, especially when one user is streaming. The system enters a degraded state and never self-recovers.

## Root Causes

1. **Server: single UDP socket processes voice and video serially.** Video packets (~300/sec at 1445 bytes each during streaming) compete with voice packets (~150/sec at ~200 bytes) for the same `async_receive_from` chain. Video dominates, delaying voice packet processing. Audio packets queue in the OS socket buffer behind video, arriving late at clients.

2. **Video packets are fixed-size (1445 bytes) regardless of payload.** `UdpVideoPacket::to_bytes()` copies the entire struct including unused payload bytes. Wastes ~30% bandwidth.

3. **Client: single UDP socket shared between voice and video.** The recv thread classifies and dispatches all packet types. Under load, video recv can delay voice recv.

4. **Jitter buffer never self-recovers from degraded state.** Once PLC (Packet Loss Concealment) starts firing continuously due to overload, the jitter buffer's drain timer accumulates debt. Even after the overload ends, it stays in a degraded state producing constant PLC — robotic sound persists indefinitely.

## Architecture Change

### Server: Before
```
Client A ──UDP──▶ [single socket port TCP+1] ──▶ do_receive_udp()
                   handles: AUDIO, VIDEO, FEC, STREAM_AUDIO, PING, KEYFRAME_REQUEST, NACK
                   serial processing, one packet at a time
```

### Server: After
```
Client A ──UDP──▶ [voice socket port TCP+1] ──▶ do_receive_voice_udp()
                   handles: AUDIO, STREAM_AUDIO, PING

Client A ──UDP──▶ [media socket port TCP+2] ──▶ do_receive_media_udp()
                   handles: VIDEO, FEC, KEYFRAME_REQUEST, NACK
```

Both sockets run on the same single-threaded `io_context`. Boost.Asio interleaves their handlers, but crucially, voice and video have independent OS socket receive buffers — a burst of video packets can never delay reading the next voice packet.

### Client: Before
```
[single Arc<UdpSocket>] ──▶ one recv thread ──▶ classify by type ──▶ audio_pkt_tx / video_tx
```

### Client: After
```
[voice Arc<UdpSocket>] ──▶ voice recv thread ──▶ audio_pkt_tx (sync_channel, cap 256)
[media Arc<UdpSocket>] ──▶ media recv thread ──▶ video_tx (mpsc)
```

## Detailed Changes

### 1. Server — Separate UDP Sockets

**File: `src/community/main.cpp`**

- `CommunityServer` gets a second UDP socket: `media_udp_socket_` on port TCP+2, with its own 2MB send/receive buffers.
- New `do_receive_media_udp()` — async receive chain for video/FEC/NACK/keyframe-request packets. Same structure as current `do_receive_udp()` but only handles media packet types.
- Rename current `do_receive_udp()` to `do_receive_voice_udp()`. Remove handling of VIDEO, FEC, KEYFRAME_REQUEST, and NACK packet types from it. It only handles AUDIO, STREAM_AUDIO, and PING.
- `SessionManager` gets `media_udp_socket_ptr_` in addition to existing `udp_socket_ptr_`.
- Each `Session` stores two UDP endpoints: `udp_voice_endpoint_` (existing, renamed) and `udp_media_endpoint_` (new). Updated when packets arrive on each socket.
- `broadcast_to_voice_channel()` sends via voice socket (existing behavior).
- `broadcast_to_watchers()` sends via media socket (new).
- `relay_keyframe_request()` and `relay_nack()` send via media socket.
- Console log at startup: "Community Server Voice UDP on port X, Media UDP on port X+1".

### 2. Compact Video Packets

**File: `tauri-client/src-tauri/src/media/video_packet.rs`**

- `UdpVideoPacket::to_bytes()` — send only header (45 bytes) + actual payload (`payload_size` bytes), not the full 1445-byte struct. Matches the pattern already used by `UdpAudioPacket::to_bytes()`.
- `UdpVideoPacket::from_bytes()` — already reads `payload_size` and copies only that many bytes. No change needed.
- `UdpFecPacket::to_bytes()` — same treatment. Header + actual XOR payload only.

**File: `src/common/udp_packet.hpp`**

- No struct changes. The C++ structs stay fixed-size for recv buffer allocation. The server receives into the full-size buffer and broadcasts `bytes_recvd` (actual received length), so compact packets are forwarded correctly without any C++ changes.

**Bandwidth impact:**
- Typical video packet payload: ~1000 bytes
- Before: 1445 bytes/packet. After: 1045 bytes/packet. **~28% reduction.**
- At 300 video packets/sec to 2 watchers: 867 KB/s → 627 KB/s saved.

### 3. Client — Separate UDP Sockets

**File: `tauri-client/src-tauri/src/media/pipeline.rs`**

- Create two UDP sockets at pipeline startup:
  - `voice_socket: Arc<UdpSocket>` — connects to server port TCP+1 (existing port)
  - `media_socket: Arc<UdpSocket>` — connects to server port TCP+2 (new port)
- Two recv threads instead of one:
  - `voice_recv_thread()` — reads from voice socket, forwards all packets to `audio_pkt_tx` (existing sync_channel). Handles AUDIO, STREAM_AUDIO, PING.
  - `media_recv_thread()` — reads from media socket, forwards all packets to `video_tx` (existing mpsc channel). Handles VIDEO, FEC, KEYFRAME_REQUEST, NACK.
- Each recv thread is simpler than the current combined one — no packet type classification needed, just forward everything.
- Main loop sends voice packets via `voice_socket`.
- NACK and keyframe request sends use `media_socket`.

**File: `tauri-client/src-tauri/src/media/audio_stream_pipeline.rs`**

- Stream audio packets sent via the voice socket (passed in at construction).

**File: `tauri-client/src-tauri/src/media/video_pipeline.rs`**

- Video and FEC packets sent via the media socket.

**File: `tauri-client/src-tauri/src/media/mod.rs`**

- NACK and keyframe request packets sent via the media socket.
- The media socket is passed to the video recv/reassembly thread (which currently receives the shared `Arc<UdpSocket>`).

**Connection setup:**

- The Tauri command that starts the pipeline receives the server address and derives two ports:
  - Voice UDP: same as current (server TCP port + 1)
  - Media UDP: server TCP port + 2
- The media socket is always created (even if not immediately streaming) to be ready for receiving video when watching a stream.

### 4. Client — Jitter Buffer Auto-Recovery

**File: `tauri-client/src-tauri/src/media/pipeline.rs`**

- Add `consecutive_losses: u32` field to `JitterBuffer`, initialized to 0.
- In `drain()`:
  - When returning `Some(None)` (missing packet): increment `consecutive_losses`.
  - When returning `Some(Some(data))` (packet present): reset `consecutive_losses` to 0.
  - When `consecutive_losses >= 10` (200ms of continuous PLC): call `reset()` and return `None`.
- New `reset()` method: clears `packets` HashMap, sets `ready = false`, sets `consecutive_losses = 0`. The buffer re-fills from scratch (JITTER_DEPTH packets = 60ms).
- In the jitter drain loop (section 4b of main loop): when `drain()` returns `None` after a reset, the `break` exits the while loop. On the next iteration where a packet arrives, `push()` re-initializes `next_seq` and starts buffering.
- Also reset `peer.voice_drain_time = drain_now` when the jitter buffer resets, preventing accumulated time debt from causing a burst of catch-up decodes.
- Clear `peer.decoded_voice` on reset to prevent stale samples from being mixed.
- Log: `[pipeline] Jitter buffer reset for peer '{}' after {} consecutive losses`.

**Threshold rationale:** 10 consecutive lost packets = 200ms of unintelligible audio. Resetting adds 60ms of silence (re-buffer period) but restores clean playback. Net improvement: silence is better than robotic noise.

## What Stays the Same

- UDP packet format and protocol — no new packet types, no field changes
- C++ struct definitions in `udp_packet.hpp` — unchanged
- Opus encoder/decoder settings — unchanged
- Voice processing chain (AEC, RNNoise, AGC) — unchanged
- Ring buffer sizes and types — unchanged
- CPAL audio callbacks — unchanged
- React frontend — unchanged
- Single-threaded `io_context` on the server — unchanged (multi-threading is a future optimization)

## Files Modified

- `src/community/main.cpp` — second UDP socket, split receive chains, per-session dual endpoints
- `src/common/udp_packet.hpp` — no changes
- `tauri-client/src-tauri/src/media/video_packet.rs` — compact `to_bytes()` for video and FEC
- `tauri-client/src-tauri/src/media/pipeline.rs` — dual sockets, dual recv threads, jitter auto-recovery
- `tauri-client/src-tauri/src/media/audio_stream_pipeline.rs` — use voice socket for stream audio
- `tauri-client/src-tauri/src/media/video_pipeline.rs` — use media socket for video/FEC sends
- `tauri-client/src-tauri/src/media/mod.rs` — use media socket for NACK/keyframe-request sends, pass media socket to video recv thread

## Testing

- **2-person voice only:** should work as before (baseline regression check)
- **3-person voice only:** should now maintain clean audio
- **3-person voice + streaming:** voice should remain clean while stream is active
- **Kill-stream test:** start stream with 3 people, stop stream, voice should immediately recover (jitter auto-recovery)
- **Packet loss simulation:** verify jitter buffer resets after 200ms of continuous loss and recovers cleanly
