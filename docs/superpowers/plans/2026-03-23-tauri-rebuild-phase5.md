# Phase 5: Video & Screen Sharing — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add high-quality screen sharing to Decibell with H.264 hardware encoding, SFU relay via the C++ community server, and WebCodecs decoding in the browser.

**Architecture:** Platform-native capture (WGC/PipeWire) feeds H.264 hardware encoder via FFmpeg FFI. Encoded frames are packetized into UDP packets matching the existing `UdpVideoPacket` format and sent through the community server SFU. The server relays packets to watchers (new watcher tracking). Viewers reassemble frames in Rust and decode via WebCodecs in the browser.

**Tech Stack:** Rust (FFmpeg FFI, ashpd/PipeWire, windows-capture), C++ (Boost.ASIO UDP relay), React + TypeScript (WebCodecs VideoDecoder, Zustand), Protobuf (existing messages + minor extensions)

**Existing Infrastructure (do NOT recreate):**
- Protobuf messages: `StartStreamRequest`, `StopStreamRequest`, `WatchStreamRequest`, `StopWatchingRequest`, `StreamPresenceUpdate`, `VideoStreamInfo` — already in `proto/messages.proto`
- UDP packet structs: `UdpVideoPacket`, `UdpKeyframeRequest`, `UdpNackPacket`, `UdpFecPacket` — already in `src/common/udp_packet.hpp`
- Community server: `start_stream()`, `stop_stream()`, `broadcast_stream_presence()`, `relay_keyframe_request()`, `relay_nack()`, `broadcast_to_voice_channel()` — already in `src/community/main.cpp`
- Rust audio pipeline: `VoiceEngine`, `UdpAudioPacket`, CPAL + Opus — in `src-tauri/src/media/`
- Frontend: `voiceStore.activeStreams: StreamInfo[]`, `setActiveStreams()` — already in stores

**Spec:** `docs/superpowers/specs/2026-03-23-tauri-rebuild-phase5-design.md`

**Deviations from spec:**
- **Packet format**: The spec defines a custom 12-byte header video packet (1200 bytes max). The plan uses the **existing** `UdpVideoPacket` struct from `src/common/udp_packet.hpp` (1445 bytes, 32-byte sender_id). This is intentional — the C++ server already parses and relays this format. The spec's packet format was written without awareness of the existing infrastructure.
- **StreamControl proto**: The spec defines a TCP-based `StreamControl` message for NACK/PLI. The plan uses the existing UDP-based `UdpKeyframeRequest` and `UdpNackPacket` structs which are already implemented in the C++ server relay. Adding a TCP path is unnecessary complexity.
- **FEC**: Forward Error Correction is deferred. The C++ server already relays FEC packets, but the Rust encoder/receiver won't generate/consume them initially.
- **Audio capture**: Marked as a stub in this plan. The `share_audio` flag is wired through signaling, but actual system audio capture (WASAPI/PipeWire) requires significant platform-specific work and will be completed as a follow-up within Phase 5.

---

## File Map

### Rust — New Files
| File | Responsibility |
|------|---------------|
| `src-tauri/src/media/video_packet.rs` | `UdpVideoPacket`, `UdpKeyframeRequest`, `UdpNackPacket` Rust structs matching C++ `udp_packet.hpp` |
| `src-tauri/src/media/capture.rs` | `CaptureSource` struct, `list_sources()` trait, platform dispatch |
| `src-tauri/src/media/capture_pipewire.rs` | Linux PipeWire screen capture via `ashpd` |
| `src-tauri/src/media/capture_wgc.rs` | Windows WGC screen capture via `windows-capture` |
| `src-tauri/src/media/encoder.rs` | H.264 hardware encoder wrapper using FFmpeg C API via `ffmpeg-next` |
| `src-tauri/src/media/video_pipeline.rs` | Orchestrator: capture → encode → packetize → UDP send |
| `src-tauri/src/media/video_receiver.rs` | Jitter buffer, NACK generation, frame reassembly |
| `src-tauri/src/commands/streaming.rs` | Tauri commands: `start_screen_share`, `stop_screen_share`, `watch_stream`, `request_keyframe`, etc. |

### Rust — Modified Files
| File | Changes |
|------|---------|
| `src-tauri/src/media/mod.rs` | Add new module declarations, `VideoEngine` struct |
| `src-tauri/src/media/packet.rs` | Add video packet type constants |
| `src-tauri/src/state.rs` | Add `video_engine: Option<VideoEngine>` to `AppState` |
| `src-tauri/src/commands/mod.rs` | Add `pub mod streaming;` |
| `src-tauri/src/lib.rs` | Register new streaming commands |
| `src-tauri/src/net/community.rs` | Add `start_stream()`, `stop_stream()`, `watch_stream()`, `stop_watching()` methods; handle `StreamPresenceUpdate` in router |
| `src-tauri/src/events/mod.rs` | Add stream event constants and emit functions |
| `src-tauri/Cargo.toml` | Add `ffmpeg-next`, `ashpd` (Linux), `windows-capture` (Windows) |

### C++ — Modified Files
| File | Changes |
|------|---------|
| `src/community/main.cpp` | Add watcher tracking (`stream_watchers_` map), `WATCH_STREAM_REQ`/`STOP_WATCHING_REQ` handlers, watcher-specific video relay, stream limit enforcement |

### Proto — Modified Files
| File | Changes |
|------|---------|
| `proto/messages.proto` | Add `resolution_width`, `resolution_height` fields to `StartStreamRequest` and `VideoStreamInfo` |

### Frontend — New Files
| File | Responsibility |
|------|---------------|
| `src/features/voice/StreamVideoPlayer.tsx` | WebCodecs H.264 decoder + `<canvas>` renderer |
| `src/features/voice/CaptureSourcePicker.tsx` | Modal: tabbed screen/window picker with quality settings |
| `src/features/voice/StreamViewPanel.tsx` | Focused view + theater mode toggle for watching streams |
| ~~`src/features/voice/useStreamEvents.ts`~~ | ~~Removed — stream events added directly to existing `useVoiceEvents.ts`~~ |

### Frontend — Modified Files
| File | Changes |
|------|---------|
| `src/stores/voiceStore.ts` | Add `watching`, `isStreaming`, `streamSettings` fields; reset in `disconnect()` |
| `src/types/index.ts` | Extend `StreamInfo` with `resolutionWidth`, `resolutionHeight`, `fps` |
| `src/features/voice/VoiceControlBar.tsx` | Add "Share Screen" / "Stop Sharing" button, stream indicator |
| `src/features/voice/VoicePanel.tsx` | Integrate `StreamViewPanel` when watching a stream |

---

## Task 1: Extend Proto & Types

**Files:**
- Modify: `proto/messages.proto:237-253`
- Modify: `tauri-client/src/types/index.ts:47-51`

- [ ] **Step 1: Add resolution/fps fields to proto messages**

In `proto/messages.proto`, extend `VideoStreamInfo` and `StartStreamRequest`:

```protobuf
message VideoStreamInfo {
  string stream_id = 1;
  string owner_username = 2;
  bool has_audio = 3;
  uint32 resolution_width = 4;
  uint32 resolution_height = 5;
  uint32 fps = 6;
}

message StartStreamRequest {
  string channel_id = 1;
  int32 target_fps = 2;
  int32 target_bitrate_kbps = 3;
  bool has_audio = 4;
  uint32 resolution_width = 5;
  uint32 resolution_height = 6;
}
```

- [ ] **Step 2: Extend frontend StreamInfo type**

In `tauri-client/src/types/index.ts`:

```typescript
export interface StreamInfo {
  streamId: string;
  ownerUsername: string;
  hasAudio: boolean;
  resolutionWidth: number;
  resolutionHeight: number;
  fps: number;
}
```

- [ ] **Step 3: Verify proto compiles**

Run: `cd tauri-client && cargo build 2>&1 | head -20`
Expected: Successful compilation (proto is compiled via build.rs)

- [ ] **Step 4: Commit**

```bash
git add proto/messages.proto tauri-client/src/types/index.ts
git commit -m "feat(proto): add resolution/fps fields to stream messages"
```

---

## Task 2: Rust Video Packet Structs

**Files:**
- Create: `tauri-client/src-tauri/src/media/video_packet.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`
- Modify: `tauri-client/src-tauri/src/media/packet.rs`

- [ ] **Step 1: Write tests for video packet serialization**

Create `tauri-client/src-tauri/src/media/video_packet.rs` with tests first:

