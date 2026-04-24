// Module-level LRU cache of attachment-id → blob: URL.
//
// Virtualization means an ImagePreview for an attachment scrolled out of the
// viewport unmounts, and when the user scrolls back the component remounts
// fresh. If each mount re-fetched the image bytes, rapid scroll-up/down
// would trigger burst IPC calls, defeating the point of caching.
//
// The cache survives unmounts. Eviction revokes the object URL so the
// browser can free the decoded image bytes, bounding peak memory even for
// channels with thousands of images. LRU: any access (get or re-put) moves
// the entry to the "most recent" end via delete+reinsert, which is
// JavaScript's Map iteration order.

const MAX_ENTRIES = 50;
const cache = new Map<number, string>();

export function getCachedImage(attachmentId: number): string | null {
  const url = cache.get(attachmentId);
  if (url === undefined) return null;
  // LRU promotion — delete+reinsert moves to the end of iteration order.
  cache.delete(attachmentId);
  cache.set(attachmentId, url);
  return url;
}

export function cacheImage(attachmentId: number, url: string): void {
  if (cache.has(attachmentId)) return;
  while (cache.size >= MAX_ENTRIES) {
    const oldestKey = cache.keys().next();
    if (oldestKey.done) break;
    const stale = cache.get(oldestKey.value);
    if (stale !== undefined) URL.revokeObjectURL(stale);
    cache.delete(oldestKey.value);
  }
  cache.set(attachmentId, url);
}

/** For dev / hot-reload hygiene — not called in normal flow. */
export function clearImageCache(): void {
  for (const url of cache.values()) URL.revokeObjectURL(url);
  cache.clear();
}
