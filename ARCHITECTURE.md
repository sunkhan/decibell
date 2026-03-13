# Decibell Architecture

Reference document for the Decibell codebase. This serves as the authoritative guide
for the Tauri v2 client rebuild.

## 1. System Overview

Decibell is a decentralized Discord-like application with three components:

### Central Server (`src/server/`)
- Handles user registration, login, JWT issuance
- Manages friend system (add, remove, block, accept, reject)
- Routes direct messages between users
- Tracks user presence (online/offline)
- Maintains server directory (`community_servers` table in PostgreSQL)
- Runs on port **8080** with TLS/SSL (TLSv1.2+)

### Community Servers (`src/community/`)
- User-hosted servers for group communication
- Authenticates users via JWT from central server (HS256, issuer `decibell_central_auth`)
- Manages text channels and voice channels
- Relays UDP audio/video packets between voice channel participants
- Handles screen sharing signaling (start/stop/watch)
- Runs on port **8082** (TCP/TLS) + port **8083** (UDP relay, i.e. `port + 1`)
- Hardcoded channels on auth: `general` (text), `announcements` (text), `voice-lounge` (voice)

### Client (`src/client/`)
- Qt6/QML desktop application (being replaced with Tauri v2)
- Connects to central server for auth, friends, DMs
- Connects to community servers for channels, voice, video
- Runs audio/video encoding/decoding in-process
- Sends/receives UDP packets for real-time media

### Data Flow
```
Client <--TCP/TLS--> Central Server (auth, friends, DMs, presence, discovery)
Client <--TCP/TLS--> Community Server (channels, voice signaling, stream signaling)
Client <--UDP-----> Community Server (audio/video relay to other clients)
```

---

## 2. Network Protocols

### TCP Protocol
All TCP communication uses length-prefix framing with Protobuf serialization:
- **Frame format:** `[4-byte big-endian length][Protobuf Packet bytes]`
- **Helper:** `chatproj::create_framed_packet()` in `src/common/net_utils.hpp`
- **Max body size enforced:** 2 MB (server drops packets larger than this)
- **TLS/SSL:** All TCP connections use TLS 1.2+. Client certificate verification is disabled (`ssl::verify_none`).

### Connection Details
- Central server: port **8080** (hardcoded in server `main.cpp`)
- Community servers: host/port from `ServerListResponse`; default 8082 TCP + 8083 UDP
- UDP audio/video: Client binds ephemeral port, sends to community server's `port + 1`
- UDP socket buffers: 2 MB send + 2 MB receive (configured by community server)

### Authentication Flow
1. Client connects to central server over TCP/TLS
2. Client sends `LoginRequest` (username, password)
3. Central server verifies credentials against PostgreSQL, returns `LoginResponse` with JWT
4. JWT: HS256, issuer `"decibell_central_auth"`, subject = username; secret is a hardcoded placeholder in `main()`
5. Client presents JWT in `CommunityAuthRequest.jwt_token` when connecting to community server
6. Community server verifies JWT signature and issuer, returns channel list in `CommunityAuthResponse`
7. Subsequent TCP packets from the client to the central server include `auth_token` (field 16 of `Packet`)
8. UDP packets use the last 31 chars of the JWT as a compact `sender_id` to identify the session

### Security Notes (Known Issues)
- JWT secret: hardcoded `"super_secret_decibell_key_change_in_production"` in both server `main.cpp` files
- DB connection string: hardcoded in `src/server/main.cpp` `main()`
- SSL cert paths: hardcoded to `C:/dev/chatproj-core/server.{crt,key}`
- Password hashing: `auth_manager.hpp` declares `hashPassword`/`verifyPassword` (implementation in `.cpp`)

---

## 3. Protobuf Message Catalog

All messages are wrapped in a `Packet` with a `type` field and optional `auth_token`.
Source: `proto/messages.proto`.

### Packet Envelope
```protobuf
message Packet {
  Type type = 1;
  int64 timestamp = 2;
  string auth_token = 16;   // JWT, sent by client on all post-login requests
  oneof payload { ... }
}
```

