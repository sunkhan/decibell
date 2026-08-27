import { app, ipcMain } from "electron";
import * as fs from "node:fs";
import * as path from "node:path";

// GIF search for the picker's GIFs tab. Two providers — the two Discord
// moved to when Google shut the Tenor API down on 2026-06-30:
//
//   klipy  — https://api.klipy.com/api/v1/{key}/gifs/{search|trending}
//            page-numbered; items carry file.{hd,md,sm,xs}.gif
//   giphy  — https://api.giphy.com/v1/gifs/{search|trending}
//            offset-paged; items carry images.{original,fixed_width,…}
//
// Lives in main so the key never reaches the renderer and CORS is moot.
// Provisioning mirrors the Sentry DSN: CI writes resources/gifs.json
// ({"provider":"klipy","key":"…"}) from the GIF_API_KEY secret (and the
// GIF_API_PROVIDER variable) before packaging; a dev checkout reads
// electron-client/resources/gifs.json or the GIF_API_KEY /
// GIF_API_PROVIDER env vars. No key → the tab explains itself.
//
// Both vendors hand out a test key on signup (100 calls/hour) and lift
// the cap on request: partner.klipy.com/api-keys (KLIPY, free) and
// developers.giphy.com/dashboard (GIPHY, production keys are reviewed).
//
// Sending a GIF is sending its https URL as the message text: every
// client (old ones included) sees a link, and this client's link
// preview turns it into the animated image.

export type GifProvider = "klipy" | "giphy";

export interface GifResult {
  id: string;
  title: string;
  /// Full-size GIF — what gets sent.
  url: string;
  width: number;
  height: number;
  /// Smaller GIF for the picker grid.
  preview: string;
  previewWidth: number;
  previewHeight: number;
}

export interface GifPage {
  results: GifResult[];
  /// Opaque cursor for the next page (a page number or offset, per
  /// provider); null when exhausted.
  next: string | null;
}

export type GifSearchResult =
  | { ok: true; page: GifPage }
  | { ok: false; error: string };

const PAGE_SIZE = 24;
const QUERY_MAX = 200;
const TIMEOUT_MS = 10_000;
/// KLIPY content_filter (off | low | medium | high) and the GIPHY
/// rating (g | pg | pg-13 | r). `low` only drops explicit adult content
/// — the owner's call: the widest catalogue short of unfiltered (`off`
/// / no rating). Keep the two in step if changing.
const KLIPY_CONTENT_FILTER = "low";
const GIPHY_RATING = "r";

// ── Config ───────────────────────────────────────────────────────────

interface GifConfig {
  provider: GifProvider;
  key: string;
}

let cachedConfig: GifConfig | null | undefined;

function asProvider(v: unknown): GifProvider | null {
  return v === "klipy" || v === "giphy" ? v : null;
}

function loadConfig(): GifConfig | null {
  if (cachedConfig !== undefined) return cachedConfig;
  const envKey = process.env.GIF_API_KEY?.trim();
  if (envKey) {
    cachedConfig = {
      provider: asProvider(process.env.GIF_API_PROVIDER?.trim().toLowerCase()) ?? "klipy",
      key: envKey,
    };
    return cachedConfig;
  }
  // resourcesPath is Electron-only (undefined under plain node, which
  // the standalone probe script runs in).
  const candidates = [
    ...(process.resourcesPath ? [path.join(process.resourcesPath, "gifs.json")] : []),
    path.join(app.getAppPath(), "resources", "gifs.json"),
  ];
  for (const p of candidates) {
    try {
      let raw = fs.readFileSync(p, "utf8");
      if (raw.charCodeAt(0) === 0xfeff) raw = raw.slice(1);
      const j = JSON.parse(raw) as { provider?: unknown; key?: unknown };
      const provider = asProvider(typeof j.provider === "string" ? j.provider.toLowerCase() : null);
      if (provider && typeof j.key === "string" && j.key.trim()) {
        cachedConfig = { provider, key: j.key.trim() };
        return cachedConfig;
      }
      console.warn(`[gifs] ${p}: expected {"provider":"klipy"|"giphy","key":"…"}`);
    } catch {
      /* absent or unreadable → try the next location */
    }
  }
  cachedConfig = null;
  return null;
}

// ── IPC ──────────────────────────────────────────────────────────────

