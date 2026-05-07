import { protocol, net } from "electron";
import * as path from "path";
import * as fs from "fs";
import { app } from "electron";
import { getAttachmentTarget } from "./attachmentRegistry";

const SCHEME = "decibell-asset";
const ATTACHMENT_SCHEME = "decibell-attachment";

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
      // eslint-disable-next-line no-console
      console.log(`[attachment] GET ${upstream}`);
      const resp = await net.fetch(upstream, {
        method: "GET",
        headers: { Authorization: `Bearer ${target.jwt}` },
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
