# Direct Messages Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Add direct messaging between users via the central server, with a user profile popup, DM conversation UI, and friends-only privacy setting.

**Architecture:** New `dmStore` manages DM conversations separately from channel chat. A `DmChatPanel` component handles DM-specific rendering. The Rust event payload gains a `recipient` field so outgoing DM echoes route correctly. The C++ server adds DM privacy enforcement. A `UserProfilePopup` component provides the entry point for starting DMs from anywhere in the app.

**Tech Stack:** React, Zustand, Tauri v2 (Rust), C++ central server, Protobuf

**Spec:** `docs/superpowers/specs/2026-03-22-direct-messages-design.md`

---

## File Structure

### New files
| File | Responsibility |
|------|---------------|
| `src/stores/dmStore.ts` | DM conversation state — conversations keyed by username, active DM user, privacy setting |
| `src/features/dm/DmChatPanel.tsx` | DM chat view — header with recipient info, message list, input bar |
| `src/features/dm/UserProfilePopup.tsx` | Floating profile popup — avatar, status, message input, portal-rendered |
| `src/features/dm/useDmEvents.ts` | DM-specific event listener hook — routes `message_received` with context="dm" to dmStore |

### Modified files
| File | Change |
|------|--------|
| `src-tauri/src/events/mod.rs` | Add `recipient` field to `MessageReceivedPayload` and `emit_message_received` |
| `src-tauri/src/net/central.rs` | Populate `recipient` from `DirectMessage.recipient` for DM events |
| `src/stores/uiStore.ts` | Add `"dm"` to activeView union, add profile popup state |
| `src/types/index.ts` | Add `DmMessage` interface (extends Message without channelId) |
| `src/features/chat/useChatEvents.ts` | Skip DM messages (handled by useDmEvents instead) |
| `src/layouts/MainLayout.tsx` | Add `"dm"` view routing — render DmChatPanel, hide right panel |
| `src/layouts/DmSidebar.tsx` | Populate with DM conversation avatars from dmStore |
| `src/features/channels/ChannelSidebar.tsx` | Show DM conversation list in home view |
| `src/features/chat/MessageBubble.tsx` | Click handler on sender username to open profile popup |
| `src/features/friends/FriendsList.tsx` | Click handler on friend username to open profile popup |
| `src/features/friends/MembersList.tsx` | Click handler on member username to open profile popup |
| `src/pages/SettingsPage.tsx` | Add Privacy section with friends-only DM toggle |
| `src-tauri/src/commands/messaging.rs` | Add `set_dm_privacy` command |
| `src-tauri/src/net/central.rs` | Send DM privacy packet |
| `proto/messages.proto` | Add `DM_PRIVACY = <type>` and `DmPrivacySetting` message |
| `src/server/main.cpp` | Enforce DM privacy check before delivering DMs |

---

## Task 1: Rust — Add `recipient` field to DM event payload

**Files:**
- Modify: `src-tauri/src/events/mod.rs`
- Modify: `src-tauri/src/net/central.rs`
- Modify: `src-tauri/src/net/community.rs`

- [ ] **Step 1: Add `recipient` to `MessageReceivedPayload`**

In `src-tauri/src/events/mod.rs`, add `recipient` field to the struct:

```rust
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MessageReceivedPayload {
    pub context: String,
    pub sender: String,
    pub recipient: String,  // NEW — populated for DMs, empty for channel messages
    pub content: String,
    pub timestamp: String,
}
```

- [ ] **Step 2: Update `emit_message_received` to accept `recipient`**

```rust
pub fn emit_message_received(
    app: &AppHandle,
    context: String,
    sender: String,
    recipient: String,  // NEW
    content: String,
    timestamp: String,
) {
    let _ = app.emit(
        MESSAGE_RECEIVED,
        MessageReceivedPayload {
            context,
            sender,
            recipient,  // NEW
            content,
            timestamp,
        },
    );
}
```

- [ ] **Step 3: Update all call sites of `emit_message_received`**

In `src-tauri/src/net/central.rs`, find the DM routing (around line 255):

```rust
Some(packet::Payload::DirectMsg(msg)) => {
    events::emit_message_received(
        &app,
        "dm".to_string(),
        msg.sender,
        msg.recipient,  // NEW — pass recipient through
        msg.content,
        msg.timestamp.to_string(),
    );
}
```

Find the channel message routing and pass empty string for recipient:

```rust
Some(packet::Payload::ChannelMsg(msg)) => {
    events::emit_message_received(
        &app,
        msg.channel_id,
        msg.sender,
        String::new(),  // NEW — no recipient for channel messages
        msg.content,
        msg.timestamp.to_string(),
    );
}
```

Also update the call site in `src-tauri/src/net/community.rs` (channel messages from community servers):

```rust
events::emit_message_received(
    &app,
    context,
    msg.sender,
    String::new(),  // No recipient for channel messages
    msg.content,
    msg.timestamp.to_string(),
);
```

Search for any other call sites of `emit_message_received` across the entire `src-tauri/` directory and add the empty `String::new()` recipient parameter.