### Packet Types
| Type ID | Name | Direction | Purpose |
|---------|------|-----------|---------|
| 0 | UNKNOWN | — | Default/unset |
| 1 | HANDSHAKE | Client→Server | Protocol version negotiation (not enforced by server) |
| 2 | REGISTER_REQ | Client→Central | Register new account (username, email, password) |
| 3 | REGISTER_RES | Central→Client | Registration result (success, message) |
| 4 | LOGIN_REQ | Client→Central | Login (username, password) |
| 5 | LOGIN_RES | Central→Client | Login result (success, message, jwt_token) |
| 6 | DIRECT_MSG | Bidirectional | Private message (sender, recipient, content, timestamp) |
| 7 | PRESENCE_UPDATE | Central→Client | List of online users |
| 8 | SERVER_LIST_REQ | Client→Central | Request public server directory |
| 9 | SERVER_LIST_RES | Central→Client | List of CommunityServerInfo |
| 10 | COMMUNITY_AUTH_REQ | Client→Community | Present JWT for authentication |
| 11 | COMMUNITY_AUTH_RES | Community→Client | Auth result + channel list |
| 12 | CHANNEL_MSG | Bidirectional | Text channel message (sender, channel_id, content, timestamp) |
| 13 | JOIN_CHANNEL_REQ | Client→Community | Join a text channel |
| 14 | JOIN_CHANNEL_RES | Community→Client | Join result + active_users |
| 15 | FRIEND_ACTION_REQ | Client→Central | Friend action (ADD/REMOVE/BLOCK/ACCEPT/REJECT) |
| 16 | FRIEND_ACTION_RES | Central→Client | Friend action result |
| 17 | FRIEND_LIST_REQ | Client→Central | Request friend list |
| 18 | FRIEND_LIST_RES | Central→Client | List of FriendInfo with status |
| 19 | JOIN_VOICE_REQ | Client→Community | Join a voice channel |
| 20 | LEAVE_VOICE_REQ | Client→Community | Leave voice channel |
| 21 | VOICE_PRESENCE_UPDATE | Community→Client | Users in a voice channel |
| 22 | STREAM_PRESENCE_UPDATE | Community→Client | Active streams in voice channel |
| 23 | START_STREAM_REQ | Client→Community | Start screen sharing (fps, bitrate, has_audio) |
| 24 | STOP_STREAM_REQ | Client→Community | Stop screen sharing |
| 25 | WATCH_STREAM_REQ | Client→Community | Subscribe to a stream (not currently handled server-side) |
| 26 | STOP_WATCHING_REQ | Client→Community | Unsubscribe from a stream (not currently handled server-side) |

### Key Message Structures

```protobuf
message CommunityServerInfo {
  int32 id = 1; string name = 2; string description = 3;
  string host_ip = 4; int32 port = 5; int32 member_count = 6;
}

message ChannelInfo {
  string id = 1; string name = 2;
  enum Type { TEXT = 0; VOICE = 1; }
  Type type = 3;
}

message FriendInfo {
  string username = 1;
  enum Status { ONLINE=0; OFFLINE=1; PENDING_INCOMING=2; PENDING_OUTGOING=3; BLOCKED=4; }
  Status status = 2;
}

enum FriendActionType { ADD=0; REMOVE=1; BLOCK=2; ACCEPT=3; REJECT=4; }

message VideoStreamInfo { string stream_id=1; string owner_username=2; bool has_audio=3; }

message StartStreamRequest { string channel_id=1; int32 target_fps=2; int32 target_bitrate_kbps=3; bool has_audio=4; }
```

---

## 4. UDP Packet Formats

All structs use `#pragma pack(push, 1)` (1-byte alignment, no padding). Source: `src/common/udp_packet.hpp`.

### Constants
| Name | Value | Description |
|------|-------|-------------|
| `SENDER_ID_SIZE` | 32 | Bytes reserved for sender identifier in all packets |
| `UDP_MAX_PAYLOAD` | 1400 | Max payload bytes per packet (MTU-safe) |
| `NACK_MAX_ENTRIES` | 64 | Max missing packet indices per NACK packet |
| `FEC_GROUP_SIZE` | 5 | Data packets per FEC group |

