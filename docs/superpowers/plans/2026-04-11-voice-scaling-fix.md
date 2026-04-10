# Voice Pipeline Scaling Fix — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Fix voice chat degradation with 3+ users by separating audio/video UDP paths on server and client, compacting video packets, and adding jitter buffer auto-recovery.

**Architecture:** Server gets two UDP sockets (voice on TCP+1, media on TCP+2) with independent receive chains. Client mirrors this with two sockets and two recv threads. Video packets become variable-length. Jitter buffer resets after 200ms of continuous packet loss.

**Tech Stack:** C++ (Boost.Asio), Rust (std::net::UdpSocket, cpal, ringbuf), protobuf

**Files:**
- Modify: `src/community/main.cpp`
- Modify: `tauri-client/src-tauri/src/media/video_packet.rs`
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`
- Modify: `tauri-client/src-tauri/src/media/video_pipeline.rs`
- Modify: `tauri-client/src-tauri/src/media/audio_stream_pipeline.rs`

---

### Task 1: Compact Video Packets

**Files:**
- Modify: `tauri-client/src-tauri/src/media/video_packet.rs:82-93`

This is a self-contained change with no dependencies. Makes video packets variable-length (header + actual payload) instead of fixed 1445 bytes. Cuts bandwidth ~28%.

- [ ] **Step 1: Replace `UdpVideoPacket::to_bytes()` with compact serialization**

In `tauri-client/src-tauri/src/media/video_packet.rs`, replace the `to_bytes` method (lines 82-93):

```rust
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
```

With compact serialization that only sends the header (45 bytes) + actual payload:

```rust
    /// Serialize to a compact byte vector: header (45 bytes) + actual payload only.
    /// This saves hundreds of bytes per packet vs the old fixed 1445-byte format.
    /// The C++ server broadcasts `bytes_recvd` so compact packets relay correctly.
    pub fn to_bytes(&self) -> Vec<u8> {
        let ps = { self.payload_size } as usize;
        let header_size = std::mem::size_of::<Self>() - UDP_MAX_PAYLOAD; // 45 bytes
        let total = header_size + ps;
        let mut buf = vec![0u8; total];
        // Copy the fixed header fields (everything before payload)
        unsafe {
            std::ptr::copy_nonoverlapping(
                self as *const Self as *const u8,
                buf.as_mut_ptr(),
                header_size,
            );
        }
        // Copy only the actual payload bytes
        buf[header_size..header_size + ps].copy_from_slice(&self.payload[..ps]);
        buf
    }
