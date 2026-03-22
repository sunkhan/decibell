# Phase 5: Video & Screen Sharing — Design Spec

## Overview

High-quality screen sharing for Decibell's Tauri v2 + React client with C++ community server relay. Targets Discord-level quality and performance across Windows and Linux.

**Key decisions:**
- H.264 hardware encoding only (NVENC on Windows, VA-API on Linux) — no software fallback initially
- Platform-native capture: Windows Graphics Capture API (Windows), PipeWire (Linux)
- WebCodecs VideoDecoder for hardware-accelerated H.264 decoding in the browser
- Multiple concurrent streams per channel (default limit: 8, configurable per-server, can be set to unlimited)
- Audio capture as an opt-in checkbox at stream start time

---

## Section 1: Capture & Encode Pipeline (Rust)

### Capture Layer (`capture.rs`)

Platform-specific screen/window capture abstracted behind a common trait:

```
trait ScreenCapture {
    fn list_sources() -> Vec<CaptureSource>   // enumerate screens + windows
    fn start(source_id, config) -> FrameStream // returns async stream of raw frames
    fn stop()
}
```

- **Windows**: `windows-capture` crate wrapping the Windows Graphics Capture API. Returns BGRA frames via Direct3D texture sharing.
- **Linux**: `ashpd` crate for the XDG Desktop Portal / PipeWire pipeline. Portal shows system permission dialog, then PipeWire delivers frames.

Both backends produce `RawFrame { data: Vec<u8>, width, height, format, timestamp }`.

### Encoder (`encoder.rs`)

Hardware H.264 encoding via FFmpeg's C API (called through Rust FFI):

- **Windows**: `h264_nvenc` codec (NVIDIA) or `h264_amf` (AMD) or `h264_qsv` (Intel)
- **Linux**: `h264_vaapi` codec (Intel/AMD), or `h264_nvenc` (NVIDIA)

Frame rate limiting: Capture may produce frames faster than the target FPS (e.g., 144Hz monitors). Frame decimation is applied before encoding to match the configured frame rate, avoiding wasted encoder work.

Configuration from user settings:
- Resolution: source, 1080p, 720p (downscale before encode if needed)
- Frame rate: 60, 30, 15 fps
- Quality preset: maps to CRF/bitrate targets
  - High: ~6 Mbps at 1080p60
  - Medium: ~3 Mbps
  - Low: ~1.5 Mbps

Output: H.264 NAL units with SPS/PPS prepended to keyframes. Keyframe interval: every 2 seconds.

### Video Pipeline Orchestrator (`video_pipeline.rs`)

Connects capture → encode → packetize → send:

1. Captures raw frames from `ScreenCapture`
2. Feeds frames to encoder
3. Splits encoded NAL units into <=1200-byte packets (see Video Packet Format below)
4. Sends packets to server via UDP (reusing existing voice UDP socket)

### Video Packet Format

Video and voice share the same UDP socket. The first byte distinguishes packet types:

```
Byte 0:     Packet type (0x01 = voice, 0x02 = video, 0x03 = stream audio)
Bytes 1-4:  Sequence number (uint32, big-endian) — monotonically increasing per stream
Bytes 5-8:  Frame timestamp (uint32) — same value for all fragments of one frame
Byte 9:     Flags: bit 0 = keyframe, bits 1-2 = reserved
Byte 10:    Fragment index (uint8) — 0-based index of this fragment within the frame
Byte 11:    Fragment count (uint8) — total fragments for this frame
Bytes 12+:  Payload (H.264 NAL unit data, max 1188 bytes)
```

Total max packet size: 1200 bytes. The streamer's username is not in the packet — the server knows it from the UDP source address and maps it when relaying.

### Stream Identification

Each user can have at most **one active stream** at a time. Streams are identified by `streamer_username` throughout the system — no separate stream ID is needed. If multi-stream-per-user is added later (e.g., screen + camera), a dedicated stream ID field would be introduced.

### Audio Capture (`audio_capture.rs`)

When "Share audio" is enabled:

- **Windows**: WASAPI loopback capture for system/application audio
- **Linux**: PipeWire audio capture node

Audio is encoded with Opus (reusing existing voice codec) and sent with packet type `0x03` over the same UDP socket. Same packet header format as video but payload contains Opus frames.

---

## Section 2: C++ Community Server Video Relay

The C++ server acts as a selective forwarding unit (SFU) — it does not decode or re-encode video. It simply relays packets from streamer to watchers.

### Server-Side State

```cpp
struct StreamInfo {
    std::string streamer_username;
    std::string channel_id;
    uint32_t resolution_width;
    uint32_t resolution_height;
    uint32_t fps;
    std::vector<std::string> watchers;  // usernames watching this stream
};

// Per-channel stream registry
std::unordered_map<std::string, std::vector<StreamInfo>> channel_streams_;
uint32_t max_streams_per_channel_ = 8;  // configurable, 0 = unlimited
```

