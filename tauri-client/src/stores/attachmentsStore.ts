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
  // Object URL of a small JPEG thumbnail captured at queue time
  // (image/video kinds). Drives the tile preview in the composer
  // chrome. Revoked when the entry is removed or the channel cleared.
  thumbnailUrl?: string;
  // Duration (ms) for audio/video, captured at queue time. Forwarded
  // to the server via /init so receivers can show "0:00 / 3:45"
  // before the file is downloaded.
  durationMs?: number;
  // True once the user has hit send and this entry has been associated
  // with an optimistic message in the chat. Composer's selectForChannel
  // filters these out so they vanish from the input bar instantly,
  // while the entry itself stays alive in the store so the optimistic
  // bubble can read its live upload progress.
  outbound?: boolean;
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
  /// Move `fromPendingId` to sit immediately before or after
  /// `toPendingId` in its channel's order. Both ids must already
  /// be in the same channel; otherwise this is a no-op. Used by
  /// drag-to-reorder in the composer pending row.
  reorderPending: (
    channelId: string,
    fromPendingId: string,
    toPendingId: string,
    position: "before" | "after",
  ) => void;
  /// Mark a set of pending entries as outbound so they disappear
  /// from the composer immediately. The entries stay in the store
  /// (still updating progress) so the optimistic message bubble can
  /// keep rendering them until reconciliation.
  markOutbound: (pendingIds: string[]) => void;
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
    if (existing.thumbnailUrl) URL.revokeObjectURL(existing.thumbnailUrl);
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

  reorderPending: (channelId, fromPendingId, toPendingId, position) => set((state) => {
    if (fromPendingId === toPendingId) return state;
    const order = state.orderByChannel[channelId];
    if (!order) return state;
    const fromIdx = order.indexOf(fromPendingId);
    const toIdx = order.indexOf(toPendingId);
    if (fromIdx === -1 || toIdx === -1) return state;
    const next = order.slice();
    next.splice(fromIdx, 1);
    // Recompute target index AFTER removal — splice shifts everything
    // right of fromIdx down by one. Insertion sits before/after the
    // target's new position depending on which edge the user dropped on.
    const adjustedTo = toIdx > fromIdx ? toIdx - 1 : toIdx;
    const insertAt = position === "after" ? adjustedTo + 1 : adjustedTo;
    next.splice(insertAt, 0, fromPendingId);
    return {
      orderByChannel: { ...state.orderByChannel, [channelId]: next },
    };
  }),

  clearChannel: (channelId) => set((state) => {
    const order = state.orderByChannel[channelId] ?? [];
    const next = { ...state.byPendingId };
    for (const id of order) {
      const entry = next[id];
      if (entry?.thumbnailUrl) URL.revokeObjectURL(entry.thumbnailUrl);
      delete next[id];
    }
    return {
      byPendingId: next,
      orderByChannel: { ...state.orderByChannel, [channelId]: [] },
    };
  }),

  markOutbound: (pendingIds) => set((state) => {
    const next = { ...state.byPendingId };
    for (const id of pendingIds) {
      const e = next[id];
      if (e) next[id] = { ...e, outbound: true };
    }
    return { byPendingId: next };
  }),

  selectForChannel: (channelId) => {
    const state = get();
    const order = state.orderByChannel[channelId] ?? [];
    // Composer view: filter out entries that have been associated
    // with an optimistic message. They're still in the store driving
    // upload progress for the bubble — they just shouldn't appear in
    // the input bar anymore.
    return order
      .map((id) => state.byPendingId[id])
      .filter((x): x is PendingAttachment => !!x && !x.outbound);
  },
}));
