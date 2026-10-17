import { create } from "zustand";
import type { DmMessage } from "../types";
import { mergeStoredPage } from "../lib/historyMerge";
import { useAuthStore } from "./authStore";
import { useUiStore } from "./uiStore";
import { nameOf, toUid, type Uid } from "./usersStore";

/// The conversation whose DmChatPanel is mounted right now, or null — the
/// DM twin of chatStore's displayed channel. markHistoriesStale leaves it
/// to the panel, which knows whether the user is at the bottom.
let displayedDmPeer: Uid | null = null;
export function setDisplayedDmPeer(peer: Uid | null): void {
  displayedDmPeer = peer;
}
export function getDisplayedDmPeer(): Uid | null {
  return displayedDmPeer;
}

interface DmConversation {
  /// The other person's account (the conversation's key).
  peerUid: Uid;
  /// Their name as last delivered; drawn via the users store.
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
  /// A re-sync after central signed us in again is in flight — see
  /// chatStore.resyncing.
  resyncing?: boolean;
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
  peerUid: Uid;
  peer: string;
  lastMessageContent: string;
  lastMessageSender: string;
  lastMessageSenderUid?: Uid;
  lastMessageId: number;
  lastTimestamp: number;
  unreadCount: number;
  encrypted?: boolean;
  decryptError?: string;
}

interface HistoryMessageInput {
  id: number;
  sender: string;
  senderUid?: Uid;
  content: string;
  timestamp: number;
  editedAt?: number;
  replyTo?: number;
  replyToSender?: string;
  replyToSenderUid?: Uid;
  replyToContent?: string;
  encrypted?: boolean;
  decryptError?: string;
}

interface DmState {
  /// By peer uid.
  conversations: Record<Uid, DmConversation>;
  /// The open conversation's peer.
  activeDmUser: Uid | null;
  friendsOnlyDms: boolean;
  /// Per-peer snapshot of optimistically-removed DM messages awaiting
  /// the server ack. Mirror of chatStore.pendingDeletions for DMs.
  pendingDmDeletions: Record<Uid, Map<number, DmMessage>>;
  /// Bumped every time central signs us in again (markHistoriesStale):
  /// DmChatPanel brings the open conversation up to date (review B4).
  resyncEpoch: number;

