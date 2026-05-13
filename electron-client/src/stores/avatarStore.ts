// Per-user avatar cache, keyed by username.
//
// Cache shape:
//   { version, data, blobUrl, status }
//
// status state machine:
//   idle    — version known but bytes not fetched yet (or just invalidated)
//   loading — fetch_avatar napi call in flight
//   loaded  — bytes fetched, blobUrl ready for <img>
//   missing — user has no avatar (server returned version='' + empty data)
//   error   — fetch failed; falls back to letter on render
//
// Cache invalidation drivers (all in Task 19's listener wiring):
//   - friend_list_received: setVersion for each FriendInfo
//   - user_list_updated (presence): setVersion for each UserPresence
//   - avatar_changed: setVersion for the broadcasting user
//
// See docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md §7.

import { create } from "zustand";
import { invoke } from "../lib/ipc";

type AvatarStatus = "idle" | "loading" | "loaded" | "missing" | "error";

interface AvatarEntry {
  version: string;
  data: Uint8Array | null;
  blobUrl: string | null;
  status: AvatarStatus;
}

interface AvatarStoreState {
  entries: Map<string, AvatarEntry>;
  /// Update the known current version for a user. If it differs from
  /// the cached entry's version, invalidate (revoke blobUrl, mark
  /// idle) so the next render fetches fresh.
  setVersion: (username: string, version: string) => void;
  /// Trigger a fetch if the entry is `idle` (just-invalidated or
  /// never-seen). No-op when already loading/loaded/missing/error.
  fetchIfNeeded: (username: string) => void;
  /// Drop the cached entry, revoke its blob URL, mark idle. Internal
  /// helper called by setVersion when versions differ; also exposed
  /// for forcing a re-fetch.
  invalidate: (username: string) => void;
  /// Logout cleanup: revoke every blob URL, clear the map.
  clearAll: () => void;
}

export const useAvatarStore = create<AvatarStoreState>((set, get) => ({
  entries: new Map(),

  setVersion: (username, version) => {
    const existing = get().entries.get(username);
    if (existing && existing.version === version) return;
    // Versions differ (or no entry). Revoke + reset.
    if (existing?.blobUrl) URL.revokeObjectURL(existing.blobUrl);
    set((s) => {
      const next = new Map(s.entries);
      next.set(username, {
        version,
        data: null,
        blobUrl: null,
        status: "idle",
      });
      return { entries: next };
    });
  },

  fetchIfNeeded: (username) => {
    const entry = get().entries.get(username);
    // Only fetch from `idle`. Loading / loaded / missing / error skip.
    if (!entry || entry.status !== "idle") return;
    set((s) => {
      const next = new Map(s.entries);
      const cur = next.get(username);
      if (cur) next.set(username, { ...cur, status: "loading" });
      return { entries: next };
    });
    void (async () => {
      try {
        const result = (await invoke("fetch_avatar", { username })) as {
          version: string;
          data: Uint8Array;
        };
        set((s) => {
          const next = new Map(s.entries);
          const cur = next.get(username);
          if (!result.version || result.data.byteLength === 0) {
            next.set(username, {
              version: result.version,
              data: null,
              blobUrl: null,
              status: "missing",
            });
          } else {
            // Revoke any blob URL the cache already had for the
            // previous version (defensive — setVersion should have
            // done this on invalidation, but races are cheap to guard).
            if (cur?.blobUrl) URL.revokeObjectURL(cur.blobUrl);
            const blob = new Blob([result.data as BlobPart], {
              type: "image/jpeg",
            });
            const blobUrl = URL.createObjectURL(blob);
            next.set(username, {
              version: result.version,
              data: result.data,
              blobUrl,
              status: "loaded",
            });
          }
          return { entries: next };
        });
      } catch (e) {
        console.warn(`[avatarStore] fetch failed for ${username}:`, e);
        set((s) => {
          const next = new Map(s.entries);
          const cur = next.get(username);
          if (cur) next.set(username, { ...cur, status: "error" });
          return { entries: next };
        });
      }
    })();
  },

  invalidate: (username) => {
    const entry = get().entries.get(username);
    if (entry?.blobUrl) URL.revokeObjectURL(entry.blobUrl);
    set((s) => {
      const next = new Map(s.entries);
      next.delete(username);
      return { entries: next };
    });
  },

  clearAll: () => {
    for (const e of get().entries.values()) {
      if (e.blobUrl) URL.revokeObjectURL(e.blobUrl);
    }
    set({ entries: new Map() });
  },
}));
