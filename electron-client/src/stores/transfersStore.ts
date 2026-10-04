import { create } from "zustand";
import type { DownloadView } from "../types";

// The Transfers panel's state (features/transfers/). Downloads are a
// mirror of main's download manager: seeded by `list()` and patched by
// its `downloads_changed` / `downloads_removed` events (see
// initTransfers). Main is the source of truth; nothing here persists.

export type TransfersTab = "downloads" | "uploads";

/// The title-bar button's dot: something ended while the panel was shut.
export type TransfersAttention = "none" | "done" | "failed";

interface TransfersState {
  /// Newest first.
  downloads: DownloadView[];
  panelOpen: boolean;
  tab: TransfersTab;
  attention: TransfersAttention;
  /// Bumped when a download starts; the button replays its arrow drop.
  startPulse: number;

  /// Settings → Network → Downloads. "" = the OS Downloads folder.
  downloadDir: string;
  askDownloadLocation: boolean;

  setDownloads: (list: DownloadView[]) => void;
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
      return { downloads, attention };
    }),

  removeDownloads: (ids) =>
    set((state) => {
      const drop = new Set(ids);
      const downloads = state.downloads.filter((d) => !drop.has(d.id));
      return downloads.length === state.downloads.length ? state : { downloads };
    }),

  openPanel: (tab) =>
    set((state) => ({ panelOpen: true, tab: tab ?? state.tab, attention: "none" })),
  closePanel: () => set({ panelOpen: false }),
  setTab: (tab) => set({ tab }),
  noteStarted: () => set((state) => ({ startPulse: state.startPulse + 1 })),
  setDownloadDir: (dir) => set({ downloadDir: dir }),
  setAskDownloadLocation: (ask) => set({ askDownloadLocation: ask }),
}));
