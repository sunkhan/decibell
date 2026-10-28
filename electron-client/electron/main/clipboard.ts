import { clipboard, ipcMain, nativeImage } from "electron";
import { fetchAttachmentBytes } from "./netFetch";

// Copying images and links (the image viewer's toolbar, the image
// right-click menu). Main writes them with Electron's clipboard instead of
// the renderer using the async Clipboard API, which needs `ClipboardItem`
// (a secure-context global the owner's dev renderer reported missing) and a
// focused document.
//
// An attachment is copied in one trip: main fetches it (decrypted, at the
// download cap), decodes it and writes it, so its bytes never cross to the
// renderer. nativeImage reads PNG and JPEG only; anything else (WebP, AVIF,
// a GIF) goes back to the renderer once to become a PNG on a canvas, then
// returns through `writeImage`. Link images come the same way from the
// renderer, which fetched them.

const IMAGE_MAX = 128 * 1024 * 1024;
const TEXT_MAX = 64 * 1024;

function isPngOrJpeg(b: Uint8Array): boolean {
  const png = b.length > 4 && b[0] === 0x89 && b[1] === 0x50 && b[2] === 0x4e && b[3] === 0x47;
  const jpeg = b.length > 3 && b[0] === 0xff && b[1] === 0xd8 && b[2] === 0xff;
  return png || jpeg;
}

function writeImageBytes(bytes: Uint8Array): void {
  if (bytes.byteLength === 0) throw new Error("Not an image");
  if (bytes.byteLength > IMAGE_MAX) throw new Error("Too large to copy; save it instead");
  const image = nativeImage.createFromBuffer(Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength));
  if (image.isEmpty()) throw new Error("Not a PNG or JPEG image");
  clipboard.writeImage(image);
}

export function registerClipboardHandlers(): void {
  ipcMain.handle(
    "decibell:clipboard:copyAttachment",
    async (_e, serverId: unknown, attachmentId: unknown): Promise<{ copied: true } | { convert: Uint8Array }> => {
      if (typeof serverId !== "string" || typeof attachmentId !== "number" || !Number.isSafeInteger(attachmentId) || attachmentId <= 0) {
        throw new Error("Not an attachment");
      }
      const { status, body } = await fetchAttachmentBytes(serverId, String(attachmentId));
      if (status >= 400) throw new Error(`HTTP ${status}`);
      if (body.byteLength > IMAGE_MAX) throw new Error("Too large to copy; save it instead");
      if (!isPngOrJpeg(body)) return { convert: body };
      writeImageBytes(body);
      return { copied: true };
    },
  );

  ipcMain.handle("decibell:clipboard:writeImage", (_e, bytes: unknown) => {
    if (!(bytes instanceof Uint8Array)) throw new Error("Not an image");
    writeImageBytes(bytes);
  });

  ipcMain.handle("decibell:clipboard:writeText", (_e, text: unknown) => {
    if (typeof text !== "string" || text.length > TEXT_MAX) throw new Error("Not text");
    clipboard.writeText(text);
  });
}