### Packet Type Enum (`UdpPacketType : uint8_t`)
| Value | Name | Purpose |
|-------|------|---------|
| 0 | AUDIO | Voice audio data |
| 1 | VIDEO | Video/screen share frame fragment |
| 2 | KEYFRAME_REQUEST | Viewer requests I-frame from streamer (PLI) |
| 3 | NACK | Report missing video packets for retransmission |
| 4 | FEC | Forward error correction data |

### VideoCodec Enum (`VideoCodec : uint8_t`)
| Value | Name | Description |
|-------|------|-------------|
| 0 | CODEC_VP9 | VP9 software (libvpx) |
| 1 | CODEC_H264 | H.264 hardware (Media Foundation Transform) |

### UdpAudioPacket — 1437 bytes total
| Field | Offset | Size | Type | Description |
|-------|--------|------|------|-------------|
| packet_type | 0 | 1 | uint8 | Always 0 (AUDIO) |
| sender_id | 1 | 32 | char[32] | Last 31 chars of JWT (compact identifier); upstream=token hash, downstream=username |
| sequence | 33 | 2 | uint16 | Monotonic counter — drop out-of-order packets |
| payload_size | 35 | 2 | uint16 | Exact byte count of Opus-encoded audio |
| payload | 37 | 1400 | uint8[1400] | Opus-encoded audio data |

### UdpVideoPacket — 1445 bytes total
| Field | Offset | Size | Type | Description |
|-------|--------|------|------|-------------|
| packet_type | 0 | 1 | uint8 | Always 1 (VIDEO) |
| sender_id | 1 | 32 | char[32] | JWT hash (upstream) / username (downstream) |
| frame_id | 33 | 4 | uint32 | Incrementing frame counter |
| packet_index | 37 | 2 | uint16 | Fragment index within the frame |
| total_packets | 39 | 2 | uint16 | Total fragments for this frame |
| payload_size | 41 | 2 | uint16 | Encoded chunk byte count |
| is_keyframe | 43 | 1 | bool | True if this fragment belongs to a keyframe (I-frame) |
| codec | 44 | 1 | uint8 | VideoCodec: 0=VP9, 1=H.264 |
| payload | 45 | 1400 | uint8[1400] | Encoded video chunk |

### UdpFecPacket — 1443 bytes total
| Field | Offset | Size | Type | Description |
|-------|--------|------|------|-------------|
| packet_type | 0 | 1 | uint8 | Always 4 (FEC) |
| sender_id | 1 | 32 | char[32] | Same as corresponding video packets |
| frame_id | 33 | 4 | uint32 | Frame this FEC group covers |
| group_start | 37 | 2 | uint16 | packet_index of first packet in FEC group |
| group_count | 39 | 2 | uint16 | Number of data packets in group (typically FEC_GROUP_SIZE=5) |
| payload_size_xor | 41 | 2 | uint16 | XOR of all payload_sizes in the group |
| payload | 43 | 1400 | uint8[1400] | XOR of all payloads in the group (zero-padded) |

### UdpNackPacket — 199 bytes total
| Field | Offset | Size | Type | Description |
|-------|--------|------|------|-------------|
| packet_type | 0 | 1 | uint8 | Always 3 (NACK) |
| sender_id | 1 | 32 | char[32] | Viewer's JWT hash (requester) |
| target_username | 33 | 32 | char[32] | Streamer username to request retransmit from |
| frame_id | 65 | 4 | uint32 | Frame containing missing packets |
| nack_count | 69 | 2 | uint16 | Number of valid entries in missing_indices |
| missing_indices | 71 | 128 | uint16[64] | Up to 64 missing packet indices |

### UdpKeyframeRequest — 65 bytes total
| Field | Offset | Size | Type | Description |
|-------|--------|------|------|-------------|
| packet_type | 0 | 1 | uint8 | Always 2 (KEYFRAME_REQUEST) |
| sender_id | 1 | 32 | char[32] | Token hash or username of requester |
| target_username | 33 | 32 | char[32] | Username of the streamer to send PLI to |

### UDP Identity Matching
The community server uses `find_session_by_token(udp_id)`: it checks whether the session's full JWT token **ends with** the `sender_id` string from the UDP packet. On broadcast, `sender_id` is overwritten with the authenticated username before forwarding to other clients.

