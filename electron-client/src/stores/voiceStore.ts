import type { VoiceE2eeStatePayload } from "../types";
import { create } from "zustand";
import { StreamAudioMode, type VoiceParticipant, type StreamInfo, type ClientCapabilities, type VideoCodec } from "../types";
import { useVoiceStatsStore } from "./voiceStatsStore";
import { nameOf, type Uid } from "./usersStore";
import { useAuthStore } from "./authStore";

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

/// Our own joins and leaves, shown before the server confirms them. The
/// rosters are the server's, which lists us in a channel only once native
/// has started the new engine and the server's broadcast is back: until
/// then a switch showed us still in the channel we left and missing from
/// the one we joined, and a leave left us in the channel for a moment.
///
/// Native sends the server our LEAVEs and JOINs in order, and the server
/// broadcasts each as that channel's roster (without us / with us), in
/// order too. So per channel we queue what we asked for and take an entry
/// off the front when a roster shows it; the channel shows the last one
/// asked for meanwhile. A roster that doesn't match the front is one sent
/// before the server got to our request (someone else joined, a mute), so
/// it's corrected, not taken as an answer. An entry nothing answers (a
/// join that never left native, a refused one) expires: the server's roster
/// is shown as is after SELF_MOVE_TIMEOUT_MS.
type SelfPlacement = "in" | "out";
interface SelfMove {
  /// What we asked the server for in this channel, oldest first.
  pending: SelfPlacement[];
  /// The server's latest roster for the channel, uncorrected.
  raw: Uid[];
  /// Our row while we're shown in the channel ahead of the server.
  self: VoiceUserState;
  /// Shown as the server has it from then on.
  until: number;
}

const SELF_MOVE_TIMEOUT_MS = 10_000;

/// The roster a channel shows while `move` is pending.
function correctedRoster(move: SelfMove, me: Uid): Uid[] {
  const want = move.pending[move.pending.length - 1];
  const listed = move.raw.includes(me);
  if (want === "in") return listed ? move.raw : [...move.raw, me];
  return listed ? move.raw.filter((u) => u !== me) : move.raw;
}

/// Queue `placement` for `key`. Returns the store slices it changes, or
/// null when the server's roster already shows it and nothing is pending
/// (e.g. leaving a channel a moderator already took us out of).
function queueSelfMove(
  state: Pick<VoiceState, "channelPresence" | "channelUserStates" | "selfMoves">,
  key: string,
  placement: SelfPlacement,
  me: Uid,
  self: VoiceUserState,
  now: number,
): Pick<VoiceState, "channelPresence" | "channelUserStates" | "selfMoves"> | null {
  const prev = state.selfMoves[key];
  const raw = prev ? prev.raw : state.channelPresence[key] ?? [];
  if (!prev && raw.includes(me) === (placement === "in")) return null;
  const move: SelfMove = {
    pending: [...(prev?.pending ?? []), placement],
    raw,
    self,
    until: now + SELF_MOVE_TIMEOUT_MS,
  };
  const states = state.channelUserStates[key] ?? {};
  return {
    selfMoves: { ...state.selfMoves, [key]: move },
    channelPresence: { ...state.channelPresence, [key]: correctedRoster(move, me) },
    channelUserStates:
      placement === "in" && !states[me]
        ? { ...state.channelUserStates, [key]: { ...states, [me]: self } }
        : state.channelUserStates,
  };
}

let selfMoveTimer: ReturnType<typeof setTimeout> | null = null;

/// Show the server's roster for every channel whose pending moves timed
/// out, and wake again for the next one.
function expireSelfMoves(): void {
  selfMoveTimer = null;
  const now = Date.now();
  useVoiceStore.setState((state) => {
    const expired = Object.entries(state.selfMoves).filter(([, m]) => m.until <= now);
    if (expired.length === 0) return state;
    const selfMoves = { ...state.selfMoves };
    const channelPresence = { ...state.channelPresence };
    for (const [key, move] of expired) {
      delete selfMoves[key];
      channelPresence[key] = move.raw;
    }
    return { selfMoves, channelPresence };
  });
  scheduleSelfMoveExpiry();
}

