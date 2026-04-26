import { create } from "zustand";

// Tiny reactivity bridge for the (otherwise plain) tempVideoCache map.
// Bumped whenever a cache entry changes in a way the placeholder UI
// needs to react to (poster captured, etc.). VideoPlayer subscribes to
// `version` so a bump triggers a re-render and it picks up the new
// poster / playback position from the cache on the next render.
interface State {
  version: number;
  bump: () => void;
}

export const useVideoCacheVersionStore = create<State>((set) => ({
  version: 0,
  bump: () => set((s) => ({ version: s.version + 1 })),
}));
