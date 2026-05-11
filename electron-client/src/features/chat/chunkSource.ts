// Unified chunk-source abstraction for attachment uploads. The upload
// loop reads bytes through a ChunkSource without caring whether the
// source is:
//   - a path the user picked (streamed from disk via decibell-file://)
//   - a File from drag-drop with a backing path (also via decibell-file://)
//   - a File from clipboard paste with no path (browser-owned Blob)
//
// All three expose:
//   - `url`: usable as `<img src=>`/`<video src=>` for in-renderer probes
//   - `readChunk(start, end)`: byte-range read, called once per upload chunk
//
// Why per-source `readChunk` rather than a shared `fetch(url, {Range})`?
// Chromium's blob: URL fetch implementation **ignores Range headers** —
// it returns the full Blob with status 200, not the requested slice.
// That works for an image-element src but corrupts a chunked upload.
// `decibell-file://` (handled by main's net.fetch on file://) does
// honour Range correctly. So path-backed sources use range fetch and
// truly stream from disk; blob-backed sources use `file.slice()` and
// the Blob's bytes stay where Chromium already owns them.
//
// `cleanup()` is idempotent. Callers MUST call it after the upload
// completes, fails, or is aborted — otherwise decibell-file:// entries
// linger on the whitelist (a 1h stale sweep eventually catches them)
// and blob: URLs leak until GC.

export interface ChunkSource {
  /// Fetchable URL — usable directly as an `<img>` / `<video>` /
  /// `<audio>` src for the upload-bubble preview and probeMetadata.
  url: string;
  size: number;
  mime: string;
  name: string;
  /// Read a half-open byte range [start, end) as a Uint8Array.
  readChunk: (start: number, end: number, signal?: AbortSignal) => Promise<Uint8Array>;
  cleanup: () => void;
}

/// Build a ChunkSource from an absolute disk path the user picked /
/// dropped. Registers the path with main; bytes never cross IPC as a
/// single buffer. Per-chunk reads go through `decibell-file://` with
/// a Range header — main's net.fetch streams the requested slice
/// straight from disk via `file://`, so even for a 500MB upload the
/// renderer only ever holds one chunk.
export async function chunkSourceFromPath(
  absolutePath: string,
): Promise<ChunkSource> {
  const { url, size, mime, name } = await window.decibell.file.register(absolutePath);
  let released = false;
  return {
    url,
    size,
    mime,
    name,
    readChunk: async (start, end, signal) => {
      if (start >= end) return new Uint8Array(0);
      const resp = await fetch(url, {
        method: "GET",
        headers: { Range: `bytes=${start}-${end - 1}` },
        signal,
      });
      if (!resp.ok && resp.status !== 206 && resp.status !== 200) {
        throw new Error(`chunk read failed: HTTP ${resp.status}`);
      }
      const buf = await resp.arrayBuffer();
      return new Uint8Array(buf);
    },
    cleanup: () => {
      if (released) return;
      released = true;
      window.decibell.file.unregister(url).catch(() => {});
    },
  };
}

/// Build a ChunkSource from a browser File object (drag-drop or paste).
/// If the File has a backing disk path (drag-drop from the OS via
/// `webUtils.getPathForFile`), prefer the streaming `decibell-file://`
/// route — that way a 500MB drag-dropped video also gets the lazy-disk
/// treatment instead of materialising as a Blob in renderer RAM.
/// Pasted clipboard files have no path; fall back to a Blob URL +
/// `file.slice()` for chunk reads (the bytes already live in
/// Chromium's Blob storage; we don't make a second copy).
export async function chunkSourceFromFile(file: File): Promise<ChunkSource> {
  const path = window.decibell.file.pathOf(file);
  // Only take the path-streaming route for genuine absolute disk
  // paths. Cross-app drags (e.g. images out of Discord) sometimes
  // hand back a non-path string (URL, internal Chromium handle) —
  // passing those to chunkSourceFromPath would either fail at
  // fs.stat in main or, worse, register a bogus "path" with the
  // file whitelist. Accept only POSIX absolute (/...) or Windows
  // drive-letter (C:\... / \\server\...) paths.
  if (path && isAbsoluteDiskPath(path)) {
    try {
      return await chunkSourceFromPath(path);
    } catch (e) {
      // fs.stat rejected — fall through to the Blob path so the
      // upload still works via Chromium's in-renderer File bytes.
      console.warn("[chunkSource] path register failed, falling back to Blob:", e);
    }
  }
  const url = URL.createObjectURL(file);
  let released = false;
  return {
    url,
    size: file.size,
    mime: file.type || "application/octet-stream",
    name: file.name,
    readChunk: async (start, end) => {
      // file.slice(start, end) returns a Blob view over the same
      // underlying bytes — no copy. arrayBuffer() materialises just
      // the slice's bytes, not the whole file.
      if (start >= end) return new Uint8Array(0);
      const blob = file.slice(start, end);
      const buf = await blob.arrayBuffer();
      return new Uint8Array(buf);
    },
    cleanup: () => {
      if (released) return;
      released = true;
      URL.revokeObjectURL(url);
    },
  };
}

/// True only for genuine absolute disk paths (POSIX `/...` or Windows
/// `C:\...` / UNC `\\server\...`). Used to gate which drag-drop files
/// take the streaming `decibell-file://` route — anything else falls
/// back to a Blob URL so we don't ship URLs / weird internal handles
/// to main's `fs.stat`.
function isAbsoluteDiskPath(p: string): boolean {
  if (!p) return false;
  if (p.startsWith("/")) return true; // POSIX
  if (/^[A-Za-z]:[\\/]/.test(p)) return true; // Windows drive
  if (p.startsWith("\\\\")) return true; // Windows UNC
  return false;
}
