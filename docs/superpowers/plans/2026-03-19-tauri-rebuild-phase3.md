# Phase 3: Core UI Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the React frontend that consumes Phase 2 Rust networking commands and events — login, layout, server discovery, channels, chat, and friends/members panel.

**Architecture:** Feature-module organization with co-located event listener hooks. Each feature owns its components and Tauri event hook. Zustand stores hold all state. Cross-cutting concerns (connection status, presence) live in shared hooks mounted at the layout level.

**Tech Stack:** React 18, TypeScript, Tailwind CSS v4, Zustand 5, React Router 7, Tauri v2 API, Vitest

**Spec:** `docs/superpowers/specs/2026-03-19-tauri-rebuild-phase3-design.md`

---

## File Map

### New Files

| File | Responsibility |
|------|---------------|
| `src/features/auth/LoginPage.tsx` | Login/register form with centered card layout |
| `src/features/auth/useAuthEvents.ts` | Listens: `login_succeeded`, `login_failed`, `register_responded` |
| `src/features/servers/ServerBar.tsx` | Horizontal server tabs + add server button |
| `src/features/servers/ServerDiscoveryModal.tsx` | Card grid modal for browsing/joining servers |
| `src/features/servers/useServerEvents.ts` | Listens: `server_list_received`, `community_auth_responded` |
| `src/features/channels/ChannelSidebar.tsx` | Channel list per server, DM placeholder on home |
| `src/features/channels/useChannelEvents.ts` | Listens: `join_channel_responded` |
| `src/features/chat/ChatPanel.tsx` | Message list + input bar |
| `src/features/chat/MessageBubble.tsx` | Single message row component |
| `src/features/chat/useChatEvents.ts` | Listens: `message_received` |
| `src/features/friends/FriendsList.tsx` | Sectioned friend list (online/offline/pending/blocked) |
| `src/features/friends/MembersList.tsx` | Channel member list for server view |
| `src/features/friends/FriendActionButton.tsx` | Add/accept/reject/remove/block friend button |
| `src/features/friends/useFriendsEvents.ts` | Listens: `friend_list_received`, `friend_action_responded` |
| `src/layouts/MainLayout.tsx` | 4-panel QML-match layout shell |
| `src/layouts/DmSidebar.tsx` | Far-left 72px DM/home sidebar |
| `src/hooks/useConnectionEvents.ts` | Listens: `connection_lost`, `connection_restored`, `logged_out` |
| `src/hooks/usePresenceEvents.ts` | Listens: `user_list_updated` |
| `src/stores/friendsStore.ts` | Friends state (list, loading, actions) |

### Modified Files

| File | Changes |
|------|---------|
| `src/stores/authStore.ts` | Add loading/error state, change `login()` to take only username |
| `src/stores/chatStore.ts` | Replace flat arrays with keyed structures, add onlineUsers |
| `src/stores/uiStore.ts` | Add connectionStatus, activeView |
| `src/types/index.ts` | Remove unused `idle`/`dnd` from User status |
| `src/App.tsx` | Update routes to use new LoginPage and MainLayout |
| `src/layouts/AppLayout.tsx` | Keep as-is (already correct) |

### Deleted Files

| File | Reason |
|------|--------|
| `src/pages/LoginPage.tsx` | Replaced by `src/features/auth/LoginPage.tsx` |
| `src/pages/HomePage.tsx` | Replaced by `MainLayout` as the home route |

### Unchanged Files

| File | Reason |
|------|--------|
| `src/pages/SettingsPage.tsx` | Placeholder, untouched in Phase 3 |
| `src/stores/voiceStore.ts` | Phase 4 concern, untouched |
| `src/styles/globals.css` | Already has all needed theme tokens |
| `src/main.tsx` | Already correct |

---

## Task 1: Set Up Test Infrastructure

**Files:**
- Modify: `tauri-client/package.json`
- Create: `tauri-client/src/test-setup.ts`

This task adds Vitest so we can test stores and hooks.

- [ ] **Step 1: Install Vitest and testing dependencies**

```bash
cd tauri-client && npm install -D vitest @testing-library/react @testing-library/jest-dom jsdom
```

- [ ] **Step 2: Create test setup file**

Create `tauri-client/src/test-setup.ts`:

```typescript
import "@testing-library/jest-dom/vitest";
```

- [ ] **Step 3: Add Vitest config to vite.config.ts**

Add the `test` block to the existing Vite config in `tauri-client/vite.config.ts`:

```typescript
/// <reference types="vitest/config" />
// ... existing imports ...

export default defineConfig(async () => ({
  // ... existing config ...
  test: {
    environment: "jsdom",
    setupFiles: ["./src/test-setup.ts"],
    globals: true,
  },
}));
```

- [ ] **Step 4: Add test script to package.json**

Add to the `"scripts"` section:

```json
"test": "vitest run",
"test:watch": "vitest"
```

- [ ] **Step 5: Verify test infrastructure works**

Run: `cd tauri-client && npm test`
Expected: "No test files found" (no tests yet, but no config errors)

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "chore: add Vitest test infrastructure for Phase 3"
```

---

## Task 2: Update Types and Stores

**Files:**
- Modify: `tauri-client/src/types/index.ts`
- Modify: `tauri-client/src/stores/authStore.ts`
- Modify: `tauri-client/src/stores/chatStore.ts`
- Modify: `tauri-client/src/stores/uiStore.ts`
- Create: `tauri-client/src/stores/friendsStore.ts`
- Create: `tauri-client/src/stores/__tests__/authStore.test.ts`
- Create: `tauri-client/src/stores/__tests__/chatStore.test.ts`
- Create: `tauri-client/src/stores/__tests__/friendsStore.test.ts`
- Create: `tauri-client/src/stores/__tests__/uiStore.test.ts`

### Step 2a: Update types

- [ ] **Step 1: Update `src/types/index.ts`**

Change `User.status` to match backend capabilities (remove `idle` and `dnd`):

```typescript
export interface User {
  username: string;
  status: "online" | "offline";
}
```

All other types remain unchanged — `FriendInfo`, `CommunityServer`, `Channel`, `Message`, `VoiceParticipant`, `StreamInfo` are already correct.

### Step 2b: Rewrite authStore with tests

- [ ] **Step 2: Write authStore tests**

Create `tauri-client/src/stores/__tests__/authStore.test.ts`:

```typescript
import { describe, it, expect, beforeEach } from "vitest";
import { useAuthStore } from "../authStore";

