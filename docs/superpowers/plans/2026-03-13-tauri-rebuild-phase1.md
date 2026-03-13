# Phase 1: Architecture Documentation + Tauri v2 Scaffold

> **For agentic workers:** REQUIRED: Use superpowers:subagent-driven-development (if subagents available) or superpowers:executing-plans to implement this plan. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Create ARCHITECTURE.md documenting the existing Decibell codebase and a buildable Tauri v2 project scaffold in `tauri-client/`.

**Architecture:** Two independent deliverables. ARCHITECTURE.md is a pure documentation task — read source files and write a comprehensive reference. The Tauri scaffold is a project setup task — initialize a Tauri v2 app with React + TypeScript + Vite + Tailwind CSS + Zustand, configure the Decibell theme, create placeholder pages, and verify it builds and runs.

**Tech Stack:** Tauri v2, React 19, TypeScript, Vite, Tailwind CSS v4, Zustand, React Router v7

---

## Prerequisites

Before starting, verify these tools are installed:

- **Node.js** ≥ 20 and **npm** ≥ 10 (confirmed: Node 24.14, npm 11.9)
- **Rust** stable toolchain with `cargo` — **NOT currently installed**. Install via:
  ```
  winget install Rustlang.Rustup
  ```
  Then restart the shell and verify with `rustc --version && cargo --version`.
- **Tauri CLI** — available via npx (`npx @tauri-apps/cli@latest`), version 2.10.1 confirmed

---

## File Map

### Deliverable 1: Architecture Documentation

| Action | File | Responsibility |
|--------|------|---------------|
| Create | `ARCHITECTURE.md` | Complete reference of existing codebase for Tauri rebuild |

### Deliverable 2: Tauri v2 Scaffold

**Frontend (React + TypeScript):**

| Action | File | Responsibility |
|--------|------|---------------|
| Create | `tauri-client/package.json` | npm dependencies and scripts |
| Create | `tauri-client/index.html` | HTML entry point |
| Create | `tauri-client/vite.config.ts` | Vite + Tailwind plugin config |
| Create | `tauri-client/tsconfig.json` | TypeScript config |
| Create | `tauri-client/tsconfig.node.json` | TypeScript config for Vite/node |
| Create | `tauri-client/src/main.tsx` | React entry point, renders App |
| Create | `tauri-client/src/App.tsx` | Root component with React Router |
| Create | `tauri-client/src/styles/globals.css` | Tailwind directives + Decibell theme tokens |
| Create | `tauri-client/src/layouts/AppLayout.tsx` | Shell layout with dark background |
| Create | `tauri-client/src/pages/LoginPage.tsx` | Placeholder login screen |
| Create | `tauri-client/src/pages/HomePage.tsx` | Placeholder home screen |
| Create | `tauri-client/src/pages/SettingsPage.tsx` | Placeholder settings screen |
| Create | `tauri-client/src/stores/authStore.ts` | Zustand auth state skeleton |
| Create | `tauri-client/src/stores/chatStore.ts` | Zustand chat state skeleton |
| Create | `tauri-client/src/stores/voiceStore.ts` | Zustand voice state skeleton |
| Create | `tauri-client/src/stores/uiStore.ts` | Zustand UI state skeleton |
| Create | `tauri-client/src/types/index.ts` | Core TypeScript type definitions |
| Create | `tauri-client/src/vite-env.d.ts` | Vite type declarations |

**Rust Backend (src-tauri/):**

| Action | File | Responsibility |
|--------|------|---------------|
| Create | `tauri-client/src-tauri/Cargo.toml` | Rust dependencies (tauri, serde, tokio) |
| Create | `tauri-client/src-tauri/tauri.conf.json` | Tauri v2 app config |
| Create | `tauri-client/src-tauri/build.rs` | Tauri build script |
| Create | `tauri-client/src-tauri/src/main.rs` | Tauri entry point |
| Create | `tauri-client/src-tauri/src/lib.rs` | Command registration + app builder |
| Create | `tauri-client/src-tauri/src/state.rs` | Empty AppState struct |
| Create | `tauri-client/src-tauri/src/commands/mod.rs` | Placeholder ping command |
| Create | `tauri-client/src-tauri/src/events/mod.rs` | Empty module |
| Create | `tauri-client/src-tauri/src/net/mod.rs` | Empty module (Phase 2) |
| Create | `tauri-client/src-tauri/src/media/mod.rs` | Empty module (Phase 4-5) |

---

## Chunk 1: ARCHITECTURE.md

### Task 1: Write ARCHITECTURE.md

**Files:**
- Create: `ARCHITECTURE.md`

This task requires reading the existing source files to produce accurate documentation. The implementer must read each file listed below and synthesize the information into the architecture document.

**Source files to read:**
- `proto/messages.proto` — all protobuf message types
- `src/common/net_utils.hpp` — TCP framing helper
- `src/common/udp_packet.hpp` — UDP packed structs
- `src/server/main.cpp` — central server architecture
- `src/server/auth_manager.hpp` — auth + friend system
- `src/server/session_manager.hpp` — session management
- `src/community/main.cpp` — community server architecture
- `src/client/backend.hpp` — ChatBackend Q_INVOKABLE methods and signals
- `src/client/audio_engine.hpp` — audio pipeline
- `src/client/video_engine.hpp` — video pipeline
- `src/client/App.qml` — root QML entry
- `src/client/MainScreen.qml` — main screen controller
- `src/client/MainScreenForm.ui.qml` — main layout
- All other `.qml` files in `src/client/`
- `CMakeLists.txt` — build targets and dependencies

- [ ] **Step 1: Read all source files listed above**

Read every file to extract: system architecture, protocol details, message formats, API surfaces, UI components, theme values.

- [ ] **Step 2: Write ARCHITECTURE.md**

