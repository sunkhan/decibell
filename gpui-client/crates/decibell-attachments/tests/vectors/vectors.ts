// Cross-implementation vectors for decibell-attachments, made by the Electron client's own
// modules (bundled by gen_vectors.mjs with the repo's esbuild):
//   - the renderer's sealer   electron-client/src/features/chat/attachmentCrypto.ts (WebCrypto)
//   - main's opener + ranges  electron-client/electron/main/attachmentCrypto.ts (node:crypto)
//   - the ThumbHash encoder / decoder the renderer uses (npm `thumbhash`, via thumbhash.ts)
//
//   gen <dir> [--big]   write vectors.json (+ the small sealed files) into <dir>;
//                       --big also writes every sealed file (5 MiB included) for the Rust test
//   verify <dir>        open the files Rust sealed (manifest.json in <dir>) with main's code
//
// Plaintexts, keys and images come from a xorshift32 stream that the Rust tests reproduce.

import { createHash } from "node:crypto";
import { mkdirSync, readFileSync, writeFileSync } from "node:fs";
import { join } from "node:path";
import { rgbaToThumbHash, thumbHashToRGBA } from "thumbhash";
import * as renderer from "../../../../../electron-client/src/features/chat/attachmentCrypto";
import * as main from "../../../../../electron-client/electron/main/attachmentCrypto";

export const SIZES = [0, 1, 65535, 65536, 65537, 200 * 1024, 5 * 1024 * 1024];
const COMMITTED = new Set([0, 1, 65537]);

function xorshift(seed: number): () => number {
  let x = seed >>> 0 || 1;
  return () => {
    x ^= x << 13;
    x >>>= 0;
    x ^= x >>> 17;
    x ^= x << 5;
    x >>>= 0;
    return x;
  };
}

export function plaintext(size: number): Uint8Array {
  const next = xorshift(0x9e3779b9 ^ size);
  const out = new Uint8Array(size);
  for (let i = 0; i < size; i++) out[i] = next() & 0xff;
  return out;
}

export function keyFor(size: number): Uint8Array {
  const k = new Uint8Array(32);
  for (let i = 0; i < 32; i++) k[i] = (i * 31 + size * 7 + 11) & 0xff;
  return k;
}

const sha256 = (b: Uint8Array) => createHash("sha256").update(b).digest("hex");
const b64 = (b: Uint8Array) => Buffer.from(b).toString("base64");

async function cipherFrom(raw: Uint8Array): Promise<renderer.AttachmentCipher> {
  const cryptoKey = await crypto.subtle.importKey("raw", raw, { name: "AES-GCM" }, false, ["encrypt"]);
  return { keyB64: b64(raw), cryptoKey };
}

/// The upload loop's exact path: whole sealed chunks per read via sealSpan.
async function rendererSeal(cipher: renderer.AttachmentCipher, plain: Uint8Array): Promise<Uint8Array> {
  const span = 8 * renderer.CHUNK_BYTES; // any multiple of CHUNK_BYTES, as uploadChunkBytes() is
  const parts: Uint8Array[] = [];
  for (let off = 0; off < plain.length; off += span) {
    parts.push(await renderer.sealSpan(cipher, off / renderer.CHUNK_BYTES, plain.subarray(off, Math.min(off + span, plain.length))));
  }
  return Buffer.concat(parts);
}

function mainOpenRange(key: Buffer, sealed: Buffer, size: number, header: string | undefined) {
  const info = { key, chunkBytes: main.DEFAULT_CHUNK_BYTES, sizeBytes: size, mime: "application/octet-stream", filename: "x" };
  const range = main.parseRange(header, size);
  if (header && !range) return { parsed: null, plan: null, plainSha256: null, plainLen: 0 };
  const plan = main.planRange(info, range);
  const sealedChunk = info.chunkBytes + main.TAG_BYTES;
  const ctTotal = main.ciphertextSize(size, info.chunkBytes);
  const parts: Buffer[] = [];
  for (let i = plan.firstChunk; i <= plan.lastChunk && size > 0; i++) {
    const start = i * sealedChunk;
    const len = Math.min(sealedChunk, ctTotal - start);
    parts.push(main.decryptChunk(key, i, sealed.subarray(start, start + len)));
  }
  const plain = Buffer.concat(parts).subarray(plan.trimStart, plan.trimEnd);
  return {
    parsed: range ? [range.start, range.end] : null,
    plan: {
      firstChunk: plan.firstChunk,
      lastChunk: plan.lastChunk,
      upstreamRange: plan.upstreamRange,
      trimStart: plan.trimStart,
      trimEnd: plan.trimEnd,
      plainStart: plan.plainStart,
      plainEnd: plan.plainEnd,
    },
    plainSha256: sha256(plain),
    plainLen: plain.length,
  };
}

const RANGE_HEADERS = [
  undefined,
  "bytes=0-0",
  "bytes=0-",
  "bytes=65535-65535",
  "bytes=65535-65536",
  "bytes=65536-",
  "bytes=65536-131071",
  "bytes=131071-131072",
  "bytes=100000-150000",
  "bytes=-1",
  "bytes=-65537",
  "bytes=204799-",
  "bytes=204800-",
  "bytes=5-3",
  "bytes=0-999999999",
];

