# Phase 4: Voice Chat Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add real-time voice chat — users can join voice channels, hear others, and speak via their microphone. The C++ community server already handles voice channel management and UDP audio relay.

**Architecture:** Dedicated OS audio thread (CPAL + Opus) isolated from Tokio, communicating via mpsc channels. UDP for audio packets, TCP for signaling (join/leave). VoiceEngine stored in AppState, Tauri events bridge audio thread state to React frontend.

**Tech Stack:** Rust (cpal, audiopus, std::net::UdpSocket), C++ (Boost.Asio UDP echo), React/TypeScript/Zustand/Tailwind

**Spec:** `docs/superpowers/specs/2026-03-21-tauri-rebuild-phase4-design.md`

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `tauri-client/src-tauri/src/media/packet.rs` | UDP audio packet serialization/deserialization matching C++ `UdpAudioPacket` layout |
| `tauri-client/src-tauri/src/media/codec.rs` | Opus encoder/decoder wrappers (48kHz, mono, 20ms frames) |
| `tauri-client/src-tauri/src/media/speaking.rs` | Noise-gated RMS amplitude speaking detection with hysteresis |
| `tauri-client/src-tauri/src/media/pipeline.rs` | Audio thread main loop: CPAL capture → Opus encode → UDP send, UDP recv → Opus decode → mix → CPAL playback |
| `tauri-client/src-tauri/src/commands/voice.rs` | Tauri commands: join_voice_channel, leave_voice_channel, set_voice_mute, set_voice_deafen |
| `tauri-client/src/features/voice/VoiceControlBar.tsx` | Compact voice controls at bottom of channel sidebar |
| `tauri-client/src/features/voice/VoiceParticipantList.tsx` | Inline participant list nested under active voice channel |
| `tauri-client/src/features/voice/VoicePanel.tsx` | Full voice view with participant cards and speaking glow rings |
| `tauri-client/src/features/voice/useVoiceEvents.ts` | Listens for all voice Tauri events, dispatches to voiceStore |

### Modified Files

| File | Changes |
|------|---------|
| `src/common/udp_packet.hpp` | Add `PING = 5` to `UdpPacketType` enum |
| `src/community/main.cpp` | Add UDP ping echo handler before token-extraction in `do_receive_udp()` |
| `tauri-client/src-tauri/Cargo.toml` | Add `cpal` and `audiopus` dependencies |
| `tauri-client/src-tauri/src/media/mod.rs` | Replace placeholder with VoiceEngine struct and public API |
| `tauri-client/src-tauri/src/state.rs` | Add `voice_engine: Option<VoiceEngine>` to AppState |
| `tauri-client/src-tauri/src/events/mod.rs` | Add voice event constants, payloads, and emit helpers |
| `tauri-client/src-tauri/src/net/community.rs` | Add `join_voice_channel()`, `leave_voice_channel()` methods, handle `VoicePresenceUpdate` in `route_packets` |
| `tauri-client/src-tauri/src/commands/mod.rs` | Add `pub mod voice;` |
| `tauri-client/src-tauri/src/lib.rs` | Register voice commands |
| `tauri-client/src/stores/voiceStore.ts` | Expand with speakingUsers, isDeafened, latencyMs, error, connectedServerId, disconnect action |
| `tauri-client/src/stores/uiStore.ts` | Add `"voice"` to activeView union type |
| `tauri-client/src/features/channels/ChannelSidebar.tsx` | Make voice channels clickable, show VoiceParticipantList and VoiceControlBar |
| `tauri-client/src/layouts/MainLayout.tsx` | Add voice view routing branch, mount useVoiceEvents hook |

### Unchanged Files

| File | Reason |
|------|--------|
| `tauri-client/src/stores/chatStore.ts` | No voice-related state |
| `tauri-client/src/features/chat/ChatPanel.tsx` | Untouched, hidden when voice view active |
| `tauri-client/src/stores/authStore.ts` | Untouched |

---

## Task 1: C++ Server — Add PING Packet Type and UDP Echo

**Files:**
- Modify: `src/common/udp_packet.hpp`
- Modify: `src/community/main.cpp`

This task adds the `PING = 5` enum variant and the UDP echo handler that reflects ping packets back to the sender.

- [ ] **Step 1: Add PING to UdpPacketType enum**

In `src/common/udp_packet.hpp`, add `PING = 5` after `FEC = 4`:

```cpp
enum UdpPacketType {
    AUDIO = 0,
    VIDEO = 1,
    KEYFRAME_REQUEST = 2,
    NACK = 3,
    FEC = 4,
    PING = 5
};
```

- [ ] **Step 2: Add ping echo handler in `do_receive_udp()`**

In `src/community/main.cpp`, in the `do_receive_udp()` method, add a ping handler **before** the existing token-extraction logic (early return, like KEYFRAME_REQUEST/NACK). Find the section after `bytes_recvd` is set and packet_type is read:

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

- [ ] **Step 3: Build the C++ server to verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell && cmake --build build --target community_server
```

Expected: Compiles without errors.

- [ ] **Step 4: Commit**

```bash
git add src/common/udp_packet.hpp src/community/main.cpp
git commit -m "feat(server): add UDP PING packet type and echo handler for voice latency measurement"
```

---

## Task 2: Rust — Add cpal and audiopus Dependencies

**Files:**
- Modify: `tauri-client/src-tauri/Cargo.toml`

- [ ] **Step 1: Add cpal and audiopus to Cargo.toml**

Add these under `[dependencies]`:

```toml
cpal = "0.15"
audiopus = "0.3"
```

- [ ] **Step 2: Verify the project compiles with new dependencies**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

Expected: Compiles (may take a while to download/build cpal and audiopus). If `audiopus` requires `libopus-dev`, install it first: `sudo pacman -S opus`.

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/Cargo.toml
git commit -m "feat(deps): add cpal and audiopus crates for voice audio pipeline"
```

---

## Task 3: Rust — UDP Packet Serialization (`packet.rs`)

**Files:**
- Create: `tauri-client/src-tauri/src/media/packet.rs`

This module provides `UdpAudioPacket` serialization/deserialization matching the C++ struct layout (1437 bytes total).

- [ ] **Step 1: Create packet.rs with struct and serialization**

