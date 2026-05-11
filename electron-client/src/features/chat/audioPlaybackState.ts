import { useChatStore } from "../../stores/chatStore";
import { useActiveAudioStore } from "../../stores/activeAudioStore";

// Per-channel cache of audio playback state — last position only.
// Mirrors videoPlaybackState but with no poster bookkeeping (audio
// has no visual preview frame).
//
// Tauri-era equivalent (tempAudioCache.ts) also held a temp-file path
// + cleanup_temp_attachment IPC for downloaded bytes; PR8's electron
// build uses `decibell-attachment://` URLs through Chromium's HTTP
// cache, so we only keep the playback-state half.

interface CacheEntry {
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
  // LRU promote.
  const entry = list[idx];
  list.splice(idx, 1);
  list.push(entry);
  return entry;
}

/// Read-only lookup that does NOT promote the entry in the LRU. Used
/// by the inactive AudioPlayer rows to display the paused-at position
/// without distorting eviction order on every render.
export function peekCachedAudio(
  channelId: string,
  attachmentId: number,
): CacheEntry | undefined {
  return byChannel.get(channelId)?.find((e) => e.attachmentId === attachmentId);
}

export function cacheAudio(channelId: string, entry: { attachmentId: number; lastTime?: number }): void {
  let list = byChannel.get(channelId);
  if (!list) {
    list = [];
    byChannel.set(channelId, list);
  }
  if (list.some((e) => e.attachmentId === entry.attachmentId)) return;
  list.push({
    attachmentId: entry.attachmentId,
    lastTime: entry.lastTime ?? 0,
  });
  while (list.length > MAX_PER_CHANNEL) list.shift();
}

/// Update the playback position of a cached entry. Called from the
/// persistent audio layer on `timeupdate` so a future replay of the
/// same attachment can resume where it left off.
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
  const active = useActiveAudioStore.getState().active;
  if (active && active.channelId === channelId) {
    useActiveAudioStore.getState().setActive(null);
  }
  byChannel.delete(channelId);
}

let lastActiveChannelId: string | null = useChatStore.getState().activeChannelId;
useChatStore.subscribe((state) => {
  const next = state.activeChannelId;
  if (next === lastActiveChannelId) return;
  if (lastActiveChannelId) clearChannel(lastActiveChannelId);
  lastActiveChannelId = next;
});