/// Deterministic RGBA test images.
export function image(kind: string, w: number, h: number): Uint8Array {
  const px = new Uint8Array(w * h * 4);
  const next = xorshift(0x1234567 ^ (w * 131 + h));
  for (let y = 0; y < h; y++) {
    for (let x = 0; x < w; x++) {
      const i = (y * w + x) * 4;
      if (kind === "gradient") {
        px[i] = Math.floor((x * 255) / Math.max(1, w - 1));
        px[i + 1] = Math.floor((y * 255) / Math.max(1, h - 1));
        px[i + 2] = 128;
        px[i + 3] = 255;
      } else if (kind === "noise") {
        const v = next();
        px[i] = v & 255;
        px[i + 1] = (v >>> 8) & 255;
        px[i + 2] = (v >>> 16) & 255;
        px[i + 3] = 255;
      } else if (kind === "alpha") {
        px[i] = 220;
        px[i + 1] = Math.floor((x * 255) / Math.max(1, w - 1));
        px[i + 2] = 40;
        px[i + 3] = Math.floor((y * 255) / Math.max(1, h - 1));
      } else if (kind === "noisealpha") {
        const v = next();
        px[i] = v & 255;
        px[i + 1] = (v >>> 8) & 255;
        px[i + 2] = (v >>> 16) & 255;
        px[i + 3] = (v >>> 24) & 255;
      } else if (kind === "clear") {
        px[i + 3] = 0;
      } else {
        px[i] = 30;
        px[i + 1] = 144;
        px[i + 2] = 255;
        px[i + 3] = 255;
      }
    }
  }
  return px;
}

/// Random pixels (smooth-ish: a noisy gradient, so the hash has structure).
export function fuzzImage(seed: number, w: number, h: number, alpha: boolean): Uint8Array {
  const next = xorshift(seed);
  const px = new Uint8Array(w * h * 4);
  const base = [next() & 255, next() & 255, next() & 255];
  for (let i = 0; i < w * h; i++) {
    const v = next();
    px[i * 4] = (base[0] + (v & 63)) & 255;
    px[i * 4 + 1] = (base[1] + ((v >>> 6) & 63) + (i % w)) & 255;
    px[i * 4 + 2] = (base[2] + ((v >>> 12) & 63)) & 255;
    px[i * 4 + 3] = alpha ? (v >>> 24) & 255 : 255;
  }
  return px;
}

const IMAGES: Array<[string, number, number]> = [
  ["gradient", 100, 75],
  ["gradient", 75, 100],
  ["noise", 37, 100],
  ["noise", 100, 100],
  ["alpha", 64, 64],
  ["noisealpha", 57, 43],
  ["solid", 1, 1],
  ["solid", 3, 2],
  ["gradient", 100, 1],
  ["clear", 100, 100],
];

