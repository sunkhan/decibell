import { protocol, net } from "electron";
import * as path from "path";
import * as fs from "fs";
import { app } from "electron";
import { getAttachmentTarget } from "./attachmentRegistry";
import { lookupFile } from "./fileRegistry";

const SCHEME = "decibell-asset";
const ATTACHMENT_SCHEME = "decibell-attachment";
const FILE_SCHEME = "decibell-file";

// Replaces tauri-client's local_media_server.rs — instead of running an
// HTTP server on a random localhost port, register a custom protocol
// that streams from the on-disk media cache. Renderer references files
// as `decibell-asset:///<filename>`; protocol.handle resolves to a
// file:// URL inside the cache dir, which Electron's net.fetch streams
// with proper Range support for <video>/<audio> seeking.
export function registerProtocol(): void {
  protocol.handle(SCHEME, async (req) => {
    try {
      const url = new URL(req.url);
      // Strip leading slash from pathname; reject anything that escapes
      // the cache dir via .. or absolute paths.
      const rel = decodeURIComponent(url.pathname.replace(/^\/+/, ""));
      if (rel.includes("..") || path.isAbsolute(rel)) {
        return new Response("forbidden", { status: 403 });
      }
      const abs = path.join(cacheDir(), rel);
      if (!fs.existsSync(abs)) {
        return new Response("not found", { status: 404 });
      }
      return await net.fetch(`file://${abs}`);
    } catch (e) {
      return new Response(`bad request: ${(e as Error).message}`, { status: 400 });
    }
  });
}

export function cacheDir(): string {
  // Mirrors tauri-client/src-tauri/src/local_media_server.rs::cache_dir.
  // Linux: ~/.cache/com.decibell.app, Windows: %LOCALAPPDATA%/com.decibell.app/cache,
  // macOS: ~/Library/Caches/com.decibell.app. app.getPath('userData') is per-app
  // already, so we nest 'media-cache' under it for clarity.
  return path.join(app.getPath("userData"), "media-cache");
}

// Custom schemes need to be registered as privileged BEFORE app.whenReady()
// for them to support fetch, streaming, CSP bypass, etc.
protocol.registerSchemesAsPrivileged([
  {
    scheme: SCHEME,
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      stream: true,
      bypassCSP: false,
    },
  },
  {
    scheme: ATTACHMENT_SCHEME,
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      stream: true,
      bypassCSP: false,
    },
  },
  {
    scheme: FILE_SCHEME,
    privileges: {
      standard: true,
      secure: true,
      supportFetchAPI: true,
      stream: true,
      bypassCSP: false,
      // No `corsEnabled` — leaving it off matches the other schemes
      // and means renderer fetch() doesn't get a CORS preflight that
      // would fail without an explicit Access-Control-Allow-Origin
      // response. The privileged scheme is already trusted.
    },
  },
]);