  setActiveDmUser: (uid: Uid | null) => void;
  addDmMessage: (otherUser: Uid, message: DmMessage, isFromSelf: boolean, otherName?: string) => void;
  setFriendsOnlyDms: (value: boolean) => void;
  /// Replace conversation previews with the server-truth list from
  /// DmConversationsRes. Per peer: keep the existing `messages` array
  /// (live DMs from this session may already be there), reset
  /// unreadCount + lastTimestamp from the server, leave
  /// historyLoaded alone (still false until the user opens the
  /// conversation and request_dm_history responds).
  hydrateConversations: (previews: ConversationPreviewInput[]) => void;
  /// Merge a newest or older DmHistoryRes page into a peer's conversation
  /// (lib/historyMerge: by id, settling sends whose echo was lost, and
  /// replacing the loaded rows when the page lies entirely above them).
  /// Flips historyLoaded to true.
  appendHistory: (
    peer: Uid,
    messages: HistoryMessageInput[],
    hasMore: boolean,
  ) => void;
  /// Jump-to-message: REPLACE a peer's loaded messages with a context window
  /// (drops the preview placeholder / stale live tail), setting both the
  /// older and newer "has more" flags. Input may be any order — sorted by id.
  setDmWindow: (
    peer: Uid,
    messages: HistoryMessageInput[],
    hasMoreOlder: boolean,
    hasMoreNewer: boolean,
  ) => void;
  /// Downward pagination (and re-sync / echo checks): merge the page of
  /// rows above `afterId` — chatStore.appendNewer's twin.
  appendNewerDm: (
    peer: Uid,
    messages: HistoryMessageInput[],
    hasMoreNewer: boolean,
    afterId: number,
  ) => void;
  /// Jump-to-present: clear the loaded slice so the caller can reload the
  /// newest page from scratch (hasMoreAfter → false, historyLoaded → false).
  /// Unconfirmed sends stay.
  resetDmForJump: (peer: Uid) => void;
  /// Central signed us in again (review B4): DMs sent, edited or deleted
  /// meanwhile never reached the loaded conversations. Each one drops its
  /// rows and reloads on open (the conversations refetch puts the preview
  /// back), unless it holds an unconfirmed send: that one goes windowed
  /// and pages down. The open conversation is DmChatPanel's. Bumps
  /// resyncEpoch.
  markHistoriesStale: () => void;
  /// Newer DMs may exist below the slice — chatStore.markWindowed's twin.
  markDmWindowed: (peer: Uid) => void;
  /// chatStore.setResyncing's twin.
  setDmResyncing: (peer: Uid, on: boolean) => void;
  /// Sliding-window trims (RealMessageList) — mirror of chatStore's
  /// trimTail/trimHead on the per-conversation flags. trimDmTail keeps the
  /// oldest `keep` rows and flips hasMoreAfter → true (the dropped tail is
  /// re-fetchable via after_id; live DMs are dropped past the gap and the
  /// "jump to present" pill shows). No-op while a row lacks a real id and
  /// when nothing would drop.
  trimDmTail: (peer: Uid, keep: number) => void;
  /// Keeps the newest `keep` rows and flips hasMoreHistory → true.
  trimDmHead: (peer: Uid, keep: number) => void;
  /// Optimistically zero the unread count and bump lastReadId.
  /// Called from DmChatPanel when the conversation becomes visible.
  markRead: (peer: Uid, upToId: number) => void;
  /// Remove a DM from a peer's visible message list. Idempotent.
  removeDmMessage: (peer: Uid, messageId: number) => void;
  /// Drop an optimistic (id-less) DM by its nonce — the send failed or
  /// was never confirmed.
  removeDmMessageByNonce: (peer: Uid, nonce: string) => void;
  /// Apply a DM edit broadcast: replace content + set editedAt on the match.
  applyDmEdit: (
    peer: Uid,
    messageId: number,
    content: string,
    editedAt: number,
    encrypted?: boolean,
    decryptError?: string,
  ) => void;
  /// E2EE: after keys unlock, every loaded row that arrived sealed holds a
  /// placeholder. Drop all loaded history so the panels reload it (the
  /// caller re-requests conversations + the active peer's page).
  invalidateAllHistory: () => void;
  /// Snapshot + remove for optimistic delete; returns the snapshot.
  snapshotAndRemoveDm: (peer: Uid, messageId: number) => DmMessage | undefined;
  /// Re-insert a snapshotted DM (rejection path). Sorted by id.
  restorePendingDmDeletion: (peer: Uid, messageId: number) => void;
  /// Drop the pending snapshot (success-ack or matching broadcast).
  clearPendingDmDeletion: (peer: Uid, messageId: number) => void;
}

function fromHistory(m: HistoryMessageInput): DmMessage {
  return {
    sender: m.sender,
    senderUid: toUid(m.senderUid),
    content: m.content,
    // DmMessage.timestamp is a string in the renderer's wire shape
    // (matches DIRECT_MSG event payload). Convert here.
    timestamp: String(m.timestamp),
    id: m.id,
    editedAt: m.editedAt || undefined,
    replyTo: m.replyTo || undefined,
    replyToSender: m.replyToSender || undefined,
    replyToSenderUid: m.replyToSenderUid,
    replyToContent: m.replyToContent || undefined,
    encrypted: m.encrypted || undefined,
    decryptError: m.decryptError || undefined,
  };
}

/// See chatStore's deletingIds.
function deletingDmIds(state: DmState, peer: Uid): Set<number> | undefined {
  const pending = state.pendingDmDeletions[peer];
  return pending && pending.size > 0 ? new Set(pending.keys()) : undefined;
}

