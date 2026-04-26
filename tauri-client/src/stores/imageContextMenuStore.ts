import { create } from "zustand";

interface ImageContextMenuState {
  open: boolean;
  x: number;
  y: number;
  serverId: string | null;
  attachmentId: number | null;
  filename: string | null;
  show: (opts: {
    x: number;
    y: number;
    serverId: string;
    attachmentId: number;
    filename?: string;
  }) => void;
  close: () => void;
}

export const useImageContextMenuStore = create<ImageContextMenuState>((set) => ({
  open: false,
  x: 0,
  y: 0,
  serverId: null,
  attachmentId: null,
  filename: null,
  show: ({ x, y, serverId, attachmentId, filename }) =>
    set({
      open: true,
      x,
      y,
      serverId,
      attachmentId,
      filename: filename ?? null,
    }),
  close: () => set({ open: false }),
}));
