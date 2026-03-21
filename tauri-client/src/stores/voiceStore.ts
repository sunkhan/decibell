import { create } from "zustand";
import type { VoiceParticipant, StreamInfo } from "../types";

interface VoiceState {
  connectedServerId: string | null;
  connectedChannelId: string | null;
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  isMuted: boolean;
  isDeafened: boolean;
  speakingUsers: string[];
  latencyMs: number | null;
  error: string | null;
  setConnectedChannel: (serverId: string | null, channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  setActiveStreams: (streams: StreamInfo[]) => void;
  setMuted: (muted: boolean) => void;
  setDeafened: (deafened: boolean) => void;
  setSpeaking: (username: string, speaking: boolean) => void;
  setLatency: (ms: number) => void;
  setError: (error: string | null) => void;
  disconnect: () => void;
}

export const useVoiceStore = create<VoiceState>((set) => ({
  connectedServerId: null,
  connectedChannelId: null,
  participants: [],
  activeStreams: [],
  isMuted: false,
  isDeafened: false,
  speakingUsers: [],
  latencyMs: null,
  error: null,
  setConnectedChannel: (serverId, channelId) =>
    set({ connectedServerId: serverId, connectedChannelId: channelId }),
  setParticipants: (participants) => set({ participants }),
  setActiveStreams: (streams) => set({ activeStreams: streams }),
  setMuted: (muted) => set({ isMuted: muted }),
  setDeafened: (deafened) =>
    set(deafened ? { isDeafened: true, isMuted: true } : { isDeafened: false }),
  setSpeaking: (username, speaking) =>
    set((state) => ({
      speakingUsers: speaking
        ? state.speakingUsers.includes(username)
          ? state.speakingUsers
          : [...state.speakingUsers, username]
        : state.speakingUsers.filter((u) => u !== username),
    })),
  setLatency: (ms) => set({ latencyMs: ms }),
  setError: (error) => set({ error }),
  disconnect: () =>
    set({
      connectedServerId: null,
      connectedChannelId: null,
      participants: [],
      isMuted: false,
      isDeafened: false,
      speakingUsers: [],
      latencyMs: null,
      error: null,
    }),
}));
