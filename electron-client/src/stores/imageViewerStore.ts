import { create } from "zustand";

export interface ViewerImage {
  url: string;
  filename: string;
  width: number;
  height: number;
  /// serverId + attachmentId carried so the fullscreen viewer can
  /// surface the right-click menu (Copy / Save as) just like the
  /// inline thumbnail does. Optional for back-compat with any callers
  /// that haven't been updated.
  serverId?: string;
  attachmentId?: number;
  mime?: string;
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
