// Attachment upload — chunked, resumable, cancellable, *streaming*.
// Driven from the renderer through Electron main's `net.fetch` (exposed
// via the preload bridge as `window.decibell.netFetch`). main-process
// net is what tauri-client's hand-rolled tokio-rustls path was
// approximating; it speaks Chromium's full TLS stack, honours
// `setCertificateVerifyProc` (so self-signed community certs work),
// and bypasses CORS — all things renderer-side `fetch` doesn't get
// reliably even with `webSecurity: false`.
//
// Wire protocol:
//   POST /attachments/init         → { id, uploadOffset }
//   PATCH /attachments/<id>        with Upload-Offset: <byte>
//   POST /attachments/<id>/complete → { id, kind, filename, mime, sizeBytes }
//   POST /attachments/<id>/thumbnail?size=<n>  → 204
//
// Memory shape: `source` is a ChunkSource that wraps either a
// `decibell-file://` URL (picked / dragged file — bytes never enter
// renderer RAM as a single buffer) or a `blob:` URL (paste / no-path
// drag — bytes already exist in Chromium's Blob storage). Per chunk
// we range-fetch ~8MB, send it, drop it. Peak renderer RAM during a
// 500MB upload is one chunk, not two full file copies.
//
// Thumbnails are generated client-side via OffscreenCanvas — no
// `image` crate needed, Chromium already has the pixel pipeline.

import { useAttachmentsStore } from "../../stores/attachmentsStore";
import { toast } from "../../stores/toastStore";
import type { AttachmentKind } from "../../types";
import type { ChunkSource } from "./chunkSource";

export const MAX_ATTACHMENTS_PER_MESSAGE = 10;

interface InitResponse {
  id: number;
  uploadOffset: number;
}

interface CompleteResponse {
  id: number;
  kind: number;
  filename: string;
  mime: string;
  sizeBytes: number;
  uploadStatus?: string;
}

const CHUNK_BYTES = 8 * 1024 * 1024;
const MAX_RETRY = 5;

const KIND_NAMES: Record<number, AttachmentKind> = {
  0: "image",
  1: "video",
  2: "document",
  3: "audio",
};

function classify(mime: string): AttachmentKind {
  if (mime.startsWith("image/")) return "image";
  if (mime.startsWith("video/")) return "video";
  if (mime.startsWith("audio/")) return "audio";
  return "document";
}

/// Convert an HTMLMediaElement.duration (seconds, possibly Infinity
/// or NaN) into the ms integer the wire protocol expects. Chromium
/// reports `Infinity` for the duration of CBR MP3s and other
/// container formats it can't size without scanning the whole file —
/// JSON.stringify renders that as `null` which trips the server-side
/// json::value<int32> conversion and trips a 400. Treat unknown
/// duration as 0 (server already accepts this for files where the
/// uploader couldn't probe).
function finiteDurationMs(seconds: number): number {
  if (!Number.isFinite(seconds) || seconds <= 0) return 0;
  return Math.round(seconds * 1000);
}

/// Coax a real duration value out of an HTMLMediaElement that came
/// back as `Infinity` from `loadedmetadata`. Chromium needs to scan
/// the file to compute the real length for formats that don't carry
/// a precise length tag (CBR MP3 is the common offender). Setting
/// `currentTime = MAX_SAFE_INTEGER` forces that scan — the browser
/// seeks past the end, fires `durationchange` with the real value,
/// then we restore `currentTime` to 0 so playback still starts from
/// the beginning. A 5s safety timeout caps how long we wait.
async function resolveDuration(media: HTMLMediaElement): Promise<number> {
  if (Number.isFinite(media.duration) && media.duration > 0) {
    return media.duration;
  }
  return await new Promise<number>((resolve) => {
    let done = false;
    const finish = (value: number) => {
      if (done) return;
      done = true;
      media.removeEventListener("durationchange", onDur);
      media.removeEventListener("error", onErr);
      window.clearTimeout(timer);
      // Snap currentTime back to 0 so subsequent playback (or seek
      // requests against the bubble's player) starts at the beginning,
      // not the past-end position we used to force the scan.
      try {
        media.currentTime = 0;
      } catch {
        // Some elements throw if currentTime is set before metadata
        // is ready; we already have what we came for.
      }
      resolve(value);
    };
    const onDur = () => {
      if (Number.isFinite(media.duration) && media.duration > 0) {
        finish(media.duration);
      }
    };
    const onErr = () => finish(media.duration);
    media.addEventListener("durationchange", onDur);
    media.addEventListener("error", onErr);
    const timer = window.setTimeout(() => finish(media.duration), 5000);
    try {
      media.currentTime = Number.MAX_SAFE_INTEGER;
    } catch {
      finish(media.duration);
    }
  });
}