```rust
/// Rust equivalents of the C++ structs in src/common/udp_packet.hpp.
/// Must be byte-compatible for UDP relay through the C++ community server.

pub const SENDER_ID_SIZE: usize = 32;
pub const UDP_MAX_PAYLOAD: usize = 1400;
pub const NACK_MAX_ENTRIES: usize = 64;

pub const PACKET_TYPE_VIDEO: u8 = 1;
pub const PACKET_TYPE_KEYFRAME_REQUEST: u8 = 2;
pub const PACKET_TYPE_NACK: u8 = 3;

pub const CODEC_H264: u8 = 1;

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpVideoPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub packet_index: u16,
    pub total_packets: u16,
    pub payload_size: u16,
    pub is_keyframe: bool,
    pub codec: u8,
    pub payload: [u8; UDP_MAX_PAYLOAD],
}

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpKeyframeRequest {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub target_username: [u8; SENDER_ID_SIZE],
}

#[repr(C, packed)]
#[derive(Clone)]
pub struct UdpNackPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub target_username: [u8; SENDER_ID_SIZE],
    pub frame_id: u32,
    pub nack_count: u16,
    pub missing_indices: [u16; NACK_MAX_ENTRIES],
}

impl UdpVideoPacket {
    pub fn new(
        sender_id_str: &str,
        frame_id: u32,
        packet_index: u16,
        total_packets: u16,
        is_keyframe: bool,
        data: &[u8],
    ) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; UDP_MAX_PAYLOAD];
        let data_len = data.len().min(UDP_MAX_PAYLOAD);
        payload[..data_len].copy_from_slice(&data[..data_len]);

        UdpVideoPacket {
            packet_type: PACKET_TYPE_VIDEO,
            sender_id,
            frame_id,
            packet_index,
            total_packets,
            payload_size: data_len as u16,
            is_keyframe,
            codec: CODEC_H264,
            payload,
        }
    }

    /// Serialize to bytes for UDP send.
    /// Sends the full struct size to match the C++ UdpVideoPacket layout.
    /// The server uses bytes_recvd for relay, so this works correctly.
    pub fn to_bytes(&self) -> Vec<u8> {
        let total = std::mem::size_of::<Self>();
        let mut buf = vec![0u8; total];
        // Safety: UdpVideoPacket is repr(C, packed), plain data, no padding
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                total,
            );
        }
        buf
    }

    /// Deserialize from received UDP bytes.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        let expected = std::mem::size_of::<Self>();
        if buf.len() < expected {
            return None;
        }
        let mut pkt = Self {
            packet_type: 0,
            sender_id: [0; SENDER_ID_SIZE],
            frame_id: 0,
            packet_index: 0,
            total_packets: 0,
            payload_size: 0,
            is_keyframe: false,
            codec: 0,
            payload: [0; UDP_MAX_PAYLOAD],
        };
        unsafe {
            std::ptr::copy_nonoverlapping(
                buf.as_ptr(),
                &mut pkt as *mut Self as *mut u8,
                expected,
            );
        }
        Some(pkt)
    }

    /// Extract sender username from sender_id bytes.
    pub fn sender_username(&self) -> String {
        let end = self.sender_id.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
        String::from_utf8_lossy(&self.sender_id[..end]).to_string()
    }

    /// Get the actual payload data slice.
    pub fn payload_data(&self) -> &[u8] {
        &self.payload[..self.payload_size as usize]
    }
}

impl UdpKeyframeRequest {
    pub fn new(sender_id_str: &str, target: &str) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let s = sender_id_str.as_bytes();
        sender_id[..s.len().min(SENDER_ID_SIZE)].copy_from_slice(&s[..s.len().min(SENDER_ID_SIZE)]);

        let mut target_username = [0u8; SENDER_ID_SIZE];
        let t = target.as_bytes();
        target_username[..t.len().min(SENDER_ID_SIZE)].copy_from_slice(&t[..t.len().min(SENDER_ID_SIZE)]);

        UdpKeyframeRequest {
            packet_type: PACKET_TYPE_KEYFRAME_REQUEST,
            sender_id,
            target_username,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let total = std::mem::size_of::<Self>();
        let mut buf = vec![0u8; total];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                total,
            );
        }
        buf
    }
}

impl UdpNackPacket {
    pub fn new(sender_id_str: &str, target: &str, frame_id: u32, missing: &[u16]) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let s = sender_id_str.as_bytes();
        sender_id[..s.len().min(SENDER_ID_SIZE)].copy_from_slice(&s[..s.len().min(SENDER_ID_SIZE)]);

        let mut target_username = [0u8; SENDER_ID_SIZE];
        let t = target.as_bytes();
        target_username[..t.len().min(SENDER_ID_SIZE)].copy_from_slice(&t[..t.len().min(SENDER_ID_SIZE)]);

        let count = missing.len().min(NACK_MAX_ENTRIES);
        let mut missing_indices = [0u16; NACK_MAX_ENTRIES];
        missing_indices[..count].copy_from_slice(&missing[..count]);

        UdpNackPacket {
            packet_type: PACKET_TYPE_NACK,
            sender_id,
            target_username,
            frame_id,
            nack_count: count as u16,
            missing_indices,
        }
    }

    pub fn to_bytes(&self) -> Vec<u8> {
        let total = std::mem::size_of::<Self>();
        let mut buf = vec![0u8; total];
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                total,
            );
        }
        buf
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_packet_roundtrip() {
        let data = b"hello video frame";
        let pkt = UdpVideoPacket::new("testuser", 42, 0, 3, true, data);
        let bytes = pkt.to_bytes();
        let decoded = UdpVideoPacket::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.packet_type, PACKET_TYPE_VIDEO);
        assert_eq!(decoded.frame_id, 42);
        assert_eq!(decoded.packet_index, 0);
        assert_eq!(decoded.total_packets, 3);
        assert!(decoded.is_keyframe);
        assert_eq!(decoded.codec, CODEC_H264);
        assert_eq!(decoded.payload_size, data.len() as u16);
        assert_eq!(decoded.payload_data(), data);
        assert_eq!(decoded.sender_username(), "testuser");
    }

    #[test]
    fn video_packet_size_matches_cpp() {
        // C++ struct: 1 + 32 + 4 + 2 + 2 + 2 + 1 + 1 + 1400 = 1445 bytes
        assert_eq!(std::mem::size_of::<UdpVideoPacket>(), 1445);
    }

    #[test]
    fn keyframe_request_roundtrip() {
        let req = UdpKeyframeRequest::new("viewer1", "streamer1");
        let bytes = req.to_bytes();
        assert_eq!(bytes[0], PACKET_TYPE_KEYFRAME_REQUEST);
        // sender_id at offset 1, target at offset 33
        let sender = String::from_utf8_lossy(&bytes[1..8]).trim_matches('\0').to_string();
        assert_eq!(sender, "viewer1");
    }

    #[test]
    fn nack_packet_stores_missing_indices() {
        let missing = vec![2u16, 5, 7];
        let nack = UdpNackPacket::new("viewer1", "streamer1", 100, &missing);
        assert_eq!(nack.nack_count, 3);
        assert_eq!(nack.missing_indices[0], 2);
        assert_eq!(nack.missing_indices[1], 5);
        assert_eq!(nack.missing_indices[2], 7);
    }
}
```

- [ ] **Step 2: Add module declarations**

In `tauri-client/src-tauri/src/media/mod.rs`, add:
```rust
pub mod video_packet;
```

In `tauri-client/src-tauri/src/media/packet.rs`, add constants:
```rust
pub const PACKET_TYPE_VIDEO: u8 = 1;
pub const PACKET_TYPE_KEYFRAME_REQUEST: u8 = 2;
pub const PACKET_TYPE_NACK: u8 = 3;
```

- [ ] **Step 3: Run tests to verify**

Run: `cd tauri-client/src-tauri && cargo test video_packet -- --nocapture`
Expected: All 4 tests pass, size assertion confirms 1445 bytes

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/video_packet.rs tauri-client/src-tauri/src/media/mod.rs tauri-client/src-tauri/src/media/packet.rs
git commit -m "feat(media): add Rust video packet structs matching C++ UDP format"
```

---

## Task 3: C++ Community Server — Watcher Tracking & Selective Relay

The community server already relays video packets to ALL voice channel members via `broadcast_to_voice_channel()`. This task adds watcher-specific relay so video only goes to users who have explicitly requested to watch a stream.

**Files:**
- Modify: `src/community/main.cpp`

- [ ] **Step 1: Add watcher tracking data structures**

In `SessionManager` class (after `active_streams_` declaration around line 55):

```cpp
// Maps channel_id -> streamer_username -> set of watcher session pointers
std::unordered_map<std::string,
    std::unordered_map<std::string, std::set<std::shared_ptr<Session>>>>
    stream_watchers_;

uint32_t max_streams_per_channel_ = 8;  // 0 = unlimited
```

Add new public methods to `SessionManager`:

```cpp
void add_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
void remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username);
void broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id,
                           const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket);
```

- [ ] **Step 2: Implement watcher methods**

```cpp
void SessionManager::add_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username) {
    std::lock_guard<std::mutex> lock(mutex_);
    stream_watchers_[channel_id][streamer_username].insert(watcher);
}

void SessionManager::remove_watcher(std::shared_ptr<Session> watcher, const std::string& channel_id, const std::string& streamer_username) {
    std::lock_guard<std::mutex> lock(mutex_);
    auto ch_it = stream_watchers_.find(channel_id);
    if (ch_it != stream_watchers_.end()) {
        auto st_it = ch_it->second.find(streamer_username);
        if (st_it != ch_it->second.end()) {
            st_it->second.erase(watcher);
            if (st_it->second.empty()) ch_it->second.erase(st_it);
            if (ch_it->second.empty()) stream_watchers_.erase(ch_it);
        }
    }
}

void SessionManager::broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id,
                                            const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket) {
    auto buffer = std::make_shared<std::vector<char>>(data, data + length);
    std::lock_guard<std::mutex> lock(mutex_);
    auto ch_it = stream_watchers_.find(channel_id);
    if (ch_it == stream_watchers_.end()) return;
    auto st_it = ch_it->second.find(streamer_username);
    if (st_it == ch_it->second.end()) return;
    for (auto& watcher : st_it->second) {
        if (watcher->get_udp_endpoint().port() != 0) {
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), watcher->get_udp_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
        }
    }
}
```

- [ ] **Step 3: Handle WATCH_STREAM_REQ and STOP_WATCHING_REQ in Session::process_packet**

Add after the STOP_STREAM_REQ handler (around line 258):

```cpp
// --- WATCH STREAM ---
else if (packet.type() == chatproj::Packet::WATCH_STREAM_REQ) {
    const auto& req = packet.watch_stream_req();
    manager_.add_watcher(shared_from_this(), req.channel_id(), req.target_username());
    std::cout << "[Community] " << username_ << " watching " << req.target_username() << "'s stream in " << req.channel_id() << "\n";
}

// --- STOP WATCHING STREAM ---
else if (packet.type() == chatproj::Packet::STOP_WATCHING_REQ) {
    const auto& req = packet.stop_watching_req();
    manager_.remove_watcher(shared_from_this(), req.channel_id(), req.target_username());
    std::cout << "[Community] " << username_ << " stopped watching " << req.target_username() << "'s stream\n";
}
```

- [ ] **Step 4: Add stream limit enforcement to start_stream**

Modify `SessionManager::start_stream()` to check the limit:

```cpp
void SessionManager::start_stream(std::shared_ptr<Session> session, const std::string& channel_id, bool has_audio) {
    {
        std::lock_guard<std::mutex> lock(mutex_);
        // Enforce stream limit
        if (max_streams_per_channel_ > 0 && active_streams_[channel_id].size() >= max_streams_per_channel_) {
            std::cout << "[Community] Stream limit reached in " << channel_id << ", rejecting " << session->get_username() << "\n";
            return;
        }
        active_streams_[channel_id][session->get_username()] = { has_audio };
    }
    broadcast_stream_presence(channel_id);
}
```

- [ ] **Step 5: Change video relay from broadcast to watcher-specific**

In `do_receive_udp()`, change the video relay logic. Currently it calls `broadcast_to_voice_channel()` for ALL packet types. Change so VIDEO and FEC packets go through watcher-specific relay:

In the section after sender_id replacement (around line 796), change:
```cpp
// OLD: manager_.broadcast_to_voice_channel(udp_buffer_, bytes_recvd, channel, session, udp_socket_);

