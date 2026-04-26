import { invoke } from "@tauri-apps/api/core";
import { THUMB_SIZES, type ThumbSize } from "./imageMetadata";

// Module-level LRU cache of server-side thumbnail object URLs, keyed by
// `${attachmentId}:${size}`. Used for both image and video thumbnails
// since the server stores them at the same locations and the fetch
// path is identical.
//
// The size dimension matters because the same attachment may have
// multiple pre-generated sizes (320 / 640 / 1280 long-edge) and the
// renderer picks one based on the cell's display size and DPR. A grid
// cell at 200 px on a 2× DPR display wants the 640 size; the same
// attachment shown at full chat width wants 1280.
//
// Survives Virtuoso row unmounts so scrolling away and back doesn't
// re-download. Capped so a long history doesn't pile up object URLs
// forever. In-flight fetches are deduped so a rapid re-mount doesn't
// fan out duplicate IPC calls.

const MAX_ENTRIES = 60;
const urlByKey = new Map<string, string>();
const inflight = new Map<string, Promise<string | null>>();

function keyFor(attachmentId: number, size: ThumbSize | null): string {
  return size === null ? `${attachmentId}:auto` : `${attachmentId}:${size}`;
}

function promote(key: string, url: string): void {
  // Re-insert to move to end (Map iteration order = insertion order).
  urlByKey.delete(key);
  urlByKey.set(key, url);
  while (urlByKey.size > MAX_ENTRIES) {
    const oldest = urlByKey.keys().next().value;
    if (oldest === undefined) break;
    const dropped = urlByKey.get(oldest);
    if (dropped) URL.revokeObjectURL(dropped);
    urlByKey.delete(oldest);
  }
}

/** Pick the smallest pre-generated size that's >= the target px from
 *  the bitmask the server reported. Falls back to the largest available
 *  size if every option is below target, then to `null` (server picks)
 *  if the mask says nothing's available — covers legacy uploads where
 *  thumbnailSizeBytes>0 but mask=0. */
export function pickSize(targetPx: number, mask: number): ThumbSize | null {
  const available = THUMB_SIZES.filter((_s, i) => (mask & (1 << i)) !== 0);
  if (available.length === 0) return null; // legacy single-file path
  for (const s of available) {
    if (s >= targetPx) return s;
  }
  return available[available.length - 1];
}

export function getCachedThumbnail(
  attachmentId: number,
  size: ThumbSize | null = null,
): string | null {
  return urlByKey.get(keyFor(attachmentId, size)) ?? null;
}

export function fetchThumbnail(
  serverId: string,
  attachmentId: number,
  size: ThumbSize | null = null,
): Promise<string | null> {
  const key = keyFor(attachmentId, size);
  const cached = urlByKey.get(key);
  if (cached) return Promise.resolve(cached);
  const pending = inflight.get(key);
  if (pending) return pending;

  const promise = invoke<ArrayBuffer>("fetch_attachment_thumbnail", {
    serverId,
    attachmentId,
    size,
  })
    .then((buf) => {
      const blob = new Blob([buf], { type: "image/jpeg" });
      const url = URL.createObjectURL(blob);
      promote(key, url);
      return url;
    })
    .catch((err) => {
      console.warn("[attach-thumb] fetch failed", { attachmentId, size, err });
      return null;
    })
    .finally(() => {
      inflight.delete(key);
    });
  inflight.set(key, promise);
  return promise;
}