interface NetFetchInit {
  method?: string;
  headers?: Record<string, string>;
  body?: string | Uint8Array;
  attachmentTarget?: { serverId: string; path: string };
}

interface NetFetchResult {
  ok: boolean;
  status: number;
  statusText: string;
  headers: Record<string, string>;
  body: ArrayBuffer;
}

async function attachmentFetch(
  serverId: string,
  path: string,
  init: Omit<NetFetchInit, "attachmentTarget">,
): Promise<NetFetchResult> {
  return await window.decibell.netFetch("", {
    ...init,
    attachmentTarget: { serverId, path },
  });
}

function decodeJson<T>(result: NetFetchResult): T {
  const text = new TextDecoder("utf-8").decode(result.body);
  return JSON.parse(text) as T;
}

/// Race a promise against a timeout. If the timer wins, reject so
/// the caller can fall back gracefully — used by probeMetadata to
/// keep cross-app drag-drops (where Chromium may hang materialising
/// bytes from another renderer process) from freezing the renderer.
function withTimeout<T>(p: Promise<T>, ms: number, message: string): Promise<T> {
  return new Promise<T>((resolve, reject) => {
    const id = window.setTimeout(() => reject(new Error(message)), ms);
    p.then(
      (v) => {
        window.clearTimeout(id);
        resolve(v);
      },
      (e) => {
        window.clearTimeout(id);
        reject(e);
      },
    );
  });
}

