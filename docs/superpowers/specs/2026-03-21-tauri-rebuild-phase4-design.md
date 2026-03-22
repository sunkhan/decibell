# Phase 4 Design: Voice Chat

## Overview

Add real-time voice chat to the Decibell Tauri client. Users can join voice channels, hear other participants, and speak via their microphone. The C++ community server already handles voice channel management, UDP audio relay, and presence broadcasting — this phase builds the Rust audio pipeline and React voice UI.

## Scope

**In scope:**
- Opus encoding/decoding in Rust (dedicated audio thread)
- CPAL microphone capture and speaker playback
- UDP audio packet send/receive (compatible with existing C++ server format)
- Voice channel join/leave via existing TCP signaling
- Voice presence updates (who is in which voice channel)
- Mute and deafen controls
- Speaking indicators (glow rings) based on decoded audio amplitude
- Latency display (UDP ping/pong)
- Compact voice controls in channel sidebar
- Dedicated voice panel view with participant cards

**Out of scope:**
- Screen sharing / video (Phase 5)
- System audio capture / mixing (Phase 5)
- Voice activity detection (VAD) for automatic mute — users toggle mute manually
- Push-to-talk — may be added later
- Audio device selection UI — uses system defaults

## Architecture

### Audio Thread Model

A dedicated OS thread runs the audio pipeline, isolated from the Tokio async runtime:

```
┌─────────────────────────────────────────────────────────┐
│ Tauri Async Runtime (Tokio)                             │
│  ┌──────────────┐  ┌──────────────┐  ┌───────────────┐  │
│  │ TCP/TLS conn │  │ Tauri events │  │ Voice commands│  │
│  │ (signaling)  │  │ (to frontend)│  │ (from frontend│  │
│  └──────┬───────┘  └──────▲───────┘  └───────┬───────┘  │
│         │                 │                  │          │
│         │        ┌────────┴────────┐         │          │
│         │        │ Control Channel │         │          │
│         │        │ (mpsc)          │         │          │
│         │        └────────▲────────┘         │          │
└─────────┼────────────────┼──────────────────┼──────────┘
          │                │                  │
┌─────────▼────────────────┼──────────────────▼──────────┐
│ Audio Thread                                           │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐ │
│  │ CPAL In  │→│ Opus Enc │→│ UDP Send │→│ Server │ │
│  │ (mic)    │  │          │  │          │  │        │ │
│  └──────────┘  └──────────┘  └──────────┘  └────────┘ │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌────────┐ │
│  │ CPAL Out │←│ Mixer    │←│ Opus Dec │←│ UDP Recv│ │
│  │ (speaker)│  │          │  │ (per user)│  │        │ │
│  └──────────┘  └──────────┘  └──────────┘  └────────┘ │
└────────────────────────────────────────────────────────┘
```

The audio thread communicates with the main app via `std::sync::mpsc` channels:
- **Control channel** (main → audio): mute/deafen toggles, shutdown signal
- **Event channel** (audio → main): speaking state changes, audio levels, ping measurements

A **voice event bridge** task runs on the Tokio runtime: it polls `event_rx` in a loop and calls `app.emit()` to forward voice events to the frontend. This task is spawned when `VoiceEngine::start()` is called and cancelled on `stop()`. The `AppHandle` (which is `Send + Sync`) is passed to this task.

### Data Flow

1. User clicks voice channel → Tauri command `join_voice_channel` → sends `JOIN_VOICE_REQ` over TCP
2. Server adds user to voice channel, broadcasts `VOICE_PRESENCE_UPDATE` to all clients over TCP
3. Rust client emits `voice_presence_updated` Tauri event → frontend updates participant list
4. `VoiceEngine::start()` spawns audio thread, opens UDP socket to `server_host:server_port+1`
5. Audio thread begins capture/encode/send loop (20ms frames)
6. Incoming UDP packets decoded on audio thread → PCM mixed and played via CPAL output
7. Amplitude of decoded frames checked for speaking detection → events emitted to frontend
8. Disconnect → `LEAVE_VOICE_REQ` over TCP, audio thread stopped, UDP socket closed