async function gen(dir: string, big: boolean): Promise<void> {
  mkdirSync(dir, { recursive: true });
  const sealedOut: unknown[] = [];
  let sealed200: Buffer | null = null;
  for (const size of SIZES) {
    const raw = keyFor(size);
    const cipher = await cipherFrom(raw);
    const plain = plaintext(size);
    const sealed = Buffer.from(await rendererSeal(cipher, plain));
    if (sealed.length !== renderer.ciphertextSize(size)) throw new Error("ciphertextSize disagrees");
    // main's opener must read the renderer's output (the two TS modules agree).
    const opened = mainOpenRange(Buffer.from(raw), sealed, size, undefined);
    if (size > 0 && opened.plainSha256 !== sha256(plain)) throw new Error(`main can't open renderer size ${size}`);
    if (COMMITTED.has(size) || big) writeFileSync(join(dir, `sealed-${size}.bin`), sealed);
    if (size === 200 * 1024) sealed200 = sealed;
    sealedOut.push({ size, keyB64: b64(raw), plainSha256: sha256(plain), sealedSha256: sha256(sealed), sealedLen: sealed.length });
  }

  // A key from the renderer's own generator (random), to show nothing depends on our keys.
  const rnd = await renderer.newAttachmentCipher();
  const rndPlain = plaintext(70000);
  const rndSealed = Buffer.from(await rendererSeal(rnd, rndPlain));
  writeFileSync(join(dir, "sealed-random-key.bin"), rndSealed);

  const thumbs: unknown[] = [];
  for (const sizePx of [320, 640, 1280]) {
    const raw = keyFor(sizePx);
    const jpeg = plaintext(1000 + sizePx);
    const sealed = await renderer.sealThumbnail(await cipherFrom(raw), sizePx, jpeg);
    if (sha256(main.decryptThumbnail(Buffer.from(raw), sizePx, Buffer.from(sealed))) !== sha256(jpeg)) throw new Error("thumb");
    thumbs.push({ sizePx, keyB64: b64(raw), jpegLen: jpeg.length, jpegSha256: sha256(jpeg), sealedB64: b64(sealed) });
  }

  const ranges: unknown[] = [];
  const key200 = Buffer.from(keyFor(200 * 1024));
  for (const header of RANGE_HEADERS) {
    ranges.push({ size: 200 * 1024, header: header ?? null, ...mainOpenRange(key200, sealed200!, 200 * 1024, header) });
  }
  const sealed65537 = Buffer.from(await rendererSeal(await cipherFrom(keyFor(65537)), plaintext(65537)));
  for (const header of ["bytes=65536-", "bytes=65535-65536", "bytes=-2", "bytes=0-65536"]) {
    ranges.push({ size: 65537, header, ...mainOpenRange(Buffer.from(keyFor(65537)), sealed65537, 65537, header) });
  }

  const thumbhash: unknown[] = [];
  for (const [kind, w, h] of IMAGES) {
    const rgba = image(kind, w, h);
    const hash = rgbaToThumbHash(w, h, rgba);
    const d = thumbHashToRGBA(hash);
    thumbhash.push({ kind, w, h, rgbaSha256: sha256(rgba), hashB64: b64(hash), decoded: { w: d.w, h: d.h, sha256: sha256(d.rgba) } });
  }

  // Random pictures and random hashes: the encoder and decoder must agree on all of them.
  const fuzzNext = xorshift(0xc0ffee);
  const fuzzImages: unknown[] = [];
  for (let n = 0; n < 300; n++) {
    const w = 1 + (fuzzNext() % 100);
    const h = 1 + (fuzzNext() % 100);
    const seed = fuzzNext();
    const alpha = n % 3 === 0;
    const rgba = fuzzImage(seed, w, h, alpha);
    fuzzImages.push({ seed, w, h, alpha, hashB64: b64(rgbaToThumbHash(w, h, rgba)) });
  }
  const fuzzHashes: unknown[] = [];
  for (let n = 0; n < 500; n++) {
    const len = 5 + (fuzzNext() % 20);
    const hash = new Uint8Array(len);
    for (let i = 0; i < len; i++) hash[i] = fuzzNext() & 0xff;
    const d = thumbHashToRGBA(hash);
    fuzzHashes.push({ hashB64: b64(hash), w: d.w, h: d.h, sha256: sha256(d.rgba) });
  }

  const out = {
    generatedBy: "tests/vectors/vectors.ts (electron-client attachmentCrypto.ts x2 + thumbhash)",
    engine: `${process.versions.electron ? "electron " + process.versions.electron : "node " + process.version} / v8 ${process.versions.v8}`,
    chunkBytes: renderer.CHUNK_BYTES,
    sealed: sealedOut,
    randomKey: { size: 70000, keyB64: rnd.keyB64, plainSha256: sha256(rndPlain), file: "sealed-random-key.bin" },
    thumbnails: thumbs,
    ranges,
    thumbhash,
    fuzzImages,
    fuzzHashes,
  };
  writeFileSync(join(dir, "vectors.json"), JSON.stringify(out, null, 1) + "\n");
}

/// Open what Rust sealed, with main's decryptor only.
function verify(dir: string): void {
  const manifest = JSON.parse(readFileSync(join(dir, "manifest.json"), "utf8")) as {
    files: Array<{ file: string; size: number; keyB64: string; plainSha256: string }>;
    thumbnails: Array<{ sizePx: number; keyB64: string; sealedB64: string; jpegSha256: string }>;
  };
  const results: unknown[] = [];
  let ok = true;
  for (const f of manifest.files) {
    const key = Buffer.from(f.keyB64, "base64");
    const sealed = readFileSync(join(dir, f.file));
    let got: string | null = null;
    let err = "";
    try {
      if (sealed.length !== main.ciphertextSize(f.size, main.DEFAULT_CHUNK_BYTES)) throw new Error("ciphertext size");
      got = f.size === 0 ? sha256(new Uint8Array(0)) : mainOpenRange(key, sealed, f.size, undefined).plainSha256;
    } catch (e) {
      err = String(e);
    }
    const pass = got === f.plainSha256;
    ok &&= pass;
    results.push({ file: f.file, size: f.size, pass, err });
  }
  for (const t of manifest.thumbnails) {
    let pass = false;
    try {
      const jpeg = main.decryptThumbnail(Buffer.from(t.keyB64, "base64"), t.sizePx, Buffer.from(t.sealedB64, "base64"));
      pass = sha256(jpeg) === t.jpegSha256;
    } catch {
      pass = false;
    }
    ok &&= pass;
    results.push({ thumbnail: t.sizePx, pass });
  }
  process.stdout.write(JSON.stringify({ ok, results }) + "\n");
}

export async function run(argv: string[]): Promise<void> {
  const [cmd, dir, flag] = argv;
  if (cmd === "gen" && dir) return gen(dir, flag === "--big");
  if (cmd === "verify" && dir) return verify(dir);
  throw new Error("usage: gen <dir> [--big] | verify <dir>");
}
