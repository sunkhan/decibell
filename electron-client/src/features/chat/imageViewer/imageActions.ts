import type { ViewerImage } from "../../../stores/imageViewerStore";
import { toast, useToastStore } from "../../../stores/toastStore";
import { openExternal } from "../../../lib/openExternal";
import { startDownload } from "../../transfers/downloads";

// What the viewer's toolbar and the image right-click menu do with a
// picture. Main copies an attachment by itself (electron/main/clipboard.ts:
// fetched there, decrypted, at the download cap, never crossing to the
// renderer). Link images are fetched here, the same request their <img>
// made (main's netFetch takes registered community servers only).

/// Attachments aren't cached, so a copy downloads the original again: past
/// this, a toast says it's under way.
const SLOW_COPY_MS = 300;

/// PNG and JPEG go to the clipboard as they are, by their bytes.
function isPngOrJpeg(b: Uint8Array): boolean {
  const png = b.length > 4 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47;
  const jpeg = b.length > 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff;
  return png || jpeg;
}

/// WebP, AVIF, a GIF's first frame…: a PNG, through a canvas.
async function toPng(bytes: Uint8Array): Promise<Uint8Array> {
  const bitmap = await createImageBitmap(new Blob([bytes]));
  const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas 2D unavailable");
  ctx.drawImage(bitmap, 0, 0);
  bitmap.close();
  return new Uint8Array(await (await canvas.convertToBlob({ type: "image/png" })).arrayBuffer());
}

async function copyLinkImage(url: string): Promise<void> {
  const res = await fetch(url, { credentials: "omit", referrerPolicy: "no-referrer" });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  const bytes = new Uint8Array(await res.arrayBuffer());
  await window.decibell.clipboard.writeImage(isPngOrJpeg(bytes) ? bytes : await toPng(bytes));
}

/// An error's message without Electron's "Error invoking remote method …"
/// wrapper, which says nothing to a person.
function reason(err: unknown): string {
  const text = err instanceof Error ? err.message : String(err);
  return text.replace(/^Error invoking remote method '[^']*': (Error: )?/, "");
}

/// Copy the picture itself.
export async function copyImage(image: Pick<ViewerImage, "url" | "serverId" | "attachmentId" | "filename">) {
  let copying: string | null = null;
  const slow = window.setTimeout(() => {
    copying = toast.info("Copying image…", image.filename, { duration: 120_000 });
  }, SLOW_COPY_MS);
  let error: unknown = null;
  try {
    if (image.serverId != null && image.attachmentId != null) {
      const res = await window.decibell.clipboard.copyAttachment(image.serverId, image.attachmentId);
      if ("convert" in res) await window.decibell.clipboard.writeImage(await toPng(res.convert));
    } else {
      await copyLinkImage(image.url);
    }
  } catch (err) {
    error = err;
  }
  window.clearTimeout(slow);
  if (copying) useToastStore.getState().dismiss(copying);
  if (error) toast.error("Copy failed", reason(error));
  else toast.success("Image copied", image.filename);
}

export function canSave(image: ViewerImage): boolean {
  return image.serverId != null && image.attachmentId != null;
}

/// Attachments only: the download manager (progress in the Transfers panel).
export function saveImage(image: ViewerImage, saveAs = false, kind: "image" | "video" = "image") {
  if (image.serverId == null || image.attachmentId == null) return;
  void startDownload(image.serverId, image.attachmentId, {
    saveAs,
    fallback: { filename: image.filename, mime: image.mime, kind, sizeBytes: image.sizeBytes },
  });
}

/// Link images: the page the link points at, else the image itself.
export function openInBrowser(image: ViewerImage) {
  openExternal(image.pageUrl ?? image.url);
}

export async function copyLink(image: ViewerImage) {
  try {
    await window.decibell.clipboard.writeText(image.pageUrl ?? image.url);
    toast.success("Link copied");
  } catch (err) {
    toast.error("Copy failed", reason(err));
  }
}
