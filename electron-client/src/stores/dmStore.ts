import { create } from "zustand";
import type { DmMessage } from "../types";
import { useUiStore } from "./uiStore";

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
  /// Newer messages exist below the loaded window — true only after a
  /// jump-to-message (around) / downward (after) fetch leaves the view NOT at
  /// the live bottom. Drives the "jump to present" pill, downward pagination,
  /// and the live-append guard (an incoming DM while true is dropped, not
  /// stitched past the gap). Mirrors chatStore.hasMoreAfter for channels.
  hasMoreAfter: boolean;
  /// Set to true once we've received a DmHistoryRes for this peer.
  /// `false` means "messages[] is purely from live events; no
  /// server hydration yet". Drives the on-mount fetch decision.
  historyLoaded: boolean;
  /// When this conversation entry first appeared locally. Only used to
  /// order a conversation that has no messages yet — see
  /// conversationActivityTime.
  createdAt: number;
  /// Newest message the client has seen for this peer (highest id) — the
  /// sidebar preview. Kept apart from `messages` because the loaded slice
  /// is a WINDOW: a jump replaces it with an older context window and
  /// RealMessageList trims its tail, so messages[last] is not "the latest".
  lastMessage: DmMessage | null;
}

function idOf(m: DmMessage | null | undefined): number {
  return m && typeof m.id === "number" ? m.id : 0;
}

/// The newer of two messages by id; ties (id-less rows) go to the candidate.
function newerOf(current: DmMessage | null, candidate: DmMessage | null | undefined): DmMessage | null {
  if (!candidate) return current;
  if (!current) return candidate;
  return idOf(candidate) >= idOf(current) ? candidate : current;
}

function newestOf(list: DmMessage[]): DmMessage | null {
  let best: DmMessage | null = null;
  for (const m of list) best = newerOf(best, m);
  return best;
}

/// Sidebar ordering key. A conversation you just opened with someone
/// you've never messaged has lastMessageTime 0, which sorted it to the
/// bottom of the list — the opposite of what opening it implies. Empty
/// conversations fall back to when they appeared, so a fresh one lands
/// at the top and then ages naturally as other conversations get
/// messages. `||` rather than Math.max on purpose: lastMessageTime is
/// non-zero exactly when there are messages, so a real conversation
/// never sorts by the moment its entry happened to be created.
export function conversationActivityTime(c: {
  lastMessageTime: number;
  createdAt: number;
}): number {
  return c.lastMessageTime || c.createdAt;
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
  editedAt?: number;
  replyTo?: number;
  replyToSender?: string;
  replyToContent?: string;
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
  /// Jump-to-message: REPLACE a peer's loaded messages with a context window
  /// (drops the preview placeholder / stale live tail), setting both the
  /// older and newer "has more" flags. Input may be any order — sorted by id.
  setDmWindow: (
    peer: string,
    messages: HistoryMessageInput[],
    hasMoreOlder: boolean,
    hasMoreNewer: boolean,
  ) => void;
  /// Downward pagination: append a newer page (oldest→newest), updating
  /// hasMoreAfter. Dedupes by id.
  appendNewerDm: (
    peer: string,
    messages: HistoryMessageInput[],
    hasMoreNewer: boolean,
  ) => void;
  /// Jump-to-present: clear the loaded slice so the caller can reload the
  /// newest page from scratch (hasMoreAfter → false, historyLoaded → false).
  resetDmForJump: (peer: string) => void;
  /// Sliding-window trims (RealMessageList) — mirror of chatStore's
  /// trimTail/trimHead on the per-conversation flags. trimDmTail keeps the
  /// oldest `keep` rows and flips hasMoreAfter → true (the dropped tail is
  /// re-fetchable via after_id; live DMs are dropped past the gap and the
  /// "jump to present" pill shows). No-op while a row lacks a real id and
  /// when nothing would drop.
  trimDmTail: (peer: string, keep: number) => void;
  /// Keeps the newest `keep` rows and flips hasMoreHistory → true.
  trimDmHead: (peer: string, keep: number) => void;
  /// Optimistically zero the unread count and bump lastReadId.
  /// Called from DmChatPanel when the conversation becomes visible.
  markRead: (peer: string, upToId: number) => void;
  /// Remove a DM from a peer's visible message list. Idempotent.
  removeDmMessage: (peer: string, messageId: number) => void;
  /// Drop an optimistic (id-less) DM by its nonce — the send failed or
  /// was never confirmed.
  removeDmMessageByNonce: (peer: string, nonce: string) => void;
  /// Apply a DM edit broadcast: replace content + set editedAt on the match.
  applyDmEdit: (peer: string, messageId: number, content: string, editedAt: number) => void;
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
    hasMoreAfter: false,
    historyLoaded: false,
    createdAt: Date.now(),
    lastMessage: null,
  };
}

