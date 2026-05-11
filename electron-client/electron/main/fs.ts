import { ipcMain } from "electron";
import * as fs from "fs/promises";
import * as path from "node:path";
import { registerFile, unregisterFile } from "./fileRegistry";

// Bridge for renderer to read/write picked files. Chromium's
// `fetch('file://...')` is blocked from the renderer (cross-origin
// vs the loaded vite dev URL / file:// scheme), and the File API
// the user picks via showOpenDialog only returns paths — not bytes.
//
// readFile is kept around for the rare callers that genuinely need
// the whole-buffer hop through IPC (e.g. small config-style files).
// For uploads, the renderer uses `decibell:file:register` instead,
// which returns a `decibell-file://` URL that streams from disk via
// the protocol handler — no full-file structured clone, ever.

const EXT_TO_MIME: Record<string, string> = {
  png: "image/png", jpg: "image/jpeg", jpeg: "image/jpeg", gif: "image/gif",
  webp: "image/webp", bmp: "image/bmp", tiff: "image/tiff", avif: "image/avif",
  mp4: "video/mp4", webm: "video/webm", mov: "video/quicktime",
  mkv: "video/x-matroska", avi: "video/x-msvideo",
  mp3: "audio/mpeg", wav: "audio/wav", ogg: "audio/ogg",
  m4a: "audio/mp4", flac: "audio/flac", opus: "audio/opus",
  pdf: "application/pdf", txt: "text/plain", json: "application/json",
};

function guessMime(filename: string): string {
  const ext = filename.toLowerCase().split(".").pop() ?? "";
  return EXT_TO_MIME[ext] ?? "application/octet-stream";
}

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
  ipcMain.handle(
    "decibell:fs:writeFile",
    async (_e, p: string, data: Uint8Array): Promise<void> => {
      // Wrap in a real Node Buffer view of the same memory — fs.writeFile
      // accepts Uint8Array directly but Buffer is the Node-idiomatic
      // path and avoids any structured-clone re-wrapping cost.
      await fs.writeFile(p, Buffer.from(data.buffer, data.byteOffset, data.byteLength));
    },
  );

  // Register an absolute path with the file whitelist. Renderer gets
  // back a `decibell-file://` URL it can fetch (with Range support) +
  // size/mime/name metadata for the upload state. The path itself
  // never crosses IPC back to the renderer.
  ipcMain.handle(
    "decibell:file:register",
    async (
      _e,
      absolutePath: string,
    ): Promise<{ url: string; size: number; mime: string; name: string }> => {
      const stat = await fs.stat(absolutePath);
      if (!stat.isFile()) {
        throw new Error(`not a file: ${absolutePath}`);
      }
      const name = path.basename(absolutePath);
      const mime = guessMime(name);
      const token = registerFile({
        path: absolutePath,
        size: stat.size,
        mime,
        name,
      });
      return {
        url: `decibell-file://file/${encodeURIComponent(token)}`,
        size: stat.size,
        mime,
        name,
      };
    },
  );

  // Renderer signals it's done with a file (upload completed, aborted,
  // or component unmounted). Drops the whitelist entry.
  ipcMain.handle("decibell:file:unregister", (_e, url: string): void => {
    try {
      const u = new URL(url);
      const parts = u.pathname.split("/").filter((p) => p.length > 0);
      if (parts.length !== 1) return;
      unregisterFile(decodeURIComponent(parts[0]));
    } catch {
      // Bad URL — nothing to clean up.
    }
  });
}
