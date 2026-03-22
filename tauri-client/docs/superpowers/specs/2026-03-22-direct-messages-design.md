# Direct Messages Feature — Design Spec

## Overview

Add direct messaging between users via the central server. Users can DM friends and anyone who shares a community server. A user-controlled setting allows restricting DMs to friends only, enforced server-side.

Messages are session-only for this iteration — no database persistence. Server-side persistence is a planned follow-up.

## Scope

### In scope
- User profile popup (click any username anywhere in the app)
- Sending/receiving DMs in real-time
- DM conversation list in Home view (channel sidebar)
- DM contact avatars in the leftmost DM sidebar
- DM chat view reusing existing ChatPanel layout
- "Only accept DMs from friends" setting (server-side enforcement)
- Starting conversations from the profile popup's message input

### Out of scope (future work)
- Server-side message persistence (PostgreSQL storage)
- Offline message queuing
- Read receipts / typing indicators
- Group DMs
- File/image attachments in DMs

---

## Architecture

### Existing infrastructure (already working)

| Layer | What exists |
|-------|-------------|
| **Protocol** | `DIRECT_MSG = 6` packet type, `DirectMessage { sender, recipient, content, timestamp }` in `messages.proto` |
| **C++ server** | Receives DMs, enforces sender identity, routes to online recipient, returns error if offline |
| **Tauri command** | `send_private_message(recipient, message)` in Rust |
| **Network** | Sends DirectMessage packets, emits `message_received` event with `context = "dm"` |

### What needs to change

#### 1. C++ Server — DM privacy setting enforcement

The server needs to check a per-user "friends only DMs" preference before delivering a direct message.

**Approach:** Add a new packet type for setting/querying the DM privacy preference. When a DM arrives, the server checks if the recipient has friends-only mode enabled. If so, it checks the recipient's friend list — if the sender is not on it, the server sends back an error: "This user only accepts direct messages from users in their friends list."

**New proto messages:**
- `DM_PRIVACY_SETTING = <new_type>` — Client sends to set/get preference
- `DmPrivacySetting { bool friends_only }` — Payload

**Server logic change in DM handler:**
```
1. Receive DIRECT_MSG
2. Look up recipient's DM privacy setting (in-memory map, defaults to false)
3. If friends_only == true:
   a. Check if sender is in recipient's friend list
   b. If not: send error back to sender, do not deliver
4. Otherwise: deliver as normal
```

The privacy setting lives in memory (per-session). When persistence is added later, it will be stored in PostgreSQL.

#### 2. Frontend — New DM store (`dmStore.ts`)

Manages DM conversations separately from channel messages.

```typescript
interface DmConversation {
  username: string;        // The other user
  messages: Message[];     // Conversation history
  lastMessageTime: number; // For sorting conversations
}

interface DmState {
  conversations: Record<string, DmConversation>; // Keyed by username
  activeDmUser: string | null;                    // Currently open conversation
  friendsOnlyDms: boolean;                        // Privacy setting

  // Actions
  setActiveDmUser: (username: string | null) => void;
  addDmMessage: (sender: string, recipient: string, content: string, timestamp: string) => void;
  setFriendsOnlyDms: (value: boolean) => void;
}
```

**Key behavior:**
- `addDmMessage` determines the conversation key from the "other user" (if sender is us, key = recipient; otherwise key = sender)
- Auto-creates conversation entry on first message
- Updates `lastMessageTime` for sorting

#### 3. Tauri event payload — Add `recipient` field

The current `MessageReceivedPayload` in `src-tauri/src/events/mod.rs` has `{ context, sender, content, timestamp }` but no `recipient`. For incoming DMs, we can infer recipient = us. But for outgoing DM echoes (server sends our message back), `sender` = us and we need `recipient` to know which conversation it belongs to.

**Fix:** Add `recipient: String` to `MessageReceivedPayload`. Populate it from `DirectMessage.recipient` when `context = "dm"`. Leave empty for channel messages.

#### 4. Frontend — Event handling changes

The `message_received` event with `context = "dm"` currently dumps all DMs into `messagesByChannel["dm"]`. This needs to route to the DM store instead.

**In `useChatEvents.ts`:**
```
if (event.payload.context === "dm") {
  const localUsername = useAuthStore.getState().username;
  const otherUser = event.payload.sender === localUsername
    ? event.payload.recipient   // Outgoing echo — file under recipient
    : event.payload.sender;     // Incoming — file under sender
  dmStore.addDmMessage(otherUser, content, timestamp, sender);
} else {
  // Existing channel message handling
  chatStore.addMessage(...);
}
```

**Message strategy:** Server-echo only (no optimistic updates), consistent with existing channel chat. The server echoes sent DMs back to the sender, which adds them to the conversation.

#### 5. Frontend — User Profile Popup component

A floating popup triggered by clicking any username in:
- Chat message sender names
- Friends list usernames
- Members list usernames

**Component: `UserProfilePopup.tsx`**

**Layout:**
- Gradient banner at top (using `stringToGradient(username)`)
- Large avatar (72px, rounded-lg) overlapping the banner, with online status dot
- Username in bold
- Online/Offline status text
- Divider
- Message input: "Message @username" placeholder
- Enter sends the message, closes popup, navigates to DM view