```

- [ ] **Step 2: Update the video packet size check constant**

The recv thread in `pipeline.rs` currently checks `n == VIDEO_PACKET_SIZE` to identify video packets. With compact packets, video packets are variable-length. However, this check will be removed entirely in Task 4 when we split recv threads. For now, update the check to be size-agnostic. In `tauri-client/src-tauri/src/media/pipeline.rs`, find the recv thread (line 1643):

```rust
                if packet_type == PACKET_TYPE_VIDEO && n == VIDEO_PACKET_SIZE {
```

Replace with:

```rust
                if packet_type == PACKET_TYPE_VIDEO {
```

Also remove the unused `VIDEO_PACKET_SIZE` constant (line 1610):

```rust
    const VIDEO_PACKET_SIZE: usize = std::mem::size_of::<UdpVideoPacket>();
```

And update `RECV_BUF_SIZE` (lines 1611-1615) to just use the larger of the two max sizes:

```rust
    const RECV_BUF_SIZE: usize = PACKET_TOTAL_SIZE.max(std::mem::size_of::<UdpVideoPacket>());
```

- [ ] **Step 3: Update the existing video packet roundtrip test**

In `tauri-client/src-tauri/src/media/video_packet.rs`, update the `video_packet_size_matches_cpp` test (lines 226-229). The struct size is still 1445, but `to_bytes()` now produces a smaller buffer. Replace:

```rust
    #[test]
    fn video_packet_size_matches_cpp() {
        // C++ struct: 1 + 32 + 4 + 2 + 2 + 2 + 1 + 1 + 1400 = 1445 bytes
        assert_eq!(std::mem::size_of::<UdpVideoPacket>(), 1445);
    }
```

With:

```rust
    #[test]
    fn video_packet_size_matches_cpp() {
        // C++ struct: 1 + 32 + 4 + 2 + 2 + 2 + 1 + 1 + 1400 = 1445 bytes
        assert_eq!(std::mem::size_of::<UdpVideoPacket>(), 1445);
    }

    #[test]
    fn video_packet_compact_serialization() {
        let data = b"hello video frame";
        let pkt = UdpVideoPacket::new("testuser", 42, 0, 3, true, data);
        let bytes = pkt.to_bytes();
        // Header (45 bytes) + payload (17 bytes) = 62 bytes, NOT 1445
        let header_size = std::mem::size_of::<UdpVideoPacket>() - UDP_MAX_PAYLOAD;
        assert_eq!(bytes.len(), header_size + data.len());
        // Should still round-trip correctly
        let decoded = UdpVideoPacket::from_bytes(&bytes).unwrap();
        assert_eq!(decoded.payload_data(), data);
        assert_eq!(decoded.sender_username(), "testuser");
    }
```

- [ ] **Step 4: Verify it compiles and tests pass**

Run: `cd tauri-client/src-tauri && cargo test --lib media::video_packet 2>&1 | tail -15`
Expected: all tests pass, including the new `video_packet_compact_serialization` test.

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add tauri-client/src-tauri/src/media/video_packet.rs tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "perf(video): compact video packet serialization, ~28% bandwidth reduction"
```

---

### Task 2: Jitter Buffer Auto-Recovery

**Files:**
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:91-166` (JitterBuffer struct)
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:1369-1412` (jitter drain loop)

Self-contained client-side change. Adds automatic reset when the jitter buffer produces 10+ consecutive PLC frames (200ms of unintelligible audio).

- [ ] **Step 1: Add `consecutive_losses` field and `reset()` method to JitterBuffer**

In `tauri-client/src-tauri/src/media/pipeline.rs`, replace the JitterBuffer struct and impl (lines 91-166):

```rust
struct JitterBuffer {
    packets: HashMap<u16, Vec<u8>>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
}
```

With:

```rust
struct JitterBuffer {
    packets: HashMap<u16, Vec<u8>>,
    next_seq: u16,
    initialized: bool,
    ready: bool,
    /// Consecutive drain() calls that returned a missing packet (PLC).
    /// When this exceeds the threshold, the buffer resets to re-sync.
    consecutive_losses: u32,
}
```

Update `JitterBuffer::new()` (line 99-101) to initialize the new field:

```rust
    fn new() -> Self {
        Self { packets: HashMap::new(), next_seq: 0, initialized: false, ready: false, consecutive_losses: 0 }
    }
```

- [ ] **Step 2: Update `drain()` to track losses and auto-reset**

Replace the existing `drain` method (lines 150-158):

```rust
    fn drain(&mut self) -> Option<Option<Vec<u8>>> {
        if !self.ready { return None; }
        // Once initialized, always produce output — return PLC for missing
        // packets instead of going silent. Re-buffering gaps cause loud pops
        // at the silence→audio transition.
        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        Some(self.packets.remove(&seq))
    }
```

With:

```rust
    /// Pop the next frame. Returns:
    /// - `Some(Some(data))` — packet present, decode normally
    /// - `Some(None)` — packet missing, caller should do PLC
    /// - `None` — buffer not ready (initial fill or post-reset re-buffering)
    fn drain(&mut self) -> Option<Option<Vec<u8>>> {
        if !self.ready { return None; }

        // Auto-recovery: if we've produced 10+ consecutive PLC frames (200ms),
        // the audio is already unintelligible. Reset and re-buffer from scratch
        // so playback can resume cleanly once packets arrive.
        if self.consecutive_losses >= 10 {
            self.reset();
            return None;
        }

        let seq = self.next_seq;
        self.next_seq = self.next_seq.wrapping_add(1);
        let result = self.packets.remove(&seq);
        if result.is_some() {
            self.consecutive_losses = 0;
        } else {
            self.consecutive_losses += 1;
        }
        Some(result)
    }

    /// Reset the buffer to its initial state, forcing a re-buffering period.
    /// Called automatically after prolonged packet loss.
    fn reset(&mut self) {
        self.packets.clear();
        self.initialized = false;
        self.ready = false;
        self.consecutive_losses = 0;
    }
```

- [ ] **Step 3: Reset `voice_drain_time` and clear `decoded_voice` on jitter reset**

In the voice jitter drain loop (around line 1376-1412), the `drain()` returning `None` after a reset will hit the `None => break` arm and exit the while loop. But we also need to reset the drain timer and clear stale samples. Find this block:

```rust
            // ── Voice jitter buffer ──
            while drain_now.duration_since(peer.voice_drain_time) >= frame_dur {
                peer.voice_drain_time += frame_dur;
                let opus_opt = match peer.voice_jitter.drain() {
                    Some(v) => v,
                    None => break, // not ready or empty
                };
```

Replace with:

```rust
            // ── Voice jitter buffer ──
            while drain_now.duration_since(peer.voice_drain_time) >= frame_dur {
                peer.voice_drain_time += frame_dur;
                let opus_opt = match peer.voice_jitter.drain() {
                    Some(v) => v,
                    None => {
                        // Buffer not ready (initial fill or auto-recovery reset).
                        // Reset drain time to now to prevent accumulated time debt
                        // from causing a burst of catch-up decodes when packets resume.
                        peer.voice_drain_time = drain_now;
                        peer.decoded_voice.clear();
                        break;
                    }
                };
```

Do the same for the stream audio jitter drain (around line 1418). Find:

```rust
                    let opus_opt = match peer.stream_jitter.drain() {
                        Some(v) => v,
                        None => break,
                    };
```

Replace with:

```rust
                    let opus_opt = match peer.stream_jitter.drain() {
                        Some(v) => v,
                        None => {
                            peer.stream_drain_time = drain_now;
                            break;
                        }
                    };
```

- [ ] **Step 4: Add logging when jitter buffer resets**

In the `drain()` method, add a log line inside the `reset()` method, after `self.consecutive_losses = 0;`:

Actually, `reset()` doesn't know the peer's username. Instead, add logging in the drain loop where `None` is returned after a reset. Update the voice jitter `None` arm from Step 3 to:

```rust
                    None => {
                        // Buffer not ready (initial fill or auto-recovery reset).
                        if peer.voice_drain_time != drain_now {
                            eprintln!("[pipeline] Jitter buffer reset for peer '{}' — re-buffering", username);
                        }
                        peer.voice_drain_time = drain_now;
                        peer.decoded_voice.clear();
                        break;
                    }
```

Note: `username` is already the loop variable from the `for (username, peer) in remote_peers.iter_mut()` loop.

- [ ] **Step 5: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors.

- [ ] **Step 6: Commit**

```bash
git add tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "fix(voice): jitter buffer auto-recovery after 200ms continuous packet loss"
```

---

### Task 3: Server — Separate Voice and Media UDP Sockets

**Files:**
- Modify: `src/community/main.cpp`

The server gets a second UDP socket for media traffic (VIDEO, FEC, KEYFRAME_REQUEST, NACK). Voice traffic (AUDIO, STREAM_AUDIO, PING) stays on the existing socket. Each socket has its own async receive chain.

- [ ] **Step 1: Add media endpoint to Session class**

In `src/community/main.cpp`, find the Session class's private members. After `udp_endpoint_` (line 393), add a media endpoint:

```cpp
    boost::asio::ip::udp::endpoint udp_endpoint_;
    boost::asio::ip::udp::endpoint udp_media_endpoint_;
```

Add getter/setter after the existing ones (after line 131):

```cpp
    void set_udp_endpoint(const boost::asio::ip::udp::endpoint& ep) { udp_endpoint_ = ep; }
    boost::asio::ip::udp::endpoint get_udp_endpoint() const { return udp_endpoint_; }
    void set_udp_media_endpoint(const boost::asio::ip::udp::endpoint& ep) { udp_media_endpoint_ = ep; }
    boost::asio::ip::udp::endpoint get_udp_media_endpoint() const { return udp_media_endpoint_; }
```

- [ ] **Step 2: Add media socket pointer to SessionManager**

In the `SessionManager` class, add a media socket pointer. After `set_udp_socket` (line 63):

```cpp
    void set_udp_socket(boost::asio::ip::udp::socket* sock) { udp_socket_ptr_ = sock; }
    void set_media_udp_socket(boost::asio::ip::udp::socket* sock) { media_udp_socket_ptr_ = sock; }
```

In the private section, after `udp_socket_ptr_` (find it near the bottom of the private section — around line 83), add:

```cpp
    boost::asio::ip::udp::socket* udp_socket_ptr_ = nullptr;
    boost::asio::ip::udp::socket* media_udp_socket_ptr_ = nullptr;
```

- [ ] **Step 3: Update `broadcast_to_watchers` to use media endpoint and media socket**

Find `broadcast_to_watchers` (line 694). Replace:

```cpp
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

With:

```cpp
void SessionManager::broadcast_to_watchers(const char* data, size_t length, const std::string& channel_id,
                                            const std::string& streamer_username, boost::asio::ip::udp::socket& udp_socket) {
    auto buffer = std::make_shared<std::vector<char>>(data, data + length);
    std::lock_guard<std::mutex> lock(mutex_);
    auto ch_it = stream_watchers_.find(channel_id);
    if (ch_it == stream_watchers_.end()) return;
    auto st_it = ch_it->second.find(streamer_username);
    if (st_it == ch_it->second.end()) return;
    for (auto& watcher : st_it->second) {
        if (watcher->get_udp_media_endpoint().port() != 0) {
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), watcher->get_udp_media_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
        }
    }
}
```

- [ ] **Step 4: Update `relay_keyframe_request` and `relay_nack` to use media endpoint**

Find `relay_keyframe_request` (around line 643). Change `get_udp_endpoint()` to `get_udp_media_endpoint()`:

```cpp
void SessionManager::relay_keyframe_request(const std::string& target_username, boost::asio::ip::udp::socket& udp_socket) {
    // ... existing code to build the PLI packet ...
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        if (session->get_username() == target_username && session->get_udp_media_endpoint().port() != 0) {
            auto buffer = std::make_shared<std::vector<char>>(reinterpret_cast<const char*>(&req),
                                                               reinterpret_cast<const char*>(&req) + sizeof(req));
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_media_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
            return;
        }
    }
}
```

Do the same in `relay_nack` (around line 663) — change `get_udp_endpoint()` to `get_udp_media_endpoint()`:

```cpp
void SessionManager::relay_nack(const char* data, size_t length, const std::string& target_username, boost::asio::ip::udp::socket& udp_socket) {
    std::lock_guard<std::mutex> lock(mutex_);
    for (auto& session : sessions_) {
        if (session->get_username() == target_username && session->get_udp_media_endpoint().port() != 0) {
            auto buffer = std::make_shared<std::vector<char>>(data, data + length);
            udp_socket.async_send_to(
                boost::asio::buffer(*buffer), session->get_udp_media_endpoint(),
                [buffer](boost::system::error_code, std::size_t) {});
            return;
        }
    }
}
```

- [ ] **Step 5: Add second UDP socket to CommunityServer**

In the `CommunityServer` class (line 825), add the media socket. Update the constructor and private members.

After the existing `udp_socket_` initialization in the constructor (line 829):

```cpp
          udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 1)),
