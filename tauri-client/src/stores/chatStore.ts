import { create } from "zustand";
import type { CommunityServer, Channel, Message } from "../types";

interface ChatState {
  servers: CommunityServer[];
  channels: Channel[];
  messages: Message[];
  activeServerId: string | null;
  activeChannelId: string | null;
  setServers: (servers: CommunityServer[]) => void;
  setChannels: (channels: Channel[]) => void;
  addMessage: (message: Message) => void;
  setActiveServer: (serverId: string | null) => void;
  setActiveChannel: (channelId: string | null) => void;
}

export const useChatStore = create<ChatState>((set) => ({
  servers: [],
  channels: [],
  messages: [],
  activeServerId: null,
  activeChannelId: null,
  setServers: (servers) => set({ servers }),
  setChannels: (channels) => set({ channels }),
  addMessage: (message) =>
    set((state) => ({ messages: [...state.messages, message] })),
  setActiveServer: (serverId) => set({ activeServerId: serverId }),
  setActiveChannel: (channelId) => set({ activeChannelId: channelId }),
}));
