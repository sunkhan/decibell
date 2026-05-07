import type { Attachment } from "../../types";
import { useChatStore } from "../../stores/chatStore";

/// Build a `decibell-attachment://` URL for an attachment. The custom
/// protocol is registered in main; it injects the Authorization
/// header from the registered attachment target and proxies via
/// `net.fetch`, so plain `<img src=…>` / `<video src=…>` work without
/// any renderer-side fetch + Blob URL juggling.
///
/// Returns null when the attachment has been purged or the server
/// didn't advertise an HTTP endpoint at community-auth time.
export function buildAttachmentUrl(
  serverId: string | null,
  attachment: Attachment,
  variant?: { thumb: true; size: 320 | 640 | 1280 },
): string | null {
  if (attachment.purgedAt > 0) return null;
  if (!serverId) return null;
  const chat = useChatStore.getState();
  const config = chat.serverAttachmentConfig?.[serverId];
  if (!config || config.port === 0) return null;
  // URL shape mirrors what the protocol handler in main expects:
  //   decibell-attachment://attach/<serverId>/<attachmentId>?…
  // The pseudo-host "attach" sidesteps Chromium parsing numeric
  // server IDs ("1", "2", …) as IPv4-style hostnames.
  const base = `decibell-attachment://attach/${encodeURIComponent(serverId)}/${attachment.id}`;
  if (variant) {
    return `${base}?variant=thumb&size=${variant.size}`;
  }
  return base;
}

/// Pick the smallest thumbnail size that satisfies the target longest
/// edge. Falls back to the next-larger if a smaller isn't available.
export function pickThumbnailSize(
  mask: number,
  targetSize: number,
): 320 | 640 | 1280 | null {
  const has320 = (mask & 0b001) !== 0;
  const has640 = (mask & 0b010) !== 0;
  const has1280 = (mask & 0b100) !== 0;
  // Choose the smallest that's >= target, fallback to the largest <= target.
  if (targetSize <= 320 && has320) return 320;
  if (targetSize <= 640 && has640) return 640;
  if (targetSize <= 1280 && has1280) return 1280;
  if (has1280) return 1280;
  if (has640) return 640;
  if (has320) return 320;
  return null;
}

export function formatFileSize(bytes: number): string {
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 * 1024) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 * 1024 * 1024) return `${(bytes / 1024 / 1024).toFixed(1)} MB`;
  return `${(bytes / 1024 / 1024 / 1024).toFixed(1)} GB`;
}

export function formatDuration(ms: number): string {
  if (ms <= 0) return "";
  const total = Math.round(ms / 1000);
  const m = Math.floor(total / 60);
  const s = total % 60;
  return `${m}:${s.toString().padStart(2, "0")}`;
}
