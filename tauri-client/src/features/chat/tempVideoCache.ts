import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useActiveVideoStore } from "../../stores/activeVideoStore";
import { useVideoCacheVersionStore } from "../../stores/videoCacheVersionStore";

// Per-channel cache of downloaded video temp files. While the user is
// in a channel, replaying a video they already opened reuses the same
// temp file — no re-download. Capped at MAX_PER_CHANNEL so a channel
// with many videos doesn't grow the disk footprint without bound.
//
// Lifecycle:
//   - set on successful download
//   - LRU eviction within channel deletes the dropped entry's temp file
//   - channel switch clears all entries for the previous channel
//   - app exit / startup sweep handles anything that fell through
//
// Each entry also stores the last-known playback position so that
// scrolling away from a previously-playing video and starting a
// different one preserves where the first one was when it stopped.

interface CacheEntry {
  path: string;
  url: string;
  attachmentId: number;
  lastTime: number;
  wasPlaying: boolean;
  // Captured frame (object URL) for the placeholder's "paused" look
  // when this video isn't the active one. Set by PersistentVideoLayer
  // on cleanup; revoked when the entry is evicted.
  posterUrl: string | null;
}

const MAX_PER_CHANNEL = 5;
const byChannel = new Map<string, CacheEntry[]>();

export function getCachedVideo(
  channelId: string,
  attachmentId: number,
): CacheEntry | undefined {
  const list = byChannel.get(channelId);
  if (!list) return undefined;
  const idx = list.findIndex((e) => e.attachmentId === attachmentId);
  if (idx === -1) return undefined;
  // LRU promote — move to end so eviction targets least-recently-used.
  const entry = list[idx];
  list.splice(idx, 1);
  list.push(entry);
  return entry;
}

export function cacheVideo(channelId: string, entry: CacheEntry): void {
  let list = byChannel.get(channelId);
  if (!list) {
    list = [];
    byChannel.set(channelId, list);
  }
  if (list.some((e) => e.attachmentId === entry.attachmentId)) return;
  list.push(entry);
  while (list.length > MAX_PER_CHANNEL) {
    const evicted = list.shift()!;
    if (evicted.posterUrl) URL.revokeObjectURL(evicted.posterUrl);
    invoke("cleanup_temp_attachment", { path: evicted.path }).catch(() => {});
  }
}

/** Update the playback position of a cached entry. Called from the
 *  persistent video layer on `timeupdate` so a future replay of the
 *  same attachment can resume where it left off. */
export function updateCachedVideoState(
  channelId: string,
  attachmentId: number,
  lastTime: number,
  wasPlaying: boolean,
): void {
  const list = byChannel.get(channelId);
  if (!list) return;
  const entry = list.find((e) => e.attachmentId === attachmentId);
  if (entry) {
    entry.lastTime = lastTime;
    entry.wasPlaying = wasPlaying;
  }
}

function clearChannel(channelId: string): void {
  const list = byChannel.get(channelId);
  if (!list) return;
  // If the active video belongs to this channel, stop it before its
  // temp file gets deleted out from under the persistent player.
  const active = useActiveVideoStore.getState().active;
  if (active && active.channelId === channelId) {
    useActiveVideoStore.getState().setActive(null);
    useActiveVideoStore.getState().setHostElement(null);
  }
  for (const entry of list) {
    if (entry.posterUrl) URL.revokeObjectURL(entry.posterUrl);
    invoke("cleanup_temp_attachment", { path: entry.path }).catch(() => {});
  }
  byChannel.delete(channelId);
}

/** Stash the captured poster frame URL on a cached entry. The previous
 *  url (if any) is revoked. */
export function setVideoPoster(
  channelId: string,
  attachmentId: number,
  posterUrl: string,
): void {
  const list = byChannel.get(channelId);
  if (!list) {
    URL.revokeObjectURL(posterUrl);
    return;
  }
  const entry = list.find((e) => e.attachmentId === attachmentId);
  if (!entry) {
    URL.revokeObjectURL(posterUrl);
    return;
  }
  if (entry.posterUrl) URL.revokeObjectURL(entry.posterUrl);
  entry.posterUrl = posterUrl;
  // The cache is a plain Map — bump the reactivity store so any
  // VideoPlayer placeholder currently mounted for this attachment
  // re-renders and picks up the new poster.
  useVideoCacheVersionStore.getState().bump();
}

// Watch for channel switches and clear the previous channel's cache.
// Module-level subscription so this runs once and survives forever.
let lastActiveChannelId: string | null = useChatStore.getState().activeChannelId;
useChatStore.subscribe((state) => {
  const next = state.activeChannelId;
  if (next === lastActiveChannelId) return;
  if (lastActiveChannelId) clearChannel(lastActiveChannelId);
  lastActiveChannelId = next;
});
