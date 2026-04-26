import { invoke } from "@tauri-apps/api/core";

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

// Holds full-resolution image bytes (Blob object URLs). Each entry can
// be many megabytes for high-quality originals, so the cap stays small
// — the working set for the viewer is "current + a couple of
// neighbours", and inline previews use the much smaller server
// thumbnails via attachmentThumbnailCache.
const MAX_ENTRIES = 8;
const cache = new Map<number, string>();
// Concurrent fetches for the same id collapse to a single promise.
// Without this, a rapid re-mount during scroll (or a viewer fetch
// racing the inline preview) fans out duplicate IPC calls that each
// transit the full image bytes through Rust → IPC bridge before the
// `cancelled` flag prevents the second one from being used.
const inflight = new Map<number, Promise<string>>();

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

/**
 * Returns a cached blob URL for an attachment, fetching from the server
 * if not present. Used by the full-screen viewer when navigating to an
 * image that hasn't been loaded inline yet.
 */
export function getOrFetchImage(
  serverId: string,
  attachmentId: number,
  mime: string,
): Promise<string> {
  const cached = getCachedImage(attachmentId);
  if (cached) return Promise.resolve(cached);
  const pending = inflight.get(attachmentId);
  if (pending) return pending;

  const promise = invoke<ArrayBuffer>("fetch_attachment_bytes", {
    serverId,
    attachmentId,
  })
    .then((buf) => {
      const blob = new Blob([buf], { type: mime || "application/octet-stream" });
      const url = URL.createObjectURL(blob);
      cacheImage(attachmentId, url);
      return url;
    })
    .finally(() => {
      inflight.delete(attachmentId);
    });
  inflight.set(attachmentId, promise);
  return promise;
}

/**
 * Push the bytes of an attachment to the OS clipboard. Uses the LRU
 * blob if cached, otherwise fetches via `getOrFetchImage` first. The
 * actual clipboard write happens in Rust (arboard / wl-copy).
 */
export async function copyAttachmentToClipboard(
  serverId: string,
  attachmentId: number,
  mime: string = "image/png",
): Promise<void> {
  let blobUrl = getCachedImage(attachmentId);
  if (!blobUrl) {
    blobUrl = await getOrFetchImage(serverId, attachmentId, mime);
  }
  const blob = await fetch(blobUrl).then((r) => r.blob());
  const buf = await blob.arrayBuffer();
  await invoke("copy_image_to_clipboard", {
    bytes: Array.from(new Uint8Array(buf)),
  });
}
