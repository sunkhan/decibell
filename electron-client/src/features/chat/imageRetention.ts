// Keeps recently shown attachment images in memory for the session, so
// returning to a channel paints them at once instead of re-downloading.
//
// Why this exists: once a row unmounts, nothing references its image, and
// Chromium holds the bytes of an unreferenced image only weakly past a few
// MB per page — the next garbage collection can drop them. Collections run
// as channel switches allocate, so "switch away and back" kept the images
// or lost them depending on GC timing (measured: with a GC between
// switches, 29 of 30 images of the channel left behind were fetched again).
// `decibell-attachment://` responses never reach the disk cache, so a
// dropped image is a full round trip to the community server.
//
// An `Image` holding the same URL keeps that copy alive: a remounted <img>
// is then a memory-cache hit. Entries are kept most-recently-shown first
// within a byte budget; nothing survives a restart.

/// Estimated encoded bytes kept alive. Thumbnails are bounded JPEGs
/// (≤1280px, ~100–300 KB), so this holds several hundred of them.
const BUDGET_BYTES = 128 * 1024 * 1024;

/// JPEG thumbnails land around 0.3–0.5 bytes per pixel.
const BYTES_PER_PIXEL = 0.4;

const entries = new Map<string, { img: HTMLImageElement; bytes: number }>();
let total = 0;

/// Encoded size estimate for a bounded thumbnail of these dimensions.
export function thumbnailBytes(width: number, height: number): number {
  return Math.max(1, Math.round(width * height * BYTES_PER_PIXEL));
}

/// Mark `url` as just shown. Call it once the image is (or is being)
/// displayed: the holder's request then joins the one already loading or
/// finished, so it costs no extra fetch.
export function retainImage(url: string, bytes: number): void {
  const hit = entries.get(url);
  if (hit) {
    entries.delete(url);
    entries.set(url, hit);
    return;
  }
  const img = new Image();
  img.decoding = "async";
  img.src = url;
  entries.set(url, { img, bytes });
  total += bytes;
  // Oldest first; always keep the one just added.
  for (const [key, entry] of entries) {
    if (total <= BUDGET_BYTES || entries.size <= 1) break;
    entries.delete(key);
    total -= entry.bytes;
    entry.img.src = "";
  }
}

/// Forget everything (sign-out).
export function clearRetainedImages(): void {
  for (const entry of entries.values()) entry.img.src = "";
  entries.clear();
  total = 0;
}
