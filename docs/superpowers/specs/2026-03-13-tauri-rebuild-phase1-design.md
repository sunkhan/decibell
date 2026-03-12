# Phase 1 Design: Architecture Documentation + Tauri v2 Scaffold

## Overview

Phase 1 of the Decibell Tauri rebuild. Two deliverables: an `ARCHITECTURE.md` documenting the existing codebase, and a buildable Tauri v2 project scaffold in `tauri-client/`.

This phase writes no networking, no real UI, no media code. It establishes the foundation for all subsequent phases.

## Background

Decibell is a decentralized Discord-like application with:
- A **central server** (C++, Boost.Asio, PostgreSQL) handling auth, friends, DMs, server discovery
- **Community servers** (C++, user-hosted) handling text channels, voice channels, screen sharing
- A **Qt6/QML client** being replaced with Tauri v2

The client communicates over the network (TCP/TLS + UDP) — no in-process C++ calls. The Tauri client will reimplement all networking in Rust and all UI in React.

## Phased Rebuild Strategy (Context)

- **Phase 1 (this spec):** Architecture doc + Tauri scaffold
- **Phase 2:** Rust networking — TCP/TLS + Protobuf for auth & messaging
- **Phase 3:** Core UI — login, server list, channels, text chat
- **Phase 4:** Voice — Opus in Rust (`audiopus` + `cpal`), voice channel UI
- **Phase 5:** Video/screen sharing — DXGI capture, FFmpeg HW encode, WebCodecs decode
- **Phase 6:** Polish — settings, notifications, system tray, accessibility

### Key Architectural Decisions

- **No C++ FFI** — Rust reimplements TCP/TLS + UDP networking and media pipelines
- **Media pipeline (Phases 4-5):** All-Rust with hardware acceleration
  - Audio: `audiopus` (Opus) + `cpal` (I/O) + WASAPI loopback
  - Video capture: DXGI Desktop Duplication via `windows` crate (zero-copy GPU)
  - Video encoding: `ffmpeg-next` with NVENC/AMF/QSV auto-selection
  - Video decoding (receiver): WebCodecs API in webview (hardware H.264 decode)
  - Codec: H.264 primary, H.265/AV1 stretch goals
- **Transport:** Reimplement existing custom UDP protocol in Rust (stays server-compatible)
- **Goal:** Discord-level 1440p60 streaming, minimal CPU via full hardware acceleration

---

## Deliverable 1: ARCHITECTURE.md

A comprehensive reference document at the repo root covering the existing codebase. Sections:

### 1.1 System Overview
- Central server role: auth (register/login), JWT issuance, friend system, DMs, presence, server directory
- Community server role: text channels, voice channels, UDP audio/video relay, screen sharing signaling
- Client role: connects to central over TCP/TLS (port 8080), connects to communities over TCP/TLS (port 8443) + UDP (port 8083)
- Decentralized model: central authority for identity, user-hosted communities for everything else

### 1.2 Network Protocols
- TCP framing: 4-byte network-order length prefix + Protobuf body (`net_utils.hpp`)
- TLS/SSL on all TCP connections (client cert verification disabled)
- Central server: 93.131.204.246:8080
- Community servers: per-server host:port from discovery, typically 8443 TCP + 8083 UDP
- Auth flow: Login → JWT (HS256, 24h, issuer "decibell_central_auth") → present JWT to community servers

### 1.3 Protobuf Message Catalog
Complete listing of all `Packet.Type` values and their message structures from `proto/messages.proto`:
- Connection: UNKNOWN, HANDSHAKE (protocol version negotiation at connect time)
- Auth: REGISTER_REQ/RES, LOGIN_REQ/RES
- Messaging: DIRECT_MSG, CHANNEL_MSG
- Discovery: SERVER_LIST_REQ/RES, COMMUNITY_AUTH_REQ/RES
- Channels: JOIN_CHANNEL_REQ/RES
- Voice: JOIN_VOICE_REQ, LEAVE_VOICE_REQ, VOICE_PRESENCE_UPDATE
- Streaming: START_STREAM_REQ, STOP_STREAM_REQ, WATCH_STREAM_REQ, STOP_WATCHING_REQ, STREAM_PRESENCE_UPDATE
- Friends: FRIEND_ACTION_REQ/RES, FRIEND_LIST_REQ/RES
- Presence: PRESENCE_UPDATE