// NEW: Route based on packet type
if (packet_type == chatproj::UdpPacketType::AUDIO) {
    manager_.broadcast_to_voice_channel(udp_buffer_, bytes_recvd, channel, session, udp_socket_);
} else if (packet_type == chatproj::UdpPacketType::VIDEO || packet_type == chatproj::UdpPacketType::FEC) {
    manager_.broadcast_to_watchers(udp_buffer_, bytes_recvd, channel, session->get_username(), udp_socket_);
}
```

- [ ] **Step 6: Clean up watchers on stream stop and disconnect**

In `stop_stream()`, add watcher cleanup after removing from active_streams:
```cpp
void SessionManager::stop_stream(std::shared_ptr<Session> session, const std::string& channel_id) {
    bool removed = false;
    {
        std::lock_guard<std::mutex> lock(mutex_);
        auto it = active_streams_.find(channel_id);
        if (it != active_streams_.end()) {
            if (it->second.erase(session->get_username()) > 0) {
                removed = true;
            }
            if (it->second.empty()) active_streams_.erase(it);
        }
        // Clean up watchers for this stream
        auto wch = stream_watchers_.find(channel_id);
        if (wch != stream_watchers_.end()) {
            wch->second.erase(session->get_username());
            if (wch->second.empty()) stream_watchers_.erase(wch);
        }
    }
    if (removed) {
        broadcast_stream_presence(channel_id);
    }
}
```

In `SessionManager::leave()`, clean up all watcher entries for the disconnecting session:
```cpp
// In leave(), after removing from channels and voice_channels:
// Clean up any watcher entries for this session
for (auto& [ch_id, streamers] : stream_watchers_) {
    for (auto& [streamer, watchers] : streamers) {
        watchers.erase(session);
    }
}
```

- [ ] **Step 7: Send PLI on watcher join (late joiner keyframe)**

In the WATCH_STREAM_REQ handler, after adding watcher, send PLI to streamer:

```cpp
// After add_watcher:
// Send PLI to streamer so new watcher gets a keyframe
// (relay_keyframe_request is already implemented)
manager_.relay_keyframe_request(req.target_username(), /* need udp_socket reference */);
```

Note: The Session doesn't have direct access to `udp_socket_`. Add a `udp_socket` reference to SessionManager or pass it differently. The simplest approach: store a reference to the UDP socket in SessionManager.

Add to SessionManager:
```cpp
void set_udp_socket(boost::asio::ip::udp::socket* sock) { udp_socket_ = sock; }
boost::asio::ip::udp::socket* udp_socket_ = nullptr;
```

In `CommunityServer` constructor after creating udp_socket_, call:
```cpp
manager_.set_udp_socket(&udp_socket_);
```

Then in WATCH_STREAM_REQ handler:
```cpp
manager_.add_watcher(shared_from_this(), req.channel_id(), req.target_username());
manager_.relay_keyframe_request_internal(req.target_username()); // uses stored udp_socket_
```

Add internal version that uses stored socket:
```cpp
void SessionManager::relay_keyframe_request_internal(const std::string& target_username) {
    if (!udp_socket_) return;
    relay_keyframe_request(target_username, *udp_socket_);
}
```

- [ ] **Step 8: Build and verify**

Run: `cd /home/sun/Desktop/decibell/decibell && cmake --build build 2>&1 | tail -10`
Expected: Successful compilation

- [ ] **Step 9: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(community): add watcher tracking and selective video relay"
```

---

## Task 4: Rust CommunityClient — Stream Signaling Methods

**Files:**
- Modify: `tauri-client/src-tauri/src/net/community.rs`
- Modify: `tauri-client/src-tauri/src/events/mod.rs`

- [ ] **Step 1: Add stream signaling methods to CommunityClient**

In `tauri-client/src-tauri/src/net/community.rs`, add after `leave_voice_channel()`:

```rust
/// Start screen sharing in a voice channel.
pub async fn start_stream(
    &self,
    channel_id: &str,
    target_fps: i32,
    target_bitrate_kbps: i32,
    has_audio: bool,
    resolution_width: u32,
    resolution_height: u32,
) -> Result<(), String> {
    let data = build_packet(
        packet::Type::StartStreamReq,
        packet::Payload::StartStreamReq(StartStreamRequest {
            channel_id: channel_id.into(),
            target_fps,
            target_bitrate_kbps,
            has_audio,
            resolution_width,
            resolution_height,
        }),
        Some(&self.jwt),
    );
    self.send(data).await
}

/// Stop screen sharing.
pub async fn stop_stream(&self, channel_id: &str) -> Result<(), String> {
    let data = build_packet(
        packet::Type::StopStreamReq,
        packet::Payload::StopStreamReq(StopStreamRequest {
            channel_id: channel_id.into(),
        }),
        Some(&self.jwt),
    );
    self.send(data).await
}

/// Request to watch a user's stream.
pub async fn watch_stream(&self, channel_id: &str, target_username: &str) -> Result<(), String> {
    let data = build_packet(
        packet::Type::WatchStreamReq,
        packet::Payload::WatchStreamReq(WatchStreamRequest {
            channel_id: channel_id.into(),
            target_username: target_username.into(),
        }),
        Some(&self.jwt),
    );
    self.send(data).await
}

/// Stop watching a user's stream.
pub async fn stop_watching(&self, channel_id: &str, target_username: &str) -> Result<(), String> {
    let data = build_packet(
        packet::Type::StopWatchingReq,
        packet::Payload::StopWatchingReq(StopWatchingRequest {
            channel_id: channel_id.into(),
            target_username: target_username.into(),
        }),
        Some(&self.jwt),
    );
    self.send(data).await
}
```

- [ ] **Step 2: Handle StreamPresenceUpdate in route_packets**

In `route_packets()`, add a new match arm before the catch-all `_ =>`:

```rust
Some(packet::Payload::StreamPresenceUpdate(update)) => {
    events::emit_stream_presence_updated(
        &app,
        server_id.clone(),
        update.channel_id,
        update.active_streams.into_iter().map(|s| events::StreamInfoPayload {
            stream_id: s.stream_id,
            owner_username: s.owner_username,
            has_audio: s.has_audio,
            resolution_width: s.resolution_width,
            resolution_height: s.resolution_height,
            fps: s.fps,
        }).collect(),
    );
}
```

- [ ] **Step 3: Add stream event emitters to events/mod.rs**

In `tauri-client/src-tauri/src/events/mod.rs`, add:

```rust
pub const STREAM_PRESENCE_UPDATED: &str = "stream_presence_updated";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamInfoPayload {
    pub stream_id: String,
    pub owner_username: String,
    pub has_audio: bool,
    pub resolution_width: u32,
    pub resolution_height: u32,
    pub fps: u32,
}

pub fn emit_stream_presence_updated(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    streams: Vec<StreamInfoPayload>,
) {
    let _ = app.emit(
        STREAM_PRESENCE_UPDATED,
        serde_json::json!({
            "serverId": server_id,
            "channelId": channel_id,
            "streams": streams,
        }),
    );
}
```

- [ ] **Step 4: Verify compilation**

Run: `cd tauri-client && cargo build 2>&1 | tail -10`
Expected: Successful compilation

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/net/community.rs tauri-client/src-tauri/src/events/mod.rs
git commit -m "feat(net): add stream signaling methods and stream presence events"
```

---

## Task 5: Screen Capture — Platform Abstraction & Linux Backend

**Files:**
- Create: `tauri-client/src-tauri/src/media/capture.rs`
- Create: `tauri-client/src-tauri/src/media/capture_pipewire.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`
- Modify: `tauri-client/src-tauri/Cargo.toml`

- [ ] **Step 1: Add dependencies to Cargo.toml**

```toml
[target.'cfg(target_os = "linux")'.dependencies]
ashpd = { version = "0.9", features = ["pipewire", "tokio"] }
pipewire = "0.8"

[target.'cfg(target_os = "windows")'.dependencies]
windows-capture = "1"
```

Also add the FFmpeg dependency (needed in Task 7 but add now to avoid rebuild churn):
```toml
[dependencies]
ffmpeg-next = "7"
```

- [ ] **Step 2: Create capture abstraction**

Create `tauri-client/src-tauri/src/media/capture.rs`:

```rust
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CaptureSource {
    pub id: String,
    pub name: String,
    pub source_type: CaptureSourceType,
    pub width: u32,
    pub height: u32,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum CaptureSourceType {
    Screen,
    Window,
}

#[derive(Debug, Clone)]
pub struct CaptureConfig {
    pub target_fps: u32,
    pub target_width: u32,
    pub target_height: u32,
}

#[derive(Debug)]
pub struct RawFrame {
    pub data: Vec<u8>,
    pub width: u32,
    pub height: u32,
    pub timestamp_us: u64,
}

/// List available capture sources (screens and windows).
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    #[cfg(target_os = "linux")]
    {
        super::capture_pipewire::list_sources().await
    }
    #[cfg(target_os = "windows")]
    {
        super::capture_wgc::list_sources().await
    }
    #[cfg(not(any(target_os = "linux", target_os = "windows")))]
    {
        Err("Screen capture not supported on this platform".to_string())
    }
}
```

- [ ] **Step 3: Create Linux PipeWire capture backend**

Create `tauri-client/src-tauri/src/media/capture_pipewire.rs`:

```rust
use super::capture::{CaptureSource, CaptureSourceType};

/// List available screens and windows via XDG Desktop Portal.
pub async fn list_sources() -> Result<Vec<CaptureSource>, String> {
    use ashpd::desktop::screencast::{CursorMode, PersistMode, Screencast, SourceType};

    let proxy = Screencast::new()
        .await
        .map_err(|e| format!("Failed to connect to screencast portal: {}", e))?;

    // For listing, we need to create a session and get available sources
    // The portal shows a picker dialog — for list_sources we enumerate monitors
    // For now, return a placeholder that triggers the portal picker on start
    let mut sources = Vec::new();

    // Use ashpd to enumerate monitors via display portal
    use ashpd::desktop::screencast::Stream as ScreencastStream;

    // Portal-based capture: the actual source selection happens when
    // we call create_session + select_sources (shows system picker).
    // For list_sources, we return known monitors.
    sources.push(CaptureSource {
        id: "portal".to_string(),
        name: "Screen (Portal Picker)".to_string(),
        source_type: CaptureSourceType::Screen,
        width: 0,  // determined after portal selection
        height: 0,
    });

    Ok(sources)
}

/// Start capturing from a PipeWire source.
/// Returns a channel that receives RawFrames.
/// Uses std::sync::mpsc (not tokio) because the video pipeline runs on a
/// dedicated OS thread, not in the tokio runtime.
pub async fn start_capture(
    _source_id: &str,
    _config: &super::capture::CaptureConfig,
) -> Result<std::sync::mpsc::Receiver<super::capture::RawFrame>, String> {
    use ashpd::desktop::screencast::{CursorMode, PersistMode, Screencast, SourceType};

    let proxy = Screencast::new()
        .await
        .map_err(|e| format!("Screencast portal: {}", e))?;

    let session = proxy
        .create_session()
        .await
        .map_err(|e| format!("Create session: {}", e))?;

    proxy
        .select_sources(
            &session,
            CursorMode::Embedded,
            SourceType::Monitor | SourceType::Window,
            false,
            None,
            PersistMode::DoNot,
        )
        .await
        .map_err(|e| format!("Select sources: {}", e))?;

    let streams = proxy
        .start(&session, None)
        .await
        .map_err(|e| format!("Start capture: {}", e))?
        .streams()
        .to_vec();

    if streams.is_empty() {
        return Err("No streams available after portal selection".to_string());
    }

    let pipewire_node_id = streams[0].pipe_wire_node_id();
    let (tx, rx) = std::sync::mpsc::sync_channel(4);

    // PipeWire frame capture runs on a dedicated thread
    let _config = _config.clone();
    std::thread::Builder::new()
        .name("decibell-capture".to_string())
        .spawn(move || {
            // PipeWire main loop for frame capture
            // This will be implemented with the pipewire crate
            // connecting to the node_id and extracting BGRA frames
            let _ = (pipewire_node_id, tx, _config);
            // TODO: PipeWire stream connection and frame extraction
            // This requires pipewire crate initialization, connecting to the
            // node, and pushing RawFrame structs through the channel
        })
        .map_err(|e| format!("Failed to spawn capture thread: {}", e))?;

    Ok(rx)
}
```

- [ ] **Step 4: Add module declarations**

In `tauri-client/src-tauri/src/media/mod.rs`, add:
```rust
pub mod capture;
#[cfg(target_os = "linux")]
pub mod capture_pipewire;
#[cfg(target_os = "windows")]
pub mod capture_wgc;
```

- [ ] **Step 5: Verify compilation**

Run: `cd tauri-client && cargo build 2>&1 | tail -20`
Expected: Compiles (may warn about unused code, that's fine)

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/Cargo.toml tauri-client/src-tauri/src/media/capture.rs tauri-client/src-tauri/src/media/capture_pipewire.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(capture): add screen capture abstraction and Linux PipeWire backend"
```

