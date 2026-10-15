// Per-user avatar cache, keyed by account uid (a rename keeps the cache).
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
//   - avatar_changed: setVersion for the broadcasting user (our own comes
//     from upload_avatar, central tells only an account's watchers)
//   - login: seed for our own uid (we don't watch ourselves, so nothing
//     tells us its version)
//
// See docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md §7.

import { create } from "zustand";
import { invoke } from "../lib/ipc";
import type { Uid } from "./usersStore";

type AvatarStatus = "idle" | "loading" | "loaded" | "missing" | "error";

interface AvatarEntry {
  version: string;
  data: Uint8Array | null;
  blobUrl: string | null;
  status: AvatarStatus;
}

interface AvatarStoreState {
  entries: Map<Uid, AvatarEntry>;
  /// Update the known current version for a user. If it differs from
  /// the cached entry's version, invalidate (revoke blobUrl, mark
  /// idle) so the next render fetches fresh.
  setVersion: (uid: Uid, version: string) => void;
  /// Batched setVersion — one Map copy and one notify for a whole
  /// friend / presence list instead of one per user.
  setVersions: (list: { uid: Uid; version: string }[]) => void;
  /// Fetch `uid`'s picture without knowing its version (our own, at
  /// login): an idle entry whose version the fetch fills in. No-op when
  /// there is an entry already.
  seed: (uid: Uid) => void;
  /// Trigger a fetch if the entry is `idle` (just-invalidated or
  /// never-seen). No-op when already loading/loaded/missing/error.
  fetchIfNeeded: (uid: Uid) => void;
  /// Drop the cached entry, revoke its blob URL, mark idle. Internal
  /// helper called by setVersion when versions differ; also exposed
  /// for forcing a re-fetch.
  invalidate: (uid: Uid) => void;
  /// Logout cleanup: revoke every blob URL, clear the map.
  clearAll: () => void;
}

export const useAvatarStore = create<AvatarStoreState>((set, get) => ({
  entries: new Map(),

  setVersion: (uid, version) => {
    if (!(uid > 0)) return;
    const existing = get().entries.get(uid);
    if (existing && existing.version === version) return;
    // Versions differ (or no entry). Revoke + reset.
    if (existing?.blobUrl) URL.revokeObjectURL(existing.blobUrl);
    set((s) => {
      const next = new Map(s.entries);
      next.set(uid, {
        version,
        data: null,
        blobUrl: null,
        status: "idle",
      });
      return { entries: next };
    });
  },

  setVersions: (list) => {
    const cur = get().entries;
    let next: Map<Uid, AvatarEntry> | null = null;
    for (const { uid, version } of list) {
      if (!(uid > 0)) continue;
      const existing = cur.get(uid);
      if (existing && existing.version === version) continue;
      if (existing?.blobUrl) URL.revokeObjectURL(existing.blobUrl);
      if (!next) next = new Map(cur);
      next.set(uid, { version, data: null, blobUrl: null, status: "idle" });
    }
    if (next) set({ entries: next });
  },

  seed: (uid) => {
    if (!(uid > 0) || get().entries.has(uid)) return;
    set((s) => {
      const next = new Map(s.entries);
      next.set(uid, { version: "", data: null, blobUrl: null, status: "idle" });
      return { entries: next };
    });
  },

  fetchIfNeeded: (uid) => {
    const entry = get().entries.get(uid);
    // Only fetch from `idle`. Loading / loaded / missing / error skip.
    if (!entry || entry.status !== "idle") return;
    set((s) => {
      const next = new Map(s.entries);
      const cur = next.get(uid);
      if (cur) next.set(uid, { ...cur, status: "loading" });
      return { entries: next };
    });
    void (async () => {
      try {
        const result = (await invoke("fetch_avatar", { uid })) as {
          version: string;
          data: Uint8Array;
        };
        let blobUrl: string | null = null;
        if (result.version && result.data.byteLength > 0) {
          blobUrl = URL.createObjectURL(
            new Blob([result.data as BlobPart], { type: "image/jpeg" }),
          );
          // Decode before flipping to `loaded`: UserAvatar swaps the letter
          // for a brand-new <img>, and an undecoded one paints a blank frame
          // between the two. Same fix as the attachment prefetch.
          const img = new Image();
          img.src = blobUrl;
          await img.decode().catch(() => {});
        }
        set((s) => {
          const next = new Map(s.entries);
          const cur = next.get(uid);
          if (!blobUrl) {
            next.set(uid, {
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
            next.set(uid, {
              version: result.version,
              // Drop the raw bytes once the blob URL exists — only blobUrl
              // is ever consumed, so keeping `data` doubled memory per
              // avatar and grew monotonically over a session.
              data: null,
              blobUrl,
              status: "loaded",
            });
          }
          return { entries: next };
        });
      } catch (e) {
        console.warn(`[avatarStore] fetch failed for uid ${uid}:`, e);
        set((s) => {
          const next = new Map(s.entries);
          const cur = next.get(uid);
          if (cur) next.set(uid, { ...cur, status: "error" });
          return { entries: next };
        });
      }
    })();
  },

  invalidate: (uid) => {
    const entry = get().entries.get(uid);
    if (entry?.blobUrl) URL.revokeObjectURL(entry.blobUrl);
    set((s) => {
      const next = new Map(s.entries);
      next.delete(uid);
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