```rust
/// UDP audio packet format matching C++ `UdpAudioPacket` in `src/common/udp_packet.hpp`.
///
/// Layout: 1 byte type + 32 bytes sender_id + 2 bytes sequence + 2 bytes payload_size + 1400 bytes payload = 1437 total.

pub const PACKET_TYPE_AUDIO: u8 = 0;
pub const PACKET_TYPE_PING: u8 = 5;
pub const PACKET_TOTAL_SIZE: usize = 1437;
pub const SENDER_ID_SIZE: usize = 32;
pub const MAX_PAYLOAD_SIZE: usize = 1400;

#[derive(Debug)]
pub struct UdpAudioPacket {
    pub packet_type: u8,
    pub sender_id: [u8; SENDER_ID_SIZE],
    pub sequence: u16,
    pub payload_size: u16,
    pub payload: [u8; MAX_PAYLOAD_SIZE],
}

impl UdpAudioPacket {
    /// Create a new audio packet with the given sender ID suffix (from JWT) and Opus payload.
    pub fn new_audio(sender_id_str: &str, sequence: u16, opus_data: &[u8]) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        let data_len = opus_data.len().min(MAX_PAYLOAD_SIZE);
        payload[..data_len].copy_from_slice(&opus_data[..data_len]);

        UdpAudioPacket {
            packet_type: PACKET_TYPE_AUDIO,
            sender_id,
            sequence,
            payload_size: data_len as u16,
            payload,
        }
    }

    /// Create a ping packet with a timestamp in the payload.
    pub fn new_ping(sender_id_str: &str, timestamp_ns: u64) -> Self {
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        let bytes = sender_id_str.as_bytes();
        let len = bytes.len().min(SENDER_ID_SIZE);
        sender_id[..len].copy_from_slice(&bytes[..len]);

        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        payload[..8].copy_from_slice(&timestamp_ns.to_le_bytes());

        UdpAudioPacket {
            packet_type: PACKET_TYPE_PING,
            sender_id,
            sequence: 0,
            payload_size: 8,
            payload,
        }
    }

    /// Serialize to a fixed-size byte buffer for UDP transmission.
    pub fn to_bytes(&self) -> [u8; PACKET_TOTAL_SIZE] {
        let mut buf = [0u8; PACKET_TOTAL_SIZE];
        buf[0] = self.packet_type;
        buf[1..33].copy_from_slice(&self.sender_id);
        buf[33..35].copy_from_slice(&self.sequence.to_le_bytes());
        buf[35..37].copy_from_slice(&self.payload_size.to_le_bytes());
        buf[37..1437].copy_from_slice(&self.payload);
        buf
    }

    /// Deserialize from a received byte buffer.
    pub fn from_bytes(buf: &[u8]) -> Option<Self> {
        if buf.len() < PACKET_TOTAL_SIZE {
            return None;
        }
        let packet_type = buf[0];
        let mut sender_id = [0u8; SENDER_ID_SIZE];
        sender_id.copy_from_slice(&buf[1..33]);
        let sequence = u16::from_le_bytes([buf[33], buf[34]]);
        let payload_size = u16::from_le_bytes([buf[35], buf[36]]);
        let mut payload = [0u8; MAX_PAYLOAD_SIZE];
        payload.copy_from_slice(&buf[37..1437]);

        Some(UdpAudioPacket {
            packet_type,
            sender_id,
            sequence,
            payload_size,
            payload,
        })
    }

    /// Extract the sender username as a UTF-8 string (trimmed of null bytes).
    pub fn sender_username(&self) -> String {
        let end = self.sender_id.iter().position(|&b| b == 0).unwrap_or(SENDER_ID_SIZE);
        String::from_utf8_lossy(&self.sender_id[..end]).to_string()
    }

    /// Get a slice of just the valid payload data.
    pub fn payload_data(&self) -> &[u8] {
        &self.payload[..self.payload_size as usize]
    }
}
```

- [ ] **Step 2: Update `media/mod.rs` to declare the module**

Replace the placeholder content in `tauri-client/src-tauri/src/media/mod.rs` with:

```rust
pub mod packet;
```

- [ ] **Step 3: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

Expected: Compiles without errors.

- [ ] **Step 4: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/media/packet.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(media): add UDP audio packet serialization matching C++ UdpAudioPacket layout"
```

---

## Task 4: Rust — Opus Codec Wrappers (`codec.rs`)

**Files:**
- Create: `tauri-client/src-tauri/src/media/codec.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

Thin wrappers around `audiopus` for 48kHz mono 20ms Opus encoding/decoding.

- [ ] **Step 1: Create codec.rs**

```rust
use audiopus::coder::{Encoder, Decoder};
use audiopus::{Application, Channels, SampleRate};

pub const SAMPLE_RATE: u32 = 48000;
pub const CHANNELS: u16 = 1;
pub const FRAME_SIZE: usize = 960; // 20ms at 48kHz mono
pub const MAX_OPUS_FRAME_SIZE: usize = 1400;

pub struct OpusEncoder {
    encoder: Encoder,
}

impl OpusEncoder {
    pub fn new() -> Result<Self, String> {
        let encoder = Encoder::new(
            SampleRate::Hz48000,
            Channels::Mono,
            Application::Voip,
        )
        .map_err(|e| format!("Failed to create Opus encoder: {}", e))?;

        Ok(OpusEncoder { encoder })
    }

    /// Encode 960 i16 PCM samples (20ms at 48kHz mono) into Opus.
    /// Returns the number of bytes written to `output`.
    pub fn encode(&mut self, pcm: &[i16], output: &mut [u8; MAX_OPUS_FRAME_SIZE]) -> Result<usize, String> {
        self.encoder
            .encode(pcm, output)
            .map_err(|e| format!("Opus encode error: {}", e))
    }

    /// Encode a silence frame (used when muted to keep UDP alive).
    pub fn encode_silence(&mut self, output: &mut [u8; MAX_OPUS_FRAME_SIZE]) -> Result<usize, String> {
        let silence = [0i16; FRAME_SIZE];
        self.encode(&silence, output)
    }
}

pub struct OpusDecoder {
    decoder: Decoder,
}

impl OpusDecoder {
    pub fn new() -> Result<Self, String> {
        let decoder = Decoder::new(SampleRate::Hz48000, Channels::Mono)
            .map_err(|e| format!("Failed to create Opus decoder: {}", e))?;
        Ok(OpusDecoder { decoder })
    }

    /// Decode an Opus frame into 960 i16 PCM samples.
    /// Returns the number of samples decoded.
    pub fn decode(&mut self, opus_data: &[u8], output: &mut [i16; FRAME_SIZE]) -> Result<usize, String> {
        self.decoder
            .decode(Some(opus_data), output, false)
            .map_err(|e| format!("Opus decode error: {}", e))
    }
}
```

- [ ] **Step 2: Add module declaration to `media/mod.rs`**

```rust
pub mod codec;
pub mod packet;
```

- [ ] **Step 3: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

Expected: Compiles. If `libopus` is missing, run `sudo pacman -S opus`.

- [ ] **Step 4: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/media/codec.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(media): add Opus encoder/decoder wrappers for 48kHz mono voice"
```

---

## Task 5: Rust — Speaking Detection (`speaking.rs`)

**Files:**
- Create: `tauri-client/src-tauri/src/media/speaking.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

Noise-gated RMS amplitude with hysteresis: 3 consecutive "speaking" frames to trigger, 5 consecutive "silent" frames to clear.

- [ ] **Step 1: Create speaking.rs**

```rust
use super::codec::FRAME_SIZE;

const NOISE_GATE_MULTIPLIER: f32 = 3.0;
const NOISE_FLOOR_ALPHA: f32 = 0.01;
const SPEAKING_TRIGGER_FRAMES: u32 = 3;
const SILENCE_CLEAR_FRAMES: u32 = 5;

pub struct SpeakingDetector {
    noise_floor: f32,
    speaking: bool,
    consecutive_speaking: u32,
    consecutive_silent: u32,
}

impl SpeakingDetector {
    pub fn new() -> Self {
        SpeakingDetector {
            noise_floor: 100.0, // initial noise floor estimate
            speaking: false,
            consecutive_speaking: 0,
            consecutive_silent: 0,
        }
    }

    /// Process a 20ms PCM frame (960 samples). Returns `Some(true/false)` only on state transitions.
    pub fn process(&mut self, pcm: &[i16; FRAME_SIZE]) -> Option<bool> {
        let rms = compute_rms(pcm);

        // Update noise floor with slow EMA
        self.noise_floor = self.noise_floor * (1.0 - NOISE_FLOOR_ALPHA) + rms * NOISE_FLOOR_ALPHA;

        let threshold = self.noise_floor * NOISE_GATE_MULTIPLIER;
        let is_loud = rms > threshold;

        if is_loud {
            self.consecutive_speaking += 1;
            self.consecutive_silent = 0;
        } else {
            self.consecutive_silent += 1;
            self.consecutive_speaking = 0;
        }

        if !self.speaking && self.consecutive_speaking >= SPEAKING_TRIGGER_FRAMES {
            self.speaking = true;
            return Some(true);
        }

        if self.speaking && self.consecutive_silent >= SILENCE_CLEAR_FRAMES {
            self.speaking = false;
            return Some(false);
        }

        None
    }

    pub fn is_speaking(&self) -> bool {
        self.speaking
    }

    /// Reset state (e.g., when user disconnects).
    pub fn reset(&mut self) {
        self.noise_floor = 100.0;
        self.speaking = false;
        self.consecutive_speaking = 0;
        self.consecutive_silent = 0;
    }
}

fn compute_rms(samples: &[i16]) -> f32 {
    if samples.is_empty() {
        return 0.0;
    }
    let sum_sq: f64 = samples.iter().map(|&s| (s as f64) * (s as f64)).sum();
    (sum_sq / samples.len() as f64).sqrt() as f32
}
```