export function registerGifHandlers(): void {
  ipcMain.handle("decibell:gifs:status", () => {
    const c = loadConfig();
    return { configured: c !== null, provider: c?.provider ?? null };
  });
  ipcMain.handle(
    "decibell:gifs:search",
    (_e, query: unknown, pos: unknown, locale: unknown): Promise<GifSearchResult> =>
      search(
        typeof query === "string" ? query.trim().slice(0, QUERY_MAX) : "",
        typeof pos === "string" && pos ? pos : null,
        typeof locale === "string" ? locale : null,
      ),
  );
}

async function search(
  q: string,
  pos: string | null,
  locale: string | null,
): Promise<GifSearchResult> {
  const c = loadConfig();
  if (!c) return { ok: false, error: "not-configured" };
  try {
    return c.provider === "klipy"
      ? await searchKlipy(c.key, q, pos, locale)
      : await searchGiphy(c.key, q, pos, locale);
  } catch (e) {
    return { ok: false, error: (e as Error).message };
  }
}

// ── Shared helpers ───────────────────────────────────────────────────

async function getJson(u: URL): Promise<{ status: number; json: unknown }> {
  const res = await fetch(u, {
    headers: { Accept: "application/json" },
    signal: AbortSignal.timeout(TIMEOUT_MS),
  });
  const text = await res.text();
  let json: unknown = null;
  try {
    json = JSON.parse(text);
  } catch {
    /* non-JSON body → status-only error below */
  }
  return { status: res.status, json };
}

function httpsUrl(v: unknown): string | null {
  return typeof v === "string" && /^https:\/\//.test(v) ? v : null;
}

function num(v: unknown): number {
  const n = typeof v === "number" ? v : typeof v === "string" ? parseInt(v, 10) : NaN;
  return Number.isFinite(n) && n > 0 ? n : 0;
}

/// A page-number / offset cursor: positive integer or null.
function cursorInt(pos: string | null, fallback: number): number {
  const n = pos ? parseInt(pos, 10) : NaN;
  return Number.isInteger(n) && n >= 0 ? n : fallback;
}

/// "en-US" → language "en", region "us".
function localeParts(locale: string | null): { lang: string | null; region: string | null } {
  const m = locale ? /^([A-Za-z]{2,3})(?:[-_]([A-Za-z]{2}))?/.exec(locale) : null;
  return {
    lang: m ? m[1].toLowerCase() : null,
    region: m?.[2] ? m[2].toLowerCase() : null,
  };
}

// ── KLIPY ────────────────────────────────────────────────────────────

interface KlipyFormat {
  url?: unknown;
  width?: unknown;
  height?: unknown;
}
interface KlipySize {
  gif?: KlipyFormat;
}
interface KlipyItem {
  id?: unknown;
  title?: unknown;
  slug?: unknown;
  file?: { hd?: KlipySize; md?: KlipySize; sm?: KlipySize; xs?: KlipySize };
}
interface KlipyResponse {
  result?: unknown;
  errors?: { message?: unknown };
  data?: { data?: unknown; has_next?: unknown; current_page?: unknown };
}

function klipyGif(f: KlipyFormat | undefined): { url: string; width: number; height: number } | null {
  const url = f ? httpsUrl(f.url) : null;
  return url ? { url, width: num(f!.width), height: num(f!.height) } : null;
}

function normalizeKlipy(item: KlipyItem): GifResult | null {
  const file = item.file;
  const full = klipyGif(file?.hd?.gif) ?? klipyGif(file?.md?.gif);
  const preview = klipyGif(file?.sm?.gif) ?? klipyGif(file?.xs?.gif) ?? full;
  const id =
    typeof item.id === "string" || typeof item.id === "number"
      ? String(item.id)
      : typeof item.slug === "string"
        ? item.slug
        : null;
  if (!id || !full || !preview) return null;
  return {
    id,
    title: typeof item.title === "string" ? item.title : "",
    url: full.url,
    width: full.width,
    height: full.height,
    preview: preview.url,
    previewWidth: preview.width,
    previewHeight: preview.height,
  };
}

