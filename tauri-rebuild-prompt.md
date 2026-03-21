# Tauri Client Rebuild — CLI Agent Prompt

## Context

You are rebuilding the frontend client of a decentralized Discord-like application. The existing codebase has:

- A **C++ backend** handling core logic, networking, and media (voice/video/screen sharing)
- A **Qt6/QML GUI client** that you are replacing with a **Tauri (v2)** client
- **libwebrtc** integration for real-time media streaming

Your job is to create a fully functional Tauri v2 desktop client that replicates the look, feel, and functionality of the existing Qt6/QML client while wiring into the existing C++ backend.

---

## Step 0 — Understand the Project First

**Before writing any code, thoroughly explore the entire project.**

1. Walk through every directory and file in the project. Read all source files, config files, build files, and documentation.
2. Map out the full architecture:
   - How the C++ backend is structured (modules, classes, entry points)
   - How the Qt6/QML client communicates with the backend (IPC, direct calls, signals/slots, shared memory, sockets, etc.)
   - What libwebrtc is used for and how it's integrated (capture, encoding, transport, rendering)
   - All network protocols in use (WebRTC, WebSocket, custom protocols, P2P/DHT, etc.)
   - Data models and state management (users, servers, channels, messages, presence, permissions)
3. Catalog every feature and screen in the Qt6/QML client:
   - All views/pages (login, server list, channel list, chat, voice channel, video call, screen share, settings, user profile, etc.)
   - All interactive components (message input, emoji picker, file upload, context menus, modals, notifications, etc.)
   - Theming, styling, layout patterns, animations, and transitions
   - How media streams (audio/video/screen) are rendered in the QML UI
4. Identify every integration point between the QML frontend and the C++ backend — these are what you need to replicate as Tauri commands/events.
5. Document your findings as a `ARCHITECTURE.md` file before proceeding.

---

## Step 1 — Set Up the Tauri v2 Project

1. Initialize a new Tauri v2 project inside the repository (e.g., in a `tauri-client/` directory).
2. Use **React + TypeScript** for the frontend (with Vite as the bundler).
3. Use **Tailwind CSS** for styling.
4. Set up the project structure:

```
tauri-client/
├── src/                    # React frontend
│   ├── components/         # Reusable UI components
│   ├── layouts/            # Layout shells (sidebar + main content, etc.)
│   ├── pages/              # Top-level views/screens
│   ├── hooks/              # Custom React hooks (useVoice, useChat, usePresence, etc.)
│   ├── stores/             # State management (Zustand or similar)
│   ├── services/           # Frontend service layer (Tauri command wrappers)
│   ├── types/              # TypeScript type definitions
│   ├── styles/             # Global styles, Tailwind config, theme tokens
│   └── utils/              # Helpers and utilities
├── src-tauri/
│   ├── src/
│   │   ├── main.rs         # Tauri entry point
│   │   ├── commands/       # Tauri command handlers (exposed to frontend)
│   │   ├── events/         # Tauri event emitters/listeners
│   │   ├── bridge/         # FFI bindings to the C++ backend
│   │   └── state.rs        # Shared application state
│   ├── Cargo.toml
│   └── tauri.conf.json
└── package.json
```

---

## Step 2 — Build the Rust ↔ C++ Bridge

1. Examine how the Qt6/QML client interfaces with the C++ backend. Identify all function calls, callbacks, signals, and data flows.
2. Create Rust FFI bindings to the C++ backend. Choose the appropriate method based on the backend's structure:
   - **`cxx`** for safe, idiomatic bidirectional C++/Rust interop (preferred if feasible)
   - **`bindgen`** for generating raw FFI bindings from C++ headers
   - **Raw FFI (`extern "C"`)** if the backend already exposes a C-compatible API
3. Wrap the FFI layer in safe Rust abstractions in `src-tauri/src/bridge/`.
4. Expose these as **Tauri commands** (for request/response patterns) and **Tauri events** (for streaming/push data like incoming messages, presence updates, audio levels, call state changes).
5. For libwebrtc specifically:
   - Bind to the existing libwebrtc integration in the C++ backend
   - Ensure media streams (audio, video, screen capture) can be routed to the frontend for rendering
   - Determine the best transport for video frames to the webview (local WebSocket streaming, shared memory + polling, or direct WebRTC in the webview if the architecture supports it)

---

## Step 3 — Replicate the UI

Rebuild every screen and component from the Qt6/QML client in React. **Match the existing design as closely as possible.** Reference the QML files directly for layout, spacing, colors, typography, and component hierarchy.

### Core Layout
- Discord-style layout: server sidebar (left icon strip) → channel sidebar → main content area → optional member list (right)
- Top bar with channel name, call controls, search, and utility icons
- Bottom bar with user info, mic/deafen/settings controls

### Key Screens & Components
Rebuild all of the following (and any others found in the QML client):

