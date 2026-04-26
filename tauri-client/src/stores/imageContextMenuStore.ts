import { create } from "zustand";

// Despite the name, this store now backs the right-click menu for both
// image and video attachments. The `kind` field controls which items
// the menu shows (only image kind offers Copy → clipboard) and the
// labels used in toasts.
export type ContextMenuKind = "image" | "video";

interface ImageContextMenuState {
  open: boolean;
  x: number;
  y: number;
  serverId: string | null;
  attachmentId: number | null;
  filename: string | null;
  kind: ContextMenuKind;
  show: (opts: {
    x: number;
    y: number;
    serverId: string;
    attachmentId: number;
    filename?: string;
    kind?: ContextMenuKind;
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
  kind: "image",
  show: ({ x, y, serverId, attachmentId, filename, kind }) =>
    set({
      open: true,
      x,
      y,
      serverId,
      attachmentId,
      filename: filename ?? null,
      kind: kind ?? "image",
    }),
  close: () => set({ open: false }),
}));