- [ ] **Step 2: Add module declaration to `media/mod.rs`**

```rust
pub mod codec;
pub mod packet;
pub mod speaking;
```

- [ ] **Step 3: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

- [ ] **Step 4: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/media/speaking.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(media): add noise-gated speaking detection with hysteresis"
```

---

## Task 6: Rust — Audio Pipeline (`pipeline.rs`)

**Files:**
- Create: `tauri-client/src-tauri/src/media/pipeline.rs`
- Modify: `tauri-client/src-tauri/src/media/mod.rs`

The audio pipeline runs on a dedicated OS thread. It manages CPAL capture/playback, Opus encode/decode, UDP send/receive, and speaking detection.

- [ ] **Step 1: Create pipeline.rs with control/event message types and the main loop**

```rust
use std::collections::{HashMap, VecDeque};
use std::net::UdpSocket;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

use super::codec::{OpusEncoder, OpusDecoder, FRAME_SIZE, MAX_OPUS_FRAME_SIZE, SAMPLE_RATE, CHANNELS};
use super::packet::{UdpAudioPacket, PACKET_TYPE_AUDIO, PACKET_TYPE_PING, PACKET_TOTAL_SIZE};
use super::speaking::SpeakingDetector;

/// Messages from the main app to the audio thread.
pub enum ControlMessage {
    SetMute(bool),
    SetDeafen(bool),
    Shutdown,
}

/// Events from the audio thread to the main app.
pub enum VoiceEvent {
    SpeakingChanged(String, bool), // (username, is_speaking)
    PingMeasured(u32),             // latency in ms
    Error(String),
}

const PING_INTERVAL: Duration = Duration::from_secs(3);
const DECODER_TIMEOUT: Duration = Duration::from_secs(5);
const UDP_READ_TIMEOUT: Duration = Duration::from_millis(5);

struct RemoteUser {
    decoder: OpusDecoder,
    speaking: SpeakingDetector,
    last_seen: Instant,
    last_sequence: u16,
}

/// Run the audio pipeline on a dedicated OS thread.
/// This function blocks until a Shutdown message is received.
pub fn run_audio_pipeline(
    server_addr: String,
    sender_id: String,
    control_rx: std::sync::mpsc::Receiver<ControlMessage>,
    event_tx: std::sync::mpsc::Sender<VoiceEvent>,
) {
    // Bind UDP socket
    let socket = match UdpSocket::bind("0.0.0.0:0") {
        Ok(s) => s,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to bind UDP socket: {}", e)));
            return;
        }
    };
    socket.set_read_timeout(Some(UDP_READ_TIMEOUT)).ok();

    // CPAL setup
    let host = cpal::default_host();

    // Input (microphone) — may not exist
    let input_device = host.default_input_device();
    let has_mic = input_device.is_some();
    if !has_mic {
        let _ = event_tx.send(VoiceEvent::Error("No microphone detected".to_string()));
    }

    let output_device = match host.default_output_device() {
        Some(d) => d,
        None => {
            let _ = event_tx.send(VoiceEvent::Error("No audio output device found".to_string()));
            return;
        }
    };

    // Shared ring buffers for CPAL callbacks
    let capture_buf: Arc<Mutex<Vec<i16>>> = Arc::new(Mutex::new(Vec::with_capacity(FRAME_SIZE * 8)));
    let playback_buf: Arc<Mutex<VecDeque<i16>>> = Arc::new(Mutex::new(VecDeque::with_capacity(FRAME_SIZE * 8)));

    let capture_buf_writer = capture_buf.clone();
    let playback_buf_reader = playback_buf.clone();

    // Build CPAL streams
    let stream_config = cpal::StreamConfig {
        channels: CHANNELS,
        sample_rate: cpal::SampleRate(SAMPLE_RATE),
        buffer_size: cpal::BufferSize::Default,
    };

    let event_tx_err = event_tx.clone();
    let input_stream = if let Some(ref input_dev) = input_device {
        match input_dev.build_input_stream(
            &stream_config,
            move |data: &[i16], _: &cpal::InputCallbackInfo| {
                if let Ok(mut buf) = capture_buf_writer.lock() {
                    buf.extend_from_slice(data);
                }
            },
            move |err| {
                let _ = event_tx_err.send(VoiceEvent::Error(format!("Input stream error: {}", err)));
            },
            None,
        ) {
            Ok(s) => {
                s.play().ok();
                Some(s)
            }
            Err(e) => {
                let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build input stream: {}", e)));
                None
            }
        }
    } else {
        None
    };

    let event_tx_clone = event_tx.clone();
    let output_stream = match output_device.build_output_stream(
        &stream_config,
        move |data: &mut [i16], _: &cpal::OutputCallbackInfo| {
            if let Ok(mut buf) = playback_buf_reader.lock() {
                for sample in data.iter_mut() {
                    *sample = buf.pop_front().unwrap_or(0);
                }
            } else {
                for sample in data.iter_mut() {
                    *sample = 0;
                }
            }
        },
        move |err| {
            let _ = event_tx_clone.send(VoiceEvent::Error(format!("Output stream error: {}", err)));
        },
        None,
    ) {
        Ok(s) => {
            s.play().ok();
            s
        }
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(format!("Failed to build output stream: {}", e)));
            return;
        }
    };

    // Opus encoder
    let mut encoder = match OpusEncoder::new() {
        Ok(e) => e,
        Err(e) => {
            let _ = event_tx.send(VoiceEvent::Error(e));
            return;
        }
    };

    let mut is_muted = false;
    let mut is_deafened = false;
    let mut was_muted_before_deafen = false;
    let mut sequence: u16 = 0;
    let mut local_speaking = SpeakingDetector::new();
    let mut remote_users: HashMap<String, RemoteUser> = HashMap::new();
    let mut last_ping_time = Instant::now();
    let mut pending_ping_timestamp: Option<u64> = None;
    let mut recv_buf = [0u8; PACKET_TOTAL_SIZE];

    // Main loop
    loop {
        let cycle_start = Instant::now();

        // Check control messages (non-blocking)
        while let Ok(msg) = control_rx.try_recv() {
            match msg {
                ControlMessage::SetMute(muted) => {
                    is_muted = muted;
                    if is_deafened && !muted {
                        // Unmuting while deafened → undeafen too
                        is_deafened = false;
                    }
                }
                ControlMessage::SetDeafen(deafened) => {
                    if deafened {
                        was_muted_before_deafen = is_muted;
                        is_deafened = true;
                        is_muted = true;
                    } else {
                        is_deafened = false;
                        is_muted = was_muted_before_deafen;
                    }
                }
                ControlMessage::Shutdown => {
                    // Clean up
                    drop(input_stream);
                    drop(output_stream);
                    return;
                }
            }
        }

        // === CAPTURE & SEND ===
        let mut frame = [0i16; FRAME_SIZE];
        let have_frame = {
            if let Ok(mut buf) = capture_buf.lock() {
                if buf.len() >= FRAME_SIZE {
                    frame.copy_from_slice(&buf[..FRAME_SIZE]);
                    buf.drain(..FRAME_SIZE);
                    true
                } else {
                    false
                }
            } else {
                false
            }
        };

        if have_frame {
            // Speaking detection on raw mic audio (even when muted, for local indicator)
            if !is_muted {
                if let Some(speaking) = local_speaking.process(&frame) {
                    let _ = event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), speaking));
                }
            } else if local_speaking.is_speaking() {
                local_speaking.reset();
                let _ = event_tx.send(VoiceEvent::SpeakingChanged("__local__".to_string(), false));
            }

            let mut opus_out = [0u8; MAX_OPUS_FRAME_SIZE];
            let encoded_len = if is_muted {
                encoder.encode_silence(&mut opus_out).unwrap_or(0)
            } else {
                encoder.encode(&frame, &mut opus_out).unwrap_or(0)
            };

            if encoded_len > 0 {
                let packet = UdpAudioPacket::new_audio(&sender_id, sequence, &opus_out[..encoded_len]);
                let bytes = packet.to_bytes();
                let _ = socket.send_to(&bytes, &server_addr);
                sequence = sequence.wrapping_add(1);
            }
        }

        // === PING ===
        if last_ping_time.elapsed() >= PING_INTERVAL {
            let now_ns = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos() as u64;
            let ping_packet = UdpAudioPacket::new_ping(&sender_id, now_ns);
            let _ = socket.send_to(&ping_packet.to_bytes(), &server_addr);
            pending_ping_timestamp = Some(now_ns);
            last_ping_time = Instant::now();
        }

        // === RECEIVE & DECODE ===
        loop {
            match socket.recv_from(&mut recv_buf) {
                Ok((n, _)) => {
                    if n < PACKET_TOTAL_SIZE {
                        continue;
                    }
                    if let Some(pkt) = UdpAudioPacket::from_bytes(&recv_buf) {
                        if pkt.packet_type == PACKET_TYPE_PING {
                            // Ping response — measure RTT
                            if pkt.payload_size >= 8 {
                                let sent_ns = u64::from_le_bytes([
                                    pkt.payload[0], pkt.payload[1], pkt.payload[2], pkt.payload[3],
                                    pkt.payload[4], pkt.payload[5], pkt.payload[6], pkt.payload[7],
                                ]);
                                if let Some(pending) = pending_ping_timestamp {
                                    if sent_ns == pending {
                                        let now_ns = std::time::SystemTime::now()
                                            .duration_since(std::time::UNIX_EPOCH)
                                            .unwrap_or_default()
                                            .as_nanos() as u64;
                                        let rtt_ms = ((now_ns - sent_ns) / 1_000_000) as u32;
                                        let _ = event_tx.send(VoiceEvent::PingMeasured(rtt_ms));
                                        pending_ping_timestamp = None;
                                    }
                                }
                            }
                            continue;
                        }

                        if pkt.packet_type != PACKET_TYPE_AUDIO {
                            continue;
                        }

                        let username = pkt.sender_username();
                        if username.is_empty() {
                            continue;
                        }

                        // Handle sequence wrap-around
                        let remote = remote_users.entry(username.clone()).or_insert_with(|| {
                            RemoteUser {
                                decoder: OpusDecoder::new().unwrap(),
                                speaking: SpeakingDetector::new(),
                                last_seen: Instant::now(),
                                last_sequence: pkt.sequence.wrapping_sub(1),
                            }
                        });

                        // Check for out-of-order (with wrap-around handling)
                        let seq_diff = pkt.sequence.wrapping_sub(remote.last_sequence);
                        if seq_diff == 0 || seq_diff > 32768 {
                            continue; // duplicate or out-of-order
                        }
                        remote.last_sequence = pkt.sequence;
                        remote.last_seen = Instant::now();

                        // Decode
                        let mut pcm = [0i16; FRAME_SIZE];
                        if remote.decoder.decode(pkt.payload_data(), &mut pcm).is_ok() {
                            // Speaking detection
                            if let Some(speaking) = remote.speaking.process(&pcm) {
                                let _ = event_tx.send(VoiceEvent::SpeakingChanged(username, speaking));
                            }

                            // Mix into playback buffer (unless deafened)
                            if !is_deafened {
                                if let Ok(mut buf) = playback_buf.lock() {
                                    // Extend or mix into existing samples
                                    let current_len = buf.len();
                                    if current_len < FRAME_SIZE {
                                        buf.resize(FRAME_SIZE, 0);
                                    }
                                    for (i, &sample) in pcm.iter().enumerate() {
                                        // Mix (saturating add) — VecDeque supports indexing
                                        buf[i] = buf[i].saturating_add(sample);
                                    }
                                }
                            }
                        }
                    }
                }
                Err(ref e) if e.kind() == std::io::ErrorKind::WouldBlock || e.kind() == std::io::ErrorKind::TimedOut => {
                    break; // No more packets available
                }
                Err(_) => {
                    break;
                }
            }
        }

        // === CLEANUP stale remote decoders ===
        remote_users.retain(|username, user| {
            if user.last_seen.elapsed() > DECODER_TIMEOUT {
                if user.speaking.is_speaking() {
                    let _ = event_tx.send(VoiceEvent::SpeakingChanged(username.clone(), false));
                }
                false
            } else {
                true
            }
        });

        // Sleep to target ~20ms cycle (CPAL drives actual timing, this prevents busy-spinning)
        let elapsed = cycle_start.elapsed();
        if elapsed < Duration::from_millis(5) {
            std::thread::sleep(Duration::from_millis(5) - elapsed);
        }
    }
}
```

- [ ] **Step 2: Add module declaration to `media/mod.rs`**

```rust
pub mod codec;
pub mod packet;
pub mod pipeline;
pub mod speaking;
```

- [ ] **Step 3: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

- [ ] **Step 4: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/media/pipeline.rs tauri-client/src-tauri/src/media/mod.rs
git commit -m "feat(media): add audio pipeline with CPAL capture/playback, Opus codec, and UDP transport"
```