```

Add:

```cpp
          media_udp_socket_(io_context, boost::asio::ip::udp::endpoint(boost::asio::ip::udp::v4(), port + 2)),
```

After the existing buffer size settings (lines 846-847), add the same for the media socket:

```cpp
        // Voice UDP socket buffers
        udp_socket_.set_option(boost::asio::socket_base::receive_buffer_size(2 * 1024 * 1024));
        udp_socket_.set_option(boost::asio::socket_base::send_buffer_size(2 * 1024 * 1024));

        // Media UDP socket buffers
        media_udp_socket_.set_option(boost::asio::socket_base::receive_buffer_size(2 * 1024 * 1024));
        media_udp_socket_.set_option(boost::asio::socket_base::send_buffer_size(2 * 1024 * 1024));
```

Register the media socket with SessionManager. After `manager_.set_udp_socket(&udp_socket_);` (line 849):

```cpp
        manager_.set_udp_socket(&udp_socket_);
        manager_.set_media_udp_socket(&media_udp_socket_);
```

Update the startup log (lines 851-852):

```cpp
        std::cout << "Community Server TCP running on port " << port << "...\n";
        std::cout << "Community Server Voice UDP running on port " << port + 1 << "...\n";
        std::cout << "Community Server Media UDP running on port " << port + 2 << "...\n";
```

Start both receive chains (after `do_receive_udp();` at line 855, which we'll rename):

```cpp
        do_accept();
        do_receive_voice_udp();
        do_receive_media_udp();