/// Compute the natural width/height + duration for the picked file so
/// the bubble can reserve the right aspect-ratio slot during upload.
/// For videos we also try to capture a poster frame here while the
/// element is already decoded — saves seeking the file twice when
/// generating server thumbnails.
///
/// Probes go through `source.url` (Chromium streams whatever bytes
/// the probe element actually needs — typically just the header for
/// image dims / duration, plus a small chunk for the seek-to-0.5s
/// frame capture). For huge videos this means we never load more
/// than a few MB to discover the dimensions and grab a poster.
async function probeMetadata(
  source: ChunkSource,
  kind: AttachmentKind,
): Promise<{
  width: number;
  height: number;
  durationMs: number;
  /// Preview URL the bubble shows before upload completes. Reuses
  /// `source.url` directly so we don't allocate a second URL handle.
  previewUrl: string | null;
  frameBlob?: Blob;
}> {
  if (kind === "image") {
    try {
      // 5s timeout — for cross-app drags (e.g. images dragged from
      // Discord) the source bytes may live in a different renderer
      // process and Chromium can occasionally hang materialising
      // them. Without the timeout, an awaited image-load that never
      // resolves froze the renderer indefinitely. On timeout we
      // skip metadata and let the upload proceed with width/height=0.
      const img = await withTimeout(
        new Promise<HTMLImageElement>((resolve, reject) => {
          const el = new Image();
          el.onload = () => resolve(el);
          el.onerror = () => reject(new Error("image probe failed"));
          el.src = source.url;
        }),
        5000,
        "image probe timed out",
      );
      return {
        width: img.naturalWidth,
        height: img.naturalHeight,
        durationMs: 0,
        previewUrl: source.url,
      };
    } catch {
      return { width: 0, height: 0, durationMs: 0, previewUrl: null };
    }
  }
  if (kind === "video") {
    try {
      const video = document.createElement("video");
      // We need pixels (not just metadata) to capture a poster frame.
      // The browser will issue Range requests against source.url and
      // only pull what it needs.
      video.preload = "auto";
      video.muted = true;
      video.src = source.url;
      await withTimeout(
        new Promise<void>((resolve, reject) => {
          video.onloadedmetadata = () => resolve();
          video.onerror = () => reject(new Error("video probe failed"));
        }),
        5000,
        "video probe timed out",
      );

      // Seek a bit into the file (10% in, capped at 0.5s) to skip
      // black opening frames before capturing the poster. Best-effort
      // — if the seek fails we still return the size/duration.
      let frameBlob: Blob | undefined;
      try {
        const target = Math.min(0.5, (video.duration || 1) * 0.1);
        video.currentTime = target;
        await new Promise<void>((resolve, reject) => {
          video.onseeked = () => resolve();
          video.onerror = () => reject(new Error("video seek failed"));
        });
        if (video.videoWidth && video.videoHeight) {
          const canvas = new OffscreenCanvas(video.videoWidth, video.videoHeight);
          const ctx = canvas.getContext("2d");
          if (ctx) {
            ctx.drawImage(video, 0, 0);
            frameBlob = await canvas.convertToBlob({
              type: "image/jpeg",
              quality: 0.85,
            });
          }
        }
      } catch {
        // Frame capture is non-fatal.
      }

      // Resolve duration AFTER the thumbnail seek — the duration
      // trick seeks to MAX_SAFE_INTEGER which would invalidate any
      // pending frame capture. Most video containers report a finite
      // duration straight away; the resolveDuration helper short-
      // circuits in that case.
      const durationSec = await resolveDuration(video);
      return {
        width: video.videoWidth || 0,
        height: video.videoHeight || 0,
        durationMs: finiteDurationMs(durationSec),
        previewUrl: source.url,
        frameBlob,
      };
    } catch {
      return { width: 0, height: 0, durationMs: 0, previewUrl: null };
    }
  }
  if (kind === "audio") {
    try {
      const audio = document.createElement("audio");
      audio.preload = "metadata";
      audio.src = source.url;
      await withTimeout(
        new Promise<void>((resolve, reject) => {
          audio.onloadedmetadata = () => resolve();
          audio.onerror = () => reject(new Error("audio probe failed"));
        }),
        5000,
        "audio probe timed out",
      );
      const durationSec = await resolveDuration(audio);
      return {
        width: 0,
        height: 0,
        durationMs: finiteDurationMs(durationSec),
        previewUrl: source.url,
      };
    } catch {
      return { width: 0, height: 0, durationMs: 0, previewUrl: null };
    }
  }
  return { width: 0, height: 0, durationMs: 0, previewUrl: null };
}

/// Generate a JPEG thumbnail at `size` long-edge using OffscreenCanvas.
/// Accepts any Blob (image bytes fetched from the source URL, or the
/// captured video poster frame from probeMetadata).
async function generateThumbnail(blob: Blob, size: number): Promise<Uint8Array | null> {
  try {
    const bitmap = await createImageBitmap(blob);
    const ratio = bitmap.width / bitmap.height;
    let w: number, h: number;
    if (bitmap.width >= bitmap.height) {
      w = Math.min(size, bitmap.width);
      h = Math.round(w / ratio);
    } else {
      h = Math.min(size, bitmap.height);
      w = Math.round(h * ratio);
    }
    const canvas = new OffscreenCanvas(w, h);
    const ctx = canvas.getContext("2d");
    if (!ctx) {
      bitmap.close();
      return null;
    }
    ctx.drawImage(bitmap, 0, 0, w, h);
    bitmap.close();
    const blobOut = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.82 });
    return new Uint8Array(await blobOut.arrayBuffer());
  } catch {
    return null;
  }
}

interface QueueArgs {
  pendingId: string;
  serverId: string;
  channelId: string;
  source: ChunkSource;
}

