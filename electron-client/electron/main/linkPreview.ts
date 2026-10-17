import { app, ipcMain, shell } from "electron";
import * as http from "node:http";
import * as https from "node:https";
import { isIP } from "node:net";
import type { Readable } from "node:stream";
import * as zlib from "node:zlib";
import { isPublicAddress, publicOnlyLookup } from "./publicNet";

// Link unfurling for the message list. The renderer can't do this
// itself — its `fetch()` is CORS-bound and the CSP's connect-src is
// deliberately tight — so the bubble asks main for a small metadata
// record and main does the HTTP. Client-side rather than on the
// community server on purpose: no proto/server work, previews work
// against every server version, and the trade (the linked site sees
// the reader's IP, as opening the link would) is surfaced as the
// Privacy-tab toggle that gates the renderer's requests.
//
// Guards, because this is main-process HTTP driven by message text
// written by other people:
//   - http(s) only; no userinfo; redirects followed by hand (≤5 hops)
//     and every hop re-validated;
//   - only public addresses (publicNet.ts: loopback, private, link-local,
//     CGNAT, unique-local, multicast, NAT64 / 6to4 / Teredo and every
//     IPv4-mapped spelling refused). Enforced on the address actually
//     connected to — node's http(s) client resolves through
//     `publicOnlyLookup` — so a rebinding name can't answer one address
//     to a check and another to the connection; IP literals are checked
//     before connecting. Covers the page, its redirects, og:image probes
//     and the oEmbed endpoint alike (all go through fetchBounded);
//   - bounded reads (a page up to its </head> or 3 MiB — YouTube puts
//     its OG tags ~700 KB in, behind an inline script — 64 KiB of an
//     image), an 8 s timeout per request, at most 4 fetches in flight;
//   - results cached (30 min hits / 5 min misses) and de-duplicated
//     while in flight, so N rows citing one URL cost one fetch.

// ── Result shape (mirrored in preload + src/types) ───────────────────

export interface LinkPreviewImage {
  url: string;
  width: number;
  height: number;
}

export type LinkPreview =
  | {
      kind: "site";
      url: string;
      siteName: string | null;
      title: string | null;
      description: string | null;
      image: LinkPreviewImage | null;
      largeImage: boolean;
      color: string | null;
    }
  /// `gif`: an animated-format file (content type or magic bytes) — the
  /// renderer gives those a tighter box than a still.
  | { kind: "image"; url: string; width: number; height: number; gif: boolean };

// ── Limits ───────────────────────────────────────────────────────────

const URL_MAX = 2048;
const HTML_LIMIT = 3 * 1024 * 1024;
const PROBE_LIMIT = 64 * 1024;
const OEMBED_LIMIT = 64 * 1024;
const TIMEOUT_MS = 8000;
const MAX_REDIRECTS = 5;
const MAX_CONCURRENT = 4;
const OK_TTL_MS = 30 * 60_000;
const FAIL_TTL_MS = 5 * 60_000;
const CACHE_MAX = 500;
const TITLE_MAX = 200;
const DESCRIPTION_MAX = 350;
const SITE_NAME_MAX = 80;

// ── IPC ──────────────────────────────────────────────────────────────

export function registerLinkPreviewHandlers(): void {
  ipcMain.handle("decibell:shell:openExternal", async (_e, url: unknown) => {
    if (typeof url !== "string" || url.length > URL_MAX) return;
    let parsed: URL;
    try {
      parsed = new URL(url);
    } catch {
      return;
    }
    if (parsed.protocol !== "http:" && parsed.protocol !== "https:") return;
    await shell.openExternal(parsed.href);
  });

  ipcMain.handle(
    "decibell:linkPreview:fetch",
    (_e, url: unknown): Promise<LinkPreview | null> => {
      if (typeof url !== "string" || url.length > URL_MAX) {
        return Promise.resolve(null);
      }
      return cachedUnfurl(url);
    },
  );
}

