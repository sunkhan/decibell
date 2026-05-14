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
        next[p.peer] = {
          username: p.peer,
          messages: existing?.messages ?? [],
          lastMessageTime: p.lastTimestamp,
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
}));
