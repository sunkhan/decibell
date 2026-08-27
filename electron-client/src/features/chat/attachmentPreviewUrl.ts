// Preview URL selection for attachment previews.
//
// Used to also warm previews ahead of the visible range: under the
// virtualized list an <img> could not exist before its row mounted, so the
// fetch had to be decoupled from the mount. With the real-DOM list rows
// mount NEAR_PX (800px) before they scroll into view and their <img> fetch
// starts then — that is the prefetch — so only the URL helper remains.

import type { Attachment } from "../../types";
import { buildAttachmentUrl, pickThumbnailSize } from "./attachmentHelpers";
import { reserveBox, type ChatViewSize } from "./attachmentSizing";

/**
 * The exact URL `ImageItem` / `VideoItem` put in `src` — the thumbnail size
 * picked from the reserved box at the current devicePixelRatio, falling
 * back to the full content when the server has no thumbnail for it.
 */
export function previewUrlFor(
  attachment: Attachment,
  serverId: string | null,
  viewSize: ChatViewSize | null,
): string | null {
  const full = buildAttachmentUrl(serverId, attachment);
  if (!full) return null;
  // A GIF has to be the original: the upload-time thumbnail is a JPEG of
  // its first frame, and a frozen GIF in the chat is a bug, not a preview.
  if (attachment.mime === "image/gif") return full;
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
