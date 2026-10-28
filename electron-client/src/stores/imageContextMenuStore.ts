import { create } from "zustand";

// Despite the name, this store backs the right-click menu for both
// image and video attachments. The `kind` field controls which items
// the menu shows (only image kind offers Copy → clipboard) and the
// labels used in the toasts.
export type ContextMenuKind = "image" | "video";

interface ImageContextMenuState {
  open: boolean;
  x: number;
  y: number;
  serverId: string | null;
  attachmentId: number | null;
  filename: string | null;
  mime: string | null;
  kind: ContextMenuKind;
  /// Link images (no attachment): the image and the page it came from.
  url: string | null;
  pageUrl: string | null;
  show: (opts: {
    x: number;
    y: number;
    serverId?: string;
    attachmentId?: number;
    filename?: string;
    mime?: string;
    kind?: ContextMenuKind;
    url?: string;
    pageUrl?: string;
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
  mime: null,
  kind: "image",
  url: null,
  pageUrl: null,
  show: ({ x, y, serverId, attachmentId, filename, mime, kind, url, pageUrl }) =>
    set({
      open: true,
      x,
      y,
      serverId: serverId ?? null,
      attachmentId: attachmentId ?? null,
      filename: filename ?? null,
      mime: mime ?? null,
      kind: kind ?? "image",
      url: url ?? null,
      pageUrl: pageUrl ?? null,
    }),
  close: () => set({ open: false }),
}));
