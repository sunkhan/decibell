# Phase 3: Core UI — Design Spec

**Date:** 2026-03-19
**Status:** Draft
**Depends on:** Phase 2 (Rust networking layer — complete)

## Overview

Build the React frontend that consumes the Phase 2 Rust networking commands and events. The UI replicates the existing QML client's panel layout with modernized visual polish. No new Rust code — this phase is purely frontend (React + TypeScript + Tailwind CSS v4 + Zustand + React Router).

## Scope

### In Scope (Core 6)

1. **Login / Register** — Centered card form with login/register toggle
2. **Main layout** — QML-match 4-panel shell (DM sidebar, server bar, channels, chat, right panel)
3. **Server discovery** — Card grid modal for browsing/joining community servers
4. **Channel sidebar** — Text and voice channel list per server
5. **Text chat** — Message list with input, per-channel message history
6. **Friends / Members panel** — Context-aware right panel (friends on home, channel members on server)

### Out of Scope (Later Phases)

- DM conversations UI (chat with friends directly)
- Profile popup / context menus
- Voice channel joining (Phase 4)
- Screen sharing (Phase 5)
- Settings page implementation
- Notification badges / unread indicators
- Rich presence (idle, DND, invisible)

**Note:** Existing `voiceStore.ts` and `SettingsPage.tsx` remain as placeholders — Phase 3 does not modify them. The `/settings` route continues to render the existing placeholder page.

## Architecture: Feature Modules

Code is organized by feature. Each feature folder contains its components and its Tauri event listener hook. Cross-cutting concerns live in shared hooks.

```
src/
  features/
    auth/
      LoginPage.tsx
      useAuthEvents.ts
    servers/
      ServerBar.tsx
      ServerDiscoveryModal.tsx
      useServerEvents.ts
    channels/
      ChannelSidebar.tsx
      useChannelEvents.ts
    chat/
      ChatPanel.tsx
      MessageBubble.tsx
      useChatEvents.ts
    friends/
      FriendsList.tsx
      MembersList.tsx
      FriendActionButton.tsx
      useFriendsEvents.ts
  layouts/
    AppLayout.tsx
    MainLayout.tsx
    DmSidebar.tsx
  hooks/
    useConnectionEvents.ts
    usePresenceEvents.ts
  stores/
    authStore.ts
    chatStore.ts
    friendsStore.ts
    uiStore.ts
  types/
    index.ts
```

## Event Listener Architecture

Each feature has a hook that uses Tauri's `listen()` API to subscribe to backend events and dispatch to Zustand stores. Hooks mount/unmount with their UI components.

### Feature Hooks

| Hook | Mounts on | Events |
|------|-----------|--------|
| `useAuthEvents` | `LoginPage` | `login_succeeded`, `login_failed`, `register_responded` |
| `useServerEvents` | `MainLayout` | `server_list_received`, `community_auth_responded` |
| `useChannelEvents` | `ChannelSidebar` | `join_channel_responded` |
| `useChatEvents` | `ChatPanel` | `message_received` |
| `useFriendsEvents` | `MainLayout` | `friend_list_received`, `friend_action_responded` |

### Cross-Cutting Hooks (mount on MainLayout)

| Hook | Events | Purpose |
|------|--------|---------|
| `useConnectionEvents` | `connection_lost`, `connection_restored`, `logged_out` | Connection status banner, forced logout |
| `usePresenceEvents` | `user_list_updated` | Online user lists for both members and friends panels |

### Hook Pattern

```typescript
// Example: features/chat/useChatEvents.ts
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";

export function useChatEvents() {
  useEffect(() => {
    const unlisten = listen("message_received", (event) => {
      useChatStore.getState().addMessage({
        sender: event.payload.sender,
        content: event.payload.content,
        timestamp: event.payload.timestamp,
        channelId: event.payload.context,
      });
    });
    return () => { unlisten.then((fn) => fn()); };
  }, []);
}
```

## Store Design

### authStore (extended)

