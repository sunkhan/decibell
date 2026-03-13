import { create } from "zustand";
import type { VoiceParticipant, StreamInfo } from "../types";

interface VoiceState {
  connectedChannelId: string | null;
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  isMuted: boolean;
  localAudioLevel: number;
  setConnectedChannel: (channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  setActiveStreams: (streams: StreamInfo[]) => void;
  setMuted: (muted: boolean) => void;
  setLocalAudioLevel: (level: number) => void;
}

export const useVoiceStore = create<VoiceState>((set) => ({
  connectedChannelId: null,
  participants: [],
  activeStreams: [],
  isMuted: false,
  localAudioLevel: 0,
  setConnectedChannel: (channelId) => set({ connectedChannelId: channelId }),
  setParticipants: (participants) => set({ participants }),
  setActiveStreams: (streams) => set({ activeStreams: streams }),
  setMuted: (muted) => set({ isMuted: muted }),
  setLocalAudioLevel: (level) => set({ localAudioLevel: level }),
}));