### 1.4 UDP Packet Formats
Packed structs from `udp_packet.hpp`:
- `UdpAudioPacket`: type(1) + sender_id(32) + sequence(2) + payload_size(2) + payload(1400)
- `UdpVideoPacket`: type(1) + sender_id(32) + frame_id(4) + packet_index(2) + total_packets(2) + payload_size(2) + is_keyframe(1) + codec(1) + payload(1400)
- `UdpFecPacket`: type(1) + sender_id(32) + frame_id(4) + group_start(2) + group_count(2) + payload_size_xor(2) + payload(1400)
- `UdpNackPacket`: type(1) + sender_id(32) + target_username(32) + frame_id(4) + nack_count(2) + missing_indices(128)
- `UdpKeyframeRequest`: type(1) + sender_id(32) + target_username(32)

### 1.5 Audio Pipeline
- Codec: Opus 48kHz mono, 20ms frames (960 samples)
- Capture: QAudioSource (default mic), buffer 3840 bytes
- Encode: `opus_encode()` → ~40-80 bytes per frame
- Transport: UdpAudioPacket with sequence number
- Decode: `opus_decode()` → 960 PCM samples
- Playback: QAudioSink (default speaker)
- System audio: WASAPI loopback capture, mixed into mic signal

### 1.6 Video Pipeline
- Capture: DXGI Desktop Duplication (primary), WGC (windows), QScreen::grabWindow (fallback)
- Color conversion: D3D11 Video Processor BGRA→NV12 (GPU)
- Encode: VP9 (libvpx software) or H.264 (Media Foundation HW)
- Fragmentation: MTU 1400-byte chunks, FEC groups of 5, NACK retransmission ring buffer
- Decode: VP9 (libvpx software) or H.264 (MFT HW) per-user decoder state
- Adaptive bitrate: NACK ratio tracking, min 300kbps / max user-selected
- Frame reassembly: fragment buffer keyed by (frame_id, packet_index)

### 1.7 QML UI Inventory
Complete catalog of all 18 QML files with their screens, components, properties, and signals:
- App.qml, LoginScreen.qml, LoginScreenForm.ui.qml
- MainScreen.qml, MainScreenForm.ui.qml, Main.qml (legacy prototype, not active)
- ChannelsSidebarForm.ui.qml, ChatViewForm.ui.qml, VoiceChannelViewForm.ui.qml
- StreamConfigDialog.qml, ProfilePopup.qml
- DecibellButton.ui.qml, DecibellTextField.ui.qml, MessageDelegate.ui.qml
- ServerCard.ui.qml, ServerIcon.ui.qml, UserDelegate.ui.qml, DmDelegate.ui.qml

### 1.8 Backend ↔ Frontend Integration Points
Complete mapping of all Q_INVOKABLE methods and signals on ChatBackend — these become the Tauri commands and events in Phase 2+.

### 1.9 Color Palette & Typography
- Backgrounds: #0C0D0F (main), #0C0E13 (title bar), #1A1B1E (content), #242528 (containers)
- Accent: #2CA3E8 (blue), #4DB8F0 (hover), #1E8BC3 (pressed)
- Status: #43B581 (green/online), #FF4C4C (red/error), #FAA61A (pending)
- Text: #DCDDDE (messages), #884f6a86 (muted)
- Borders: #2D3245
- Fonts: Open Sans (Regular, Bold, Italic, LightItalic), Font Awesome 7 Free Solid

### 1.10 Tauri Rebuild Strategy
Summary of the phased plan and all architectural decisions documented above.

---

## Deliverable 2: Tauri v2 Project Scaffold

### 2.1 Project Location
`tauri-client/` directory at the repo root, alongside existing `src/`.

### 2.2 Frontend Structure (React + TypeScript + Vite + Tailwind)

