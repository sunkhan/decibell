import { create } from "zustand";
import type {
  ChannelInfo,
  CommunityServer,
  Message,
  PendingInvite,
  ServerInvite,
  ServerMember,
} from "../types";
import { useUiStore } from "./uiStore";

// PR4 chatStore — text channels, messages, history paging, optimistic
// bubbles. Members/bans/invites are deferred to later PRs. The LRU
// channel cache uses `channelAccessOrder` (most-recent first); each
// `setActiveChannel` moves the channel to the front and prunes any
// tail beyond `useUiStore.channelCacheSize` to keep RAM bounded.
// `enforceChannelCacheSize` runs the same prune on demand (called from
// NetworkTab when the user lowers the cap mid-session).

interface ChatState {
  // Connection state
  servers: CommunityServer[];
  onlineUsers: string[];
  activeServerId: string | null;
  activeChannelId: string | null;
  connectedServers: Set<string>;
  /// Auto-rejoin: server IDs the client is auto-connecting to as a
  /// result of LoginResponse.memberships. Drives the "connecting…"
  /// placeholder tile UI in ServerBar. Each entry is cleared in
  /// useServerEvents on the matching community_auth_responded —
  /// success moves it into connectedServers; failure drops it +
  /// fires request_drop_membership + toasts.
  pendingMembershipServerIds: Set<string>;
  serverMeta: Record<string, { name: string; description: string }>;
  serverOwner: Record<string, string>;
  membersByServer: Record<string, ServerMember[]>;
  bansByServer: Record<string, string[]>;
  invitesByServer: Record<string, ServerInvite[]>;
  pendingInvite: PendingInvite | null;
  /// Attachment HTTP endpoint advertised by each connected server.
  /// `port: 0` means the server didn't advertise one (HTTP disabled
  /// or older build). Populated from CommunityAuthResponded.
  serverAttachmentConfig: Record<string, { port: number; maxBytes: number }>;

  // Channel + message state
  channelsByServer: Record<string, ChannelInfo[]>;
  messagesByChannel: Record<string, Message[]>;
  hasMoreHistory: Record<string, boolean>;
  historyLoading: Record<string, boolean>;
  historyFetched: Record<string, boolean>;
  /// Per-channel saved scroll position so re-entering a still-cached
  /// channel restores the user to roughly where they left off (Discord-
  /// style). `topIndex` is the topmost-visible Virtuoso item index;
  /// `atBottom` is true if the user was scrolled to the latest message
  /// (in which case we restore by scrolling to LAST so new messages
  /// arrived during the absence are visible). Pruned alongside
  /// messagesByChannel via the LRU eviction in setActiveChannel.
  scrollPositionsByChannel: Record<string, { topIndex: number; atBottom: boolean }>;
  /// Live dimensions of the chat panel's viewport. Updated by ChatPanel
  /// via a ResizeObserver. Read by AttachmentList's sqrt-based sizing
  /// so image/video previews scale proportionally to the available
  /// viewport — narrow side panels render compact previews, wide
  /// fullscreen layouts render larger ones, without ever uncapped-
  /// linearly hitting "image takes up half the screen".
  chatViewSize: { width: number; height: number } | null;
  /// LRU access order for cached channels — front (index 0) is the
  /// most recently visited, tail is the least. Channels beyond
  /// `useUiStore.channelCacheSize` get evicted from every per-channel
  /// map below on the next setActiveChannel or enforceChannelCacheSize.
  channelAccessOrder: string[];

