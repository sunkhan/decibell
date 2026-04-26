import { invoke } from "@tauri-apps/api/core";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { toast } from "../../stores/toastStore";
import { kindFromMime, formatBytes } from "./attachmentHelpers";
import { extractVideoMetadata } from "./videoMetadata";
import { extractImageMetadata, type ThumbSize } from "./imageMetadata";

export const MAX_ATTACHMENTS_PER_MESSAGE = 10;

// Captured at queue time, shipped after the main upload completes.
// Keyed by pendingId. Each pending upload stores a map of long-edge
// size → JPEG blob; finalize() POSTs each size sequentially under the
// same upload-attachment-thumbnail IPC.
const pendingThumbnails = new Map<string, Map<ThumbSize, Blob>>();

export type UploadResult =
  | { ok: true; pendingId: string }
  | { ok: false; error: string };

interface StatResult {
  filename: string;
  sizeBytes: number;
  mime: string;
  width: number;
  height: number;
}

/**
 * Queue an attachment for the current compose. Stats the file, validates
 * the size against the server cap, and registers a `queued` pending entry.
 * The actual byte-pushing upload doesn't start here — `handleSend` kicks
 * off all queued items when the user hits send so we don't waste bandwidth
 * (or server space) on attachments the user might delete or never send.
 */
