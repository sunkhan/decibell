import type { ViewerImage } from "../../../stores/imageViewerStore";
import { toast } from "../../../stores/toastStore";
import { openExternal } from "../../../lib/openExternal";
import { startDownload } from "../../transfers/downloads";

// What the viewer's toolbar and the image right-click menu do with a
// picture. Attachments go through main (netFetch with an attachment
// target: auth + decryption); link images are plain https fetches, also
// through main, since a renderer fetch of another origin is refused.

async function fetchBytes(image: Pick<ViewerImage, "url" | "serverId" | "attachmentId">) {
  const res =
    image.serverId != null && image.attachmentId != null
      ? await window.decibell.netFetch("", {
          method: "GET",
          attachmentTarget: { serverId: image.serverId, path: `/attachments/${image.attachmentId}` },
        })
      : await window.decibell.netFetch(image.url, { method: "GET" });
  if (!res.ok) throw new Error(`HTTP ${res.status}`);
  return { bytes: res.body, mime: res.headers["content-type"] ?? "application/octet-stream" };
}

/// Copy the picture itself. Chromium's clipboard takes PNG everywhere;
/// anything else goes through a canvas first so the paste lands in any app.
export async function copyImage(image: Pick<ViewerImage, "url" | "serverId" | "attachmentId" | "filename">) {
  try {
    const { bytes, mime } = await fetchBytes(image);
    let blob = new Blob([bytes], { type: mime });
    if (blob.type !== "image/png") {
      const bitmap = await createImageBitmap(blob);
      const canvas = new OffscreenCanvas(bitmap.width, bitmap.height);
      const ctx = canvas.getContext("2d");
      if (!ctx) throw new Error("Canvas 2D unavailable");
      ctx.drawImage(bitmap, 0, 0);
      blob = await canvas.convertToBlob({ type: "image/png" });
      bitmap.close();
    }
    await navigator.clipboard.write([new ClipboardItem({ "image/png": blob })]);
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
    await navigator.clipboard.writeText(image.pageUrl ?? image.url);
    toast.success("Link copied");
  } catch (err) {
    toast.error("Copy failed", String(err));
  }
}