```typescript
interface AuthState {
  username: string | null;
  isAuthenticated: boolean;
  isLoggingIn: boolean;
  loginError: string | null;
  isRegistering: boolean;
  registerResult: { success: boolean; message: string } | null;
  login: (username: string) => void;
  logout: () => void;
  setLoggingIn: (v: boolean) => void;
  setLoginError: (msg: string | null) => void;
  setRegistering: (v: boolean) => void;
  setRegisterResult: (result: { success: boolean; message: string } | null) => void;
}
```

### chatStore (replaces existing flat arrays with keyed structures)

```typescript
interface ChatState {
  servers: CommunityServer[];
  activeServerId: string | null;
  activeChannelId: string | null;
  channelsByServer: Record<string, Channel[]>;
  messagesByChannel: Record<string, Message[]>;
  channelMembers: Record<string, string[]>;
  onlineUsers: string[];
  connectedServers: Set<string>;
  setServers: (servers: CommunityServer[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: Channel[]) => void;
  addMessage: (message: Message) => void;
  setChannelMembers: (channelId: string, members: string[]) => void;
  setOnlineUsers: (users: string[]) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
}
```

### friendsStore (new)

```typescript
interface FriendsState {
  friends: FriendInfo[];
  isLoading: boolean;
  setFriends: (friends: FriendInfo[]) => void;
  setLoading: (v: boolean) => void;
  updateFriend: (username: string, updates: Partial<FriendInfo>) => void;
  removeFriend: (username: string) => void;
}
```

### uiStore (extended)

```typescript
interface UiState {
  sidebarCollapsed: boolean;
  activeModal: string | null;
  connectionStatus: "connected" | "reconnecting" | "disconnected";
  activeView: "home" | "server";
  toggleSidebar: () => void;
  openModal: (modalId: string) => void;
  closeModal: () => void;
  setConnectionStatus: (status: "connected" | "reconnecting" | "disconnected") => void;
  setActiveView: (view: "home" | "server") => void;
}
```

## Routing & View Flow

### Routes

| Path | Component | Auth Required |
|------|-----------|---------------|
| `/login` | `LoginPage` | No |
| `/` | `MainLayout` | Yes |
| `/settings` | `SettingsPage` | Yes |

### View Switching (inside MainLayout, via Zustand)

- `activeServerId === null` → **Home view**: right panel shows `FriendsList`
- `activeServerId !== null` → **Server view**: right panel shows `MembersList`

Clicking a server tab sets `activeServerId`; clicking the home icon clears it.

### Login Flow

1. User submits form → `invoke("login", { username, password })` → button disabled + spinner
2. `login_succeeded` event with `{ username }` → `authStore.login(username)` → navigate to `/`
3. `login_failed` → show error below button, re-enable form
4. On register: same pattern with `invoke("register", ...)` and `register_responded` event

### Server Connection Flow

1. User clicks server card in discovery → `invoke("connect_to_community", { serverId, host, port })` → loading state
2. `community_auth_responded` → channels populate sidebar → first text channel auto-selected
3. `join_channel_responded` → members list populates, chat ready for messages

## Component Specifications

### LoginPage

- Centered card (max-width ~400px) on `bg-bg-primary` background
- Two modes: Login (username + password) and Register (username + password + email)
- Toggle between modes via text link at bottom ("Need an account? Register" / "Already have an account? Log In")
- Blue accent submit button, `disabled` with spinner during async operation
- Error message in `text-error` below button on failure
- No layout shell (no sidebar, no server bar)

### DmSidebar (72px wide)

