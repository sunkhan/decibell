import { create } from "zustand";
import type { LinkPreview } from "../types";

// Renderer-side memo of link unfurls, keyed by the URL as typed. Main
// holds the real cache (with TTLs); this layer exists so a row that
// re-renders or remounts (sliding-window trims, channel switches)
// never round-trips IPC for a URL it has already seen, and so every
// bubble citing the same URL shares one entry.
//
// Entries are only ever added or replaced, never mutated, so a
// selector on `entries[url]` returns a stable reference between
// updates (the zustand no-fresh-refs rule).

export interface LinkPreviewEntry {
  status: "loading" | "done";
  /// null once done = nothing to show (unreachable, no metadata, or
  /// refused by main's destination guard).
  preview: LinkPreview | null;
}

interface LinkPreviewState {
  entries: Record<string, LinkPreviewEntry>;
  /// Idempotent: a URL already loading or done is left alone.
  request: (url: string) => void;
}

const MAX_ENTRIES = 400;

function withEntry(
  entries: Record<string, LinkPreviewEntry>,
  url: string,
  entry: LinkPreviewEntry,
): Record<string, LinkPreviewEntry> {
  const next = { ...entries, [url]: entry };
  const keys = Object.keys(next);
  if (keys.length > MAX_ENTRIES) {
    for (const k of keys.slice(0, keys.length - MAX_ENTRIES)) delete next[k];
  }
  return next;
}

export const useLinkPreviewStore = create<LinkPreviewState>((set, get) => ({
  entries: {},
  request: (url) => {
    if (get().entries[url]) return;
    set((s) => ({ entries: withEntry(s.entries, url, { status: "loading", preview: null }) }));
    window.decibell.linkPreview.fetch(url).then(
      (preview) =>
        set((s) => ({ entries: withEntry(s.entries, url, { status: "done", preview }) })),
      () =>
        set((s) => ({ entries: withEntry(s.entries, url, { status: "done", preview: null }) })),
    );
  },
}));
