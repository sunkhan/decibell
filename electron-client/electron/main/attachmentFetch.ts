// Fetch + decrypt an encrypted-channel attachment in main. Shared by the
// three paths that hand attachment bytes to the renderer: the
// decibell-attachment:// protocol (images, save-as), the loopback media
// server (<video>/<audio>) and netFetch (save-as). Plain attachments
// are proxied by the callers as before; this only engages when the
// renderer has registered a key for (serverId, attachmentId).
//
// A plaintext byte range maps to a range of whole sealed chunks, so a
// <video> seek still costs one upstream Range request — we fetch the
// covering chunks, open them, and trim. The needed span is buffered
// rather than streamed: a media probe is a few chunks, a save-as is the
// file once; neither wants a streaming decipher pipeline's complexity.

import { net } from "electron";
import { getAttachmentTarget } from "./attachmentRegistry";
import { getAttachmentKey } from "./attachmentKeys";
import {
  TAG_BYTES,
  ciphertextSize,
  decryptChunk,
  decryptThumbnail,
  parseRange,
  planRange,
} from "./attachmentCrypto";

export interface DecryptedResponse {
  status: number;
  statusText: string;
  headers: Record<string, string>;
  body: Uint8Array;
}

function text(status: number, statusText: string, msg: string): DecryptedResponse {
  return {
    status,
    statusText,
    headers: { "Content-Type": "text/plain" },
    body: new TextEncoder().encode(msg),
  };
}

/// `null` when the attachment isn't a registered encrypted one — the
/// caller proxies upstream bytes as usual.
export async function fetchDecryptedAttachment(
  serverId: string,
  attachmentId: string,
  search: string,
  rangeHeader?: string,
): Promise<DecryptedResponse | null> {
  const info = getAttachmentKey(serverId, attachmentId);
  if (!info) return null;
  const target = getAttachmentTarget(serverId);
  if (!target) return text(404, "Not Found", "not connected");
  const upstream = `https://${target.host}:${target.port}/attachments/${attachmentId}${search}`;
  const params = new URLSearchParams(search);
  const auth = { Authorization: `Bearer ${target.jwt}` };

  if (params.get("variant") === "thumb") {
    const sizePx = Number(params.get("size")) || 0;
    const resp = await net.fetch(upstream, { method: "GET", headers: auth });
    if (!resp.ok) return text(resp.status, resp.statusText, `upstream ${resp.status}`);
    const sealed = Buffer.from(await resp.arrayBuffer());
    let jpeg: Buffer;
    try {
      jpeg = decryptThumbnail(info.key, sizePx, sealed);
    } catch {
      return text(502, "Bad Gateway", "thumbnail failed to decrypt");
    }
    return {
      status: 200,
      statusText: "OK",
      headers: {
        "Content-Type": "image/jpeg",
        "Content-Length": String(jpeg.length),
        "Cache-Control": "private, max-age=31536000, immutable",
      },
      body: jpeg,
    };
  }

  const total = info.sizeBytes;
  const range = parseRange(rangeHeader, total);
  if (rangeHeader && !range && total > 0) {
    return {
      status: 416,
      statusText: "Range Not Satisfiable",
      headers: { "Content-Range": `bytes */${total}` },
      body: new Uint8Array(0),
    };
  }
  const baseHeaders: Record<string, string> = {
    "Content-Type": info.mime,
    "Accept-Ranges": "bytes",
  };
  if (total === 0) {
    return { status: 200, statusText: "OK", headers: { ...baseHeaders, "Content-Length": "0" }, body: new Uint8Array(0) };
  }
  const plan = planRange(info, range);
  const headers: Record<string, string> = { ...auth };
  if (plan.upstreamRange) headers.Range = plan.upstreamRange;
  const resp = await net.fetch(upstream, { method: "GET", headers });
  if (resp.status !== 200 && resp.status !== 206) {
    return text(resp.status, resp.statusText, `upstream ${resp.status}`);
  }
  let sealed = Buffer.from(await resp.arrayBuffer());
  const sealedChunk = info.chunkBytes + TAG_BYTES;
  const ctTotal = ciphertextSize(total, info.chunkBytes);
  if (resp.status === 200) {
    // Upstream ignored the Range: slice the covering chunks ourselves.
    sealed = sealed.subarray(plan.firstChunk * sealedChunk, Math.min(ctTotal, (plan.lastChunk + 1) * sealedChunk));
  }
  const parts: Buffer[] = [];
  let off = 0;
  try {
    for (let i = plan.firstChunk; i <= plan.lastChunk; i += 1) {
      const len = Math.min(sealedChunk, ctTotal - i * sealedChunk);
      if (len <= TAG_BYTES || off + len > sealed.length) throw new Error("short upstream read");
      parts.push(decryptChunk(info.key, i, sealed.subarray(off, off + len)));
      off += len;
    }
  } catch {
    return text(502, "Bad Gateway", "attachment failed to decrypt");
  }
  const plain = Buffer.concat(parts).subarray(plan.trimStart, plan.trimEnd);
  if (range) {
    return {
      status: 206,
      statusText: "Partial Content",
      headers: {
        ...baseHeaders,
        "Content-Length": String(plain.length),
        "Content-Range": `bytes ${plan.plainStart}-${plan.plainEnd}/${total}`,
      },
      body: plain,
    };
  }
  return {
    status: 200,
    statusText: "OK",
    headers: {
      ...baseHeaders,
      "Content-Length": String(plain.length),
      "Cache-Control": "private, max-age=31536000, immutable",
    },
    body: plain,
  };
}