export async function uploadAttachment(opts: {
  filePath: string;
  serverId: string;
  channelId: string;
  maxBytes: number;
}): Promise<UploadResult> {
  // Per-message attachment cap. Enforced here so the limit applies to
  // every entry path (picker, drag-drop, paste) and to a series of
  // back-to-back uploads from the same batch — every call increments
  // the pending list by one, so the 11th call sees the previous 10
  // and bails. Reset happens automatically when the message sends and
  // `clearChannel` empties the pending list.
  const existing = useAttachmentsStore.getState().selectForChannel(opts.channelId);
  if (existing.length >= MAX_ATTACHMENTS_PER_MESSAGE) {
    const message = "Attachment limit reached";
    const detail = `You can attach up to ${MAX_ATTACHMENTS_PER_MESSAGE} files per message. Send this one first, then add more.`;
    toast.error(message, detail);
    return { ok: false, error: detail };
  }

  let meta: StatResult;
  try {
    meta = await invoke<StatResult>("stat_attachment_file", { path: opts.filePath });
  } catch (err) {
    const message = `Couldn't read ${opts.filePath}.`;
    toast.error(message, String(err));
    return { ok: false, error: message };
  }

  if (opts.maxBytes > 0 && meta.sizeBytes > opts.maxBytes) {
    const message = `${meta.filename} is too large`;
    const detail = `${formatBytes(meta.sizeBytes)} exceeds this server's ${formatBytes(opts.maxBytes)} attachment limit.`;
    toast.error(message, detail);
    return { ok: false, error: detail };
  }

  let width = meta.width;
  let height = meta.height;
  const kind = kindFromMime(meta.mime);
  let thumbnailUrl: string | undefined;

  // For image / video kinds, extract a small JPEG thumbnail and (for
  // video) intrinsic dimensions client-side. The Rust stat path can
  // read dimensions for some image formats but not videos, and not
  // every WebKit-supported image format (HEIC/AVIF/etc.). Using a
  // <video>/<img> here covers everything the renderer can display in
  // a single code path. Thumbnail is ~30 KB JPEG; uploaded after the
  // main file completes via the existing finalize() path.
  const pendingId = `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
  // For the local composer-tile preview we want the smallest size we
  // captured (lowest decode cost, the tile is only ~128 px). The rest
  // of the sizes get uploaded after the main file completes.
  const stashThumbnails = (thumbnails: Map<ThumbSize, Blob>) => {
    if (thumbnails.size === 0) return;
    pendingThumbnails.set(pendingId, thumbnails);
    const smallestSize = Math.min(...thumbnails.keys()) as ThumbSize;
    const smallestBlob = thumbnails.get(smallestSize);
    if (smallestBlob) thumbnailUrl = URL.createObjectURL(smallestBlob);
  };

  if (kind === "video") {
    try {
      const vm = await extractVideoMetadata(opts.filePath);
      if (vm.width > 0 && vm.height > 0) {
        width = vm.width;
        height = vm.height;
      }
      stashThumbnails(vm.thumbnails);
    } catch (err) {
      console.warn("[upload] video metadata extraction failed", err);
    }
  } else if (kind === "image") {
    try {
      const im = await extractImageMetadata(opts.filePath);
      // Only overwrite Rust-side dims when our extractor learned
      // something — Rust's image::image_dimensions handles common
      // formats (PNG/JPEG/WebP) and is what we want when both succeed
      // since it doesn't decode pixels.
      if (im.width > 0 && im.height > 0 && (width === 0 || height === 0)) {
        width = im.width;
        height = im.height;
      }
      stashThumbnails(im.thumbnails);
    } catch (err) {
      console.warn("[upload] image metadata extraction failed", err);
    }
  }

  const entry: PendingAttachment = {
    pendingId,
    channelId: opts.channelId,
    serverId: opts.serverId,
    filePath: opts.filePath,
    width,
    height,
    filename: meta.filename,
    mime: meta.mime,
    kind,
    totalBytes: meta.sizeBytes,
    transferredBytes: 0,
    status: "queued",
    thumbnailUrl,
  };
  useAttachmentsStore.getState().addPending(entry);
  return { ok: true, pendingId };
}

/** Kicks off the actual byte transfer for a queued attachment. Called
 *  from `handleSend` (one per queued item) and resolves when the
 *  attachment lands in a terminal state (`ready`, `failed`, or
 *  `cancelled`). The Rust side emits progress + complete events that
 *  `useChatEvents` translates to store updates, which is what unblocks
 *  the wait. */
export function startQueuedUpload(p: PendingAttachment): Promise<{ ok: boolean }> {
  useAttachmentsStore.getState().markUploading(p.pendingId);
  invoke("upload_attachment", {
    req: {
      pendingId: p.pendingId,
      serverId: p.serverId,
      channelId: p.channelId,
      filePath: p.filePath,
      filename: p.filename,
      mime: p.mime,
      width: p.width,
      height: p.height,
    },
  }).catch((err) => {
    useAttachmentsStore.getState().markFailed(p.pendingId, String(err), false);
  });

  return new Promise((resolve) => {
    const isTerminal = (s: PendingAttachment["status"] | undefined) =>
      s === "ready" || s === "failed" || s === "cancelled";
    const isOk = (s: PendingAttachment["status"] | undefined) => s === "ready";

    // Ship the captured thumbnail (if any) and only resolve once the
    // server has stored it. Awaiting this is what keeps the outgoing
    // ChannelMsg from racing the thumbnail POST — without the await,
    // receivers would see thumbnail_size_bytes=0 baked into their
    // message and never lazy-fetch even after the bytes land. A
    // thumbnail failure is non-fatal — message goes regardless, just
    // without a server-side poster.
    const finalize = async (ok: boolean) => {
      if (ok) {
        const thumbnails = pendingThumbnails.get(p.pendingId);
        const attachmentId = useAttachmentsStore
          .getState()
          .byPendingId[p.pendingId]?.attachmentId;
        if (thumbnails && thumbnails.size > 0 && attachmentId) {
          // Ship sizes smallest-first so a partial failure leaves the
          // most useful (smallest, fastest-to-fetch) sizes available.
          // Sequential POSTs share TLS handshake cost but stay well
          // under a second total — each thumb is ~10–50 KB.
          const sizes = [...thumbnails.keys()].sort((a, b) => a - b);
          for (const size of sizes) {
            const blob = thumbnails.get(size);
            if (!blob) continue;
            try {
              const buf = await blob.arrayBuffer();
              await invoke("upload_attachment_thumbnail", {
                serverId: p.serverId,
                attachmentId,
                size,
                bytes: Array.from(new Uint8Array(buf)),
              });
            } catch (err) {
              console.warn("[upload] thumbnail upload failed", { size, err });
              // Keep going — partial coverage is better than none.
            }
          }
        }
      }
      pendingThumbnails.delete(p.pendingId);
      resolve({ ok });
    };

    // Already terminal? (Race between markUploading and an immediate event.)
    const now = useAttachmentsStore.getState().byPendingId[p.pendingId]?.status;
    if (isTerminal(now)) {
      void finalize(isOk(now));
      return;
    }

    const unsub = useAttachmentsStore.subscribe((state) => {
      const status = state.byPendingId[p.pendingId]?.status;
      if (!status) {
        // Entry was removed (cancel × button) — treat as not-ok and
        // drop any captured thumbnail so we don't leak the Blob.
        unsub();
        pendingThumbnails.delete(p.pendingId);
        resolve({ ok: false });
        return;
      }
      if (isTerminal(status)) {
        unsub();
        void finalize(isOk(status));
      }
    });
  });
}