// ── Cache + in-flight dedup + concurrency ────────────────────────────

/// Unfurl with the cache / dedup / concurrency wrappers. Exported for
/// callers in main and the standalone probe script.
export const unfurlLink = (url: string): Promise<LinkPreview | null> => cachedUnfurl(url);

const cache = new Map<string, { expires: number; value: LinkPreview | null }>();
const inflight = new Map<string, Promise<LinkPreview | null>>();

function cachedUnfurl(url: string): Promise<LinkPreview | null> {
  const hit = cache.get(url);
  if (hit && hit.expires > Date.now()) return Promise.resolve(hit.value);
  const pending = inflight.get(url);
  if (pending) return pending;
  const p = withSlot(() => unfurl(url))
    .catch((e: unknown) => {
      if (!app.isPackaged) {
        console.log(`[linkPreview] ${url}: ${(e as Error).message}`);
      }
      return null;
    })
    .then((value) => {
      inflight.delete(url);
      remember(url, value);
      return value;
    });
  inflight.set(url, p);
  return p;
}

function remember(url: string, value: LinkPreview | null): void {
  if (cache.size >= CACHE_MAX) {
    let n = CACHE_MAX / 4;
    for (const k of cache.keys()) {
      cache.delete(k);
      if (--n <= 0) break;
    }
  }
  cache.set(url, {
    expires: Date.now() + (value ? OK_TTL_MS : FAIL_TTL_MS),
    value,
  });
}

let active = 0;
const waiting: (() => void)[] = [];

async function withSlot<T>(fn: () => Promise<T>): Promise<T> {
  if (active >= MAX_CONCURRENT) {
    await new Promise<void>((resolve) => waiting.push(resolve));
  }
  active++;
  try {
    return await fn();
  } finally {
    active--;
    waiting.shift()?.();
  }
}

// ── Destination guard ────────────────────────────────────────────────

const BLOCKED_HOST = /(^|\.)(localhost|local|internal|localdomain|home\.arpa)$/i;

/// What can be decided from the URL alone. Names are checked again where
/// it counts, on connect (`publicOnlyLookup`); an IP literal is never
/// resolved, so this is its whole check.
function assertAllowed(u: URL): void {
  if (u.protocol !== "http:" && u.protocol !== "https:") throw new Error("scheme");
  if (u.username || u.password) throw new Error("userinfo");
  const host = u.hostname.replace(/^\[|\]$/g, "").replace(/\.$/, "");
  if (!host || BLOCKED_HOST.test(host)) throw new Error("host");
  if (isIP(host) && !isPublicAddress(host)) throw new Error("private address");
}

// ── Bounded fetch ────────────────────────────────────────────────────

interface Fetched {
  url: string;
  contentType: string;
  bytes: Uint8Array;
}

/// Everything a page preview needs is in <head>; stop reading there.
const HEAD_END = "</head>";

function userAgent(): string {
  return `Mozilla/5.0 (compatible; Decibell/${app.getVersion()}; +https://github.com/sunkhan/decibell)`;
}

// Our own pools, so a socket is only ever reused if it was opened
// through publicOnlyLookup.
const httpAgent = new http.Agent({ keepAlive: true });
const httpsAgent = new https.Agent({ keepAlive: true });

/// One GET, no redirect following; resolves once headers arrive. The
/// whole exchange — connect, headers and body — gets TIMEOUT_MS from
/// here (what fetch's AbortSignal.timeout gave each hop): the deadline
/// tears down the request and the response being read. `done` disarms it.
function get(u: URL, accept: string): Promise<{ res: http.IncomingMessage; done: () => void }> {
  return new Promise((resolve, reject) => {
    const secure = u.protocol === "https:";
    let response: http.IncomingMessage | null = null;
    const timer = setTimeout(() => {
      const e = new Error("timeout");
      response?.destroy(e);
      req.destroy(e);
    }, TIMEOUT_MS);
    const req = (secure ? https : http).request(
      u,
      {
        method: "GET",
        agent: secure ? httpsAgent : httpAgent,
        lookup: publicOnlyLookup,
        headers: {
          "User-Agent": userAgent(),
          Accept: accept,
          "Accept-Language": "en,*;q=0.5",
          "Accept-Encoding": "gzip, deflate, br",
        },
      },
      (res) => {
        response = res;
        resolve({ res, done: () => clearTimeout(timer) });
      },
    );
    req.on("error", (e) => {
      clearTimeout(timer);
      reject(e);
    });
    req.end();
  });
}

