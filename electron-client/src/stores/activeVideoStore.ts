import { create } from "zustand";

// Holds the currently-active video attachment + the placeholder DOM
// element the persistent player should track. Only one video plays at
// a time. The persistent layer follows hostElement's bounding rect via
// ResizeObserver + a RAF-throttled scroll listener; when host is null
// (user scrolled past) the video parks offscreen and keeps playing,
// snapping back into position on the next host registration.

export interface ActiveVideo {
  attachmentId: number;
  serverId: string;
  channelId: string;
  src: string;
  filename: string;
  width: number;
  height: number;
}

interface ActiveVideoState {
  active: ActiveVideo | null;
  hostElement: HTMLDivElement | null;
  setActive: (v: ActiveVideo | null) => void;
  setHostElement: (el: HTMLDivElement | null) => void;
}

export const useActiveVideoStore = create<ActiveVideoState>((set) => ({
  active: null,
  hostElement: null,
  setActive: (v) => set({ active: v }),
  setHostElement: (el) => set({ hostElement: el }),
}));