- Far-left vertical strip, `bg-bg-primary`, right border
- Home icon at top — returns to home view (clears `activeServerId`)
- Below: list of recent DM conversation avatars (rounded-square, first letter of username, colored background)
- Clicking opens that DM (future phase — for now, just visual)
- No "Add Server" button (that's in ServerBar)

### ServerBar (64px tall)

- Horizontal bar across top, below DmSidebar's top area
- Connected server tabs as pills: server name, highlighted when active
- Clicking switches `activeServerId` and `activeView`
- Rightmost position: `+` button → opens `ServerDiscoveryModal`
- Empty state: just the `+` button

### ChannelSidebar (240px wide)

- Left panel below ServerBar
- **Home view**: "Direct Messages" header (placeholder for future DM list)
- **Server view**: Server name at top, then categorized lists:
  - "TEXT CHANNELS" section — channel names prefixed with `#`, active channel highlighted in accent
  - "VOICE CHANNELS" section — channel names prefixed with speaker icon, show connected user count (not joinable in Phase 3)
- Clicking a text channel → `invoke("join_channel", { serverId, channelId })` → updates `activeChannelId`

### ChatPanel (flex: 1, center area)

- **No channel selected**: centered muted text "Select a channel to start chatting"
- **Channel selected**:
  - Top: channel name header (`# channel-name`) with border-bottom
  - Center: scrollable message list, auto-scrolls to bottom on new messages
  - Bottom: input bar with placeholder "Message #channel-name", send on Enter

### MessageBubble

- Row layout: rounded-square avatar (first letter, colored) → username (accent color, bold) + timestamp (muted, small) → message content below
- No message editing/deletion in Phase 3

### FriendsList (right panel, 280px, home view)

- Sectioned list: "ONLINE", "OFFLINE", "PENDING", "BLOCKED" with count headers
- Each friend: rounded-square avatar, username, status dot (green for online, gray for offline)
- Pending incoming: Accept / Reject buttons
- Pending outgoing: "Pending" label
- Top: "Add Friend" button → simple input to type username + send friend request
- Search/filter input at top

### MembersList (right panel, 280px, server view)

- "ONLINE — N" header
- List of usernames with rounded-square avatar and green status dot
- Populated from `channelMembers[activeChannelId]`

### ServerDiscoveryModal

- Overlay modal with backdrop
- Search bar at top
- 2-column card grid:
  - Each card: rounded-square server icon (first letter, colored), name, description, member count, online status
  - Click to connect (`invoke("connect_to_community", ...)`)
- "Add by IP" card with dashed border at bottom — input for host:port direct connection
- Closes on successful connection or explicit close
- Standard scrollable container for server list (virtual scrolling deferred unless list size becomes a performance issue)

## Loading & Error States

All async operations use **explicit** feedback (not optimistic updates):

| Operation | Loading State | Error State |
|-----------|--------------|-------------|
| Login | Button disabled + spinner | Error text below form |
| Register | Button disabled + spinner | Error/success message below form |
| Server list fetch | Skeleton cards in modal | "Failed to load servers" message |
| Server connect | Loading indicator on server card | Toast/inline error |
| Channel join | Channel name shows spinner | Error text in chat panel |
| Send message | Input disabled briefly | Error text above input |
| Friend action | Button disabled + spinner | Inline error next to button |

### Connection Banner

When `connectionStatus` is `"reconnecting"`, a top banner appears across the full width: "Connection lost. Reconnecting..." in warning color. Dismisses automatically when `connection_restored` fires.

## Styling

**Framework:** Tailwind CSS v4 with existing theme tokens from `globals.css`.

**Color palette** (existing):
- `bg-primary: #0C0D0F` — darkest background (sidebars, main bg)
- `bg-secondary: #1A1B1E` — chat area background
- `bg-tertiary: #242528` — input fields, hover states
- `border: #2D3245` — all borders/dividers
- `accent: #2CA3E8` — interactive elements, links, active states
- `text-primary: #DCDDDE` — main text
- `text-muted` — secondary text, timestamps

**Modern touches (vs QML original):**
- **Rounded-square avatars** (`rounded-lg` or `rounded-xl`) — not circular, not square
- **Status dots** with 2px border matching parent background (cutout effect)
- **Hover states** on all interactive elements (`hover:bg-white/5`)
- **Smooth transitions** (`transition-colors duration-150`)
- **Open Sans font** (already configured in globals.css)
- **Thin dark scrollbars** (already configured in globals.css)
- **Focus rings** for keyboard navigation (accent color outline)
- **Loading spinners** — simple animated SVG in accent color

**No component library** — all Tailwind utility classes.

## Tauri Commands Used (from Phase 2)

| Command | Parameters | Used By |
|---------|-----------|---------|
| `login` | `username, password` | LoginPage |
| `register` | `username, email, password` | LoginPage |
| `logout` | — | MainLayout (logout action) |
| `request_server_list` | — | ServerDiscoveryModal |
| `connect_to_community` | `serverId: string, host: string, port: number` | ServerDiscoveryModal |
| `disconnect_from_community` | `serverId` | ServerBar (disconnect button on hover) |
| `join_channel` | `serverId, channelId` | ChannelSidebar |
| `send_channel_message` | `serverId, channelId, message` | ChatPanel |
| `request_friend_list` | — | MainLayout (on mount) |
| `send_friend_action` | `action: i32, targetUsername` | FriendsList |
| `send_private_message` | `recipient, message` | (wired but not fully used until DMs phase) |

**`send_friend_action` enum values** (maps to protobuf `FriendActionType`):

| Value | Action |
|-------|--------|
| `0` | ADD |
| `1` | REMOVE |
| `2` | BLOCK |
| `3` | ACCEPT |
| `4` | REJECT |

## Tauri Events Consumed

| Event | Payload | Updates |
|-------|---------|---------|
| `login_succeeded` | `{ username }` | authStore |
| `login_failed` | `{ message }` | authStore |
| `register_responded` | `{ success, message }` | authStore |
| `logged_out` | — | authStore, navigate to /login |
| `connection_lost` | `{ serverType, serverId? }` | uiStore connectionStatus |
| `connection_restored` | `{ serverType, serverId? }` | uiStore connectionStatus |
| `server_list_received` | `{ servers: [...] }` | chatStore |
| `community_auth_responded` | `{ serverId, success, message, channels }` | chatStore |
| `message_received` | `{ context, sender, content, timestamp }` | chatStore |
| `user_list_updated` | `{ onlineUsers: [...] }` | chatStore (global online list — see note below) |
| `join_channel_responded` | `{ serverId, success, channelId, activeUsers }` | chatStore |
| `friend_list_received` | `{ friends: [...] }` | friendsStore |
| `friend_action_responded` | `{ success, message }` | friendsStore (refresh list) |

### Note: Online User Data Sources

Channel members come from two sources:
- **`join_channel_responded`** provides `activeUsers` for the joined channel — used to populate `channelMembers[channelId]`
- **`user_list_updated`** is a global online user list from the central server (via `PresenceUpdate`). It has no channel/server context. Store this as `onlineUsers: string[]` in chatStore and use it for the FriendsList online status cross-referencing — not for per-channel member lists.

The MembersList panel shows `channelMembers[activeChannelId]` (from `join_channel_responded`), not the global online list.

### Note: Server ID Type Conversion

The Rust `ServerInfo.id` is `i32` (from protobuf `int32`). The frontend `CommunityServer.id` is `string`. When receiving `server_list_received`, convert `id` to string: `String(server.id)`. All frontend code uses string IDs consistently — the `connect_to_community` command also takes `server_id: String`.

## Presence Model

Matches backend capabilities only:

| Status | Dot Color | Source |
|--------|-----------|--------|
| Online | `#43B581` (success green) | `FriendInfo.status = "online"` |
| Offline | `#4f6a86` (muted gray) | `FriendInfo.status = "offline"` |
| Pending (incoming) | — (no dot, shows Accept/Reject) | `FriendInfo.status = "pending_incoming"` |
| Pending (outgoing) | — (no dot, shows "Pending" label) | `FriendInfo.status = "pending_outgoing"` |
| Blocked | — (no dot, shown in Blocked section) | `FriendInfo.status = "blocked"` |

Richer presence (idle, DND, invisible) deferred to a future phase when the backend protobuf is extended.