---

## Task 7: Rust — VoiceEngine (`media/mod.rs`)

**Files:**
- Modify: `tauri-client/src-tauri/src/media/mod.rs`
- Modify: `tauri-client/src-tauri/src/state.rs`

VoiceEngine is the top-level struct stored in AppState. It spawns the audio thread and bridges events to the Tauri frontend.

- [ ] **Step 1: Implement VoiceEngine in `media/mod.rs`**

Replace the contents of `tauri-client/src-tauri/src/media/mod.rs` with:

```rust
pub mod codec;
pub mod packet;
pub mod pipeline;
pub mod speaking;

use std::sync::mpsc;
use std::thread::{self, JoinHandle};

use tauri::{AppHandle, Emitter};

use pipeline::{ControlMessage, VoiceEvent};

pub struct VoiceEngine {
    audio_thread: Option<JoinHandle<()>>,
    event_bridge: Option<tokio::task::JoinHandle<()>>,
    control_tx: mpsc::Sender<ControlMessage>,
    is_muted: bool,
    is_deafened: bool,
}

impl VoiceEngine {
    /// Start the voice engine. Spawns the audio thread and event bridge.
    pub fn start(
        server_host: &str,
        server_port: u16,
        jwt: &str,
        app: AppHandle,
    ) -> Result<Self, String> {
        let (control_tx, control_rx) = mpsc::channel();
        let (event_tx, event_rx) = mpsc::channel();

        // The UDP audio port is server_port + 1
        let udp_addr = format!("{}:{}", server_host, server_port + 1);

        // Sender ID: last 31 chars of JWT (server uses this for auth before rewriting with username)
        let jwt_bytes = jwt.as_bytes();
        let sender_id = if jwt_bytes.len() > 31 {
            String::from_utf8_lossy(&jwt_bytes[jwt_bytes.len() - 31..]).to_string()
        } else {
            jwt.to_string()
        };

        let audio_thread = thread::Builder::new()
            .name("decibell-audio".to_string())
            .spawn(move || {
                pipeline::run_audio_pipeline(udp_addr, sender_id, control_rx, event_tx);
            })
            .map_err(|e| format!("Failed to spawn audio thread: {}", e))?;

        // Voice event bridge: poll event_rx and emit Tauri events
        let event_bridge = tokio::spawn(async move {
            // Move event_rx into this async task
            loop {
                // Use tokio::task::spawn_blocking to poll the std mpsc in a non-blocking way
                let rx_result = tokio::task::block_in_place(|| {
                    event_rx.recv_timeout(std::time::Duration::from_millis(50))
                });

                match rx_result {
                    Ok(event) => {
                        match event {
                            VoiceEvent::SpeakingChanged(username, speaking) => {
                                let _ = app.emit("voice_user_speaking", serde_json::json!({
                                    "username": username,
                                    "speaking": speaking,
                                }));
                            }
                            VoiceEvent::PingMeasured(ms) => {
                                let _ = app.emit("voice_ping_updated", serde_json::json!({
                                    "latencyMs": ms,
                                }));
                            }
                            VoiceEvent::Error(msg) => {
                                let _ = app.emit("voice_error", serde_json::json!({
                                    "message": msg,
                                }));
                            }
                        }
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {
                        // Continue polling
                    }
                    Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                        // Audio thread ended
                        break;
                    }
                }
            }
        });

        Ok(VoiceEngine {
            audio_thread: Some(audio_thread),
            event_bridge: Some(event_bridge),
            control_tx,
            is_muted: false,
            is_deafened: false,
        })
    }

    /// Stop the voice engine. Sends shutdown, joins the audio thread.
    pub fn stop(&mut self) {
        let _ = self.control_tx.send(ControlMessage::Shutdown);
        if let Some(handle) = self.audio_thread.take() {
            let _ = handle.join();
        }
        if let Some(handle) = self.event_bridge.take() {
            handle.abort();
        }
    }

    pub fn set_mute(&mut self, muted: bool) {
        self.is_muted = muted;
        let _ = self.control_tx.send(ControlMessage::SetMute(muted));
    }

    pub fn set_deafen(&mut self, deafened: bool) {
        self.is_deafened = deafened;
        let _ = self.control_tx.send(ControlMessage::SetDeafen(deafened));
        if deafened {
            self.is_muted = true;
        }
    }

    pub fn is_muted(&self) -> bool {
        self.is_muted
    }

    pub fn is_deafened(&self) -> bool {
        self.is_deafened
    }
}

impl Drop for VoiceEngine {
    fn drop(&mut self) {
        self.stop();
    }
}
```