- [ ] **Step 4: Build to verify compilation**

Run: `cd src-tauri && cargo check`
Expected: Compiles with no errors.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/events/mod.rs src-tauri/src/net/central.rs
git commit -m "feat(dm): add recipient field to message event payload"
```

---

## Task 2: Frontend — Create `dmStore`

**Files:**
- Create: `src/stores/dmStore.ts`
- Modify: `src/types/index.ts`

- [ ] **Step 1: Add `DmMessage` type to `types/index.ts`**

Add after the existing `Message` interface:

```typescript
export interface DmMessage {
  sender: string;
  content: string;
  timestamp: string;
}
```

- [ ] **Step 2: Create `src/stores/dmStore.ts`**

```typescript
import { create } from "zustand";
import type { DmMessage } from "../types";

interface DmConversation {
  username: string;
  messages: DmMessage[];
  lastMessageTime: number;
}

interface DmState {
  conversations: Record<string, DmConversation>;
  activeDmUser: string | null;
  friendsOnlyDms: boolean;

  setActiveDmUser: (username: string | null) => void;
  addDmMessage: (otherUser: string, message: DmMessage) => void;
  setFriendsOnlyDms: (value: boolean) => void;
}

export const useDmStore = create<DmState>((set) => ({
  conversations: {},
  activeDmUser: null,
  friendsOnlyDms: false,

  setActiveDmUser: (username) => set({ activeDmUser: username }),

  addDmMessage: (otherUser, message) =>
    set((state) => {
      const existing = state.conversations[otherUser];
      const timestamp = parseInt(message.timestamp, 10);
      const time = isNaN(timestamp) ? Date.now() : timestamp * 1000;

      const conversation: DmConversation = existing
        ? {
            ...existing,
            messages: [...existing.messages, message],
            lastMessageTime: time,
          }
        : {
            username: otherUser,
            messages: [message],
            lastMessageTime: time,
          };

      return {
        conversations: {
          ...state.conversations,
          [otherUser]: conversation,
        },
      };
    }),

  setFriendsOnlyDms: (value) => set({ friendsOnlyDms: value }),
}));
```

- [ ] **Step 3: Verify no TypeScript errors**

Run: `npx tsc --noEmit`
Expected: No errors related to dmStore.

- [ ] **Step 4: Commit**

```bash
git add src/types/index.ts src/stores/dmStore.ts
git commit -m "feat(dm): add DM store and DmMessage type"
```

---

## Task 3: Frontend — DM event routing

**Files:**
- Create: `src/features/dm/useDmEvents.ts`
- Modify: `src/features/chat/useChatEvents.ts`

- [ ] **Step 1: Create `src/features/dm/useDmEvents.ts`**

```typescript
import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useDmStore } from "../../stores/dmStore";
import { useAuthStore } from "../../stores/authStore";

interface MessagePayload {
  context: string;
  sender: string;
  recipient: string;
  content: string;
  timestamp: string;
}

