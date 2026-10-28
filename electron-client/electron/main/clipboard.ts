import { clipboard, ipcMain, nativeImage } from "electron";

// Copying images and links (the image viewer's toolbar, the image
// right-click menu). Main writes them with Electron's clipboard instead of
// the renderer using the async Clipboard API, which needs `ClipboardItem`
// (a secure-context global the owner's dev renderer reported missing) and a
// focused document. The renderer sends PNG or JPEG bytes, the two formats
// nativeImage reads; it converts anything else first.

const IMAGE_MAX = 128 * 1024 * 1024;
const TEXT_MAX = 64 * 1024;

export function registerClipboardHandlers(): void {
  ipcMain.handle("decibell:clipboard:writeImage", (_e, bytes: unknown) => {
    if (!(bytes instanceof Uint8Array) || bytes.byteLength === 0 || bytes.byteLength > IMAGE_MAX) {
      throw new Error("Not an image");
    }
    const image = nativeImage.createFromBuffer(Buffer.from(bytes.buffer, bytes.byteOffset, bytes.byteLength));
    if (image.isEmpty()) throw new Error("Not a PNG or JPEG image");
    clipboard.writeImage(image);
  });

  ipcMain.handle("decibell:clipboard:writeText", (_e, text: unknown) => {
    if (typeof text !== "string" || text.length > TEXT_MAX) throw new Error("Not text");
    clipboard.writeText(text);
  });
}