---

## Task 6: H.264 Hardware Encoder

**Files:**
- Create: `tauri-client/src-tauri/src/media/encoder.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Create H.264 encoder wrapper**

Create `tauri-client/src-tauri/src/media/encoder.rs`:

```rust
use std::ffi::CString;

/// H.264 hardware encoder using FFmpeg's C API via ffmpeg-next.
pub struct H264Encoder {
    encoder: ffmpeg_next::encoder::Video,
    scaler: Option<ffmpeg_next::software::scaling::Context>,
    frame_count: u64,
    keyframe_interval: u64,
    force_next_keyframe: bool,
    target_width: u32,
    target_height: u32,
}

#[derive(Debug, Clone)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_kbps: u32,
    pub keyframe_interval_secs: u32,
}

#[derive(Debug)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub pts: u64,
}

impl H264Encoder {
    /// Create a new H.264 hardware encoder.
    /// Tries hardware encoders in order: NVENC, VA-API (Linux), AMF/QSV (Windows).
    pub fn new(config: &EncoderConfig) -> Result<Self, String> {
        ffmpeg_next::init().map_err(|e| format!("FFmpeg init: {}", e))?;

        let codec = Self::find_hw_encoder()?;
        let mut context = ffmpeg_next::codec::Context::new_with_codec(codec)
            .encoder()
            .video()
            .map_err(|e| format!("Encoder context: {}", e))?;

        context.set_width(config.width);
        context.set_height(config.height);
        context.set_frame_rate(Some(ffmpeg_next::Rational::new(config.fps as i32, 1)));
        context.set_time_base(ffmpeg_next::Rational::new(1, config.fps as i32));
        context.set_bit_rate((config.bitrate_kbps as usize) * 1000);
        context.set_format(ffmpeg_next::format::Pixel::NV12);
        context.set_gop(config.fps * config.keyframe_interval_secs);

        let encoder = context
            .open()
            .map_err(|e| format!("Open encoder: {}", e))?;

        Ok(H264Encoder {
            encoder,
            scaler: None,
            frame_count: 0,
            keyframe_interval: (config.fps * config.keyframe_interval_secs) as u64,
            force_next_keyframe: false,
            target_width: config.width,
            target_height: config.height,
        })
    }

    /// Find the best available hardware H.264 encoder.
    fn find_hw_encoder() -> Result<ffmpeg_next::Codec, String> {
        // Try encoders in preference order
        let candidates = if cfg!(target_os = "linux") {
            vec!["h264_nvenc", "h264_vaapi"]
        } else {
            vec!["h264_nvenc", "h264_amf", "h264_qsv"]
        };

        for name in &candidates {
            if let Some(codec) = ffmpeg_next::encoder::find_by_name(name) {
                log::info!("Using H.264 encoder: {}", name);
                return Ok(codec);
            }
        }
        Err("No hardware H.264 encoder found. Install NVIDIA drivers (NVENC) or ensure VA-API is available.".to_string())
    }

    /// Encode a raw BGRA frame into H.264 NAL units.
    /// Returns None if the encoder is buffering (no output yet).
    pub fn encode_frame(&mut self, bgra_data: &[u8], width: u32, height: u32) -> Result<Option<EncodedFrame>, String> {
        // Convert BGRA to NV12 using swscale.
        // Recreate scaler if source resolution changed (e.g. window resize).
        let needs_new_scaler = self.scaler.is_none(); // TODO: also check if width/height changed
        if needs_new_scaler {
            self.scaler = Some(
                ffmpeg_next::software::scaling::Context::get(
                    ffmpeg_next::format::Pixel::BGRA,
                    width,
                    height,
                    ffmpeg_next::format::Pixel::NV12,
                    self.target_width,
                    self.target_height,
                    ffmpeg_next::software::scaling::Flags::BILINEAR,
                )
                .map_err(|e| format!("Failed to create scaler: {}", e))?,
            );
        }
        let scaler = self.scaler.as_mut().unwrap();

        let mut src_frame = ffmpeg_next::frame::Video::new(ffmpeg_next::format::Pixel::BGRA, width, height);
        src_frame.data_mut(0)[..bgra_data.len()].copy_from_slice(bgra_data);

        let mut nv12_frame = ffmpeg_next::frame::Video::new(
            ffmpeg_next::format::Pixel::NV12,
            self.target_width,
            self.target_height,
        );
        scaler.run(&src_frame, &mut nv12_frame)
            .map_err(|e| format!("Scale frame: {}", e))?;

        nv12_frame.set_pts(Some(self.frame_count as i64));

        // Force keyframe at interval or on demand (PLI)
        if self.frame_count % self.keyframe_interval == 0 || self.force_next_keyframe {
            nv12_frame.set_kind(ffmpeg_next::picture::Type::I);
            self.force_next_keyframe = false;
        }

        self.frame_count += 1;

        self.encoder.send_frame(&nv12_frame)
            .map_err(|e| format!("Send frame: {}", e))?;

        let mut packet = ffmpeg_next::Packet::empty();
        match self.encoder.receive_packet(&mut packet) {
            Ok(()) => {
                let is_keyframe = packet.is_key();
                let data = packet.data().unwrap_or(&[]).to_vec();
                Ok(Some(EncodedFrame {
                    data,
                    is_keyframe,
                    pts: packet.pts().unwrap_or(0) as u64,
                }))
            }
            Err(ffmpeg_next::Error::Other { errno: ffmpeg_next::error::EAGAIN }) => {
                Ok(None) // Encoder needs more input
            }
            Err(e) => Err(format!("Receive packet: {}", e)),
        }
    }

    /// Request the encoder to produce a keyframe on the next encode call.
    pub fn force_keyframe(&mut self) {
        self.force_next_keyframe = true;
    }

    /// Flush remaining frames from the encoder.
    pub fn flush(&mut self) -> Vec<EncodedFrame> {
        let _ = self.encoder.send_eof();
        let mut frames = Vec::new();
        let mut packet = ffmpeg_next::Packet::empty();
        while self.encoder.receive_packet(&mut packet).is_ok() {
            if let Some(data) = packet.data() {
                frames.push(EncodedFrame {
                    data: data.to_vec(),
                    is_keyframe: packet.is_key(),
                    pts: packet.pts().unwrap_or(0) as u64,
                });
            }
        }
        frames
    }
}
```

- [ ] **Step 2: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, add:
```rust
pub mod encoder;
```

- [ ] **Step 3: Verify compilation**

Run: `cd tauri-client && cargo build 2>&1 | tail -20`
Expected: Compiles (requires ffmpeg dev libraries installed: `sudo pacman -S ffmpeg`)

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/encoder.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(encoder): add H.264 hardware encoder wrapper using FFmpeg"
```

---

## Task 7: Video Pipeline Orchestrator

Connects capture → encode → packetize → UDP send.

**Files:**
- Create: `tauri-client/src-tauri/src/media/video_pipeline.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Create the video pipeline**

Create `tauri-client/src-tauri/src/media/video_pipeline.rs`:

```rust
use std::net::UdpSocket;
use std::sync::Arc;
use std::time::{Duration, Instant};

use super::capture::RawFrame;
use super::encoder::{EncoderConfig, H264Encoder};
use super::video_packet::{UdpVideoPacket, UDP_MAX_PAYLOAD};

pub enum VideoPipelineControl {
    ForceKeyframe,
    Shutdown,
}

pub enum VideoPipelineEvent {
    Started,
    Stopped,
    Error(String),
}

/// Run the video send pipeline on a dedicated thread.
/// Reads RawFrames from the channel, encodes them, packetizes, and sends via UDP.
///
/// IMPORTANT: `socket` must be the SAME UDP socket used by the voice audio pipeline.
/// The community server identifies senders by their UDP source address, which was
/// learned during voice connection. A different socket would have a different port
/// and the server would reject the packets.
pub fn run_video_send_pipeline(
    frame_rx: std::sync::mpsc::Receiver<RawFrame>,
    control_rx: std::sync::mpsc::Receiver<VideoPipelineControl>,
    event_tx: std::sync::mpsc::Sender<VideoPipelineEvent>,
    socket: Arc<UdpSocket>,  // shared with voice pipeline
    sender_id: String,
    config: EncoderConfig,
    target_fps: u32,
) {

    // Initialize encoder
    let mut encoder = match H264Encoder::new(&config) {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VideoPipelineEvent::Error(e));
            return;
        }
    };

    let _ = event_tx.send(VideoPipelineEvent::Started);

    let mut frame_id: u32 = 0;
    let frame_interval = Duration::from_secs_f64(1.0 / target_fps as f64);
    let mut last_frame_time = Instant::now();

    loop {
        // Check control messages
        match control_rx.try_recv() {
            Ok(VideoPipelineControl::Shutdown) => break,
            Ok(VideoPipelineControl::ForceKeyframe) => {
                encoder.force_keyframe();
            }
            Err(std::sync::mpsc::TryRecvError::Disconnected) => break,
            Err(std::sync::mpsc::TryRecvError::Empty) => {}
        }

        // Frame rate limiting: skip frames that arrive faster than target
        let now = Instant::now();
        if now.duration_since(last_frame_time) < frame_interval {
            // Try to receive but don't block long
            match frame_rx.recv_timeout(Duration::from_millis(1)) {
                Ok(_frame) => continue, // drop frame, too soon
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
            }
        }

        // Receive frame
        let frame = match frame_rx.recv_timeout(Duration::from_millis(50)) {
            Ok(f) => f,
            Err(std::sync::mpsc::RecvTimeoutError::Timeout) => continue,
            Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => break,
        };
        last_frame_time = Instant::now();

        // Encode
        match encoder.encode_frame(&frame.data, frame.width, frame.height) {
            Ok(Some(encoded)) => {
                // Packetize: split encoded data into UDP_MAX_PAYLOAD-sized chunks
                let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
                let total = chunks.len() as u16;

                for (i, chunk) in chunks.iter().enumerate() {
                    let pkt = UdpVideoPacket::new(
                        &sender_id,
                        frame_id,
                        i as u16,
                        total,
                        encoded.is_keyframe,
                        chunk,
                    );
                    let _ = socket.send(&pkt.to_bytes());
                }
                frame_id = frame_id.wrapping_add(1);
            }
            Ok(None) => {} // encoder buffering
            Err(e) => {
                let _ = event_tx.send(VideoPipelineEvent::Error(format!("Encode: {}", e)));
            }
        }
    }

    // Flush encoder
    for encoded in encoder.flush() {
        let chunks: Vec<&[u8]> = encoded.data.chunks(UDP_MAX_PAYLOAD).collect();
        let total = chunks.len() as u16;
        for (i, chunk) in chunks.iter().enumerate() {
            let pkt = UdpVideoPacket::new(&sender_id, frame_id, i as u16, total, encoded.is_keyframe, chunk);
            let _ = socket.send(&pkt.to_bytes());
        }
        frame_id = frame_id.wrapping_add(1);
    }

    let _ = event_tx.send(VideoPipelineEvent::Stopped);
}
```

- [ ] **Step 2: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, add:
```rust
pub mod video_pipeline;
```

- [ ] **Step 3: Verify compilation**

Run: `cd tauri-client && cargo build 2>&1 | tail -20`
Expected: Compiles

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/video_pipeline.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(video): add video pipeline orchestrator (capture → encode → UDP)"
```