/// Authenticated proxy to community-server attachment GETs. Renderer
/// references images/audio/video as
///   decibell-attachment://<serverId>/<attachmentId>?variant=thumb&size=640
/// and main rewrites that to
///   https://<host>:<port>/attachments/<attachmentId>?variant=thumb&size=640
/// with the right Bearer token. Browser handles caching + concurrency
/// natively; <img> / <video> / <audio> tags work without renderer-side
/// fetch + Blob URL juggling. The registry is populated from
/// useServerEvents (renderer side) on every community_auth_responded.
export function registerAttachmentProtocol(): void {
  protocol.handle(ATTACHMENT_SCHEME, async (req) => {
    try {
      const url = new URL(req.url);
      // URL shape: decibell-attachment://attach/<serverId>/<attachmentId>?…
      // Fixed pseudo-host "attach" because Chromium parses numeric
      // hostnames as IPv4 ("1" → 0.0.0.1), which broke routing for the
      // central server's numeric server IDs. Everything meaningful
      // lives in the path.
      const parts = url.pathname.split("/").filter((p) => p.length > 0);
      if (parts.length < 2) {
        return new Response("bad request", { status: 400 });
      }
      const serverId = decodeURIComponent(parts[0]);
      const attachmentId = parts[1];
      const target = getAttachmentTarget(serverId);
      if (!target) {
        return new Response("not connected", { status: 404 });
      }
      const upstream = `https://${target.host}:${target.port}/attachments/${attachmentId}${url.search}`;
      // Forward the byte-range probe that Chromium's <video>/<audio>
      // media pipeline relies on. Without this, every probe (moov-atom
      // discovery, scrub seeks, resume from cache) hits upstream as a
      // full GET and the server replies 200 OK with the whole file —
      // which Chromium can't use to seek, so playback hangs on the
      // first frame. The community server already speaks 206 Partial
      // Content (attachment_http.cpp:684); we just need to deliver the
      // header to it. If-Range / If-None-Match come along for the
      // ride so conditional revalidation also works.
      const upstreamHeaders: Record<string, string> = {
        Authorization: `Bearer ${target.jwt}`,
      };
      const passthrough = ["range", "if-range", "if-none-match", "if-modified-since"];
      for (const name of passthrough) {
        const v = req.headers.get(name);
        if (v) upstreamHeaders[name] = v;
      }
      // eslint-disable-next-line no-console
      console.log(
        `[attachment] GET ${upstream}${upstreamHeaders.range ? ` ${upstreamHeaders.range}` : ""}`,
      );
      const resp = await net.fetch(upstream, {
        method: "GET",
        headers: upstreamHeaders,
      });
      // eslint-disable-next-line no-console
      console.log(`[attachment] → ${resp.status} ${resp.statusText}`);
      return resp;
    } catch (e) {
      // eslint-disable-next-line no-console
      console.error(`[attachment] error: ${(e as Error).message}`);
      return new Response(`error: ${(e as Error).message}`, { status: 500 });
    }
  });
}

/// Streaming protocol for files the user has just picked / dropped.
/// URL shape: `decibell-file://file/<token>` where the token is what
/// `register_file` returned for an absolute path. We delegate the
/// actual streaming to `net.fetch('file://abs')` — Electron's net
/// stack handles Range requests, content-length, and chunked streaming
/// natively, which is exactly what the renderer's `<video>` seek and
/// the upload loop's `fetch(..., {Range: ...})` rely on. So a 500MB
/// file never lands in renderer RAM as a single buffer; it flows
/// chunk-by-chunk via Chromium's net loader straight from disk.
export function registerFileProtocol(): void {
  protocol.handle(FILE_SCHEME, async (req) => {
    try {
      const url = new URL(req.url);
      // URL shape: decibell-file://file/<token>
      // "file" is a fixed pseudo-host (same trick the attachment
      // protocol uses) so Chromium doesn't try to interpret the token
      // as a hostname.
      const parts = url.pathname.split("/").filter((p) => p.length > 0);
      if (parts.length !== 1) {
        return new Response("bad request", { status: 400 });
      }
      const token = decodeURIComponent(parts[0]);
      const entry = lookupFile(token);
      if (!entry) {
        return new Response("not found", { status: 404 });
      }
      // Forward Range / If-* headers transparently so video seeking
      // works (`fetch('decibell-file://...').body` becomes a streaming
      // ReadableStream; <video src=> issues range requests on seek).
      const headers: Record<string, string> = {};
      const range = req.headers.get("range");
      if (range) headers["Range"] = range;
      const resp = await net.fetch(`file://${entry.path}`, {
        method: req.method,
        headers,
      });
      // Patch the Content-Type with the mime we recorded at register
      // time. net.fetch on file:// derives a generic application/octet-
      // stream which trips up the <video> / <audio> probe element.
      const out = new Response(resp.body, {
        status: resp.status,
        statusText: resp.statusText,
        headers: resp.headers,
      });
      out.headers.set("Content-Type", entry.mime);
      return out;
    } catch (e) {
      return new Response(`error: ${(e as Error).message}`, { status: 500 });
    }
  });
}