export const useDmStore = create<DmState>((set, get) => ({
  conversations: {},
  activeDmUser: null,
  friendsOnlyDms: false,
  pendingDmDeletions: {},

  setActiveDmUser: (username) => set({ activeDmUser: username }),

  addDmMessage: (otherUser, message, isFromSelf) =>
    set((state) => {
      const existing = state.conversations[otherUser];
      // Dedup replayed live messages (reconnect / at-least-once delivery):
      // a real-id message we've already inserted must not create a
      // duplicate bubble or double-bump the unread count.
      // message.id is truthy only for real (server-assigned) ids; 0 /
      // undefined are optimistic and handled by nonce reconciliation.
      if (message.id && existing?.messages.some((m) => m.id === message.id)) {
        return {};
      }
      // While viewing a jumped/windowed slice (newer messages hidden below),
      // a live real-id DM belongs past the gap — drop it to keep the window
      // contiguous. It loads when the user returns to present / pages down.
      // No optimistic send reaches here in that state: the composer snaps to
      // present before sending.
      if (existing?.hasMoreAfter && typeof message.id === "number" && message.id > 0) {
        return {};
      }
      const timestamp = parseInt(message.timestamp, 10);
      const time = isNaN(timestamp) ? Date.now() : timestamp * 1000;

      // Central's echo of our own optimistic send: replace that bubble
      // in place (it sits at the tail) instead of appending a second
      // row. Unread and preview bookkeeping are unaffected — it's ours.
      if (message.nonce && existing) {
        const idx = existing.messages.findIndex((m) => !m.id && m.nonce === message.nonce);
        if (idx !== -1) {
          const msgs = existing.messages.slice();
          const was = msgs[idx];
          msgs[idx] = message;
          return {
            conversations: {
              ...state.conversations,
              [otherUser]: {
                ...existing,
                messages: msgs,
                lastMessage: existing.lastMessage === was ? message : existing.lastMessage,
                lastMessageTime: Math.max(existing.lastMessageTime, time),
              },
            },
          };
        }
      }

      // "Actively viewing" requires BOTH: the active DM peer is this
      // sender AND the active view is the DM view. activeDmUser is
      // sticky across view changes (same intentional pattern as
      // activeServerId), so checking it alone falsely treats DMs
      // from your last-opened peer as "you're reading them" while
      // you're in a community server / home / browse view — and the
      // unread badge never bumps. Reading activeView from uiStore
      // here keeps the gate in one place.
      const isInDmView = useUiStore.getState().activeView === "dm";
      const isViewing = isInDmView && state.activeDmUser === otherUser;
      const baseUnread = existing?.unreadCount ?? 0;
      // Don't bump unread for self-sent or for the conversation
      // we're actively reading. DmChatPanel's mark-read effect
      // clears the count when the panel is visible.
      const newUnread = isFromSelf || isViewing ? baseUnread : baseUnread + 1;

      // A live arrival is the newest by definition (even id-less legacy ones).
      const conversation: DmConversation = existing
        ? {
            ...existing,
            messages: [...existing.messages, message],
            lastMessageTime: time,
            lastMessage: message,
            unreadCount: newUnread,
          }
        : {
            ...emptyConversation(otherUser),
            messages: [message],
            lastMessageTime: time,
            lastMessage: message,
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
          hasMoreAfter: existing?.hasMoreAfter ?? false,
          historyLoaded: existing?.historyLoaded ?? false,
          lastMessage: newerOf(existing?.lastMessage ?? null, previewMessage),
          // Hydrated conversations always carry a real last message, so
          // this is never consulted — 0 keeps it out of the way.
          createdAt: existing?.createdAt ?? 0,
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
          editedAt: m.editedAt || undefined,
          replyTo: m.replyTo || undefined,
          replyToSender: m.replyToSender || undefined,
          replyToContent: m.replyToContent || undefined,
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
            lastMessage: newerOf(conv?.lastMessage ?? null, newestOf(incoming)),
            hasMoreHistory: hasMore,
            historyLoaded: true,
          },
        },
      };
    }),

  setDmWindow: (peer, messages, hasMoreOlder, hasMoreNewer) =>
    set((state) => {
      const conv = state.conversations[peer];
      const list: DmMessage[] = messages
        .map((m) => ({
          sender: m.sender,
          content: m.content,
          timestamp: String(m.timestamp),
          id: m.id,
          editedAt: m.editedAt || undefined,
          replyTo: m.replyTo || undefined,
          replyToSender: m.replyToSender || undefined,
          replyToContent: m.replyToContent || undefined,
        }))
        .sort((a, b) => (a.id ?? 0) - (b.id ?? 0));
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...emptyConversation(peer),
            ...conv,
            username: peer,
            // Replace wholesale — a jump window is a fresh, discontinuous
            // slice. lastMessageTime is left as-is (the window may not include
            // the newest message; don't reorder the sidebar downward).
            messages: list,
            lastMessage: newerOf(conv?.lastMessage ?? null, newestOf(list)),
            hasMoreHistory: hasMoreOlder,
            hasMoreAfter: hasMoreNewer,
            historyLoaded: true,
          },
        },
      };
    }),

  appendNewerDm: (peer, messages, hasMoreNewer) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      const existingIds = new Set<number>();
      for (const m of conv.messages) {
        if (typeof m.id === "number" && m.id > 0) existingIds.add(m.id);
      }
      const incoming: DmMessage[] = messages
        .filter((m) => !existingIds.has(m.id))
        .map((m) => ({
          sender: m.sender,
          content: m.content,
          timestamp: String(m.timestamp),
          id: m.id,
          editedAt: m.editedAt || undefined,
          replyTo: m.replyTo || undefined,
          replyToSender: m.replyToSender || undefined,
          replyToContent: m.replyToContent || undefined,
        }));
      const merged =
        incoming.length > 0
          ? [...conv.messages, ...incoming].sort((a, b) => (a.id ?? 0) - (b.id ?? 0))
          : conv.messages;
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            messages: merged,
            hasMoreAfter: hasMoreNewer,
            lastMessage: newerOf(conv.lastMessage, newestOf(incoming)),
          },
        },
      };
    }),

  resetDmForJump: (peer) =>
    set((state) => {
      const conv = state.conversations[peer];
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...emptyConversation(peer),
            ...conv,
            username: peer,
            messages: [],
            hasMoreHistory: true,
            hasMoreAfter: false,
            historyLoaded: false,
          },
        },
      };
    }),

  trimDmTail: (peer, keep) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv || keep <= 0 || keep >= conv.messages.length) return {};
      if (conv.messages.some((m) => !(typeof m.id === "number" && m.id > 0))) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: { ...conv, messages: conv.messages.slice(0, keep), hasMoreAfter: true },
        },
      };
    }),

  trimDmHead: (peer, keep) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv || keep <= 0 || keep >= conv.messages.length) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            messages: conv.messages.slice(conv.messages.length - keep),
            hasMoreHistory: true,
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

  removeDmMessageByNonce: (peer, nonce) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      const next = conv.messages.filter((m) => !(!m.id && m.nonce === nonce));
      if (next.length === conv.messages.length) return {};
      const last = conv.lastMessage;
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            messages: next,
            lastMessage:
              last && !last.id && last.nonce === nonce ? next[next.length - 1] ?? null : last,
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
          [peer]: {
            ...conv,
            messages: next,
            // Deleted the newest → best effort: the loaded tail.
            lastMessage:
              idOf(conv.lastMessage) === messageId ? next[next.length - 1] ?? null : conv.lastMessage,
          },
        },
      };
    }),

  applyDmEdit: (peer, messageId, content, editedAt) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return {};
      let changed = false;
      const next = conv.messages.map((m) => {
        if (m.id !== messageId) return m;
        changed = true;
        return { ...m, content, editedAt };
      });
      if (!changed) return {};
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            messages: next,
            lastMessage:
              idOf(conv.lastMessage) === messageId && conv.lastMessage
                ? { ...conv.lastMessage, content, editedAt }
                : conv.lastMessage,
          },
        },
      };
    }),

  snapshotAndRemoveDm: (peer, messageId) => {
    const state = get();
    const conv = state.conversations[peer];
    if (!conv) return undefined;
    const snap = conv.messages.find((m) => m.id === messageId);
    if (!snap) return undefined;
    set((s) => {
      const bucket = s.pendingDmDeletions[peer] ?? new Map<number, DmMessage>();
      const next = new Map(bucket);
      next.set(messageId, snap);
      const updatedConv = s.conversations[peer];
      if (!updatedConv) return {};
      const remaining = updatedConv.messages.filter((m) => m.id !== messageId);
      return {
        pendingDmDeletions: {
          ...s.pendingDmDeletions,
          [peer]: next,
        },
        conversations: {
          ...s.conversations,
          [peer]: {
            ...updatedConv,
            messages: remaining,
            lastMessage:
              idOf(updatedConv.lastMessage) === messageId
                ? remaining[remaining.length - 1] ?? null
                : updatedConv.lastMessage,
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
          [peer]: { ...conv, messages: restored, lastMessage: newerOf(conv.lastMessage, snap) },
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
