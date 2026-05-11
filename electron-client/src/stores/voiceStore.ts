import { create } from "zustand";
import type { VoiceParticipant, StreamInfo, ClientCapabilities, VideoCodec } from "../types";
import { useVoiceStatsStore } from "./voiceStatsStore";

// Full voice store — the streaming-related slices (active streams,
// watching, stream settings, isStreaming) live alongside voice state
// because they share the same connection lifecycle and presence
// updates. Streaming-side actions (start_screen_share, etc.) port
// with the streaming PR; until then the store fields keep their
// defaults and the actions stay no-ops on the wire.

interface VoiceState {
  connectedServerId: string | null;
  connectedChannelId: string | null;
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  isMuted: boolean;
  isDeafened: boolean;
  /// Set instead of array so per-row subscribers can do O(1)
  /// `s.speakingUsers.has(name)` and so the slice equality is a
  /// simple ref check (we replace the Set wholesale on every change,
  /// not mutate in place).
  speakingUsers: Set<string>;
  latencyMs: number | null;
  error: string | null;
  channelPresence: Record<string, string[]>;
  channelUserStates: Record<
    string,
    Record<string, { isMuted: boolean; isDeafened: boolean }>
  >;
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
  /// Username → that user's advertised codec capabilities. Populated
  /// from VoicePresenceUpdate.user_capabilities when the streaming
  /// PR lights up codec negotiation. Defaults to empty.
  userCapabilities: Record<string, ClientCapabilities>;
  canDecode: (username: string, codec: VideoCodec) => boolean;
  setUserState: (username: string, isMuted: boolean, isDeafened: boolean) => void;
  streamThumbnails: Record<string, string>;
  setStreamThumbnail: (username: string, dataUrl: string) => void;
  watching: string | null;
  watchingStreams: string[];
  fullscreenStream: string | null;
  isStreaming: boolean;
  streamSettings: {
    resolution: "1080p" | "720p" | "source";
    fps: 120 | 60 | 30 | 15;
    quality: "high" | "medium" | "low" | "custom";
    videoBitrateKbps: number;
    shareAudio: boolean;
    audioBitrateKbps: 128 | 192;
    enforcedCodec: VideoCodec;
  };
  setWatching: (username: string | null) => void;
  addWatching: (username: string) => void;
  removeWatching: (username: string) => void;
  isStreamFullscreen: boolean;
  setFullscreenStream: (username: string | null) => void;
  setStreamFullscreen: (fs: boolean) => void;
  setIsStreaming: (streaming: boolean) => void;
  setStreamSettings: (settings: Partial<VoiceState["streamSettings"]>) => void;
  userVolumes: Record<string, number>;
  setUserVolume: (username: string, db: number) => void;
  localMutedUsers: Set<string>;
  toggleLocalMute: (username: string) => void;
  disconnect: () => void;
}

export const useVoiceStore = create<VoiceState>((set, get) => ({
  connectedServerId: null,
  connectedChannelId: null,
  participants: [],
  activeStreams: [],
  isMuted: false,
  isDeafened: false,
  speakingUsers: new Set(),
  latencyMs: null,
  error: null,
  channelPresence: {},
  channelUserStates: {},
  setConnectedChannel: (serverId, channelId) =>
    set({ connectedServerId: serverId, connectedChannelId: channelId }),
  setParticipants: (participants) =>
    set((state) => ({
      participants: participants.map((p) => {
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
    set((state) => {
      const has = state.speakingUsers.has(username);
      // Bail early if the requested state matches what we have — keeps
      // the Set ref stable so per-row subscribers don't churn on a
      // no-op event (the wire fires speaking-stop/start frequently).
      if (has === speaking) return {};
      const next = new Set(state.speakingUsers);
      if (speaking) next.add(username);
      else next.delete(username);
      return { speakingUsers: next };
    }),
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
  canDecode: (username: string, codec: VideoCodec): boolean => {
    if (codec === 0) return true;
    const caps = get().userCapabilities[username];
    if (!caps) return true;
    return caps.decode.some((c: { codec: VideoCodec }) => c.codec === codec);
  },
  setUserState: (username, isMuted, isDeafened) =>
    set((state) => ({
      participants: state.participants.map((p) =>
        p.username === username ? { ...p, isMuted, isDeafened } : p,
      ),
    })),
  streamThumbnails: {},
  setStreamThumbnail: (username, url) =>
    set((state) => {
      // Thumbnails are blob: URLs created from raw JPEG bytes pushed
      // by main. Revoke the previous URL before replacing so we don't
      // leak Chromium-side blob storage as fresh thumbnails arrive
      // every few seconds per active stream.
      const prev = state.streamThumbnails[username];
      if (prev && prev.startsWith("blob:")) {
        try { URL.revokeObjectURL(prev); } catch { /* already revoked */ }
      }
      return {
        streamThumbnails: { ...state.streamThumbnails, [username]: url },
      };
    }),
  watching: null,
  watchingStreams: [],
  fullscreenStream: null,
  isStreaming: false,
  streamSettings: {
    resolution: "1080p",
    fps: 60,
    quality: "high",
    videoBitrateKbps: 10000,
    shareAudio: false,
    audioBitrateKbps: 128,
    enforcedCodec: 0 as VideoCodec,
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
  setStreamSettings: (settings) => {
    set((state) => ({
      streamSettings: { ...state.streamSettings, ...settings },
    }));
    // Persist on every change so picker selections survive restarts.
    // Dynamic import avoids the saveSettings → voiceStore →
    // saveSettings circular dependency (saveSettings reads from this
    // store at call time, not module load time, so the runtime cycle
    // is harmless — but the static-analysis cycle is messy).
    import("../features/settings/saveSettings").then(({ saveSettings }) =>
      saveSettings(),
    );
  },
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
  disconnect: () => {
    useVoiceStatsStore.getState().clear();
    // Revoke any in-flight thumbnail blob URLs before dropping them.
    const prevThumbs = get().streamThumbnails;
    for (const url of Object.values(prevThumbs)) {
      if (url && url.startsWith("blob:")) {
        try { URL.revokeObjectURL(url); } catch { /* already revoked */ }
      }
    }
    set({
      connectedServerId: null,
      connectedChannelId: null,
      participants: [],
      speakingUsers: new Set(),
      latencyMs: null,
      error: null,
      watching: null,
      watchingStreams: [],
      fullscreenStream: null,
      isStreamFullscreen: false,
      isStreaming: false,
      streamThumbnails: {},
    });
  },
}));
