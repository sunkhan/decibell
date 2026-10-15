import type { VoiceE2eeStatePayload } from "../types";
import { create } from "zustand";
import { StreamAudioMode, type VoiceParticipant, type StreamInfo, type ClientCapabilities, type VideoCodec } from "../types";
import { useVoiceStatsStore } from "./voiceStatsStore";
import { nameOf, type Uid } from "./usersStore";

// Every person here is keyed by account uid (the media plane names them by
// uid too); names are drawn through the users store. So a rename moves
// nothing in this store.

/// Per-user view of all active streams across every connected
/// community server. Used by UserPopup to look up "is this user live?"
/// in O(1) without scanning all of activeStreams (which only tracks
/// the local user's currently connected channel).
export interface StreamLocation {
  serverId: string;
  channelId: string;
  streamInfo: StreamInfo;
}

/// Key for the per-channel presence maps. Channel ids are name slugs
/// ("general", "lounge"), so two communities routinely share one — every
/// roster lookup goes through the server too.
export const voiceKey = (serverId: string, channelId: string): string => `${serverId}/${channelId}`;

type VoiceUserState = { isMuted: boolean; isDeafened: boolean; isServerMuted?: boolean; isServerDeafened?: boolean; isSuppressed?: boolean };

// Full voice store — the streaming-related slices (active streams,
// watching, stream settings, isStreaming) live alongside voice state
// because they share the same connection lifecycle and presence
// updates. Streaming-side actions (start_screen_share, etc.) port
// with the streaming PR; until then the store fields keep their
// defaults and the actions stay no-ops on the wire.

