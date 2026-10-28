import type { ViewerImage } from "../../../stores/imageViewerStore";
import { toast } from "../../../stores/toastStore";
import { openExternal } from "../../../lib/openExternal";
import { startDownload } from "../../transfers/downloads";

// What the viewer's toolbar and the image right-click menu do with a
// picture. Attachment bytes come through main (netFetch with an attachment
// target: auth + decryption). Link images are fetched by the renderer, the
// same request their <img> already made: main's netFetch takes registered
// community servers only. Main writes the clipboard (electron/main/clipboard.ts).

async function fetchBytes(image: Pick<ViewerImage, "url" | "serverId" | "attachmentId">): Promise<ArrayBuffer> {
  if (image.serverId != null && image.attachmentId != null) {
    const res = await window.decibell.netFetch("", {
      method: "GET",
      attachmentTarget: { serverId: image.serverId, path: `/attachments/${image.attachmentId}` },
    });
    if (!res.ok) throw new Error(`HTTP ${res.status}`);
    return res.body;
  }
  const res = await fetch(image.url, { credentials: "omit", referrerPolicy: "no-referrer" });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return res.arrayBuffer();
}

/// PNG and JPEG go to the clipboard as they are (by their bytes: an
/// encrypted attachment's served type says nothing).
function isPngOrJpeg(b: Uint8Array): boolean {
  const png = b.length > 4 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47;
  const jpeg = b.length > 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff;
  return png || jpeg;
}

/// Copy the picture itself. Anything but PNG and JPEG (WebP, AVIF, a GIF's
/// first frame) goes through a canvas to PNG, so the paste lands in any app.
export async function copyImage(image: Pick<ViewerImage, "url" | "serverId" | "attachmentId" | "filename">) {
  try {
    let bytes = new Uint8Array(await fetchBytes(image));
    if (!isPngOrJpeg(bytes)) {
      const bitmap = await createImageBitmap(new Blob([bytes]));
      const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("Canvas 2D unavailable");
      ctx.drawImage(bitmap, 0, 0);
      bitmap.close();
      bytes = new Uint8Array(await (await canvas.convertToBlob({ type: "image/png" })).arrayBuffer());
    }
    await window.decibell.clipboard.writeImage(bytes);
    toast.success("Image copied", image.filename);
  } catch (err) {
    toast.error("Copy failed", String(err));
  }
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
    toast.error("Copy failed", String(err));
  }
}