---

## 5. Audio Pipeline

Current implementation: `src/client/audio_engine.hpp/.cpp`

### Configuration
- Sample rate: 48 kHz
- Format: 16-bit signed PCM, mono
- Frame size: 960 samples = 20 ms
- Buffer size: 3840 bytes (4 frames)
- Codec: Opus (libopus)
- UDP identification: last 31 chars of JWT stored as `udp_id_`

### Encoding (Sender Side)
1. `QAudioSource` captures from default microphone at 48 kHz / 16-bit / mono
2. Audio accumulates in `input_buffer_`; when ≥ 3840 bytes, process in 960-sample chunks
3. Per chunk: calculate peak amplitude → emit `localAudioLevelChanged(level)`
4. If muted: substitute 960 samples of silence
5. If system audio capture is active: mix in WASAPI loopback signal (clamp to int16 range)
6. `opus_encode()` compresses 960 PCM samples → variable-length Opus frame (~40–80 bytes typically)
7. Pack into `UdpAudioPacket` with incrementing `sequence_number_`
8. Emit `sendUdpData(data, host, port)` → `ChatBackend` sends via `QUdpSocket`

### Decoding (Receiver Side)
1. `ChatBackend` receives UDP datagram, calls `audio_engine_->processDatagram(data)`
2. Packet type check: if `AUDIO`, cast to `UdpAudioPacket`
3. Extract `sender_id` as username, drop out-of-order packets by sequence number
4. `opus_decode()` → 960 PCM samples (16-bit signed, mono)
5. Calculate peak amplitude → emit `remoteUserSpeaking(username, level)`
6. Write PCM to `QAudioSink` (default speaker output device)

### System Audio Capture (Windows — WASAPI Loopback)
- Method: WASAPI loopback capture — records all system audio output
- Runs on a dedicated `std::thread` (`system_audio_thread_`)
- Output buffered in `sys_audio_fifo_` (thread-safe via `sys_audio_mutex_`)
- Drained during encoding: mixed sample-by-sample with microphone signal
- Use case: screen share with audio — enables broadcasting system audio alongside video

---

## 6. Video Pipeline

Current implementation: `src/client/video_engine.hpp/.cpp`

### Configuration
- Default resolution: 1280×720 (configurable via `StreamConfigDialog`)
- Default FPS: 30 (configurable: 5, 30, or 60)
- Default bitrate: 2500 kbps (configurable: 1500–10000 kbps)
- Adaptive bitrate: enabled by default; range 300 kbps to user-selected max
- UDP identification: last 31 chars of JWT stored as `udp_id_`

### Screen Capture Methods (Windows, priority order)
1. **DXGI Desktop Duplication** (`initDxgiCapture`) — hardware-accelerated, zero-copy GPU capture. Uses `IDXGIOutputDuplication` to obtain GPU textures directly. Captures entire screen or selected monitor.
2. **Windows Graphics Capture (WGC)** (`initWgcCapture`) — modern WinRT API for per-window capture. Targets a specific `HWND`. Implemented via PIMPL (`WgcState`).
3. **QScreen::grabWindow()** (`grabScreen`) — software fallback. CPU-based, slow at high resolutions.

Capture sources (screen index or window HWND) are selected by the user in `StreamConfigDialog` via `backend.getCaptureSources()`.

### Color Conversion (GPU, zero-copy)
- D3D11 Video Processor (`ID3D11VideoProcessor`) converts BGRA→NV12 on the GPU
- Uses persistent GPU textures: `gpu_bgra_texture_`, `nv12_texture_`
- No CPU readback required in the DXGI path

### Encoding
- **VP9 (software):** `vpx_codec_vp9_cx()` from libvpx. `codec = CODEC_VP9 (0)`. CPU-bound.
- **H.264 (hardware):** Media Foundation Transform (MFT). `codec = CODEC_H264 (1)`. GPU-accelerated. Uses `IMFTransform` with `IMFDXGIDeviceManager` for D3D11-aware texture-backed encoding (no CPU buffer copy).
- **Adaptive bitrate:** tracks NACK-to-packets-sent ratio each evaluation interval; adjusts codec bitrate between `min_bitrate_` (300 kbps) and `configured_bitrate_` (user max). Applied via `applyBitrate()`.