- [ ] **Step 2: Add VoiceEngine to AppState**

In `tauri-client/src-tauri/src/state.rs`:

```rust
use std::collections::HashMap;
use std::sync::Arc;
use tokio::sync::Mutex;

use crate::media::VoiceEngine;
use crate::net::central::CentralClient;
use crate::net::community::CommunityClient;

#[derive(Default)]
pub struct AppState {
    pub central: Option<CentralClient>,
    pub communities: HashMap<String, CommunityClient>,
    pub username: Option<String>,
    pub token: Option<String>,
    pub credentials: Option<(String, String)>,
    pub voice_engine: Option<VoiceEngine>,
}

pub type SharedState = Arc<Mutex<AppState>>;
```

Note: `#[derive(Default)]` requires `VoiceEngine` to not be in the default. Since `voice_engine` is `Option<VoiceEngine>` and `Option` defaults to `None`, this works without implementing `Default` for `VoiceEngine`.

- [ ] **Step 3: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

- [ ] **Step 4: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/media/mod.rs tauri-client/src-tauri/src/state.rs
git commit -m "feat(media): add VoiceEngine with audio thread lifecycle and Tauri event bridge"
```

---

## Task 8: Rust — Voice Events in events/mod.rs

**Files:**
- Modify: `tauri-client/src-tauri/src/events/mod.rs`

Add voice event constants, payload structs, and emit helpers.

- [ ] **Step 1: Add voice event constants after existing constants**

Add at the end of the event constants section (after `FRIEND_ACTION_RESPONDED`):

```rust
pub const VOICE_PRESENCE_UPDATED: &str = "voice_presence_updated";
pub const VOICE_STATE_CHANGED: &str = "voice_state_changed";
```

- [ ] **Step 2: Add voice payload structs after existing payloads**

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoicePresenceUpdatedPayload {
    pub server_id: String,
    pub channel_id: String,
    pub participants: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VoiceStateChangedPayload {
    pub is_muted: bool,
    pub is_deafened: bool,
}
```

- [ ] **Step 3: Add voice emit helpers after existing helpers**

```rust
pub fn emit_voice_presence_updated(
    app: &AppHandle,
    server_id: String,
    channel_id: String,
    participants: Vec<String>,
) {
    let _ = app.emit(
        VOICE_PRESENCE_UPDATED,
        VoicePresenceUpdatedPayload {
            server_id,
            channel_id,
            participants,
        },
    );
}

pub fn emit_voice_state_changed(app: &AppHandle, is_muted: bool, is_deafened: bool) {
    let _ = app.emit(
        VOICE_STATE_CHANGED,
        VoiceStateChangedPayload {
            is_muted,
            is_deafened,
        },
    );
}
```

- [ ] **Step 4: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

- [ ] **Step 5: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/events/mod.rs
git commit -m "feat(events): add voice presence and state change event types"
```

---

## Task 9: Rust — Community Client Voice Extensions

**Files:**
- Modify: `tauri-client/src-tauri/src/net/community.rs`

Add `join_voice_channel()` and `leave_voice_channel()` methods, and handle `VoicePresenceUpdate` in `route_packets`.

- [ ] **Step 1: Add `join_voice_channel` method to CommunityClient**

Add after the `send_channel_message` method:

```rust
    /// Join a voice channel.
    pub async fn join_voice_channel(&self, channel_id: &str) -> Result<(), String> {
        let data = build_packet(
            packet::Type::JoinVoiceReq,
            packet::Payload::JoinVoiceReq(JoinVoiceRequest {
                channel_id: channel_id.into(),
            }),
            Some(&self.jwt),
        );
        self.send(data).await
    }

    /// Leave the current voice channel.
    pub async fn leave_voice_channel(&self) -> Result<(), String> {
        let data = build_packet(
            packet::Type::LeaveVoiceReq,
            packet::Payload::LeaveVoiceReq(LeaveVoiceRequest {}),
            Some(&self.jwt),
        );
        self.send(data).await
    }
```

- [ ] **Step 2: Add VoicePresenceUpdate handler in route_packets**

In the `route_packets` match block, add before the `_ =>` default arm:

```rust
                Some(packet::Payload::VoicePresenceUpdate(update)) => {
                    events::emit_voice_presence_updated(
                        &app,
                        server_id.clone(),
                        update.channel_id,
                        update.active_users,
                    );
                }
```

- [ ] **Step 3: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

- [ ] **Step 4: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/net/community.rs
git commit -m "feat(net): add voice channel join/leave and presence update routing"
```

---

## Task 10: Rust — Voice Tauri Commands

**Files:**
- Create: `tauri-client/src-tauri/src/commands/voice.rs`
- Modify: `tauri-client/src-tauri/src/commands/mod.rs`
- Modify: `tauri-client/src-tauri/src/lib.rs`

Four new Tauri commands: `join_voice_channel`, `leave_voice_channel`, `set_voice_mute`, `set_voice_deafen`.

- [ ] **Step 1: Fix CommunityClient field visibility**

The voice commands need access to `host`, `port`, and `jwt` fields of `CommunityClient`. These are currently private. In `tauri-client/src-tauri/src/net/community.rs`, change the struct fields:

```rust
pub struct CommunityClient {
    connection: Option<Connection>,
    router_task: Option<JoinHandle<()>>,
    reconnect_task: Option<JoinHandle<()>>,
    pub server_id: String,
    pub host: String,
    pub port: u16,
    pub jwt: String,
    pub joined_channels: Vec<String>,
}
```

(Add `pub` to `host`, `port`, and `jwt`.)

- [ ] **Step 2: Create voice.rs with all four commands**