### UDP Packet Format

Matches the existing C++ `UdpAudioPacket` structure (from `src/common/udp_packet.hpp`):

| Field | Size | Description |
|-------|------|-------------|
| `packet_type` | 1 byte | `0` = audio, `5` = ping |
| `sender_id` | 32 bytes | Last 31 chars of JWT (upstream), username (downstream) |
| `sequence` | 2 bytes | Monotonic counter for ordering |
| `payload_size` | 2 bytes | Actual Opus frame size |
| `payload` | 1400 bytes | Opus-encoded audio data |

**Total**: 1437 bytes per packet. MTU-safe (fits within 1500-byte Ethernet MTU).

**Sequence wrap-around**: The `u16` sequence counter wraps at 65535 (~21 minutes at 50 packets/sec). The receive path must handle wrap-around gracefully — if `new_seq` is much smaller than `last_seq` (e.g., difference > 32768), treat it as a forward wrap rather than an out-of-order packet.

### Ping Mechanism

New UDP packet type `PING` (type byte = `5`):
- Add `PING = 5` to the `UdpPacketType` enum in `src/common/udp_packet.hpp`
- Client sends a ping packet with the current timestamp in the payload
- C++ server echoes back any packet with type `5` — handler must be placed **before** the token-extraction logic in `do_receive_udp()` (early return, like KEYFRAME_REQUEST/NACK)
- Client measures RTT from send timestamp vs receive time
- Ping sent every 3 seconds, displayed as "48ms" on hover over "Connected" status

## Rust Voice Engine

### Module Structure

```
src-tauri/src/media/
├── mod.rs              # VoiceEngine (top-level API)
├── pipeline.rs         # AudioPipeline (audio thread main loop)
├── codec.rs            # Opus encoder/decoder wrappers
├── packet.rs           # UdpAudioPacket serialization/deserialization
└── speaking.rs         # Speaking detection (noise-gated amplitude)
```

### VoiceEngine

Top-level struct stored in `AppState`. Provides the public API for Tauri commands.

```rust
pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    is_muted: bool,
    is_deafened: bool,
}
```

The audio thread owns a `std::net::UdpSocket` (blocking I/O with `set_read_timeout` for the receive loop). The `event_rx` is moved into the voice event bridge Tokio task on start. Ping measurement runs on the audio thread's receive loop (check elapsed time each cycle, send ping packet every 3 seconds).

**Methods:**
- `start(server_host, server_port, jwt, app_handle)` — Spawn audio thread, open UDP socket, begin capture/playback
- `stop()` — Send shutdown signal, join audio thread, close socket
- `set_mute(muted: bool)` — Send mute control message. If deafened and unmuting, also undeafen
- `set_deafen(deafened: bool)` — Send deafen control message. Deafen implies mute; undeafen restores previous mute state

### AudioPipeline

Runs on the dedicated audio thread. Manages the full capture → encode → send and receive → decode → playback loop.

**Capture loop (20ms cycle):**
1. CPAL input callback fills a ring buffer with 48kHz/16-bit/mono PCM
2. When 960 samples (20ms) are accumulated, encode with Opus
3. If muted: encode a silence frame instead (keeps UDP connection alive)
4. Pack into `UdpAudioPacket` with incrementing sequence number
5. Send via UDP to `server_host:server_port+1`
6. Compute local amplitude for speaking indicator (post-mute-decision)

**Playback loop:**
1. UDP receive loop reads incoming packets
2. Extract sender username from `sender_id` field (server has rewritten it)
3. Route to per-user Opus decoder (create decoder on first packet from new user, remove after timeout)
4. Decode Opus frame → 960 PCM samples
5. Compute amplitude for remote speaking detection (noise-gated)
6. Mix all decoded frames into output buffer
7. CPAL output callback reads from the mixed output buffer

### Codec

Thin wrappers around `audiopus`:

- **Encoder**: 48kHz, mono, 20ms frames (960 samples). Variable bitrate. Application mode: `Voip`.
- **Decoder**: One instance per remote user. Created on first received packet, destroyed after 5 seconds of silence.

