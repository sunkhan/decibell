import { create } from "zustand";

// Tiny reactivity bridge for the (otherwise plain) videoPlaybackState
// map. Bumped whenever an entry changes in a way the placeholder UI
// needs to react to (poster captured, etc.). A placeholder subscribes
// to its own attachment's counter and re-reads the cache on the next
// render.
//
// Keyed per attachment rather than one global integer: as a single
// counter, capturing a poster for one video re-rendered every video
// attachment mounted anywhere in the list.
interface State {
  versions: Record<string, number>;
  bump: (attachmentId: string | number) => void;
}

export const useVideoCacheVersionStore = create<State>((set) => ({
  versions: {},
  bump: (attachmentId) =>
    set((s) => {
      const key = String(attachmentId);
      return { versions: { ...s.versions, [key]: (s.versions[key] ?? 0) + 1 } };
    }),
}));