```rust
use tauri::{AppHandle, State};

use crate::events;
use crate::media::VoiceEngine;
use crate::state::SharedState;

#[tauri::command]
pub async fn join_voice_channel(
    server_id: String,
    channel_id: String,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // If already in voice, stop the existing engine
    if s.voice_engine.is_some() {
        // Leave current voice channel on the server side
        // (may be a different server, so try all communities)
        for client in s.communities.values() {
            let _ = client.leave_voice_channel().await;
        }
        if let Some(mut engine) = s.voice_engine.take() {
            engine.stop();
        }
    }

    // Send JOIN_VOICE_REQ over TCP
    let (host, port, jwt) = {
        let client = s.communities.get(&server_id)
            .ok_or(format!("Not connected to community {}", server_id))?;
        client.join_voice_channel(&channel_id).await?;
        (client.host.clone(), client.port, client.jwt.clone())
    };

    // Start VoiceEngine
    let engine = VoiceEngine::start(&host, port, &jwt, app.clone())?;
    s.voice_engine = Some(engine);

    // Emit initial state
    events::emit_voice_state_changed(&app, false, false);

    Ok(())
}

#[tauri::command]
pub async fn leave_voice_channel(
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;

    // Send LEAVE_VOICE_REQ to all connected communities
    for client in s.communities.values() {
        let _ = client.leave_voice_channel().await;
    }

    if let Some(mut engine) = s.voice_engine.take() {
        engine.stop();
    }

    events::emit_voice_state_changed(&app, false, false);

    Ok(())
}

#[tauri::command]
pub async fn set_voice_mute(
    muted: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    if let Some(ref mut engine) = s.voice_engine {
        engine.set_mute(muted);
        events::emit_voice_state_changed(&app, engine.is_muted(), engine.is_deafened());
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}

#[tauri::command]
pub async fn set_voice_deafen(
    deafened: bool,
    app: AppHandle,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let mut s = state.lock().await;
    if let Some(ref mut engine) = s.voice_engine {
        engine.set_deafen(deafened);
        events::emit_voice_state_changed(&app, engine.is_muted(), engine.is_deafened());
        Ok(())
    } else {
        Err("Not in a voice channel".to_string())
    }
}
```

- [ ] **Step 3: Add `pub mod voice;` to commands/mod.rs**

```rust
pub mod auth;
pub mod channels;
pub mod friends;
pub mod messaging;
pub mod servers;
pub mod voice;

#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}
```

- [ ] **Step 4: Register voice commands in lib.rs**

Add the four voice commands to the `invoke_handler` in `lib.rs`:

```rust
        .invoke_handler(tauri::generate_handler![
            commands::ping,
            commands::auth::login,
            commands::auth::register,
            commands::auth::logout,
            commands::servers::request_server_list,
            commands::servers::connect_to_community,
            commands::servers::disconnect_from_community,
            commands::channels::join_channel,
            commands::channels::send_channel_message,
            commands::friends::request_friend_list,
            commands::friends::send_friend_action,
            commands::messaging::send_private_message,
            commands::voice::join_voice_channel,
            commands::voice::leave_voice_channel,
            commands::voice::set_voice_mute,
            commands::voice::set_voice_deafen,
        ])
```

- [ ] **Step 5: Verify compilation**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo check
```

- [ ] **Step 6: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src-tauri/src/commands/voice.rs tauri-client/src-tauri/src/commands/mod.rs tauri-client/src-tauri/src/lib.rs tauri-client/src-tauri/src/net/community.rs
git commit -m "feat(commands): add voice channel join/leave/mute/deafen Tauri commands"
```

**Note:** The `connect_to_community` and `disconnect_from_community` commands in `commands/servers.rs` should also stop the voice engine when switching/disconnecting servers. Add this cleanup at the top of both commands:

```rust
// Stop voice engine if active
if let Some(mut engine) = s.voice_engine.take() {
    engine.stop();
}
```

Add this to `disconnect_from_community` after acquiring the lock, and to `connect_to_community` if it replaces an existing community connection.

---

## Task 11: Frontend — Expand voiceStore

**Files:**
- Modify: `tauri-client/src/stores/voiceStore.ts`

Expand the skeleton store with all Phase 4 state: speakingUsers, isDeafened, latencyMs, error, connectedServerId, and actions.

- [ ] **Step 1: Write the test for the expanded voiceStore**

Create `tauri-client/src/stores/__tests__/voiceStore.test.ts`:

```typescript
import { describe, it, expect, beforeEach } from "vitest";
import { useVoiceStore } from "../voiceStore";

describe("voiceStore", () => {
  beforeEach(() => {
    useVoiceStore.setState({
      connectedServerId: null,
      connectedChannelId: null,
      participants: [],
      activeStreams: [],
      isMuted: false,
      isDeafened: false,
      speakingUsers: [],
      latencyMs: null,
      error: null,
    });
  });

  it("sets connected channel with server ID", () => {
    useVoiceStore.getState().setConnectedChannel("srv1", "vc1");
    const s = useVoiceStore.getState();
    expect(s.connectedServerId).toBe("srv1");
    expect(s.connectedChannelId).toBe("vc1");
  });

  it("sets participants", () => {
    useVoiceStore.getState().setParticipants([
      { username: "alice", isMuted: false, isSpeaking: false, audioLevel: 0 },
    ]);
    expect(useVoiceStore.getState().participants).toHaveLength(1);
  });

  it("sets speaking user", () => {
    useVoiceStore.getState().setSpeaking("alice", true);
    expect(useVoiceStore.getState().speakingUsers).toContain("alice");

    useVoiceStore.getState().setSpeaking("alice", false);
    expect(useVoiceStore.getState().speakingUsers).not.toContain("alice");
  });

  it("deafen implies mute", () => {
    useVoiceStore.getState().setDeafened(true);
    const s = useVoiceStore.getState();
    expect(s.isDeafened).toBe(true);
    expect(s.isMuted).toBe(true);
  });

  it("disconnect clears all state", () => {
    useVoiceStore.getState().setConnectedChannel("srv1", "vc1");
    useVoiceStore.getState().setSpeaking("alice", true);
    useVoiceStore.getState().setLatency(48);
    useVoiceStore.getState().disconnect();
    const s = useVoiceStore.getState();
    expect(s.connectedServerId).toBeNull();
    expect(s.connectedChannelId).toBeNull();
    expect(s.speakingUsers).toHaveLength(0);
    expect(s.latencyMs).toBeNull();
  });
});
```

- [ ] **Step 2: Run the test to verify it fails**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx vitest run src/stores/__tests__/voiceStore.test.ts
```

Expected: FAIL — store doesn't have the new fields/methods.

- [ ] **Step 3: Rewrite voiceStore.ts with expanded state**

```typescript
import { create } from "zustand";
import type { VoiceParticipant, StreamInfo } from "../types";

