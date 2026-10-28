import { create } from "zustand";
import type { Uid } from "./usersStore";

/// One picture the viewer can show: an image attachment, or an image a
/// link preview found (no serverId / attachmentId, a remote https URL).
export interface ViewerImage {
  /// Stable identity. The thumbnail in the chat carries it as
  /// `data-viewer-key`, which is how opening flies out of it and closing
  /// flies back into it (wherever it is on screen by then).
  key: string;
  /// Full size.
  url: string;
  /// What the chat already shows for it (a thumbnail variant, in memory):
  /// painted the moment the viewer opens, under the full size.
  previewUrl?: string;
  /// Filmstrip size (the 320 thumbnail when the server has one).
  thumbUrl?: string;
  /// ThumbHash: the ambient glow behind the picture.
  placeholder?: string;
  filename: string;
  width: number;
  height: number;
  sizeBytes?: number;
  mime?: string;
  /// Attachments: Copy / Save / the right-click menu.
  serverId?: string;
  attachmentId?: number;
  /// Where it was posted: the header.
  channelId?: string;
  senderUid?: Uid;
  sender?: string;
  timestamp?: string;
  /// Link images: the page the link points at (Open in browser).
  pageUrl?: string;
}

/// The thumbnail's box when the viewer opened, and how it was cropped.
export interface ViewerOrigin {
  rect: { left: number; top: number; width: number; height: number };
  /// Grid cells crop the picture (object-cover); standalone ones don't.
  fit: "cover" | "contain";
}

interface ImageViewerState {
  items: ViewerImage[];
  index: number;
  origin: ViewerOrigin | null;
  open: (items: ViewerImage[], index: number, origin?: ViewerOrigin | null) => void;
  go: (index: number) => void;
  close: () => void;
}

export const useImageViewerStore = create<ImageViewerState>((set) => ({
  items: [],
  index: 0,
  origin: null,
  open: (items, index, origin = null) =>
    set({ items, index: Math.max(0, Math.min(items.length - 1, index)), origin }),
  go: (index) =>
    set((s) => (index < 0 || index >= s.items.length || index === s.index ? s : { index })),
  close: () => set({ items: [], index: 0, origin: null }),
}));

/// The thumbnail element for an item, if it is still mounted.
export function viewerThumbFor(key: string): HTMLElement | null {
  return document.querySelector<HTMLElement>(`[data-viewer-key="${CSS.escape(key)}"]`);
}
