// Warms attachment previews into Chromium's HTTP cache before the row
// that shows them mounts.
//
// `increaseViewportBy` mounts rows early, but the fetch still starts at
// mount — at any real scroll speed that lead is a fraction of a second,
// so the first pass over an image still shows an empty box until the
// round-trip lands. Virtualization guarantees this: the <img> cannot
// exist before the row does.
//
// The fix is to stop tying the fetch to the mount. Given the visible
// range, we warm the previews for messages either side of it, so by the
// time a row mounts its bytes are already cached and the image paints
// in the same frame. This only works because the attachment protocol
// now serves a cacheable response (see electron/main/protocol.ts) —
// without that every warmed request would be re-fetched on mount anyway.
//
// Discord gets the same effect with one extra trick we can't do
// client-side: its attachment payload carries a ~30-byte thumbhash, so
// the box shows a blurred preview instantly with zero requests. That
// would need a protocol/server change here; prefetching is the part
// that's ours to fix.

import type { Attachment, Message } from "../../types";
import { buildAttachmentUrl, pickThumbnailSize } from "./attachmentHelpers";
import { reserveBox, type ChatViewSize } from "./attachmentSizing";

/// How many messages either side of the visible range to warm. Deep
/// enough to cover a fast fling, cheap because a warmed request that
/// never gets used costs one cached response.
const PREFETCH_RADIUS = 15;

/// URLs already requested. Bounded so a long session doesn't grow it
/// without limit; eviction just means a possible re-request, and the
/// HTTP cache still answers that.
const warmed = new Set<string>();
const WARMED_MAX = 600;

/**
 * The exact URL `ImageItem` / `VideoItem` will put in `src`.
 *
 * Shared with those components rather than reimplemented: a prefetch
 * that computes a different thumbnail size than the one actually
 * requested is worse than no prefetch — it doubles the requests and
 * still misses the cache.
 */
export function previewUrlFor(
  attachment: Attachment,
  serverId: string | null,
  viewSize: ChatViewSize | null,
): string | null {
  const full = buildAttachmentUrl(serverId, attachment);
  if (!full) return null;
  const box = reserveBox(attachment, viewSize);
  const targetPx =
    box.width > 0
      ? Math.round(Math.max(box.width, box.height) * (window.devicePixelRatio || 1))
      : 640;
  const picked = pickThumbnailSize(attachment.thumbnailSizesMask, targetPx);
  if (picked !== null && attachment.thumbnailSizeBytes > 0) {
    return buildAttachmentUrl(serverId, attachment, { thumb: true, size: picked });
  }
  return full;
}

function warm(url: string): void {
  if (warmed.has(url)) return;
  if (warmed.size >= WARMED_MAX) {
    // Cheap FIFO: drop the oldest quarter rather than tracking real LRU.
    let n = WARMED_MAX / 4;
    for (const old of warmed) {
      warmed.delete(old);
      if (--n <= 0) break;
    }
  }
  warmed.add(url);
  // A detached Image is enough to populate the HTTP cache; it is never
  // added to the DOM and drops out of scope once the load settles.
  const img = new Image();
  img.decoding = "async";
  img.src = url;
}

/**
 * Warm previews around the visible range.
 *
 * `start`/`end` are indices into `messages` — rebase Virtuoso's
 * absolute indices before calling.
 */
export function prefetchAround(
  messages: Message[],
  start: number,
  end: number,
  serverId: string | null,
  viewSize: ChatViewSize | null,
): void {
  const from = Math.max(0, start - PREFETCH_RADIUS);
  const to = Math.min(messages.length - 1, end + PREFETCH_RADIUS);
  for (let i = from; i <= to; i++) {
    const attachments = messages[i]?.attachments;
    if (!attachments || attachments.length === 0) continue;
    for (const a of attachments) {
      // Tombstones have nothing to fetch, and audio/documents don't
      // render a preview image.
      if (a.purgedAt !== 0) continue;
      if (a.kind !== "image" && a.kind !== "video") continue;
      const url = previewUrlFor(a, serverId, viewSize);
      if (url) warm(url);
    }
  }
}