```

In the private members section (after line 974), add the media socket, buffer, and endpoint:

```cpp
    boost::asio::ip::udp::socket udp_socket_;
    boost::asio::ip::udp::socket media_udp_socket_;
    char udp_buffer_[sizeof(chatproj::UdpVideoPacket) > sizeof(chatproj::UdpFecPacket) ? sizeof(chatproj::UdpVideoPacket) : sizeof(chatproj::UdpFecPacket)];
    char media_udp_buffer_[sizeof(chatproj::UdpVideoPacket) > sizeof(chatproj::UdpFecPacket) ? sizeof(chatproj::UdpVideoPacket) : sizeof(chatproj::UdpFecPacket)];
    boost::asio::ip::udp::endpoint udp_sender_endpoint_;
    boost::asio::ip::udp::endpoint media_udp_sender_endpoint_;
```

- [ ] **Step 6: Rename `do_receive_udp` to `do_receive_voice_udp` and strip media packet handling**

Rename the existing `do_receive_udp()` method (line 870) to `do_receive_voice_udp()`. Remove the handling for VIDEO, FEC, KEYFRAME_REQUEST, and NACK packet types. The voice receive chain only handles AUDIO, STREAM_AUDIO, and PING.

The `do_receive_voice_udp` should:
1. Use `udp_socket_`, `udp_buffer_`, `udp_sender_endpoint_` (unchanged)
2. Only handle packet types AUDIO (0), STREAM_AUDIO (6), and PING (5)
3. Call `session->set_udp_endpoint()` (voice endpoint) when a session is found
4. Call `do_receive_voice_udp()` at the end to continue the chain
5. Ignore/drop any other packet types that arrive on this socket

Replace the entire `do_receive_udp` method with:

```cpp
    void do_receive_voice_udp() {
        udp_socket_.async_receive_from(
            boost::asio::buffer(udp_buffer_, sizeof(udp_buffer_)), udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                if (!ec && bytes_recvd >= 1) {
                    uint8_t packet_type = static_cast<uint8_t>(udp_buffer_[0]);

                    // PING: echo back immediately
                    if (packet_type == chatproj::UdpPacketType::PING) {
                        auto echo_buf = std::make_shared<std::vector<uint8_t>>(
                            udp_buffer_, udp_buffer_ + bytes_recvd);
                        udp_socket_.async_send_to(
                            boost::asio::buffer(*echo_buf), udp_sender_endpoint_,
                            [echo_buf](boost::system::error_code, std::size_t) {});
                        do_receive_voice_udp();
                        return;
                    }

                    // AUDIO or STREAM_AUDIO
                    if ((packet_type == chatproj::UdpPacketType::AUDIO ||
                         packet_type == chatproj::UdpPacketType::STREAM_AUDIO) &&
                        bytes_recvd >= 1 + chatproj::SENDER_ID_SIZE + 4) {

                        std::string token_str;
                        chatproj::UdpAudioPacket* packet = reinterpret_cast<chatproj::UdpAudioPacket*>(udp_buffer_);
                        for (int i = 0; i < chatproj::SENDER_ID_SIZE; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }

                        if (!token_str.empty()) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            if (session) {
                                if (session->get_udp_endpoint() != udp_sender_endpoint_) {
                                    session->set_udp_endpoint(udp_sender_endpoint_);
                                }
                                std::string channel = session->get_current_voice_channel();
                                if (!channel.empty()) {
                                    std::string uname = session->get_username();
                                    std::memset(udp_buffer_ + 1, 0, chatproj::SENDER_ID_SIZE);
                                    std::memcpy(udp_buffer_ + 1, uname.c_str(),
                                                std::min(uname.size(), size_t(chatproj::SENDER_ID_SIZE - 1)));

                                    if (packet_type == chatproj::UdpPacketType::AUDIO) {
                                        manager_.broadcast_to_voice_channel(
                                            udp_buffer_, bytes_recvd, channel, session, udp_socket_);
                                    } else if (packet_type == chatproj::UdpPacketType::STREAM_AUDIO) {
                                        manager_.broadcast_to_watchers(
                                            udp_buffer_, bytes_recvd, channel, uname, udp_socket_);
                                    }
                                }
                            }
                        }
                    }
                }
                do_receive_voice_udp();
            });
    }