---

## Task 8: Video Receiver — Jitter Buffer, NACK, Frame Reassembly

**Files:**
- Create: `tauri-client/src-tauri/src/media/video_receiver.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Write tests for frame reassembly**

Create `tauri-client/src-tauri/src/media/video_receiver.rs` with tests first:

```rust
use std::collections::HashMap;
use std::net::UdpSocket;
use std::time::{Duration, Instant};

use super::video_packet::{
    UdpKeyframeRequest, UdpNackPacket, UdpVideoPacket, PACKET_TYPE_KEYFRAME_REQUEST,
    PACKET_TYPE_NACK, PACKET_TYPE_VIDEO, UDP_MAX_PAYLOAD,
};

/// A reassembled video frame ready for decoding.
#[derive(Debug, Clone)]
pub struct ReassembledFrame {
    pub frame_id: u32,
    pub data: Vec<u8>,
    pub is_keyframe: bool,
    pub streamer_username: String,
}

/// Tracks in-progress frame assembly.
struct FrameAssembly {
    total_packets: u16,
    received: HashMap<u16, Vec<u8>>, // packet_index -> payload
    is_keyframe: bool,
    created_at: Instant,
}

/// Jitter buffer and frame reassembly for incoming video packets.
pub struct VideoReceiver {
    frames_in_progress: HashMap<u32, FrameAssembly>, // frame_id -> assembly
    last_complete_frame_id: Option<u32>,
    nack_timeout: Duration,
    max_nack_retries: u32,
    nack_tracking: HashMap<(u32, u16), (Instant, u32)>, // (frame_id, pkt_idx) -> (last_nack_time, retry_count)
    buffer_depth: Duration,
}

impl VideoReceiver {
    pub fn new() -> Self {
        VideoReceiver {
            frames_in_progress: HashMap::new(),
            last_complete_frame_id: None,
            nack_timeout: Duration::from_millis(50), // ~1 RTT at typical latency
            max_nack_retries: 3,
            nack_tracking: HashMap::new(),
            buffer_depth: Duration::from_millis(50),
        }
    }

    /// Process an incoming video packet. Returns a complete frame if one is ready.
    pub fn process_packet(&mut self, pkt: &UdpVideoPacket) -> Option<ReassembledFrame> {
        let frame = self.frames_in_progress.entry(pkt.frame_id).or_insert_with(|| {
            FrameAssembly {
                total_packets: pkt.total_packets,
                received: HashMap::new(),
                is_keyframe: pkt.is_keyframe,
                created_at: Instant::now(),
            }
        });

        frame.received.insert(pkt.packet_index, pkt.payload_data().to_vec());

        // Check if frame is complete
        if frame.received.len() == frame.total_packets as usize {
            let assembly = self.frames_in_progress.remove(&pkt.frame_id).unwrap();
            self.last_complete_frame_id = Some(pkt.frame_id);

            // Clear NACK tracking for this frame
            self.nack_tracking.retain(|&(fid, _), _| fid != pkt.frame_id);

            // Reassemble in order
            let mut data = Vec::new();
            for i in 0..assembly.total_packets {
                if let Some(chunk) = assembly.received.get(&i) {
                    data.extend_from_slice(chunk);
                }
            }

            return Some(ReassembledFrame {
                frame_id: pkt.frame_id,
                data,
                is_keyframe: assembly.is_keyframe,
                streamer_username: pkt.sender_username(),
            });
        }

        None
    }

    /// Check for missing packets and return NACK requests to send.
    /// Also returns true if PLI should be sent (too many failures).
    pub fn check_missing(&mut self) -> (Vec<(u32, Vec<u16>)>, bool) {
        let now = Instant::now();
        let mut nacks: Vec<(u32, Vec<u16>)> = Vec::new();
        let mut need_pli = false;

        let mut stale_frames = Vec::new();

        for (&frame_id, assembly) in &self.frames_in_progress {
            // Skip frames older than buffer depth
            if assembly.created_at.elapsed() > self.buffer_depth * 3 {
                stale_frames.push(frame_id);
                continue;
            }

            // Find missing packet indices
            if assembly.created_at.elapsed() > self.nack_timeout {
                let mut missing = Vec::new();
                for i in 0..assembly.total_packets {
                    if !assembly.received.contains_key(&i) {
                        let key = (frame_id, i);
                        let entry = self.nack_tracking.entry(key).or_insert((Instant::now(), 0));
                        if entry.1 >= self.max_nack_retries {
                            need_pli = true;
                        } else if now.duration_since(entry.0) > self.nack_timeout {
                            missing.push(i);
                            entry.0 = now;
                            entry.1 += 1;
                        }
                    }
                }
                if !missing.is_empty() {
                    nacks.push((frame_id, missing));
                }
            }
        }

        // Clean up stale frames
        for frame_id in stale_frames {
            self.frames_in_progress.remove(&frame_id);
            self.nack_tracking.retain(|&(fid, _), _| fid != frame_id);
        }

        (nacks, need_pli)
    }

