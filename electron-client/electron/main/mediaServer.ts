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
// port) and proxy /<token>/attachments/<sid>/<aid> → community server
// with bearer auth + range/conditional headers. Mirrors
// registerAttachmentProtocol logic in protocol.ts — same authentication
// injection, same Range header forwarding — only the transport differs.
//
// Locked down, because this port attaches the user's JWT and serves
// decrypted E2EE attachments to whoever asks (review S4):
//   - every path starts with a per-launch random 128-bit token; the
//     renderer gets it (with the port) from preload's synchronous
//     `decibell:mediaServer:access` IPC — not argv, which every local
//     user can read in /proc/<pid>/cmdline or `ps`;
//   - the Host header must be exactly 127.0.0.1:<port>, so a web page
//     that DNS-rebinds its own name onto 127.0.0.1 is refused even if
//     it somehow had the token;
//   - attachment ids are digits only; GET only.

import * as http from "node:http";
import { randomBytes, timingSafeEqual } from "node:crypto";
import { app, ipcMain, net } from "electron";
import { getAttachmentTarget } from "./attachmentRegistry";
import { fetchDecryptedAttachment } from "./attachmentFetch";
import { pacedBody } from "./downloadPacer";
import { NO_STORE } from "./noStore";

let server: http.Server | null = null;
let port = 0;
/// Per-launch secret every request path must start with (hex, 128 bits).
const token = randomBytes(16).toString("hex");
const tokenBytes = Buffer.from(token, "utf8");

const ATTACHMENT_ID = /^\d{1,20}$/;

/// What the renderer needs to build media URLs. Asked for synchronously
/// by preload (`ipcRenderer.sendSync`) before the page's code runs.
export interface MediaServerAccess {
  port: number;
  token: string;
}

let ipcRegistered = false;

export async function startMediaServer(): Promise<number> {
  if (!ipcRegistered) {
    ipcRegistered = true;
    ipcMain.on("decibell:mediaServer:access", (e) => {
      const access: MediaServerAccess = { port, token: port > 0 ? token : "" };
      e.returnValue = access;
    });
  }
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

export function stopMediaServer(): void {
  if (server) {
    server.close();
    server = null;
    port = 0;
  }
}

/// Constant-time check of a path's first segment against the token.
function tokenMatches(segment: string | undefined): boolean {
  if (!segment) return false;
  const got = Buffer.from(segment, "utf8");
  return got.length === tokenBytes.length && timingSafeEqual(got, tokenBytes);
}

async function handleRequest(
  req: http.IncomingMessage,
  res: http.ServerResponse,
): Promise<void> {
  try {
    // Anything else in Host is a rebound DNS name or a request not meant
    // for us. Refused before the path is even looked at.
    if (req.headers.host !== `127.0.0.1:${port}`) {
      res.writeHead(403, { "Content-Type": "text/plain" });
      res.end("forbidden");
      return;
    }
    if (req.method !== "GET") {
      res.writeHead(405, { "Content-Type": "text/plain", Allow: "GET" });
      res.end("method not allowed");
      return;
    }
    const url = new URL(req.url ?? "/", `http://127.0.0.1:${port}`);
    const parts = url.pathname.split("/").filter((p) => p.length > 0);
    // Expected: /<token>/attachments/<serverId>/<attachmentId>
    let serverId = "";
    try {
      serverId = parts.length === 4 ? decodeURIComponent(parts[2]) : "";
    } catch {
      serverId = "";
    }
    const attachmentId = parts[3] ?? "";
    if (
      !tokenMatches(parts[0]) ||
      parts[1] !== "attachments" ||
      !serverId ||
      !ATTACHMENT_ID.test(attachmentId)
    ) {
      res.writeHead(404, { "Content-Type": "text/plain" });
      res.end("not found");
      return;
    }
    const target = getAttachmentTarget(serverId);
    if (!target) {
      res.writeHead(404, { "Content-Type": "text/plain" });
      res.end("not connected");
      return;
    }
    // Encrypted-channel attachment: main opens the sealed chunks and
    // answers the media element's Range probes with plaintext.
    const dec = await fetchDecryptedAttachment(
      serverId,
      attachmentId,
      url.search,
      typeof req.headers.range === "string" ? req.headers.range : undefined,
    );
    if (dec) {
      res.writeHead(dec.status, dec.statusText, dec.headers);
      res.end(Buffer.from(dec.body.buffer, dec.body.byteOffset, dec.body.byteLength));
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
    if (!app.isPackaged) {
      // Dev only: one line per range request (video seeks/scrubs fire
      // many), and main-process stdout writes are synchronous.
      // eslint-disable-next-line no-console
      console.log(
        `[mediaServer] GET ${upstream}${upstreamHeaders.range ? ` ${upstreamHeaders.range}` : ""}`,
      );
    }
    // no-store: see protocol.ts.
    const upstreamResp = await net.fetch(upstream, {
      method: "GET",
      headers: upstreamHeaders,
      ...NO_STORE,
    });
    if (!app.isPackaged) {
      // eslint-disable-next-line no-console
      console.log(
        `[mediaServer] → ${upstreamResp.status} ${upstreamResp.statusText}`,
      );
    }
    // Mirror status + headers. Node's writeHead expects a plain object.
    const respHeaders: Record<string, string> = {};
    upstreamResp.headers.forEach((value, name) => {
      // Strip hop-by-hop headers Node will set itself.
      if (name === "connection" || name === "transfer-encoding") return;
      respHeaders[name] = value;
    });
    res.writeHead(upstreamResp.status, upstreamResp.statusText, respHeaders);
    if (upstreamResp.body) {
      const reader = pacedBody(upstreamResp.body, serverId, attachmentId).getReader();
      // Stream the body chunk-by-chunk so a 4 GB video doesn't get
      // buffered into RAM. Chromium's MF renderer reads with its own
      // range-fetch loop anyway, so each request is typically only
      // a few MB.
      //
      // The client routinely goes away mid-stream (seek = abort the
      // old range request, close the player = abort everything), so
      // the loop must stop pulling from upstream the moment the
      // response socket closes — and the backpressure wait must wake
      // on close too, or a disconnect during a full write buffer
      // leaves this handler awaiting a "drain" that never fires.
      let clientGone = false;
      const onClose = () => {
        clientGone = true;
        reader.cancel().catch(() => {});
      };
      res.on("close", onClose);
      try {
        while (!clientGone) {
          const { done, value } = await reader.read();
          if (done) break;
          if (value && !clientGone) {
            if (!res.write(Buffer.from(value))) {
              await new Promise<void>((r) => {
                const wake = () => {
                  res.off("drain", wake);
                  res.off("close", wake);
                  r();
                };
                res.once("drain", wake);
                res.once("close", wake);
              });
            }
          }
        }
      } finally {
        res.off("close", onClose);
        reader.cancel().catch(() => {});
      }
    }
    res.end();
  } catch (e) {
    // eslint-disable-next-line no-console
    console.error(`[mediaServer] error: ${(e as Error).message}`);
    if (!res.headersSent) {
      res.writeHead(500, { "Content-Type": "text/plain" });
      res.end(`error: ${(e as Error).message}`);
    } else {
      // Headers (and possibly part of a binary body) already went
      // out — appending an error string would corrupt the stream.
      res.destroy();
    }
  }
}