### Fragmentation and Transport
1. Encoded frame split into 1400-byte chunks
2. Each chunk → `UdpVideoPacket` (frame_id, packet_index, total_packets, is_keyframe, codec)
3. Sent via `sendUdpData` signal → `ChatBackend` QUdpSocket
4. FEC: XOR parity packet (`UdpFecPacket`) emitted every `FEC_GROUP_SIZE` (5) data packets
5. Retransmission (sender): ring buffer of `RETX_BUFFER_MAX_FRAMES` (30) frames of sent packets; resent on `UdpNackPacket`

### Decoding (Receiver Side)
- Per-user `DecoderState` map keyed by username
- Supports both VP9 (libvpx software) and H.264 (MFT hardware) decoders
- Frame reassembly: collects all `total_packets` fragments for a `frame_id`
- When complete: decode → emit `remoteFrameReceived(username, QVideoFrame)`
- `QVideoFrame` is pushed to registered `QVideoSink*` objects (one per viewer)
- If fragments missing and FEC cannot recover: send `UdpKeyframeRequest` → server relays PLI to streamer

### Keyframe and NACK Flow
- **Viewer → Server:** `UdpKeyframeRequest` (UDP, type=2)
- **Server → Streamer:** server calls `relay_keyframe_request(target_username, udp_socket)` and sends the PLI packet to the streamer's UDP endpoint
- **NACK:** `UdpNackPacket` sent by viewer; server calls `relay_nack(target_username)` to forward to streamer
- **PLI cooldown:** `last_pli_time` tracked per decoder to avoid flooding the streamer

---

## 7. QML UI Inventory

18 QML files in `src/client/`. The active entry point is `App.qml`. `Main.qml` is an older prototype bundled in the QML module but not used.

### Screens
| File | Type | Description |
|------|------|-------------|
| `App.qml` | Root Window | `Window` shell (420×520 login / 1280×720 main). Loads LoginScreen initially, switches on `loginSucceeded` / `loggedOut` signals. Loads Font Awesome. |
| `LoginScreen.qml` | Controller | Login/register logic. Handles button clicks, calls `backend.attemptLogin` and `backend.attemptRegister`. Animates between login and register states. |
| `LoginScreenForm.ui.qml` | Layout | Login form: username, email, password, confirm-password fields; Log In / Register buttons; error text. Animated state transition (400ms InOutQuad). |
| `MainScreen.qml` | Controller | All backend signal connections. Manages `activeServerId`, `activeChannelId`, `activeVoiceChannelId`, `activeStreamsMap`. Owns `ProfilePopup`, `StreamConfigDialog`, fullscreen window. |
| `MainScreenForm.ui.qml` | Layout | Root layout: left DM sidebar (72px) + top server bar (64px) + `ChannelsSidebarForm` (240px) + `StackLayout` (chat/voice) + right friends panel (280px). |

### Views
| File | Type | Description |
|------|------|-------------|
| `ChannelsSidebarForm.ui.qml` | Layout | Channel list (`channelsListView`). Text channels prefix `# `, voice channels prefix `🔊`. Voice participant list with speaking indicator (blue border, `isSpeaking` toggled by `userSpeakingSignal`). Voice connection status panel at bottom with audio level meter. |
| `ChatViewForm.ui.qml` | Layout | Header with channel name. `ListView` (id: `messageList`) with `BottomToTop` layout direction (newest at bottom). `TextInput` (id: `inputField`) for sending messages. Emits `usernameClicked` for profile popup. |
| `VoiceChannelViewForm.ui.qml` | Layout | 320×240 `GridView` of participant tiles. Each tile: `VideoOutput` (Qt Multimedia), speaking border (`#2CA3E8`), username plate, LIVE indicator, fullscreen button. Expanded focused-stream view (click tile to focus). Bottom toolbar: mute, camera, screen share, disconnect buttons. |