Create `ARCHITECTURE.md` at the repo root with the following sections. Content must be derived from the actual source files, not guessed.

```markdown
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
- Maintains server directory (community_servers table in PostgreSQL)
- Runs on port 8080 with TLS/SSL

### Community Servers (`src/community/`)
- User-hosted servers for group communication
- Authenticates users via JWT from central server
- Manages text channels and voice channels
- Relays UDP audio/video packets between voice channel participants
- Handles screen sharing signaling (start/stop/watch)
- Runs on port 8443 (TCP/TLS) + port 8083 (UDP relay)

### Client (`src/client/`)
- Qt6/QML desktop application (being replaced with Tauri v2)
- Connects to central server for auth, friends, DMs
- Connects to community servers for channels, voice, video
- Runs audio/video encoding/decoding in-process
- Sends/receives UDP packets for real-time media

### Data Flow
```
Client ←TCP/TLS→ Central Server (auth, friends, DMs, presence, discovery)
Client ←TCP/TLS→ Community Server (channels, voice signaling, stream signaling)
Client ←UDP→ Community Server (audio/video relay to other clients)
```

## 2. Network Protocols

### TCP Protocol
All TCP communication uses length-prefix framing with Protobuf serialization:
- **Frame format:** `[4-byte big-endian length][Protobuf Packet bytes]`
- **Helper:** `chatproj::create_framed_packet()` in `src/common/net_utils.hpp`
- **TLS/SSL:** All TCP connections use TLS. Client certificate verification is disabled.

### Connection Details
- Central server: `93.131.204.246:8080` (hardcoded in client)
- Community servers: host:port from `ServerListResponse`, typically `8443` TCP + `8083` UDP
- UDP audio/video: Client binds ephemeral port, sends to community server's UDP relay port

### Authentication Flow
1. Client sends `LoginRequest` (username, password) to central server
2. Central server verifies credentials against PostgreSQL, returns `LoginResponse` with JWT
3. JWT: HS256, issuer "decibell_central_auth", 24-hour expiry
4. Client presents JWT in `CommunityAuthRequest` when connecting to community servers
5. Community server verifies JWT signature and issuer, returns channel list

## 3. Protobuf Message Catalog

All messages are wrapped in a `Packet` with a `type` field. Source: `proto/messages.proto`.

### Packet Types
| Type | Direction | Purpose |
|------|-----------|---------|
| UNKNOWN | — | Default/unset |
| HANDSHAKE | Client→Server | Protocol version negotiation at connect time |
| REGISTER_REQ | Client→Central | Register new account (username, email, password) |
| REGISTER_RES | Central→Client | Registration result (success, message) |
| LOGIN_REQ | Client→Central | Login (username, password) |
| LOGIN_RES | Central→Client | Login result (success, message, jwt_token) |
| DIRECT_MSG | Bidirectional | Private message (sender, recipient, content, timestamp) |
| PRESENCE_UPDATE | Central→Client | List of online users |
| SERVER_LIST_REQ | Client→Central | Request public server directory |
| SERVER_LIST_RES | Central→Client | List of CommunityServerInfo |
| COMMUNITY_AUTH_REQ | Client→Community | Present JWT for authentication |
| COMMUNITY_AUTH_RES | Community→Client | Auth result + channel list |
| JOIN_CHANNEL_REQ | Client→Community | Join a text channel |
| JOIN_CHANNEL_RES | Community→Client | Join result |
| CHANNEL_MSG | Bidirectional | Text channel message (sender, channel_id, content, timestamp) |
| JOIN_VOICE_REQ | Client→Community | Join a voice channel |
| LEAVE_VOICE_REQ | Client→Community | Leave voice channel |
| VOICE_PRESENCE_UPDATE | Community→Client | Users in a voice channel |
| START_STREAM_REQ | Client→Community | Start screen sharing |
| STOP_STREAM_REQ | Client→Community | Stop screen sharing |
| WATCH_STREAM_REQ | Client→Community | Subscribe to a stream |
| STOP_WATCHING_REQ | Client→Community | Unsubscribe from a stream |
| STREAM_PRESENCE_UPDATE | Community→Client | Active streams in voice channel |
| FRIEND_ACTION_REQ | Client→Central | Friend action (ADD, REMOVE, BLOCK, ACCEPT, REJECT) |
| FRIEND_ACTION_RES | Central→Client | Friend action result |
| FRIEND_LIST_REQ | Client→Central | Request friend list |
| FRIEND_LIST_RES | Central→Client | List of FriendInfo with status |

### Key Message Structures
- `CommunityServerInfo`: id, name, description, host_ip, port, member_count
- `ChannelInfo`: id, name, type (TEXT=0, VOICE=1)
- `FriendInfo`: username, status (ONLINE=0, OFFLINE=1, PENDING_INCOMING=2, PENDING_OUTGOING=3, BLOCKED=4)
- `VideoStreamInfo`: stream_id, owner_username, has_audio
- `StartStreamRequest`: channel_id, target_fps, target_bitrate_kbps, has_audio

## 4. UDP Packet Formats

All sizes in bytes. Source: `src/common/udp_packet.hpp`.

### Packet Type Enum
| Value | Name | Purpose |
|-------|------|---------|
| 0 | AUDIO | Voice audio data |
| 1 | VIDEO | Video/screen share frame fragment |
| 2 | KEYFRAME_REQUEST | Viewer requests I-frame from streamer |
| 3 | NACK | Report missing video packets for retransmission |
| 4 | FEC | Forward error correction data |

### UdpAudioPacket (1437 bytes max)
| Field | Size | Type | Description |
|-------|------|------|-------------|
| packet_type | 1 | uint8 | Always 0 (AUDIO) |
| sender_id | 32 | char[] | Last 31 chars of JWT (compact identifier) |
| sequence | 2 | uint16 | Monotonic counter, drop out-of-order |
| payload_size | 2 | uint16 | Opus-encoded byte count |
| payload | 1400 | uint8[] | Opus-encoded audio data |

### UdpVideoPacket (1445 bytes max)
| Field | Size | Type | Description |
|-------|------|------|-------------|
| packet_type | 1 | uint8 | Always 1 (VIDEO) |
| sender_id | 32 | char[] | JWT hash identifier |
| frame_id | 4 | uint32 | Incrementing frame counter |
| packet_index | 2 | uint16 | Fragment index within frame |
| total_packets | 2 | uint16 | Total fragments for this frame |
| payload_size | 2 | uint16 | Encoded chunk byte count |
| is_keyframe | 1 | bool | True if this frame is an I-frame |
| codec | 1 | uint8 | 0=VP9, 1=H.264 |
| payload | 1400 | uint8[] | Encoded video chunk |

### UdpFecPacket (1443 bytes max)
| Field | Size | Type | Description |
|-------|------|------|-------------|
| packet_type | 1 | uint8 | Always 4 (FEC) |
| sender_id | 32 | char[] | JWT hash identifier |
| frame_id | 4 | uint32 | Frame this FEC covers |
| group_start | 2 | uint16 | First packet index in FEC group |
| group_count | 2 | uint16 | Number of data packets in group (5) |
| payload_size_xor | 2 | uint16 | XOR of all payload sizes in group |
| payload | 1400 | uint8[] | XOR of all payloads in group |

### UdpNackPacket (199 bytes max)
| Field | Size | Type | Description |
|-------|------|------|-------------|
| packet_type | 1 | uint8 | Always 3 (NACK) |
| sender_id | 32 | char[] | Viewer's JWT hash |
| target_username | 32 | char[] | Streamer to request from |
| frame_id | 4 | uint32 | Frame with missing packets |
| nack_count | 2 | uint16 | Number of missing packet indices |
| missing_indices | 128 | uint16[64] | Up to 64 missing packet indices |

### UdpKeyframeRequest (65 bytes)
| Field | Size | Type | Description |
|-------|------|------|-------------|
| packet_type | 1 | uint8 | Always 2 (KEYFRAME_REQUEST) |
| sender_id | 32 | char[] | Viewer's JWT hash |
| target_username | 32 | char[] | Streamer's username |

## 5. Audio Pipeline

Current implementation in `src/client/audio_engine.hpp/.cpp`.

### Encoding (Sender)
1. QAudioSource captures from default microphone (48kHz, 16-bit signed, mono)
2. Buffer: 3840 bytes = 4 frames of 960 samples
3. Every 960 samples (20ms): calculate peak amplitude → emit `localAudioLevelChanged(level)`
4. If muted, substitute silence; if system audio capture active, mix in loopback signal
5. `opus_encode()` compresses 960 samples → ~40-80 bytes
6. Pack into `UdpAudioPacket` with incrementing sequence number
7. Send UDP to community server relay

### Decoding (Receiver)
1. Receive `UdpAudioPacket` from UDP relay
2. `opus_decode()` → 960 PCM samples (16-bit signed, mono)
3. Calculate peak amplitude → emit `remoteUserSpeaking(username, level)`
4. Write PCM to QAudioSink (default speaker)

### System Audio Capture (Windows)
- WASAPI loopback: captures all system audio output
- Runs on separate thread
- Mixed into microphone signal: `mixed = clamp(mic + system, -32768, 32767)`
- Used when screen sharing with audio enabled

## 6. Video Pipeline

Current implementation in `src/client/video_engine.hpp/.cpp`.

### Screen Capture (Windows)
Three methods, in priority order:
1. **DXGI Desktop Duplication** — zero-copy GPU capture of entire screen. Uses `IDXGIOutputDuplication` to get GPU textures directly. Fastest method.
2. **Windows Graphics Capture (WGC)** — modern WinRT API for per-window capture. Uses `HWND` to target specific windows.
3. **QScreen::grabWindow()** — software fallback. CPU-based, slow at high resolutions.

### Color Conversion
- D3D11 Video Processor converts BGRA→NV12 on GPU (zero-copy)
- Uses persistent GPU textures, no CPU readback

### Encoding
- **VP9 (software):** libvpx `vpx_codec_vp9_cx()`. CPU-bound, codec ID = 0.
- **H.264 (hardware):** Media Foundation Transform (MFT). GPU-accelerated, codec ID = 1.
- Adaptive bitrate: tracks NACK ratio, adjusts between 300kbps and user-selected max

### Fragmentation & Transport
- Encoded frame split into 1400-byte chunks (MTU-safe)
- Each chunk sent as `UdpVideoPacket` with frame_id, packet_index, total_packets
- FEC: XOR parity packet every 5 data packets (`UdpFecPacket`)
- Retransmission: ring buffer of ~30 frames of sent packets, resend on NACK

### Decoding (Receiver)
- Per-user decoder state (VP9 software or H.264 MFT hardware)
- Frame reassembly: buffer fragments keyed by (frame_id, packet_index)
- When all fragments received → decode → emit `remoteFrameReceived(username, frame)`
- If packets lost and unrecoverable: request keyframe via `UdpKeyframeRequest`

## 7. QML UI Inventory

18 QML files in `src/client/`. The active UI uses App.qml as entry (Main.qml is a legacy prototype).

### Screens
| File | Type | Description |
|------|------|-------------|
| App.qml | Root | Window shell, switches between LoginScreen and MainScreen |
| LoginScreen.qml | Controller | Login/register logic, button handlers |
| LoginScreenForm.ui.qml | Layout | Login form (username, email, password fields, buttons) |
| MainScreen.qml | Controller | Main app logic, all backend signal connections |
| MainScreenForm.ui.qml | Layout | Discord-style layout: server bar + sidebar + content + friends |
| Main.qml | Legacy | Old prototype, not active |

### Views
| File | Type | Description |
|------|------|-------------|
| ChannelsSidebarForm.ui.qml | Layout | Channel list with text/voice icons, voice participants |
| ChatViewForm.ui.qml | Layout | Message list (inverted scroll), message input |
| VoiceChannelViewForm.ui.qml | Layout | Video grid (300x220 cells), speaking indicators, toolbar |

### Dialogs & Popups
| File | Type | Description |
|------|------|-------------|
| StreamConfigDialog.qml | Dialog | Screen share settings (source, resolution, FPS, bitrate) |
| ProfilePopup.qml | Popup | User profile card (avatar, status, message input) |

### Reusable Components
| File | Type | Description |
|------|------|-------------|
| DecibellButton.ui.qml | Button | 116x40, blue accent, hover/press states |
| DecibellTextField.ui.qml | Input | 300x40, dark bg, blue focus border |
| MessageDelegate.ui.qml | List item | Avatar + username + message + timestamp |
| ServerCard.ui.qml | Card | 260x180, server name/description/member count |
| ServerIcon.ui.qml | Icon | 120x48, server initials, hover animation |
| UserDelegate.ui.qml | List item | 248x44, avatar + status dot + username |
| DmDelegate.ui.qml | Icon | 48x48, user initials squircle |

## 8. Backend ↔ Frontend Integration Points

All methods and signals on `ChatBackend` (`src/client/backend.hpp`). These map to Tauri commands (request/response) and events (push notifications) in the rebuild.

### Commands (Q_INVOKABLE → Tauri Commands)
| Method | Args | Returns | Phase |
|--------|------|---------|-------|
| `attemptLogin` | username, password | — (emits signal) | 2 |
| `attemptRegister` | username, email, password | — (emits signal) | 2 |
| `logout` | — | — | 2 |
| `requestServerList` | — | — (emits signal) | 2 |
| `connectToCommunityServer` | serverId, host, port | — (emits signal) | 2 |
| `joinChannel` | serverId, channelId | — | 2 |
| `sendChannelMessage` | serverId, channelId, message | — | 2 |
| `sendPrivateMessage` | recipient, message | — | 2 |
| `disconnectFromCommunityServer` | serverId | — | 2 |
| `requestFriendList` | — | — (emits signal) | 2 |
| `sendFriendAction` | action, targetUsername | — (emits signal) | 2 |
| `joinVoiceChannel` | serverId, channelId | — | 4 |
| `leaveVoiceChannel` | — | — | 4 |
| `toggleMute` | — | — | 4 |
| `isMuted` | — | bool | 4 |
| `startVideoStream` | serverId, channelId, fps, bitrateKbps, includeAudio, sourceType, sourceId, resWidth, resHeight, adaptiveBitrate | — | 5 |
| `getCaptureSources` | — | QVariantList | 5 |
| `registerVideoSink` | username, sink | — | 5 |
| `unregisterVideoSink` | username, sink | — | 5 |

### Events (Signals → Tauri Events)
| Signal | Payload | Phase |
|--------|---------|-------|
| `loginSucceeded` | — | 2 |
| `registerResponded` | success, message | 2 |
| `loggedOut` | — | 2 |
| `statusMessageChanged` | message | 2 |
| `connectionLost` | errorMessage | 2 |
| `serverListReceived` | servers[] | 2 |
| `communityAuthResponded` | serverId, success, message, channels[] | 2 |
| `messageReceived` | context, sender, content, timestamp | 2 |
| `friendListReceived` | friends[] | 2 |
| `friendActionResponded` | success, message | 2 |
| `voicePresenceUpdated` | channelId, users[] | 4 |
| `streamPresenceUpdated` | channelId, streams[] | 5 |
| `localAudioLevelChanged` | level (0.0-1.0) | 4 |
| `remoteUserSpeaking` | username, level | 4 |
| `muteChanged` | muted | 4 |
| `userListUpdated` | users[] | 2 |

## 9. Color Palette & Typography

### Colors
| Token | Hex | Usage |
|-------|-----|-------|
| bg-primary | #0C0D0F | Main background, sidebars, server bar |
| bg-titlebar | #0C0E13 | Title bar (darkest) |
| bg-secondary | #1A1B1E | Content area (chat, voice view) |
| bg-tertiary | #242528 | Containers, input fields, cards |
| border | #2D3245 | Borders, disabled states |
| accent | #2CA3E8 | Buttons, speaking indicator, links |
| accent-hover | #4DB8F0 | Button hover state |
| accent-pressed | #1E8BC3 | Button pressed state |
| success | #43B581 | Online status, success messages |
| warning | #FAA61A | Pending friend requests |
| error | #FF4C4C | Error messages, destructive actions |
| text-primary | #DCDDDE | Message text, primary content |
| text-muted | rgba(79, 106, 134, 0.53) | Timestamps, secondary text (QML: #884f6a86 ARGB) |

### Typography
- **Primary font:** Open Sans (Regular 400, Bold 700, Italic, Light Italic)
- **Icon font:** Font Awesome 7 Free Solid 900
- **Font files:** `src/client/assets/OpenSans-*.ttf`, `FontAwesome7Free-Solid-900.otf`

### Component Dimensions (from QML)
- Login window: 420×520
- Main window: 1280×720
- Server bar height: 48px
- Left sidebar (DMs): 72px wide
- Channel sidebar: 240px wide
- Friends panel: 280px wide
- Button: 116×40, border-radius 6px
- Text field: 300×40, border-radius 8px
- Message avatar: 40×40, rounded
- Server card: 260×180, border-radius 8px
- Server icon: 120×48, border-radius 8px
- User delegate: 248×44
- DM avatar: 48×48, border-radius 8px

## 10. Tauri Rebuild Strategy

### Phased Plan
1. **Phase 1:** Architecture doc + Tauri scaffold (this document)
2. **Phase 2:** Rust networking — TCP/TLS + Protobuf for auth & messaging
3. **Phase 3:** Core UI — login, server list, channels, text chat
4. **Phase 4:** Voice — Opus in Rust + voice channel UI
5. **Phase 5:** Video/screen sharing — DXGI capture, FFmpeg HW encode, WebCodecs decode
6. **Phase 6:** Polish — settings, notifications, system tray, accessibility

### Key Decisions
- **No C++ FFI** — all networking and media reimplemented in Rust
- **Media pipeline:** Hardware-accelerated (NVENC/AMF/QSV encode, WebCodecs decode)
- **Transport:** Compatible with existing community server UDP protocol
- **Target:** Discord-level 1440p60 streaming quality with minimal CPU usage
```

