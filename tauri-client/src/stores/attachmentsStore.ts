import { create } from "zustand";
import type { AttachmentKind } from "../types";

// Each file the user has queued for the *current compose* gets a client-side
// `pendingId`. The Rust upload loop echoes that id in every progress event so
// the UI can key them here. The attachment becomes server-ready when a
// `complete` event arrives; at that point `attachmentId` is populated and the
// card stops spinning.
//
// Entries live as long as the channel's composer holds them. When the user
// hits send, we pass `attachmentId`s to send_channel_message and clear the
// channel's pending list. When the user removes one via the × button we
// cancel the in-flight upload (or just drop the entry if it's already ready).
// Lifecycle:
//   queued    — selected by the user but not yet sent. The file is on disk,
//                metadata is captured, but no upload has started.
//   uploading — the user hit send; the Rust upload is in flight.
//   ready     — server returned an attachment_id; included in the next sent
//                message. Stays here briefly until the message actually goes.
//   failed    — Rust reported an error. User can remove + try again.
//   cancelled — user clicked × during upload.
export type PendingStatus = "queued" | "uploading" | "ready" | "failed" | "cancelled";

export interface PendingAttachment {
  pendingId: string;
  channelId: string;
  // Captured at queue time so handleSend can launch the upload later.
  serverId: string;
  filePath: string;
  width: number;
  height: number;
  filename: string;
  mime: string;
  kind: AttachmentKind;
  totalBytes: number;
  transferredBytes: number;
  status: PendingStatus;
  // Populated on `complete` — the server-side attachment id that
  // send_channel_message will reference.
  attachmentId?: number;
  error?: string;
}

interface AttachmentsState {
  // channelId → pendingId[] (insertion order = display order above the input)
  orderByChannel: Record<string, string[]>;
  // pendingId → record
  byPendingId: Record<string, PendingAttachment>;

  addPending: (a: PendingAttachment) => void;
  updateProgress: (pendingId: string, transferred: number) => void;
  markUploading: (pendingId: string) => void;
  markReady: (pendingId: string, attachmentId: number, kind: AttachmentKind, mime: string, filename: string) => void;
  markFailed: (pendingId: string, error: string, cancelled: boolean) => void;
  removePending: (pendingId: string) => void;
  clearChannel: (channelId: string) => void;
  /// Helper: returns pending attachments for a channel in display order.
  selectForChannel: (channelId: string) => PendingAttachment[];
}

export const useAttachmentsStore = create<AttachmentsState>((set, get) => ({
  orderByChannel: {},
  byPendingId: {},

  addPending: (a) => set((state) => {
    const list = state.orderByChannel[a.channelId] ?? [];
    return {
      orderByChannel: { ...state.orderByChannel, [a.channelId]: [...list, a.pendingId] },
      byPendingId: { ...state.byPendingId, [a.pendingId]: a },
    };
  }),

  updateProgress: (pendingId, transferred) => set((state) => {
    const existing = state.byPendingId[pendingId];
    if (!existing || existing.status !== "uploading") return state;
    return {
      byPendingId: {
        ...state.byPendingId,
        [pendingId]: { ...existing, transferredBytes: transferred },
      },
    };
  }),

  markUploading: (pendingId) => set((state) => {
    const existing = state.byPendingId[pendingId];
    if (!existing || existing.status !== "queued") return state;
    return {
      byPendingId: {
        ...state.byPendingId,
        [pendingId]: { ...existing, status: "uploading", transferredBytes: 0 },
      },
    };
  }),

  markReady: (pendingId, attachmentId, kind, mime, filename) => set((state) => {
    const existing = state.byPendingId[pendingId];
    if (!existing) return state;
    return {
      byPendingId: {
        ...state.byPendingId,
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

  markFailed: (pendingId, error, cancelled) => set((state) => {
    const existing = state.byPendingId[pendingId];
    if (!existing) return state;
    return {
      byPendingId: {
        ...state.byPendingId,
        [pendingId]: {
          ...existing,
          status: cancelled ? "cancelled" : "failed",
          error,
        },
      },
    };
  }),

  removePending: (pendingId) => set((state) => {
    const existing = state.byPendingId[pendingId];
    if (!existing) return state;
    const { [pendingId]: _, ...rest } = state.byPendingId;
    const order = state.orderByChannel[existing.channelId] ?? [];
    return {
      byPendingId: rest,
      orderByChannel: {
        ...state.orderByChannel,
        [existing.channelId]: order.filter((id) => id !== pendingId),
      },
    };
  }),

  clearChannel: (channelId) => set((state) => {
    const order = state.orderByChannel[channelId] ?? [];
    const next = { ...state.byPendingId };
    for (const id of order) delete next[id];
    return {
      byPendingId: next,
      orderByChannel: { ...state.orderByChannel, [channelId]: [] },
    };
  }),

  selectForChannel: (channelId) => {
    const state = get();
    const order = state.orderByChannel[channelId] ?? [];
    return order.map((id) => state.byPendingId[id]).filter((x): x is PendingAttachment => !!x);
  },
}));
