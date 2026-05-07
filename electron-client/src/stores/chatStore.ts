import { create } from "zustand";
import type { ChannelInfo, CommunityServer, Message, ServerMember } from "../types";

// PR4 chatStore — text channels, messages, history paging, optimistic
// bubbles. Members/bans/invites and the LRU channel cache are deferred
// to later PRs; the per-channel maps are simple unbounded objects for
// now (worst-case heavy chatters can hit a re-eviction story later).

interface ChatState {
  // Connection state
  servers: CommunityServer[];
  onlineUsers: string[];
  activeServerId: string | null;
  activeChannelId: string | null;
  connectedServers: Set<string>;
  serverMeta: Record<string, { name: string; description: string }>;
  serverOwner: Record<string, string>;
  membersByServer: Record<string, ServerMember[]>;
  bansByServer: Record<string, string[]>;
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

  // Mutators
  setServers: (servers: CommunityServer[]) => void;
  setOnlineUsers: (users: string[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: ChannelInfo[]) => void;
  upsertChannel: (serverId: string, channel: ChannelInfo) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
  setServerMeta: (
    serverId: string,
    meta: { name: string; description: string },
  ) => void;
  setServerOwner: (serverId: string, owner: string) => void;
  setMembersForServer: (serverId: string, members: ServerMember[], bans: string[]) => void;
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
}

// Merge a new message into a channel's list, sorted by id ascending and
// deduped by id. id=0 entries (optimistic bubbles) sit at the tail
// since they have no stable cursor; on receipt of a real server message
// echoing the same nonce, the optimistic is reaped.
function mergeMessage(existing: Message[], incoming: Message): Message[] {
  if (incoming.id === 0) {
    return [...existing, incoming];
  }
  const filtered = existing.filter((m) => {
    if (m.id === incoming.id) return false;
    if (m.id === 0 && incoming.nonce && m.nonce === incoming.nonce) return false;
    return true;
  });
  let idx = filtered.length;
  for (let i = filtered.length - 1; i >= 0; --i) {
    if (filtered[i].id !== 0 && filtered[i].id < incoming.id) {
      idx = i + 1;
      break;
    }
    if (i === 0) idx = 0;
  }
  return [...filtered.slice(0, idx), incoming, ...filtered.slice(idx)];
}

export const useChatStore = create<ChatState>((set) => ({
  servers: [],
  onlineUsers: [],
  activeServerId: null,
  activeChannelId: null,
  connectedServers: new Set(),
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

  setServers: (servers) => set({ servers }),
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),

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

  setServerMeta: (serverId, meta) =>
    set((state) => ({ serverMeta: { ...state.serverMeta, [serverId]: meta } })),

  setServerOwner: (serverId, owner) =>
    set((state) => ({ serverOwner: { ...state.serverOwner, [serverId]: owner } })),

  setMembersForServer: (serverId, members, bans) =>
    set((state) => ({
      membersByServer: { ...state.membersByServer, [serverId]: members },
      bansByServer: { ...state.bansByServer, [serverId]: bans },
    })),

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
    }),
}));