  // Mutators
  setServers: (servers: CommunityServer[]) => void;
  setOnlineUsers: (users: string[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: ChannelInfo[]) => void;
  upsertChannel: (serverId: string, channel: ChannelInfo) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
  /// Auto-rejoin: replace the pending-membership set entirely (used
  /// on memberships_received). Setting an empty array clears.
  setPendingMemberships: (ids: string[]) => void;
  removePendingMembership: (id: string) => void;
  /// De-duplicating union of the existing servers list with the new
  /// entries. Used by useServerEvents on memberships_received to
  /// backfill any servers not yet covered by server_list_received.
  mergeServers: (entries: CommunityServer[]) => void;
  setServerMeta: (
    serverId: string,
    meta: { name: string; description: string },
  ) => void;
  setServerOwner: (serverId: string, owner: string) => void;
  setMembersForServer: (serverId: string, members: ServerMember[], bans: string[]) => void;
  setInvitesForServer: (serverId: string, invites: ServerInvite[]) => void;
  upsertInvite: (serverId: string, invite: ServerInvite) => void;
  removeInvite: (serverId: string, code: string) => void;
  setPendingInvite: (invite: PendingInvite | null) => void;
  setServerAttachmentConfig: (
    serverId: string,
    port: number,
    maxBytes: number,
  ) => void;
  resetForLogout: () => void;
  addMessage: (message: Message) => void;
  prependHistory: (channelId: string, messages: Message[], hasMore: boolean) => void;
  setHistoryLoading: (channelId: string, loading: boolean) => void;
  markHistoryFetched: (channelId: string) => void;
  applyChannelPruned: (channelId: string, deletedMessageIds: number[]) => void;
  applyChannelWiped: (channelId: string) => void;
  /// Capture the user's current scroll position for a channel — called
  /// from ChatPanel on Virtuoso range/atBottom events.
  setScrollPosition: (channelId: string, topIndex: number, atBottom: boolean) => void;
  /// Update the live chat viewport dimensions. Called from ChatPanel's
  /// ResizeObserver. Pass `null` on unmount so AttachmentList's sizing
  /// helpers fall back to their fixed defaults.
  setChatViewSize: (size: { width: number; height: number } | null) => void;
  /// Drop cached channels beyond `useUiStore.channelCacheSize`. Called
  /// when the cap shrinks so eviction is immediate, not deferred to
  /// the next channel switch.
  enforceChannelCacheSize: () => void;
}

// Merge a new message into a channel's list, sorted by id ascending
// and deduped by id. id=0 entries (optimistic bubbles) sit at the tail
// since they have no stable cursor; on receipt of a real server
// message echoing the same nonce, the optimistic is reaped.
//
// The wire-message hot path is "incoming.id > every existing real id"
// (i.e. a fresh server message arriving in order). We special-case
// that to a single tail-scan + one allocation: O(M) work in 2 ops
// instead of the 4 ops the general path needs (filter + double slice
// + spread).
function mergeMessage(existing: Message[], incoming: Message): Message[] {
  // Optimistic bubbles always anchor at the tail; no dedupe needed,
  // no insertion point to compute.
  if (incoming.id === 0) {
    const out = existing.slice();
    out.push(incoming);
    return out;
  }

  const len = existing.length;
  if (len === 0) return [incoming];

  // Single tail-scan: locate the last real id (insertion anchor) and
  // any optimistic in the trailing block whose nonce echoes ours.
  // The trailing block is "everything after the last real" — that's
  // where unsent optimistics live.
  let lastRealIdx = -1;
  let nonceMatchIdx = -1;
  for (let i = len - 1; i >= 0; --i) {
    const m = existing[i];
    if (m.id === 0) {
      if (
        nonceMatchIdx === -1 &&
        incoming.nonce &&
        m.nonce === incoming.nonce
      ) {
        nonceMatchIdx = i;
      }
    } else {
      lastRealIdx = i;
      break;
    }
  }

  // Hot path: incoming is strictly newer than every real id, so we
  // can skip the full O(M) dedupe filter — no real-id collision is
  // possible. One slice + at most one trailing-optimistic removal +
  // one insert.
  if (lastRealIdx === -1 || existing[lastRealIdx].id < incoming.id) {
    const out = existing.slice();
    if (nonceMatchIdx !== -1) out.splice(nonceMatchIdx, 1);
    out.splice(lastRealIdx + 1, 0, incoming);
    return out;
  }

  // Slow path: incoming.id is <= some existing real id (history
  // back-fill, out-of-order delivery, or a duplicate). Match the
  // original ordering semantics: insert AFTER the last real with
  // id < incoming.id, ignoring optimistics for position. Fold the
  // dedupe filter and the position scan into the same allocation.
  const filtered: Message[] = [];
  for (let i = 0; i < len; ++i) {
    const m = existing[i];
    if (m.id === incoming.id) continue;
    if (m.id === 0 && incoming.nonce && m.nonce === incoming.nonce) continue;
    filtered.push(m);
  }
  let idx = filtered.length;
  for (let i = filtered.length - 1; i >= 0; --i) {
    if (filtered[i].id !== 0 && filtered[i].id < incoming.id) {
      idx = i + 1;
      break;
    }
    if (i === 0) idx = 0;
  }
  filtered.splice(idx, 0, incoming);
  return filtered;
}

export const useChatStore = create<ChatState>((set) => ({
  servers: [],
  onlineUsers: [],
  activeServerId: null,
  activeChannelId: null,
  connectedServers: new Set(),
  pendingMembershipServerIds: new Set(),
  serverMeta: {},
  serverOwner: {},
  membersByServer: {},
  bansByServer: {},
  invitesByServer: {},
  pendingInvite: null,
  serverAttachmentConfig: {},
  channelsByServer: {},
  messagesByChannel: {},
  hasMoreHistory: {},
  historyLoading: {},
  historyFetched: {},
  scrollPositionsByChannel: {},
  chatViewSize: null,
  channelAccessOrder: [],

  setServers: (servers) => set({ servers }),
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) =>
    set((state) => {
      if (!channelId) return { activeChannelId: null };
      const cap = Math.max(1, useUiStore.getState().channelCacheSize || 10);
      // Move the activated channel to the front of the access order.
      const reordered = [
        channelId,
        ...state.channelAccessOrder.filter((id) => id !== channelId),
      ];
      if (reordered.length <= cap) {
        return { activeChannelId: channelId, channelAccessOrder: reordered };
      }
      // Over the cap — drop the tail and prune every cached slice for
      // channels that fell off.
      const keep = reordered.slice(0, cap);
      const keepSet = new Set(keep);
      const filter = <T,>(rec: Record<string, T>): Record<string, T> =>
        Object.fromEntries(
          Object.entries(rec).filter(([id]) => keepSet.has(id)),
        );
      return {
        activeChannelId: channelId,
        channelAccessOrder: keep,
        messagesByChannel: filter(state.messagesByChannel),
        hasMoreHistory: filter(state.hasMoreHistory),
        historyLoading: filter(state.historyLoading),
        historyFetched: filter(state.historyFetched),
        scrollPositionsByChannel: filter(state.scrollPositionsByChannel),
      };
    }),

  setChannelsForServer: (serverId, channels) =>
    set((state) => ({
      channelsByServer: { ...state.channelsByServer, [serverId]: channels },
    })),

  upsertChannel: (serverId, channel) =>
    set((state) => {
      const existing = state.channelsByServer[serverId] ?? [];
      const idx = existing.findIndex((c) => c.id === channel.id);
      const next =
        idx === -1
          ? [...existing, channel]
          : existing.map((c, i) => (i === idx ? channel : c));
      return { channelsByServer: { ...state.channelsByServer, [serverId]: next } };
    }),

  addConnectedServer: (serverId) =>
    set((state) => ({ connectedServers: new Set([...state.connectedServers, serverId]) })),

  removeConnectedServer: (serverId) =>
    set((state) => {
      const next = new Set(state.connectedServers);
      next.delete(serverId);
      return { connectedServers: next };
    }),

  setPendingMemberships: (ids) =>
    set({ pendingMembershipServerIds: new Set(ids) }),
  removePendingMembership: (id) =>
    set((state) => {
      if (!state.pendingMembershipServerIds.has(id)) return {};
      const next = new Set(state.pendingMembershipServerIds);
      next.delete(id);
      return { pendingMembershipServerIds: next };
    }),
  mergeServers: (entries) =>
    set((state) => {
      const byId = new Map<string, CommunityServer>();
      for (const s of state.servers) byId.set(s.id, s);
      for (const s of entries) {
        if (!byId.has(s.id)) byId.set(s.id, s);
      }
      return { servers: Array.from(byId.values()) };
    }),

  setServerMeta: (serverId, meta) =>
    set((state) => ({ serverMeta: { ...state.serverMeta, [serverId]: meta } })),

  setServerOwner: (serverId, owner) =>
    set((state) => ({ serverOwner: { ...state.serverOwner, [serverId]: owner } })),

  setMembersForServer: (serverId, members, bans) =>
    set((state) => ({
      membersByServer: { ...state.membersByServer, [serverId]: members },
      bansByServer: { ...state.bansByServer, [serverId]: bans },
    })),

  setInvitesForServer: (serverId, invites) =>
    set((state) => ({
      invitesByServer: { ...state.invitesByServer, [serverId]: invites },
    })),

  upsertInvite: (serverId, invite) =>
    set((state) => {
      const existing = state.invitesByServer[serverId] ?? [];
      const filtered = existing.filter((i) => i.code !== invite.code);
      return {
        invitesByServer: {
          ...state.invitesByServer,
          [serverId]: [invite, ...filtered],
        },
      };
    }),

  removeInvite: (serverId, code) =>
    set((state) => {
      const existing = state.invitesByServer[serverId] ?? [];
      return {
        invitesByServer: {
          ...state.invitesByServer,
          [serverId]: existing.filter((i) => i.code !== code),
        },
      };
    }),

  setPendingInvite: (invite) => set({ pendingInvite: invite }),

  setServerAttachmentConfig: (serverId, port, maxBytes) =>
    set((state) => ({
      serverAttachmentConfig: {
        ...state.serverAttachmentConfig,
        [serverId]: { port, maxBytes },
      },
    })),

  addMessage: (message) =>
    set((state) => ({
      messagesByChannel: {
        ...state.messagesByChannel,
        [message.channelId]: mergeMessage(
          state.messagesByChannel[message.channelId] ?? [],
          message,
        ),
      },
    })),

  prependHistory: (channelId, messages, hasMore) =>
    set((state) => {
      const existing = state.messagesByChannel[channelId] ?? [];
      // History batch may overlap with already-loaded live messages —
      // dedup by real id (id=0 entries are optimistic, not in history).
      const existingIds = new Set(existing.filter((m) => m.id !== 0).map((m) => m.id));
      const fresh = messages.filter((m) => m.id !== 0 && !existingIds.has(m.id));
      const withId = [...fresh, ...existing.filter((m) => m.id !== 0)].sort(
        (a, b) => a.id - b.id,
      );
      const ephemeral = existing.filter((m) => m.id === 0);
      return {
        messagesByChannel: {
          ...state.messagesByChannel,
          [channelId]: [...withId, ...ephemeral],
        },
        hasMoreHistory: { ...state.hasMoreHistory, [channelId]: hasMore },
      };
    }),

  setHistoryLoading: (channelId, loading) =>
    set((state) => ({
      historyLoading: { ...state.historyLoading, [channelId]: loading },
    })),

  markHistoryFetched: (channelId) =>
    set((state) => ({
      historyFetched: { ...state.historyFetched, [channelId]: true },
    })),

  applyChannelPruned: (channelId, deletedMessageIds) =>
    set((state) => {
      const existing = state.messagesByChannel[channelId] ?? [];
      const deletedSet = new Set(deletedMessageIds);
      const next = existing.filter((m) => m.id === 0 || !deletedSet.has(m.id));
      return {
        messagesByChannel: { ...state.messagesByChannel, [channelId]: next },
      };
    }),

  applyChannelWiped: (channelId) =>
    set((state) => ({
      messagesByChannel: { ...state.messagesByChannel, [channelId]: [] },
      hasMoreHistory: { ...state.hasMoreHistory, [channelId]: false },
      historyFetched: { ...state.historyFetched, [channelId]: true },
    })),

  resetForLogout: () =>
    set({
      servers: [],
      onlineUsers: [],
      activeServerId: null,
      activeChannelId: null,
      connectedServers: new Set(),
      pendingMembershipServerIds: new Set(),
      serverMeta: {},
      serverOwner: {},
      membersByServer: {},
      bansByServer: {},
      serverAttachmentConfig: {},
      channelsByServer: {},
      messagesByChannel: {},
      hasMoreHistory: {},
      historyLoading: {},
      historyFetched: {},
      scrollPositionsByChannel: {},
      channelAccessOrder: [],
      invitesByServer: {},
      pendingInvite: null,
    }),

  enforceChannelCacheSize: () =>
    set((state) => {
      const cap = Math.max(1, useUiStore.getState().channelCacheSize || 10);
      if (state.channelAccessOrder.length <= cap) return {};
      // Always retain the active channel even if it's somehow not in
      // the top `cap` of the access order (defensive — shouldn't
      // happen since setActiveChannel reorders).
      const keep = state.channelAccessOrder.slice(0, cap);
      if (state.activeChannelId && !keep.includes(state.activeChannelId)) {
        keep.pop();
        keep.unshift(state.activeChannelId);
      }
      const keepSet = new Set(keep);
      const filter = <T,>(rec: Record<string, T>): Record<string, T> =>
        Object.fromEntries(
          Object.entries(rec).filter(([id]) => keepSet.has(id)),
        );
      return {
        channelAccessOrder: keep,
        messagesByChannel: filter(state.messagesByChannel),
        hasMoreHistory: filter(state.hasMoreHistory),
        historyLoading: filter(state.historyLoading),
        historyFetched: filter(state.historyFetched),
        scrollPositionsByChannel: filter(state.scrollPositionsByChannel),
      };
    }),

  setScrollPosition: (channelId, topIndex, atBottom) =>
    set((state) => ({
      scrollPositionsByChannel: {
        ...state.scrollPositionsByChannel,
        [channelId]: { topIndex, atBottom },
      },
    })),

  setChatViewSize: (size) => set({ chatViewSize: size }),
}));