```

Note: `broadcast_to_watchers` for STREAM_AUDIO uses the voice socket (`udp_socket_`) and voice endpoints — stream audio stays on the voice path per the spec.

- [ ] **Step 7: Add `do_receive_media_udp` method**

Add a new method to `CommunityServer` that handles VIDEO, FEC, KEYFRAME_REQUEST, and NACK on the media socket. Add this after `do_receive_voice_udp`:

```cpp
    void do_receive_media_udp() {
        media_udp_socket_.async_receive_from(
            boost::asio::buffer(media_udp_buffer_, sizeof(media_udp_buffer_)), media_udp_sender_endpoint_,
            [this](boost::system::error_code ec, std::size_t bytes_recvd) {
                if (!ec && bytes_recvd >= 1) {
                    uint8_t packet_type = static_cast<uint8_t>(media_udp_buffer_[0]);
                    constexpr int SID = chatproj::SENDER_ID_SIZE;

                    // KEYFRAME_REQUEST: relay to the target streamer
                    if (packet_type == chatproj::UdpPacketType::KEYFRAME_REQUEST &&
                        bytes_recvd >= sizeof(chatproj::UdpKeyframeRequest)) {
                        chatproj::UdpKeyframeRequest* packet =
                            reinterpret_cast<chatproj::UdpKeyframeRequest*>(media_udp_buffer_);
                        std::string target;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->target_username[i] == '\0') break;
                            target.push_back(packet->target_username[i]);
                        }
                        if (!target.empty()) {
                            manager_.relay_keyframe_request(target, media_udp_socket_);
                        }
                        do_receive_media_udp();
                        return;
                    }

                    // NACK: relay to the target streamer
                    if (packet_type == chatproj::UdpPacketType::NACK &&
                        bytes_recvd >= sizeof(chatproj::UdpNackPacket) - sizeof(uint16_t) * chatproj::NACK_MAX_ENTRIES) {
                        chatproj::UdpNackPacket* packet =
                            reinterpret_cast<chatproj::UdpNackPacket*>(media_udp_buffer_);
                        std::string target;
                        for (int i = 0; i < SID; ++i) {
                            if (packet->target_username[i] == '\0') break;
                            target.push_back(packet->target_username[i]);
                        }
                        if (!target.empty()) {
                            manager_.relay_nack(media_udp_buffer_, bytes_recvd, target, media_udp_socket_);
                        }
                        do_receive_media_udp();
                        return;
                    }

                    // VIDEO or FEC: authenticate, rewrite sender_id, broadcast to watchers
                    if ((packet_type == chatproj::UdpPacketType::VIDEO ||
                         packet_type == chatproj::UdpPacketType::FEC) &&
                        bytes_recvd >= 1 + SID + 8) {

                        std::string token_str;
                        chatproj::UdpVideoPacket* packet =
                            reinterpret_cast<chatproj::UdpVideoPacket*>(media_udp_buffer_);
                        for (int i = 0; i < SID; ++i) {
                            if (packet->sender_id[i] == '\0') break;
                            token_str.push_back(packet->sender_id[i]);
                        }

                        if (!token_str.empty()) {
                            auto session = manager_.find_session_by_token(token_str, jwt_secret_);
                            if (session) {
                                if (session->get_udp_media_endpoint() != media_udp_sender_endpoint_) {
                                    session->set_udp_media_endpoint(media_udp_sender_endpoint_);
                                }
                                std::string channel = session->get_current_voice_channel();
                                if (!channel.empty()) {
                                    std::string uname = session->get_username();
                                    std::memset(media_udp_buffer_ + 1, 0, SID);
                                    std::memcpy(media_udp_buffer_ + 1, uname.c_str(),
                                                std::min(uname.size(), size_t(SID - 1)));

                                    manager_.broadcast_to_watchers(
                                        media_udp_buffer_, bytes_recvd, channel, uname, media_udp_socket_);
                                }
                            }
                        }
                    }
                }
                do_receive_media_udp();
            });
    }
```

- [ ] **Step 8: Verify it compiles**

Run: `cd src/community && mkdir -p build && cd build && cmake .. && make -j$(nproc) 2>&1 | tail -10`
(Or whatever the existing build command is for the community server.)

Expected: compiles with no errors.

- [ ] **Step 9: Commit**

```bash
git add src/community/main.cpp
git commit -m "feat(server): separate voice and media UDP sockets

Voice (AUDIO, STREAM_AUDIO, PING) on port TCP+1.
Media (VIDEO, FEC, NACK, KEYFRAME_REQUEST) on port TCP+2.
Independent receive chains prevent video from starving voice."
```

---

### Task 4: Client — Dual UDP Sockets and Recv Threads

**Files:**
- Modify: `tauri-client/src-tauri/src/media/mod.rs:49-184` (VoiceEngine::start)
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:648-657` (run_audio_pipeline signature)
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:844-857` (recv thread spawn)
- Modify: `tauri-client/src-tauri/src/media/pipeline.rs:1594-1684` (recv thread function)

The client creates two UDP sockets (voice and media) and spawns two dedicated recv threads. The voice recv thread feeds `audio_pkt_tx`, the media recv thread feeds `video_tx`.

- [ ] **Step 1: Create two UDP sockets in `VoiceEngine::start`**

In `tauri-client/src-tauri/src/media/mod.rs`, find the socket creation section (lines 60-156). Replace the single socket creation with two sockets.

Replace:

```rust
        // UDP audio port is server_port + 1
        let udp_addr = format!("{}:{}", server_host, server_port + 1);

        // Sender ID: last 31 chars of JWT
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        // Create and connect UDP socket (shared between audio + video pipelines)
        let socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("UDP bind failed: {}", e))?;
        socket
            .connect(&udp_addr)
            .map_err(|e| format!("UDP connect failed: {}", e))?;
```

With:

```rust
        // Voice UDP port is server_port + 1, media UDP port is server_port + 2
        let voice_udp_addr = format!("{}:{}", server_host, server_port + 1);
        let media_udp_addr = format!("{}:{}", server_host, server_port + 2);

        // Sender ID: last 31 chars of JWT
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        // Voice socket — carries AUDIO, STREAM_AUDIO, PING
        let voice_socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Voice UDP bind failed: {}", e))?;
        voice_socket
            .connect(&voice_udp_addr)
            .map_err(|e| format!("Voice UDP connect failed: {}", e))?;

        // Media socket — carries VIDEO, FEC, KEYFRAME_REQUEST, NACK
        let media_socket = UdpSocket::bind("0.0.0.0:0")
            .map_err(|e| format!("Media UDP bind failed: {}", e))?;
        media_socket
            .connect(&media_udp_addr)
            .map_err(|e| format!("Media UDP connect failed: {}", e))?;
