import { ipcMain } from "electron";
import * as fs from "fs/promises";

// Bridge for renderer to read picked files from disk. Chromium's
// `fetch('file://...')` is blocked from the renderer (cross-origin
// vs the loaded vite dev URL / file:// scheme), and the File API
// the user picks via showOpenDialog only returns paths — not bytes.
// Node fs in main solves it without re-implementing the file picker.

export function registerFsHandlers(): void {
  ipcMain.handle("decibell:fs:readFile", async (_e, p: string): Promise<Uint8Array> => {
    const buf = await fs.readFile(p);
    // Convert Node Buffer to Uint8Array so the renderer doesn't see
    // a Buffer (which Chromium can't structured-clone with all its
    // Buffer-specific properties). New view, same memory.
    return new Uint8Array(buf.buffer, buf.byteOffset, buf.byteLength);
  });
  ipcMain.handle("decibell:fs:stat", async (_e, p: string) => {
    const s = await fs.stat(p);
    return { size: s.size, isFile: s.isFile(), isDirectory: s.isDirectory() };
  });
}
