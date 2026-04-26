import { invoke } from "@tauri-apps/api/core";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { toast } from "../../stores/toastStore";
import { kindFromMime, formatBytes } from "./attachmentHelpers";
import { extractVideoMetadata } from "./videoMetadata";

export const MAX_ATTACHMENTS_PER_MESSAGE = 10;

// Captured at queue time, shipped after the main upload completes. Keyed
// by pendingId so we don't have to thread Blobs through the zustand
// store (Blobs are not serialization-friendly + zustand devtools choke
// on them).
const pendingThumbnails = new Map<string, Blob>();

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

  // For video kind, the Rust stat path doesn't read dimensions (no
  // decoder dep), so we extract here via a hidden <video> element. The
  // same pass yields a JPEG thumbnail blob we ship after the main
  // upload completes. Fast (<1s for typical videos) since only the
  // header + first frame need to be decoded.
  const pendingId = `${Date.now()}-${Math.random().toString(36).slice(2, 10)}`;
  if (kind === "video") {
    try {
      const vm = await extractVideoMetadata(opts.filePath);
      if (vm.width > 0 && vm.height > 0) {
        width = vm.width;
        height = vm.height;
      }
      if (vm.thumbnail) {
        pendingThumbnails.set(pendingId, vm.thumbnail);
      }
    } catch (err) {
      console.warn("[upload] video metadata extraction failed", err);
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

    // Already terminal? (Race between markUploading and an immediate event.)
    const now = useAttachmentsStore.getState().byPendingId[p.pendingId]?.status;
    if (isTerminal(now)) {
      resolve({ ok: isOk(now) });
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
        const ok = isOk(status);
        if (ok) {
          // Fire-and-forget the thumbnail upload. Failure is non-fatal —
          // the placeholder just shows its plain look on the receiver
          // side. Send the message either way.
          const blob = pendingThumbnails.get(p.pendingId);
          const attachmentId = useAttachmentsStore
            .getState()
            .byPendingId[p.pendingId]?.attachmentId;
          if (blob && attachmentId) {
            blob
              .arrayBuffer()
              .then((buf) =>
                invoke("upload_attachment_thumbnail", {
                  serverId: p.serverId,
                  attachmentId,
                  bytes: Array.from(new Uint8Array(buf)),
                }),
              )
              .catch((err) => {
                console.warn("[upload] thumbnail upload failed", err);
              });
          }
        }
        pendingThumbnails.delete(p.pendingId);
        resolve({ ok });
      }
    });
  });
}
