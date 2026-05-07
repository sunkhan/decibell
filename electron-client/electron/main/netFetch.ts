import { ipcMain, net } from "electron";
import {
  setAttachmentTarget,
  clearAttachmentTarget,
  clearAllAttachmentTargets,
  getAttachmentTarget,
} from "./attachmentRegistry";

interface FetchInit {
  method?: string;
  headers?: Record<string, string>;
  /// Either a string body, or a Uint8Array transferred from the
  /// renderer (an ArrayBuffer view; structured-cloned over IPC).
  body?: string | Uint8Array;
}

interface FetchResult {
  ok: boolean;
  status: number;
  statusText: string;
  headers: Record<string, string>;
  body: ArrayBuffer;
}

// Renderer-driven HTTP through Electron's main-process `net` module.
// main's net.fetch goes through the same session as the BrowserWindow
// (so `setCertificateVerifyProc` applies) but bypasses CORS entirely
// — exactly what attachment uploads need against the community
// server's bespoke HTTPS endpoint.
//
// Renderer can also pass `attachmentTarget: { serverId }` to skip
// repeating host/port/jwt — the main-process registry resolves it.
export function registerNetHandlers(): void {
  ipcMain.handle("decibell:attachmentRegistry:set", (_e, serverId: string, target: { host: string; port: number; jwt: string }) => {
    setAttachmentTarget(serverId, target);
  });
  ipcMain.handle("decibell:attachmentRegistry:clear", (_e, serverId: string) => {
    clearAttachmentTarget(serverId);
  });
  ipcMain.handle("decibell:attachmentRegistry:clearAll", () => {
    clearAllAttachmentTargets();
  });

  ipcMain.handle(
    "decibell:net:fetch",
    async (
      _e,
      url: string,
      init: FetchInit & {
        attachmentTarget?: { serverId: string; path: string };
      },
    ): Promise<FetchResult> => {
      let finalUrl = url;
      const headers: Record<string, string> = { ...(init.headers ?? {}) };
      if (init.attachmentTarget) {
        const target = getAttachmentTarget(init.attachmentTarget.serverId);
        if (!target) {
          // eslint-disable-next-line no-console
          console.error(
            "[netFetch] no attachment target for server:",
            init.attachmentTarget.serverId,
          );
          throw new Error("Attachment target not registered");
        }
        finalUrl = `https://${target.host}:${target.port}${init.attachmentTarget.path}`;
        headers["Authorization"] = `Bearer ${target.jwt}`;
      }
      const bodyLen =
        init.body instanceof Uint8Array
          ? init.body.byteLength
          : typeof init.body === "string"
            ? init.body.length
            : 0;
      // eslint-disable-next-line no-console
      console.log(
        `[netFetch] ${init.method ?? "GET"} ${finalUrl} body=${bodyLen} off=${
          headers["Upload-Offset"] ?? "-"
        }`,
      );

      // net.fetch accepts string / ArrayBuffer / Uint8Array as body.
      // The structured-clone of a renderer-side Uint8Array arrives as
      // a Uint8Array on the main side; pass through unchanged.
      const fetchInit: Parameters<typeof net.fetch>[1] = {
        method: init.method ?? "GET",
        headers,
      };
      if (init.body !== undefined) {
        // eslint-disable-next-line @typescript-eslint/no-explicit-any
        (fetchInit as any).body = init.body;
      }

      try {
        const response = await net.fetch(finalUrl, fetchInit);
        // eslint-disable-next-line no-console
        console.log(`[netFetch] → ${response.status} ${response.statusText}`);
        const responseHeaders: Record<string, string> = {};
        response.headers.forEach((value, key) => {
          responseHeaders[key] = value;
        });
        const buf = await response.arrayBuffer();
        return {
          ok: response.ok,
          status: response.status,
          statusText: response.statusText,
          headers: responseHeaders,
          body: buf,
        };
      } catch (e) {
        // eslint-disable-next-line no-console
        console.error(`[netFetch] error: ${(e as Error).message}`);
        throw e;
      }
    },
  );
}
