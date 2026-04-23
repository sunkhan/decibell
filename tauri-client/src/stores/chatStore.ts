import { create } from "zustand";
import type { CommunityServer, Channel, Message, ServerMember, ServerInvite } from "../types";

export interface PendingInvite {
  host: string;
  port: number;
  code: string;
}

interface ChatState {
  servers: CommunityServer[];
  activeServerId: string | null;
  activeChannelId: string | null;
  channelsByServer: Record<string, Channel[]>;
  messagesByChannel: Record<string, Message[]>;
  // Per-channel pagination state. `hasMoreHistory[channelId]=true` means the
  // server told us more older messages exist. `historyLoading` prevents
  // duplicate fetches when the user scrolls fast near the top.
  hasMoreHistory: Record<string, boolean>;
  historyLoading: Record<string, boolean>;
  // Channels whose initial history page has been fetched, so we don't keep
  // re-fetching every time the user switches back to an empty channel.
  historyFetched: Record<string, boolean>;
  onlineUsers: string[];
  connectedServers: Set<string>;
  serverOwner: Record<string, string>;
  serverMeta: Record<string, { name: string; description: string }>;
  // Attachment HTTP endpoint advertised by each connected server, used by
  // the file-upload UI. Port 0 = server doesn't support attachments.
  serverAttachmentConfig: Record<string, { port: number; maxBytes: number }>;
  membersByServer: Record<string, ServerMember[]>;
  bansByServer: Record<string, string[]>;
  invitesByServer: Record<string, ServerInvite[]>;
  pendingInvite: PendingInvite | null;
  setServers: (servers: CommunityServer[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: Channel[]) => void;
  upsertChannel: (serverId: string, channel: Channel) => void;
  addMessage: (message: Message) => void;
  prependHistory: (channelId: string, messages: Message[], hasMore: boolean) => void;
  setHistoryLoading: (channelId: string, loading: boolean) => void;
  markHistoryFetched: (channelId: string) => void;
  applyChannelPruned: (
    channelId: string,
    deletedMessageIds: number[],
    attachmentTombstones: Array<{ attachmentId: number; purgedAt: number }>,
  ) => void;
  setOnlineUsers: (users: string[]) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
  setServerOwner: (serverId: string, owner: string) => void;
  setServerMeta: (serverId: string, meta: { name: string; description: string }) => void;
  setServerAttachmentConfig: (serverId: string, port: number, maxBytes: number) => void;
  setMembersForServer: (serverId: string, members: ServerMember[], bans: string[]) => void;
  setInvitesForServer: (serverId: string, invites: ServerInvite[]) => void;
  upsertInvite: (serverId: string, invite: ServerInvite) => void;
  removeInvite: (serverId: string, code: string) => void;
  setPendingInvite: (invite: PendingInvite | null) => void;
}

// Merge a new message into an existing list, sorted by id ascending and
// deduped by id. id=0 means "ephemeral" (DMs, pending-insert) — those are
// always appended since they have no stable cursor.
function mergeMessage(existing: Message[], incoming: Message): Message[] {
  if (incoming.id === 0) {
    return [...existing, incoming];
  }
  // Replace any existing entry with the same id (upsert). Common path: history
  // re-fetch after a reconnect.
  const filtered = existing.filter((m) => m.id !== incoming.id);
  // Find insertion point by id.
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
  activeServerId: null,
  activeChannelId: null,
  channelsByServer: {},
  messagesByChannel: {},
  hasMoreHistory: {},
  historyLoading: {},
  historyFetched: {},
  onlineUsers: [],
  connectedServers: new Set(),
  serverOwner: {},
  serverMeta: {},
  serverAttachmentConfig: {},
  membersByServer: {},
  bansByServer: {},
  invitesByServer: {},
  pendingInvite: null,
  setServers: (servers) => set({ servers }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),
  setChannelsForServer: (serverId, channels) => set((state) => ({ channelsByServer: { ...state.channelsByServer, [serverId]: channels } })),
  upsertChannel: (serverId, channel) => set((state) => {
    const existing = state.channelsByServer[serverId] ?? [];
    const idx = existing.findIndex((c) => c.id === channel.id);
    const next = idx === -1 ? [...existing, channel] : existing.map((c, i) => (i === idx ? channel : c));
    return { channelsByServer: { ...state.channelsByServer, [serverId]: next } };
  }),
  addMessage: (message) => set((state) => ({
    messagesByChannel: {
      ...state.messagesByChannel,
      [message.channelId]: mergeMessage(state.messagesByChannel[message.channelId] ?? [], message),
    },
  })),
  prependHistory: (channelId, messages, hasMore) => set((state) => {
    const existing = state.messagesByChannel[channelId] ?? [];
    // Dedupe by id — incoming history page may overlap with already-loaded
    // live messages. Keep the existing entries (they may have local-only state
    // we want to preserve) and only add ids we don't already have.
    const existingIds = new Set(existing.filter((m) => m.id !== 0).map((m) => m.id));
    const fresh = messages.filter((m) => m.id !== 0 && !existingIds.has(m.id));
    // History batch is oldest→newest; prepend it to the beginning, then rely
    // on the usual id-sorted order by reconstructing via mergeMessage.
    let merged = [...fresh, ...existing];
    // Ensure ascending-by-id order for rows with real ids. Rows with id=0
    // (unsent or DM) stay where they were at the tail.
    const withId = merged.filter((m) => m.id !== 0).sort((a, b) => a.id - b.id);
    const ephemeral = merged.filter((m) => m.id === 0);
    merged = [...withId, ...ephemeral];
    return {
      messagesByChannel: { ...state.messagesByChannel, [channelId]: merged },
      hasMoreHistory: { ...state.hasMoreHistory, [channelId]: hasMore },
    };
  }),
  setHistoryLoading: (channelId, loading) => set((state) => ({
    historyLoading: { ...state.historyLoading, [channelId]: loading },
  })),
  markHistoryFetched: (channelId) => set((state) => ({
    historyFetched: { ...state.historyFetched, [channelId]: true },
  })),
  applyChannelPruned: (channelId, deletedMessageIds, attachmentTombstones) => set((state) => {
    const existing = state.messagesByChannel[channelId] ?? [];
    const deletedSet = new Set(deletedMessageIds);
    const tombstoneMap = new Map(attachmentTombstones.map((t) => [t.attachmentId, t.purgedAt]));
    const next = existing
      .filter((m) => m.id === 0 || !deletedSet.has(m.id))
      .map((m) => {
        if (m.attachments.length === 0) return m;
        let changed = false;
        const nextAttachments = m.attachments.map((a) => {
          const purgedAt = tombstoneMap.get(a.id);
          if (purgedAt !== undefined && a.purgedAt === 0) {
            changed = true;
            return { ...a, purgedAt, url: "" };
          }
          return a;
        });
        return changed ? { ...m, attachments: nextAttachments } : m;
      });
    return {
      messagesByChannel: { ...state.messagesByChannel, [channelId]: next },
    };
  }),
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  addConnectedServer: (serverId) => set((state) => ({ connectedServers: new Set([...state.connectedServers, serverId]) })),
  removeConnectedServer: (serverId) => set((state) => { const next = new Set(state.connectedServers); next.delete(serverId); return { connectedServers: next }; }),
  setServerOwner: (serverId, owner) => set((state) => ({ serverOwner: { ...state.serverOwner, [serverId]: owner } })),
  setServerMeta: (serverId, meta) => set((state) => ({ serverMeta: { ...state.serverMeta, [serverId]: meta } })),
  setServerAttachmentConfig: (serverId, port, maxBytes) => set((state) => ({
    serverAttachmentConfig: { ...state.serverAttachmentConfig, [serverId]: { port, maxBytes } },
  })),
  setMembersForServer: (serverId, members, bans) => set((state) => ({
    membersByServer: { ...state.membersByServer, [serverId]: members },
    bansByServer: { ...state.bansByServer, [serverId]: bans },
  })),
  setInvitesForServer: (serverId, invites) => set((state) => ({ invitesByServer: { ...state.invitesByServer, [serverId]: invites } })),
  upsertInvite: (serverId, invite) => set((state) => {
    const existing = state.invitesByServer[serverId] ?? [];
    const filtered = existing.filter((i) => i.code !== invite.code);
    return { invitesByServer: { ...state.invitesByServer, [serverId]: [invite, ...filtered] } };
  }),
  removeInvite: (serverId, code) => set((state) => {
    const existing = state.invitesByServer[serverId] ?? [];
    return { invitesByServer: { ...state.invitesByServer, [serverId]: existing.filter((i) => i.code !== code) } };
  }),
  setPendingInvite: (invite) => set({ pendingInvite: invite }),
}));