/// The body with its Content-Encoding undone (what fetch() did for us);
/// an unknown coding passes through as is.
function decoded(res: http.IncomingMessage): Readable {
  const coding = String(res.headers["content-encoding"] ?? "").trim().toLowerCase();
  const decoder =
    coding === "gzip" || coding === "x-gzip"
      ? zlib.createGunzip()
      : coding === "deflate"
        ? zlib.createInflate()
        : coding === "br"
          ? zlib.createBrotliDecompress()
          : null;
  if (!decoder) return res;
  res.on("error", (e) => decoder.destroy(e));
  decoder.on("close", () => res.destroy());
  return res.pipe(decoder);
}

async function fetchBounded(
  start: string,
  limit: number,
  accept: string,
  stopAt?: string,
): Promise<Fetched | null> {
  let current = new URL(start);
  for (let hop = 0; hop <= MAX_REDIRECTS; hop++) {
    assertAllowed(current);
    const { res, done } = await get(current, accept);
    try {
      const status = res.statusCode ?? 0;
      if (status >= 300 && status < 400) {
        const location = res.headers.location;
        res.destroy();
        if (!location) return null;
        current = new URL(location, current);
        continue;
      }
      if (status < 200 || status > 299) {
        res.destroy();
        return null;
      }
      return {
        url: current.href,
        contentType: String(res.headers["content-type"] ?? "").toLowerCase(),
        bytes: await readUpTo(decoded(res), limit, stopAt),
      };
    } finally {
      done();
    }
  }
  return null;
}

/// Read the body up to `limit` bytes, or through the first chunk that
/// completes `stopAt` (an ASCII marker; the previous chunk's tail is
/// carried so a marker straddling two chunks still matches).
async function readUpTo(body: Readable, limit: number, stopAt?: string): Promise<Uint8Array> {
  const chunks: Uint8Array[] = [];
  let total = 0;
  let carry = "";
  const latin1 = new TextDecoder("latin1");
  try {
    for await (const chunk of body) {
      const value = chunk as Uint8Array;
      if (!value.byteLength) continue;
      chunks.push(value);
      total += value.byteLength;
      if (total >= limit) break;
      if (stopAt) {
        const text = carry + latin1.decode(value).toLowerCase();
        if (text.includes(stopAt)) break;
        carry = text.slice(-(stopAt.length - 1));
      }
    }
  } finally {
    // Stop the download: leaving the loop early (limit / marker) or an
    // error must not keep the socket pulling bytes nobody reads.
    body.destroy();
  }
  const out = new Uint8Array(Math.min(total, limit));
  let off = 0;
  for (const c of chunks) {
    const take = Math.min(c.byteLength, out.length - off);
    if (take <= 0) break;
    out.set(c.subarray(0, take), off);
    off += take;
  }
  return out;
}

// ── HTML metadata ────────────────────────────────────────────────────