function emptyConversation(peerUid: Uid, username = nameOf(peerUid)): DmConversation {
  return {
    peerUid,
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
  resyncEpoch: 0,

  setActiveDmUser: (uid) => set({ activeDmUser: uid }),

  addDmMessage: (otherUser, message, isFromSelf, otherName) =>
    set((state) => {
      const existing = state.conversations[otherUser];
      // Dedup replayed live messages (reconnect / at-least-once delivery):
      // a real-id message we've already inserted must not create a
      // duplicate bubble or double-bump the unread count.
      // message.id is truthy only for real (server-assigned) ids; 0 /
      // undefined are optimistic and handled by nonce reconciliation.
      if (message.id && existing?.messages.some((m) => m.id === message.id)) {
        return state;
      }
      // While viewing a jumped/windowed slice (newer messages hidden below),
      // a live real-id DM belongs past the gap — drop it to keep the window
      // contiguous. It loads when the user returns to present / pages down.
      // No optimistic send reaches here in that state: the composer snaps to
      // present before sending.
      if (existing?.hasMoreAfter && typeof message.id === "number" && message.id > 0) {
        return state;
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
            ...emptyConversation(otherUser, otherName),
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
        const existing = next[p.peerUid];
        // Server's last_timestamp is Unix seconds; the sidebar's
        // formatRelativeTime + the rest of the store work in ms.
        const lastMessageTimeMs = p.lastTimestamp * 1000;
        // Synthesize a single placeholder message from the preview
        // so the sidebar can render last-message content + timestamp
        // immediately, before the user clicks into the conversation
        // and triggers request_dm_history. The preview text may be cut
        // (central sends 512 bytes, review A1) and carries no reply or
        // edit marks, so the first history page replaces this entry
        // (appendHistory drops `preview` rows). Skip synthesis if the
        // conversation already has messages in-memory from this session.
        const previewMessage: DmMessage = {
          id: p.lastMessageId,
          sender: p.lastMessageSender,
          senderUid: toUid(p.lastMessageSenderUid),
          content: p.lastMessageContent,
          timestamp: String(p.lastTimestamp),
          encrypted: p.encrypted || undefined,
          decryptError: p.decryptError || undefined,
          preview: true,
        };
        const hasInMemoryMessages = (existing?.messages.length ?? 0) > 0;
        next[p.peerUid] = {
          peerUid: p.peerUid,
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
      // A row synthesized from the conversation preview gives way to the
      // server's: the first page (before_id 0) holds the newest message.
      const existing = (conv?.messages ?? []).filter((m) => !m.preview);
      // The page is newest-first per the protocol; the merge sorts by id.
      const incoming = messages.map(fromHistory);
      const r = mergeStoredPage(existing, incoming, useAuthStore.getState().uid, 0, deletingDmIds(state, peer));
      const merged = r.list;
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
            messages: merged,
            lastMessageTime,
            lastMessage: newerOf(conv?.lastMessage ?? null, newestOf(incoming)),
            hasMoreHistory: r.headExtended ? hasMore : conv?.hasMoreHistory ?? hasMore,
            // The newest page: the slice is at the present now.
            hasMoreAfter: r.reachedNewest ? false : conv?.hasMoreAfter ?? false,
            historyLoaded: true,
          },
        },
      };
    }),

  setDmWindow: (peer, messages, hasMoreOlder, hasMoreNewer) =>
    set((state) => {
      const conv = state.conversations[peer];
      const list: DmMessage[] = messages.map(fromHistory).sort((a, b) => (a.id ?? 0) - (b.id ?? 0));
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...emptyConversation(peer),
            ...conv,
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

  appendNewerDm: (peer, messages, hasMoreNewer, afterId) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return state;
      const existing = conv.messages.filter((m) => !m.preview);
      const incoming = messages.map(fromHistory);
      const r = mergeStoredPage(existing, incoming, useAuthStore.getState().uid, afterId, deletingDmIds(state, peer));
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            messages: r.list === existing ? conv.messages : r.list,
            // Only a windowed slice can reach the present (see appendNewer).
            hasMoreAfter: r.applied ? conv.hasMoreAfter && hasMoreNewer : conv.hasMoreAfter,
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
            messages: (conv?.messages ?? []).filter((m) => !m.id && !!m.nonce),
            hasMoreHistory: true,
            hasMoreAfter: false,
            historyLoaded: false,
          },
        },
      };
    }),

  markHistoriesStale: () =>
    set((state) => {
      const next: Record<Uid, DmConversation> = {};
      for (const conv of Object.values(state.conversations)) {
        const stored = conv.messages.some((m) => !m.preview && typeof m.id === "number" && m.id > 0);
        if (conv.peerUid === displayedDmPeer || !stored) {
          next[conv.peerUid] = conv;
          continue;
        }
        // Live DMs from before the drop count too: one that arrives now
        // would sit past whatever the drop swallowed.
        next[conv.peerUid] = conv.messages.some((m) => !m.id && !!m.nonce)
          ? { ...conv, hasMoreAfter: true }
          : { ...conv, messages: [], historyLoaded: false, hasMoreHistory: true, hasMoreAfter: false };
      }
      return { conversations: next, resyncEpoch: state.resyncEpoch + 1 };
    }),

  markDmWindowed: (peer) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv || conv.hasMoreAfter) return state;
      return { conversations: { ...state.conversations, [peer]: { ...conv, hasMoreAfter: true } } };
    }),

  setDmResyncing: (peer, on) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv || !!conv.resyncing === on) return state;
      const next = on ? { ...conv, resyncing: true, hasMoreAfter: true } : { ...conv, resyncing: undefined };
      return { conversations: { ...state.conversations, [peer]: next } };
    }),

  trimDmTail: (peer, keep) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv || keep <= 0 || keep >= conv.messages.length) return state;
      if (conv.messages.some((m) => !(typeof m.id === "number" && m.id > 0))) return state;
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
      if (!conv || keep <= 0 || keep >= conv.messages.length) return state;
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
      if (!conv) return state;
      // A count from central can outlive our own read marker: a mark-read
      // lost to a dropped connection leaves central counting what we read.
      if (upToId <= conv.lastReadId && conv.unreadCount === 0) return state;
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            unreadCount: 0,
            lastReadId: Math.max(conv.lastReadId, upToId),
          },
        },
      };
    }),

  removeDmMessageByNonce: (peer, nonce) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return state;
      const next = conv.messages.filter((m) => !(!m.id && m.nonce === nonce));
      if (next.length === conv.messages.length) return state;
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
      if (!conv) return state;
      const next = conv.messages.filter((m) => m.id !== messageId);
      if (next.length === conv.messages.length) return state;
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

  applyDmEdit: (peer, messageId, content, editedAt, encrypted, decryptError) =>
    set((state) => {
      const conv = state.conversations[peer];
      if (!conv) return state;
      let changed = false;
      const patch = {
        content,
        editedAt,
        encrypted: encrypted || undefined,
        decryptError: decryptError || undefined,
      };
      const next = conv.messages.map((m) => {
        if (m.id !== messageId) return m;
        changed = true;
        return { ...m, ...patch };
      });
      if (!changed) return state;
      return {
        conversations: {
          ...state.conversations,
          [peer]: {
            ...conv,
            messages: next,
            lastMessage:
              idOf(conv.lastMessage) === messageId && conv.lastMessage
                ? { ...conv.lastMessage, ...patch }
                : conv.lastMessage,
          },
        },
      };
    }),

  invalidateAllHistory: () =>
    set((state) => {
      const next: Record<string, DmConversation> = {};
      for (const [peer, conv] of Object.entries(state.conversations)) {
        next[peer] = {
          ...conv,
          messages: [],
          hasMoreHistory: true,
          hasMoreAfter: false,
          historyLoaded: false,
        };
      }
      return { conversations: next, pendingDmDeletions: {} };
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
      if (!updatedConv) return s;
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
      if (!snap) return state;
      const conv = state.conversations[peer];
      if (!conv) return state;
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
      if (!bucket || !bucket.has(messageId)) return state;
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