/// Phase 1: probe metadata + register the attachment as `queued` in
/// the store. NO bytes leave the renderer here — the file just sits in
/// the composer's PendingAttachmentsRow until the user clicks send.
/// This matches the tauri-client behaviour: we don't waste bandwidth
/// (or the server's storage) on attachments the user might delete or
/// never send. Phase 2 (`startQueuedUpload`) is invoked from
/// ChatPanel.handleSend.
export async function queueUpload(args: QueueArgs): Promise<void> {
  const { pendingId, serverId, channelId, source } = args;
  const store = useAttachmentsStore.getState();

  // Per-message attachment cap. Enforced HERE so the limit applies to
  // every entry path (file picker, drag-drop, paste) and to a series
  // of back-to-back uploads from the same batch — every call sees the
  // current pending list, so the 11th call bails. Reset happens
  // automatically when the message sends and the channel's pendings
  // get cleared (handleSend in ChatPanel calls removePending).
  const existing = store.selectForChannel(serverId, channelId);
  if (existing.length >= MAX_ATTACHMENTS_PER_MESSAGE) {
    toast.error(
      "Attachment limit reached",
      `You can attach up to ${MAX_ATTACHMENTS_PER_MESSAGE} files per message. Send this one first, then add more.`,
    );
    // Release the source so the whitelist entry / Blob URL doesn't
    // leak when we bail without entering the upload flow.
    source.cleanup();
    throw new Error("attachment limit reached");
  }

  const kind = classify(source.mime);
  const meta = await probeMetadata(source, kind);

  store.add({
    pendingId,
    serverId,
    channelId,
    filename: source.name,
    mime: source.mime || "application/octet-stream",
    kind,
    totalBytes: source.size,
    abortController: new AbortController(),
    previewUrl: meta.previewUrl,
    width: meta.width,
    height: meta.height,
    durationMs: meta.durationMs,
    source,
    // probeMetadata.frameBlob (video poster) is captured into a
    // module-scoped cache below — it's needed at upload-finish time
    // for thumbnail uploads, but plumbing it through the pending
    // entry would mean a cross-store dependency on Blob refs that
    // might race with cleanup. Module map keyed by pendingId is
    // simpler.
  });

  if (meta.frameBlob) {
    pendingFrameBlobs.set(pendingId, meta.frameBlob);
  }
}

/// Per-pending video poster captured during probeMetadata, looked up
/// by startQueuedUpload when the upload completes so we can ship
/// server thumbnails. Cleared in startQueuedUpload's finally so a
/// pending that gets discarded without being sent doesn't leak.
const pendingFrameBlobs = new Map<string, Blob>();

