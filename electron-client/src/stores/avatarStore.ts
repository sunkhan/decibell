// Per-user avatar cache, keyed by account uid (a rename keeps the cache).
//
// Cache shape:
//   { version, blobUrl, status }
//
// status state machine:
//   idle    — version known but bytes not fetched yet (just invalidated,
//             never fetched, or evicted)
//   loading — fetch queued or fetch_avatar call in flight
//   loaded  — bytes fetched, blobUrl ready for <img>
//   missing — the user has no avatar (central said version '')
//   error   — fetch failed; letter on render, retried when central next
//             names the version
//
// Where versions come from:
//   - friend_list_received: each FriendInfo
//   - user_list_updated (subscription answers): every covered user, the
//     visible ones from their presence entries, the rest from `avatars`;
//     a covered user in neither has no avatar
//   - presence_delta / avatar_changed for watched users (our own comes from
//     upload_avatar, central tells only an account's watchers)
//   - login: seed for our own uid (we don't watch ourselves, so nothing
//     tells us its version)
//
// Fetches run a few at a time, newest first, and skip users no longer on
// screen: a fast scroll through a long member list fetches what stays in
// view, not everything it passed. Past a cap, loaded pictures of users off
// screen are evicted (bytes dropped, version kept), so a long session
// doesn't hold every picture it ever showed.
//
// See docs/superpowers/specs/2026-05-12-custom-profile-pictures-design.md §7.

import { create } from "zustand";
import { invoke } from "../lib/ipc";
import { isOnScreen } from "../features/presence/presenceInterest";
import type { Uid } from "./usersStore";

type AvatarStatus = "idle" | "loading" | "loaded" | "missing" | "error";

interface AvatarEntry {
  version: string;
  blobUrl: string | null;
  status: AvatarStatus;
}

/// Fetches in flight at once: enough to fill a list quickly without
/// queueing a screenful of requests behind each other on the central
/// connection (each times out after 5 s).
const MAX_IN_FLIGHT = 6;
/// Loaded pictures kept; beyond this, the oldest of users off screen go.
const MAX_LOADED = 400;

/// Uids waiting to be fetched; the newest request goes first.
const queue: Uid[] = [];
const queued = new Set<Uid>();
let inFlight = 0;
/// Bumped by clearAll: fetches started before it drop their result.
let generation = 0;

interface AvatarStoreState {
  entries: Map<Uid, AvatarEntry>;
  /// Update the known current version for a user. If it differs from
  /// the cached entry's version, invalidate (revoke blobUrl, mark idle,
  /// or missing for '') so the next render fetches fresh.
  setVersion: (uid: Uid, version: string) => void;
  /// Batched setVersion — one Map copy and one notify for a whole
  /// friend / presence list instead of one per user.
  setVersions: (list: { uid: Uid; version: string }[]) => void;
  /// Fetch `uid`'s picture without knowing its version (our own, at
  /// login): an idle entry whose version the fetch fills in. No-op when
  /// there is an entry already.
  seed: (uid: Uid) => void;
  /// Queue a fetch if the entry is `idle` (just-invalidated, never
  /// fetched or evicted). No-op otherwise.
  fetchIfNeeded: (uid: Uid) => void;
  /// Logout cleanup: revoke every blob URL, clear the map.
  clearAll: () => void;
}

/// The entry `version` leads to, or null when `existing` already reflects
/// it. A failed fetch is retried on the next mention.
function entryFor(existing: AvatarEntry | undefined, version: string): AvatarEntry | null {
  if (existing && existing.version === version && existing.status !== "error") return null;
  return { version, blobUrl: null, status: version ? "idle" : "missing" };
}