### New Protocol Messages

```protobuf
enum MessageType {
    STREAM_START    = 32;
    STREAM_STOP     = 33;
    STREAM_WATCH    = 34;
    STREAM_UNWATCH  = 35;
    STREAM_ANNOUNCE = 36;
    STREAM_CONTROL  = 37;
}

message StreamStart {
    uint32 resolution_width  = 1;
    uint32 resolution_height = 2;
    uint32 fps               = 3;
    bool   has_audio         = 4;
}

message StreamStop {}  // no fields needed — server knows who sent it

message StreamWatch {
    string streamer_username = 1;  // whose stream to watch
}

message StreamUnwatch {
    string streamer_username = 1;
}

message StreamAnnounce {
    string streamer_username = 1;
    bool   started           = 2;  // true = started, false = stopped
    uint32 resolution_width  = 3;  // only set when started=true
    uint32 resolution_height = 4;
    uint32 fps               = 5;
    bool   has_audio         = 6;
}

message StreamControl {
    enum ControlType {
        NACK = 0;           // request retransmission of specific packets
        PLI  = 1;           // picture loss indication — request keyframe
    }
    ControlType type             = 1;
    string      target_username  = 2;  // who this control message is for
    repeated uint32 nack_seqs    = 3;  // sequence numbers to retransmit (NACK only)
}
```

### Packet Flow

1. **Streamer** sends UDP video packets to server (identified by source address)
2. **Server** looks up `stream_watchers_` for that stream, forwards each packet to all watchers
3. **NACK handling**: Watcher sends NACK for missing sequence numbers → server forwards NACK to streamer → streamer retransmits
4. **PLI handling**: Watcher requests keyframe → server forwards PLI to streamer → streamer forces next frame as keyframe
5. **Late joiner**: When a watcher joins, server immediately sends PLI to streamer so watcher can start decoding from the next keyframe

### Stream Lifecycle

- `STREAM_START`: Server validates channel membership + stream count limit, registers stream, broadcasts `STREAM_ANNOUNCE` to channel
- `STREAM_STOP`: Server removes stream, broadcasts stop announcement, cleans up watcher lists
- **Disconnect detection**: If streamer's TCP connection drops, server auto-broadcasts `STREAM_STOP` for all their active streams

---

## Section 3: Viewer Pipeline

### Packet Reassembly (Rust — `video_receiver.rs`)

Runs in a Tauri async task, receives UDP video packets:

1. **Jitter buffer**: Collects packets, reorders by sequence number. Buffer depth: 50ms (~3 frames at 60fps). Packets arriving later than the buffer window are dropped.
2. **NACK generation**: Detects gaps in sequence numbers. Waits 1 RTT (measured from voice ping) before sending NACK. Max 3 retransmission attempts per packet before giving up and requesting keyframe via PLI.
3. **Frame reassembly**: Combines packets sharing same frame timestamp into complete H.264 NAL units
4. **Delivery**: Sends complete frames to frontend via Tauri's raw event channel (binary, not JSON-serialized) to avoid base64 overhead at 60fps. The `stream-frame` event carries a pointer to a shared byte buffer; the frontend reads the buffer directly into an `EncodedVideoChunk`.
5. **PLI fallback**: If too many packets are missing to reconstruct a frame, requests keyframe via PLI

### Browser Decoding (`StreamVideoPlayer.tsx`)

Uses the WebCodecs `VideoDecoder` API for hardware-accelerated H.264 decoding:

```typescript
const decoder = new VideoDecoder({
    output: (frame: VideoFrame) => {
        // Draw to <canvas> via requestAnimationFrame
        ctx.drawImage(frame, 0, 0);
        frame.close();
    },
    error: (e) => { /* request keyframe, reset decoder */ }
});

decoder.configure({
    codec: 'avc1.640028',  // H.264 High Profile Level 4.0
    hardwareAcceleration: 'prefer-hardware'
});
```

Frame data arrives via Tauri events as `EncodedVideoChunk` (keyframe or delta), fed directly to the decoder. Canvas renders at native refresh rate via `requestAnimationFrame`.

Audio playback: Opus packets decoded via existing WebAudio pipeline, played through standard audio output.

---

## Section 4: Frontend UI

### Capture Source Picker (Modal)

Discord-style tabbed picker modal, triggered from voice control bar "Share Screen" button:

- **Tabs**: "Screens" and "Windows" — each shows a thumbnail grid of available sources
- **Thumbnails**: Live preview snapshots of each source, selected source gets accent border
- **Quality settings bar**: Resolution dropdown (Source/1080p/720p), Frame Rate dropdown (60/30/15), Quality dropdown (High/Medium/Low)
- **Audio toggle**: "Share audio" switch at bottom-left
- **Go Live button**: Bottom-right, starts the stream with selected source and settings