```
tauri-client/
├── index.html
├── package.json
├── tsconfig.json
├── vite.config.ts
├── tailwind.config.ts      # (or CSS-based config for Tailwind v4)
├── src/
│   ├── main.tsx             # React entry point
│   ├── App.tsx              # Root component with React Router
│   ├── components/          # Reusable UI components (empty, stubbed)
│   ├── layouts/
│   │   └── AppLayout.tsx    # Placeholder shell layout
│   ├── pages/
│   │   ├── LoginPage.tsx    # Placeholder
│   │   ├── HomePage.tsx     # Placeholder
│   │   └── SettingsPage.tsx # Placeholder
│   ├── hooks/               # Empty directory
│   ├── stores/
│   │   ├── authStore.ts     # Empty Zustand store skeleton
│   │   ├── chatStore.ts     # Empty Zustand store skeleton
│   │   ├── voiceStore.ts    # Empty Zustand store skeleton
│   │   └── uiStore.ts      # Empty Zustand store skeleton
│   ├── services/            # Empty directory (Tauri command wrappers in Phase 2)
│   ├── types/
│   │   └── index.ts         # Core type stubs (User, Server, Channel, Message)
│   ├── styles/
│   │   └── globals.css      # Tailwind directives + Decibell theme tokens
│   └── utils/               # Empty directory
└── public/
    └── fonts/               # Open Sans + Font Awesome 7
```

### 2.3 Rust Backend Structure (src-tauri/)

```
tauri-client/src-tauri/
├── Cargo.toml               # Tauri v2 + serde + tokio (minimal deps)
├── tauri.conf.json           # App config
├── build.rs                  # Tauri build script
├── src/
│   ├── main.rs               # Tauri entry point
│   ├── lib.rs                # Tauri command registration
│   ├── state.rs              # Empty AppState struct
│   ├── commands/
│   │   └── mod.rs            # Placeholder ping command
│   ├── events/
│   │   └── mod.rs            # Empty
│   ├── net/
│   │   └── mod.rs            # Empty (Phase 2)
│   └── media/
│       └── mod.rs            # Empty (Phase 4-5)
└── icons/                    # Default Tauri icons (can customize later)
```

### 2.4 Tailwind Theme Configuration
CSS custom properties and/or Tailwind config mapping Decibell colors:
- `--bg-primary: #0C0D0F`
- `--bg-secondary: #1A1B1E`
- `--bg-tertiary: #242528`
- `--bg-titlebar: #0C0E13`
- `--accent: #2CA3E8`
- `--accent-hover: #4DB8F0`
- `--accent-pressed: #1E8BC3`
- `--success: #43B581`
- `--error: #FF4C4C`
- `--text-primary: #DCDDDE`
- `--text-muted: rgba(79, 106, 134, 0.53)` (note: QML source uses `#884f6a86` which is ARGB format; CSS equivalent is `#4f6a8688` or this rgba value)
- `--border: #2D3245`

### 2.5 Tauri Configuration (tauri.conf.json)
- App identifier: `com.decibell.app`
- Window title: "Decibell"
- Window size: 1280x720 (matching existing Qt client)
- Min window size: 800x600
- Decorations: true (native title bar for now)
- Dark theme default

### 2.6 Zustand Store Skeletons
Empty but typed store files so Phase 2-3 can start filling them in:
- `authStore`: username, token, isAuthenticated, login/logout actions
- `chatStore`: messages, channels, servers, activeServer, activeChannel
- `voiceStore`: connectedChannel, participants, isMuted, localAudioLevel
- `uiStore`: theme, sidebarCollapsed, activeModal

### 2.7 Placeholder Pages
Each page renders its name in white text on the Decibell dark background. React Router configured with:
- `/login` → LoginPage
- `/` → HomePage (redirects to /login if not authenticated)
- `/settings` → SettingsPage

### 2.8 Verification Criteria
Phase 1 is complete when:
- `ARCHITECTURE.md` exists at repo root with all sections populated
- `cd tauri-client && npm install && npm run tauri dev` launches a window
- Window shows dark (#0C0D0F) background with placeholder text
- Rust `ping` command is callable from the frontend and returns a response
- No errors in console

---

## Out of Scope

- Networking code (TCP/TLS, Protobuf, UDP) — Phase 2
- Real UI screens beyond placeholders — Phase 3
- Audio/video engines — Phases 4-5
- Any modifications to existing C++ code in `src/`
- System tray, auto-update, keybinds — Phase 6
