import { app, ipcMain } from "electron";
import * as fs from "node:fs";
import * as path from "node:path";

// GIF search for the picker's GIFs tab — Tenor v2, the same provider
// Discord uses. Lives in main so the API key never reaches the
// renderer and the request isn't subject to CORS (Tenor's error
// responses carry no ACAO header).
//
// Key provisioning mirrors the Sentry DSN: CI writes
// resources/tenor.json ({"key": "..."}) from the TENOR_API_KEY secret
// before packaging; a dev checkout reads electron-client/resources/
// tenor.json or the TENOR_API_KEY env var. No key → the tab explains
// itself instead of searching. A Tenor key is a free Google Cloud API
// key with the "Tenor API" enabled.
//
// Sending a GIF is sending its https://media.tenor.com/….gif URL as
// the message text: every client (old ones included) sees a link, and
// this client's link preview turns it into the animated image.

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
  /// Tenor's opaque cursor for the next page; null when exhausted.
  next: string | null;
}

export type GifSearchResult =
  | { ok: true; page: GifPage }
  | { ok: false; error: string };

const TENOR = "https://tenor.googleapis.com/v2";
const CLIENT_KEY = "decibell";
const PAGE_LIMIT = 30;
const QUERY_MAX = 200;
const TIMEOUT_MS = 10_000;
/// Tenor's `contentfilter`: off | low | medium | high.
const CONTENT_FILTER = "medium";

let cachedKey: string | null | undefined;

function loadKey(): string | null {
  if (cachedKey !== undefined) return cachedKey;
  const env = process.env.TENOR_API_KEY?.trim();
  if (env) {
    cachedKey = env;
    return env;
  }
  // resourcesPath is Electron-only (undefined under plain node, which
  // the standalone probe script runs in).
  const candidates = [
    ...(process.resourcesPath ? [path.join(process.resourcesPath, "tenor.json")] : []),
    path.join(app.getAppPath(), "resources", "tenor.json"),
  ];
  for (const p of candidates) {
    try {
      let raw = fs.readFileSync(p, "utf8");
      if (raw.charCodeAt(0) === 0xfeff) raw = raw.slice(1);
      const j = JSON.parse(raw) as { key?: unknown };
      if (typeof j.key === "string" && j.key.trim()) {
        cachedKey = j.key.trim();
        return cachedKey;
      }
    } catch {
      /* absent or unreadable → try the next location */
    }
  }
  cachedKey = null;
  return null;
}

export function registerGifHandlers(): void {
  ipcMain.handle("decibell:gifs:status", () => ({ configured: loadKey() !== null }));
  ipcMain.handle(
    "decibell:gifs:search",
    (_e, query: unknown, pos: unknown, locale: unknown): Promise<GifSearchResult> =>
      search(
        typeof query === "string" ? query : "",
        typeof pos === "string" && pos ? pos : null,
        typeof locale === "string" ? locale : null,
      ),
  );
}

interface TenorMedia {
  url?: unknown;
  dims?: unknown;
}
interface TenorItem {
  id?: unknown;
  title?: unknown;
  content_description?: unknown;
  media_formats?: Record<string, TenorMedia>;
}
interface TenorResponse {
  results?: TenorItem[];
  next?: unknown;
}

function dims(m: TenorMedia): [number, number] {
  const d = Array.isArray(m.dims) ? m.dims : [];
  const w = typeof d[0] === "number" && d[0] > 0 ? d[0] : 0;
  const h = typeof d[1] === "number" && d[1] > 0 ? d[1] : 0;
  return [w, h];
}

function httpsUrl(v: unknown): string | null {
  return typeof v === "string" && /^https:\/\//.test(v) ? v : null;
}

function normalize(item: TenorItem): GifResult | null {
  const gif = item.media_formats?.gif;
  const tiny = item.media_formats?.tinygif ?? gif;
  const id = typeof item.id === "string" ? item.id : null;
  const url = gif ? httpsUrl(gif.url) : null;
  const preview = tiny ? httpsUrl(tiny.url) : null;
  if (!id || !gif || !tiny || !url || !preview) return null;
  const [width, height] = dims(gif);
  const [previewWidth, previewHeight] = dims(tiny);
  const title =
    (typeof item.content_description === "string" && item.content_description) ||
    (typeof item.title === "string" && item.title) ||
    "";
  return { id, title, url, width, height, preview, previewWidth, previewHeight };
}

/// Tenor wants `en_US`; the renderer hands over navigator.language.
function tenorLocale(locale: string | null): string | null {
  if (!locale) return null;
  const m = /^([a-z]{2,3})(?:-([A-Za-z]{2}))?/.exec(locale);
  if (!m) return null;
  return m[2] ? `${m[1]}_${m[2].toUpperCase()}` : m[1];
}

async function search(
  query: string,
  pos: string | null,
  locale: string | null,
): Promise<GifSearchResult> {
  const key = loadKey();
  if (!key) return { ok: false, error: "not-configured" };
  const q = query.trim().slice(0, QUERY_MAX);
  const u = new URL(q ? `${TENOR}/search` : `${TENOR}/featured`);
  u.searchParams.set("key", key);
  u.searchParams.set("client_key", CLIENT_KEY);
  u.searchParams.set("limit", String(PAGE_LIMIT));
  u.searchParams.set("media_filter", "gif,tinygif");
  u.searchParams.set("contentfilter", CONTENT_FILTER);
  if (q) u.searchParams.set("q", q);
  if (pos) u.searchParams.set("pos", pos.slice(0, 64));
  const loc = tenorLocale(locale);
  if (loc) u.searchParams.set("locale", loc);

  try {
    const res = await fetch(u, {
      headers: { Accept: "application/json" },
      signal: AbortSignal.timeout(TIMEOUT_MS),
    });
    const text = await res.text();
    if (!res.ok) {
      let message = `Tenor returned HTTP ${res.status}`;
      try {
        const j = JSON.parse(text) as { error?: { message?: unknown } };
        if (typeof j.error?.message === "string") message = j.error.message;
      } catch {
        /* keep the status line */
      }
      return { ok: false, error: message };
    }
    const j = JSON.parse(text) as TenorResponse;
    const results: GifResult[] = [];
    for (const item of j.results ?? []) {
      const r = normalize(item);
      if (r) results.push(r);
    }
    return {
      ok: true,
      page: { results, next: typeof j.next === "string" && j.next ? j.next : null },
    };
  } catch (e) {
    return { ok: false, error: (e as Error).message };
  }
}