### Speaking Detection

Noise-gated amplitude check on decoded PCM frames:

1. Compute RMS amplitude of each 20ms frame
2. Maintain exponential moving average (EMA) of background noise floor (slow decay, α = 0.01)
3. If RMS > noise_floor × 3.0 → user is speaking
4. Hysteresis: require 3 consecutive "speaking" frames to trigger, 5 consecutive "silent" frames to clear
5. Emit `VoiceEvent::SpeakingChanged(username, bool)` on state transitions only (not every frame)

Applied to both local user (post-mute) and remote users (post-decode). Since detection runs on the decoded audio stream, speaking indicators are perfectly synchronized with what each client actually hears.

## Tauri Commands & Events

### New Commands

| Command | Parameters | Description |
|---------|-----------|-------------|
| `join_voice_channel` | `server_id`, `channel_id` | Send `JOIN_VOICE_REQ`, start voice engine |
| `leave_voice_channel` | — | Send `LEAVE_VOICE_REQ`, stop voice engine |
| `set_voice_mute` | `muted: bool` | Toggle mute on voice engine |
| `set_voice_deafen` | `deafened: bool` | Toggle deafen (implies mute) |

### New Events

| Event | Payload | Frequency |
|-------|---------|-----------|
| `voice_presence_updated` | `{ serverId, channelId, participants: string[] }` | On join/leave |
| `voice_user_speaking` | `{ username, speaking: bool }` | On state change |
| `voice_state_changed` | `{ isMuted, isDeafened }` | On toggle |
| `voice_ping_updated` | `{ latencyMs: number }` | Every 3 seconds |
| `voice_error` | `{ message: string }` | On error |

### Community Client Extensions

Add to `CommunityClient` in `net/community.rs`:
- `join_voice_channel(channel_id)` — Build and send `JOIN_VOICE_REQ` packet
- `leave_voice_channel()` — Build and send `LEAVE_VOICE_REQ` packet

Add to `route_packets` match:
- `VoicePresenceUpdate` → emit `voice_presence_updated` event with channel ID and participant list

## C++ Server Changes

### UDP Ping Echo

In `do_receive_udp()`, add handling for packet type `5` (PING):

```cpp
else if (packet_type == 5) { // PING
    // Echo the packet back to the sender
    auto echo_buf = std::make_shared<std::vector<uint8_t>>(
        udp_buffer_, udp_buffer_ + bytes_recvd);
    udp_socket_.async_send_to(
        boost::asio::buffer(*echo_buf), sender_endpoint_,
        [echo_buf](boost::system::error_code, std::size_t) {});
}
```

No other server changes required — all voice channel management, UDP relay, and presence broadcasting already exist.

## Frontend Design

### New Components

#### VoiceControlBar

Location: Bottom of channel sidebar, between channel list and user panel.
Visible: Only when connected to a voice channel.

Contents:
- Voice channel name with speaker icon
- "Connected" status text (hover shows ping: "48ms")
- Three buttons: Mute (toggle), Deafen (toggle), Disconnect
- Mute/deafen buttons show active state when toggled (e.g., red slash icon)

#### VoiceParticipantList

Location: Nested under the active voice channel in the channel sidebar.
Shows: Mini avatar (20px, rounded, colored by `stringToColor`), username, speaking indicator (green dot), mute icon if muted.

#### VoicePanel

Location: Replaces chat panel + members list (everything right of channel sidebar, below server bar).
Activated: When user clicks on their connected voice channel.

Contents:
- Header: voice channel name, participant count
- Centered grid of participant cards:
  - Large avatar (80px, rounded, colored)
  - Username below
  - Speaking glow ring: green `box-shadow` ring animates when user is speaking
  - Mute badge: small red circle with mute icon at bottom-right of avatar
  - Status text: "Speaking" (green), "Muted" (red), or empty
- Bottom control bar: Mute, Deafen, Disconnect buttons (larger than sidebar version)

### State Management

