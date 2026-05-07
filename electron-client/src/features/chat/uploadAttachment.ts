// Attachment upload — chunked, resumable, cancellable. Driven from
// the renderer through Electron main's `net.fetch` (exposed via the
// preload bridge as `window.decibell.netFetch`). main-process net is
// what tauri-client's hand-rolled tokio-rustls path was approximating;
// it speaks Chromium's full TLS stack, honours `setCertificateVerifyProc`
// (so self-signed community certs work), and bypasses CORS — all things
// renderer-side `fetch` doesn't reliably get even with `webSecurity: false`.
//
// Wire protocol:
//   POST /attachments/init         → { id, uploadOffset }
//   PATCH /attachments/<id>        with Content-Range: bytes <a>-<b>/<total>
//   POST /attachments/<id>/complete → { id, kind, filename, mime, sizeBytes }
//   POST /attachments/<id>/thumbnail?size=<n>  → 204
//
// Thumbnails are generated client-side via OffscreenCanvas — no
// `image` crate needed, Chromium already has the pixel pipeline.

import { useAttachmentsStore } from "../../stores/attachmentsStore";
import type { AttachmentKind } from "../../types";

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

/// Compute the natural width/height + duration for the picked file so
/// the bubble can reserve the right aspect-ratio slot during upload.
async function probeMetadata(
  file: File,
  kind: AttachmentKind,
): Promise<{
  width: number;
  height: number;
  durationMs: number;
  previewUrl: string | null;
}> {
  if (kind === "image") {
    const url = URL.createObjectURL(file);
    try {
      const img = await new Promise<HTMLImageElement>((resolve, reject) => {
        const el = new Image();
        el.onload = () => resolve(el);
        el.onerror = () => reject(new Error("image probe failed"));
        el.src = url;
      });
      return {
        width: img.naturalWidth,
        height: img.naturalHeight,
        durationMs: 0,
        previewUrl: url,
      };
    } catch {
      URL.revokeObjectURL(url);
      return { width: 0, height: 0, durationMs: 0, previewUrl: null };
    }
  }
  if (kind === "video") {
    const url = URL.createObjectURL(file);
    try {
      const video = document.createElement("video");
      video.src = url;
      video.preload = "metadata";
      await new Promise<void>((resolve, reject) => {
        video.onloadedmetadata = () => resolve();
        video.onerror = () => reject(new Error("video probe failed"));
      });
      return {
        width: video.videoWidth || 0,
        height: video.videoHeight || 0,
        durationMs: Math.round((video.duration || 0) * 1000),
        previewUrl: url,
      };
    } catch {
      URL.revokeObjectURL(url);
      return { width: 0, height: 0, durationMs: 0, previewUrl: null };
    }
  }
  if (kind === "audio") {
    const url = URL.createObjectURL(file);
    try {
      const audio = document.createElement("audio");
      audio.src = url;
      audio.preload = "metadata";
      await new Promise<void>((resolve, reject) => {
        audio.onloadedmetadata = () => resolve();
        audio.onerror = () => reject(new Error("audio probe failed"));
      });
      return {
        width: 0,
        height: 0,
        durationMs: Math.round((audio.duration || 0) * 1000),
        previewUrl: url,
      };
    } catch {
      URL.revokeObjectURL(url);
      return { width: 0, height: 0, durationMs: 0, previewUrl: null };
    }
  }
  return { width: 0, height: 0, durationMs: 0, previewUrl: null };
}

/// Generate a JPEG thumbnail at `size` long-edge using OffscreenCanvas.
async function generateThumbnail(file: File, size: number): Promise<Uint8Array | null> {
  try {
    const bitmap = await createImageBitmap(file);
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
    const blob = await canvas.convertToBlob({ type: "image/jpeg", quality: 0.82 });
    return new Uint8Array(await blob.arrayBuffer());
  } catch {
    return null;
  }
}

interface EnqueueArgs {
  pendingId: string;
  serverId: string;
  channelId: string;
  file: File;
}

export async function enqueueUpload(args: EnqueueArgs): Promise<number> {
  const { pendingId, serverId, channelId, file } = args;
  const store = useAttachmentsStore.getState();
  const kind = classify(file.type);
  const meta = await probeMetadata(file, kind);
  const abortController = new AbortController();

  store.add({
    pendingId,
    serverId,
    channelId,
    filename: file.name,
    mime: file.type || "application/octet-stream",
    kind,
    totalBytes: file.size,
    abortController,
    previewUrl: meta.previewUrl,
    width: meta.width,
    height: meta.height,
    durationMs: meta.durationMs,
  });

  try {
    // POST /init
    const initResult = await attachmentFetch(serverId, "/attachments/init", {
      method: "POST",
      headers: { "Content-Type": "application/json" },
      body: JSON.stringify({
        channelId,
        filename: file.name,
        mime: file.type || "application/octet-stream",
        size: file.size,
        width: meta.width,
        height: meta.height,
        durationMs: meta.durationMs,
      }),
    });
    if (!initResult.ok) {
      throw new Error(
        `init failed: HTTP ${initResult.status} ${initResult.statusText}`,
      );
    }
    const initBody = decodeJson<InitResponse>(initResult);
    let offset = initBody.uploadOffset ?? 0;

    // PATCH chunks until we hit file.size, with retry/backoff per chunk.
    // Hold a single ArrayBuffer copy and slice into fresh Uint8Arrays per
    // chunk — `subarray` returns a view over the *full* underlying buffer,
    // and the structured-clone path used by IPC plus net.fetch's body
    // handling can confuse the byteOffset/byteLength contract enough to
    // send the wrong content length, which the server rejects with 400.
    const fileBuffer = await file.arrayBuffer();
    while (offset < file.size) {
      if (abortController.signal.aborted) {
        throw new Error("Upload cancelled");
      }
      const end = Math.min(offset + CHUNK_BYTES, file.size);
      // .slice() copies into a brand-new ArrayBuffer of exactly the chunk
      // length — no shared underlying buffer means no surprises after IPC.
      const chunk = new Uint8Array(fileBuffer.slice(offset, end));
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
    if (finalKind === "image") {
      for (const size of [320, 640, 1280]) {
        const thumb = await generateThumbnail(file, size);
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
    store.markFailed(pendingId, cancelled ? "Cancelled" : err.message, cancelled);
    throw err;
  }
}
