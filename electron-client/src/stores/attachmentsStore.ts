import { create } from "zustand";
import type { AttachmentKind } from "../types";
import type { ChunkSource } from "../features/chat/chunkSource";

// Per-attachment lifecycle for the composer:
//
//   queued    — file selected + metadata probed; bytes NOT yet sent.
//               The attachment sits here until the user clicks send,
//               matching tauri-client behaviour: nothing leaves the
//               machine until the user commits to the message.
//   uploading — chunked PATCH in flight; `transferredBytes` advances.
//   ready     — server returned the attachment id; safe to send.
//   failed    — gave up after retries / cancel / network error.
//
// Legacy "pending" is retained as an alias for "queued" so the
// existing addPending callers don't break — internally we treat them
// identically.
//
// The store keys by client-generated `pendingId` (a UUID), which is
// also stamped onto the optimistic message bubble. Once the server
// echoes the broadcast back with the real attachment IDs, the bubble
// stops referencing pendings and reads the canonical Attachment[]
// from the chatStore message.

export type PendingStatus = "queued" | "pending" | "uploading" | "ready" | "failed";

export interface PendingAttachment {
  pendingId: string;
  serverId: string;
  channelId: string;
  status: PendingStatus;
  filename: string;
  mime: string;
  kind: AttachmentKind;
  /// Total bytes (file size). Set on enqueue.
  totalBytes: number;
  /// Bytes successfully PATCH'd to the server.
  transferredBytes: number;
  /// Server-assigned attachment id, populated on `ready`.
  attachmentId: number | null;
  /// Error message when status === "failed".
  errorMessage: string | null;
  /// Cancellation handle. uploadAttachment subscribes to abort().
  abortController: AbortController;
  /// Renderer-only blob URL for the optimistic preview (image/video).
  /// Revoked when the pending is removed.
  previewUrl: string | null;
  /// Width / height for images (used by the bubble layout to reserve
  /// space at the right aspect ratio while uploading). 0 unknown.
  width: number;
  height: number;
  /// Audio + video duration in ms; 0 unknown.
  durationMs: number;
  /// The streaming source the upload loop reads from. Held on the
  /// pending entry so handleSend can call startQueuedUpload without
  /// the caller (file picker / drag-drop / paste) having to keep its
  /// own reference. cleanup() runs once the upload terminates so the
  /// decibell-file:// whitelist entry / Blob URL is released.
  source: ChunkSource;
}

interface AttachmentsState {
  pendings: Record<string, PendingAttachment>;
  add: (p: Omit<PendingAttachment, "status" | "transferredBytes" | "attachmentId" | "errorMessage">) => void;
  setStatus: (pendingId: string, status: PendingStatus) => void;
  updateProgress: (pendingId: string, transferredBytes: number) => void;
  markReady: (
    pendingId: string,
    attachmentId: number,
    kind: AttachmentKind,
    mime: string,
    filename: string,
  ) => void;
  markFailed: (pendingId: string, message: string, cancelled?: boolean) => void;
  remove: (pendingId: string) => void;
  removePending: (pendingId: string) => void;
  /// All pendings tied to a specific channel — used to render the
  /// composer's PendingAttachmentsRow.
  selectForChannel: (serverId: string, channelId: string) => PendingAttachment[];
}

export const useAttachmentsStore = create<AttachmentsState>((set, get) => ({
  pendings: {},

  add: (p) =>
    set((state) => ({
      pendings: {
        ...state.pendings,
        [p.pendingId]: {
          ...p,
          status: "queued",
          transferredBytes: 0,
          attachmentId: null,
          errorMessage: null,
        },
      },
    })),

  setStatus: (pendingId, status) =>
    set((state) => {
      const existing = state.pendings[pendingId];
      if (!existing) return {};
      return { pendings: { ...state.pendings, [pendingId]: { ...existing, status } } };
    }),

  updateProgress: (pendingId, transferredBytes) =>
    set((state) => {
      const existing = state.pendings[pendingId];
      if (!existing) return {};
      return {
        pendings: {
          ...state.pendings,
          [pendingId]: { ...existing, status: "uploading", transferredBytes },
        },
      };
    }),

  markReady: (pendingId, attachmentId, kind, mime, filename) =>
    set((state) => {
      const existing = state.pendings[pendingId];
      if (!existing) return {};
      return {
        pendings: {
          ...state.pendings,
          [pendingId]: {
            ...existing,
            status: "ready",
            attachmentId,
            kind,
            mime,
            filename,
            transferredBytes: existing.totalBytes,
          },
        },
      };
    }),

  markFailed: (pendingId, message, _cancelled) =>
    set((state) => {
      const existing = state.pendings[pendingId];
      if (!existing) return {};
      return {
        pendings: {
          ...state.pendings,
          [pendingId]: { ...existing, status: "failed", errorMessage: message },
        },
      };
    }),

  remove: (pendingId) =>
    set((state) => {
      const existing = state.pendings[pendingId];
      if (existing?.previewUrl) URL.revokeObjectURL(existing.previewUrl);
      const next = { ...state.pendings };
      delete next[pendingId];
      return { pendings: next };
    }),

  removePending: (pendingId) => {
    const existing = get().pendings[pendingId];
    if (existing?.previewUrl) URL.revokeObjectURL(existing.previewUrl);
    set((state) => {
      const next = { ...state.pendings };
      delete next[pendingId];
      return { pendings: next };
    });
  },

  selectForChannel: (serverId, channelId) =>
    Object.values(get().pendings).filter(
      (p) => p.serverId === serverId && p.channelId === channelId,
    ),
}));