describe("authStore", () => {
  beforeEach(() => {
    useAuthStore.setState({
      username: null,
      isAuthenticated: false,
      isLoggingIn: false,
      loginError: null,
      isRegistering: false,
      registerResult: null,
    });
  });

  it("login sets username and isAuthenticated", () => {
    useAuthStore.getState().login("alice");
    const s = useAuthStore.getState();
    expect(s.username).toBe("alice");
    expect(s.isAuthenticated).toBe(true);
  });

  it("logout clears auth state", () => {
    useAuthStore.getState().login("alice");
    useAuthStore.getState().logout();
    const s = useAuthStore.getState();
    expect(s.username).toBeNull();
    expect(s.isAuthenticated).toBe(false);
  });

  it("setLoggingIn toggles loading state", () => {
    useAuthStore.getState().setLoggingIn(true);
    expect(useAuthStore.getState().isLoggingIn).toBe(true);
    useAuthStore.getState().setLoggingIn(false);
    expect(useAuthStore.getState().isLoggingIn).toBe(false);
  });

  it("setLoginError sets and clears error", () => {
    useAuthStore.getState().setLoginError("bad password");
    expect(useAuthStore.getState().loginError).toBe("bad password");
    useAuthStore.getState().setLoginError(null);
    expect(useAuthStore.getState().loginError).toBeNull();
  });

  it("setRegisterResult stores result", () => {
    useAuthStore.getState().setRegisterResult({ success: true, message: "ok" });
    expect(useAuthStore.getState().registerResult).toEqual({ success: true, message: "ok" });
  });
});
```

- [ ] **Step 3: Run test to verify it fails**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/authStore.test.ts`
Expected: FAIL (current authStore doesn't have new fields)

- [ ] **Step 4: Rewrite `src/stores/authStore.ts`**

```typescript
import { create } from "zustand";

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

export const useAuthStore = create<AuthState>((set) => ({
  username: null,
  isAuthenticated: false,
  isLoggingIn: false,
  loginError: null,
  isRegistering: false,
  registerResult: null,
  login: (username) =>
    set({ username, isAuthenticated: true, isLoggingIn: false, loginError: null }),
  logout: () =>
    set({
      username: null,
      isAuthenticated: false,
      isLoggingIn: false,
      loginError: null,
      registerResult: null,
    }),
  setLoggingIn: (v) => set({ isLoggingIn: v, loginError: null }),
  setLoginError: (msg) => set({ loginError: msg, isLoggingIn: false }),
  setRegistering: (v) => set({ isRegistering: v, registerResult: null }),
  setRegisterResult: (result) => set({ registerResult: result, isRegistering: false }),
}));
```

- [ ] **Step 5: Run authStore tests**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/authStore.test.ts`
Expected: All 5 tests PASS

### Step 2c: Rewrite chatStore with tests

- [ ] **Step 6: Write chatStore tests**

Create `tauri-client/src/stores/__tests__/chatStore.test.ts`:

```typescript
import { describe, it, expect, beforeEach } from "vitest";
import { useChatStore } from "../chatStore";

describe("chatStore", () => {
  beforeEach(() => {
    useChatStore.setState({
      servers: [],
      activeServerId: null,
      activeChannelId: null,
      channelsByServer: {},
      messagesByChannel: {},
      channelMembers: {},
      onlineUsers: [],
      connectedServers: new Set(),
    });
  });

  it("setServers replaces server list", () => {
    const servers = [
      { id: "1", name: "Test", description: "desc", hostIp: "127.0.0.1", port: 9090, memberCount: 5 },
    ];
    useChatStore.getState().setServers(servers);
    expect(useChatStore.getState().servers).toEqual(servers);
  });

  it("setActiveServer and setActiveChannel update selection", () => {
    useChatStore.getState().setActiveServer("1");
    expect(useChatStore.getState().activeServerId).toBe("1");
    useChatStore.getState().setActiveChannel("ch1");
    expect(useChatStore.getState().activeChannelId).toBe("ch1");
  });

  it("setChannelsForServer stores channels keyed by server", () => {
    const channels = [{ id: "ch1", name: "general", type: "text" as const }];
    useChatStore.getState().setChannelsForServer("s1", channels);
    expect(useChatStore.getState().channelsByServer["s1"]).toEqual(channels);
  });

  it("addMessage appends to channel message list", () => {
    const msg = { sender: "alice", content: "hi", timestamp: "123", channelId: "ch1" };
    useChatStore.getState().addMessage(msg);
    expect(useChatStore.getState().messagesByChannel["ch1"]).toEqual([msg]);
    useChatStore.getState().addMessage({ ...msg, content: "hello" });
    expect(useChatStore.getState().messagesByChannel["ch1"]).toHaveLength(2);
  });

  it("setChannelMembers stores member list per channel", () => {
    useChatStore.getState().setChannelMembers("ch1", ["alice", "bob"]);
    expect(useChatStore.getState().channelMembers["ch1"]).toEqual(["alice", "bob"]);
  });

  it("setOnlineUsers replaces global online list", () => {
    useChatStore.getState().setOnlineUsers(["alice", "bob"]);
    expect(useChatStore.getState().onlineUsers).toEqual(["alice", "bob"]);
  });

  it("addConnectedServer and removeConnectedServer manage set", () => {
    useChatStore.getState().addConnectedServer("s1");
    expect(useChatStore.getState().connectedServers.has("s1")).toBe(true);
    useChatStore.getState().removeConnectedServer("s1");
    expect(useChatStore.getState().connectedServers.has("s1")).toBe(false);
  });
});
```

- [ ] **Step 7: Run test to verify it fails**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/chatStore.test.ts`
Expected: FAIL

- [ ] **Step 8: Rewrite `src/stores/chatStore.ts`**

```typescript
import { create } from "zustand";
import type { CommunityServer, Channel, Message } from "../types";

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

export const useChatStore = create<ChatState>((set) => ({
  servers: [],
  activeServerId: null,
  activeChannelId: null,
  channelsByServer: {},
  messagesByChannel: {},
  channelMembers: {},
  onlineUsers: [],
  connectedServers: new Set(),
  setServers: (servers) => set({ servers }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),
  setChannelsForServer: (serverId, channels) =>
    set((state) => ({
      channelsByServer: { ...state.channelsByServer, [serverId]: channels },
    })),
  addMessage: (message) =>
    set((state) => ({
      messagesByChannel: {
        ...state.messagesByChannel,
        [message.channelId]: [
          ...(state.messagesByChannel[message.channelId] ?? []),
          message,
        ],
      },
    })),
  setChannelMembers: (channelId, members) =>
    set((state) => ({
      channelMembers: { ...state.channelMembers, [channelId]: members },
    })),
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  addConnectedServer: (serverId) =>
    set((state) => ({
      connectedServers: new Set([...state.connectedServers, serverId]),
    })),
  removeConnectedServer: (serverId) =>
    set((state) => {
      const next = new Set(state.connectedServers);
      next.delete(serverId);
      return { connectedServers: next };
    }),
}));
```

- [ ] **Step 9: Run chatStore tests**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/chatStore.test.ts`
Expected: All 7 tests PASS

### Step 2d: Create friendsStore with tests

- [ ] **Step 10: Write friendsStore tests**

Create `tauri-client/src/stores/__tests__/friendsStore.test.ts`:

```typescript
import { describe, it, expect, beforeEach } from "vitest";
import { useFriendsStore } from "../friendsStore";

describe("friendsStore", () => {
  beforeEach(() => {
    useFriendsStore.setState({ friends: [], isLoading: false });
  });

  it("setFriends replaces friend list", () => {
    const friends = [{ username: "alice", status: "online" as const }];
    useFriendsStore.getState().setFriends(friends);
    expect(useFriendsStore.getState().friends).toEqual(friends);
  });

  it("updateFriend merges partial update", () => {
    useFriendsStore.getState().setFriends([{ username: "alice", status: "online" }]);
    useFriendsStore.getState().updateFriend("alice", { status: "offline" });
    expect(useFriendsStore.getState().friends[0].status).toBe("offline");
  });

  it("removeFriend filters out by username", () => {
    useFriendsStore.getState().setFriends([
      { username: "alice", status: "online" },
      { username: "bob", status: "offline" },
    ]);
    useFriendsStore.getState().removeFriend("alice");
    expect(useFriendsStore.getState().friends).toHaveLength(1);
    expect(useFriendsStore.getState().friends[0].username).toBe("bob");
  });

  it("setLoading toggles loading state", () => {
    useFriendsStore.getState().setLoading(true);
    expect(useFriendsStore.getState().isLoading).toBe(true);
  });
});
```

- [ ] **Step 11: Run test to verify it fails**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/friendsStore.test.ts`
Expected: FAIL (file doesn't exist)

- [ ] **Step 12: Create `src/stores/friendsStore.ts`**

```typescript
import { create } from "zustand";
import type { FriendInfo } from "../types";

interface FriendsState {
  friends: FriendInfo[];
  isLoading: boolean;
  setFriends: (friends: FriendInfo[]) => void;
  setLoading: (v: boolean) => void;
  updateFriend: (username: string, updates: Partial<FriendInfo>) => void;
  removeFriend: (username: string) => void;
}

export const useFriendsStore = create<FriendsState>((set) => ({
  friends: [],
  isLoading: false,
  setFriends: (friends) => set({ friends }),
  setLoading: (v) => set({ isLoading: v }),
  updateFriend: (username, updates) =>
    set((state) => ({
      friends: state.friends.map((f) =>
        f.username === username ? { ...f, ...updates } : f
      ),
    })),
  removeFriend: (username) =>
    set((state) => ({
      friends: state.friends.filter((f) => f.username !== username),
    })),
}));
```

- [ ] **Step 13: Run friendsStore tests**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/friendsStore.test.ts`
Expected: All 4 tests PASS

### Step 2e: Extend uiStore with tests

- [ ] **Step 14: Write uiStore tests**

Create `tauri-client/src/stores/__tests__/uiStore.test.ts`:

```typescript
import { describe, it, expect, beforeEach } from "vitest";
import { useUiStore } from "../uiStore";

describe("uiStore", () => {
  beforeEach(() => {
    useUiStore.setState({
      sidebarCollapsed: false,
      activeModal: null,
      connectionStatus: "connected",
      activeView: "home",
    });
  });

  it("setConnectionStatus updates status", () => {
    useUiStore.getState().setConnectionStatus("reconnecting");
    expect(useUiStore.getState().connectionStatus).toBe("reconnecting");
  });

  it("setActiveView switches view", () => {
    useUiStore.getState().setActiveView("server");
    expect(useUiStore.getState().activeView).toBe("server");
  });

  it("openModal and closeModal manage modal state", () => {
    useUiStore.getState().openModal("server-discovery");
    expect(useUiStore.getState().activeModal).toBe("server-discovery");
    useUiStore.getState().closeModal();
    expect(useUiStore.getState().activeModal).toBeNull();
  });
});
```

- [ ] **Step 15: Run test to verify it fails**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/uiStore.test.ts`
Expected: FAIL (missing new fields/actions)

- [ ] **Step 16: Update `src/stores/uiStore.ts`**

```typescript
import { create } from "zustand";

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

export const useUiStore = create<UiState>((set) => ({
  sidebarCollapsed: false,
  activeModal: null,
  connectionStatus: "connected",
  activeView: "home",
  toggleSidebar: () =>
    set((state) => ({ sidebarCollapsed: !state.sidebarCollapsed })),
  openModal: (modalId) => set({ activeModal: modalId }),
  closeModal: () => set({ activeModal: null }),
  setConnectionStatus: (status) => set({ connectionStatus: status }),
  setActiveView: (view) => set({ activeView: view }),
}));
```

- [ ] **Step 17: Run all store tests**

Run: `cd tauri-client && npx vitest run src/stores/__tests__/`
Expected: All tests PASS (5 + 7 + 4 + 3 = 19 tests)

- [ ] **Step 18: Commit**

```bash
git add -A && git commit -m "feat: update stores and types for Phase 3 Core UI"
```

---

## Task 3: Create Shared Hooks (Event Listeners)

**Files:**
- Create: `tauri-client/src/hooks/useConnectionEvents.ts`
- Create: `tauri-client/src/hooks/usePresenceEvents.ts`

These are cross-cutting event listeners that mount on MainLayout.

- [ ] **Step 1: Create `src/hooks/useConnectionEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useUiStore } from "../stores/uiStore";
import { useChatStore } from "../stores/chatStore";
import { useAuthStore } from "../stores/authStore";
import { useNavigate } from "react-router-dom";

export function useConnectionEvents() {
  const navigate = useNavigate();

  useEffect(() => {
    const unlistenLost = listen<{ serverType: string; serverId?: string }>(
      "connection_lost",
      (event) => {
        const { serverType, serverId } = event.payload;
        if (serverType === "central") {
          useUiStore.getState().setConnectionStatus("reconnecting");
        } else if (serverType === "community" && serverId) {
          useChatStore.getState().removeConnectedServer(serverId);
        }
      }
    );

    const unlistenRestored = listen<{ serverType: string; serverId?: string }>(
      "connection_restored",
      (event) => {
        const { serverType, serverId } = event.payload;
        if (serverType === "central") {
          useUiStore.getState().setConnectionStatus("connected");
        } else if (serverType === "community" && serverId) {
          useChatStore.getState().addConnectedServer(serverId);
        }
      }
    );

    const unlistenLogout = listen("logged_out", () => {
      useAuthStore.getState().logout();
      navigate("/login");
    });

    return () => {
      unlistenLost.then((fn) => fn());
      unlistenRestored.then((fn) => fn());
      unlistenLogout.then((fn) => fn());
    };
  }, [navigate]);
}
```

- [ ] **Step 2: Create `src/hooks/usePresenceEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../stores/chatStore";

export function usePresenceEvents() {
  useEffect(() => {
    const unlisten = listen<{ onlineUsers: string[] }>(
      "user_list_updated",
      (event) => {
        useChatStore.getState().setOnlineUsers(event.payload.onlineUsers);
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 3: Verify TypeScript compiles**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors (or only pre-existing errors from unused files)

- [ ] **Step 4: Commit**

```bash
git add -A && git commit -m "feat: add cross-cutting event listener hooks"
```

---

## Task 4: Build Auth Feature (LoginPage + Events)

**Files:**
- Create: `tauri-client/src/features/auth/LoginPage.tsx`
- Create: `tauri-client/src/features/auth/useAuthEvents.ts`
- Modify: `tauri-client/src/App.tsx`
- Delete: `tauri-client/src/pages/LoginPage.tsx`
- Delete: `tauri-client/src/pages/HomePage.tsx`

- [ ] **Step 1: Create `src/features/auth/useAuthEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useAuthStore } from "../../stores/authStore";
import { useNavigate } from "react-router-dom";

export function useAuthEvents() {
  const navigate = useNavigate();

  useEffect(() => {
    const unlistenSuccess = listen<{ username: string }>(
      "login_succeeded",
      (event) => {
        useAuthStore.getState().login(event.payload.username);
        navigate("/");
      }
    );

    const unlistenFailed = listen<{ message: string }>(
      "login_failed",
      (event) => {
        useAuthStore.getState().setLoginError(event.payload.message);
      }
    );

    const unlistenRegister = listen<{ success: boolean; message: string }>(
      "register_responded",
      (event) => {
        useAuthStore.getState().setRegisterResult(event.payload);
      }
    );

    return () => {
      unlistenSuccess.then((fn) => fn());
      unlistenFailed.then((fn) => fn());
      unlistenRegister.then((fn) => fn());
    };
  }, [navigate]);
}
```

- [ ] **Step 2: Create `src/features/auth/LoginPage.tsx`**

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAuthStore } from "../../stores/authStore";
import { useAuthEvents } from "./useAuthEvents";

export default function LoginPage() {
  useAuthEvents();

  const [mode, setMode] = useState<"login" | "register">("login");
  const [username, setUsername] = useState("");
  const [password, setPassword] = useState("");
  const [email, setEmail] = useState("");

  const isLoggingIn = useAuthStore((s) => s.isLoggingIn);
  const loginError = useAuthStore((s) => s.loginError);
  const isRegistering = useAuthStore((s) => s.isRegistering);
  const registerResult = useAuthStore((s) => s.registerResult);

  const handleLogin = async () => {
    useAuthStore.getState().setLoggingIn(true);
    try {
      await invoke("login", { username, password });
    } catch (err) {
      useAuthStore.getState().setLoginError(String(err));
    }
  };

  const handleRegister = async () => {
    useAuthStore.getState().setRegistering(true);
    try {
      await invoke("register", { username, email, password });
    } catch (err) {
      useAuthStore.getState().setRegisterResult({
        success: false,
        message: String(err),
      });
    }
  };

  const handleSubmit = (e: React.FormEvent) => {
    e.preventDefault();
    if (mode === "login") handleLogin();
    else handleRegister();
  };

  const isLoading = mode === "login" ? isLoggingIn : isRegistering;

  return (
    <div className="flex h-screen w-screen items-center justify-center bg-bg-primary">
      <form
        onSubmit={handleSubmit}
        className="w-full max-w-[400px] rounded-xl bg-bg-secondary p-8 shadow-lg"
      >
        <h1 className="mb-1 text-center text-xl font-bold text-text-primary">
          {mode === "login" ? "Welcome back!" : "Create an account"}
        </h1>
        <p className="mb-6 text-center text-sm text-text-muted">
          {mode === "login"
            ? "We're so excited to see you again!"
            : "Join the conversation"}
        </p>

        <label className="mb-1 block text-xs font-semibold uppercase text-text-muted">
          Username
        </label>
        <input
          type="text"
          value={username}
          onChange={(e) => setUsername(e.target.value)}
          className="mb-3 w-full rounded-md border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          required
        />

        {mode === "register" && (
          <>
            <label className="mb-1 block text-xs font-semibold uppercase text-text-muted">
              Email
            </label>
            <input
              type="email"
              value={email}
              onChange={(e) => setEmail(e.target.value)}
              className="mb-3 w-full rounded-md border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none transition-colors focus:border-accent"
              required
            />
          </>
        )}

        <label className="mb-1 block text-xs font-semibold uppercase text-text-muted">
          Password
        </label>
        <input
          type="password"
          value={password}
          onChange={(e) => setPassword(e.target.value)}
          className="mb-4 w-full rounded-md border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none transition-colors focus:border-accent"
          required
        />

        <button
          type="submit"
          disabled={isLoading}
          className="flex w-full items-center justify-center rounded-md bg-accent py-2.5 text-sm font-semibold text-white transition-colors hover:bg-accent-hover active:bg-accent-pressed disabled:opacity-50"
        >
          {isLoading ? (
            <svg
              className="h-5 w-5 animate-spin"
              viewBox="0 0 24 24"
              fill="none"
            >
              <circle
                cx="12"
                cy="12"
                r="10"
                stroke="currentColor"
                strokeWidth="3"
                className="opacity-25"
              />
              <path
                d="M4 12a8 8 0 018-8"
                stroke="currentColor"
                strokeWidth="3"
                strokeLinecap="round"
                className="opacity-75"
              />
            </svg>
          ) : mode === "login" ? (
            "Log In"
          ) : (
            "Register"
          )}
        </button>

        {loginError && (
          <p className="mt-3 text-center text-sm text-error">{loginError}</p>
        )}
        {registerResult && (
          <p
            className={`mt-3 text-center text-sm ${
              registerResult.success ? "text-success" : "text-error"
            }`}
          >
            {registerResult.message}
          </p>
        )}

        <p className="mt-4 text-center text-sm text-text-muted">
          {mode === "login" ? (
            <>
              Need an account?{" "}
              <button
                type="button"
                onClick={() => setMode("register")}
                className="text-accent hover:underline"
              >
                Register
              </button>
            </>
          ) : (
            <>
              Already have an account?{" "}
              <button
                type="button"
                onClick={() => setMode("login")}
                className="text-accent hover:underline"
              >
                Log In
              </button>
            </>
          )}
        </p>
      </form>
    </div>
  );
}
```

- [ ] **Step 3: Delete old pages and update App.tsx**

Delete `src/pages/LoginPage.tsx` and `src/pages/HomePage.tsx`.

Update `src/App.tsx` — remove the old page imports, add the new LoginPage. The `"/"` route will point to `MainLayout` (built in Task 5), but for now use a temporary placeholder:

```tsx
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import AppLayout from "./layouts/AppLayout";
import LoginPage from "./features/auth/LoginPage";
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
                <div className="flex h-full w-full items-center justify-center text-text-muted">
                  Main layout coming next...
                </div>
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

- [ ] **Step 4: Verify app builds and login page renders**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

Run: `cd tauri-client && npm run dev` (verify in browser at localhost:1420)
Expected: Login page renders with username/password fields and Log In button. Toggling to Register shows email field.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: add LoginPage with auth events and form"
```

---

## Task 5: Build Main Layout Shell

**Files:**
- Create: `tauri-client/src/layouts/MainLayout.tsx`
- Create: `tauri-client/src/layouts/DmSidebar.tsx`
- Modify: `tauri-client/src/App.tsx`

- [ ] **Step 1: Create `src/layouts/DmSidebar.tsx`**

The far-left 72px vertical strip with home icon.

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useNavigate } from "react-router-dom";
import { useChatStore } from "../stores/chatStore";
import { useUiStore } from "../stores/uiStore";
import { useAuthStore } from "../stores/authStore";

