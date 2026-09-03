// Encrypted-channel attachments — the encrypt side (renderer, WebCrypto).
// Must stay byte-for-byte in step with electron/main/attachmentCrypto.ts,
// which decrypts:
//
//   content   = ‖ over chunks i of AES-256-GCM(key, nonce_i, plain chunk i,
//                                             aad = "decibell-att-v1" ‖ 0x00 ‖ u64le(i)) ‖ tag16
//   nonce_i   = u64le(i) ‖ 00 00 00 01
//   thumbnail = AES-256-GCM(key, u64le(sizePx) ‖ 00 00 00 02, jpeg,
//                           aad = "decibell-att-v1" ‖ 0x01 ‖ u64le(sizePx)) ‖ tag16
//
// One random key per attachment; the key travels inside the message
// envelope, never to the server. Deterministic nonces are safe because
// the key is single-use, and they let a retried upload chunk re-encrypt
// to identical bytes.

import type { Attachment } from "../../types";

export const CHUNK_BYTES = 64 * 1024;
export const TAG_BYTES = 16;
const AAD_DOMAIN = new TextEncoder().encode("decibell-att-v1");

export interface AttachmentCipher {
  keyB64: string;
  cryptoKey: CryptoKey;
}

function u64le(n: number): Uint8Array {
  const b = new Uint8Array(8);
  new DataView(b.buffer).setBigUint64(0, BigInt(n), true);
  return b;
}

function concat(...parts: Uint8Array[]): Uint8Array {
  const total = parts.reduce((n, p) => n + p.length, 0);
  const out = new Uint8Array(total);
  let off = 0;
  for (const p of parts) {
    out.set(p, off);
    off += p.length;
  }
  return out;
}

function toB64(bytes: Uint8Array): string {
  let s = "";
  for (const b of bytes) s += String.fromCharCode(b);
  return btoa(s);
}

export async function newAttachmentCipher(): Promise<AttachmentCipher> {
  const raw = new Uint8Array(32);
  crypto.getRandomValues(raw);
  const cryptoKey = await crypto.subtle.importKey("raw", raw, { name: "AES-GCM" }, false, ["encrypt"]);
  return { keyB64: toB64(raw), cryptoKey };
}

export function chunkCount(sizeBytes: number): number {
  return sizeBytes === 0 ? 0 : Math.ceil(sizeBytes / CHUNK_BYTES);
}

/// Total ciphertext size the server should expect for a plaintext of `sizeBytes`.
export function ciphertextSize(sizeBytes: number): number {
  return sizeBytes + chunkCount(sizeBytes) * TAG_BYTES;
}

/// Ciphertext offset of plaintext chunk `index`.
export function ciphertextOffset(index: number): number {
  return index * (CHUNK_BYTES + TAG_BYTES);
}

/// Seal one plaintext chunk (`index` is its position in the file).
export async function sealChunk(cipher: AttachmentCipher, index: number, plain: Uint8Array): Promise<Uint8Array> {
  const iv = concat(u64le(index), new Uint8Array([0, 0, 0, 1]));
  const aad = concat(AAD_DOMAIN, new Uint8Array([0]), u64le(index));
  const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: aad, tagLength: 128 }, cipher.cryptoKey, plain);
  return new Uint8Array(ct);
}

/// Seal a whole plaintext span that starts at chunk `firstIndex` and is a
/// multiple of CHUNK_BYTES long (except possibly the file's last chunk).
export async function sealSpan(cipher: AttachmentCipher, firstIndex: number, plain: Uint8Array): Promise<Uint8Array> {
  const parts: Uint8Array[] = [];
  for (let off = 0, i = firstIndex; off < plain.length; off += CHUNK_BYTES, i += 1) {
    parts.push(await sealChunk(cipher, i, plain.subarray(off, Math.min(off + CHUNK_BYTES, plain.length))));
  }
  return concat(...parts);
}

/// The `encryptedAttachments` argument of send/edit_channel_message for
/// a message's already-decrypted attachments (an edit re-seals the body).
export function encryptedAttachmentArgs(atts: Attachment[] | undefined) {
  const out = (atts ?? [])
    .filter((a) => a.encrypted && a.keyB64)
    .map((a) => ({
      id: a.id,
      keyB64: a.keyB64!,
      filename: a.filename,
      mime: a.mime,
      sizeBytes: a.sizeBytes,
      width: a.width,
      height: a.height,
      durationMs: a.durationMs,
      placeholder: a.placeholder,
      chunkBytes: a.chunkBytes ?? CHUNK_BYTES,
      thumbnailSizesMask: a.thumbnailSizesMask,
    }));
  return out.length > 0 ? out : undefined;
}

/// Seal a JPEG thumbnail of long edge `sizePx`.
export async function sealThumbnail(cipher: AttachmentCipher, sizePx: number, jpeg: Uint8Array): Promise<Uint8Array> {
  const iv = concat(u64le(sizePx), new Uint8Array([0, 0, 0, 2]));
  const aad = concat(AAD_DOMAIN, new Uint8Array([1]), u64le(sizePx));
  const ct = await crypto.subtle.encrypt({ name: "AES-GCM", iv, additionalData: aad, tagLength: 128 }, cipher.cryptoKey, jpeg);
  return new Uint8Array(ct);
}