- [ ] **Step 3: Verify ARCHITECTURE.md content accuracy**

Spot-check at least 3 sections by re-reading the corresponding source files. Verify:
- Protobuf types match `proto/messages.proto` exactly
- UDP struct sizes match `src/common/udp_packet.hpp`
- Backend signals/methods match `src/client/backend.hpp`

- [ ] **Step 4: Commit**

```bash
git add ARCHITECTURE.md
git commit -m "docs: add comprehensive architecture reference for Tauri rebuild"
```

---

## Chunk 2: Tauri v2 Project Scaffold

### Task 2: Verify prerequisites

- [ ] **Step 1: Check Rust is installed**

Run: `rustc --version && cargo --version`
Expected: Rust stable version (e.g., `rustc 1.85.x`)

If Rust is not installed:
```bash
winget install Rustlang.Rustup
```
Then restart shell and re-verify.

- [ ] **Step 2: Verify Tauri CLI**

Run: `npx @tauri-apps/cli@latest --version`
Expected: `tauri-cli 2.x.x`

---

### Task 3: Initialize Tauri v2 project

**Files:**
- Create: `tauri-client/package.json`
- Create: `tauri-client/index.html`
- Create: `tauri-client/vite.config.ts`
- Create: `tauri-client/tsconfig.json`
- Create: `tauri-client/tsconfig.node.json`
- Create: `tauri-client/src/main.tsx`
- Create: `tauri-client/src/vite-env.d.ts`
- Create: `tauri-client/src-tauri/Cargo.toml`
- Create: `tauri-client/src-tauri/tauri.conf.json`
- Create: `tauri-client/src-tauri/build.rs`
- Create: `tauri-client/src-tauri/src/main.rs`
- Create: `tauri-client/src-tauri/src/lib.rs`

