import { create } from "zustand";
import type { DmMessage } from "../types";

interface DmConversation {
  username: string;
  messages: DmMessage[];
  lastMessageTime: number;
  /// Unread DM count for this peer (messages they sent that the
  /// local user hasn't acked yet). Set from the server's preview on
  /// hydrate; bumped on incoming DIRECT_MSG when the user isn't
  /// actively viewing this conversation; cleared optimistically when
  /// the panel mounts and the next preview confirms.
  unreadCount: number;
  /// Highest message id the user has marked-read up through.
  /// Drives the `up_to_id` argument of mark_dm_read.
  lastReadId: number;
  /// Server says there are older messages available before the
  /// oldest currently-loaded one. Drives the scroll-up paginator.
  hasMoreHistory: boolean;
  /// Set to true once we've received a DmHistoryRes for this peer.
  /// `false` means "messages[] is purely from live events; no
  /// server hydration yet". Drives the on-mount fetch decision.
  historyLoaded: boolean;
}

interface ConversationPreviewInput {
  peer: string;
  lastMessageContent: string;
  lastMessageSender: string;
  lastMessageId: number;
  lastTimestamp: number;
  unreadCount: number;
}

interface HistoryMessageInput {
  id: number;
  sender: string;
  content: string;
  timestamp: number;
}

interface DmState {
  conversations: Record<string, DmConversation>;
  activeDmUser: string | null;
  friendsOnlyDms: boolean;
  /// Per-peer snapshot of optimistically-removed DM messages awaiting
  /// the server ack. Mirror of chatStore.pendingDeletions for DMs.
  pendingDmDeletions: Record<string, Map<number, DmMessage>>;

  setActiveDmUser: (username: string | null) => void;
  addDmMessage: (otherUser: string, message: DmMessage, isFromSelf: boolean) => void;
  setFriendsOnlyDms: (value: boolean) => void;
  /// Replace conversation previews with the server-truth list from
  /// DmConversationsRes. Per peer: keep the existing `messages` array
  /// (live DMs from this session may already be there), reset
  /// unreadCount + lastTimestamp from the server, leave
  /// historyLoaded alone (still false until the user opens the
  /// conversation and request_dm_history responds).
  hydrateConversations: (previews: ConversationPreviewInput[]) => void;
  /// Merge a DmHistoryRes page into a peer's conversation, ordered
  /// oldest→newest in memory. Dedupes by id against existing
  /// messages. Sets hasMoreHistory + flips historyLoaded to true.
  appendHistory: (
    peer: string,
    messages: HistoryMessageInput[],
    hasMore: boolean,
  ) => void;
  /// Optimistically zero the unread count and bump lastReadId.
  /// Called from DmChatPanel when the conversation becomes visible.
  markRead: (peer: string, upToId: number) => void;
  /// Remove a DM from a peer's visible message list. Idempotent.
  removeDmMessage: (peer: string, messageId: number) => void;
  /// Snapshot + remove for optimistic delete; returns the snapshot.
  snapshotAndRemoveDm: (peer: string, messageId: number) => DmMessage | undefined;
  /// Re-insert a snapshotted DM (rejection path). Sorted by id.
  restorePendingDmDeletion: (peer: string, messageId: number) => void;
  /// Drop the pending snapshot (success-ack or matching broadcast).
  clearPendingDmDeletion: (peer: string, messageId: number) => void;
}

function emptyConversation(username: string): DmConversation {
  return {
    username,
    messages: [],
    lastMessageTime: 0,
    unreadCount: 0,
    lastReadId: 0,
    hasMoreHistory: false,
    historyLoaded: false,
  };
}

