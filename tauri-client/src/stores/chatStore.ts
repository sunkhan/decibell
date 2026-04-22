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
  onlineUsers: string[];
  connectedServers: Set<string>;
  serverOwner: Record<string, string>;
  serverMeta: Record<string, { name: string; description: string }>;
  membersByServer: Record<string, ServerMember[]>;
  bansByServer: Record<string, string[]>;
  invitesByServer: Record<string, ServerInvite[]>;
  pendingInvite: PendingInvite | null;
  setServers: (servers: CommunityServer[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: Channel[]) => void;
  addMessage: (message: Message) => void;
  setOnlineUsers: (users: string[]) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
  setServerOwner: (serverId: string, owner: string) => void;
  setServerMeta: (serverId: string, meta: { name: string; description: string }) => void;
  setMembersForServer: (serverId: string, members: ServerMember[], bans: string[]) => void;
  setInvitesForServer: (serverId: string, invites: ServerInvite[]) => void;
  upsertInvite: (serverId: string, invite: ServerInvite) => void;
  removeInvite: (serverId: string, code: string) => void;
  setPendingInvite: (invite: PendingInvite | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  servers: [],
  activeServerId: null,
  activeChannelId: null,
  channelsByServer: {},
  messagesByChannel: {},
  onlineUsers: [],
  connectedServers: new Set(),
  serverOwner: {},
  serverMeta: {},
  membersByServer: {},
  bansByServer: {},
  invitesByServer: {},
  pendingInvite: null,
  setServers: (servers) => set({ servers }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),
  setChannelsForServer: (serverId, channels) => set((state) => ({ channelsByServer: { ...state.channelsByServer, [serverId]: channels } })),
  addMessage: (message) => set((state) => ({ messagesByChannel: { ...state.messagesByChannel, [message.channelId]: [...(state.messagesByChannel[message.channelId] ?? []), message] } })),
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  addConnectedServer: (serverId) => set((state) => ({ connectedServers: new Set([...state.connectedServers, serverId]) })),
  removeConnectedServer: (serverId) => set((state) => { const next = new Set(state.connectedServers); next.delete(serverId); return { connectedServers: next }; }),
  setServerOwner: (serverId, owner) => set((state) => ({ serverOwner: { ...state.serverOwner, [serverId]: owner } })),
  setServerMeta: (serverId, meta) => set((state) => ({ serverMeta: { ...state.serverMeta, [serverId]: meta } })),
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
