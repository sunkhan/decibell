import { create } from "zustand";
import { channelKey, type ChannelKey } from "../lib/channelKey";
import type { Uid } from "./usersStore";

// In-memory drafts for unsent messages so the composer survives a channel or
// DM switch. Keys: ChannelKey (serverId + channelId — bare channel ids
// collide across servers) for channel drafts, the recipient's uid for DM
// drafts. Empty strings are deleted so the map doesn't grow forever.

interface DraftsState {
  channelDrafts: Record<ChannelKey, string>;
  dmDrafts: Record<Uid, string>;
  /// The last draft put back from outside the composer (a refused send),
  /// so the open composer can show it. `n` changes on every restore.
  restored: { key: ChannelKey; n: number } | null;
  /// The DM twin of `restored`.
  restoredDm: { peer: Uid; n: number } | null;
  getChannelDraft: (serverId: string, channelId: string) => string;
  setChannelDraft: (serverId: string, channelId: string, value: string) => void;
  /// Put the text of a refused message back, unless something new has
  /// been typed since. True when it was put back.
  restoreChannelDraft: (serverId: string, channelId: string, value: string) => boolean;
  restoreDmDraft: (peer: Uid, value: string) => boolean;
  setDmDraft: (peer: Uid, value: string) => void;
  clearChannelDraft: (serverId: string, channelId: string) => void;
  clearDmDraft: (peer: Uid) => void;
}

export const useDraftsStore = create<DraftsState>((set, get) => ({
  channelDrafts: {},
  dmDrafts: {},
  restored: null,
  restoredDm: null,
  getChannelDraft: (serverId, channelId) =>
    get().channelDrafts[channelKey(serverId, channelId)] ?? "",
  setChannelDraft: (serverId, channelId, value) =>
    set((s) => {
      const key = channelKey(serverId, channelId);
      const next = { ...s.channelDrafts };
      if (value) next[key] = value;
      else delete next[key];
      return { channelDrafts: next };
    }),
  restoreChannelDraft: (serverId, channelId, value) => {
    const key = channelKey(serverId, channelId);
    if (!value || get().channelDrafts[key]) return false;
    set((s) => ({
      channelDrafts: { ...s.channelDrafts, [key]: value },
      restored: { key, n: (s.restored?.n ?? 0) + 1 },
    }));
    return true;
  },
  restoreDmDraft: (peer, value) => {
    if (!value || (get().dmDrafts[peer] ?? "").trim()) return false;
    set((s) => ({
      dmDrafts: { ...s.dmDrafts, [peer]: value },
      restoredDm: { peer, n: (s.restoredDm?.n ?? 0) + 1 },
    }));
    return true;
  },
  setDmDraft: (peer, value) =>
    set((s) => {
      const next = { ...s.dmDrafts };
      if (value) next[peer] = value;
      else delete next[peer];
      return { dmDrafts: next };
    }),
  clearChannelDraft: (serverId, channelId) =>
    set((s) => {
      const key = channelKey(serverId, channelId);
      if (!(key in s.channelDrafts)) return s;
      const next = { ...s.channelDrafts };
      delete next[key];
      return { channelDrafts: next };
    }),
  clearDmDraft: (peer) =>
    set((s) => {
      if (!(peer in s.dmDrafts)) return s;
      const next = { ...s.dmDrafts };
      delete next[peer];
      return { dmDrafts: next };
    }),
}));