/// Phase 2: actually send the bytes for a previously-queued
/// attachment. Returns the server-assigned attachment id on success.
/// Called from ChatPanel.handleSend once per queued item; the wait
/// loop there reads pending.status to know when each upload reaches a
/// terminal state.
export async function startQueuedUpload(pendingId: string): Promise<number> {
  const store = useAttachmentsStore.getState();
  const pending = store.pendings[pendingId];
  if (!pending) throw new Error(`unknown pendingId: ${pendingId}`);
  const { serverId, channelId, source, abortController, width, height, durationMs } = pending;
  const kind = pending.kind;
  // Mark as uploading so the chat-side BubbleInflightAttachments
  // switches from the queued chip to the live progress bar.
  store.setStatus(pendingId, "uploading");

  try {
    // POST /init
    const initResult = await attachmentFetch(serverId, "/attachments/init", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        channelId,
        filename: source.name,
        mime: source.mime || "application/octet-stream",
        size: source.size,
        width,
        height,
        durationMs,
      }),
    });
    if (!initResult.ok) {
      throw new Error(
        `init failed: HTTP ${initResult.status} ${initResult.statusText}`,
      );
    }
    const initBody = decodeJson<InitResponse>(initResult);
    let offset = initBody.uploadOffset ?? 0;

    // PATCH chunks until we hit source.size, with retry/backoff per
    // chunk. Per-chunk Range fetch against source.url means peak
    // renderer RAM is one chunk (~8MB) regardless of file size — no
    // pre-loaded ArrayBuffer of the whole file.
    while (offset < source.size) {
      if (abortController.signal.aborted) {
        throw new Error("Upload cancelled");
      }
      const end = Math.min(offset + CHUNK_BYTES, source.size);
      const chunk = await source.readChunk(offset, end, abortController.signal);
      let attempt = 0;
      let lastErr: Error | null = null;

      while (attempt <= MAX_RETRY) {
        if (abortController.signal.aborted) {
          throw new Error("Upload cancelled");
        }
        try {
          const resp = await attachmentFetch(
            serverId,
            `/attachments/${initBody.id}`,
            {
              method: "PATCH",
              headers: {
                // Server speaks tus.io resumable-upload semantics: it
                // looks at `Upload-Offset` to know where this chunk
                // starts, not the HTTP Content-Range header. Sending
                // Content-Range without Upload-Offset → 400.
                "Upload-Offset": String(offset),
                "Content-Type": "application/octet-stream",
              },
              body: chunk,
            },
          );
          if (!resp.ok) {
            throw new Error(`PATCH ${offset}: HTTP ${resp.status} ${resp.statusText}`);
          }
          offset = end;
          store.updateProgress(pendingId, offset);
          break;
        } catch (e) {
          lastErr = e as Error;
          attempt += 1;
          if (attempt > MAX_RETRY) break;
          const wait = Math.min(8000, 250 * Math.pow(2, attempt));
          await new Promise((r) => setTimeout(r, wait));
        }
      }

      if (attempt > MAX_RETRY && lastErr) {
        throw lastErr;
      }
    }

    // POST /complete
    const completeResult = await attachmentFetch(
      serverId,
      `/attachments/${initBody.id}/complete`,
      { method: "POST" },
    );
    if (!completeResult.ok) {
      throw new Error(
        `complete failed: HTTP ${completeResult.status} ${completeResult.statusText}`,
      );
    }
    const completeBody = decodeJson<CompleteResponse>(completeResult);
    const finalKind = KIND_NAMES[completeBody.kind] ?? kind;
    store.markReady(
      pendingId,
      completeBody.id,
      finalKind,
      completeBody.mime,
      completeBody.filename,
    );

    // Thumbnail uploads — best-effort, non-fatal if any fail.
    // Videos use the poster frame captured during probeMetadata as
    // the source (already in memory as a small JPEG, looked up from
    // the module map by pendingId). Images need their bytes; we
    // fetch the source URL once and feed the Blob to generateThumbnail
    // at all three sizes.
    let thumbSource: Blob | null = null;
    if (finalKind === "image") {
      try {
        const r = await fetch(source.url);
        thumbSource = await r.blob();
      } catch {
        thumbSource = null;
      }
    } else if (finalKind === "video") {
      thumbSource = pendingFrameBlobs.get(pendingId) ?? null;
    }
    if (thumbSource) {
      for (const size of [320, 640, 1280]) {
        const thumb = await generateThumbnail(thumbSource, size);
        if (!thumb) continue;
        try {
          await attachmentFetch(
            serverId,
            `/attachments/${completeBody.id}/thumbnail?size=${size}`,
            {
              method: "POST",
              headers: { "Content-Type": "image/jpeg" },
              body: thumb,
            },
          );
        } catch {
          // Non-fatal
        }
      }
    }

    return completeBody.id;
  } catch (e) {
    const err = e as Error;
    const cancelled = abortController.signal.aborted;
    useAttachmentsStore
      .getState()
      .markFailed(pendingId, cancelled ? "Cancelled" : err.message, cancelled);
    throw err;
  } finally {
    // Always release the source (drops the decibell-file:// whitelist
    // entry or revokes the blob: URL). Idempotent — safe even if the
    // upload errored before any chunk went out. Same for the cached
    // poster frame so a stale pending doesn't pin a JPEG forever.
    source.cleanup();
    pendingFrameBlobs.delete(pendingId);
  }
}