export default function DmSidebar() {
  const navigate = useNavigate();
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const activeView = useUiStore((s) => s.activeView);

  const handleLogout = async () => {
    try {
      await invoke("logout");
    } catch (err) {
      console.error("Logout failed:", err);
    }
    useAuthStore.getState().logout();
    navigate("/login");
  };

  const handleHomeClick = () => {
    setActiveServer(null);
    setActiveChannel(null);
    setActiveView("home");
  };

  return (
    <div className="flex h-full w-[72px] flex-shrink-0 flex-col items-center border-r border-border bg-bg-primary pt-3">
      {/* Home button */}
      <button
        onClick={handleHomeClick}
        className={`flex h-12 w-12 items-center justify-center rounded-xl transition-colors ${
          activeView === "home"
            ? "bg-accent text-white"
            : "bg-bg-tertiary text-text-muted hover:bg-white/10"
        }`}
        title="Home"
      >
        <svg className="h-5 w-5" fill="currentColor" viewBox="0 0 20 20">
          <path d="M10.707 2.293a1 1 0 00-1.414 0l-7 7a1 1 0 001.414 1.414L4 10.414V17a1 1 0 001 1h2a1 1 0 001-1v-2a1 1 0 011-1h2a1 1 0 011 1v2a1 1 0 001 1h2a1 1 0 001-1v-6.586l.293.293a1 1 0 001.414-1.414l-7-7z" />
        </svg>
      </button>

      {/* Separator */}
      <div className="my-2 h-px w-8 bg-border" />

      {/* DM avatars placeholder - will be populated in DM phase */}
      <div className="flex flex-1 flex-col items-center gap-2 overflow-y-auto py-1" />

      {/* Logout button at bottom */}
      <button
        onClick={handleLogout}
        className="mb-3 flex h-10 w-10 items-center justify-center rounded-xl bg-bg-tertiary text-text-muted transition-colors hover:bg-error/20 hover:text-error"
        title="Log out"
      >
        <svg className="h-4 w-4" fill="none" viewBox="0 0 24 24" stroke="currentColor" strokeWidth={2}>
          <path strokeLinecap="round" strokeLinejoin="round" d="M17 16l4-4m0 0l-4-4m4 4H7m6 4v1a3 3 0 01-3 3H6a3 3 0 01-3-3V7a3 3 0 013-3h4a3 3 0 013 3v1" />
        </svg>
      </button>
    </div>
  );
}
```

- [ ] **Step 2: Create `src/layouts/MainLayout.tsx`**

The 4-panel layout shell. Uses placeholder divs for panels that haven't been built yet — they'll be swapped in during subsequent tasks.

```tsx
import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import { useConnectionEvents } from "../hooks/useConnectionEvents";
import { usePresenceEvents } from "../hooks/usePresenceEvents";
import { useUiStore } from "../stores/uiStore";