### Stream Viewing — Focused View (Default)

When watching a stream, the voice panel transforms:

- **Main area**: Stream video fills the primary content area via `<canvas>` element
- **Stream header**: Streamer avatar + name, resolution/fps badge, theater mode toggle button
- **Right sidebar** (compact, ~140px): Voice participant list with speaking indicators. Streamer gets a "Streaming" badge. Stream switcher at bottom when multiple streams are active — lists all active streams, click to switch.

### Stream Viewing — Theater Mode (Toggle)

Toggled via the fullscreen button in focused view:

- Stream fills the entire voice panel area
- Controls appear as a bottom overlay on hover (fade in/out):
  - Left: Streamer avatar + name + quality badge
  - Right: Stacked participant avatars + exit theater button
- Overlay appears on mouse enter/movement, stays visible while hovering over the overlay itself, auto-hides after 3 seconds of no mouse activity

### Voice Control Bar Additions

When streaming:
- "Share Screen" button changes to "Stop Sharing" (red/destructive style)
- Small stream preview thumbnail appears in the control bar

When others are streaming:
- Stream indicator appears showing active stream count in channel

---

## Section 5: Tauri Commands, Events & Store

### Tauri Commands (Rust → C++ Server)

| Command | Parameters | Description |
|---------|-----------|-------------|
| `start_screen_share` | `source_id, resolution, fps, quality, share_audio` | Begins capture + encode pipeline, sends STREAM_START to server |
| `stop_screen_share` | — | Tears down pipeline, sends STREAM_STOP to server |
| `watch_stream` | `streamer_username` | Tells server to relay that user's stream to us |
| `stop_watching` | `streamer_username` | Stops receiving a stream |
| `list_capture_sources` | — | Returns available screens and windows for the picker |
| `request_keyframe` | `streamer_username` | Frontend decoder requests a keyframe (PLI) when WebCodecs encounters a decode error |

### Tauri Events (C++ Server → Frontend)

| Event | Payload | Description |
|-------|---------|-------------|
| `stream-started` | `{ username, resolution, fps }` | Someone in the channel started streaming |
| `stream-stopped` | `{ username }` | Someone stopped streaming |
| `stream-frame` | `{ username, data, timestamp, keyframe }` | Reassembled frame ready for WebCodecs decoding |

### Zustand Store Extension (`voiceStore.ts`)

The existing `StreamInfo` type in `types/index.ts` is extended with resolution and fps fields:

```typescript
// Updated StreamInfo in types/index.ts
export interface StreamInfo {
    streamId: string;           // same as ownerUsername (one stream per user)
    ownerUsername: string;
    hasAudio: boolean;
    resolutionWidth: number;    // NEW
    resolutionHeight: number;   // NEW
    fps: number;                // NEW
}

// New fields added to voiceStore
activeStreams: StreamInfo[]       // who's streaming in current channel (extends existing array shape)
watching: string | null          // which streamer username we're currently viewing
isStreaming: boolean             // are we sharing our screen
streamSettings: {
    resolution: '1080p' | '720p' | 'source';
    fps: 60 | 30 | 15;
    quality: 'high' | 'medium' | 'low';
    shareAudio: boolean;
}
```

The `disconnect()` action in voiceStore must also reset streaming state:

```typescript
disconnect: () => set({
    // ...existing resets...
    activeStreams: [],
    watching: null,
    isStreaming: false,
}),
```

---

## Section 6: Error Handling & Edge Cases

- **No hardware encoder available**: Surface clear error message to user (toast/notification). Do not attempt stream. Software fallback may be added in a future phase.
- **Packet loss / stream drops**: NACK-based retransmission for missing packets. If too many frames lost, send PLI to request keyframe. Viewer displays last successfully decoded frame during recovery — no black screen or freeze indicator.
- **Streamer disconnects unexpectedly**: Server detects TCP connection drop, broadcasts `stream-stopped` to all watchers. Viewer cleans up decoder and returns to normal voice panel view.
- **Watcher joins mid-stream**: Server sends PLI to streamer to force an immediate keyframe so the new watcher can start decoding right away.
- **Concurrent stream limit**: Default 8 per channel, configurable per-server by the operator. Can be set to 0 for unlimited. Server rejects `STREAM_START` with an error if limit reached.
- **Platform-specific capture failures**: PipeWire portal denial on Linux, WGC permission issues on Windows — caught at Rust layer, surfaced as user-friendly error in the capture picker modal.
- **Audio capture**: Mixed into a separate Opus-encoded audio track sent alongside video packets. Viewer receives and plays via standard WebAudio pipeline. Controlled by checkbox at stream start.
- **Decoder error recovery**: If WebCodecs decoder encounters a corrupt frame, it requests a keyframe via PLI and resets the decoder state. No user-visible glitch beyond a brief quality dip.
