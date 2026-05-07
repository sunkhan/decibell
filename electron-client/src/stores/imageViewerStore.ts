import { create } from "zustand";

export interface ViewerImage {
  url: string;
  filename: string;
  width: number;
  height: number;
}

interface ImageViewerState {
  current: ViewerImage | null;
  open: (image: ViewerImage) => void;
  close: () => void;
}

export const useImageViewerStore = create<ImageViewerState>((set) => ({
  current: null,
  open: (image) => set({ current: image }),
  close: () => set({ current: null }),
}));
