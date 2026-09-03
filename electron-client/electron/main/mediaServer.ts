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
import { fetchDecryptedAttachment } from "./attachmentFetch";

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