**Positioning:** Use `getBoundingClientRect()` on the clicked element to get anchor coordinates. Render via a React portal at the document root. Clamp position to viewport bounds so the popup never goes off-screen (prefer appearing to the right of the click, fall back to left if near right edge; prefer below, fall back to above if near bottom). Clicking outside or pressing Escape closes it.

**State management:** Add to `uiStore`:
```typescript
profilePopupUser: string | null;
profilePopupAnchor: { x: number; y: number } | null;
openProfilePopup: (username: string, anchor: { x: number; y: number }) => void;
closeProfilePopup: () => void;
```

#### 6. Frontend — DM conversation list in Home view

When `activeView === "home"`, the channel sidebar currently shows "Direct Messages" header and "Coming soon...". Replace with:

**Conversation list items showing:**
- Avatar (34px, rounded-lg) with online status dot
- Username (bold)
- Last message preview (truncated, muted text)
- Relative timestamp (2m, 1h, 3d, etc.)
- Selected conversation highlighted with accent-soft background

**Relative timestamps:** Roll a simple formatter (no library) — "now" for <1m, "Xm" for minutes, "Xh" for hours, "Xd" for days, then date for older.

**Sorted by `lastMessageTime` descending** (most recent at top).

Clicking a conversation sets `activeDmUser` and switches to DM chat view.

#### 7. Frontend — DM avatars in DM sidebar (leftmost bar)

The DM sidebar (`DmSidebar.tsx`) currently has empty placeholder slots. Populate with:

- Rounded-rectangle avatars (40px) for each active conversation
- Online status dots
- Selected conversation gets accent ring (`shadow-[0_0_0_2px_var(--color-accent)]`)
- Clicking navigates to that DM conversation

**Order matches the conversation list** (most recent first).

#### 8. Frontend — DM chat view

Create a new `DmChatPanel.tsx` component (not modifying `ChatPanel`) since DMs have different data sources and send mechanics.

**Header:** Recipient's avatar (small, rounded-lg), username (bold), online status dot + text. No search/sidebar toggle buttons.

**Message area:** Same as channel chat — uses `MessageBubble` with grouping, timestamps, etc. Messages sourced from `dmStore.conversations[activeDmUser].messages`.

**Input:** Placeholder says "Message @username". Calls `invoke("send_private_message", { recipient, message })`.

**View routing in MainLayout:** Add `"dm"` to the `activeView` union type in `uiStore`. When `activeView === "dm"`:
- Left sidebar: `ChannelSidebar` renders in home-style mode (showing DM conversation list)
- Center: `DmChatPanel` renders instead of `ChatPanel`
- Right panel: hidden (no members list for DMs)

#### 9. Frontend — Settings toggle

The current `SettingsPage.tsx` is a stub ("coming in Phase 6"). Add a minimal "Privacy" section with the DM toggle:

**"Only accept DMs from friends"** — Toggle switch, sends the preference to the server via a new Tauri command (`set_dm_privacy`). Default: off. This is the first real settings entry; keep the section minimal and expandable.

---

## UI State Flow

```
Click username anywhere
  → Open UserProfilePopup at click position
  → Type message + Enter
  → send_private_message(recipient, message)
  → Close popup
  → Set activeDmUser = recipient
  → Set activeView = "dm"
  → Conversation appears in DM sidebar + Home view list

Click DM avatar in sidebar OR conversation in Home list
  → Set activeDmUser = username
  → Set activeView = "dm"
  → ChatPanel renders in DM mode
```

## New/Modified Files

### New files
- `src/stores/dmStore.ts` — DM conversation state
- `src/features/dm/UserProfilePopup.tsx` — Profile popup component
- `src/features/dm/DmChatPanel.tsx` — DM-specific chat panel

### Modified files
- `src/stores/uiStore.ts` — Add `"dm"` view type, profile popup state
- `src-tauri/src/events/mod.rs` — Add `recipient` field to `MessageReceivedPayload`
- `src-tauri/src/net/central.rs` — Populate `recipient` field for DM events
- `src/features/chat/useChatEvents.ts` — Route DM events to dmStore
- `src/layouts/DmSidebar.tsx` — Populate with DM conversation avatars
- `src/features/channels/ChannelSidebar.tsx` — Show DM list in home view
- `src/layouts/MainLayout.tsx` — Handle `"dm"` view routing
- `src/pages/SettingsPage.tsx` — Add DM privacy toggle
- `src-tauri/src/commands/messaging.rs` — Add `set_dm_privacy` command
- `src-tauri/src/net/central.rs` — Handle DM privacy packet
- `proto/messages.proto` — Add DM privacy message type
- `src/server/main.cpp` — Enforce DM privacy setting

## Error Handling

- **Recipient offline:** Server already sends error → display as system message in the DM conversation
- **DMs restricted:** Server sends "This user only accepts direct messages from users in their friends list." → display as system message
- **Send failure:** Show error below input (same pattern as channel chat)

## Design Tokens

All UI follows existing theme variables from `globals.css`:
- Popup background: `bg-bg-secondary` with `border-border`
- Avatar gradients: `stringToGradient(username)`
- Username colors: `stringToColor(username)`
- Status dot: `bg-success` (online), `bg-text-muted` (offline)
- Selection: `bg-accent-soft`, accent ring
- Animations: `fadeUp` entry animation on popup