```

Then apply the existing socket buffer/DSCP settings to BOTH sockets. The existing block (lines 77-155) applies settings to `socket`. Wrap it in a helper closure or duplicate for both. The simplest approach — extract into a helper:

After the two socket creations, add:

```rust
        // Apply buffer sizes and DSCP to both sockets
        fn configure_udp_socket(socket: &UdpSocket) {
            #[cfg(unix)]
            {
                use std::os::fd::AsRawFd;
                let fd = socket.as_raw_fd();
                let buf_size: libc::c_int = 4 * 1024 * 1024;
                unsafe {
                    libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_RCVBUF,
                        &buf_size as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                    libc::setsockopt(fd, libc::SOL_SOCKET, libc::SO_SNDBUF,
                        &buf_size as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                    let dscp_ef: libc::c_int = 0xB8;
                    libc::setsockopt(fd, libc::IPPROTO_IP, libc::IP_TOS,
                        &dscp_ef as *const _ as *const libc::c_void,
                        std::mem::size_of::<libc::c_int>() as libc::socklen_t);
                }
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawSocket;
                let sock = socket.as_raw_socket();
                let buf_size: i32 = 4 * 1024 * 1024;
                unsafe {
                    let _ = windows::Win32::Networking::WinSock::setsockopt(
                        windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                        windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                        windows::Win32::Networking::WinSock::SO_RCVBUF as i32,
                        Some(std::slice::from_raw_parts(
                            &buf_size as *const i32 as *const u8, std::mem::size_of::<i32>(),
                        )),
                    );
                    let _ = windows::Win32::Networking::WinSock::setsockopt(
                        windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                        windows::Win32::Networking::WinSock::SOL_SOCKET as i32,
                        windows::Win32::Networking::WinSock::SO_SNDBUF as i32,
                        Some(std::slice::from_raw_parts(
                            &buf_size as *const i32 as *const u8, std::mem::size_of::<i32>(),
                        )),
                    );
                    let dscp_ef: i32 = 0xB8;
                    let _ = windows::Win32::Networking::WinSock::setsockopt(
                        windows::Win32::Networking::WinSock::SOCKET(sock as usize),
                        windows::Win32::Networking::WinSock::IPPROTO_IP.0 as i32,
                        windows::Win32::Networking::WinSock::IP_TOS as i32,
                        Some(std::slice::from_raw_parts(
                            &dscp_ef as *const i32 as *const u8, std::mem::size_of::<i32>(),
                        )),
                    );
                    let wsa_sock = windows::Win32::Networking::WinSock::SOCKET(sock as usize);
                    const SIO_UDP_CONNRESET: u32 = 0x9800000C;
                    let mut false_val: u32 = 0;
                    let mut bytes_returned: u32 = 0;
                    let _ = windows::Win32::Networking::WinSock::WSAIoctl(
                        wsa_sock, SIO_UDP_CONNRESET,
                        Some(&false_val as *const u32 as *const std::ffi::c_void),
                        std::mem::size_of::<u32>() as u32, None, 0,
                        &mut bytes_returned, None, None,
                    );
                }
            }
        }
        configure_udp_socket(&voice_socket);
        configure_udp_socket(&media_socket);
        let voice_socket = Arc::new(voice_socket);
        let media_socket = Arc::new(media_socket);
```

Remove the old per-platform socket config blocks and the `let socket = Arc::new(socket);` line.

- [ ] **Step 2: Update thread spawning to use separate sockets**

Replace the audio thread spawn (lines 167-174):

```rust
        let socket_for_audio = socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(socket_for_audio, sender_id_for_audio, control_rx, event_tx, video_packet_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;
```

With:

```rust
        let voice_socket_for_audio = voice_socket.clone();
        let sender_id_for_audio = sender_id.clone();
        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(voice_socket_for_audio, sender_id_for_audio, control_rx, event_tx, video_packet_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;
```

Replace the video recv thread spawn (lines 177-184):

```rust
        let socket_for_video_recv = socket.clone();
        let sender_id_for_video = sender_id.clone();
        let video_recv_thread = thread::Builder::new()
            .name("decibell-video-recv".to_string())
            .spawn(move || {
                run_video_recv_thread(video_packet_rx, socket_for_video_recv, sender_id_for_video, event_tx_video);
            })
            .map_err(|e| format!("Failed to spawn video recv thread: {}", e))?;
```

With:

```rust
        let media_socket_for_video_recv = media_socket.clone();
        let sender_id_for_video = sender_id.clone();
        let video_recv_thread = thread::Builder::new()
            .name("decibell-video-recv".to_string())
            .spawn(move || {
                run_video_recv_thread(video_packet_rx, media_socket_for_video_recv, sender_id_for_video, event_tx_video);
            })
            .map_err(|e| format!("Failed to spawn video recv thread: {}", e))?;
```

- [ ] **Step 3: Update VoiceEngine struct to store both sockets**

Replace the `socket` field in the struct (line 39) and constructor return:

In the struct definition:

```rust
pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    video_recv_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    voice_socket: Arc<UdpSocket>,
    media_socket: Arc<UdpSocket>,
    sender_id: String,
    is_muted: bool,
    is_deafened: bool,
    was_muted_before_deafen: bool,
    keyframe_tx: Arc<std::sync::Mutex<Option<mpsc::Sender<video_pipeline::VideoPipelineControl>>>>,
}
```

Update the return in `start()` (line 279):

```rust
        Ok(VoiceEngine {
            audio_thread: Some(audio_thread),
            video_recv_thread: Some(video_recv_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            voice_socket,
            media_socket,
            sender_id,
            is_muted: false,
            is_deafened: false,
            was_muted_before_deafen: false,
            keyframe_tx,
        })
```

Update the `socket()` method (line 385) to return the appropriate sockets:

```rust
    pub fn voice_socket(&self) -> Arc<UdpSocket> { self.voice_socket.clone() }
    pub fn media_socket(&self) -> Arc<UdpSocket> { self.media_socket.clone() }
```

- [ ] **Step 4: Update callers of `socket()` to use `voice_socket()` or `media_socket()`**

Search the codebase for uses of `voice_engine.socket()` or `.socket()` on VoiceEngine. These are typically in the Tauri commands that start the video pipeline and audio stream pipeline. Update them:

- Video pipeline (`VideoEngine::start`): should receive `media_socket`
- Audio stream pipeline: should receive `voice_socket`

Find where `VideoEngine::start` is called and ensure it gets `voice_engine.media_socket()`.
Find where the audio stream pipeline is started and ensure it gets `voice_engine.voice_socket()`.

- [ ] **Step 5: Split the recv thread in `pipeline.rs`**

In `tauri-client/src-tauri/src/media/pipeline.rs`, the `run_audio_pipeline` function currently spawns a single `udp_recv_thread`. Since the voice and media sockets are now separate, and video packets come via the `video_packet_tx` channel from the video recv thread (in `mod.rs`), the audio pipeline's recv thread only needs to handle voice packets.

Replace the recv thread function (lines 1604-1684) with a simplified voice-only version:

```rust
// ── Dedicated voice UDP receive thread ──────────────────────────────────────
//
// Reads voice packets (AUDIO, STREAM_AUDIO, PING) from the voice UDP socket
// and forwards them to the audio processing thread. Video packets arrive on
// a separate media socket handled by the video recv thread in mod.rs.

fn voice_recv_thread(
    socket: Arc<UdpSocket>,
    audio_tx: std::sync::mpsc::SyncSender<Vec<u8>>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // Voice packets are at most AUDIO_HEADER_SIZE + MAX_PAYLOAD_SIZE
    const RECV_BUF_SIZE: usize = PACKET_TOTAL_SIZE;

    if let Err(e) = socket.set_read_timeout(Some(Duration::from_millis(1))) {
        let _ = event_tx.send(VoiceEvent::Error(format!("voice recv thread: set_read_timeout: {}", e)));
        return;
    }

    let mut buf = [0u8; RECV_BUF_SIZE];
    let mut recv_count: u64 = 0;
    let mut recv_log_time = Instant::now();

    loop {
        match socket.recv(&mut buf) {
            Ok(n) if n >= 1 => {
                recv_count += 1;
                if recv_log_time.elapsed() >= Duration::from_secs(5) {
                    eprintln!("[voice-recv] 5s stats: packets={}", recv_count);
                    recv_count = 0;
                    recv_log_time = Instant::now();
                }

                // Forward all packets on the voice socket to the audio thread.
                // No type classification needed — only voice packets arrive here.
                match audio_tx.try_send(buf[..n].to_vec()) {
                    Ok(()) => {}
                    Err(std::sync::mpsc::TrySendError::Full(_)) => {
                        // Audio thread is behind — drop this packet.
                        // The jitter buffer's PLC will smooth the gap.
                    }
                    Err(std::sync::mpsc::TrySendError::Disconnected(_)) => {
                        break;
                    }
                }
            }
            Ok(_) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::ConnectionReset
                    || e.raw_os_error() == Some(997)
                    || e.raw_os_error() == Some(10054)
            => {}
            Err(e) => {
                eprintln!("[voice-recv] Socket error: {}", e);
                break;
            }
        }
    }

    eprintln!("[voice-recv] Exiting");
}
```

Update the recv thread spawn in the main pipeline function (lines 844-857). The `video_packet_tx` parameter is no longer needed here since video packets come from the media recv thread in `mod.rs`. However, `run_audio_pipeline` still receives `video_packet_tx` because the video recv thread in `mod.rs` sends assembled frames through events, not through this channel. Actually — looking at the architecture, the `video_packet_tx` is passed from `mod.rs` into `run_audio_pipeline`, and the old recv thread forwarded raw video bytes to it. Now the media recv thread in `mod.rs` feeds `video_packet_rx` directly.

So `run_audio_pipeline` no longer needs the `video_packet_tx` parameter. Update the signature (line 651):

```rust
pub fn run_audio_pipeline(
    socket: Arc<UdpSocket>,
    sender_id: String,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
```

Update the recv thread spawn (lines 844-857):

```rust
    let (audio_pkt_tx, audio_pkt_rx) = std::sync::mpsc::sync_channel::<Vec<u8>>(256);
    let recv_socket = Arc::clone(&socket);
    let recv_event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("decibell-voice-recv".to_string())
        .spawn(move || {
            voice_recv_thread(recv_socket, audio_pkt_tx, recv_event_tx);
        })
        .expect("spawn voice recv thread");
```

Update the call site in `mod.rs` (line 172):

```rust
                pipeline::run_audio_pipeline(voice_socket_for_audio, sender_id_for_audio, control_rx, event_tx);
```

- [ ] **Step 6: Add media recv thread to `mod.rs` video recv thread**

The `run_video_recv_thread` in `mod.rs` currently receives video packets via `packet_rx` (an mpsc channel that the old unified recv thread fed). Now it should read directly from the media socket instead. Update its signature (line 398):

```rust
fn run_video_recv_thread(
    socket: Arc<UdpSocket>,
    sender_id: String,
    event_tx: std::sync::mpsc::Sender<pipeline::VoiceEvent>,
) {
```

Replace the packet receive loop. Instead of `packet_rx.recv_timeout()`, read directly from the media socket. Set a read timeout and recv in a loop:

Replace the loop (lines 462-505):

```rust
    // Set short read timeout so we can do periodic maintenance
    let _ = socket.set_read_timeout(Some(std::time::Duration::from_millis(5)));
    let mut recv_buf = [0u8; std::mem::size_of::<video_packet::UdpVideoPacket>()];

    loop {
        // Read from media socket
        match socket.recv(&mut recv_buf) {
            Ok(n) if n >= 1 => {
                let packet_type = recv_buf[0];

                if packet_type == video_packet::PACKET_TYPE_VIDEO {
                    if let Some(pkt) = UdpVideoPacket::from_bytes(&recv_buf[..n]) {
                        let username = pkt.sender_username();
                        if video_streamer_username.as_deref() != Some(&username) {
                            has_received_keyframe = false;
                        }
                        video_streamer_username = Some(username.clone());
                        if let Some(frame) = video_receiver.process_packet(&pkt) {
                            video_frames_received += 1;
                            if frame.is_keyframe || video_frames_received % 300 == 1 {
                                eprintln!("[video-recv] Frame {} reassembled: {} bytes, keyframe={} (total={})",
                                    frame.frame_id, frame.data.len(), frame.is_keyframe, video_frames_received);
                            }
                            if frame.is_keyframe { has_received_keyframe = true; }
                            emit_frame(frame);
                        }
                    }
                } else if packet_type == video_packet::PACKET_TYPE_KEYFRAME_REQUEST && n >= std::mem::size_of::<UdpKeyframeRequest>() {
                    eprintln!("[video-recv] Keyframe request received, signaling encoder");
                    let _ = event_tx.send(pipeline::VoiceEvent::KeyframeRequested);
                }
                // NACK and FEC packets from other sources are also received here
                // but currently only the server sends NACKs/FEC — ignore on client
            }
            Ok(_) => {}
            Err(ref e)
                if e.kind() == std::io::ErrorKind::WouldBlock
                    || e.kind() == std::io::ErrorKind::TimedOut
                    || e.kind() == std::io::ErrorKind::ConnectionReset
                    || e.raw_os_error() == Some(997)
                    || e.raw_os_error() == Some(10054)
            => {}
            Err(_) => break,
        }

        // Periodic maintenance (existing code, unchanged)
```

Remove the `packet_rx` parameter and the old channel-based receive code. The `video_packet_tx` channel in `mod.rs` `VoiceEngine::start` is no longer needed — remove it and the corresponding `video_packet_rx`.

- [ ] **Step 7: Remove the `video_packet_tx` channel from `VoiceEngine::start`**

In `mod.rs`, remove the channel creation (line 161):

```rust
        let (video_packet_tx, video_packet_rx) = mpsc::channel::<Vec<u8>>();
```

Update the video recv thread spawn to not pass `video_packet_rx`:

```rust
        let media_socket_for_video_recv = media_socket.clone();
        let sender_id_for_video = sender_id.clone();
        let video_recv_thread = thread::Builder::new()
            .name("decibell-video-recv".to_string())
            .spawn(move || {
                run_video_recv_thread(media_socket_for_video_recv, sender_id_for_video, event_tx_video);
            })
            .map_err(|e| format!("Failed to spawn video recv thread: {}", e))?;
```

Remove `video_packet_tx` from the audio thread spawn since `run_audio_pipeline` no longer takes it.

Also update the keyframe request handling in the audio pipeline's packet drain (lines 1351-1354 in pipeline.rs). Currently keyframe requests arriving on the old shared socket are forwarded via events. With separate sockets, keyframe requests arrive on the media socket and are handled by the video recv thread. Remove the keyframe request handling from `pipeline.rs`'s packet drain loop:

```rust
                    } else if !raw.is_empty() && raw[0] == PACKET_TYPE_KEYFRAME_REQUEST {
                        eprintln!("[recv] Keyframe request received, signaling encoder");
                        let _ = event_tx.send(VoiceEvent::KeyframeRequested);
                    }
```

This block can be removed since keyframe requests now go to the media socket → video recv thread.

- [ ] **Step 8: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -10`
Expected: compiles. There may be warnings about unused imports (e.g., `UdpVideoPacket` in pipeline.rs) — clean those up.

- [ ] **Step 9: Commit**

```bash
git add tauri-client/src-tauri/src/media/mod.rs tauri-client/src-tauri/src/media/pipeline.rs
git commit -m "feat(voice): separate voice and media UDP sockets on client

Voice socket (port TCP+1): AUDIO, STREAM_AUDIO, PING
Media socket (port TCP+2): VIDEO, FEC, NACK, KEYFRAME_REQUEST
Independent recv threads prevent video from starving voice."
```

---

### Task 5: Wire Media Socket Through Video and Stream Audio Pipelines

**Files:**
- Modify: `tauri-client/src-tauri/src/media/mod.rs` (VideoEngine::start)
- Modify: `tauri-client/src-tauri/src/media/video_pipeline.rs:104-113`
- Modify: `tauri-client/src-tauri/src/media/audio_stream_pipeline.rs:24-31`

Ensure the video send pipeline uses the media socket and the audio stream pipeline uses the voice socket.

- [ ] **Step 1: Find where VideoEngine::start and audio stream pipeline are called**

Search for call sites that pass the socket to `VideoEngine::start` and the audio stream pipeline. These are typically in Tauri commands (likely in a commands module). Find them:

Run: `grep -rn "VideoEngine::start\|run_audio_stream_pipeline\|\.socket()" tauri-client/src-tauri/src/ --include="*.rs" | grep -v "test" | head -20`

Update each call site:
- `VideoEngine::start(...)` should receive `voice_engine.media_socket()`
- Audio stream pipeline start should receive `voice_engine.voice_socket()`

The `VideoEngine::start` already takes `socket: Arc<UdpSocket>` — no signature change needed, just pass the right socket at the call site.

Similarly, `run_audio_stream_pipeline` already takes `socket: Arc<UdpSocket>` — just pass the voice socket at the call site.

- [ ] **Step 2: Verify it compiles**

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors.

- [ ] **Step 3: Commit**

```bash
git add -A
git commit -m "feat(voice): wire media socket to video pipeline, voice socket to stream audio"
```

---

### Task 6: Final Verification

- [ ] **Step 1: Run all Rust tests**

Run: `cd tauri-client && npx tsc --noEmit 2>&1 | tail -5`
Expected: no TypeScript errors.

Run: `cd tauri-client/src-tauri && cargo test 2>&1 | tail -15`
Expected: all tests pass.

Run: `cd tauri-client/src-tauri && cargo check 2>&1 | tail -5`
Expected: compiles with no errors or warnings (clean up any remaining unused import warnings).

- [ ] **Step 2: Commit any cleanup**

If there were unused import cleanups:

```bash
git add -A
git commit -m "chore: clean up unused imports after UDP socket split"
```
