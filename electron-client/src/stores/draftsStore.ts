import { create } from "zustand";
import { channelKey, type ChannelKey } from "../lib/channelKey";

// In-memory drafts for unsent messages so the composer survives a channel or
// DM switch. Keys: ChannelKey (serverId + channelId — bare channel ids
// collide across servers) for channel drafts, recipient username for DM
// drafts. Empty strings are deleted so the map doesn't grow forever.

interface DraftsState {
  channelDrafts: Record<ChannelKey, string>;
  dmDrafts: Record<string, string>;
  getChannelDraft: (serverId: string, channelId: string) => string;
  setChannelDraft: (serverId: string, channelId: string, value: string) => void;
  setDmDraft: (username: string, value: string) => void;
  clearChannelDraft: (serverId: string, channelId: string) => void;
  clearDmDraft: (username: string) => void;
}

export const useDraftsStore = create<DraftsState>((set, get) => ({
  channelDrafts: {},
  dmDrafts: {},
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
  setDmDraft: (username, value) =>
    set((s) => {
      const next = { ...s.dmDrafts };
      if (value) next[username] = value;
      else delete next[username];
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
  clearDmDraft: (username) =>
    set((s) => {
      if (!(username in s.dmDrafts)) return s;
      const next = { ...s.dmDrafts };
      delete next[username];
      return { dmDrafts: next };
    }),
}));
