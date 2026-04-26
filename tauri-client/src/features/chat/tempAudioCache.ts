import { invoke } from "@tauri-apps/api/core";
import { useChatStore } from "../../stores/chatStore";
import { useActiveAudioStore } from "../../stores/activeAudioStore";

// Per-channel cache of downloaded audio temp files. Mirrors
// tempVideoCache but with no poster bookkeeping (audio has no
// visual preview frame). While the user is in a channel, re-clicking
// play on a previously-opened audio attachment reuses the same temp
// file and resumes from where it was paused. Capped per channel so
// disk footprint stays bounded.
//
// Lifecycle:
//   - set on successful download
//   - LRU eviction within channel unlinks the dropped entry's file
//   - channel switch clears all entries for the previous channel
//   - app exit / startup sweep handles anything that fell through

interface CacheEntry {
  path: string;
  url: string;
  attachmentId: number;
  lastTime: number;
}

const MAX_PER_CHANNEL = 5;
const byChannel = new Map<string, CacheEntry[]>();

export function getCachedAudio(
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

/** Read-only lookup that does NOT promote the entry in the LRU. Used
 *  by the inactive AudioPlayer rows to display the paused-at position
 *  without distorting eviction order on every render. */
export function peekCachedAudio(
  channelId: string,
  attachmentId: number,
): CacheEntry | undefined {
  return byChannel.get(channelId)?.find((e) => e.attachmentId === attachmentId);
}

export function cacheAudio(channelId: string, entry: CacheEntry): void {
  let list = byChannel.get(channelId);
  if (!list) {
    list = [];
    byChannel.set(channelId, list);
  }
  if (list.some((e) => e.attachmentId === entry.attachmentId)) return;
  list.push(entry);
  while (list.length > MAX_PER_CHANNEL) {
    const evicted = list.shift()!;
    invoke("cleanup_temp_attachment", { path: evicted.path }).catch(() => {});
  }
}

/** Update the playback position of a cached entry. Called from the
 *  persistent audio layer on `timeupdate` so a future replay of the
 *  same attachment can resume where it left off. */
export function updateCachedAudioState(
  channelId: string,
  attachmentId: number,
  lastTime: number,
): void {
  const list = byChannel.get(channelId);
  if (!list) return;
  const entry = list.find((e) => e.attachmentId === attachmentId);
  if (entry) entry.lastTime = lastTime;
}

function clearChannel(channelId: string): void {
  const list = byChannel.get(channelId);
  if (!list) return;
  // If the active audio belongs to this channel, stop it before its
  // temp file gets deleted out from under the persistent player.
  const active = useActiveAudioStore.getState().active;
  if (active && active.channelId === channelId) {
    useActiveAudioStore.getState().setActive(null);
  }
  for (const entry of list) {
    invoke("cleanup_temp_attachment", { path: entry.path }).catch(() => {});
  }
  byChannel.delete(channelId);
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
