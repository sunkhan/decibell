// Loopback HTTP proxy for `<video>` / `<audio>` attachment URLs.
//
// Why this exists: we used to feed media elements the same
// `decibell-attachment://` custom protocol that images use. That works
// on Linux/Mac. On Windows with `MediaFoundationClearPlayback`
// enabled (required for WebCodecs API surface in Castlabs Electron 33),
// Chromium routes media-element decode through the Media Foundation
// renderer service, which can't handle custom URL schemes and fails
// with `PIPELINE_ERROR_INITIALIZATION_FAILED: MediaFoundationRendererClient
// disconnected`. The MF renderer demands a real HTTP(S) URL it can
// range-fetch on its own.
//
// Bind to 127.0.0.1 on an OS-assigned port (we don't need a stable
// port — the renderer learns the chosen port via additionalArguments
// at window-create time, see index.ts) and proxy /attachments/<sid>/<aid>
// → community server with bearer auth + range/conditional headers.
// Mirrors registerAttachmentProtocol logic in protocol.ts — same
// authentication injection, same Range header forwarding — only the
// transport differs.

import * as http from "node:http";
import { net } from "electron";
import { getAttachmentTarget } from "./attachmentRegistry";

let server: http.Server | null = null;
let port = 0;

export async function startMediaServer(): Promise<number> {
  return new Promise((resolve, reject) => {
    server = http.createServer(handleRequest);
    server.on("error", reject);
    server.listen(0, "127.0.0.1", () => {
      const addr = server!.address();
      if (typeof addr === "object" && addr) {
        port = addr.port;
      }
      // eslint-disable-next-line no-console
      console.log(`[mediaServer] listening on 127.0.0.1:${port}`);
      resolve(port);
    });
  });
}

export function getMediaServerPort(): number {
  return port;
}

export function stopMediaServer(): void {
  if (server) {
    server.close();
    server = null;
    port = 0;
  }
}

async function handleRequest(
  req: http.IncomingMessage,
  res: http.ServerResponse,
): Promise<void> {
  try {
    const url = new URL(req.url ?? "/", `http://127.0.0.1:${port}`);
    const parts = url.pathname.split("/").filter((p) => p.length > 0);
    // Expected: /attachments/<serverId>/<attachmentId>
    if (parts.length < 3 || parts[0] !== "attachments") {
      res.writeHead(404, { "Content-Type": "text/plain" });
      res.end("not found");
      return;
    }
    const serverId = decodeURIComponent(parts[1]);
    const attachmentId = parts[2];
    const target = getAttachmentTarget(serverId);
    if (!target) {
      res.writeHead(404, { "Content-Type": "text/plain" });
      res.end("not connected");
      return;
    }
    const upstream =
      `https://${target.host}:${target.port}/attachments/${attachmentId}${url.search}`;
    const upstreamHeaders: Record<string, string> = {
      Authorization: `Bearer ${target.jwt}`,
    };
    // Same passthrough set as the custom-protocol handler — required
    // for Chromium's range-based seek/probe to work end-to-end.
    const passthrough = ["range", "if-range", "if-none-match", "if-modified-since"];
    for (const name of passthrough) {
      const v = req.headers[name];
      if (typeof v === "string") upstreamHeaders[name] = v;
    }
    // eslint-disable-next-line no-console
    console.log(
      `[mediaServer] GET ${upstream}${upstreamHeaders.range ? ` ${upstreamHeaders.range}` : ""}`,
    );
    const upstreamResp = await net.fetch(upstream, {
      method: "GET",
      headers: upstreamHeaders,
    });
    // eslint-disable-next-line no-console
    console.log(
      `[mediaServer] → ${upstreamResp.status} ${upstreamResp.statusText}`,
    );
    // Mirror status + headers. Node's writeHead expects a plain object.
    const respHeaders: Record<string, string> = {};
    upstreamResp.headers.forEach((value, name) => {
      // Strip hop-by-hop headers Node will set itself.
      if (name === "connection" || name === "transfer-encoding") return;
      respHeaders[name] = value;
    });
    res.writeHead(upstreamResp.status, upstreamResp.statusText, respHeaders);
    if (upstreamResp.body) {
      const reader = upstreamResp.body.getReader();
      // Stream the body chunk-by-chunk so a 4 GB video doesn't get
      // buffered into RAM. Chromium's MF renderer reads with its own
      // range-fetch loop anyway, so each request is typically only
      // a few MB.
      // eslint-disable-next-line no-constant-condition
      while (true) {
        const { done, value } = await reader.read();
        if (done) break;
        if (value) {
          if (!res.write(Buffer.from(value))) {
            // Backpressure — wait for drain.
            await new Promise<void>((r) => res.once("drain", () => r()));
          }
        }
      }
    }
    res.end();
  } catch (e) {
    // eslint-disable-next-line no-console
    console.error(`[mediaServer] error: ${(e as Error).message}`);
    if (!res.headersSent) {
      res.writeHead(500, { "Content-Type": "text/plain" });
    }
    res.end(`error: ${(e as Error).message}`);
  }
}