interface VoiceState {
  connectedServerId: string | null;
  connectedChannelId: string | null;
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  isMuted: boolean;
  isDeafened: boolean;
  speakingUsers: string[];
  latencyMs: number | null;
  error: string | null;
  setConnectedChannel: (serverId: string | null, channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  setActiveStreams: (streams: StreamInfo[]) => void;
  setMuted: (muted: boolean) => void;
  setDeafened: (deafened: boolean) => void;
  setSpeaking: (username: string, speaking: boolean) => void;
  setLatency: (ms: number) => void;
  setError: (error: string | null) => void;
  disconnect: () => void;
}

export const useVoiceStore = create<VoiceState>((set) => ({
  connectedServerId: null,
  connectedChannelId: null,
  participants: [],
  activeStreams: [],
  isMuted: false,
  isDeafened: false,
  speakingUsers: [],
  latencyMs: null,
  error: null,
  setConnectedChannel: (serverId, channelId) =>
    set({ connectedServerId: serverId, connectedChannelId: channelId }),
  setParticipants: (participants) => set({ participants }),
  setActiveStreams: (streams) => set({ activeStreams: streams }),
  setMuted: (muted) => set({ isMuted: muted }),
  setDeafened: (deafened) =>
    set(deafened ? { isDeafened: true, isMuted: true } : { isDeafened: false }),
  setSpeaking: (username, speaking) =>
    set((state) => ({
      speakingUsers: speaking
        ? state.speakingUsers.includes(username)
          ? state.speakingUsers
          : [...state.speakingUsers, username]
        : state.speakingUsers.filter((u) => u !== username),
    })),
  setLatency: (ms) => set({ latencyMs: ms }),
  setError: (error) => set({ error }),
  disconnect: () =>
    set({
      connectedServerId: null,
      connectedChannelId: null,
      participants: [],
      isMuted: false,
      isDeafened: false,
      speakingUsers: [],
      latencyMs: null,
      error: null,
    }),
}));
```

- [ ] **Step 4: Run the tests to verify they pass**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx vitest run src/stores/__tests__/voiceStore.test.ts
```

Expected: All 5 tests PASS.

- [ ] **Step 5: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/stores/voiceStore.ts tauri-client/src/stores/__tests__/voiceStore.test.ts
git commit -m "feat(store): expand voiceStore with speaking, deafen, latency, and disconnect state"
```

---

## Task 12: Frontend — Add "voice" to uiStore activeView

**Files:**
- Modify: `tauri-client/src/stores/uiStore.ts`

- [ ] **Step 1: Update the activeView type**

Change the `activeView` type and `setActiveView` parameter from:
```typescript
activeView: "home" | "server" | "browse";
```
to:
```typescript
activeView: "home" | "server" | "browse" | "voice";
```

Update both the interface and the `setActiveView` parameter type.

- [ ] **Step 2: Verify existing uiStore tests still pass**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx vitest run src/stores/__tests__/uiStore.test.ts
```

Expected: PASS.

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/stores/uiStore.ts
git commit -m "feat(store): add 'voice' to activeView union type"
```

---

## Task 13: Frontend — useVoiceEvents Hook

**Files:**
- Create: `tauri-client/src/features/voice/useVoiceEvents.ts`

Listens for all voice Tauri events and dispatches to voiceStore.

- [ ] **Step 1: Create useVoiceEvents.ts**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useVoiceStore } from "../../stores/voiceStore";
import { useAuthStore } from "../../stores/authStore";

export function useVoiceEvents() {
  const setSpeaking = useVoiceStore((s) => s.setSpeaking);
  const setMuted = useVoiceStore((s) => s.setMuted);
  const setDeafened = useVoiceStore((s) => s.setDeafened);
  const setLatency = useVoiceStore((s) => s.setLatency);
  const setError = useVoiceStore((s) => s.setError);
  const setParticipants = useVoiceStore((s) => s.setParticipants);
  const username = useAuthStore((s) => s.username);

  useEffect(() => {
    const unlisten: (() => void)[] = [];

    listen<{ serverId: string; channelId: string; participants: string[] }>(
      "voice_presence_updated",
      (event) => {
        setParticipants(
          event.payload.participants.map((u) => ({
            username: u,
            isMuted: false,
            isSpeaking: useVoiceStore.getState().speakingUsers.includes(u),
            audioLevel: 0,
          }))
        );
      }
    ).then((u) => unlisten.push(u));

    listen<{ username: string; speaking: boolean }>(
      "voice_user_speaking",
      (event) => {
        const speakingUsername =
          event.payload.username === "__local__"
            ? username ?? ""
            : event.payload.username;
        if (speakingUsername) {
          setSpeaking(speakingUsername, event.payload.speaking);
        }
      }
    ).then((u) => unlisten.push(u));

    listen<{ isMuted: boolean; isDeafened: boolean }>(
      "voice_state_changed",
      (event) => {
        setMuted(event.payload.isMuted);
        setDeafened(event.payload.isDeafened);
      }
    ).then((u) => unlisten.push(u));

    listen<{ latencyMs: number }>("voice_ping_updated", (event) => {
      setLatency(event.payload.latencyMs);
    }).then((u) => unlisten.push(u));

    listen<{ message: string }>("voice_error", (event) => {
      setError(event.payload.message);
    }).then((u) => unlisten.push(u));

    return () => {
      unlisten.forEach((fn) => fn());
    };
  }, [username, setSpeaking, setMuted, setDeafened, setLatency, setError, setParticipants]);
}
```

- [ ] **Step 2: Verify compilation (dev server)**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

Expected: No type errors.

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/features/voice/useVoiceEvents.ts
git commit -m "feat(voice): add useVoiceEvents hook for voice Tauri event listeners"
```

---

## Task 14: Frontend — VoiceControlBar Component

**Files:**
- Create: `tauri-client/src/features/voice/VoiceControlBar.tsx`

Compact voice controls shown at the bottom of channel sidebar when connected to voice.

- [ ] **Step 1: Create VoiceControlBar.tsx**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function VoiceControlBar() {
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const error = useVoiceStore((s) => s.error);
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? [] : [];
  });

  if (!connectedChannelId) return null;

  const channelName =
    channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";

  const handleMute = () => {
    invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
  };

  const handleDeafen = () => {
    invoke("set_voice_deafen", { deafened: !isDeafened }).catch(console.error);
  };

  const handleDisconnect = () => {
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  return (
    <div className="border-t border-border bg-bg-secondary px-2 py-2">
      <div className="mb-1.5 flex items-center gap-1.5 px-1">
        <span className="text-[11px] text-success">🔊 {channelName}</span>
        <span
          className="ml-auto cursor-default text-[11px] text-success"
          title={latencyMs != null ? `${latencyMs}ms` : "Measuring..."}
        >
          Connected
        </span>
      </div>
      {error && (
        <p className="mb-1 px-1 text-[10px] text-warning">{error}</p>
      )}
      <div className="flex gap-1">
        <button
          onClick={handleMute}
          className={`flex-1 rounded-md py-1.5 text-[11px] transition-colors ${
            isMuted
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex-1 rounded-md py-1.5 text-[11px] transition-colors ${
            isDeafened
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={handleDisconnect}
          className="w-9 rounded-md bg-danger py-1.5 text-center text-[11px] text-white transition-colors hover:bg-danger/80"
        >
          ✕
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/features/voice/VoiceControlBar.tsx
git commit -m "feat(voice): add VoiceControlBar component with mute/deafen/disconnect"
```

---

## Task 15: Frontend — VoiceParticipantList Component

**Files:**
- Create: `tauri-client/src/features/voice/VoiceParticipantList.tsx`

Inline participant list shown nested under the active voice channel in the sidebar.

- [ ] **Step 1: Create VoiceParticipantList.tsx**

```tsx
import { useVoiceStore } from "../../stores/voiceStore";
import { stringToColor } from "../../utils/colors";

export default function VoiceParticipantList() {
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);

  if (participants.length === 0) return null;

  return (
    <div className="space-y-0.5 pb-1 pl-5">
      {participants.map((p) => {
        const isSpeaking = speakingUsers.includes(p.username);
        return (
          <div
            key={p.username}
            className="flex items-center gap-1.5 rounded px-1.5 py-0.5 text-[11px] text-text-secondary"
          >
            <div
              className="flex h-[18px] w-[18px] flex-shrink-0 items-center justify-center rounded-md text-[8px] font-bold text-white"
              style={{ backgroundColor: stringToColor(p.username) }}
            >
              {p.username.charAt(0).toUpperCase()}
            </div>
            <span className="truncate">{p.username}</span>
            {isSpeaking && (
              <span className="text-[14px] leading-none text-success">●</span>
            )}
            {p.isMuted && (
              <span className="text-[9px] text-danger">🔇</span>
            )}
          </div>
        );
      })}
    </div>
  );
}
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/features/voice/VoiceParticipantList.tsx
git commit -m "feat(voice): add VoiceParticipantList component for sidebar inline display"
```

---

## Task 16: Frontend — VoicePanel Component

**Files:**
- Create: `tauri-client/src/features/voice/VoicePanel.tsx`

Full voice view with participant cards, speaking glow rings, and bottom controls. Replaces chat + members list area.

- [ ] **Step 1: Create VoicePanel.tsx**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useVoiceStore } from "../../stores/voiceStore";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { stringToColor } from "../../utils/colors";

export default function VoicePanel() {
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const participants = useVoiceStore((s) => s.participants);
  const speakingUsers = useVoiceStore((s) => s.speakingUsers);
  const isMuted = useVoiceStore((s) => s.isMuted);
  const isDeafened = useVoiceStore((s) => s.isDeafened);
  const latencyMs = useVoiceStore((s) => s.latencyMs);
  const disconnect = useVoiceStore((s) => s.disconnect);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const channels = useChatStore((s) => {
    const serverId = s.activeServerId;
    return serverId ? s.channelsByServer[serverId] ?? [] : [];
  });

  const channelName =
    channels.find((ch) => ch.id === connectedChannelId)?.name ?? "Voice";

  const handleMute = () => {
    invoke("set_voice_mute", { muted: !isMuted }).catch(console.error);
  };

  const handleDeafen = () => {
    invoke("set_voice_deafen", { deafened: !isDeafened }).catch(console.error);
  };

  const handleDisconnect = () => {
    invoke("leave_voice_channel").catch(console.error);
    disconnect();
    setActiveView("server");
  };

  return (
    <div className="flex flex-1 flex-col bg-bg-secondary">
      {/* Header */}
      <div className="flex items-center gap-2 border-b border-border px-4 py-3">
        <span className="text-accent">🔊</span>
        <span className="text-sm font-semibold text-text-primary">
          {channelName}
        </span>
        <span
          className="ml-auto text-xs text-text-muted"
          title={latencyMs != null ? `${latencyMs}ms` : undefined}
        >
          {participants.length} participant{participants.length !== 1 ? "s" : ""}
        </span>
      </div>

      {/* Participant cards grid */}
      <div className="flex flex-1 flex-wrap items-center justify-center gap-5 p-6">
        {participants.map((p) => {
          const isSpeaking = speakingUsers.includes(p.username);
          const color = stringToColor(p.username);

          return (
            <div key={p.username} className="w-[100px] text-center">
              <div className="relative mx-auto mb-2">
                <div
                  className="flex h-20 w-20 items-center justify-center rounded-2xl text-[28px] font-bold text-white transition-shadow duration-200"
                  style={{
                    backgroundColor: color,
                    boxShadow: isSpeaking
                      ? `0 0 0 3px ${color}, 0 0 12px rgba(74, 170, 119, 0.4)`
                      : "none",
                  }}
                >
                  {p.username.charAt(0).toUpperCase()}
                </div>
                {p.isMuted && (
                  <div className="absolute -bottom-1 -right-1 flex h-[22px] w-[22px] items-center justify-center rounded-full border-2 border-bg-secondary bg-danger text-[10px]">
                    🔇
                  </div>
                )}
              </div>
              <div className="text-xs font-medium text-text-primary">
                {p.username}
              </div>
              <div className="mt-0.5 text-[10px]">
                {isSpeaking ? (
                  <span className="text-success">Speaking</span>
                ) : p.isMuted ? (
                  <span className="text-danger">Muted</span>
                ) : null}
              </div>
            </div>
          );
        })}
      </div>

      {/* Bottom controls */}
      <div className="flex justify-center gap-3 border-t border-border bg-bg-primary px-5 py-3">
        <button
          onClick={handleMute}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs transition-colors ${
            isMuted
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isMuted ? "🔇 Unmute" : "🎤 Mute"}
        </button>
        <button
          onClick={handleDeafen}
          className={`flex items-center gap-1.5 rounded-lg px-5 py-2 text-xs transition-colors ${
            isDeafened
              ? "bg-danger/20 text-danger"
              : "bg-white/5 text-text-muted hover:bg-white/10"
          }`}
        >
          {isDeafened ? "🔇 Undeafen" : "🎧 Deafen"}
        </button>
        <button
          onClick={handleDisconnect}
          className="flex items-center gap-1.5 rounded-lg bg-danger px-5 py-2 text-xs text-white transition-colors hover:bg-danger/80"
        >
          Disconnect
        </button>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/features/voice/VoicePanel.tsx
git commit -m "feat(voice): add VoicePanel with participant cards and speaking glow rings"
```

---

## Task 17: Frontend — ChannelSidebar Voice Integration

**Files:**
- Modify: `tauri-client/src/features/channels/ChannelSidebar.tsx`

Make voice channels clickable (join voice), show VoiceParticipantList under the active voice channel, and show VoiceControlBar between channel list and user panel.

- [ ] **Step 1: Update ChannelSidebar.tsx**

Add imports at the top:

```typescript
import { useVoiceStore } from "../../stores/voiceStore";
import VoiceControlBar from "../voice/VoiceControlBar";
import VoiceParticipantList from "../voice/VoiceParticipantList";
```

Add voice state selectors inside the `ChannelSidebar` component (after the existing selectors):

```typescript
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);
  const setActiveView = useUiStore((s) => s.setActiveView);
```

Add a voice channel click handler (after `handleChannelClick`):

```typescript
  const handleVoiceChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    if (channelId === connectedChannelId) {
      // Already connected — switch to voice view
      setActiveView("voice");
      return;
    }
    // Join new voice channel
    useVoiceStore.getState().setConnectedChannel(activeServerId, channelId);
    invoke("join_voice_channel", {
      serverId: activeServerId,
      channelId,
    }).catch((err) => {
      console.error(err);
      useVoiceStore.getState().disconnect();
    });
    setActiveView("voice");
  };
```

Replace the voice channels rendering section (the `voiceChannels.map` block) with:

```tsx
            {voiceChannels.map((ch) => (
              <div key={ch.id}>
                <button
                  onClick={() => handleVoiceChannelClick(ch.id)}
                  className={`flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm transition-colors ${
                    connectedChannelId === ch.id
                      ? "bg-white/10 text-accent"
                      : "text-text-muted hover:bg-white/5 hover:text-text-primary"
                  }`}
                >
                  <span>🔊</span>
                  <span className="truncate">{ch.name}</span>
                </button>
                {connectedChannelId === ch.id && <VoiceParticipantList />}
              </div>
            ))}
```

In the server view return (the second `return` block), insert `<VoiceControlBar />` between the channel list `div` and `<UserPanel />`:

```tsx
      </div>
      <VoiceControlBar />
      <UserPanel />
    </div>
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/features/channels/ChannelSidebar.tsx
git commit -m "feat(voice): make voice channels clickable with inline participants and control bar"
```

---

## Task 18: Frontend — MainLayout Voice View Routing

**Files:**
- Modify: `tauri-client/src/layouts/MainLayout.tsx`

Add voice view rendering branch and mount the useVoiceEvents hook.

- [ ] **Step 1: Update MainLayout.tsx**

Add imports:

```typescript
import VoicePanel from "../features/voice/VoicePanel";
import { useVoiceEvents } from "../features/voice/useVoiceEvents";
```

Add the hook call inside `MainLayout` (after existing hooks):

```typescript
  useVoiceEvents();
```

Replace the inner content area (the `<div className="flex flex-1 overflow-hidden">` block) with:

```tsx
          <div className="flex flex-1 overflow-hidden">
            {activeView === "browse" ? (
              <ServerBrowseView />
            ) : (
              <>
                <ChannelSidebar />
                {activeView === "voice" ? (
                  <VoicePanel />
                ) : (
                  <>
                    <ChatPanel />
                    {activeView === "home" ? <FriendsList /> : <MembersList />}
                  </>
                )}
              </>
            )}
          </div>
```

- [ ] **Step 2: Verify TypeScript compiles**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

- [ ] **Step 3: Commit**

```bash
cd /home/sun/Desktop/decibell/decibell
git add tauri-client/src/layouts/MainLayout.tsx
git commit -m "feat(layout): add voice view routing and mount useVoiceEvents hook"
```

---

## Task 19: Integration — Full Build & Smoke Test

**Files:** None (testing only)

Verify the complete application builds and runs.

- [ ] **Step 1: Build the C++ community server**

```bash
cd /home/sun/Desktop/decibell/decibell && cmake --build build --target community_server
```

Expected: Compiles without errors.

- [ ] **Step 2: Build the Tauri app (dev mode)**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && cargo tauri dev
```

Expected: App launches, sidebar shows voice channels as clickable. Clicking a voice channel switches to voice view.

- [ ] **Step 3: Run all frontend tests**

```bash
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx vitest run
```

Expected: All store tests pass (including new voiceStore tests).

- [ ] **Step 4: Manual smoke test**

1. Start the C++ community server and central server
2. Launch the Tauri app, login, join a community
3. Click a voice channel → should switch to voice panel view
4. Click a text channel → should switch back to chat view (voice stays connected, sidebar shows controls)
5. Click the connected voice channel again → should return to voice panel
6. Test mute/deafen buttons → icons should toggle
7. Test disconnect → should return to server view
8. If a second client is available, test two-party voice

- [ ] **Step 5: Commit any fixes from smoke testing**

```bash
git add -A && git commit -m "fix: address issues found during Phase 4 integration smoke test"
```

(Only if fixes were needed.)