async function searchKlipy(
  key: string,
  q: string,
  pos: string | null,
  locale: string | null,
): Promise<GifSearchResult> {
  const page = Math.max(1, cursorInt(pos, 1));
  const u = new URL(
    `https://api.klipy.com/api/v1/${encodeURIComponent(key)}/gifs/${q ? "search" : "trending"}`,
  );
  u.searchParams.set("page", String(page));
  u.searchParams.set("per_page", String(PAGE_SIZE));
  u.searchParams.set("content_filter", KLIPY_CONTENT_FILTER);
  u.searchParams.set("format_filter", "gif");
  if (q) u.searchParams.set("q", q);
  const { region } = localeParts(locale);
  if (region) u.searchParams.set("locale", region);

  const { status, json } = await getJson(u);
  const j = (json ?? {}) as KlipyResponse;
  if (j.result !== true) {
    const msgs = j.errors?.message;
    const message = Array.isArray(msgs)
      ? msgs.filter((m): m is string => typeof m === "string").join("; ")
      : typeof msgs === "string"
        ? msgs
        : "";
    return { ok: false, error: message || `KLIPY returned HTTP ${status}` };
  }
  const items = Array.isArray(j.data?.data) ? (j.data!.data as KlipyItem[]) : [];
  const results: GifResult[] = [];
  for (const item of items) {
    const r = normalizeKlipy(item);
    if (r) results.push(r);
  }
  return {
    ok: true,
    page: { results, next: j.data?.has_next === true ? String(page + 1) : null },
  };
}

// ── GIPHY ────────────────────────────────────────────────────────────

interface GiphyRendition {
  url?: unknown;
  width?: unknown;
  height?: unknown;
}
interface GiphyItem {
  id?: unknown;
  title?: unknown;
  alt_text?: unknown;
  images?: Record<string, GiphyRendition | undefined>;
}
interface GiphyResponse {
  data?: unknown;
  pagination?: { total_count?: unknown; count?: unknown; offset?: unknown };
  meta?: { status?: unknown; msg?: unknown };
}

function giphyGif(r: GiphyRendition | undefined): { url: string; width: number; height: number } | null {
  const url = r ? httpsUrl(r.url) : null;
  return url ? { url, width: num(r!.width), height: num(r!.height) } : null;
}

function normalizeGiphy(item: GiphyItem): GifResult | null {
  const im = item.images ?? {};
  const full = giphyGif(im.original) ?? giphyGif(im.downsized);
  const preview =
    giphyGif(im.fixed_width) ?? giphyGif(im.downsized) ?? giphyGif(im.preview_gif) ?? full;
  const id = typeof item.id === "string" ? item.id : null;
  if (!id || !full || !preview) return null;
  const title =
    (typeof item.title === "string" && item.title) ||
    (typeof item.alt_text === "string" && item.alt_text) ||
    "";
  return {
    id,
    title,
    url: full.url,
    width: full.width,
    height: full.height,
    preview: preview.url,
    previewWidth: preview.width,
    previewHeight: preview.height,
  };
}

async function searchGiphy(
  key: string,
  q: string,
  pos: string | null,
  locale: string | null,
): Promise<GifSearchResult> {
  const offset = cursorInt(pos, 0);
  const u = new URL(`https://api.giphy.com/v1/gifs/${q ? "search" : "trending"}`);
  u.searchParams.set("api_key", key);
  u.searchParams.set("limit", String(PAGE_SIZE));
  u.searchParams.set("offset", String(offset));
  u.searchParams.set("rating", GIPHY_RATING);
  if (q) u.searchParams.set("q", q.slice(0, 50)); // GIPHY caps q at 50 chars
  const { lang } = localeParts(locale);
  if (q && lang) u.searchParams.set("lang", lang);

  const { status, json } = await getJson(u);
  const j = (json ?? {}) as GiphyResponse;
  const metaStatus = num(j.meta?.status) || status;
  if (metaStatus < 200 || metaStatus >= 300 || !Array.isArray(j.data)) {
    const msg = typeof j.meta?.msg === "string" ? j.meta.msg : "";
    return { ok: false, error: msg ? `GIPHY: ${msg}` : `GIPHY returned HTTP ${status}` };
  }
  const results: GifResult[] = [];
  for (const item of j.data as GiphyItem[]) {
    const r = normalizeGiphy(item);
    if (r) results.push(r);
  }
  const total = num(j.pagination?.total_count);
  const nextOffset = offset + (j.data as unknown[]).length;
  const exhausted =
    (j.data as unknown[]).length < PAGE_SIZE || (total > 0 && nextOffset >= total);
  return { ok: true, page: { results, next: exhausted ? null : String(nextOffset) } };
}
