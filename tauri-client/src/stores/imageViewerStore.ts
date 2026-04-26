import { create } from "zustand";
import type { Attachment } from "../types";

interface ImageViewerState {
  open: boolean;
  images: Attachment[];
  serverId: string | null;
  index: number;
  show: (images: Attachment[], serverId: string, startIndex: number) => void;
  close: () => void;
  next: () => void;
  prev: () => void;
  setIndex: (i: number) => void;
}

export const useImageViewerStore = create<ImageViewerState>((set) => ({
  open: false,
  images: [],
  serverId: null,
  index: 0,
  show: (images, serverId, startIndex) =>
    set({
      open: true,
      images,
      serverId,
      index: Math.max(0, Math.min(images.length - 1, startIndex)),
    }),
  close: () => set({ open: false }),
  next: () =>
    set((s) => ({
      index: Math.min(s.images.length - 1, s.index + 1),
    })),
  prev: () =>
    set((s) => ({
      index: Math.max(0, s.index - 1),
    })),
  setIndex: (i) =>
    set((s) => ({ index: Math.max(0, Math.min(s.images.length - 1, i)) })),
}));
