// ThumbHash placeholders — a ~25-byte blurred preview of an image,
// carried in the message payload itself.
//
// Why this exists: prefetching (attachmentPrefetch.ts) warms images
// near the viewport, but it cannot cover the two cases that still show
// an empty box — the first screen of a channel, where nothing has been
// prefetched yet, and a fling that outruns the prefetch window. In both
// the <img> mounts before its bytes exist and paints nothing for a
// frame or two.
//
// A placeholder removes the empty state entirely rather than racing it.
// The hash is small enough to ride along with the attachment metadata,
// so painting it costs zero requests and zero latency: by the time the
// row can render at all, the blur is already in hand.
//
// The uploader computes it from the bitmap it already decodes for
// thumbnails; the server stores the bytes opaquely and echoes them
// back. Nothing but this file needs to know the format.

import { rgbaToThumbHash, thumbHashToRGBA } from "thumbhash";

/// ThumbHash's encoder wants a max 100x100 input, and quality is
/// insensitive well below that — the output is a handful of DCT
/// coefficients either way.
const ENCODE_MAX_EDGE = 100;

/// Encoded hashes are ~25 bytes; anything wildly larger came from
/// somewhere else and shouldn't be handed to the decoder.
const MAX_HASH_BYTES = 64;

function toBase64(bytes: Uint8Array): string {
  let s = "";
  for (let i = 0; i < bytes.length; i++) s += String.fromCharCode(bytes[i]);
  return btoa(s);
}

function fromBase64(b64: string): Uint8Array | null {
  try {
    const bin = atob(b64);
    if (bin.length === 0 || bin.length > MAX_HASH_BYTES) return null;
    const out = new Uint8Array(bin.length);
    for (let i = 0; i < bin.length; i++) out[i] = bin.charCodeAt(i);
    return out;
  } catch {
    return null;
  }
}

/**
 * Encode a blob (still image, or a captured video poster frame) to a
 * base64 ThumbHash. Returns "" on any failure — a missing placeholder
 * is a cosmetic downgrade, never a reason to fail an upload.
 */
export async function encodeThumbHash(blob: Blob): Promise<string> {
  try {
    const bitmap = await createImageBitmap(blob);
    const scale = Math.min(
      1,
      ENCODE_MAX_EDGE / Math.max(bitmap.width, bitmap.height),
    );
    const w = Math.max(1, Math.round(bitmap.width * scale));
    const h = Math.max(1, Math.round(bitmap.height * scale));
    const canvas = new OffscreenCanvas(w, h);
    const ctx = canvas.getContext("2d", { willReadFrequently: true });
    if (!ctx) {
      bitmap.close();
      return "";
    }
    ctx.drawImage(bitmap, 0, 0, w, h);
    bitmap.close();
    const { data } = ctx.getImageData(0, 0, w, h);
    return toBase64(rgbaToThumbHash(w, h, data));
  } catch {
    return "";
  }
}

/// Decoded hashes keyed by the base64 string. A channel scrolls the
/// same rows past repeatedly and the decode + canvas encode is ~1ms;
/// bounded so a long session can't grow it without limit.
const decoded = new Map<string, string>();
const DECODED_MAX = 400;

/**
 * Decode a base64 ThumbHash to a data URL suitable for `background-image`.
 *
 * Returns null when the input is empty or malformed, which is the
 * normal case for pre-thumbhash uploads and non-image attachments —
 * callers fall back to a flat fill.
 *
 * Synchronous on purpose: it has to be available during the render that
 * mounts the row, or it would be racing the very fetch it exists to
 * cover.
 */
export function thumbHashToDataUrl(hashB64: string): string | null {
  if (!hashB64) return null;
  const hit = decoded.get(hashB64);
  if (hit !== undefined) return hit || null;

  let url = "";
  try {
    const bytes = fromBase64(hashB64);
    if (bytes) {
      const { w, h, rgba } = thumbHashToRGBA(bytes);
      const canvas = document.createElement("canvas");
      canvas.width = w;
      canvas.height = h;
      const ctx = canvas.getContext("2d");
      if (ctx) {
        const img = ctx.createImageData(w, h);
        img.data.set(rgba);
        ctx.putImageData(img, 0, 0);
        url = canvas.toDataURL("image/png");
      }
    }
  } catch {
    url = "";
  }

  if (decoded.size >= DECODED_MAX) {
    // Cheap FIFO trim — a re-decode costs ~1ms, so exact LRU isn't worth it.
    let n = DECODED_MAX / 4;
    for (const k of decoded.keys()) {
      decoded.delete(k);
      if (--n <= 0) break;
    }
  }
  // Cache failures too ("") so a malformed hash isn't retried every render.
  decoded.set(hashB64, url);
  return url || null;
}