### Dialogs and Popups
| File | Type | Description |
|------|------|-------------|
| `StreamConfigDialog.qml` | Modal Popup | 420×580 modal. ComboBoxes: capture source, resolution (720p/1080p/1440p/native), FPS (5/30/60), bitrate (1500–10000 kbps). Switches: include system audio, adaptive bitrate. Emits `startStream(fps, bitrateKbps, includeAudio, sourceType, sourceId, resWidth, resHeight, adaptiveBitrate)`. |
| `ProfilePopup.qml` | Non-modal Popup | 300×350. Avatar circle (color derived from username hash). Username + online dot. Inline message `TextInput` → emits `messageSent(username, message)`. Positioned near click point, clamped to parent bounds. |

### Reusable Components
| File | Type | Dimensions | Description |
|------|------|-----------|-------------|
| `DecibellButton.ui.qml` | Button | 116×40 | Blue accent (`#2CA3E8`), hover (`#4DB8F0`), pressed (`#1E8BC3`). Open Sans Regular 14px. Border-radius 6px. |
| `DecibellTextField.ui.qml` | Input | 300×40 | Dark background (`#0c0f16`), border highlights on hover/focus. Open Sans Italic 14px. Exposes `highlightColor` property. Border-radius 8px. |
| `MessageDelegate.ui.qml` | List Item | variable height | 40×40 rounded avatar (left gutter). Username + timestamp header. Message text (color `#DCDDDE`). `showHeader` collapses avatar/header for consecutive messages from same user within 5 min. Hover: `#242528` bg. Clicking username emits `usernameClicked`. |
| `ServerCard.ui.qml` | Card | 260×180 | Server name (bold white), description (muted), member count with green dot. Border highlights `#2CA3E8` on hover. Border-radius 8px. |
| `ServerIcon.ui.qml` | Icon | 120×48 | Server initials (2 chars), `#2D3245` bg, hover `#3A405A` (animated 150ms). Border-radius 8px. |
| `UserDelegate.ui.qml` | List Item | 248×44 | Rounded-rect avatar (32×32, radius 8) + status dot (12×12 circle). Username text. Hover: `#2D3245` bg, white text. |
| `DmDelegate.ui.qml` | Icon | 48×48 | User initials squircle, `#2D3245` bg, hover `#3A405A`. Border-radius 8px. |

---

## 8. Backend ↔ Frontend Integration Points

All networking is mediated by `ChatBackend : QObject` (`src/client/backend.hpp`).
In the Tauri rebuild, `public slots` become Tauri **commands**, and Qt `signals` become Tauri **events**.

### Internal Architecture of ChatBackend
- One `boost::asio::io_context` + dedicated `network_thread_`
- `central_connection_`: `ConnectionState` holding the TLS socket to the central server
- `community_connections_`: `unordered_map<int, ConnectionState>` — one per joined community server
- `QUdpSocket* udp_socket_`: shared for all voice/video UDP traffic
- `AudioEngine` and `VideoEngine` owned as `unique_ptr`; emit `sendUdpData` connected to `udp_socket_`
- `video_sinks_`: `unordered_map<QString, vector<QVideoSink*>>` — maps username → registered sinks

### Commands (Public Slots / Q_INVOKABLE → Tauri Commands)
| Method | Args | Purpose | Rebuild Phase |
|--------|------|---------|---------------|
| `attemptLogin` | username, password | Connect to central server + send `LOGIN_REQ` | 2 |
| `attemptRegister` | username, email, password | Send `REGISTER_REQ` | 2 |
| `logout` | — | Disconnect from central + community servers | 2 |
| `requestServerList` | — | Send `SERVER_LIST_REQ` | 2 |
| `connectToCommunityServer` | serverId, host, port | Connect + send `COMMUNITY_AUTH_REQ` | 2 |
| `joinChannel` | serverId, channelId | Send `JOIN_CHANNEL_REQ` | 2 |
| `sendChannelMessage` | serverId, channelId, message | Send `CHANNEL_MSG` | 2 |
| `sendPrivateMessage` | recipient, message | Send `DIRECT_MSG` | 2 |
| `disconnectFromCommunityServer` | serverId | Close community TCP connection | 2 |
| `requestFriendList` | — | Send `FRIEND_LIST_REQ` | 2 |
| `sendFriendAction` | action (int), targetUsername | Send `FRIEND_ACTION_REQ` | 2 |
| `joinVoiceChannel` | serverId, channelId | Send `JOIN_VOICE_REQ`; start UDP | 4 |
| `leaveVoiceChannel` | — | Send `LEAVE_VOICE_REQ`; stop audio | 4 |
| `toggleMute` *(Q_INVOKABLE)* | — | Toggle `AudioEngine::setMuted` | 4 |
| `isMuted` *(Q_INVOKABLE)* | — | Returns bool | 4 |
| `startVideoStream` | serverId, channelId, fps, bitrateKbps, includeAudio, sourceType, sourceId, resWidth, resHeight, adaptiveBitrate | Send `START_STREAM_REQ`; start `VideoEngine` | 5 |
| `getCaptureSources` *(Q_INVOKABLE)* | — | Returns `QVariantList` of available screens/windows | 5 |
| `registerVideoSink` *(Q_INVOKABLE)* | username, sink (QObject*) | Register a `QVideoSink` to receive decoded frames for a user | 5 |
| `unregisterVideoSink` *(Q_INVOKABLE)* | username, sink (QObject*) | Deregister a video sink | 5 |