    /// Clean up old frame assemblies.
    pub fn cleanup_stale(&mut self) {
        let cutoff = Duration::from_millis(500);
        self.frames_in_progress.retain(|_, assembly| {
            assembly.created_at.elapsed() < cutoff
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_packet(frame_id: u32, index: u16, total: u16, keyframe: bool, data: &[u8]) -> UdpVideoPacket {
        UdpVideoPacket::new("streamer1", frame_id, index, total, keyframe, data)
    }

    #[test]
    fn single_packet_frame_completes_immediately() {
        let mut receiver = VideoReceiver::new();
        let pkt = make_packet(0, 0, 1, true, b"keyframe data");
        let result = receiver.process_packet(&pkt);
        assert!(result.is_some());
        let frame = result.unwrap();
        assert_eq!(frame.frame_id, 0);
        assert_eq!(frame.data, b"keyframe data");
        assert!(frame.is_keyframe);
    }

    #[test]
    fn multi_packet_frame_completes_on_last() {
        let mut receiver = VideoReceiver::new();

        // 3-packet frame
        let pkt0 = make_packet(1, 0, 3, false, b"part0");
        let pkt1 = make_packet(1, 1, 3, false, b"part1");
        let pkt2 = make_packet(1, 2, 3, false, b"part2");

        assert!(receiver.process_packet(&pkt0).is_none());
        assert!(receiver.process_packet(&pkt1).is_none());
        let result = receiver.process_packet(&pkt2);
        assert!(result.is_some());

        let frame = result.unwrap();
        assert_eq!(frame.data, b"part0part1part2");
    }

    #[test]
    fn out_of_order_packets_still_complete() {
        let mut receiver = VideoReceiver::new();

        let pkt2 = make_packet(1, 2, 3, false, b"c");
        let pkt0 = make_packet(1, 0, 3, false, b"a");
        let pkt1 = make_packet(1, 1, 3, false, b"b");

        assert!(receiver.process_packet(&pkt2).is_none());
        assert!(receiver.process_packet(&pkt0).is_none());
        let result = receiver.process_packet(&pkt1);
        assert!(result.is_some());
        assert_eq!(result.unwrap().data, b"abc"); // reassembled in order
    }

    #[test]
    fn missing_packet_detected() {
        let mut receiver = VideoReceiver::new();
        receiver.nack_timeout = Duration::from_millis(0); // immediate for test

        let pkt0 = make_packet(1, 0, 3, false, b"a");
        let pkt2 = make_packet(1, 2, 3, false, b"c");
        // pkt1 is missing

        receiver.process_packet(&pkt0);
        receiver.process_packet(&pkt2);

        let (nacks, _need_pli) = receiver.check_missing();
        assert_eq!(nacks.len(), 1);
        assert_eq!(nacks[0].0, 1); // frame_id
        assert_eq!(nacks[0].1, vec![1]); // missing index 1
    }
}
```

- [ ] **Step 2: Add module declaration**

In `tauri-client/src-tauri/src/media/mod.rs`, add:
```rust
pub mod video_receiver;
```

- [ ] **Step 3: Run tests**

Run: `cd tauri-client/src-tauri && cargo test video_receiver -- --nocapture`
Expected: All 4 tests pass

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src-tauri/src/media/video_receiver.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(receiver): add video receiver with jitter buffer and NACK generation"
```

---

## Task 9: VideoEngine & Tauri Commands

**Files:**
- Modify: `tauri-client/src-tauri/src/media/mod.rs`
- Create: `tauri-client/src-tauri/src/commands/streaming.rs`
- Modify: `tauri-client/src-tauri/src/commands/mod.rs`
- Modify: `tauri-client/src-tauri/src/state.rs`
- Modify: `tauri-client/src-tauri/src/lib.rs`

- [ ] **Step 1: Add VideoEngine to media/mod.rs**

Add to `tauri-client/src-tauri/src/media/mod.rs`:

```rust
pub mod video_pipeline;
pub mod video_receiver;

use std::thread::JoinHandle;
use tauri::AppHandle;

pub struct VideoEngine {
    pipeline_thread: Option<JoinHandle<()>>,
    receiver_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    pipeline_control_tx: std::sync::mpsc::Sender<video_pipeline::VideoPipelineControl>,
}

impl VideoEngine {
    pub fn stop(&mut self) {
        let _ = self.pipeline_control_tx.send(video_pipeline::VideoPipelineControl::Shutdown);
        if let Some(h) = self.pipeline_thread.take() { let _ = h.join(); }
        if let Some(h) = self.receiver_thread.take() { let _ = h.join(); }
        if let Some(h) = self.event_bridge.take() { h.abort(); }
    }

    pub fn force_keyframe(&self) {
        let _ = self.pipeline_control_tx.send(video_pipeline::VideoPipelineControl::ForceKeyframe);
    }
}

impl Drop for VideoEngine {
    fn drop(&mut self) {
        self.stop();
    }
}
```

- [ ] **Step 2: Add video_engine to AppState**

In `tauri-client/src-tauri/src/state.rs`:

```rust
use crate::media::{VoiceEngine, VideoEngine};

pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub credentials: Option<(String, String)>,
    pub voice_engine: Option<VoiceEngine>,
    pub video_engine: Option<VideoEngine>,
}
```

- [ ] **Step 3: Create streaming commands**

Create `tauri-client/src-tauri/src/commands/streaming.rs`:

```rust
use tauri::{AppHandle, State};
use crate::state::SharedState;
use crate::media::capture;

#[tauri::command]
pub async fn list_capture_sources() -> Result<Vec<capture::CaptureSource>, String> {
    capture::list_sources().await
}

#[tauri::command]
pub async fn start_screen_share(
    server_id: String,
    channel_id: String,
    source_id: String,
    resolution: String,     // "1080p", "720p", "source"
    fps: u32,               // 60, 30, 15
    quality: String,        // "high", "medium", "low"
    share_audio: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    if s.video_engine.is_some() {
        return Err("Already sharing screen".to_string());
    }

    // Resolve resolution
    let (width, height) = match resolution.as_str() {
        "720p" => (1280, 720),
        "source" => (0, 0), // determined by capture
        _ => (1920, 1080),  // default to 1080p
    };

    let bitrate_kbps = match quality.as_str() {
        "low" => 1500,
        "medium" => 3000,
        _ => 6000, // high
    };

    // Notify community server
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.start_stream(&channel_id, fps as i32, bitrate_kbps as i32, share_audio, width, height).await?;

    // NOTE: The actual capture + encode pipeline startup is a stub here.
    // The TCP signaling works (stream appears in active streams list), but
    // actual video frame capture/encoding requires the platform capture backends
    // (Task 5) to be fully implemented. When ready, this will:
    // 1. Call capture::start_capture(source_id, config) to get frame_rx
    // 2. Get Arc<UdpSocket> from the VoiceEngine (shared socket)
    // 3. Spawn video_pipeline::run_video_send_pipeline on a dedicated thread
    // 4. Store the VideoEngine handle in AppState

    Ok(())
}

#[tauri::command]
pub async fn stop_screen_share(
    server_id: String,
    channel_id: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // Stop video engine if running
    if let Some(mut engine) = s.video_engine.take() {
        engine.stop();
    }

    // Notify community server
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.stop_stream(&channel_id).await
}

#[tauri::command]
pub async fn watch_stream(
    server_id: String,
    channel_id: String,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.watch_stream(&channel_id, &target_username).await
}

#[tauri::command]
pub async fn stop_watching(
    server_id: String,
    channel_id: String,
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let client = s.communities.get(&server_id)
        .ok_or(format!("Not connected to community {}", server_id))?;
    client.stop_watching(&channel_id, &target_username).await
}

/// Frontend decoder calls this when WebCodecs encounters a decode error.
/// Sends a PLI (keyframe request) via UDP to the streamer through the server.
#[tauri::command]
pub async fn request_keyframe(
    target_username: String,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    // Send PLI via the voice engine's UDP socket
    // The UdpKeyframeRequest is sent as a UDP packet that the server relays
    if let Some(ref engine) = s.voice_engine {
        engine.send_keyframe_request(&target_username);
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}
```

- [ ] **Step 4: Register commands**

In `tauri-client/src-tauri/src/commands/mod.rs`, add:
```rust
pub mod streaming;
```

In `tauri-client/src-tauri/src/lib.rs`, add to `generate_handler!`:
```rust
commands::streaming::list_capture_sources,
commands::streaming::start_screen_share,
commands::streaming::stop_screen_share,
commands::streaming::watch_stream,
commands::streaming::stop_watching,
commands::streaming::request_keyframe,
```

- [ ] **Step 5: Verify compilation**

Run: `cd tauri-client && cargo build 2>&1 | tail -20`
Expected: Compiles

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/commands/streaming.rs tauri-client/src-tauri/src/commands/mod.rs tauri-client/src-tauri/src/state.rs tauri-client/src-tauri/src/media/mod.rs tauri-client/src-tauri/src/lib.rs
git commit -m "feat(commands): add screen sharing Tauri commands and VideoEngine"
```

---

## Task 10: Frontend — Store Extensions & Stream Events

**Files:**
- Modify: `tauri-client/src/stores/voiceStore.ts`
- Create: `tauri-client/src/features/voice/useStreamEvents.ts`

- [ ] **Step 1: Extend voiceStore with streaming state**

In `tauri-client/src/stores/voiceStore.ts`, add to the interface and implementation:

```typescript
// Add to VoiceState interface:
watching: string | null;          // username of stream we're watching
isStreaming: boolean;             // are we sharing our screen
streamSettings: {
  resolution: '1080p' | '720p' | 'source';
  fps: 60 | 30 | 15;
  quality: 'high' | 'medium' | 'low';
  shareAudio: boolean;
};
setWatching: (username: string | null) => void;
setIsStreaming: (streaming: boolean) => void;
setStreamSettings: (settings: Partial<VoiceState['streamSettings']>) => void;
```

Add to the create() implementation:
```typescript
watching: null,
isStreaming: false,
streamSettings: {
  resolution: '1080p',
  fps: 60,
  quality: 'high',
  shareAudio: false,
},
setWatching: (username) => set({ watching: username }),
setIsStreaming: (streaming) => set({ isStreaming: streaming }),
setStreamSettings: (settings) =>
  set((state) => ({
    streamSettings: { ...state.streamSettings, ...settings },
  })),
```

Update `disconnect()` to also reset streaming state:
```typescript
disconnect: () =>
  set({
    connectedServerId: null,
    connectedChannelId: null,
    participants: [],
    activeStreams: [],
    isMuted: false,
    isDeafened: false,
    speakingUsers: [],
    latencyMs: null,
    error: null,
    watching: null,
    isStreaming: false,
  }),
```

- [ ] **Step 2: Add stream_presence_updated listener to existing useVoiceEvents.ts**

In `tauri-client/src/features/voice/useVoiceEvents.ts`, add a new `listen` call alongside the existing voice event listeners:

```typescript
// Add import at top:
import type { StreamInfo } from "../../types";

// Add inside the useEffect, alongside existing listen calls:
const unlistenStreamPresence = listen("stream_presence_updated", (event: any) => {
  const { streams } = event.payload;
  const mapped: StreamInfo[] = streams.map((s: any) => ({
    streamId: s.streamId,
    ownerUsername: s.ownerUsername,
    hasAudio: s.hasAudio,
    resolutionWidth: s.resolutionWidth || 0,
    resolutionHeight: s.resolutionHeight || 0,
    fps: s.fps || 0,
  }));
  useVoiceStore.getState().setActiveStreams(mapped);

  // If we were watching someone who stopped streaming, clear watching
  const watching = useVoiceStore.getState().watching;
  if (watching && !mapped.some((s) => s.ownerUsername === watching)) {
    useVoiceStore.getState().setWatching(null);
  }
});

// Add to cleanup return:
return () => {
  // ...existing unlisten calls...
  unlistenStreamPresence.then((fn) => fn());
};
```

- [ ] **Step 3: Commit**

```bash
git add tauri-client/src/stores/voiceStore.ts tauri-client/src/features/voice/useVoiceEvents.ts
git commit -m "feat(store): add streaming state and stream presence event listener"
```

---

## Task 11: Frontend — Capture Source Picker Modal

**Files:**
- Create: `tauri-client/src/features/voice/CaptureSourcePicker.tsx`

- [ ] **Step 1: Create the picker modal**

Create `tauri-client/src/features/voice/CaptureSourcePicker.tsx`:

```tsx
import { useState, useEffect } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { stringToGradient } from "../../utils/colors";

interface CaptureSource {
  id: string;
  name: string;
  sourceType: "screen" | "window";
  width: number;
  height: number;
}

interface Props {
  serverId: string;
  channelId: string;
  onClose: () => void;
}

export default function CaptureSourcePicker({ serverId, channelId, onClose }: Props) {
  const [sources, setSources] = useState<CaptureSource[]>([]);
  const [tab, setTab] = useState<"screen" | "window">("screen");
  const [selected, setSelected] = useState<string | null>(null);
  const [loading, setLoading] = useState(true);
  const [error, setError] = useState<string | null>(null);
  const [starting, setStarting] = useState(false);

  const streamSettings = useVoiceStore((s) => s.streamSettings);
  const setStreamSettings = useVoiceStore((s) => s.setStreamSettings);

  useEffect(() => {
    invoke<CaptureSource[]>("list_capture_sources")
      .then((s) => {
        setSources(s);
        if (s.length > 0) setSelected(s[0].id);
      })
      .catch((e) => setError(String(e)))
      .finally(() => setLoading(false));
  }, []);

  const screens = sources.filter((s) => s.sourceType === "screen");
  const windows = sources.filter((s) => s.sourceType === "window");
  const displayed = tab === "screen" ? screens : windows;

  const handleGoLive = async () => {
    if (!selected) return;
    setStarting(true);
    setError(null);
    try {
      await invoke("start_screen_share", {
        serverId,
        channelId,
        sourceId: selected,
        resolution: streamSettings.resolution,
        fps: streamSettings.fps,
        quality: streamSettings.quality,
        shareAudio: streamSettings.shareAudio,
      });
      useVoiceStore.getState().setIsStreaming(true);
      onClose();
    } catch (e) {
      setError(String(e));
      setStarting(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && onClose()}
    >
      <div className="w-[560px] rounded-xl border border-border bg-bg-secondary shadow-2xl">
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-5 py-4">
          <h2 className="text-[15px] font-extrabold text-text-bright">
            Share Your Screen
          </h2>
          <button
            onClick={onClose}
            className="text-text-muted transition-colors hover:text-text-secondary"
          >
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        </div>

        {/* Tabs */}
        <div className="flex border-b-2 border-border px-5">
          <button
            onClick={() => setTab("screen")}
            className={`-mb-[2px] border-b-2 px-4 py-2.5 text-xs font-bold transition-colors ${
              tab === "screen"
                ? "border-accent text-accent"
                : "border-transparent text-text-muted hover:text-text-secondary"
            }`}
          >
            Screens
          </button>
          <button
            onClick={() => setTab("window")}
            className={`-mb-[2px] border-b-2 px-4 py-2.5 text-xs font-bold transition-colors ${
              tab === "window"
                ? "border-accent text-accent"
                : "border-transparent text-text-muted hover:text-text-secondary"
            }`}
          >
            Windows
          </button>
        </div>

        {/* Source grid */}
        <div className="grid grid-cols-2 gap-3 p-5">
          {loading && (
            <p className="col-span-2 py-8 text-center text-sm text-text-muted">
              Loading sources...
            </p>
          )}
          {!loading && displayed.length === 0 && (
            <p className="col-span-2 py-8 text-center text-sm text-text-muted">
              No {tab === "screen" ? "screens" : "windows"} found
            </p>
          )}
          {displayed.map((source) => (
            <button
              key={source.id}
              onClick={() => setSelected(source.id)}
              className={`overflow-hidden rounded-lg border-2 text-left transition-all ${
                selected === source.id
                  ? "border-accent"
                  : "border-border hover:border-text-muted"
              }`}
            >
              <div className="flex h-20 items-center justify-center bg-bg-primary">
                <span className="text-xs text-text-muted">
                  {source.width > 0
                    ? `${source.width} × ${source.height}`
                    : "Preview"}
                </span>
              </div>
              <div
                className={`px-3 py-2 text-[11px] font-semibold ${
                  selected === source.id
                    ? "bg-accent/10 text-text-bright"
                    : "text-text-secondary"
                }`}
              >
                {source.name}
              </div>
            </button>
          ))}
        </div>

        {/* Quality settings */}
        <div className="mx-5 flex gap-2.5 rounded-lg bg-bg-primary p-3">
          <div className="flex-1">
            <label className="mb-1 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Resolution
            </label>
            <select
              value={streamSettings.resolution}
              onChange={(e) => setStreamSettings({ resolution: e.target.value as any })}
              className="w-full rounded-md bg-surface-hover px-2.5 py-1.5 text-xs text-text-bright outline-none"
            >
              <option value="source">Source</option>
              <option value="1080p">1080p</option>
              <option value="720p">720p</option>
            </select>
          </div>
          <div className="flex-1">
            <label className="mb-1 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Frame Rate
            </label>
            <select
              value={streamSettings.fps}
              onChange={(e) => setStreamSettings({ fps: Number(e.target.value) as any })}
              className="w-full rounded-md bg-surface-hover px-2.5 py-1.5 text-xs text-text-bright outline-none"
            >
              <option value={60}>60 FPS</option>
              <option value={30}>30 FPS</option>
              <option value={15}>15 FPS</option>
            </select>
          </div>
          <div className="flex-1">
            <label className="mb-1 block text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              Quality
            </label>
            <select
              value={streamSettings.quality}
              onChange={(e) => setStreamSettings({ quality: e.target.value as any })}
              className="w-full rounded-md bg-surface-hover px-2.5 py-1.5 text-xs text-text-bright outline-none"
            >
              <option value="high">High</option>
              <option value="medium">Medium</option>
              <option value="low">Low</option>
            </select>
          </div>
        </div>

        {/* Bottom bar */}
        <div className="flex items-center justify-between px-5 py-4">
          <label className="flex cursor-pointer items-center gap-2.5 text-xs text-text-secondary">
            <div
              onClick={() => setStreamSettings({ shareAudio: !streamSettings.shareAudio })}
              className={`relative h-5 w-9 rounded-full transition-colors ${
                streamSettings.shareAudio ? "bg-accent" : "bg-surface-hover"
              }`}
            >
              <div
                className={`absolute top-0.5 h-4 w-4 rounded-full bg-white transition-transform ${
                  streamSettings.shareAudio ? "translate-x-4" : "translate-x-0.5"
                }`}
              />
            </div>
            Share audio
          </label>
          <button
            onClick={handleGoLive}
            disabled={!selected || starting}
            className="rounded-lg bg-accent px-6 py-2 text-[13px] font-bold text-white transition-colors hover:bg-accent-hover disabled:opacity-50"
          >
            {starting ? "Starting..." : "Go Live"}
          </button>
        </div>

        {error && (
          <p className="px-5 pb-3 text-xs text-error">{error}</p>
        )}
      </div>
    </div>,
    document.body
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src/features/voice/CaptureSourcePicker.tsx
git commit -m "feat(ui): add capture source picker modal with quality settings"
```

---

## Task 11b: Frontend — StreamVideoPlayer (WebCodecs Decoder)

This is the core viewer component that decodes H.264 frames via the WebCodecs API and renders them to a `<canvas>`.

**Files:**
- Create: `tauri-client/src/features/voice/StreamVideoPlayer.tsx`

- [ ] **Step 1: Create the WebCodecs decoder component**

Create `tauri-client/src/features/voice/StreamVideoPlayer.tsx`:

```tsx
import { useEffect, useRef, useCallback } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";

interface Props {
  streamerUsername: string;
  className?: string;
}

interface StreamFramePayload {
  username: string;
  data: number[];  // H.264 NAL unit bytes (binary via Tauri event)
  timestamp: number;
  keyframe: boolean;
}

export default function StreamVideoPlayer({ streamerUsername, className }: Props) {
  const canvasRef = useRef<HTMLCanvasElement>(null);
  const decoderRef = useRef<VideoDecoder | null>(null);
  const ctxRef = useRef<CanvasRenderingContext2D | null>(null);

  const handleDecoderError = useCallback((e: DOMException) => {
    console.error("[StreamVideoPlayer] Decoder error:", e);
    // Request keyframe via PLI so we can recover
    invoke("request_keyframe", { targetUsername: streamerUsername }).catch(console.error);
    // Reset decoder
    if (decoderRef.current && decoderRef.current.state !== "closed") {
      decoderRef.current.reset();
      decoderRef.current.configure({
        codec: "avc1.640028", // H.264 High Profile Level 4.0
        hardwareAcceleration: "prefer-hardware",
      });
    }
  }, [streamerUsername]);

  useEffect(() => {
    const canvas = canvasRef.current;
    if (!canvas) return;

    ctxRef.current = canvas.getContext("2d");

    // Initialize WebCodecs VideoDecoder
    const decoder = new VideoDecoder({
      output: (frame: VideoFrame) => {
        const ctx = ctxRef.current;
        if (ctx && canvas) {
          // Resize canvas to match frame if needed
          if (canvas.width !== frame.displayWidth || canvas.height !== frame.displayHeight) {
            canvas.width = frame.displayWidth;
            canvas.height = frame.displayHeight;
          }
          ctx.drawImage(frame, 0, 0);
        }
        frame.close();
      },
      error: handleDecoderError,
    });

    decoder.configure({
      codec: "avc1.640028", // H.264 High Profile Level 4.0
      hardwareAcceleration: "prefer-hardware",
    });

    decoderRef.current = decoder;

    // Listen for stream-frame events from the Rust video receiver
    const unlisten = listen<StreamFramePayload>("stream_frame", (event) => {
      const { username, data, timestamp, keyframe } = event.payload;
      if (username !== streamerUsername) return;

      if (decoder.state === "closed") return;

      try {
        const chunk = new EncodedVideoChunk({
          type: keyframe ? "key" : "delta",
          timestamp: timestamp,
          data: new Uint8Array(data),
        });
        decoder.decode(chunk);
      } catch (e) {
        console.error("[StreamVideoPlayer] Decode error:", e);
      }
    });

    return () => {
      unlisten.then((fn) => fn());
      if (decoder.state !== "closed") {
        decoder.close();
      }
      decoderRef.current = null;
    };
  }, [streamerUsername, handleDecoderError]);

  return (
    <canvas
      ref={canvasRef}
      className={className ?? "h-full w-full object-contain"}
    />
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src/features/voice/StreamVideoPlayer.tsx
git commit -m "feat(ui): add StreamVideoPlayer component with WebCodecs H.264 decoder"
```

---

## Task 12: Frontend — Stream View Panel

**Files:**
- Create: `tauri-client/src/features/voice/StreamViewPanel.tsx`

- [ ] **Step 1: Create the stream view panel**

Create `tauri-client/src/features/voice/StreamViewPanel.tsx`:

```tsx
import { useState, useRef, useCallback } from "react";
import { useVoiceStore } from "../../stores/voiceStore";
import { stringToGradient } from "../../utils/colors";
import { invoke } from "@tauri-apps/api/core";
import StreamVideoPlayer from "./StreamVideoPlayer";

export default function StreamViewPanel() {
  const watching = useVoiceStore((s) => s.watching);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);

  const [theaterMode, setTheaterMode] = useState(false);
  const [overlayVisible, setOverlayVisible] = useState(false);
  const overlayTimeout = useRef<ReturnType<typeof setTimeout>>();

  const stream = activeStreams.find((s) => s.ownerUsername === watching);

  const handleMouseMove = useCallback(() => {
    if (!theaterMode) return;
    setOverlayVisible(true);
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    overlayTimeout.current = setTimeout(() => setOverlayVisible(false), 3000);
  }, [theaterMode]);

  const handleMouseLeave = useCallback(() => {
    if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
    setOverlayVisible(false);
  }, []);

  const handleSwitchStream = async (username: string) => {
    if (!connectedServerId || !connectedChannelId) return;
    // Stop watching current
    if (watching) {
      await invoke("stop_watching", {
        serverId: connectedServerId,
        channelId: connectedChannelId,
        targetUsername: watching,
      }).catch(() => {});
    }
    // Start watching new
    await invoke("watch_stream", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
      targetUsername: username,
    }).catch(() => {});
    useVoiceStore.getState().setWatching(username);
  };

  const handleStopWatching = async () => {
    if (!watching || !connectedServerId || !connectedChannelId) return;
    await invoke("stop_watching", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
      targetUsername: watching,
    }).catch(() => {});
    useVoiceStore.getState().setWatching(null);
    setTheaterMode(false);
  };

  if (!watching || !stream) return null;

  const resLabel = stream.resolutionWidth > 0
    ? `${stream.resolutionHeight}p`
    : "";
  const fpsLabel = stream.fps > 0 ? `${stream.fps}fps` : "";
  const qualityBadge = [resLabel, fpsLabel].filter(Boolean).join(" · ");

  // Theater mode
  if (theaterMode) {
    return (
      <div
        className="relative flex flex-1 cursor-none items-center justify-center bg-black"
        onMouseMove={handleMouseMove}
        onMouseLeave={handleMouseLeave}
      >
        <StreamVideoPlayer streamerUsername={watching} className="h-full w-full object-contain" />

        {/* Overlay controls */}
        <div
          className={`absolute inset-x-0 bottom-0 bg-gradient-to-t from-black/90 to-transparent px-4 pb-3 pt-8 transition-opacity duration-300 ${
            overlayVisible ? "opacity-100" : "pointer-events-none opacity-0"
          }`}
          onMouseEnter={() => {
            if (overlayTimeout.current) clearTimeout(overlayTimeout.current);
            setOverlayVisible(true);
          }}
        >
          <div className="flex items-center justify-between">
            <div className="flex items-center gap-2">
              <div
                className="flex h-[22px] w-[22px] items-center justify-center rounded-md text-[10px] font-bold text-white"
                style={{ background: stringToGradient(watching) }}
              >
                {watching.charAt(0).toUpperCase()}
              </div>
              <span className="text-xs font-semibold text-white">
                {watching}'s screen
              </span>
              {qualityBadge && (
                <span className="text-[10px] text-white/60">{qualityBadge}</span>
              )}
            </div>
            <div className="flex items-center gap-2">
              {/* Participant avatars */}
              <div className="flex -space-x-1.5">
                {participants.slice(0, 4).map((p) => (
                  <div
                    key={p.username}
                    className="flex h-6 w-6 items-center justify-center rounded-md border-2 border-black text-[9px] font-bold text-white"
                    style={{ background: stringToGradient(p.username) }}
                  >
                    {p.username.charAt(0).toUpperCase()}
                  </div>
                ))}
              </div>
              <button
                onClick={() => setTheaterMode(false)}
                className="flex h-7 w-7 items-center justify-center rounded-md border border-white/20 bg-bg-primary/50 text-sm transition-colors hover:bg-bg-primary"
                title="Exit theater mode"
              >
                ⛶
              </button>
            </div>
          </div>
        </div>
      </div>
    );
  }

  // Focused view (default)
  return (
    <div className="flex flex-1 bg-bg-primary">
      {/* Main stream area */}
      <div className="flex min-w-0 flex-1 flex-col p-2">
        {/* Stream header */}
        <div className="mb-1.5 flex items-center gap-2">
          <div
            className="flex h-5 w-5 items-center justify-center rounded-md text-[9px] font-bold text-white"
            style={{ background: stringToGradient(watching) }}
          >
            {watching.charAt(0).toUpperCase()}
          </div>
          <span className="text-xs font-bold text-text-bright">
            {watching}'s screen
          </span>
          {qualityBadge && (
            <span className="text-[10px] text-text-muted">{qualityBadge}</span>
          )}
          <div className="ml-auto flex gap-1">
            <button
              onClick={handleStopWatching}
              className="rounded-md px-2 py-1 text-[10px] font-semibold text-error transition-colors hover:bg-error/10"
            >
              Stop
            </button>
            <button
              onClick={() => setTheaterMode(true)}
              className="flex h-6 w-6 items-center justify-center rounded-md bg-surface-hover text-xs transition-colors hover:bg-border"
              title="Theater mode"
            >
              ⛶
            </button>
          </div>
        </div>

        {/* Video canvas */}
        <div className="flex flex-1 items-center justify-center rounded-lg border border-border bg-bg-tertiary">
          <StreamVideoPlayer streamerUsername={watching} className="h-full w-full rounded-lg object-contain" />
        </div>
      </div>

      {/* Right sidebar: participants */}
      <div className="flex w-[140px] shrink-0 flex-col gap-1 border-l border-border p-2">
        <h4 className="px-1 text-[9px] font-bold uppercase tracking-wider text-text-muted">
          Voice — {participants.length}
        </h4>

        {participants.map((p) => {
          const isStreaming = activeStreams.some((s) => s.ownerUsername === p.username);
          const isSpeaking = speakingUsers.includes(p.username);
          return (
            <div key={p.username} className="flex items-center gap-1.5 rounded-md p-1.5">
              <div
                className={`flex h-7 w-7 items-center justify-center rounded-md text-[11px] font-bold text-white ${
                  isSpeaking ? "ring-2 ring-success ring-offset-1 ring-offset-bg-primary" : ""
                }`}
                style={{ background: stringToGradient(p.username) }}
              >
                {p.username.charAt(0).toUpperCase()}
              </div>
              <div className="min-w-0">
                <div className="truncate text-[11px] font-semibold text-text-secondary">
                  {p.username}
                </div>
                {isStreaming && (
                  <div className="text-[9px] text-error">📺 Streaming</div>
                )}
              </div>
            </div>
          );
        })}

        {/* Stream switcher */}
        {activeStreams.length > 1 && (
          <div className="mt-auto border-t border-border pt-2">
            <h4 className="mb-1 px-1 text-[9px] font-bold uppercase tracking-wider text-text-muted">
              Streams
            </h4>
            {activeStreams.map((s) => (
              <button
                key={s.ownerUsername}
                onClick={() => handleSwitchStream(s.ownerUsername)}
                className={`w-full rounded-md px-2 py-1.5 text-left text-[10px] font-semibold transition-colors ${
                  s.ownerUsername === watching
                    ? "border-l-2 border-accent bg-accent/10 text-accent"
                    : "text-text-secondary hover:bg-surface-hover"
                }`}
              >
                📺 {s.ownerUsername}'s screen
              </button>
            ))}
          </div>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Commit**

```bash
git add tauri-client/src/features/voice/StreamViewPanel.tsx
git commit -m "feat(ui): add stream view panel with focused and theater modes"
```

---

## Task 13: Frontend — Voice Control Bar & Panel Integration

**Files:**
- Modify: `tauri-client/src/features/voice/VoiceControlBar.tsx`
- Modify: `tauri-client/src/features/voice/VoicePanel.tsx`

- [ ] **Step 1: Add Share Screen button to VoiceControlBar**

In `tauri-client/src/features/voice/VoiceControlBar.tsx`, add:

```tsx
// Import at top:
import { useState } from "react";
import CaptureSourcePicker from "./CaptureSourcePicker";

// Inside the component, add state:
const isStreaming = useVoiceStore((s) => s.isStreaming);
const activeStreams = useVoiceStore((s) => s.activeStreams);
const connectedServerId = useVoiceStore((s) => s.connectedServerId);
const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
const [showPicker, setShowPicker] = useState(false);

// Add handler:
const handleStopSharing = async () => {
  if (!connectedServerId || !connectedChannelId) return;
  try {
    await invoke("stop_screen_share", {
      serverId: connectedServerId,
      channelId: connectedChannelId,
    });
    useVoiceStore.getState().setIsStreaming(false);
  } catch (e) {
    console.error("Stop share failed:", e);
  }
};
```

Add the share screen button in the controls area (next to mute/deafen buttons):

```tsx
{/* Share Screen / Stop Sharing */}
{isStreaming ? (
  <button
    onClick={handleStopSharing}
    className="flex h-8 items-center gap-1.5 rounded-md bg-error/20 px-3 text-[11px] font-semibold text-error transition-colors hover:bg-error/30"
  >
    <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <line x1="8" y1="21" x2="16" y2="21" />
      <line x1="12" y1="17" x2="12" y2="21" />
    </svg>
    Stop Sharing
  </button>
) : (
  <button
    onClick={() => setShowPicker(true)}
    className="flex h-8 w-8 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
    title="Share Screen"
  >
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
      <rect x="2" y="3" width="20" height="14" rx="2" />
      <line x1="8" y1="21" x2="16" y2="21" />
      <line x1="12" y1="17" x2="12" y2="21" />
    </svg>
  </button>
)}

{/* Active streams indicator */}
{activeStreams.length > 0 && !isStreaming && (
  <span className="text-[10px] font-semibold text-accent">
    {activeStreams.length} stream{activeStreams.length > 1 ? "s" : ""}
  </span>
)}

{/* Picker modal */}
{showPicker && connectedServerId && connectedChannelId && (
  <CaptureSourcePicker
    serverId={connectedServerId}
    channelId={connectedChannelId}
    onClose={() => setShowPicker(false)}
  />
)}
```

- [ ] **Step 2: Integrate StreamViewPanel into VoicePanel**

In `tauri-client/src/features/voice/VoicePanel.tsx`, add:

```tsx
import StreamViewPanel from "./StreamViewPanel";

// Inside VoicePanel component:
// (stream events are handled by useVoiceEvents.ts, no separate hook needed)
const watching = useVoiceStore((s) => s.watching);
const activeStreams = useVoiceStore((s) => s.activeStreams);

// In the render, conditionally show stream view:
{watching ? (
  <StreamViewPanel />
) : (
  // existing participant grid
  <div className="flex flex-1 items-center justify-center">
    {/* existing voice participant cards */}
    {activeStreams.length > 0 && (
      <div className="mt-4 flex flex-col gap-1">
        {activeStreams.map((s) => (
          <button
            key={s.ownerUsername}
            onClick={() => handleWatchStream(s.ownerUsername)}
            className="rounded-lg bg-accent/10 px-4 py-2 text-xs font-semibold text-accent transition-colors hover:bg-accent/20"
          >
            📺 Watch {s.ownerUsername}'s screen
          </button>
        ))}
      </div>
    )}
  </div>
)}
```

Add watch handler:
```tsx
const handleWatchStream = async (username: string) => {
  const serverId = useVoiceStore.getState().connectedServerId;
  const channelId = useVoiceStore.getState().connectedChannelId;
  if (!serverId || !channelId) return;
  try {
    await invoke("watch_stream", { serverId, channelId, targetUsername: username });
    useVoiceStore.getState().setWatching(username);
  } catch (e) {
    console.error("Watch stream failed:", e);
  }
};
```

- [ ] **Step 3: Verify frontend builds**

Run: `cd tauri-client && npm run build 2>&1 | tail -10`
Expected: Successful build

- [ ] **Step 4: Commit**

```bash
git add tauri-client/src/features/voice/VoiceControlBar.tsx tauri-client/src/features/voice/VoicePanel.tsx
git commit -m "feat(ui): add share screen button and stream viewing integration"
```

---

## Task 14: Integration Wiring & Final Verification

**Files:**
- Modify: various files for final wiring

- [ ] **Step 1: Verify stream event handling is wired**

Confirm that `stream_presence_updated` listener was added to `useVoiceEvents.ts` (done in Task 10, Step 2). Verify the hook is mounted in a component that persists while in a voice channel.

- [ ] **Step 3: Verify full build**

Run both builds:
```bash
cd /home/sun/Desktop/decibell/decibell && cmake --build build 2>&1 | tail -5
cd tauri-client && cargo build 2>&1 | tail -5 && npm run build 2>&1 | tail -5
```
Expected: All three compile successfully

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "feat(phase5): complete screen sharing integration wiring"
```

---

## Summary of Task Dependencies

```
Task 1 (Proto) ──┬──→ Task 4 (Community methods) ──→ Task 9 (Commands)
                  │
Task 2 (Packets) ─┼──→ Task 7 (Video pipeline) ────→ Task 9
                  │
Task 3 (C++ SFU) ─┘
                       Task 5 (Capture) ────────────→ Task 7
                       Task 6 (Encoder) ────────────→ Task 7
                       Task 8 (Receiver) ───────────→ Task 9

Task 9 (Commands) ──→ Task 10 (Store/Events) ──→ Task 11 (Picker)
                                                 ──→ Task 11b (VideoPlayer)
                                                 ──→ Task 12 (View, depends on 11b)
                                                 ──→ Task 13 (Integration)
                                                      ↓
                                                 Task 14 (Final wiring)
```

Tasks 1, 2, 3 can run in parallel.
Tasks 5, 6 can run in parallel.
Tasks 11, 11b can run in parallel after Task 10. Task 12 depends on 11b.
