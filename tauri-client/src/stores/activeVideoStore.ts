import { create } from "zustand";

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
  // The currently-playing video. `null` when no video is active. Setting
  // this to a new value replaces the previous (the persistent <video>
  // element swaps src, which naturally pauses the old playback).
  active: ActiveVideo | null;
  // The placeholder DOM element in the chat row that represents where
  // the active video should be visually overlaid. `null` when the host
  // isn't currently mounted (user scrolled away) — the persistent video
  // continues playing in that case, just parked offscreen.
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