export const useDmStore = create<DmState>((set) => ({
  conversations: {},
  activeDmUser: null,
  friendsOnlyDms: false,
  pendingDmDeletions: {},

  setActiveDmUser: (username) => set({ activeDmUser: username }),

  addDmMessage: (otherUser, message, isFromSelf) =>
    set((state) => {
      const existing = state.conversations[otherUser];
      const timestamp = parseInt(message.timestamp, 10);
      const time = isNaN(timestamp) ? Date.now() : timestamp * 1000;

      const isViewing = state.activeDmUser === otherUser;
      const baseUnread = existing?.unreadCount ?? 0;
      // Don't bump unread for self-sent or for the conversation
      // we're actively reading. DmChatPanel's mark-read effect
      // clears the count when the panel is visible.
      const newUnread = isFromSelf || isViewing ? baseUnread : baseUnread + 1;

      const conversation: DmConversation = existing
        ? {
            ...existing,
            messages: [...existing.messages, message],
            lastMessageTime: time,
            unreadCount: newUnread,
          }
        : {
            ...emptyConversation(otherUser),
            messages: [message],
            lastMessageTime: time,
            unreadCount: newUnread,
          };

      return {
        conversations: {
          ...state.conversations,
          [otherUser]: conversation,
        },
      };
    }),

  setFriendsOnlyDms: (value) => set({ friendsOnlyDms: value }),

  hydrateConversations: (previews) =>
    set((state) => {
      const next = { ...state.conversations };
      for (const p of previews) {
        const existing = next[p.peer];
        // Server's last_timestamp is Unix seconds; the sidebar's
        // formatRelativeTime + the rest of the store work in ms.
        const lastMessageTimeMs = p.lastTimestamp * 1000;
        // Synthesize a single placeholder message from the preview
        // so the sidebar can render last-message content + timestamp
        // immediately, before the user clicks into the conversation
        // and triggers request_dm_history. When the full history
        // page arrives, appendHistory's id-based dedup removes this
        // entry (the server's message with the same id replaces it).
        // Skip synthesis if the conversation already has messages
        // in-memory from this session.
        const previewMessage: DmMessage = {
          id: p.lastMessageId,
          sender: p.lastMessageSender,
          content: p.lastMessageContent,
          timestamp: String(p.lastTimestamp),
        };
        const hasInMemoryMessages = (existing?.messages.length ?? 0) > 0;
        next[p.peer] = {
          username: p.peer,
          messages: hasInMemoryMessages
            ? existing!.messages
            : [previewMessage],
          lastMessageTime: lastMessageTimeMs,
          unreadCount: p.unreadCount,
          lastReadId: existing?.lastReadId ?? 0,
          // Server has at least the preview message; assume there
          // may be older ones until the first history page resolves
          // (which sets hasMoreHistory authoritatively from the
          // server's flag).
          hasMoreHistory: true,
          historyLoaded: existing?.historyLoaded ?? false,
        };
      }
      return { conversations: next };
    }),

  appendHistory: (peer, messages, hasMore) =>
    set((state) => {
      const conv = state.conversations[peer];
      const existing = conv?.messages ?? [];
      const existingIds = new Set<number>();
      for (const m of existing) {
        if (typeof m.id === "number" && m.id > 0) existingIds.add(m.id);
      }
      // History page is newest-first per the protocol; flip to
      // oldest-first to match in-memory ordering, then dedupe.
      const incoming: DmMessage[] = [...messages]
        .reverse()
        .filter((m) => !existingIds.has(m.id))
        .map((m) => ({
          sender: m.sender,
          content: m.content,
          // DmMessage.timestamp is a string in the renderer's wire
          // shape (matches DIRECT_MSG event payload). Convert here.
          timestamp: String(m.timestamp),
          id: m.id,
        }));
      const merged: DmMessage[] = [...incoming, ...existing];
      const lastMessageTime =
        merged.length > 0
          ? Math.max(
              ...merged.map((m) => {
                const t = parseInt(m.timestamp, 10);
                return isNaN(t) ? 0 : t * 1000;
              }),
            )
          : conv?.lastMessageTime ?? 0;
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...emptyConversation(peer),
            ...conv,
            username: peer,
            messages: merged,
            lastMessageTime,
            hasMoreHistory: hasMore,
            historyLoaded: true,
          },
        },
      };
    }),

  markRead: (peer, upToId) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      if (upToId <= conv.lastReadId) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            unreadCount: 0,
            lastReadId: upToId,
          },
        },
      };
    }),

  removeDmMessage: (peer, messageId) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      const next = conv.messages.filter((m) => m.id !== messageId);
      if (next.length === conv.messages.length) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: { ...conv, messages: next },
        },
      };
    }),

  snapshotAndRemoveDm: (peer, messageId) => {
    const state = useDmStore.getState();
    const conv = state.conversations[peer];
    if (!conv) return undefined;
    const snap = conv.messages.find((m) => m.id === messageId);
    if (!snap) return undefined;
    useDmStore.setState((s) => {
      const bucket = s.pendingDmDeletions[peer] ?? new Map<number, DmMessage>();
      const next = new Map(bucket);
      next.set(messageId, snap);
      const updatedConv = s.conversations[peer];
      if (!updatedConv) return {};
      return {
        pendingDmDeletions: {
          ...s.pendingDmDeletions,
          [peer]: next,
        },
        conversations: {
          ...s.conversations,
          [peer]: {
            ...updatedConv,
            messages: updatedConv.messages.filter((m) => m.id !== messageId),
          },
        },
      };
    });
    return snap;
  },

  restorePendingDmDeletion: (peer, messageId) =>
    set((state) => {
      const bucket = state.pendingDmDeletions[peer];
      const snap = bucket?.get(messageId);
      if (!snap) return {};
      const conv = state.conversations[peer];
      if (!conv) return {};
      // Re-insert by id ascending. messages are stored oldest-first;
      // linear scan is fine (50-200 messages typically).
      const restored: DmMessage[] = [];
      let inserted = false;
      const snapId = snap.id ?? 0;
      for (const m of conv.messages) {
        const mid = typeof m.id === "number" ? m.id : 0;
        if (!inserted && mid > snapId) {
          restored.push(snap);
          inserted = true;
        }
        restored.push(m);
      }
      if (!inserted) restored.push(snap);

      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDmDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[peer];
      } else {
        nextPending[peer] = nextBucket;
      }
      return {
        conversations: {
          ...state.conversations,
          [peer]: { ...conv, messages: restored },
        },
        pendingDmDeletions: nextPending,
      };
    }),

  clearPendingDmDeletion: (peer, messageId) =>
    set((state) => {
      const bucket = state.pendingDmDeletions[peer];
      if (!bucket || !bucket.has(messageId)) return {};
      const nextBucket = new Map(bucket);
      nextBucket.delete(messageId);
      const nextPending = { ...state.pendingDmDeletions };
      if (nextBucket.size === 0) {
        delete nextPending[peer];
      } else {
        nextPending[peer] = nextBucket;
      }
      return { pendingDmDeletions: nextPending };
    }),
}));