export default function MainLayout() {
  useConnectionEvents();
  usePresenceEvents();

  const connectionStatus = useUiStore((s) => s.connectionStatus);

  // On mount: fetch friends and server list
  useEffect(() => {
    invoke("request_friend_list").catch(console.error);
    invoke("request_server_list").catch(console.error);
  }, []);

  return (
    <div className="flex h-full w-full flex-col">
      {/* Connection banner */}
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      <div className="flex flex-1 overflow-hidden">
        {/* DM Sidebar - 72px */}
        <DmSidebar />

        {/* Main content area */}
        <div className="flex flex-1 flex-col overflow-hidden">
          {/* Server Bar - 64px (placeholder, Task 6) */}
          <div
            id="server-bar-slot"
            className="flex h-16 items-center border-b border-border bg-bg-primary px-4"
          >
            <span className="text-sm text-text-muted">Server bar...</span>
          </div>

          {/* Below server bar: channels + chat + right panel */}
          <div className="flex flex-1 overflow-hidden">
            {/* Channel Sidebar - 240px (placeholder, Task 7) */}
            <div
              id="channel-sidebar-slot"
              className="flex w-60 flex-shrink-0 flex-col border-r border-border bg-bg-primary p-4"
            >
              <span className="text-sm text-text-muted">Channels...</span>
            </div>

            {/* Chat Panel - flex center (placeholder, Task 8) */}
            <div
              id="chat-panel-slot"
              className="flex flex-1 items-center justify-center bg-bg-secondary"
            >
              <span className="text-sm text-text-muted">
                Select a channel to start chatting
              </span>
            </div>

            {/* Right Panel - 280px (placeholder, Task 9) */}
            <div
              id="right-panel-slot"
              className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary p-4"
            >
              <span className="text-sm text-text-muted">Friends / Members...</span>
            </div>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Update `src/App.tsx` to use MainLayout**

Replace the temporary placeholder for the `"/"` route:

```tsx
import { BrowserRouter, Routes, Route, Navigate } from "react-router-dom";
import AppLayout from "./layouts/AppLayout";
import MainLayout from "./layouts/MainLayout";
import LoginPage from "./features/auth/LoginPage";
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
                <MainLayout />
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

- [ ] **Step 4: Verify layout renders**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

Manually verify: The layout should show DM sidebar (left 72px), server bar (top 64px), channel sidebar (240px), chat center, and right panel (280px) — all with placeholder text. The connection banner should not be visible.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: add MainLayout shell with DmSidebar and panel placeholders"
```

---

## Task 6: Build Server Bar + Server Discovery Modal

**Files:**
- Create: `tauri-client/src/features/servers/ServerBar.tsx`
- Create: `tauri-client/src/features/servers/ServerDiscoveryModal.tsx`
- Create: `tauri-client/src/features/servers/useServerEvents.ts`
- Modify: `tauri-client/src/layouts/MainLayout.tsx`

- [ ] **Step 1: Create `src/features/servers/useServerEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";

interface ServerInfoPayload {
  id: number;
  name: string;
  description: string;
  hostIp: string;
  port: number;
  memberCount: number;
}

interface CommunityAuthPayload {
  serverId: string;
  success: boolean;
  message: string;
  channels: { id: string; name: string; type: string }[];
}

export function useServerEvents() {
  useEffect(() => {
    const unlistenServers = listen<{ servers: ServerInfoPayload[] }>(
      "server_list_received",
      (event) => {
        const servers = event.payload.servers.map((s) => ({
          id: String(s.id),
          name: s.name,
          description: s.description,
          hostIp: s.hostIp,
          port: s.port,
          memberCount: s.memberCount,
        }));
        useChatStore.getState().setServers(servers);
      }
    );

    const unlistenAuth = listen<CommunityAuthPayload>(
      "community_auth_responded",
      (event) => {
        const { serverId, success, channels } = event.payload;
        if (success) {
          useChatStore.getState().addConnectedServer(serverId);
          const typedChannels = channels.map((ch) => ({
            id: ch.id,
            name: ch.name,
            type: ch.type as "text" | "voice",
          }));
          useChatStore.getState().setChannelsForServer(serverId, typedChannels);

          // Auto-select first text channel
          const firstText = typedChannels.find((ch) => ch.type === "text");
          if (firstText) {
            useChatStore.getState().setActiveChannel(firstText.id);
            invoke("join_channel", {
              serverId,
              channelId: firstText.id,
            }).catch(console.error);
          }
        }
      }
    );

    return () => {
      unlistenServers.then((fn) => fn());
      unlistenAuth.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 2: Create `src/features/servers/ServerBar.tsx`**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

export default function ServerBar() {
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const openModal = useUiStore((s) => s.openModal);

  const connected = servers.filter((s) => connectedServers.has(s.id));

  const handleServerClick = (serverId: string) => {
    const currentChannel = useChatStore.getState().activeChannelId;
    setActiveServer(serverId);
    setActiveView("server");

    // Only auto-join first text channel if we don't already have one selected for this server
    const channels = useChatStore.getState().channelsByServer[serverId] ?? [];
    const currentInThisServer = channels.some((ch) => ch.id === currentChannel);
    if (!currentInThisServer) {
      setActiveChannel(null);
      const firstText = channels.find((ch) => ch.type === "text");
      if (firstText) {
        setActiveChannel(firstText.id);
        invoke("join_channel", { serverId, channelId: firstText.id }).catch(
          console.error
        );
      }
    }
  };

  const handleDisconnect = (e: React.MouseEvent, serverId: string) => {
    e.stopPropagation();
    invoke("disconnect_from_community", { serverId }).catch(console.error);
    useChatStore.getState().removeConnectedServer(serverId);
    if (activeServerId === serverId) {
      setActiveServer(null);
      setActiveChannel(null);
      setActiveView("home");
    }
  };

  return (
    <div className="flex h-16 items-center gap-2 border-b border-border bg-bg-primary px-4">
      {connected.map((server) => (
        <button
          key={server.id}
          onClick={() => handleServerClick(server.id)}
          className={`group relative flex items-center gap-2 rounded-lg px-3 py-1.5 text-sm transition-colors ${
            activeServerId === server.id
              ? "bg-accent/20 text-accent"
              : "text-text-primary hover:bg-white/5"
          }`}
        >
          <div className="flex h-7 w-7 items-center justify-center rounded-lg bg-bg-tertiary text-xs font-bold">
            {server.name.charAt(0).toUpperCase()}
          </div>
          <span className="max-w-[100px] truncate">{server.name}</span>

          {/* Disconnect button on hover */}
          <button
            onClick={(e) => handleDisconnect(e, server.id)}
            className="absolute -right-1 -top-1 hidden h-4 w-4 items-center justify-center rounded-full bg-error text-[10px] text-white group-hover:flex"
            title="Disconnect"
          >
            ×
          </button>
        </button>
      ))}

      {/* Add Server button */}
      <button
        onClick={() => openModal("server-discovery")}
        className="flex h-8 w-8 items-center justify-center rounded-lg bg-bg-tertiary text-lg text-success transition-colors hover:bg-success hover:text-white"
        title="Add Server"
      >
        +
      </button>
    </div>
  );
}
```

- [ ] **Step 3: Create `src/features/servers/ServerDiscoveryModal.tsx`**

```tsx
import { useState, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";

// Generate a consistent color from a string
function stringToColor(str: string): string {
  const colors = [
    "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
    "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
  ];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

export default function ServerDiscoveryModal() {
  const activeModal = useUiStore((s) => s.activeModal);
  const closeModal = useUiStore((s) => s.closeModal);
  const servers = useChatStore((s) => s.servers);
  const connectedServers = useChatStore((s) => s.connectedServers);
  const setActiveServer = useChatStore((s) => s.setActiveServer);
  const setActiveView = useUiStore((s) => s.setActiveView);

  const [search, setSearch] = useState("");
  const [isLoadingList, setIsLoadingList] = useState(false);
  const [connectingId, setConnectingId] = useState<string | null>(null);
  const [error, setError] = useState<string | null>(null);

  // Direct connect fields
  const [directHost, setDirectHost] = useState("");
  const [directPort, setDirectPort] = useState("");
  const [showDirect, setShowDirect] = useState(false);

  // Refresh server list when modal opens
  useEffect(() => {
    if (activeModal === "server-discovery") {
      setIsLoadingList(true);
      invoke("request_server_list")
        .catch(console.error)
        .finally(() => setIsLoadingList(false));
    }
  }, [activeModal]);

  if (activeModal !== "server-discovery") return null;

  const filtered = servers.filter(
    (s) =>
      s.name.toLowerCase().includes(search.toLowerCase()) ||
      s.description.toLowerCase().includes(search.toLowerCase())
  );

  const handleConnect = async (serverId: string, host: string, port: number) => {
    setConnectingId(serverId);
    setError(null);
    try {
      await invoke("connect_to_community", {
        serverId,
        host,
        port,
      });
      setActiveServer(serverId);
      setActiveView("server");
      closeModal();
    } catch (err) {
      setError(String(err));
    } finally {
      setConnectingId(null);
    }
  };

  const handleDirectConnect = async () => {
    const port = parseInt(directPort, 10);
    if (!directHost || isNaN(port)) {
      setError("Enter a valid host and port");
      return;
    }
    const serverId = `direct-${directHost}-${port}`;
    await handleConnect(serverId, directHost, port);
  };

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={closeModal}
    >
      <div
        className="flex max-h-[80vh] w-full max-w-[600px] flex-col rounded-xl bg-bg-secondary shadow-2xl"
        onClick={(e) => e.stopPropagation()}
      >
        {/* Header */}
        <div className="flex items-center justify-between border-b border-border px-6 py-4">
          <div>
            <h2 className="text-lg font-bold text-text-primary">
              Discover Servers
            </h2>
            <p className="text-sm text-text-muted">
              Browse available community servers
            </p>
          </div>
          <button
            onClick={closeModal}
            className="flex h-8 w-8 items-center justify-center rounded-lg text-text-muted transition-colors hover:bg-white/10 hover:text-text-primary"
          >
            ×
          </button>
        </div>

        {/* Search */}
        <div className="px-6 pt-4">
          <input
            type="text"
            value={search}
            onChange={(e) => setSearch(e.target.value)}
            placeholder="🔍 Search servers..."
            className="w-full rounded-lg border border-border bg-bg-primary px-4 py-2.5 text-sm text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent"
          />
        </div>

        {/* Error */}
        {error && (
          <p className="px-6 pt-2 text-sm text-error">{error}</p>
        )}

        {/* Server grid */}
        <div className="flex-1 overflow-y-auto p-6">
          {isLoadingList && servers.length === 0 ? (
            <div className="grid grid-cols-2 gap-3">
              {[1, 2, 3, 4].map((i) => (
                <div
                  key={i}
                  className="flex animate-pulse flex-col items-center rounded-xl border border-border bg-bg-primary p-4"
                >
                  <div className="mb-3 h-12 w-12 rounded-xl bg-bg-tertiary" />
                  <div className="mb-1 h-4 w-20 rounded bg-bg-tertiary" />
                  <div className="mb-2 h-3 w-28 rounded bg-bg-tertiary" />
                  <div className="h-3 w-16 rounded bg-bg-tertiary" />
                </div>
              ))}
            </div>
          ) : (
          <div className="grid grid-cols-2 gap-3">
            {filtered.map((server) => {
              const isConnected = connectedServers.has(server.id);
              const isConnecting = connectingId === server.id;

              return (
                <button
                  key={server.id}
                  onClick={() =>
                    !isConnected &&
                    handleConnect(server.id, server.hostIp, server.port)
                  }
                  disabled={isConnected || isConnecting}
                  className="flex flex-col items-center rounded-xl border border-border bg-bg-primary p-4 text-center transition-colors hover:border-accent/50 disabled:opacity-50"
                >
                  <div
                    className="mb-3 flex h-12 w-12 items-center justify-center rounded-xl text-lg font-bold text-white"
                    style={{ backgroundColor: stringToColor(server.name) }}
                  >
                    {server.name.charAt(0).toUpperCase()}
                  </div>
                  <span className="mb-1 text-sm font-semibold text-text-primary">
                    {server.name}
                  </span>
                  <span className="mb-2 line-clamp-2 text-xs text-text-muted">
                    {server.description}
                  </span>
                  <span className="mt-auto text-xs text-text-muted">
                    {server.memberCount} members
                    {isConnected && (
                      <span className="ml-1 text-success">· Connected</span>
                    )}
                  </span>
                  {isConnecting && (
                    <svg
                      className="mt-2 h-4 w-4 animate-spin text-accent"
                      viewBox="0 0 24 24"
                      fill="none"
                    >
                      <circle
                        cx="12"
                        cy="12"
                        r="10"
                        stroke="currentColor"
                        strokeWidth="3"
                        className="opacity-25"
                      />
                      <path
                        d="M4 12a8 8 0 018-8"
                        stroke="currentColor"
                        strokeWidth="3"
                        strokeLinecap="round"
                        className="opacity-75"
                      />
                    </svg>
                  )}
                </button>
              );
            })}

            {/* Add by IP card */}
            <button
              onClick={() => setShowDirect(!showDirect)}
              className="flex flex-col items-center rounded-xl border border-dashed border-border bg-bg-primary p-4 text-center transition-colors hover:border-accent/50"
            >
              <div className="mb-3 flex h-12 w-12 items-center justify-center rounded-xl bg-bg-tertiary text-2xl text-success">
                +
              </div>
              <span className="text-sm font-semibold text-text-muted">
                Add by IP
              </span>
              <span className="text-xs text-text-muted">Connect directly</span>
            </button>
          </div>

          </div>
          )}

          {/* Direct connect form */}
          {showDirect && (
            <div className="mt-4 flex gap-2">
              <input
                type="text"
                value={directHost}
                onChange={(e) => setDirectHost(e.target.value)}
                placeholder="Host (e.g., 192.168.1.100)"
                className="flex-1 rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
              />
              <input
                type="text"
                value={directPort}
                onChange={(e) => setDirectPort(e.target.value)}
                placeholder="Port"
                className="w-20 rounded-lg border border-border bg-bg-primary px-3 py-2 text-sm text-text-primary outline-none focus:border-accent"
              />
              <button
                onClick={handleDirectConnect}
                className="rounded-lg bg-accent px-4 py-2 text-sm font-semibold text-white transition-colors hover:bg-accent-hover"
              >
                Connect
              </button>
            </div>
          )}

          {/* Empty state */}
          {filtered.length === 0 && (
            <p className="mt-8 text-center text-sm text-text-muted">
              No servers found.
            </p>
          )}
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Update `src/layouts/MainLayout.tsx`**

Replace the server bar placeholder with the real components. Add server event hook. Import and render `ServerDiscoveryModal`:

```tsx
import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerDiscoveryModal from "../features/servers/ServerDiscoveryModal";
import { useConnectionEvents } from "../hooks/useConnectionEvents";
import { usePresenceEvents } from "../hooks/usePresenceEvents";
import { useServerEvents } from "../features/servers/useServerEvents";
import { useFriendsEvents } from "../features/friends/useFriendsEvents";
import { useUiStore } from "../stores/uiStore";

export default function MainLayout() {
  useConnectionEvents();
  usePresenceEvents();
  useServerEvents();
  useFriendsEvents();

  const connectionStatus = useUiStore((s) => s.connectionStatus);

  useEffect(() => {
    invoke("request_friend_list").catch(console.error);
    invoke("request_server_list").catch(console.error);
  }, []);

  return (
    <div className="flex h-full w-full flex-col">
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      <div className="flex flex-1 overflow-hidden">
        <DmSidebar />

        <div className="flex flex-1 flex-col overflow-hidden">
          <ServerBar />

          <div className="flex flex-1 overflow-hidden">
            {/* Channel Sidebar placeholder - Task 7 */}
            <div className="flex w-60 flex-shrink-0 flex-col border-r border-border bg-bg-primary p-4">
              <span className="text-sm text-text-muted">Channels...</span>
            </div>

            {/* Chat Panel placeholder - Task 8 */}
            <div className="flex flex-1 items-center justify-center bg-bg-secondary">
              <span className="text-sm text-text-muted">
                Select a channel to start chatting
              </span>
            </div>

            {/* Right Panel placeholder - Task 9 */}
            <div className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary p-4">
              <span className="text-sm text-text-muted">Friends / Members...</span>
            </div>
          </div>
        </div>
      </div>

      <ServerDiscoveryModal />
    </div>
  );
}
```

**Note:** This step references `useFriendsEvents` which is created in Task 9. You may need to create a stub first or build Task 9 before Task 6. Alternatively, comment out the `useFriendsEvents` import/call until Task 9, then add it back.

To avoid the dependency, create a minimal stub now:

Create `src/features/friends/useFriendsEvents.ts` (will be fully implemented in Task 9):

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { invoke } from "@tauri-apps/api/core";
import { useFriendsStore } from "../../stores/friendsStore";

export function useFriendsEvents() {
  useEffect(() => {
    const unlistenList = listen<{ friends: { username: string; status: string }[] }>(
      "friend_list_received",
      (event) => {
        const friends = event.payload.friends.map((f) => ({
          username: f.username,
          status: f.status as "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked",
        }));
        useFriendsStore.getState().setFriends(friends);
      }
    );

    const unlistenAction = listen<{ success: boolean; message: string }>(
      "friend_action_responded",
      () => {
        // Refresh friend list after any action
        invoke("request_friend_list").catch(console.error);
      }
    );

    return () => {
      unlistenList.then((fn) => fn());
      unlistenAction.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 5: Verify TypeScript compiles and layout renders with server bar**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

Verify visually: Server bar shows at top with just a green "+" button. Clicking "+" opens the discovery modal.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: add ServerBar, ServerDiscoveryModal, and server event listeners"
```

---

## Task 7: Build Channel Sidebar

**Files:**
- Create: `tauri-client/src/features/channels/ChannelSidebar.tsx`
- Create: `tauri-client/src/features/channels/useChannelEvents.ts`
- Modify: `tauri-client/src/layouts/MainLayout.tsx`

- [ ] **Step 1: Create `src/features/channels/useChannelEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";

interface JoinChannelPayload {
  serverId: string;
  success: boolean;
  channelId: string;
  activeUsers: string[];
}

export function useChannelEvents() {
  useEffect(() => {
    const unlisten = listen<JoinChannelPayload>(
      "join_channel_responded",
      (event) => {
        const { success, channelId, activeUsers } = event.payload;
        if (success) {
          useChatStore.getState().setChannelMembers(channelId, activeUsers);
        }
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 2: Create `src/features/channels/ChannelSidebar.tsx`**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import { useChannelEvents } from "./useChannelEvents";

export default function ChannelSidebar() {
  useChannelEvents();

  const activeView = useUiStore((s) => s.activeView);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const servers = useChatStore((s) => s.servers);
  const setActiveChannel = useChatStore((s) => s.setActiveChannel);

  const channels = activeServerId
    ? channelsByServer[activeServerId] ?? []
    : [];
  const textChannels = channels.filter((ch) => ch.type === "text");
  const voiceChannels = channels.filter((ch) => ch.type === "voice");
  const serverName = servers.find((s) => s.id === activeServerId)?.name;

  const handleChannelClick = (channelId: string) => {
    if (!activeServerId) return;
    setActiveChannel(channelId);
    invoke("join_channel", {
      serverId: activeServerId,
      channelId,
    }).catch(console.error);
  };

  if (activeView === "home") {
    return (
      <div className="flex w-60 flex-shrink-0 flex-col border-r border-border bg-bg-primary">
        <div className="border-b border-border px-4 py-3">
          <h2 className="text-sm font-semibold text-text-primary">
            Direct Messages
          </h2>
        </div>
        <div className="flex flex-1 items-center justify-center">
          <p className="text-xs text-text-muted">Coming soon...</p>
        </div>
      </div>
    );
  }

  return (
    <div className="flex w-60 flex-shrink-0 flex-col border-r border-border bg-bg-primary">
      {/* Server name header */}
      <div className="border-b border-border px-4 py-3">
        <h2 className="truncate text-sm font-semibold text-text-primary">
          {serverName ?? "Server"}
        </h2>
      </div>

      {/* Channel list */}
      <div className="flex-1 overflow-y-auto px-2 py-3">
        {/* Text channels */}
        {textChannels.length > 0 && (
          <div className="mb-4">
            <h3 className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wide text-text-muted">
              Text Channels
            </h3>
            {textChannels.map((ch) => (
              <button
                key={ch.id}
                onClick={() => handleChannelClick(ch.id)}
                className={`flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm transition-colors ${
                  activeChannelId === ch.id
                    ? "bg-white/10 text-accent"
                    : "text-text-muted hover:bg-white/5 hover:text-text-primary"
                }`}
              >
                <span className="text-text-muted">#</span>
                <span className="truncate">{ch.name}</span>
              </button>
            ))}
          </div>
        )}

        {/* Voice channels */}
        {voiceChannels.length > 0 && (
          <div>
            <h3 className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wide text-text-muted">
              Voice Channels
            </h3>
            {voiceChannels.map((ch) => (
              <div
                key={ch.id}
                className="flex w-full items-center gap-1.5 rounded-md px-2 py-1.5 text-sm text-text-muted"
                title="Voice channels available in Phase 4"
              >
                <span>🔊</span>
                <span className="truncate">{ch.name}</span>
              </div>
            ))}
          </div>
        )}

        {channels.length === 0 && (
          <p className="px-2 text-xs text-text-muted">No channels</p>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Update `src/layouts/MainLayout.tsx`**

Replace the channel sidebar placeholder with the real component:

Import `ChannelSidebar` from `"../features/channels/ChannelSidebar"` and replace the placeholder div (the one with id `channel-sidebar-slot` or the "Channels..." text) with `<ChannelSidebar />`.

- [ ] **Step 4: Verify TypeScript compiles and channels render**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

Verify visually: Home view shows "Direct Messages" header with "Coming soon..." text. When connected to a server, text and voice channels appear categorized.

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "feat: add ChannelSidebar with text/voice channel lists"
```

---

## Task 8: Build Chat Panel

**Files:**
- Create: `tauri-client/src/features/chat/ChatPanel.tsx`
- Create: `tauri-client/src/features/chat/MessageBubble.tsx`
- Create: `tauri-client/src/features/chat/useChatEvents.ts`
- Modify: `tauri-client/src/layouts/MainLayout.tsx`

- [ ] **Step 1: Create `src/features/chat/useChatEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";

interface MessagePayload {
  context: string;
  sender: string;
  content: string;
  timestamp: string;
}

export function useChatEvents() {
  useEffect(() => {
    const unlisten = listen<MessagePayload>("message_received", (event) => {
      useChatStore.getState().addMessage({
        sender: event.payload.sender,
        content: event.payload.content,
        timestamp: event.payload.timestamp,
        channelId: event.payload.context,
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 2: Create `src/features/chat/MessageBubble.tsx`**

```tsx
import type { Message } from "../../types";

// Same color function as server discovery
function stringToColor(str: string): string {
  const colors = [
    "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
    "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
  ];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

function formatTimestamp(ts: string): string {
  const date = new Date(parseInt(ts, 10) * 1000);
  if (isNaN(date.getTime())) return ts;
  const now = new Date();
  const isToday = date.toDateString() === now.toDateString();
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  return isToday ? `Today at ${time}` : `${date.toLocaleDateString()} ${time}`;
}

export default function MessageBubble({ message }: { message: Message }) {
  return (
    <div className="flex gap-3 px-4 py-1.5 hover:bg-white/[0.02]">
      {/* Avatar */}
      <div
        className="flex h-9 w-9 flex-shrink-0 items-center justify-center rounded-xl text-sm font-bold text-white"
        style={{ backgroundColor: stringToColor(message.sender) }}
      >
        {message.sender.charAt(0).toUpperCase()}
      </div>

      {/* Content */}
      <div className="min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span className="text-sm font-semibold text-accent">
            {message.sender}
          </span>
          <span className="text-[11px] text-text-muted">
            {formatTimestamp(message.timestamp)}
          </span>
        </div>
        <p className="mt-0.5 break-words text-sm text-text-primary">
          {message.content}
        </p>
      </div>
    </div>
  );
}
```

- [ ] **Step 3: Create `src/features/chat/ChatPanel.tsx`**

```tsx
import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useUiStore } from "../../stores/uiStore";
import MessageBubble from "./MessageBubble";
import { useChatEvents } from "./useChatEvents";

export default function ChatPanel() {
  useChatEvents();

  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const messagesByChannel = useChatStore((s) => s.messagesByChannel);
  const channelsByServer = useChatStore((s) => s.channelsByServer);
  const activeView = useUiStore((s) => s.activeView);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const messages = activeChannelId
    ? messagesByChannel[activeChannelId] ?? []
    : [];

  const channelName = activeServerId
    ? channelsByServer[activeServerId]?.find(
        (ch) => ch.id === activeChannelId
      )?.name
    : null;

  // Auto-scroll to bottom on new messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length]);

  const handleSend = async () => {
    if (!input.trim() || !activeServerId || !activeChannelId) return;

    setSending(true);
    setSendError(null);
    try {
      await invoke("send_channel_message", {
        serverId: activeServerId,
        channelId: activeChannelId,
        message: input.trim(),
      });
      setInput("");
    } catch (err) {
      setSendError(String(err));
    } finally {
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  // Empty state
  if (activeView === "home" || !activeChannelId) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-secondary">
        <p className="text-sm text-text-muted">
          Select a channel to start chatting
        </p>
      </div>
    );
  }

  return (
    <div className="flex flex-1 flex-col bg-bg-secondary">
      {/* Channel header */}
      <div className="flex h-12 items-center border-b border-border px-4">
        <span className="text-sm font-semibold text-text-primary">
          # {channelName ?? activeChannelId}
        </span>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto py-4">
        {messages.length === 0 ? (
          <div className="flex h-full items-center justify-center">
            <p className="text-sm text-text-muted">
              No messages yet. Be the first to say something!
            </p>
          </div>
        ) : (
          messages.map((msg, i) => <MessageBubble key={i} message={msg} />)
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Input bar */}
      <div className="px-4 pb-4">
        <input
          type="text"
          value={input}
          onChange={(e) => setInput(e.target.value)}
          onKeyDown={handleKeyDown}
          disabled={sending}
          placeholder={`Message #${channelName ?? "channel"}`}
          className="w-full rounded-lg bg-bg-tertiary px-4 py-2.5 text-sm text-text-primary outline-none placeholder:text-text-muted disabled:opacity-50"
        />
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Update `src/layouts/MainLayout.tsx`**

Replace the chat panel placeholder with the real component:

Import `ChatPanel` from `"../features/chat/ChatPanel"` and replace the placeholder div with `<ChatPanel />`.

- [ ] **Step 5: Verify TypeScript compiles and chat panel renders**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

Verify visually: Empty state shows "Select a channel to start chatting". When a channel is selected, shows the message input bar.

- [ ] **Step 6: Commit**

```bash
git add -A && git commit -m "feat: add ChatPanel with message list and input"
```

---

## Task 9: Build Friends List + Members List

**Files:**
- Create: `tauri-client/src/features/friends/FriendsList.tsx`
- Create: `tauri-client/src/features/friends/MembersList.tsx`
- Create: `tauri-client/src/features/friends/FriendActionButton.tsx`
- Modify: `tauri-client/src/features/friends/useFriendsEvents.ts` (already created as stub in Task 6)
- Modify: `tauri-client/src/layouts/MainLayout.tsx`

- [ ] **Step 1: Verify `useFriendsEvents.ts` is complete**

The stub created in Task 6 should already be the full implementation. If not, ensure it matches the version from Task 6 Step 4.

- [ ] **Step 2: Create `src/features/friends/FriendActionButton.tsx`**

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";

// Maps to protobuf FriendActionType enum
const FRIEND_ACTIONS = {
  ADD: 0,
  REMOVE: 1,
  BLOCK: 2,
  ACCEPT: 3,
  REJECT: 4,
} as const;

interface Props {
  action: keyof typeof FRIEND_ACTIONS;
  targetUsername: string;
  label: string;
  variant?: "accent" | "success" | "error" | "muted";
}

export default function FriendActionButton({
  action,
  targetUsername,
  label,
  variant = "accent",
}: Props) {
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  const handleClick = async () => {
    setLoading(true);
    setError(null);
    try {
      await invoke("send_friend_action", {
        action: FRIEND_ACTIONS[action],
        targetUsername,
      });
    } catch (err) {
      setError(String(err));
    } finally {
      setLoading(false);
    }
  };

  const colorClass = {
    accent: "bg-accent hover:bg-accent-hover",
    success: "bg-success hover:bg-success/80",
    error: "bg-error hover:bg-error/80",
    muted: "bg-bg-tertiary hover:bg-white/10",
  }[variant];

  return (
    <div className="inline-flex flex-col">
      <button
        onClick={handleClick}
        disabled={loading}
        className={`rounded-md px-2.5 py-1 text-xs font-semibold text-white transition-colors disabled:opacity-50 ${colorClass}`}
      >
        {loading ? "..." : label}
      </button>
      {error && <span className="mt-0.5 text-[10px] text-error">{error}</span>}
    </div>
  );
}
```

- [ ] **Step 3: Create `src/features/friends/FriendsList.tsx`**

```tsx
import { useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useFriendsStore } from "../../stores/friendsStore";
import FriendActionButton from "./FriendActionButton";
import type { FriendInfo } from "../../types";

function stringToColor(str: string): string {
  const colors = [
    "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
    "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
  ];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

function FriendRow({ friend }: { friend: FriendInfo }) {
  const isOnline = friend.status === "online";
  const isPendingIn = friend.status === "pending_incoming";
  const isPendingOut = friend.status === "pending_outgoing";

  return (
    <div className="flex items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-white/5">
      {/* Avatar with status dot */}
      <div className="relative flex-shrink-0">
        <div
          className="flex h-8 w-8 items-center justify-center rounded-xl text-xs font-bold text-white"
          style={{ backgroundColor: stringToColor(friend.username) }}
        >
          {friend.username.charAt(0).toUpperCase()}
        </div>
        {(friend.status === "online" || friend.status === "offline") && (
          <div
            className={`absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-bg-primary ${
              isOnline ? "bg-success" : "bg-[#4f6a86]"
            }`}
          />
        )}
      </div>

      {/* Username */}
      <span className="min-w-0 flex-1 truncate text-sm text-text-primary">
        {friend.username}
      </span>

      {/* Actions */}
      {isPendingIn && (
        <div className="flex gap-1">
          <FriendActionButton
            action="ACCEPT"
            targetUsername={friend.username}
            label="Accept"
            variant="success"
          />
          <FriendActionButton
            action="REJECT"
            targetUsername={friend.username}
            label="Reject"
            variant="error"
          />
        </div>
      )}
      {isPendingOut && (
        <span className="text-xs text-text-muted">Pending</span>
      )}
    </div>
  );
}

export default function FriendsList() {
  const friends = useFriendsStore((s) => s.friends);
  const [search, setSearch] = useState("");
  const [addUsername, setAddUsername] = useState("");
  const [showAdd, setShowAdd] = useState(false);

  const filtered = friends.filter((f) =>
    f.username.toLowerCase().includes(search.toLowerCase())
  );

  const sections: { label: string; items: FriendInfo[] }[] = [
    {
      label: "ONLINE",
      items: filtered.filter((f) => f.status === "online"),
    },
    {
      label: "OFFLINE",
      items: filtered.filter((f) => f.status === "offline"),
    },
    {
      label: "PENDING",
      items: filtered.filter(
        (f) =>
          f.status === "pending_incoming" || f.status === "pending_outgoing"
      ),
    },
    {
      label: "BLOCKED",
      items: filtered.filter((f) => f.status === "blocked"),
    },
  ];

  const handleAddFriend = async () => {
    if (!addUsername.trim()) return;
    try {
      await invoke("send_friend_action", {
        action: 0, // ADD
        targetUsername: addUsername.trim(),
      });
      setAddUsername("");
      setShowAdd(false);
    } catch (err) {
      console.error("Failed to add friend:", err);
    }
  };

  return (
    <div className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary">
      {/* Header */}
      <div className="flex items-center justify-between border-b border-border px-4 py-3">
        <h2 className="text-sm font-semibold text-text-primary">Friends</h2>
        <button
          onClick={() => setShowAdd(!showAdd)}
          className="text-xs text-accent hover:underline"
        >
          Add Friend
        </button>
      </div>

      {/* Add friend input */}
      {showAdd && (
        <div className="flex gap-2 border-b border-border px-3 py-2">
          <input
            type="text"
            value={addUsername}
            onChange={(e) => setAddUsername(e.target.value)}
            onKeyDown={(e) => e.key === "Enter" && handleAddFriend()}
            placeholder="Username"
            className="flex-1 rounded-md border border-border bg-bg-primary px-2 py-1 text-sm text-text-primary outline-none focus:border-accent"
          />
          <button
            onClick={handleAddFriend}
            className="rounded-md bg-accent px-2 py-1 text-xs font-semibold text-white hover:bg-accent-hover"
          >
            Send
          </button>
        </div>
      )}

      {/* Search */}
      <div className="px-3 py-2">
        <input
          type="text"
          value={search}
          onChange={(e) => setSearch(e.target.value)}
          placeholder="Search friends..."
          className="w-full rounded-md border border-border bg-bg-primary px-2.5 py-1.5 text-xs text-text-primary outline-none placeholder:text-text-muted focus:border-accent"
        />
      </div>

      {/* Friend sections */}
      <div className="flex-1 overflow-y-auto px-2 pb-2">
        {sections.map(
          (section) =>
            section.items.length > 0 && (
              <div key={section.label} className="mb-3">
                <h3 className="mb-1 px-2 text-[11px] font-semibold uppercase tracking-wide text-text-muted">
                  {section.label} — {section.items.length}
                </h3>
                {section.items.map((friend) => (
                  <FriendRow key={friend.username} friend={friend} />
                ))}
              </div>
            )
        )}
        {friends.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No friends yet. Add someone!
          </p>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 4: Create `src/features/friends/MembersList.tsx`**

```tsx
import { useChatStore } from "../../stores/chatStore";

function stringToColor(str: string): string {
  const colors = [
    "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
    "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
  ];
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return colors[Math.abs(hash) % colors.length];
}

export default function MembersList() {
  const activeChannelId = useChatStore((s) => s.activeChannelId);
  const channelMembers = useChatStore((s) => s.channelMembers);

  const members = activeChannelId
    ? channelMembers[activeChannelId] ?? []
    : [];

  return (
    <div className="flex w-70 flex-shrink-0 flex-col border-l border-border bg-bg-primary">
      <div className="border-b border-border px-4 py-3">
        <h3 className="text-[11px] font-semibold uppercase tracking-wide text-text-muted">
          Online — {members.length}
        </h3>
      </div>

      <div className="flex-1 overflow-y-auto px-2 py-2">
        {members.map((username) => (
          <div
            key={username}
            className="flex items-center gap-3 rounded-lg px-2 py-1.5 hover:bg-white/5"
          >
            <div className="relative flex-shrink-0">
              <div
                className="flex h-8 w-8 items-center justify-center rounded-xl text-xs font-bold text-white"
                style={{ backgroundColor: stringToColor(username) }}
              >
                {username.charAt(0).toUpperCase()}
              </div>
              <div className="absolute -bottom-0.5 -right-0.5 h-3 w-3 rounded-full border-2 border-bg-primary bg-success" />
            </div>
            <span className="truncate text-sm text-text-primary">
              {username}
            </span>
          </div>
        ))}
        {members.length === 0 && (
          <p className="mt-4 text-center text-xs text-text-muted">
            No members in this channel
          </p>
        )}
      </div>
    </div>
  );
}
```

- [ ] **Step 5: Update `src/layouts/MainLayout.tsx` — final version**

Replace the right panel placeholder with context-aware rendering. This is the final version of MainLayout with all real components:

```tsx
import { useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import DmSidebar from "./DmSidebar";
import ServerBar from "../features/servers/ServerBar";
import ServerDiscoveryModal from "../features/servers/ServerDiscoveryModal";
import ChannelSidebar from "../features/channels/ChannelSidebar";
import ChatPanel from "../features/chat/ChatPanel";
import FriendsList from "../features/friends/FriendsList";
import MembersList from "../features/friends/MembersList";
import { useConnectionEvents } from "../hooks/useConnectionEvents";
import { usePresenceEvents } from "../hooks/usePresenceEvents";
import { useServerEvents } from "../features/servers/useServerEvents";
import { useFriendsEvents } from "../features/friends/useFriendsEvents";
import { useUiStore } from "../stores/uiStore";

export default function MainLayout() {
  useConnectionEvents();
  usePresenceEvents();
  useServerEvents();
  useFriendsEvents();

  const connectionStatus = useUiStore((s) => s.connectionStatus);
  const activeView = useUiStore((s) => s.activeView);

  useEffect(() => {
    invoke("request_friend_list").catch(console.error);
    invoke("request_server_list").catch(console.error);
  }, []);

  return (
    <div className="flex h-full w-full flex-col">
      {connectionStatus === "reconnecting" && (
        <div className="flex h-8 items-center justify-center bg-warning text-xs font-semibold text-bg-primary">
          Connection lost. Reconnecting...
        </div>
      )}

      <div className="flex flex-1 overflow-hidden">
        <DmSidebar />

        <div className="flex flex-1 flex-col overflow-hidden">
          <ServerBar />

          <div className="flex flex-1 overflow-hidden">
            <ChannelSidebar />
            <ChatPanel />
            {activeView === "home" ? <FriendsList /> : <MembersList />}
          </div>
        </div>
      </div>

      <ServerDiscoveryModal />
    </div>
  );
}
```

- [ ] **Step 6: Verify TypeScript compiles and full layout renders**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

Verify visually: Full layout renders with all panels. Home view shows friends panel on right. No real data yet (no backend running), but all panels should be visible.

- [ ] **Step 7: Run all tests**

Run: `cd tauri-client && npx vitest run`
Expected: All 19 store tests pass

- [ ] **Step 8: Commit**

```bash
git add -A && git commit -m "feat: add FriendsList, MembersList, and complete MainLayout"
```

---

## Task 10: Extract Shared Utility + Final Cleanup

**Files:**
- Create: `tauri-client/src/utils/colors.ts`
- Modify: `tauri-client/src/features/chat/MessageBubble.tsx`
- Modify: `tauri-client/src/features/friends/FriendsList.tsx`
- Modify: `tauri-client/src/features/friends/MembersList.tsx`
- Modify: `tauri-client/src/features/servers/ServerDiscoveryModal.tsx`

The `stringToColor` function is duplicated in 4 files. Extract it.

- [ ] **Step 1: Create `src/utils/colors.ts`**

```typescript
const AVATAR_COLORS = [
  "#2CA3E8", "#E8752C", "#8B5CF6", "#43B581",
  "#FAA61A", "#FF4C4C", "#E879F9", "#06B6D4",
];

export function stringToColor(str: string): string {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return AVATAR_COLORS[Math.abs(hash) % AVATAR_COLORS.length];
}
```

- [ ] **Step 2: Replace all `stringToColor` copies with import**

In each of `MessageBubble.tsx`, `FriendsList.tsx`, `MembersList.tsx`, `ServerDiscoveryModal.tsx`:
- Remove the local `stringToColor` function
- Add: `import { stringToColor } from "../../utils/colors";` (adjust path as needed — `MembersList` and `FriendsList` use `../../utils/colors`, `MessageBubble` uses `../../utils/colors`, `ServerDiscoveryModal` uses `../../utils/colors`)

- [ ] **Step 3: Delete old `src/pages/` directory if still present**

Verify `src/pages/LoginPage.tsx` and `src/pages/HomePage.tsx` are deleted (should have been removed in Task 4). `src/pages/SettingsPage.tsx` stays.

- [ ] **Step 4: Run all tests and verify build**

Run: `cd tauri-client && npx vitest run`
Expected: All 19 tests pass

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

- [ ] **Step 5: Commit**

```bash
git add -A && git commit -m "refactor: extract shared stringToColor utility, final cleanup"
```

---

## Task 11: Full Integration Verification

**Files:** None (verification only)

- [ ] **Step 1: Run all tests**

Run: `cd tauri-client && npx vitest run`
Expected: All tests pass

- [ ] **Step 2: Run TypeScript check**

Run: `cd tauri-client && npx tsc --noEmit`
Expected: No errors

- [ ] **Step 3: Build the Tauri app**

Run: `cd tauri-client && WEBKIT_DISABLE_DMABUF_RENDERER=1 npm run tauri dev`
Expected: App builds and launches. Verify:

1. **Login page** renders with centered card, username/password fields, Log In button
2. **Register toggle** works — shows email field
3. **Layout** (after manually setting auth via dev tools or test credentials):
   - DM sidebar on far left (72px, home icon)
   - Server bar at top (64px, "+" button)
   - Channel sidebar (240px)
   - Chat panel (center, "Select a channel..." empty state)
   - Right panel (280px, friends list)
4. **Server discovery** opens when clicking "+"
5. **Connection banner** does not show when connected

- [ ] **Step 4: Final commit if any fixes were needed**

```bash
git add -A && git commit -m "fix: integration verification fixes"
```

(Skip if no fixes needed)
