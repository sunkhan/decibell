// Encrypted-channel attachments — the decrypt side (main process) and
// the byte math both sides share. The renderer encrypts on upload with
// WebCrypto (src/features/chat/attachmentCrypto.ts) using the same
// layout; the two must stay in step.
//
// Layout (spec docs/superpowers/specs/2026-09-04-encrypted-text-channels-design.md):
//   content   = ‖ over chunks i of AES-256-GCM(key, nonce_i, plain[i*C .. min((i+1)*C, size)],
//                                             aad = "decibell-att-v1" ‖ 0x00 ‖ u64le(i)) ‖ tag16
//   nonce_i   = u64le(i) ‖ 0x00 0x00 0x00 0x01
//   thumbnail = AES-256-GCM(key, u64le(sizePx) ‖ 0x00 0x00 0x00 0x02, jpeg, aad = "decibell-att-v1" ‖ 0x01 ‖ u64le(sizePx)) ‖ tag16
//
// C (chunk_bytes) is carried in the message envelope next to the key,
// so a range of plaintext maps to a range of whole sealed chunks:
// chunk i occupies ciphertext bytes [i*(C+16), (i+1)*(C+16)).

import { createDecipheriv } from "node:crypto";

export const TAG_BYTES = 16;
export const DEFAULT_CHUNK_BYTES = 64 * 1024;
const AAD_DOMAIN = Buffer.from("decibell-att-v1", "utf8");

export interface AttachmentKeyInfo {
  /// 32-byte AES-256-GCM key.
  key: Buffer;
  chunkBytes: number;
  /// Plaintext size of the content.
  sizeBytes: number;
  mime: string;
  filename: string;
}

function u64le(n: number): Buffer {
  const b = Buffer.alloc(8);
  b.writeBigUInt64LE(BigInt(n));
  return b;
}

export function contentNonce(chunkIndex: number): Buffer {
  return Buffer.concat([u64le(chunkIndex), Buffer.from([0, 0, 0, 1])]);
}

export function thumbnailNonce(sizePx: number): Buffer {
  return Buffer.concat([u64le(sizePx), Buffer.from([0, 0, 0, 2])]);
}

export function contentAad(chunkIndex: number): Buffer {
  return Buffer.concat([AAD_DOMAIN, Buffer.from([0]), u64le(chunkIndex)]);
}

export function thumbnailAad(sizePx: number): Buffer {
  return Buffer.concat([AAD_DOMAIN, Buffer.from([1]), u64le(sizePx)]);
}

/// Number of sealed chunks for a plaintext of `sizeBytes`.
export function chunkCount(sizeBytes: number, chunkBytes: number): number {
  return sizeBytes === 0 ? 0 : Math.ceil(sizeBytes / chunkBytes);
}

/// Total ciphertext size for a plaintext of `sizeBytes`.
export function ciphertextSize(sizeBytes: number, chunkBytes: number): number {
  return sizeBytes + chunkCount(sizeBytes, chunkBytes) * TAG_BYTES;
}

/// Ciphertext byte range (inclusive-exclusive) covering plaintext chunks
/// [firstChunk, lastChunk].
export function ciphertextRangeForChunks(
  firstChunk: number,
  lastChunk: number,
  chunkBytes: number,
): { start: number; end: number } {
  const sealed = chunkBytes + TAG_BYTES;
  return { start: firstChunk * sealed, end: (lastChunk + 1) * sealed };
}

/// Open one sealed chunk (ciphertext ‖ tag). Throws on a bad tag.
export function decryptChunk(key: Buffer, chunkIndex: number, sealed: Buffer): Buffer {
  if (sealed.length < TAG_BYTES) throw new Error("sealed chunk too short");
  const body = sealed.subarray(0, sealed.length - TAG_BYTES);
  const tag = sealed.subarray(sealed.length - TAG_BYTES);
  const d = createDecipheriv("aes-256-gcm", key, contentNonce(chunkIndex));
  d.setAAD(contentAad(chunkIndex));
  d.setAuthTag(tag);
  return Buffer.concat([d.update(body), d.final()]);
}

/// Open a sealed thumbnail.
export function decryptThumbnail(key: Buffer, sizePx: number, sealed: Buffer): Buffer {
  if (sealed.length < TAG_BYTES) throw new Error("sealed thumbnail too short");
  const body = sealed.subarray(0, sealed.length - TAG_BYTES);
  const tag = sealed.subarray(sealed.length - TAG_BYTES);
  const d = createDecipheriv("aes-256-gcm", key, thumbnailNonce(sizePx));
  d.setAAD(thumbnailAad(sizePx));
  d.setAuthTag(tag);
  return Buffer.concat([d.update(body), d.final()]);
}

/// Parse `bytes=a-b` / `bytes=a-` / `bytes=-n` against a plaintext size.
/// Returns null for no/invalid header (→ full response).
export function parseRange(header: string | undefined, size: number): { start: number; end: number } | null {
  if (!header) return null;
  const m = /^bytes=(\d*)-(\d*)$/.exec(header.trim());
  if (!m) return null;
  const [, a, b] = m;
  if (a === "" && b === "") return null;
  let start: number;
  let end: number;
  if (a === "") {
    const n = parseInt(b, 10);
    start = Math.max(0, size - n);
    end = size - 1;
  } else {
    start = parseInt(a, 10);
    end = b === "" ? size - 1 : Math.min(size - 1, parseInt(b, 10));
  }
  if (!Number.isFinite(start) || !Number.isFinite(end) || start > end || start >= size) return null;
  return { start, end };
}

/// Serve a plaintext byte range [start, end] of an encrypted attachment:
/// which ciphertext range to fetch upstream, and how to trim the decrypted
/// chunks to the requested window.
export function planRange(
  info: AttachmentKeyInfo,
  range: { start: number; end: number } | null,
): {
  firstChunk: number;
  lastChunk: number;
  upstreamRange: string | null;
  trimStart: number;
  trimEnd: number;
  plainStart: number;
  plainEnd: number;
} {
  const total = info.sizeBytes;
  const plainStart = range ? range.start : 0;
  const plainEnd = range ? range.end : Math.max(0, total - 1);
  const firstChunk = Math.floor(plainStart / info.chunkBytes);
  const lastChunk = Math.floor(plainEnd / info.chunkBytes);
  const ct = ciphertextRangeForChunks(firstChunk, lastChunk, info.chunkBytes);
  const ctTotal = ciphertextSize(total, info.chunkBytes);
  const ctEnd = Math.min(ct.end, ctTotal) - 1;
  return {
    firstChunk,
    lastChunk,
    // Fetch every needed chunk (a no-range request still asks for the
    // exact ciphertext extent so a short upstream read is detectable).
    upstreamRange: total === 0 ? null : `bytes=${ct.start}-${ctEnd}`,
    trimStart: plainStart - firstChunk * info.chunkBytes,
    trimEnd: plainEnd - firstChunk * info.chunkBytes + 1,
    plainStart,
    plainEnd,
  };
}