function decodeHtml(bytes: Uint8Array, contentType: string): string {
  let label = /charset=["']?\s*([\w.:-]+)/i.exec(contentType)?.[1];
  if (!label) {
    const head = new TextDecoder("latin1").decode(bytes.subarray(0, 2048));
    label = /<meta[^>]+charset=["']?\s*([\w.:-]+)/i.exec(head)?.[1];
  }
  try {
    return new TextDecoder(label ?? "utf-8").decode(bytes);
  } catch {
    return new TextDecoder("utf-8").decode(bytes);
  }
}

const NAMED_ENTITIES: Record<string, string> = {
  amp: "&", lt: "<", gt: ">", quot: '"', apos: "'", nbsp: " ",
  ndash: "–", mdash: "—", hellip: "…", copy: "©",
  reg: "®", trade: "™", laquo: "«", raquo: "»",
  lsquo: "‘", rsquo: "’", ldquo: "“", rdquo: "”",
  bull: "•", middot: "·", euro: "€", pound: "£",
  yen: "¥", deg: "°", times: "×",
};

function decodeEntities(s: string): string {
  return s.replace(/&(#x[0-9a-f]+|#\d+|[a-z][a-z0-9]*);/gi, (m, e: string) => {
    if (e[0] === "#") {
      const hex = e[1] === "x" || e[1] === "X";
      const cp = parseInt(e.slice(hex ? 2 : 1), hex ? 16 : 10);
      if (!Number.isFinite(cp) || cp <= 0 || cp > 0x10ffff) return m;
      try {
        return String.fromCodePoint(cp);
      } catch {
        return m;
      }
    }
    return NAMED_ENTITIES[e.toLowerCase()] ?? m;
  });
}

function clean(s: string, max: number): string {
  const t = s.replace(/\s+/g, " ").trim();
  return t.length > max ? `${t.slice(0, max - 1).trimEnd()}…` : t;
}

const ATTR_RE = /([^\s"'<>/=]+)(?:\s*=\s*(?:"([^"]*)"|'([^']*)'|([^\s"'<>`]+)))?/g;

function parseAttrs(s: string): Map<string, string> {
  const out = new Map<string, string>();
  for (const a of s.matchAll(ATTR_RE)) {
    const k = a[1].toLowerCase();
    if (!out.has(k)) out.set(k, a[2] ?? a[3] ?? a[4] ?? "");
  }
  return out;
}

interface PageMeta {
  meta: Map<string, string>;
  title: string | null;
  oembed: string | null;
}

function parsePage(html: string): PageMeta {
  const meta = new Map<string, string>();
  for (const m of html.matchAll(/<meta\b([^>]*)>/gi)) {
    const attrs = parseAttrs(m[1]);
    const key = (attrs.get("property") ?? attrs.get("name") ?? attrs.get("itemprop"))
      ?.trim()
      .toLowerCase();
    const content = attrs.get("content");
    if (!key || content === undefined || meta.has(key)) continue;
    meta.set(key, decodeEntities(content));
  }
  const t = /<title\b[^>]*>([\s\S]*?)<\/title>/i.exec(html);
  const title = t ? clean(decodeEntities(t[1]), TITLE_MAX) || null : null;
  let oembed: string | null = null;
  for (const m of html.matchAll(/<link\b([^>]*)>/gi)) {
    const attrs = parseAttrs(m[1]);
    const href = attrs.get("href");
    if ((attrs.get("type") ?? "").toLowerCase() === "application/json+oembed" && href) {
      oembed = decodeEntities(href);
      break;
    }
  }
  return { meta, title, oembed };
}

function pick(meta: Map<string, string>, ...keys: string[]): string | null {
  for (const k of keys) {
    const v = meta.get(k)?.trim();
    if (v) return v;
  }
  return null;
}

function toInt(s: string | null | undefined): number {
  const n = s ? parseInt(s, 10) : NaN;
  return Number.isFinite(n) && n > 0 && n < 100_000 ? n : 0;
}

/// Resolve a page-relative image URL to something the renderer can
/// load: its CSP allows remote images over https only, so a plain
/// http reference is upgraded (a broken upgrade just hides the image).
function resolveImage(raw: string | null, base: string): string | null {
  if (!raw) return null;
  try {
    const u = new URL(raw.trim(), base);
    if (u.protocol === "http:") u.protocol = "https:";
    if (u.protocol !== "https:") return null;
    if (u.username || u.password) return null;
    return u.href.length <= URL_MAX ? u.href : null;
  } catch {
    return null;
  }
}

const HEX_COLOR = /^#(?:[0-9a-f]{3}|[0-9a-f]{4}|[0-9a-f]{6}|[0-9a-f]{8})$/i;

function validColor(raw: string | null): string | null {
  const v = raw?.trim() ?? "";
  return HEX_COLOR.test(v) ? v.toLowerCase() : null;
}

// ── Image dimension probe ────────────────────────────────────────────
// Header-only parsing of PNG / GIF / WebP / JPEG so a preview can
// reserve its box before the bytes arrive (the same no-pop rule the
// attachment list follows). JPEG needs the SOF marker, which sits past
// the probe window when an image carries a large EXIF blob — those
// come back unknown and the row grows on load.

function probeImageDims(b: Uint8Array): { width: number; height: number } | null {
  const dv = new DataView(b.buffer, b.byteOffset, b.byteLength);
  const ascii = (from: number, to: number) =>
    String.fromCharCode(...b.subarray(from, to));
  if (b.length >= 24 && b[0] === 0x89 && ascii(1, 4) === "PNG") {
    return { width: dv.getUint32(16), height: dv.getUint32(20) };
  }
  if (b.length >= 10 && ascii(0, 3) === "GIF") {
    return { width: dv.getUint16(6, true), height: dv.getUint16(8, true) };
  }
  if (b.length >= 30 && ascii(0, 4) === "RIFF" && ascii(8, 12) === "WEBP") {
    const chunk = ascii(12, 16);
    if (chunk === "VP8X") {
      return {
        width: 1 + (b[24] | (b[25] << 8) | (b[26] << 16)),
        height: 1 + (b[27] | (b[28] << 8) | (b[29] << 16)),
      };
    }
    if (chunk === "VP8L" && b[20] === 0x2f) {
      const bits = b[21] | (b[22] << 8) | (b[23] << 16) | (b[24] << 24);
      return { width: (bits & 0x3fff) + 1, height: ((bits >>> 14) & 0x3fff) + 1 };
    }
    if (chunk === "VP8 " && b[23] === 0x9d && b[24] === 0x01 && b[25] === 0x2a) {
      return {
        width: dv.getUint16(26, true) & 0x3fff,
        height: dv.getUint16(28, true) & 0x3fff,
      };
    }
    return null;
  }
  if (b.length >= 4 && b[0] === 0xff && b[1] === 0xd8) {
    let i = 2;
    while (i + 9 < b.length) {
      if (b[i] !== 0xff) {
        i++;
        continue;
      }
      const marker = b[i + 1];
      if (marker === 0xff) {
        i++;
        continue;
      }
      // Standalone markers carry no length.
      if (marker === 0xd8 || marker === 0x01 || (marker >= 0xd0 && marker <= 0xd7)) {
        i += 2;
        continue;
      }
      const len = dv.getUint16(i + 2);
      const isSof =
        marker >= 0xc0 && marker <= 0xcf && marker !== 0xc4 && marker !== 0xc8 && marker !== 0xcc;
      if (isSof) return { height: dv.getUint16(i + 5), width: dv.getUint16(i + 7) };
      if (marker === 0xda) break;
      i += 2 + len;
    }
  }
  return null;
}

async function probeImage(url: string): Promise<{ width: number; height: number }> {
  try {
    const got = await fetchBounded(url, PROBE_LIMIT, "image/*");
    if (!got || !got.contentType.startsWith("image/")) return { width: 0, height: 0 };
    return probeImageDims(got.bytes) ?? { width: 0, height: 0 };
  } catch {
    return { width: 0, height: 0 };
  }
}

// ── Unfurl ───────────────────────────────────────────────────────────

async function unfurl(url: string): Promise<LinkPreview | null> {
  const got = await fetchBounded(
    url,
    HTML_LIMIT,
    "text/html,application/xhtml+xml;q=0.9,*/*;q=0.5",
    HEAD_END,
  );
  if (!got) return null;

  if (got.contentType.startsWith("image/")) {
    const imageUrl = resolveImage(got.url, got.url);
    if (!imageUrl) return null;
    const dims = probeImageDims(got.bytes) ?? { width: 0, height: 0 };
    const gif =
      got.contentType.startsWith("image/gif") ||
      String.fromCharCode(...got.bytes.subarray(0, 3)) === "GIF";
    return { kind: "image", url: imageUrl, ...dims, gif };
  }

  if (!/^(text\/html|application\/xhtml\+xml)\b/.test(got.contentType)) return null;

  const page = parsePage(decodeHtml(got.bytes, got.contentType));
  const { meta } = page;
  const finalUrl = got.url;

  const ogTitle = pick(meta, "og:title", "twitter:title");
  let title = ogTitle ?? page.title;
  let description = pick(meta, "og:description", "twitter:description", "description");
  let siteName = pick(meta, "og:site_name");
  let imageUrl = resolveImage(
    pick(meta, "og:image:secure_url", "og:image", "og:image:url", "twitter:image", "twitter:image:src"),
    finalUrl,
  );
  let imageW = toInt(meta.get("og:image:width"));
  let imageH = toInt(meta.get("og:image:height"));
  const card = (pick(meta, "twitter:card") ?? "").toLowerCase();
  const ogType = (pick(meta, "og:type") ?? "").toLowerCase();
  let largeImage =
    card === "summary_large_image" ||
    card === "player" ||
    ogType.startsWith("video") ||
    meta.has("og:video") ||
    meta.has("og:video:url");

  // oEmbed fills what the page's own tags left out — YouTube, Vimeo,
  // SoundCloud and friends all advertise a discovery link.
  if (page.oembed && (!ogTitle || !imageUrl)) {
    try {
      const o = await fetchBounded(new URL(page.oembed, finalUrl).href, OEMBED_LIMIT, "application/json");
      if (o && /json/.test(o.contentType)) {
        const j = JSON.parse(new TextDecoder("utf-8").decode(o.bytes)) as Record<string, unknown>;
        const str = (k: string) => (typeof j[k] === "string" ? (j[k] as string) : null);
        if (!ogTitle && str("title")) title = str("title");
        if (!siteName) siteName = str("provider_name");
        if (!description && str("author_name")) description = `by ${str("author_name")}`;
        if (!imageUrl) {
          imageUrl = resolveImage(str("thumbnail_url"), finalUrl);
          imageW = toInt(typeof j.thumbnail_width === "number" ? String(j.thumbnail_width) : null);
          imageH = toInt(typeof j.thumbnail_height === "number" ? String(j.thumbnail_height) : null);
        }
        if (j.type === "video" || j.type === "rich") largeImage = true;
      }
    } catch {
      /* oEmbed is a bonus; the page's own tags stand. */
    }
  }

  if (imageUrl && (imageW === 0 || imageH === 0)) {
    ({ width: imageW, height: imageH } = await probeImage(imageUrl));
  }

  if (!siteName) {
    try {
      siteName = new URL(finalUrl).hostname.replace(/^www\./, "");
    } catch {
      siteName = null;
    }
  }

  title = title ? clean(title, TITLE_MAX) : null;
  description = description ? clean(description, DESCRIPTION_MAX) : null;
  siteName = siteName ? clean(siteName, SITE_NAME_MAX) : null;
  if (!title && !description && !imageUrl) return null;

  return {
    kind: "site",
    url: finalUrl,
    siteName,
    title,
    description,
    image: imageUrl ? { url: imageUrl, width: imageW, height: imageH } : null,
    largeImage,
    color: validColor(pick(meta, "theme-color")),
  };
}