function scheduleSelfMoveExpiry(): void {
  if (selfMoveTimer) clearTimeout(selfMoveTimer);
  selfMoveTimer = null;
  const untils = Object.values(useVoiceStore.getState().selfMoves).map((m) => m.until);
  if (untils.length === 0) return;
  selfMoveTimer = setTimeout(expireSelfMoves, Math.max(0, Math.min(...untils) - Date.now()) + 20);
}

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
  /// Our own channel moves the server hasn't confirmed yet, by
  /// `voiceKey` (see SelfMove). `channelPresence` already shows them.
  selfMoves: Record<string, SelfMove>;
  setConnectedChannel: (serverId: string | null, channelId: string | null) => void;
  setParticipants: (participants: VoiceParticipant[]) => void;
  /// Make (serverId, channelId) the connected channel: a join, a switch,
  /// or a moderator's move. The old channel's session state goes (its
  /// encryption state, speaking rings) and the roster and stream list are
  /// seeded from the per-channel caches, all in one update, so nothing of
  /// the old channel is drawn under the new one. `optimistic` is our own
  /// join or switch (native starts a new engine, the server hasn't seen it
  /// yet): it also shows us in the new channel and out of the old one before
  /// the server's broadcasts do, `suppressed` being our no-SPEAK badge there
  /// meanwhile. Without it (a moderator's move) the engine keeps running.
  enterChannel: (
    serverId: string,
    channelId: string,
    opts?: { optimistic?: boolean; suppressed?: boolean },
  ) => void;
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
    fps: 120 | 60 | 30 | 15 | 5;
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
  selfMoves: {},
  channelPresence: {},
  channelUserStates: {},
  setConnectedChannel: (serverId, channelId) =>
    set({ connectedServerId: serverId, connectedChannelId: channelId }),
  // The roster's mute / deafen is the server's current state (it rebroadcasts
  // on every VOICE_STATE_NOTIFY), so it replaces what the audio-packet flags
  // last said rather than being overridden by it.
  setParticipants: (participants) => set({ participants }),
  enterChannel: (serverId, channelId, opts) => {
    set((state) => {
      const to = voiceKey(serverId, channelId);
      const from =
        state.connectedServerId !== null && state.connectedChannelId !== null
          ? voiceKey(state.connectedServerId, state.connectedChannelId)
          : null;
      const changed = from !== to;
      let slices: Pick<VoiceState, "channelPresence" | "channelUserStates" | "selfMoves"> = {
        channelPresence: state.channelPresence,
        channelUserStates: state.channelUserStates,
        selfMoves: state.selfMoves,
      };
      const me = useAuthStore.getState().uid;
      if (opts?.optimistic && changed && me > 0) {
        const now = Date.now();
        // A moderator's mute / deafen is the member's, so it follows us.
        const was = from ? state.channelUserStates[from]?.[me] : undefined;
        const self: VoiceUserState = {
          isMuted: state.isMuted,
          isDeafened: state.isDeafened,
          isServerMuted: was?.isServerMuted ?? false,
          isServerDeafened: was?.isServerDeafened ?? false,
          isSuppressed: opts.suppressed ?? false,
        };
        if (from) slices = queueSelfMove(slices, from, "out", me, self, now) ?? slices;
        slices = queueSelfMove(slices, to, "in", me, self, now) ?? slices;
      }
      const roster = slices.channelPresence[to] ?? [];
      const states = slices.channelUserStates[to] ?? {};
      // Speaking rings come from the engine. A new one starts silent; one
      // that carries on through a move doesn't announce us again while we
      // talk, so ours stays.
      const speakingUsers = !changed
        ? state.speakingUsers
        : !opts?.optimistic && me > 0 && state.speakingUsers.has(me)
          ? new Set<Uid>([me])
          : new Set<Uid>();
      return {
        ...slices,
        connectedServerId: serverId,
        connectedChannelId: channelId,
        participants: roster.map((u) => ({
          uid: u,
          username: nameOf(u),
          isMuted: states[u]?.isMuted ?? false,
          isDeafened: states[u]?.isDeafened ?? false,
          isServerMuted: states[u]?.isServerMuted ?? false,
          isServerDeafened: states[u]?.isServerDeafened ?? false,
          isSuppressed: states[u]?.isSuppressed ?? false,
          isSpeaking: speakingUsers.has(u),
          audioLevel: 0,
        })),
        // The new channel's streams, until its stream presence arrives
        // (the old channel's showed under the new one meanwhile).
        activeStreams: [...state.streamsByUser.values()]
          .filter((loc) => loc.serverId === serverId && loc.channelId === channelId)
          .map((loc) => loc.streamInfo),
        ...(changed ? { e2ee: null, speakingUsers } : {}),
      };
    });
    scheduleSelfMoveExpiry();
  },
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
      // The auth snapshot that follows is the server's word on us too.
      const selfMoves = Object.fromEntries(
        Object.entries(state.selfMoves).filter(([k]) => !drop(k)),
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
        Object.keys(selfMoves).length === Object.keys(state.selfMoves).length &&
        streamsByUser === state.streamsByUser;
      return unchanged ? state : { channelPresence, channelUserStates, selfMoves, streamsByUser };
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
      // Our own unconfirmed join / leave here (see SelfMove).
      let roster = uids;
      let selfMoves = state.selfMoves;
      const prev = state.selfMoves[key];
      const me = useAuthStore.getState().uid;
      if (prev && me > 0) {
        const shows: SelfPlacement = uids.includes(me) ? "in" : "out";
        const pending = prev.pending[0] === shows ? prev.pending.slice(1) : prev.pending;
        selfMoves = { ...state.selfMoves };
        if (pending.length === 0) {
          delete selfMoves[key];
        } else {
          const move = { ...prev, pending, raw: uids };
          selfMoves[key] = move;
          roster = correctedRoster(move, me);
          if (roster.includes(me) && !stateMap[me]) stateMap[me] = move.self;
        }
      }
      return {
        channelPresence: { ...state.channelPresence, [key]: roster },
        channelUserStates: { ...state.channelUserStates, [key]: stateMap },
        selfMoves,
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
    // Leaving (or a join that failed): out of the channel's roster now,
    // not when the server's broadcast comes back. A no-op when the server
    // already took us out (a moderator's disconnect, a DM call).
    const cur = get();
    const me = useAuthStore.getState().uid;
    const leaving =
      me > 0 && cur.connectedServerId !== null && cur.connectedChannelId !== null
        ? voiceKey(cur.connectedServerId, cur.connectedChannelId)
        : null;
    const left = leaving
      ? queueSelfMove(
          cur,
          leaving,
          "out",
          me,
          cur.selfMoves[leaving]?.self ?? { isMuted: cur.isMuted, isDeafened: cur.isDeafened },
          Date.now(),
        )
      : null;
    // Revoke any in-flight thumbnail blob URLs before dropping them.
    const prevThumbs = get().streamThumbnails;
    for (const url of Object.values(prevThumbs)) {
      if (url && url.startsWith("blob:")) {
        try { URL.revokeObjectURL(url); } catch { /* already revoked */ }
      }
    }
    set({
      ...left,
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
    scheduleSelfMoveExpiry();
  },
}));