### Events (Qt Signals → Tauri Events)
| Signal | Payload | Purpose | Rebuild Phase |
|--------|---------|---------|---------------|
| `loginSucceeded` | — | Triggers navigation to main screen | 2 |
| `registerResponded` | success (bool), message (string) | Registration outcome | 2 |
| `loggedOut` | — | Triggers navigation back to login | 2 |
| `statusMessageChanged` | message (string) | Generic status text (login feedback) | 2 |
| `connectionLost` | errorMsg (string) | TCP connection dropped | 2 |
| `serverListReceived` | servers (QVariantList of CommunityServerInfo) | Populate server directory grid | 2 |
| `communityAuthResponded` | serverId (int), success (bool), message (string), channels (QVariantList) | Auth result + channel list | 2 |
| `messageReceived` | context (string), sender (string), content (string), timestamp (int64) | New chat/DM message | 2 |
| `friendListReceived` | friends (QVariantList of {usernameLabel, status, statusColor}) | Populate friends panel | 2 |
| `friendActionResponded` | success (bool), message (string) | Friend action result | 2 |
| `userListUpdated` | users (QStringList) | Presence update — triggers friend list refresh | 2 |
| `voicePresenceUpdated` | channelId (string), users (QStringList) | Who is in a voice channel | 4 |
| `streamPresenceUpdated` | channelId (string), streams (QVariantList of VideoStreamInfo) | Active screen shares in channel | 5 |
| `localAudioLevelChanged` | level (qreal, 0.0–1.0) | Own microphone level for VU meter | 4 |
| `remoteUserSpeaking` | username (string), level (qreal) | Remote user speaking indicator | 4 |
| `muteChanged` | muted (bool) | Mute state changed | 4 |

---

## 9. Color Palette & Typography

All colors verified against QML source files.

### Colors
| Token | Hex | ARGB (QML) | Usage |
|-------|-----|-----------|-------|
| bg-primary | `#0C0D0F` | — | Main background, left sidebar, server bar, bottom voice controls |
| bg-titlebar | `#0C0E13` | — | Darkest accent (right sidebar border, profile popup banner, expanded view bg) |
| bg-secondary | `#1A1B1E` | — | Content area (chat view bg, voice view bg, stream config bg) |
| bg-tertiary | `#242528` | — | Containers, input fields, cards, message hover, voice status panel |
| bg-input | `#0c0f16` | — | Text field background (DecibellTextField) |
| border | `#2D3245` | — | Borders, server icon bg, avatar bg, disabled states, scrollbars |
| border-hover | `#3A405A` | — | Server icon / DM avatar hover state |
| accent | `#2CA3E8` | — | Buttons, speaking indicator border, links, home icon |
| accent-hover | `#4DB8F0` | — | Button hover state |
| accent-pressed | `#1E8BC3` | — | Button pressed state |
| success | `#43B581` | — | Online status dot, success messages |
| warning | `#FAA61A` | — | Pending friend requests |
| error | `#FF4C4C` | — | Error messages, muted mic button, disconnect button, error red |
| error-hover | `#FF6B6B` | — | Disconnect/mute button hover |
| text-primary | `#FFFFFF` / `white` | — | Headers, bold labels, button text |
| text-content | `#DCDDDE` | — | Message body text |
| text-muted | `#4f6a86` @ ~53% alpha | `#884f6a86` | Timestamps, secondary labels, placeholder text, channel names |
| login-accent | `#1FB2FF` | — | Login screen "Decibell" title text |