export const useAvatarStore = create<AvatarStoreState>((set, get) => ({
  entries: new Map(),

  setVersion: (uid, version) => get().setVersions([{ uid, version }]),

  setVersions: (list) => {
    const cur = get().entries;
    let next: Map<Uid, AvatarEntry> | null = null;
    for (const { uid, version } of list) {
      if (!(uid > 0)) continue;
      const existing = (next ?? cur).get(uid);
      const entry = entryFor(existing, version);
      if (!entry) continue;
      if (existing?.blobUrl) URL.revokeObjectURL(existing.blobUrl);
      if (!next) next = new Map(cur);
      next.set(uid, entry);
    }
    if (next) set({ entries: next });
  },

  seed: (uid) => {
    if (!(uid > 0) || get().entries.has(uid)) return;
    const next = new Map(get().entries);
    next.set(uid, { version: "", blobUrl: null, status: "idle" });
    set({ entries: next });
  },

  fetchIfNeeded: (uid) => {
    const entry = get().entries.get(uid);
    if (!entry || entry.status !== "idle") return;
    patch(uid, (e) => ({ ...e, status: "loading" }));
    if (!queued.has(uid)) {
      queued.add(uid);
      queue.push(uid);
    }
    pump();
  },

  clearAll: () => {
    generation++;
    queue.length = 0;
    queued.clear();
    for (const e of get().entries.values()) {
      if (e.blobUrl) URL.revokeObjectURL(e.blobUrl);
    }
    set({ entries: new Map() });
  },
}));

/// Replace one entry, if it exists.
function patch(uid: Uid, change: (e: AvatarEntry) => AvatarEntry): void {
  const cur = useAvatarStore.getState().entries;
  const e = cur.get(uid);
  if (!e) return;
  const next = new Map(cur);
  next.set(uid, change(e));
  useAvatarStore.setState({ entries: next });
}

function pump(): void {
  while (inFlight < MAX_IN_FLIGHT && queue.length > 0) {
    const uid = queue.pop()!;
    queued.delete(uid);
    const entry = useAvatarStore.getState().entries.get(uid);
    if (!entry || entry.status !== "loading") continue;
    if (!isOnScreen(uid)) {
      // Scrolled away while queued: fetched when it shows again.
      patch(uid, (e) => ({ ...e, status: "idle" }));
      continue;
    }
    inFlight++;
    void fetchOne(uid, entry.version, generation).finally(() => {
      inFlight--;
      pump();
    });
  }
}

async function fetchOne(uid: Uid, asked: string, gen: number): Promise<void> {
  let blobUrl: string | null = null;
  try {
    const result = (await invoke("fetch_avatar", { uid })) as { version: string; data: Uint8Array };
    if (result.version && result.data.byteLength > 0) {
      blobUrl = URL.createObjectURL(new Blob([result.data as BlobPart], { type: "image/jpeg" }));
      // Decode before flipping to `loaded`: UserAvatar swaps the letter
      // for a brand-new <img>, and an undecoded one paints a blank frame
      // between the two. Same fix as the attachment prefetch.
      const img = new Image();
      img.src = blobUrl;
      await img.decode().catch(() => {});
    }
    const cur = useAvatarStore.getState().entries.get(uid);
    // Logged out meanwhile, or a newer version was announced while this
    // was in flight (that one is fetched next).
    if (gen !== generation || !cur || cur.version !== asked) {
      if (blobUrl) URL.revokeObjectURL(blobUrl);
      return;
    }
    if (cur.blobUrl) URL.revokeObjectURL(cur.blobUrl);
    const next = new Map(useAvatarStore.getState().entries);
    // Re-inserted, so the map runs from least to most recently loaded.
    next.delete(uid);
    next.set(uid, {
      version: result.version,
      blobUrl,
      status: blobUrl ? "loaded" : "missing",
    });
    useAvatarStore.setState({ entries: next });
    if (blobUrl) evictOffScreen();
  } catch (e) {
    if (blobUrl) URL.revokeObjectURL(blobUrl);
    if (gen !== generation) return;
    console.warn(`[avatarStore] fetch failed for uid ${uid}:`, e);
    patch(uid, (cur) => (cur.version === asked && cur.status === "loading" ? { ...cur, status: "error" } : cur));
  }
}

/// Past MAX_LOADED, drop the bytes of the least recently loaded pictures
/// of users off screen; they're fetched again if they come back.
function evictOffScreen(): void {
  const cur = useAvatarStore.getState().entries;
  let loaded = 0;
  for (const e of cur.values()) if (e.status === "loaded") loaded++;
  let excess = loaded - MAX_LOADED;
  if (excess <= 0) return;
  let next: Map<Uid, AvatarEntry> | null = null;
  for (const [uid, e] of cur) {
    if (excess === 0) break;
    if (e.status !== "loaded" || isOnScreen(uid)) continue;
    if (e.blobUrl) URL.revokeObjectURL(e.blobUrl);
    if (!next) next = new Map(cur);
    next.set(uid, { version: e.version, blobUrl: null, status: "idle" });
    excess--;
  }
  if (next) useAvatarStore.setState({ entries: next });
}
