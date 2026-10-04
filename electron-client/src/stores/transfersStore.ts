import { create } from "zustand";
import type { AttachmentKind, DownloadView } from "../types";

// The Transfers panel's state (features/transfers/). Downloads are a
// mirror of main's download manager: seeded by `list()` and patched by
// its `downloads_changed` / `downloads_removed` events (see
// initTransfers). Main is the source of truth; nothing here persists.
// Uploads are this session's history, fed from attachmentsStore (see
// features/transfers/uploads.ts) so a row outlives its pending.

export type TransfersTab = "downloads" | "uploads";

export type UploadState = "uploading" | "paused" | "done" | "failed" | "cancelled";

export interface UploadEntry {
  pendingId: string;
  serverId: string;
  serverName: string;
  channelId: string;
  channelName: string;
  filename: string;
  kind: AttachmentKind;
  /// The composer's blob preview, while the pending still exists.
  previewUrl: string | null;
  /// Server id once uploaded (for the thumbnail afterwards).
  attachmentId: number | null;
  totalBytes: number;
  transferredBytes: number;
  state: UploadState;
  error: string | null;
  speedBps: number;
  startedAt: number;
  finishedAt: number | null;
}

/// The title-bar button's dot: something ended while the panel was shut.
export type TransfersAttention = "none" | "done" | "failed";

interface TransfersState {
  /// Newest first.
  downloads: DownloadView[];
  /// Newest first. This session only.
  uploads: UploadEntry[];
  /// When each tab last saw something start, and when the panel was
  /// last opened: opening shows the tab with news, else the last one.
  activityAt: Record<TransfersTab, number>;
  openedAt: number;
  panelOpen: boolean;
  tab: TransfersTab;
  attention: TransfersAttention;
  /// Bumped when a download starts; the button replays its arrow drop.
  startPulse: number;

  /// Settings → Network → Downloads. "" = the OS Downloads folder.
  downloadDir: string;
  askDownloadLocation: boolean;

  setDownloads: (list: DownloadView[]) => void;
  setUploads: (uploads: UploadEntry[], attention?: TransfersAttention, newActivity?: boolean) => void;
  removeUpload: (pendingId: string) => void;
  clearFinishedUploads: () => void;
  upsertDownload: (d: DownloadView) => void;
  removeDownloads: (ids: string[]) => void;
  openPanel: (tab?: TransfersTab) => void;
  closePanel: () => void;
  setTab: (tab: TransfersTab) => void;
  noteStarted: () => void;
  setDownloadDir: (dir: string) => void;
  setAskDownloadLocation: (ask: boolean) => void;
}

function ended(d: DownloadView): boolean {
  return d.state === "done" || d.state === "failed";
}

export const useTransfersStore = create<TransfersState>((set) => ({
  downloads: [],
  uploads: [],
  activityAt: { downloads: 0, uploads: 0 },
  openedAt: 0,
  panelOpen: false,
  tab: "downloads",
  attention: "none",
  startPulse: 0,
  downloadDir: "",
  askDownloadLocation: false,

  setDownloads: (list) => set({ downloads: list }),

  upsertDownload: (d) =>
    set((state) => {
      const i = state.downloads.findIndex((x) => x.id === d.id);
      const prev = i >= 0 ? state.downloads[i] : null;
      const downloads =
        i >= 0
          ? state.downloads.map((x, j) => (j === i ? d : x))
          : [d, ...state.downloads];
      // A download that just finished or failed while nobody was
      // looking lights the dot; failure outranks success.
      let attention = state.attention;
      if (!state.panelOpen && ended(d) && (!prev || !ended(prev) || prev.state !== d.state)) {
        if (d.state === "failed") attention = "failed";
        else if (attention === "none") attention = "done";
      }
      const activityAt = prev ? state.activityAt : { ...state.activityAt, downloads: Date.now() };
      return { downloads, attention, activityAt };
    }),

  setUploads: (uploads, attention, newActivity) =>
    set((state) => ({
      uploads,
      attention: attention && !state.panelOpen && attention !== "none" ? attention : state.attention,
      activityAt: newActivity ? { ...state.activityAt, uploads: Date.now() } : state.activityAt,
    })),

  removeUpload: (pendingId) =>
    set((state) => ({ uploads: state.uploads.filter((u) => u.pendingId !== pendingId) })),

  clearFinishedUploads: () =>
    set((state) => ({
      uploads: state.uploads.filter((u) => u.state === "uploading" || u.state === "paused"),
    })),

  removeDownloads: (ids) =>
    set((state) => {
      const drop = new Set(ids);
      const downloads = state.downloads.filter((d) => !drop.has(d.id));
      return downloads.length === state.downloads.length ? state : { downloads };
    }),

  openPanel: (tab) =>
    set((state) => {
      let next = tab ?? state.tab;
      if (!tab) {
        const { downloads, uploads } = state.activityAt;
        if (Math.max(downloads, uploads) > state.openedAt) {
          next = uploads > downloads ? "uploads" : "downloads";
        }
      }
      return { panelOpen: true, tab: next, attention: "none", openedAt: Date.now() };
    }),
  closePanel: () => set({ panelOpen: false }),
  setTab: (tab) => set({ tab }),
  noteStarted: () => set((state) => ({ startPulse: state.startPulse + 1 })),
  setDownloadDir: (dir) => set({ downloadDir: dir }),
  setAskDownloadLocation: (ask) => set({ askDownloadLocation: ask }),
}));