- **Authentication:** Login, registration, account recovery
- **Server list:** Collapsible icon sidebar with server avatars, unread indicators, notification badges
- **Channel list:** Categorized channels (text, voice, video), drag-to-reorder if present in original, collapse/expand categories
- **Text chat:** Message list with infinite scroll, message grouping by author/time, markdown rendering, embeds, file attachments, image previews, reply threads, reactions, edit/delete
- **Message input:** Rich text input with emoji picker, file upload, mention autocomplete (@user, #channel), typing indicators
- **Voice channel:** Connected users list with speaking indicators, mute/deafen states, self-mute/deafen controls, volume sliders
- **Video call:** Grid/spotlight layout for video streams, screen share view, PiP support if present
- **Screen sharing:** Source picker (screen/window), preview, quality settings
- **User profile:** Avatar, status, about me, mutual servers/friends
- **Settings:** All settings pages from the original client (account, privacy, appearance/theme, voice & video, keybinds, notifications, etc.)
- **Notifications:** Toast notifications, desktop notifications, sound alerts
- **Context menus:** Right-click menus throughout the app
- **Modals/dialogs:** Server creation, channel creation, invite links, confirmations, etc.
- **Presence & status:** Online/idle/DND/offline indicators, custom status

### Theming
- Extract all colors, fonts, spacing, border radii, and shadow values from the QML stylesheets/theme files
- Implement as Tailwind theme tokens and/or CSS custom properties
- Support dark mode (and light mode if the original client has it)
- Ensure smooth transitions and animations matching the original

---

## Step 4 — Wire Up Real-Time Data

1. **Messages:** Incoming messages should arrive via Tauri events from the C++ backend → update React state → render in chat
2. **Presence:** User online/offline/status changes pushed as events
3. **Voice state:** Who's in a voice channel, speaking indicators, mute states — all streamed as events
4. **Typing indicators:** Emit on keypress, receive from others via events
5. **Notifications:** Backend pushes notification events → frontend renders toasts and updates badges
6. **Media streams:**
   - Audio: Routed through the C++ backend's libwebrtc pipeline. The frontend should display voice activity UI (speaking indicators, volume meters) based on audio level events from Rust.
   - Video/Screen share: Determine from the existing implementation how frames are delivered. Render in the webview using `<video>` elements connected to a local media stream, or `<canvas>` if frames are delivered as raw data.

---

## Step 5 — libwebrtc Integration

1. **Do not reimplement media logic in the webview.** The C++ backend owns the libwebrtc stack.
2. The Tauri Rust layer should:
   - Forward call signaling (offer/answer/ICE candidates) between frontend UI actions and the C++ backend
   - Relay audio/video device enumeration and selection
   - Provide call state updates (ringing, connected, disconnected, failed) as Tauri events
   - Stream audio level data for voice activity UI
3. For **video rendering**, evaluate and implement the best approach based on the existing architecture:
   - **Option A:** C++ backend sends frames via a local WebSocket as MJPEG or similar → frontend renders in `<img>` or `<canvas>`
   - **Option B:** C++ backend writes to shared memory → Rust reads and forwards to frontend
   - **Option C:** If the architecture supports it, use WebRTC directly in the webview with the C++ backend acting as an SFU/relay
   - Match whatever the Qt client currently does in terms of frame delivery, and adapt it for the webview
4. For **screen sharing source selection**, use Tauri's native dialog capabilities or a custom Rust command that enumerates available screens/windows via platform APIs and returns the list to the frontend for the user to pick.

---

## Step 6 — Platform & Polish

1. **Cross-platform:** Test on Windows, macOS, and Linux. Handle webview differences (WebView2, WKWebView, WebKitGTK).
2. **Performance:**
   - Use `virtual scrolling` for message lists and member lists
   - Lazy-load images and embeds
   - Debounce/throttle high-frequency events (typing indicators, audio levels)
   - Profile and optimize React renders — avoid unnecessary re-renders in the chat and voice UI
3. **Keyboard shortcuts:** Replicate all keybinds from the original client
4. **System tray:** Minimize to tray, notification badge on tray icon
5. **Auto-update:** Configure Tauri's built-in updater if applicable
6. **Error handling:** Graceful handling of backend disconnects, failed calls, network issues. Show appropriate UI feedback.
7. **Accessibility:** Semantic HTML, ARIA labels, keyboard navigation, screen reader support

---

## Important Constraints

- **Do not modify the C++ backend** unless absolutely necessary for interop (e.g., adding a thin C-compatible wrapper). If changes are needed, document them clearly and keep them minimal.
- **Match the existing Qt6/QML client's appearance and UX as closely as possible.** This is a frontend rebuild, not a redesign.
- **Prioritize correctness over speed.** Get each feature working reliably before optimizing.
- **Commit incrementally** with clear messages describing what was added/changed.

---

## Deliverables

1. `ARCHITECTURE.md` — Full analysis of the existing codebase and integration plan
2. Working Tauri v2 project in `tauri-client/` that builds and runs
3. All screens and features from the Qt6/QML client replicated and wired to the C++ backend
4. Voice, video, and screen sharing functional via the existing libwebrtc integration
5. Cross-platform build configurations (Windows, macOS, Linux)
