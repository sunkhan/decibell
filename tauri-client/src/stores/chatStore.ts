import { create } from "zustand";
import type { CommunityServer, Channel, Message } from "../types";

interface ChatState {
  servers: CommunityServer[];
  activeServerId: string | null;
  activeChannelId: string | null;
  channelsByServer: Record<string, Channel[]>;
  messagesByChannel: Record<string, Message[]>;
  channelMembers: Record<string, string[]>;
  onlineUsers: string[];
  connectedServers: Set<string>;
  setServers: (servers: CommunityServer[]) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
  setChannelsForServer: (serverId: string, channels: Channel[]) => void;
  addMessage: (message: Message) => void;
  setChannelMembers: (channelId: string, members: string[]) => void;
  setOnlineUsers: (users: string[]) => void;
  addConnectedServer: (serverId: string) => void;
  removeConnectedServer: (serverId: string) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  servers: [], activeServerId: null, activeChannelId: null, channelsByServer: {}, messagesByChannel: {}, channelMembers: {}, onlineUsers: [], connectedServers: new Set(),
  setServers: (servers) => set({ servers }),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),
  setChannelsForServer: (serverId, channels) => set((state) => ({ channelsByServer: { ...state.channelsByServer, [serverId]: channels } })),
  addMessage: (message) => set((state) => ({ messagesByChannel: { ...state.messagesByChannel, [message.channelId]: [...(state.messagesByChannel[message.channelId] ?? []), message] } })),
  setChannelMembers: (channelId, members) => set((state) => ({ channelMembers: { ...state.channelMembers, [channelId]: members } })),
  setOnlineUsers: (users) => set({ onlineUsers: users }),
  addConnectedServer: (serverId) => set((state) => ({ connectedServers: new Set([...state.connectedServers, serverId]) })),
  removeConnectedServer: (serverId) => set((state) => { const next = new Set(state.connectedServers); next.delete(serverId); return { connectedServers: next }; }),
}));
