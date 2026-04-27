import { create } from "zustand";
import type { VoiceParticipant, StreamInfo, ClientCapabilities, VideoCodec } from "../types";

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
  channelPresence: Record<string, string[]>;
  channelUserStates: Record<string, Record<string, { isMuted: boolean; isDeafened: boolean }>>;
  setConnectedChannel: (serverId: string | null, channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  setActiveStreams: (streams: StreamInfo[]) => void;
  setMuted: (muted: boolean) => void;
  setDeafened: (deafened: boolean) => void;
  setSpeaking: (username: string, speaking: boolean) => void;
  setLatency: (ms: number) => void;
  setError: (error: string | null) => void;
  setChannelPresence: (
    channelId: string,
    users: string[],
    userStates?: { username: string; isMuted: boolean; isDeafened: boolean }[],
    userCapabilities?: ClientCapabilities[],
  ) => void;
  /// Username → that user's advertised codec capabilities, populated
  /// from VoicePresenceUpdate.user_capabilities. Drives JS-side LCD
  /// evaluation for the streamer (Plan C) and watch-button gating.
  userCapabilities: Record<string, ClientCapabilities>;
  /// Local helper: does any user in voice have this codec in their decode caps?
  /// Returns true when caps unknown, so the watch button is permissive
  /// for legacy peers that haven't shipped capabilities yet.
  canDecode: (username: string, codec: VideoCodec) => boolean;
  setUserState: (username: string, isMuted: boolean, isDeafened: boolean) => void;
  streamThumbnails: Record<string, string>;
  setStreamThumbnail: (username: string, dataUrl: string) => void;
  watching: string | null; // deprecated — use watchingStreams/fullscreenStream
  watchingStreams: string[];
  fullscreenStream: string | null;
  isStreaming: boolean;
  streamSettings: {
    resolution: '1080p' | '720p' | 'source';
    fps: 60 | 30 | 15;
    quality: 'high' | 'medium' | 'low' | 'custom';
    videoBitrateKbps: number;
    shareAudio: boolean;
    audioBitrateKbps: 128 | 192;
  };
  setWatching: (username: string | null) => void;
  addWatching: (username: string) => void;
  removeWatching: (username: string) => void;
  isStreamFullscreen: boolean;
  setFullscreenStream: (username: string | null) => void;
  setStreamFullscreen: (fs: boolean) => void;
  setIsStreaming: (streaming: boolean) => void;
  setStreamSettings: (settings: Partial<VoiceState['streamSettings']>) => void;
  userVolumes: Record<string, number>; // username → dB value (0 = default)
  setUserVolume: (username: string, db: number) => void;
  localMutedUsers: Set<string>; // users muted locally by this client
  toggleLocalMute: (username: string) => void;
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
  channelPresence: {},
  channelUserStates: {},
  setConnectedChannel: (serverId, channelId) =>
    set({ connectedServerId: serverId, connectedChannelId: channelId }),
  setParticipants: (participants) =>
    set((state) => ({
      participants: participants.map((p) => {
        // Preserve existing mute/deafen state from remote updates
        const existing = state.participants.find((e) => e.username === p.username);
        return existing
          ? { ...p, isMuted: existing.isMuted, isDeafened: existing.isDeafened }
          : p;
      }),
    })),
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
  setChannelPresence: (channelId, users, userStates, userCapabilities) =>
    set((state) => {
      const stateMap: Record<string, { isMuted: boolean; isDeafened: boolean }> = {};
      if (userStates) {
        for (const s of userStates) {
          stateMap[s.username] = { isMuted: s.isMuted, isDeafened: s.isDeafened };
        }
      }
      // userCapabilities is parallel to users — userCapabilities[i] belongs
      // to users[i]. Merge into the global username→caps map so other voice
      // channels' members are not lost.
      const newCaps = { ...state.userCapabilities };
      if (userCapabilities && userCapabilities.length === users.length) {
        users.forEach((u, i) => {
          newCaps[u] = userCapabilities[i];
        });
      }
      return {
        channelPresence: { ...state.channelPresence, [channelId]: users },
        channelUserStates: { ...state.channelUserStates, [channelId]: stateMap },
        userCapabilities: newCaps,
      };
    }),
  userCapabilities: {},
  canDecode: (username, codec) => {
    // Unknown codec is always "decodable" (treat as legacy / no info).
    if (codec === 0) return true;
    const caps = (useVoiceStore.getState().userCapabilities as Record<string, ClientCapabilities>)[username];
    if (!caps) return true; // legacy peer with no caps advertised
    return caps.decode.some((c) => c.codec === codec);
  },
  setUserState: (username, isMuted, isDeafened) =>
    set((state) => ({
      participants: state.participants.map((p) =>
        p.username === username ? { ...p, isMuted, isDeafened } : p
      ),
    })),
  streamThumbnails: {},
  setStreamThumbnail: (username, dataUrl) =>
    set((state) => ({
      streamThumbnails: { ...state.streamThumbnails, [username]: dataUrl },
    })),
  watching: null,
  watchingStreams: [],
  fullscreenStream: null,
  isStreaming: false,
  streamSettings: {
    resolution: '1080p',
    fps: 60,
    quality: 'high',
    videoBitrateKbps: 10000,
    shareAudio: false,
    audioBitrateKbps: 128,
  },
  setWatching: (username) => set({ watching: username }),
  addWatching: (username) =>
    set((state) => ({
      watchingStreams: state.watchingStreams.includes(username)
        ? state.watchingStreams
        : [...state.watchingStreams, username],
    })),
  removeWatching: (username) =>
    set((state) => ({
      watchingStreams: state.watchingStreams.filter((u) => u !== username),
      fullscreenStream:
        state.fullscreenStream === username ? null : state.fullscreenStream,
    })),
  isStreamFullscreen: false,
  setFullscreenStream: (username) => set({ fullscreenStream: username }),
  setStreamFullscreen: (fs) => set({ isStreamFullscreen: fs }),
  setIsStreaming: (streaming) => set({ isStreaming: streaming }),
  setStreamSettings: (settings) =>
    set((state) => ({
      streamSettings: { ...state.streamSettings, ...settings },
    })),
  userVolumes: {},
  setUserVolume: (username, db) =>
    set((state) => ({
      userVolumes: { ...state.userVolumes, [username]: db },
    })),
  localMutedUsers: new Set(),
  toggleLocalMute: (username) =>
    set((state) => {
      const next = new Set(state.localMutedUsers);
      if (next.has(username)) {
        next.delete(username);
      } else {
        next.add(username);
      }
      return { localMutedUsers: next };
    }),
  disconnect: () =>
    set({
      connectedServerId: null,
      connectedChannelId: null,
      participants: [],
      speakingUsers: [],
      latencyMs: null,
      error: null,
      watching: null,
      watchingStreams: [],
      fullscreenStream: null,
      isStreamFullscreen: false,
      isStreaming: false,
      streamThumbnails: {},
    }),
}));
