import { create } from "zustand";

// In-memory drafts for unsent messages so the composer survives a channel or
// DM switch. Keys: server channelId for channel drafts, recipient username for
// DM drafts. Empty strings are deleted so the map doesn't grow forever.

interface DraftsState {
  channelDrafts: Record<string, string>;
  dmDrafts: Record<string, string>;
  setChannelDraft: (channelId: string, value: string) => void;
  setDmDraft: (username: string, value: string) => void;
  clearChannelDraft: (channelId: string) => void;
  clearDmDraft: (username: string) => void;
}

export const useDraftsStore = create<DraftsState>((set) => ({
  channelDrafts: {},
  dmDrafts: {},
  setChannelDraft: (channelId, value) =>
    set((s) => {
      const next = { ...s.channelDrafts };
      if (value) next[channelId] = value;
      else delete next[channelId];
      return { channelDrafts: next };
    }),
  setDmDraft: (username, value) =>
    set((s) => {
      const next = { ...s.dmDrafts };
      if (value) next[username] = value;
      else delete next[username];
      return { dmDrafts: next };
    }),
  clearChannelDraft: (channelId) =>
    set((s) => {
      if (!(channelId in s.channelDrafts)) return s;
      const next = { ...s.channelDrafts };
      delete next[channelId];
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