Expand existing `voiceStore` (already has skeleton). The existing `localAudioLevel` field is removed — speaking state is now tracked per-user via the `speakingUsers` array. The existing `activeStreams` field is retained for Phase 5 screen sharing but unused in Phase 4.

```typescript
interface VoiceState {
  connectedServerId: string | null;
  connectedChannelId: string | null;
  participants: VoiceParticipant[];  // existing type
  activeStreams: StreamInfo[];       // retained for Phase 5
  isMuted: boolean;
  isDeafened: boolean;
  speakingUsers: string[];           // array, not Set — Zustand needs immutable updates
  latencyMs: number | null;
  error: string | null;
  // actions
  setConnectedChannel: (serverId: string | null, channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  setMuted: (muted: boolean) => void;
  setDeafened: (deafened: boolean) => void;
  setSpeaking: (username: string, speaking: boolean) => void;
  setLatency: (ms: number) => void;
  setError: (error: string | null) => void;
  disconnect: () => void;
}
```

### View Switching

`activeView` in `uiStore` gains a new value: `"voice"`.

- Click voice channel → `invoke("join_voice_channel")`, set `activeView` to `"voice"`
- Click text channel while in voice → set `activeView` to `"server"` (voice stays connected, sidebar controls remain)
- Click connected voice channel again → set `activeView` to `"voice"` (return to voice panel)
- Disconnect → clear voice state, set `activeView` to `"server"` (return to last text channel)

### useVoiceEvents Hook

Mounted in `MainLayout`. Listens for all voice events and dispatches to `voiceStore`:

- `voice_presence_updated` → `setParticipants`
- `voice_user_speaking` → `setSpeaking`
- `voice_state_changed` → `setMuted`, `setDeafened`
- `voice_ping_updated` → `setLatency`
- `voice_error` → `setError`

### ChannelSidebar Changes

- Voice channels become clickable (call `invoke("join_voice_channel")`)
- When connected: show `VoiceParticipantList` nested under the active voice channel
- Show `VoiceControlBar` between channel list and user panel (only when connected)

### MainLayout Changes

Add `activeView === "voice"` rendering branch. `ChannelSidebar` remains visible in all non-browse views (it hosts the voice controls):
```tsx
<ChannelSidebar />
{activeView === "voice" ? <VoicePanel /> : (
  <>
    <ChatPanel />
    {activeView === "home" ? <FriendsList /> : <MembersList />}
  </>
)}
```

## Error Handling

| Scenario | Behavior |
|----------|----------|
| No microphone available | Join voice in listen-only mode. Show "No microphone detected" warning in VoiceControlBar. |
| Audio device disconnected mid-call | Stop audio pipeline, show "Audio device disconnected" warning, attempt re-init with new default device after 1 second. |
| UDP packets stop arriving (>5s) | Show "Connection unstable" in VoiceControlBar. Continue attempting to send/receive. |
| Community TCP connection drops | Auto-disconnect from voice (stop audio thread, clear state). Existing reconnect flow handles TCP re-auth. User rejoins voice manually. |
| Switching community servers | Auto-disconnect from current voice before connecting to new server. Only one voice connection at a time. |
| Switching voice channels (same server) | Send `LEAVE_VOICE_REQ` + `JOIN_VOICE_REQ`. Audio thread continues with same UDP endpoint — seamless transition. |

## Mute & Deafen Behavior

- **Mute**: Stops encoding mic audio. Sends silence-encoded Opus frames to keep UDP alive (~3-4 kbps). Speaking indicator stays off.
- **Deafen**: Implies mute. Stops playback of incoming audio (decoded frames discarded). Previous mute state tracked — undeafening restores it.
- **Deafen + Unmute**: Not possible. Deafen always forces mute.
- **Undeafen**: Restores previous mute state. If user was unmuted before deafening, they return to unmuted.

## Dependencies

### Rust Crates (new)
- `cpal` — Cross-platform audio I/O (ALSA/PulseAudio/PipeWire on Linux, WASAPI on Windows)
- `audiopus` — Opus codec bindings (wraps libopus)

### Platform Support
All components are cross-platform (Linux + Windows). No platform-specific code required.