export function useDmEvents() {
  useEffect(() => {
    const unlisten = listen<MessagePayload>("message_received", (event) => {
      if (event.payload.context !== "dm") return;

      const localUsername = useAuthStore.getState().username;
      const otherUser =
        event.payload.sender === localUsername
          ? event.payload.recipient
          : event.payload.sender;

      if (!otherUser) return;

      useDmStore.getState().addDmMessage(otherUser, {
        sender: event.payload.sender,
        content: event.payload.content,
        timestamp: event.payload.timestamp,
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
```

- [ ] **Step 2: Modify `useChatEvents.ts` to skip DM messages**

In `src/features/chat/useChatEvents.ts`, find the `message_received` listener callback and add a guard at the top of the handler:

```typescript
if (event.payload.context === "dm") return;
```

This prevents DMs from being added to `messagesByChannel`.

- [ ] **Step 3: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 4: Commit**

```bash
git add src/features/dm/useDmEvents.ts src/features/chat/useChatEvents.ts
git commit -m "feat(dm): route DM events to dmStore, skip in chatStore"
```

---

## Task 4: Frontend — Add `"dm"` view type and profile popup state to uiStore

**Files:**
- Modify: `src/stores/uiStore.ts`

- [ ] **Step 1: Update activeView type and add profile popup state**

In `src/stores/uiStore.ts`:

1. Add `"dm"` to the `activeView` type (both in the interface and the setter):

```typescript
activeView: "home" | "server" | "browse" | "voice" | "dm";
```

2. Add profile popup state fields:

```typescript
profilePopupUser: string | null;
profilePopupAnchor: { x: number; y: number } | null;
openProfilePopup: (username: string, anchor: { x: number; y: number }) => void;
closeProfilePopup: () => void;
```

3. Add initial values and implementations:

```typescript
profilePopupUser: null,
profilePopupAnchor: null,
openProfilePopup: (username, anchor) =>
  set({ profilePopupUser: username, profilePopupAnchor: anchor }),
closeProfilePopup: () =>
  set({ profilePopupUser: null, profilePopupAnchor: null }),
```

- [ ] **Step 2: Verify no TypeScript errors**

Run: `npx tsc --noEmit`
Expected: May show errors in components that exhaustively check activeView — these will be fixed in later tasks.

- [ ] **Step 3: Commit**

```bash
git add src/stores/uiStore.ts
git commit -m "feat(dm): add dm view type and profile popup state to uiStore"
```

---

## Task 5: Frontend — Create `UserProfilePopup` component

**Files:**
- Create: `src/features/dm/UserProfilePopup.tsx`

- [ ] **Step 1: Create `src/features/dm/UserProfilePopup.tsx`**

```tsx
import { useState, useEffect, useRef } from "react";
import { createPortal } from "react-dom";
import { invoke } from "@tauri-apps/api/core";
import { useUiStore } from "../../stores/uiStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { stringToGradient } from "../../utils/colors";

export default function UserProfilePopup() {
  const username = useUiStore((s) => s.profilePopupUser);
  const anchor = useUiStore((s) => s.profilePopupAnchor);
  const closePopup = useUiStore((s) => s.closeProfilePopup);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const popupRef = useRef<HTMLDivElement>(null);

  // Reset input when popup opens for a different user
  useEffect(() => {
    setInput("");
    setSending(false);
  }, [username]);

  // Close on outside click
  useEffect(() => {
    if (!username) return;
    const handler = (e: MouseEvent) => {
      if (popupRef.current && !popupRef.current.contains(e.target as Node)) {
        closePopup();
      }
    };
    document.addEventListener("mousedown", handler);
    return () => document.removeEventListener("mousedown", handler);
  }, [username, closePopup]);

  // Close on Escape
  useEffect(() => {
    if (!username) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") closePopup();
    };
    document.addEventListener("keydown", handler);
    return () => document.removeEventListener("keydown", handler);
  }, [username, closePopup]);

  if (!username || !anchor) return null;

  const friend = friends.find((f) => f.username === username);
  const isOnline =
    friend?.status === "online" || onlineUsers.includes(username);

  // Clamp popup position to viewport
  const popupWidth = 320;
  const popupHeight = 260;
  const x = Math.min(anchor.x, window.innerWidth - popupWidth - 16);
  const y = Math.min(anchor.y, window.innerHeight - popupHeight - 16);

  const handleSend = async () => {
    if (!input.trim() || sending) return;
    setSending(true);
    try {
      await invoke("send_private_message", {
        recipient: username,
        message: input.trim(),
      });
      setInput("");
      closePopup();
      setActiveDmUser(username);
      setActiveView("dm");
    } catch (err) {
      console.error("DM send failed:", err);
      setSending(false);
    }
  };

  const handleKeyDown = (e: React.KeyboardEvent) => {
    if (e.key === "Enter" && !e.shiftKey) {
      e.preventDefault();
      handleSend();
    }
  };

  return createPortal(
    <div className="fixed inset-0 z-[100]">
      <div
        ref={popupRef}
        className="absolute animate-[fadeUp_0.2s_ease_both] overflow-hidden rounded-2xl border border-border bg-bg-secondary shadow-2xl"
        style={{ left: x, top: y, width: popupWidth }}
      >
        {/* Banner */}
        <div
          className="h-[70px]"
          style={{ background: stringToGradient(username) }}
        />

        {/* Avatar */}
        <div className="px-5">
          <div className="relative -mt-9">
            <div
              className="flex h-[72px] w-[72px] items-center justify-center rounded-xl border-4 border-bg-secondary text-[28px] font-extrabold text-white"
              style={{ background: stringToGradient(username) }}
            >
              {username.charAt(0).toUpperCase()}
            </div>
            <div
              className={`absolute -bottom-0.5 -right-0.5 h-4 w-4 rounded-full border-[3px] border-bg-secondary ${
                isOnline ? "bg-success" : "bg-text-muted"
              }`}
            />
          </div>
        </div>

        {/* Info */}
        <div className="px-5 pt-3">
          <div className="text-lg font-extrabold text-text-bright">
            {username}
          </div>
          <div
            className={`mt-0.5 text-xs font-semibold ${
              isOnline ? "text-success" : "text-text-muted"
            }`}
          >
            {isOnline ? "Online" : "Offline"}
          </div>
        </div>

        {/* Divider */}
        <div className="mx-5 my-3.5 h-px bg-border" />

        {/* Message input */}
        <div className="px-3.5 pb-3.5">
          <div className="flex items-center rounded-xl border border-border bg-bg-tertiary px-3.5 py-2.5 transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
            <input
              type="text"
              value={input}
              onChange={(e) => setInput(e.target.value)}
              onKeyDown={handleKeyDown}
              disabled={sending}
              placeholder={`Message @${username}`}
              className="flex-1 bg-transparent text-sm text-text-primary outline-none placeholder:text-text-muted disabled:opacity-50"
              autoFocus
            />
          </div>
        </div>
      </div>
    </div>,
    document.body
  );
}
```

- [ ] **Step 2: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 3: Commit**

```bash
git add src/features/dm/UserProfilePopup.tsx
git commit -m "feat(dm): add user profile popup component"
```

---

## Task 6: Frontend — Create `DmChatPanel` component

**Files:**
- Create: `src/features/dm/DmChatPanel.tsx`

- [ ] **Step 1: Create `src/features/dm/DmChatPanel.tsx`**

```tsx
import { useState, useRef, useEffect } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import { stringToGradient, stringToColor } from "../../utils/colors";
import MessageBubble, { shouldGroup } from "../chat/MessageBubble";

export default function DmChatPanel() {
  const activeDmUser = useDmStore((s) => s.activeDmUser);
  const conversations = useDmStore((s) => s.conversations);
  const friends = useFriendsStore((s) => s.friends);
  const onlineUsers = useChatStore((s) => s.onlineUsers);

  const [input, setInput] = useState("");
  const [sending, setSending] = useState(false);
  const [sendError, setSendError] = useState<string | null>(null);
  const messagesEndRef = useRef<HTMLDivElement>(null);

  const conversation = activeDmUser
    ? conversations[activeDmUser]
    : null;
  const messages = conversation?.messages ?? [];

  const friend = activeDmUser
    ? friends.find((f) => f.username === activeDmUser)
    : null;
  const isOnline =
    friend?.status === "online" ||
    (activeDmUser ? onlineUsers.includes(activeDmUser) : false);

  // Reset on conversation switch
  useEffect(() => {
    setSendError(null);
    setInput("");
  }, [activeDmUser]);

  // Auto-scroll on new messages
  useEffect(() => {
    messagesEndRef.current?.scrollIntoView({ behavior: "smooth" });
  }, [messages.length]);

  const handleSend = async () => {
    if (!input.trim() || !activeDmUser) return;
    setSending(true);
    setSendError(null);
    try {
      await invoke("send_private_message", {
        recipient: activeDmUser,
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

  // Empty state — no active DM
  if (!activeDmUser) {
    return (
      <div className="flex flex-1 items-center justify-center bg-bg-tertiary">
        <p className="text-sm text-text-muted">
          Select a conversation or start a new one
        </p>
      </div>
    );
  }

  // Map DmMessages to Message shape for MessageBubble compatibility
  const bubbleMessages = messages.map((m) => ({
    ...m,
    channelId: "",
  }));

  return (
    <div className="flex flex-1 flex-col bg-bg-tertiary">
      {/* DM header */}
      <div className="flex h-12 shrink-0 items-center gap-2.5 border-b border-border px-4">
        <div className="relative">
          <div
            className="flex h-[26px] w-[26px] items-center justify-center rounded-lg text-[11px] font-bold text-white"
            style={{ background: stringToGradient(activeDmUser) }}
          >
            {activeDmUser.charAt(0).toUpperCase()}
          </div>
          <div
            className={`absolute -bottom-px -right-px h-2.5 w-2.5 rounded-full border-2 border-bg-tertiary ${
              isOnline ? "bg-success" : "bg-text-muted"
            }`}
          />
        </div>
        <span className="text-[15px] font-bold text-text-bright">
          {activeDmUser}
        </span>
        <span
          className={`text-xs font-medium ${
            isOnline ? "text-success" : "text-text-muted"
          }`}
        >
          {isOnline ? "Online" : "Offline"}
        </span>
      </div>

      {/* Messages */}
      <div className="flex-1 overflow-y-auto px-4 py-4">
        {messages.length === 0 ? (
          <div className="animate-[fadeUp_0.4s_ease_both]">
            <div className="border-b border-border pb-5 mb-5">
              <div
                className="mb-3 flex h-[60px] w-[60px] items-center justify-center rounded-xl text-[26px] font-bold text-white"
                style={{ background: stringToGradient(activeDmUser) }}
              >
                {activeDmUser.charAt(0).toUpperCase()}
              </div>
              <h1 className="mb-1.5 text-[26px] font-extrabold tracking-tight text-text-bright">
                {activeDmUser}
              </h1>
              <p className="text-sm text-text-secondary leading-relaxed">
                This is the beginning of your conversation with{" "}
                <span
                  className="font-semibold"
                  style={{ color: stringToColor(activeDmUser) }}
                >
                  {activeDmUser}
                </span>
                .
              </p>
            </div>
          </div>
        ) : (
          bubbleMessages.map((msg, i) => (
            <MessageBubble
              key={`${msg.timestamp}-${msg.sender}-${i}`}
              message={msg}
              grouped={shouldGroup(
                i > 0 ? bubbleMessages[i - 1] : undefined,
                msg
              )}
            />
          ))
        )}
        <div ref={messagesEndRef} />
      </div>

      {/* Send error */}
      {sendError && (
        <p className="px-4 text-xs text-error">{sendError}</p>
      )}

      {/* Input bar */}
      <div className="px-4 pb-[18px]">
        <div className="flex items-center gap-2.5 rounded-xl border border-border bg-bg-secondary px-3.5 py-[11px] transition-all focus-within:border-accent focus-within:shadow-[0_0_0_2px_var(--color-accent-soft)]">
          <input
            type="text"
            value={input}
            onChange={(e) => setInput(e.target.value)}
            onKeyDown={handleKeyDown}
            disabled={sending}
            placeholder={`Message @${activeDmUser}`}
            className="flex-1 bg-transparent text-sm text-text-primary outline-none placeholder:text-text-muted disabled:opacity-50"
          />
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 3: Commit**

```bash
git add src/features/dm/DmChatPanel.tsx
git commit -m "feat(dm): add DM chat panel component"
```

---

## Task 7: Frontend — Wire up MainLayout for DM view

**Files:**
- Modify: `src/layouts/MainLayout.tsx`

- [ ] **Step 1: Add imports for DM components**

Add at the top of `MainLayout.tsx`:

```typescript
import DmChatPanel from "../features/dm/DmChatPanel";
import UserProfilePopup from "../features/dm/UserProfilePopup";
import { useDmEvents } from "../features/dm/useDmEvents";
```

- [ ] **Step 2: Add `useDmEvents()` hook call**

Inside the `MainLayout` component body, add the hook call alongside other hooks:

```typescript
useDmEvents();
```

- [ ] **Step 3: Update view routing**

Replace the existing view routing JSX. The current structure is:

```tsx
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
```

Replace with:

```tsx
{activeView === "browse" ? (
  <ServerBrowseView />
) : (
  <>
    <ChannelSidebar />
    {activeView === "voice" ? (
      <VoicePanel />
    ) : activeView === "dm" ? (
      <DmChatPanel />
    ) : (
      <>
        <ChatPanel />
        {activeView === "home" ? <FriendsList /> : <MembersList />}
      </>
    )}
  </>
)}
```

- [ ] **Step 4: Add `UserProfilePopup` to the render tree**

Add `<UserProfilePopup />` at the end of the component's return, just before the closing fragment/div. Since it uses a portal, position doesn't matter structurally.

- [ ] **Step 5: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 6: Commit**

```bash
git add src/layouts/MainLayout.tsx
git commit -m "feat(dm): wire DM view and profile popup into MainLayout"
```

---

## Task 8: Frontend — Populate DM sidebar with conversation avatars

**Files:**
- Modify: `src/layouts/DmSidebar.tsx`

- [ ] **Step 1: Add imports and store hooks**

Add imports for `useDmStore`, `useFriendsStore`, `useChatStore`, `useUiStore`, and `stringToGradient`:

```typescript
import { useDmStore } from "../stores/dmStore";
import { useFriendsStore } from "../stores/friendsStore";
import { useChatStore } from "../stores/chatStore";
import { stringToGradient } from "../utils/colors";
```

- [ ] **Step 2: Add store selectors inside the component**

```typescript
const conversations = useDmStore((s) => s.conversations);
const activeDmUser = useDmStore((s) => s.activeDmUser);
const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
const setActiveView = useUiStore((s) => s.setActiveView);
const friends = useFriendsStore((s) => s.friends);
const onlineUsers = useChatStore((s) => s.onlineUsers);
```

- [ ] **Step 3: Create sorted conversation list and click handler**

```typescript
const sortedConversations = Object.values(conversations).sort(
  (a, b) => b.lastMessageTime - a.lastMessageTime
);

const handleDmClick = (username: string) => {
  setActiveDmUser(username);
  setActiveView("dm");
};
```

- [ ] **Step 4: Replace the empty placeholder div with DM avatars**

Find the empty placeholder section:

```tsx
<div className="flex flex-1 flex-col items-center gap-1.5 overflow-y-auto py-1">
  {/* These would map over actual DM contacts from a store */}
</div>
```

Replace with:

```tsx
<div className="flex flex-1 flex-col items-center gap-1.5 overflow-y-auto py-1">
  {sortedConversations.map((conv) => {
    const isOnline =
      friends.some((f) => f.username === conv.username && f.status === "online") ||
      onlineUsers.includes(conv.username);
    const isActive = activeDmUser === conv.username;
    return (
      <button
        key={conv.username}
        onClick={() => handleDmClick(conv.username)}
        className={`relative flex h-[40px] w-[40px] shrink-0 items-center justify-center rounded-xl text-sm font-bold text-white transition-all duration-200 hover:-translate-y-0.5 ${
          isActive
            ? "shadow-[0_0_0_2px_var(--color-accent)]"
            : ""
        }`}
        style={{ background: stringToGradient(conv.username) }}
        title={conv.username}
      >
        {conv.username.charAt(0).toUpperCase()}
        <div
          className={`absolute -bottom-px -right-px h-3 w-3 rounded-full border-[2.5px] border-bg-dmbar ${
            isOnline ? "bg-success" : "bg-text-muted"
          }`}
        />
      </button>
    );
  })}
</div>
```

- [ ] **Step 5: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 6: Commit**

```bash
git add src/layouts/DmSidebar.tsx
git commit -m "feat(dm): populate DM sidebar with conversation avatars"
```

---

## Task 9: Frontend — DM conversation list in Home view (ChannelSidebar)

**Files:**
- Modify: `src/features/channels/ChannelSidebar.tsx`

- [ ] **Step 1: Add imports**

Add to existing imports:

```typescript
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
```

- [ ] **Step 2: Add store selectors in the component**

Inside `ChannelSidebar`, add:

```typescript
const conversations = useDmStore((s) => s.conversations);
const activeDmUser = useDmStore((s) => s.activeDmUser);
const setActiveDmUser = useDmStore((s) => s.setActiveDmUser);
const friends = useFriendsStore((s) => s.friends);
const onlineUsers = useChatStore((s) => s.onlineUsers);
```

- [ ] **Step 3: Add relative timestamp formatter and DM click handler**

```typescript
function formatRelativeTime(epochMs: number): string {
  const diff = Date.now() - epochMs;
  const mins = Math.floor(diff / 60000);
  if (mins < 1) return "now";
  if (mins < 60) return `${mins}m`;
  const hours = Math.floor(mins / 60);
  if (hours < 24) return `${hours}h`;
  const days = Math.floor(hours / 24);
  if (days < 30) return `${days}d`;
  return `${Math.floor(days / 30)}mo`;
}
```

Add inside the component:

```typescript
const sortedConversations = Object.values(conversations).sort(
  (a, b) => b.lastMessageTime - a.lastMessageTime
);

const handleDmConversationClick = (username: string) => {
  setActiveDmUser(username);
  setActiveView("dm");
};
```

- [ ] **Step 4: Update the home view condition to also match `"dm"` view**

In `ChannelSidebar`, find the early return condition:

```typescript
if (activeView === "home") {
```

Change it to:

```typescript
if (activeView === "home" || activeView === "dm") {
```

This ensures the DM conversation list sidebar renders when viewing a DM conversation, not the server channel list.

- [ ] **Step 5: Replace the "Coming soon" placeholder**

Find the home view return block (the one with "Coming soon..."):

```tsx
<div className="flex flex-1 items-center justify-center">
  <p className="text-xs text-text-muted">Coming soon...</p>
</div>
```

Replace with:

```tsx
<div className="flex-1 overflow-y-auto px-2 py-2.5">
  {sortedConversations.length === 0 ? (
    <div className="flex flex-1 items-center justify-center pt-8">
      <p className="text-xs text-text-muted">No conversations yet</p>
    </div>
  ) : (
    sortedConversations.map((conv) => {
      const isOnline =
        friends.some((f) => f.username === conv.username && f.status === "online") ||
        onlineUsers.includes(conv.username);
      const lastMsg = conv.messages[conv.messages.length - 1];
      const isActive = activeDmUser === conv.username;
      return (
        <button
          key={conv.username}
          onClick={() => handleDmConversationClick(conv.username)}
          className={`flex w-full items-center gap-2.5 rounded-lg px-2.5 py-2 transition-colors ${
            isActive
              ? "bg-accent-soft text-text-bright"
              : "text-text-secondary hover:bg-surface-hover hover:text-text-primary"
          }`}
        >
          <div className="relative shrink-0">
            <div
              className="flex h-[34px] w-[34px] items-center justify-center rounded-lg text-sm font-bold text-white"
              style={{ background: stringToGradient(conv.username) }}
            >
              {conv.username.charAt(0).toUpperCase()}
            </div>
            <div
              className={`absolute -bottom-px -right-px h-2.5 w-2.5 rounded-full border-2 border-bg-secondary ${
                isOnline ? "bg-success" : "bg-text-muted"
              }`}
            />
          </div>
          <div className="min-w-0 flex-1 text-left">
            <div className="truncate text-[13px] font-bold">
              {conv.username}
            </div>
            {lastMsg && (
              <div className="truncate text-[11px] text-text-muted">
                {lastMsg.content}
              </div>
            )}
          </div>
          {conv.lastMessageTime > 0 && (
            <span className="shrink-0 text-[10px] text-text-muted">
              {formatRelativeTime(conv.lastMessageTime)}
            </span>
          )}
        </button>
      );
    })
  )}
</div>
```

- [ ] **Step 6: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 7: Commit**

```bash
git add src/features/channels/ChannelSidebar.tsx
git commit -m "feat(dm): show DM conversation list in home view sidebar"
```

---

## Task 10: Frontend — Add click-to-popup on usernames

**Files:**
- Modify: `src/features/chat/MessageBubble.tsx`
- Modify: `src/features/friends/FriendsList.tsx`
- Modify: `src/features/friends/MembersList.tsx`

- [ ] **Step 1: Add popup trigger to MessageBubble sender names**

In `src/features/chat/MessageBubble.tsx`, import `useUiStore`:

```typescript
import { useUiStore } from "../../stores/uiStore";
```

In the `MessageBubble` component, add:

```typescript
const openProfilePopup = useUiStore((s) => s.openProfilePopup);
```

Find the sender name `<span>` in the non-grouped message render (the one with `cursor-pointer`):

```tsx
<span
  className="cursor-pointer text-sm font-bold hover:underline"
  style={{ color: stringToColor(message.sender) }}
>
  {message.sender}
</span>
```

Add an `onClick` handler:

```tsx
<span
  className="cursor-pointer text-sm font-bold hover:underline"
  style={{ color: stringToColor(message.sender) }}
  onClick={(e) => {
    const rect = e.currentTarget.getBoundingClientRect();
    openProfilePopup(message.sender, { x: rect.right + 8, y: rect.top });
  }}
>
  {message.sender}
</span>
```

- [ ] **Step 2: Add popup trigger to FriendsList usernames**

In `src/features/friends/FriendsList.tsx`, import `useUiStore` and add the selector:

```typescript
const openProfilePopup = useUiStore((s) => s.openProfilePopup);
```

Find each friend's username display element and wrap it with a click handler. Look for the username text rendering (the friend's name) and add:

```tsx
onClick={(e) => {
  const rect = e.currentTarget.getBoundingClientRect();
  openProfilePopup(friend.username, { x: rect.right + 8, y: rect.top });
}}
className="... cursor-pointer hover:underline"
```

- [ ] **Step 3: Add popup trigger to MembersList usernames**

Same pattern as FriendsList — in `src/features/friends/MembersList.tsx`, import `useUiStore`, add the selector, and add click handlers on member username elements.

- [ ] **Step 4: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 5: Commit**

```bash
git add src/features/chat/MessageBubble.tsx src/features/friends/FriendsList.tsx src/features/friends/MembersList.tsx
git commit -m "feat(dm): add profile popup trigger on username clicks"
```

---

## Task 11: C++ Server — Add DM privacy enforcement

**Files:**
- Modify: `proto/messages.proto`
- Modify: `src/server/main.cpp`

- [ ] **Step 1: Add DM privacy proto messages**

In `proto/messages.proto`, add a new type to the `Packet.Type` enum (after `VOICE_STATE_NOTIFY = 27`):

```protobuf
DM_PRIVACY = 28;
```

Add a new message (at the end of the file):

```protobuf
message DmPrivacySetting {
  bool friends_only = 1;
}
```

Add to the `Packet` oneof (after `stop_watching_req = 29`):

```protobuf
DmPrivacySetting dm_privacy = 31;
```

- [ ] **Step 2: Rebuild protobuf C++ files**

Run: `cd /home/sun/Desktop/decibell/decibell && mkdir -p build && cd build && cmake .. && make -j$(nproc)`
Expected: Compiles with new proto messages.

- [ ] **Step 3: Add DM privacy state and handler in `main.cpp`**

In the Session class, add a member variable:

```cpp
bool dm_friends_only_ = false;
```

Add a handler for the `DM_PRIVACY` packet type in the packet processing switch:

```cpp
else if (packet.type() == chatproj::Packet::DM_PRIVACY) {
    if (!authenticated_) return;
    dm_friends_only_ = packet.dm_privacy().friends_only();
}
```

In `SessionManager`, add a method to check DM privacy:

```cpp
bool check_dm_allowed(const std::string& sender, const std::string& recipient);
```

This method should:
1. Find the recipient's session
2. Check if their `dm_friends_only_` flag is true
3. If true, check the recipient's friend list for the sender
4. Return true if allowed, false if blocked

- [ ] **Step 4: Add privacy check to DM handler**

In the `DIRECT_MSG` handler, after the authentication check and before calling `send_private`, add:

```cpp
if (!manager_.check_dm_allowed(username_, dmsg->recipient())) {
    chatproj::Packet error_packet;
    error_packet.set_type(chatproj::Packet::DIRECT_MSG);
    auto* err_msg = error_packet.mutable_direct_msg();
    err_msg->set_sender(username_);             // Set sender to the user who tried to send
    err_msg->set_recipient(dmsg->recipient());   // Keep intended recipient
    err_msg->set_content("This user only accepts direct messages from users in their friends list.");
    err_msg->set_timestamp(current_time);

    std::string serialized;
    error_packet.SerializeToString(&serialized);
    auto framed = std::make_shared<std::vector<uint8_t>>(chatproj::create_framed_packet(serialized));
    deliver(framed);
    return;
}
```

Note: By setting `sender = username_` (the local user) and keeping the `recipient`, the frontend's `useDmEvents` will see `sender === localUsername`, compute `otherUser = recipient`, and file the error message under the correct conversation. The error content will appear as a message in that conversation thread.

- [ ] **Step 5: Rebuild and verify**

Run: `cd /home/sun/Desktop/decibell/decibell/build && make -j$(nproc)`
Expected: Compiles with no errors.

- [ ] **Step 6: Commit**

```bash
git add proto/messages.proto src/server/main.cpp
git commit -m "feat(dm): add server-side DM privacy enforcement"
```

---

## Task 12: Rust — Add `set_dm_privacy` Tauri command

**Files:**
- Modify: `src-tauri/src/commands/messaging.rs`
- Modify: `src-tauri/src/net/central.rs`
- Modify: `src-tauri/src/lib.rs`

- [ ] **Step 1: Add `send_dm_privacy` method to central client**

In `src-tauri/src/net/central.rs`, add a method to send the DM privacy packet. Follow the same pattern as `send_private_message` — build a `Packet` with `DM_PRIVACY` type and `DmPrivacySetting` payload, serialize and send.

- [ ] **Step 2: Add `set_dm_privacy` Tauri command**

In `src-tauri/src/commands/messaging.rs`:

```rust
#[tauri::command]
pub async fn set_dm_privacy(
    friends_only: bool,
    state: State<'_, SharedState>,
) -> Result<(), String> {
    let s = state.lock().await;
    let token = s.token.clone();
    match &s.central {
        Some(client) => {
            client.send_dm_privacy(friends_only, token.as_deref()).await
        }
        None => Err("Not connected to central server".to_string()),
    }
}
```

- [ ] **Step 3: Register command in `lib.rs`**

Add `commands::messaging::set_dm_privacy` to the `invoke_handler` macro in `src-tauri/src/lib.rs`.

- [ ] **Step 4: Rebuild Rust protobuf**

Run: `cd src-tauri && cargo build`
Expected: Compiles — `build.rs` regenerates proto bindings automatically.

- [ ] **Step 5: Commit**

```bash
git add src-tauri/src/commands/messaging.rs src-tauri/src/net/central.rs src-tauri/src/lib.rs
git commit -m "feat(dm): add set_dm_privacy Tauri command"
```

---

## Task 13: Frontend — Settings page DM privacy toggle

**Files:**
- Modify: `src/pages/SettingsPage.tsx`

- [ ] **Step 1: Replace the settings stub with a privacy section**

```tsx
import { invoke } from "@tauri-apps/api/core";
import { useDmStore } from "../stores/dmStore";

export default function SettingsPage() {
  const friendsOnlyDms = useDmStore((s) => s.friendsOnlyDms);
  const setFriendsOnlyDms = useDmStore((s) => s.setFriendsOnlyDms);

  const handleToggle = async () => {
    const newValue = !friendsOnlyDms;
    setFriendsOnlyDms(newValue);
    try {
      await invoke("set_dm_privacy", { friendsOnly: newValue });
    } catch (err) {
      console.error("Failed to update DM privacy:", err);
      setFriendsOnlyDms(!newValue); // Revert on error
    }
  };

  return (
    <div className="flex h-full w-full justify-center overflow-y-auto py-10">
      <div className="w-full max-w-lg px-6">
        <h1 className="mb-6 text-2xl font-extrabold text-text-bright">
          Settings
        </h1>

        {/* Privacy section */}
        <div className="mb-8">
          <h2 className="mb-4 text-sm font-bold uppercase tracking-wider text-text-muted">
            Privacy
          </h2>
          <div className="flex items-center justify-between rounded-xl border border-border bg-bg-secondary px-4 py-3.5">
            <div>
              <div className="text-sm font-semibold text-text-primary">
                Only accept DMs from friends
              </div>
              <div className="mt-0.5 text-xs text-text-muted">
                When enabled, only users on your friends list can send you
                direct messages.
              </div>
            </div>
            <button
              onClick={handleToggle}
              className={`relative h-6 w-11 shrink-0 rounded-full transition-colors ${
                friendsOnlyDms ? "bg-accent" : "bg-surface-active"
              }`}
            >
              <div
                className={`absolute top-0.5 h-5 w-5 rounded-full bg-white shadow transition-transform ${
                  friendsOnlyDms ? "translate-x-[22px]" : "translate-x-0.5"
                }`}
              />
            </button>
          </div>
        </div>
      </div>
    </div>
  );
}
```

- [ ] **Step 2: Verify no TypeScript errors**

Run: `npx tsc --noEmit`

- [ ] **Step 3: Commit**

```bash
git add src/pages/SettingsPage.tsx
git commit -m "feat(dm): add DM privacy toggle to settings page"
```

---

## Task 14: Integration — End-to-end smoke test

- [ ] **Step 1: Build the full project**

```bash
cd /home/sun/Desktop/decibell/decibell/build && cmake .. && make -j$(nproc)
cd /home/sun/Desktop/decibell/decibell/tauri-client && npx tsc --noEmit
```

Expected: Both compile without errors.

- [ ] **Step 2: Manual smoke test**

1. Start the central server
2. Launch the Tauri client, log in with two accounts
3. Click a username in the friends list → verify profile popup appears
4. Type a message in the popup and press Enter → verify you're taken to the DM view
5. Verify the message appears in the DM chat
6. Verify the conversation appears in the DM sidebar (leftmost bar)
7. Click Home → verify the conversation appears in the DM conversation list
8. Send a reply from the other account → verify it appears in real-time
9. Go to Settings → toggle "Only accept DMs from friends" on
10. Try sending a DM from a non-friend → verify the error message appears

- [ ] **Step 3: Fix any issues found during smoke testing**

- [ ] **Step 4: Final commit if any fixes were needed**

```bash
git add -A
git commit -m "fix(dm): smoke test fixes"
```
