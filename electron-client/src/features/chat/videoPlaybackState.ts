import { useChatStore } from "../../stores/chatStore";
import { useActiveVideoStore } from "../../stores/activeVideoStore";
import { useVideoCacheVersionStore } from "../../stores/videoCacheVersionStore";

// Per-channel cache of video playback state — last position, was-it-
// playing, captured poster frame for the placeholder when this video
// isn't the active one. Bounded per channel so memory stays under
// control even in heavy-attachment channels.
//
// Tauri-era equivalent (tempVideoCache.ts) also held a temp-file path
// + native cleanup IPC for the downloaded bytes. PR8's electron build
// uses `decibell-attachment://` URLs that go through Chromium's HTTP
// cache, so re-fetches are free and there are no native temp files to
// clean up. We only keep the playback-state half.
//
// Lifecycle:
//   - cacheVideo on first play
//   - LRU eviction within channel revokes any captured poster URL
//   - channel switch clears the previous channel's entries
//   - logout / store-reset wipes everything

interface CacheEntry {
  attachmentId: number;
  lastTime: number;
  wasPlaying: boolean;
  /// Captured frame (object URL) for the placeholder's "paused" look
  /// when this video isn't the active one. Set by PersistentVideoLayer
  /// on cleanup; revoked when the entry is evicted.
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

export function cacheVideo(
  channelId: string,
  entry: { attachmentId: number; lastTime?: number; wasPlaying?: boolean },
): void {
  let list = byChannel.get(channelId);
  if (!list) {
    list = [];
    byChannel.set(channelId, list);
  }
  if (list.some((e) => e.attachmentId === entry.attachmentId)) return;
  list.push({
    attachmentId: entry.attachmentId,
    lastTime: entry.lastTime ?? 0,
    wasPlaying: entry.wasPlaying ?? false,
    posterUrl: null,
  });
  while (list.length > MAX_PER_CHANNEL) {
    const evicted = list.shift()!;
    if (evicted.posterUrl) URL.revokeObjectURL(evicted.posterUrl);
  }
}

/// Update the playback position of a cached entry. Called from the
/// persistent video layer on `timeupdate` so a future replay of the
/// same attachment can resume where it left off.
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
  // entry gets dropped — the persistent player would otherwise keep
  // playing a "ghost" video that nothing in the chat tree references.
  const active = useActiveVideoStore.getState().active;
  if (active && active.channelId === channelId) {
    useActiveVideoStore.getState().setActive(null);
    useActiveVideoStore.getState().setHostElement(null);
  }
  for (const entry of list) {
    if (entry.posterUrl) URL.revokeObjectURL(entry.posterUrl);
  }
  byChannel.delete(channelId);
}

/// Stash the captured poster frame URL on a cached entry. The previous
/// url (if any) is revoked.
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
  // Bump the reactivity store so any VideoPlayer placeholder
  // currently mounted for this attachment re-renders and picks up
  // the new poster.
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
