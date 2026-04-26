import { invoke } from "@tauri-apps/api/core";

// Module-level LRU cache of server-side thumbnail object URLs, keyed by
// attachment id. Used for both image and video thumbnails — the server
// stores them at the same `<storage_path>.thumb.jpg` location and the
// fetch path is identical. Survives Virtuoso row unmounts so scrolling
// away and back doesn't re-download. Capped so a long history doesn't
// pile up object URLs forever. In-flight fetches are deduped so a
// rapid re-mount of the same row doesn't fan out duplicate IPC calls.

const MAX_ENTRIES = 60;
const urlByAttachment = new Map<number, string>();
const inflight = new Map<number, Promise<string | null>>();

function promote(id: number, url: string): void {
  // Re-insert to move to end (Map iteration order = insertion order).
  urlByAttachment.delete(id);
  urlByAttachment.set(id, url);
  while (urlByAttachment.size > MAX_ENTRIES) {
    const oldest = urlByAttachment.keys().next().value;
    if (oldest === undefined) break;
    const dropped = urlByAttachment.get(oldest);
    if (dropped) URL.revokeObjectURL(dropped);
    urlByAttachment.delete(oldest);
  }
}

export function getCachedThumbnail(attachmentId: number): string | null {
  return urlByAttachment.get(attachmentId) ?? null;
}

export function fetchThumbnail(
  serverId: string,
  attachmentId: number,
): Promise<string | null> {
  const cached = urlByAttachment.get(attachmentId);
  if (cached) return Promise.resolve(cached);
  const pending = inflight.get(attachmentId);
  if (pending) return pending;

  const promise = invoke<ArrayBuffer>("fetch_attachment_thumbnail", {
    serverId,
    attachmentId,
  })
    .then((buf) => {
      const blob = new Blob([buf], { type: "image/jpeg" });
      const url = URL.createObjectURL(blob);
      promote(attachmentId, url);
      return url;
    })
    .catch((err) => {
      console.warn("[video-thumb] fetch failed", { attachmentId, err });
      return null;
    })
    .finally(() => {
      inflight.delete(attachmentId);
    });
  inflight.set(attachmentId, promise);
  return promise;
}