interface VoiceState {
  connectedServerId: string | null;
  connectedChannelId: string | null;
  /// P2P DM call in progress with this user. The media session (mute /
  /// deafen / speaking / ping / streams / player) then runs with
  /// `connectedServerId === null && connectedChannelId === null` — the
  /// same native VoiceEngine, just reached peer-to-peer. Components that
  /// gate on "am I in a media session" use `mediaSessionActive` below.
  callPeer: Uid | null;
  setCallPeer: (peer: Uid | null) => void;
  /// MLS encryption state of the connected community channel (null in a
  /// DM call or when not connected). Drives the header badge.
  e2ee: VoiceE2eeStatePayload | null;
  setE2ee: (state: VoiceE2eeStatePayload | null) => void;
  participants: VoiceParticipant[];
  activeStreams: StreamInfo[];
  /// Source-of-truth map of streaming users across every connected
  /// community server. Driven by the `stream_presence_updated`
  /// listener; UserPopup reads it by uid.
  streamsByUser: Map<Uid, StreamLocation>;
  isMuted: boolean;
  isDeafened: boolean;
  /// Set instead of array so per-row subscribers can do O(1)
  /// `s.speakingUsers.has(uid)` and so the slice equality is a
  /// simple ref check (we replace the Set wholesale on every change,
  /// not mutate in place).
  speakingUsers: Set<Uid>;
  latencyMs: number | null;
  error: string | null;
  /// Every visible voice channel's roster on every connected community,
  /// keyed by `voiceKey(serverId, channelId)`. The community sends these to
  /// all members (snapshot at auth, then each change), so they are
  /// community state, not voice-session state: leaving voice keeps them.
  channelPresence: Record<string, Uid[]>;
  channelUserStates: Record<string, Record<Uid, VoiceUserState>>;
  setConnectedChannel: (serverId: string | null, channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  /// Rebuild the connected-channel roster from the presence cache — for a
  /// channel switch or move, before the server's next update for it.
  seedParticipants: (serverId: string, channelId: string) => void;
  /// Drop one community's rosters + streams (its auth snapshot replaces
  /// them, and it omits empty channels). With `keepChannelIds`, drop only
  /// the channels not in it (hidden or deleted ones).
  clearServerPresence: (serverId: string, keepChannelIds?: Set<string>) => void;
  setActiveStreams: (streams: StreamInfo[]) => void;
  setStreamsByUser: (m: Map<Uid, StreamLocation>) => void;
  setMuted: (muted: boolean) => void;
  setDeafened: (deafened: boolean) => void;
  setSpeaking: (uid: Uid, speaking: boolean) => void;
  setLatency: (ms: number) => void;
  setError: (error: string | null) => void;
  setChannelPresence: (
    serverId: string,
    channelId: string,
    uids: Uid[],
    userStates?: ({ uid: Uid } & VoiceUserState)[],
    userCapabilities?: ClientCapabilities[],
  ) => void;
  /// uid → that user's advertised codec capabilities. Populated
  /// from VoicePresenceUpdate.user_capabilities when the streaming
  /// PR lights up codec negotiation. Defaults to empty.
  userCapabilities: Record<Uid, ClientCapabilities>;
  setUserState: (uid: Uid, isMuted: boolean, isDeafened: boolean) => void;
  streamThumbnails: Record<Uid, string>;
  setStreamThumbnail: (uid: Uid, dataUrl: string) => void;
  watchingStreams: Uid[];
  fullscreenStream: Uid | null;
  /// The stream currently loaded in the single persistent player. Unlike
  /// fullscreenStream it survives going "back to the streams grid", so the
  /// decoder stays warm and re-focusing the same stream is instant. Managed by
  /// StreamPipManager (set on focus, dropped when unwatched / idle on the grid).
  pipStream: Uid | null;
  isStreaming: boolean;
  streamSettings: {
    resolution: "1080p" | "720p" | "source";
    fps: 120 | 60 | 30 | 15;
    quality: "high" | "medium" | "low" | "custom";
    videoBitrateKbps: number;
    shareAudio: boolean;
    audioBitrateKbps: 128 | 192;
    /// Share-audio app filter: how the ticked set is read, and the ticked
    /// app identities (see features/voice/streamAudioFilter.ts).
    audioMode: StreamAudioMode;
    audioApps: string[];
    enforcedCodec: VideoCodec;
    includeCursor: boolean;
  };
  addWatching: (uid: Uid) => void;
  removeWatching: (uid: Uid) => void;
  setPipStream: (uid: Uid | null) => void;
  isStreamFullscreen: boolean;
  setFullscreenStream: (uid: Uid | null) => void;
  setStreamFullscreen: (fs: boolean) => void;
  setIsStreaming: (streaming: boolean) => void;
  setStreamSettings: (settings: Partial<VoiceState["streamSettings"]>) => void;
  /// Saved per-user volume (dB) and local mute, by uid (settings).
  userVolumes: Record<Uid, number>;
  setUserVolume: (uid: Uid, db: number) => void;
  localMutedUsers: Set<Uid>;
  toggleLocalMute: (uid: Uid) => void;
  disconnect: () => void;
}

export const useVoiceStore = create<VoiceState>((set, get) => ({
  connectedServerId: null,
  connectedChannelId: null,
  callPeer: null,
  setCallPeer: (peer) => set({ callPeer: peer }),
  e2ee: null,
  setE2ee: (e2ee) => set({ e2ee }),
  participants: [],
  activeStreams: [],
  streamsByUser: new Map(),
  isMuted: false,
  isDeafened: false,
  speakingUsers: new Set(),
  latencyMs: null,
  error: null,
  channelPresence: {},
  channelUserStates: {},
  setConnectedChannel: (serverId, channelId) =>
    set({ connectedServerId: serverId, connectedChannelId: channelId }),
  // The roster's mute / deafen is the server's current state (it rebroadcasts
  // on every VOICE_STATE_NOTIFY), so it replaces what the audio-packet flags
  // last said rather than being overridden by it.
  setParticipants: (participants) => set({ participants }),
  seedParticipants: (serverId, channelId) =>
    set((state) => {
      const key = voiceKey(serverId, channelId);
      const roster = state.channelPresence[key] ?? [];
      const states = state.channelUserStates[key] ?? {};
      return {
        participants: roster.map((u) => ({
          uid: u,
          username: nameOf(u),
          isMuted: states[u]?.isMuted ?? false,
          isDeafened: states[u]?.isDeafened ?? false,
          isServerMuted: states[u]?.isServerMuted ?? false,
          isServerDeafened: states[u]?.isServerDeafened ?? false,
          isSuppressed: states[u]?.isSuppressed ?? false,
          isSpeaking: state.speakingUsers.has(u),
          audioLevel: 0,
        })),
      };
    }),
  clearServerPresence: (serverId, keepChannelIds) =>
    set((state) => {
      const prefix = `${serverId}/`;
      const drop = (key: string) =>
        key.startsWith(prefix) && !keepChannelIds?.has(key.slice(prefix.length));
      const channelPresence = Object.fromEntries(
        Object.entries(state.channelPresence).filter(([k]) => !drop(k)),
      );
      const channelUserStates = Object.fromEntries(
        Object.entries(state.channelUserStates).filter(([k]) => !drop(k)),
      );
      let streamsByUser = state.streamsByUser;
      for (const [user, loc] of state.streamsByUser) {
        if (loc.serverId === serverId && !keepChannelIds?.has(loc.channelId)) {
          if (streamsByUser === state.streamsByUser) streamsByUser = new Map(state.streamsByUser);
          streamsByUser.delete(user);
        }
      }
      const unchanged =
        Object.keys(channelPresence).length === Object.keys(state.channelPresence).length &&
        Object.keys(channelUserStates).length === Object.keys(state.channelUserStates).length &&
        streamsByUser === state.streamsByUser;
      return unchanged ? state : { channelPresence, channelUserStates, streamsByUser };
    }),
  setActiveStreams: (streams) => set({ activeStreams: streams }),
  setStreamsByUser: (m) => set({ streamsByUser: m }),
  setMuted: (muted) => set({ isMuted: muted }),
  setDeafened: (deafened) =>
    set(deafened ? { isDeafened: true, isMuted: true } : { isDeafened: false }),
  setSpeaking: (uid, speaking) =>
    set((state) => {
      const has = state.speakingUsers.has(uid);
      // Bail early if the requested state matches what we have — keeps
      // the Set ref stable so per-row subscribers don't churn on a
      // no-op event (the wire fires speaking-stop/start frequently).
      if (has === speaking) return state;
      const next = new Set(state.speakingUsers);
      if (speaking) next.add(uid);
      else next.delete(uid);
      return { speakingUsers: next };
    }),
  setLatency: (ms) => set({ latencyMs: ms }),
  setError: (error) => set({ error }),
  setChannelPresence: (serverId, channelId, uids, userStates, userCapabilities) =>
    set((state) => {
      const key = voiceKey(serverId, channelId);
      const stateMap: Record<Uid, VoiceUserState> = {};
      if (userStates) {
        for (const s of userStates) {
          stateMap[s.uid] = {
            isMuted: s.isMuted,
            isDeafened: s.isDeafened,
            isServerMuted: s.isServerMuted,
            isServerDeafened: s.isServerDeafened,
            isSuppressed: s.isSuppressed,
          };
        }
      }
      const newCaps = { ...state.userCapabilities };
      if (userCapabilities && userCapabilities.length === uids.length) {
        uids.forEach((u, i) => {
          newCaps[u] = userCapabilities[i];
        });
      }
      return {
        channelPresence: { ...state.channelPresence, [key]: uids },
        channelUserStates: { ...state.channelUserStates, [key]: stateMap },
        userCapabilities: newCaps,
      };
    }),
  userCapabilities: {},
  setUserState: (uid, isMuted, isDeafened) =>
    set((state) => ({
      participants: state.participants.map((p) =>
        p.uid === uid ? { ...p, isMuted, isDeafened } : p,
      ),
    })),
  streamThumbnails: {},
  setStreamThumbnail: (uid, url) =>
    set((state) => {
      // Thumbnails are blob: URLs created from raw JPEG bytes pushed
      // by main. Revoke the previous URL before replacing so we don't
      // leak Chromium-side blob storage as fresh thumbnails arrive
      // every few seconds per active stream.
      const prev = state.streamThumbnails[uid];
      if (prev && prev.startsWith("blob:")) {
        try { URL.revokeObjectURL(prev); } catch { /* already revoked */ }
      }
      return {
        streamThumbnails: { ...state.streamThumbnails, [uid]: url },
      };
    }),
  watchingStreams: [],
  fullscreenStream: null,
  pipStream: null,
  isStreaming: false,
  streamSettings: {
    resolution: "1080p",
    fps: 60,
    quality: "high",
    videoBitrateKbps: 10000,
    shareAudio: false,
    audioBitrateKbps: 128,
    audioMode: StreamAudioMode.ALL,
    audioApps: [],
    enforcedCodec: 0 as VideoCodec,
    includeCursor: true,
  },
  addWatching: (uid) =>
    set((state) => ({
      watchingStreams: state.watchingStreams.includes(uid)
        ? state.watchingStreams
        : [...state.watchingStreams, uid],
    })),
  removeWatching: (uid) =>
    set((state) => ({
      watchingStreams: state.watchingStreams.filter((u) => u !== uid),
      fullscreenStream: state.fullscreenStream === uid ? null : state.fullscreenStream,
      pipStream: state.pipStream === uid ? null : state.pipStream,
    })),
  setPipStream: (uid) => set({ pipStream: uid }),
  isStreamFullscreen: false,
  setFullscreenStream: (uid) => set({ fullscreenStream: uid }),
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
  setUserVolume: (uid, db) =>
    set((state) => ({
      userVolumes: { ...state.userVolumes, [uid]: db },
    })),
  localMutedUsers: new Set(),
  toggleLocalMute: (uid) =>
    set((state) => {
      const next = new Set(state.localMutedUsers);
      if (next.has(uid)) {
        next.delete(uid);
      } else {
        next.add(uid);
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
      callPeer: null,
      e2ee: null,
      participants: [],
      speakingUsers: new Set(),
      latencyMs: null,
      error: null,
      watchingStreams: [],
      fullscreenStream: null,
      pipStream: null,
      isStreamFullscreen: false,
      isStreaming: false,
      streamThumbnails: {},
      // Session state only. channelPresence / channelUserStates /
      // streamsByUser stay: they mirror every channel of every community
      // and the server keeps them current whether or not we're in voice —
      // wiping them blanked every other channel's members until that
      // channel next changed. userCapabilities is refilled by the next
      // join's presence; clearing it keeps it from growing unbounded.
      activeStreams: [],
      userCapabilities: {},
    });
  },
}));