### Typography
- **Primary font:** Open Sans family
  - `OpenSans-Regular.ttf` — body text, buttons (weight 400)
  - `OpenSans-Bold.ttf` — headers, labels (weight 700)
  - `OpenSans-Italic.ttf` — text field input font
  - `OpenSans-LightItalic.ttf` — bundled, not directly referenced in QML
- **Icon font:** Font Awesome 7 Free Solid 900 (`FontAwesome7Free-Solid-900.otf`)
  - Used in `VoiceChannelViewForm` for mute (`\uf130`/`\uf131`), expand (`\uf065`), back (`\uf060`) icons
- **Font files location:** `src/client/assets/`

### Component Dimensions
| Component | Dimensions | Notes |
|-----------|-----------|-------|
| Login window | 420×520 | Fixed (non-resizable) |
| Main window | 1280×720 | Minimum 960×540 |
| Left DM sidebar | 72px wide | |
| Top server bar | 64px tall | (ListView 48px centered) |
| Channel sidebar | 240px wide | |
| Right friends panel | 280px wide | |
| Chat header | 48px tall | |
| Chat input area | 76px tall | |
| Voice bottom controls | 80px tall | |
| Button (DecibellButton) | 116×40 | border-radius 6px |
| Text field (DecibellTextField) | 300×40 | border-radius 8px |
| Message avatar | 40×40 | border-radius 8px |
| Server card | 260×180 | border-radius 8px |
| Server icon (top bar) | 120×48 | border-radius 8px |
| User delegate (friends) | 248×44 | avatar 32×32, radius 8 |
| DM avatar | 48×48 | border-radius 8px |
| Voice tile | 300×220 | within 320×240 grid cell, border-radius 8px |
| Profile popup | 300×350 | non-modal |
| Stream config dialog | 420×580 | modal |

---

## 10. Tauri Rebuild Strategy

### Phased Plan
| Phase | Scope |
|-------|-------|
| 1 | Architecture doc + Tauri scaffold (this document + `tauri-client/`) |
| 2 | Rust networking — TCP/TLS + Protobuf for auth, friends, DMs, server list |
| 3 | Core UI — login screen, server discovery, channels sidebar, text chat |
| 4 | Voice — Opus encode/decode in Rust, voice channel UI, mute, speaking indicators |
| 5 | Video/screen sharing — DXGI capture, hardware H.264 encode, WebCodecs decode in React |
| 6 | Polish — settings, notifications, system tray, accessibility |

### Key Decisions for the Rebuild
- **No C++ FFI** — all networking and media pipelines reimplemented in Rust
- **Codec compatibility** — UDP packet format is fixed by the existing community server; the Tauri client must produce identical `UdpAudioPacket` and `UdpVideoPacket` structs
- **JWT passthrough** — community servers only verify the JWT received in `CommunityAuthRequest.jwt_token`; the same JWT is used as the `sender_id` source (last 31 chars) for UDP identification
- **Video decode in browser** — `VideoEngine.remoteFrameReceived` → use WebCodecs API or a WASM VP9/H.264 decoder; frames pushed to `<video>` or `<canvas>`
- **Tauri events for media** — audio/video level meters and speaking indicators are high-frequency; consider batching or using SharedArrayBuffer
- **Target quality:** 1440p60 streaming with minimal CPU usage (hardware paths only)

### Protocol Compatibility Requirements
The Tauri client must interoperate with the existing C++ community server without any server changes:
- TCP framing: `[4-byte big-endian length][Protobuf bytes]`
- Protobuf schema: `proto/messages.proto` — exact field numbers must match
- UDP packet structs: packed, no padding, exact byte layout per Section 4
- JWT: HS256, issuer `"decibell_central_auth"`, sent as `community_auth_req.jwt_token` and as the last 31-char `sender_id` in UDP packets