- [ ] **Step 1: Update .gitignore before creating any files**

Add these entries to `.gitignore` at the repo root (create it if it doesn't exist) to prevent committing build artifacts:

```
tauri-client/node_modules/
tauri-client/src-tauri/target/
tauri-client/dist/
.superpowers/
```

```bash
cd C:/dev/chatproj-core
git add .gitignore
git commit -m "chore: add gitignore entries for Tauri build artifacts"
```

- [ ] **Step 2: Create the Tauri project using the CLI**

```bash
cd C:/dev/chatproj-core
npm create tauri-app@latest tauri-client -- --template react-ts --manager npm
```

This scaffolds a Tauri v2 + Vite + React + TypeScript project.

**Note for agentic workers:** This command may prompt interactively for project name or other options. If it hangs, kill it and manually create the scaffold by following Tauri v2 docs: create `package.json` with vite/react/tauri deps, `vite.config.ts`, `tsconfig.json`, `index.html`, `src/main.tsx`, and `src-tauri/` with `Cargo.toml`, `tauri.conf.json`, `build.rs`, `src/main.rs`, `src/lib.rs`.

- [ ] **Step 3: Install dependencies**

```bash
cd C:/dev/chatproj-core/tauri-client
npm install
```

Expected: Clean install, `node_modules/` created.

- [ ] **Step 4: Install additional frontend dependencies**

```bash
cd C:/dev/chatproj-core/tauri-client
npm install zustand react-router-dom
npm install -D tailwindcss @tailwindcss/vite
```

- [ ] **Step 5: Configure Vite with Tailwind plugin**

Update `tauri-client/vite.config.ts`:

```typescript
import { defineConfig } from "vite";
import react from "@vitejs/plugin-react";
import tailwindcss from "@tailwindcss/vite";

const host = process.env.TAURI_DEV_HOST;

export default defineConfig(async () => ({
  plugins: [react(), tailwindcss()],
  clearScreen: false,
  server: {
    port: 1420,
    strictPort: true,
    host: host || false,
    hmr: host
      ? {
          protocol: "ws",
          host,
          port: 1421,
        }
      : undefined,
    watch: {
      ignored: ["**/src-tauri/**"],
    },
  },
}));
```

- [ ] **Step 6: Verify the project compiles (frontend only)**

```bash
cd C:/dev/chatproj-core/tauri-client
npx vite build
```

Expected: Build succeeds, `dist/` directory created.

- [ ] **Step 7: Commit scaffold**

```bash
cd C:/dev/chatproj-core
git add tauri-client/
git commit -m "feat: initialize Tauri v2 project with React + TypeScript + Vite"
```

---

### Task 4: Configure Tailwind theme and global styles

**Files:**
- Create: `tauri-client/src/styles/globals.css`
- Modify: `tauri-client/src/main.tsx` (import globals.css)

- [ ] **Step 1: Create globals.css with Decibell theme**

Create `tauri-client/src/styles/globals.css`:

```css
@import "tailwindcss";

@theme {
  --color-bg-primary: #0C0D0F;
  --color-bg-titlebar: #0C0E13;
  --color-bg-secondary: #1A1B1E;
  --color-bg-tertiary: #242528;
  --color-border: #2D3245;

  --color-accent: #2CA3E8;
  --color-accent-hover: #4DB8F0;
  --color-accent-pressed: #1E8BC3;

  --color-success: #43B581;
  --color-warning: #FAA61A;
  --color-error: #FF4C4C;

  --color-text-primary: #DCDDDE;
  --color-text-muted: rgba(79, 106, 134, 0.53);

  --font-sans: "Open Sans", ui-sans-serif, system-ui, sans-serif;
}

@layer base {
  body {
    @apply bg-bg-primary text-text-primary font-sans antialiased;
    margin: 0;
    overflow: hidden;
  }

  * {
    scrollbar-width: thin;
    scrollbar-color: theme(--color-bg-tertiary) transparent;
  }
}
```

- [ ] **Step 2: Update main.tsx to import globals.css**

Update `tauri-client/src/main.tsx` — replace any existing CSS import with:

```typescript
import React from "react";
import ReactDOM from "react-dom/client";
import App from "./App";
import "./styles/globals.css";

ReactDOM.createRoot(document.getElementById("root") as HTMLElement).render(
  <React.StrictMode>
    <App />
  </React.StrictMode>,
);
```

- [ ] **Step 3: Remove default CSS files**

Delete any default CSS files created by the scaffolding (e.g., `src/App.css`, `src/index.css`, `src/styles.css`) that are no longer imported.

- [ ] **Step 4: Verify Tailwind works**

```bash
cd C:/dev/chatproj-core/tauri-client
npx vite build
```

Expected: Build succeeds with no CSS errors.

- [ ] **Step 5: Commit**

```bash
cd C:/dev/chatproj-core
git add tauri-client/src/styles/ tauri-client/src/main.tsx
git add -u tauri-client/src/  # stages deleted default CSS files
git commit -m "feat: configure Tailwind CSS v4 with Decibell theme tokens"
```

---

### Task 5: Copy font assets

**Files:**
- Create: `tauri-client/public/fonts/OpenSans-Regular.ttf`
- Create: `tauri-client/public/fonts/OpenSans-Bold.ttf`
- Create: `tauri-client/public/fonts/OpenSans-Italic.ttf`
- Create: `tauri-client/public/fonts/OpenSans-LightItalic.ttf`
- Create: `tauri-client/public/fonts/FontAwesome7Free-Solid-900.otf`
- Modify: `tauri-client/src/styles/globals.css` (add @font-face rules)

- [ ] **Step 1: Copy fonts from existing client assets**

```bash
mkdir -p C:/dev/chatproj-core/tauri-client/public/fonts
cp C:/dev/chatproj-core/src/client/assets/OpenSans-*.ttf C:/dev/chatproj-core/tauri-client/public/fonts/
cp C:/dev/chatproj-core/src/client/assets/FontAwesome7Free-Solid-900.otf C:/dev/chatproj-core/tauri-client/public/fonts/
```

- [ ] **Step 2: Add @font-face declarations to globals.css**

Add to the top of `tauri-client/src/styles/globals.css` (before the `@import "tailwindcss"` line):

```css
@font-face {
  font-family: "Open Sans";
  src: url("/fonts/OpenSans-Regular.ttf") format("truetype");
  font-weight: 400;
  font-style: normal;
}

@font-face {
  font-family: "Open Sans";
  src: url("/fonts/OpenSans-Bold.ttf") format("truetype");
  font-weight: 700;
  font-style: normal;
}

@font-face {
  font-family: "Open Sans";
  src: url("/fonts/OpenSans-Italic.ttf") format("truetype");
  font-weight: 400;
  font-style: italic;
}

@font-face {
  font-family: "Open Sans";
  src: url("/fonts/OpenSans-LightItalic.ttf") format("truetype");
  font-weight: 300;
  font-style: italic;
}

@font-face {
  font-family: "Font Awesome 7 Free";
  src: url("/fonts/FontAwesome7Free-Solid-900.otf") format("opentype");
  font-weight: 900;
  font-style: normal;
}
```

- [ ] **Step 3: Verify fonts load**

```bash
cd C:/dev/chatproj-core/tauri-client
npx vite build
```

Expected: Build succeeds. Font files included in output.

- [ ] **Step 4: Commit**

```bash
cd C:/dev/chatproj-core
git add tauri-client/public/fonts/ tauri-client/src/styles/globals.css
git commit -m "feat: add Open Sans and Font Awesome 7 font assets"
```

---

### Task 6: Create TypeScript types and Zustand stores

**Files:**
- Create: `tauri-client/src/types/index.ts`
- Create: `tauri-client/src/stores/authStore.ts`
- Create: `tauri-client/src/stores/chatStore.ts`
- Create: `tauri-client/src/stores/voiceStore.ts`
- Create: `tauri-client/src/stores/uiStore.ts`

- [ ] **Step 1: Create core type definitions**

Create `tauri-client/src/types/index.ts`:

```typescript
export interface User {
  username: string;
  status: "online" | "offline" | "idle" | "dnd";
}

export interface FriendInfo {
  username: string;
  status: "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked";
}

export interface CommunityServer {
  id: string;
  name: string;
  description: string;
  hostIp: string;
  port: number;
  memberCount: number;
}

export interface Channel {
  id: string;
  name: string;
  type: "text" | "voice";
}

export interface Message {
  sender: string;
  content: string;
  timestamp: string;
  channelId: string;
}

export interface VoiceParticipant {
  username: string;
  isMuted: boolean;
  isSpeaking: boolean;
  audioLevel: number;
}

export interface StreamInfo {
  streamId: string;
  ownerUsername: string;
  hasAudio: boolean;
}
```

- [ ] **Step 2: Create auth store**

Create `tauri-client/src/stores/authStore.ts`:

```typescript
import { create } from "zustand";

interface AuthState {
  username: string | null;
  token: string | null;
  isAuthenticated: boolean;
  login: (username: string, token: string) => void;
  logout: () => void;
}

export const useAuthStore = create<AuthState>((set) => ({
  username: null,
  token: null,
  isAuthenticated: false,
  login: (username, token) =>
    set({ username, token, isAuthenticated: true }),
  logout: () =>
    set({ username: null, token: null, isAuthenticated: false }),
}));
```

- [ ] **Step 3: Create chat store**

Create `tauri-client/src/stores/chatStore.ts`:

```typescript
import { create } from "zustand";
import type { CommunityServer, Channel, Message } from "../types";

interface ChatState {
  servers: CommunityServer[];
  channels: Channel[];
  messages: Message[];
  activeServerId: string | null;
  activeChannelId: string | null;
  setServers: (servers: CommunityServer[]) => void;
  setChannels: (channels: Channel[]) => void;
  addMessage: (message: Message) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  servers: [],
  channels: [],
  messages: [],
  activeServerId: null,
  activeChannelId: null,
  setServers: (servers) => set({ servers }),
  setChannels: (channels) => set({ channels }),
  addMessage: (message) =>
    set((state) => ({ messages: [...state.messages, message] })),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),
}));
```

- [ ] **Step 4: Create voice store**

Create `tauri-client/src/stores/voiceStore.ts`:

```typescript
import { create } from "zustand";
import type { VoiceParticipant, StreamInfo } from "../types";

interface VoiceState {
  connectedChannelId: string | null;
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  isMuted: boolean;
  localAudioLevel: number;
  setConnectedChannel: (channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  setActiveStreams: (streams: StreamInfo[]) => void;
  setMuted: (muted: boolean) => void;
  setLocalAudioLevel: (level: number) => void;
}

export const useVoiceStore = create<VoiceState>((set) => ({
  connectedChannelId: null,
  participants: [],
  activeStreams: [],
  isMuted: false,
  localAudioLevel: 0,
  setConnectedChannel: (channelId) => set({ connectedChannelId: channelId }),
  setParticipants: (participants) => set({ participants }),
  setActiveStreams: (streams) => set({ activeStreams: streams }),
  setMuted: (muted) => set({ isMuted: muted }),
  setLocalAudioLevel: (level) => set({ localAudioLevel: level }),
}));
```

- [ ] **Step 5: Create UI store**

Create `tauri-client/src/stores/uiStore.ts`:

```typescript
import { create } from "zustand";

interface UiState {
  sidebarCollapsed: boolean;
  activeModal: string | null;
  toggleSidebar: () => void;
  openModal: (modalId: string) => void;
  closeModal: () => void;
}

export const useUiStore = create<UiState>((set) => ({
  sidebarCollapsed: false,
  activeModal: null,
  toggleSidebar: () =>
    set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed })),
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
}));
```

- [ ] **Step 6: Verify TypeScript compiles**

```bash
cd C:/dev/chatproj-core/tauri-client
npx tsc --noEmit
```

Expected: No TypeScript errors.

- [ ] **Step 7: Commit**

```bash
cd C:/dev/chatproj-core
git add tauri-client/src/types/ tauri-client/src/stores/
git commit -m "feat: add TypeScript types and Zustand store skeletons"
```

---

### Task 7: Create placeholder pages and routing

**Files:**
- Create: `tauri-client/src/layouts/AppLayout.tsx`
- Create: `tauri-client/src/pages/LoginPage.tsx`
- Create: `tauri-client/src/pages/HomePage.tsx`
- Create: `tauri-client/src/pages/SettingsPage.tsx`
- Modify: `tauri-client/src/App.tsx`

- [ ] **Step 1: Create AppLayout**

Create `tauri-client/src/layouts/AppLayout.tsx`:

```typescript
import { Outlet } from "react-router-dom";

export default function AppLayout() {
  return (
    <div className="flex h-screen w-screen bg-bg-primary text-text-primary">
      <Outlet />
    </div>
  );
}
```

- [ ] **Step 2: Create LoginPage**

Create `tauri-client/src/pages/LoginPage.tsx`:

```typescript
export default function LoginPage() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <h1 className="text-4xl font-bold text-accent mb-4">Decibell</h1>
        <p className="text-text-muted">Login page — coming in Phase 3</p>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Create HomePage**

Create `tauri-client/src/pages/HomePage.tsx`:

```typescript
export default function HomePage() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <h1 className="text-2xl font-bold mb-2">Home</h1>
        <p className="text-text-muted">Main app view — coming in Phase 3</p>
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Create SettingsPage**

Create `tauri-client/src/pages/SettingsPage.tsx`:

```typescript
export default function SettingsPage() {
  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <h1 className="text-2xl font-bold mb-2">Settings</h1>
        <p className="text-text-muted">Settings — coming in Phase 6</p>
      </div>
    </div>
  );
}
```

- [ ] **Step 5: Set up React Router in App.tsx**

Replace `tauri-client/src/App.tsx` with:

```typescript
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import AppLayout from "./layouts/AppLayout";
import LoginPage from "./pages/LoginPage";
import HomePage from "./pages/HomePage";
import SettingsPage from "./pages/SettingsPage";
import { useAuthStore } from "./stores/authStore";

function ProtectedRoute({ children }: { children: React.ReactNode }) {
  const isAuthenticated = useAuthStore((s) => s.isAuthenticated);
  if (!isAuthenticated) {
    return <Navigate to="/login" replace />;
  }
  return <>{children}</>;
}

export default function App() {
  return (
    <BrowserRouter>
      <Routes>
        <Route element={<AppLayout />}>
          <Route path="/login" element={<LoginPage />} />
          <Route
            path="/"
            element={
              <ProtectedRoute>
                <HomePage />
              </ProtectedRoute>
            }
          />
          <Route
            path="/settings"
            element={
              <ProtectedRoute>
                <SettingsPage />
              </ProtectedRoute>
            }
          />
        </Route>
      </Routes>
    </BrowserRouter>
  );
}
```

- [ ] **Step 6: Verify frontend builds**

```bash
cd C:/dev/chatproj-core/tauri-client
npx vite build
```

Expected: Build succeeds.

- [ ] **Step 7: Commit**

```bash
cd C:/dev/chatproj-core
git add tauri-client/src/layouts/ tauri-client/src/pages/ tauri-client/src/App.tsx
git commit -m "feat: add placeholder pages with React Router and protected routes"
```

---

### Task 8: Set up Rust backend structure with ping command

**Files:**
- Modify: `tauri-client/src-tauri/src/lib.rs`
- Create: `tauri-client/src-tauri/src/state.rs`
- Create: `tauri-client/src-tauri/src/commands/mod.rs`
- Create: `tauri-client/src-tauri/src/events/mod.rs`
- Create: `tauri-client/src-tauri/src/net/mod.rs`
- Create: `tauri-client/src-tauri/src/media/mod.rs`

- [ ] **Step 1: Create module files**

Create `tauri-client/src-tauri/src/state.rs`:

```rust
#[derive(Debug, Default)]
pub struct AppState {
    // Will be populated in Phase 2 with auth state, connections, etc.
}
```

Create `tauri-client/src-tauri/src/commands/mod.rs`:

```rust
#[tauri::command]
pub fn ping() -> String {
    "pong".to_string()
}
```

Create `tauri-client/src-tauri/src/events/mod.rs`:

```rust
// Event definitions for Tauri event system.
// Will be populated in Phase 2 with message, presence, and auth events.
```

Create `tauri-client/src-tauri/src/net/mod.rs`:

```rust
// Networking module: TCP/TLS + UDP communication with servers.
// Will be implemented in Phase 2.
```

Create `tauri-client/src-tauri/src/media/mod.rs`:

```rust
// Media engines: audio (Opus) and video (H.264 HW encode/decode).
// Will be implemented in Phases 4-5.
```

- [ ] **Step 2: Update lib.rs to register modules and commands**

Replace `tauri-client/src-tauri/src/lib.rs` with:

```rust
mod commands;
mod events;
mod media;
mod net;
mod state;

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![commands::ping])
        .run(tauri::generate_context!())
        .expect("error while running tauri application");
}
```

- [ ] **Step 3: Ensure Cargo.toml has serde dependency**

Check `tauri-client/src-tauri/Cargo.toml` and ensure `serde` is listed under `[dependencies]` with the `derive` feature. If not, add:

```toml
serde = { version = "1", features = ["derive"] }
```

- [ ] **Step 4: Verify Rust compiles**

```bash
cd C:/dev/chatproj-core/tauri-client/src-tauri
cargo check
```

Expected: Compilation succeeds with no errors.

- [ ] **Step 5: Commit**

```bash
cd C:/dev/chatproj-core
git add tauri-client/src-tauri/src/
git commit -m "feat: set up Rust backend modules with ping command"
```

---

### Task 9: Configure Tauri window and verify end-to-end

**Files:**
- Modify: `tauri-client/src-tauri/tauri.conf.json`
- Modify: `tauri-client/src/pages/LoginPage.tsx` (add ping test)

- [ ] **Step 1: Update tauri.conf.json**

Update the window configuration in `tauri-client/src-tauri/tauri.conf.json`. Find the existing `"windows"` array (or `"app" > "windows"`) and update to:

```json
{
  "label": "main",
  "title": "Decibell",
  "width": 1280,
  "height": 720,
  "minWidth": 800,
  "minHeight": 600
}
```

Also update the `identifier` field to `"com.decibell.app"`.

- [ ] **Step 2: Add ping test button to LoginPage**

Update `tauri-client/src/pages/LoginPage.tsx`:

```typescript
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

export default function LoginPage() {
  const [pingResult, setPingResult] = useState<string>("");

  async function handlePing() {
    const result = await invoke<string>("ping");
    setPingResult(result);
  }

  return (
    <div className="flex h-full w-full items-center justify-center">
      <div className="text-center">
        <h1 className="text-4xl font-bold text-accent mb-4">Decibell</h1>
        <p className="text-text-muted mb-6">Login page — coming in Phase 3</p>
        <button
          onClick={handlePing}
          className="rounded-md bg-accent px-6 py-2 text-white font-bold hover:bg-accent-hover active:bg-accent-pressed transition-colors"
        >
          Ping Backend
        </button>
        {pingResult && (
          <p className="mt-4 text-success">Backend says: {pingResult}</p>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Build and run the full Tauri app**

```bash
cd C:/dev/chatproj-core/tauri-client
npm run tauri dev
```

Expected:
- Window opens with title "Decibell" at 1280×720
- Dark background (#0C0D0F)
- "Decibell" heading in blue (#2CA3E8)
- "Ping Backend" button visible
- Clicking the button shows "Backend says: pong" in green

- [ ] **Step 4: Verify all acceptance criteria**

1. ✅ `ARCHITECTURE.md` exists at repo root with all 10 sections
2. ✅ `npm run tauri dev` launches a window from `tauri-client/`
3. ✅ Window shows dark background with Decibell branding
4. ✅ Ping command works (frontend → Rust → frontend)
5. ✅ No errors in browser console or Rust terminal

- [ ] **Step 5: Commit**

```bash
cd C:/dev/chatproj-core
git add tauri-client/src-tauri/tauri.conf.json tauri-client/src/pages/LoginPage.tsx
git commit -m "feat: configure Decibell window and verify end-to-end IPC"
```

