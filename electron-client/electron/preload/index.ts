import { contextBridge, ipcRenderer, webUtils } from "electron";

// Tauri-compatible event envelope so the IPC shim in src/lib/ipc.ts can
// match @tauri-apps/api/event signatures one-for-one.
type EventEnvelope = { name: string; payload: unknown };
type Handler = (envelope: { event: string; payload: unknown; id: number }) => void;

let nextId = 0;
const subs = new Map<number, { name: string; cb: Handler }>();

ipcRenderer.on("decibell:event", (_e, env: EventEnvelope) => {
  for (const [id, { name, cb }] of subs) {
    if (name === env.name) {
      cb({ event: env.name, payload: env.payload, id });
    }
  }
});

// Window-resized broadcasts (any of resize / maximize / unmaximize /
// fullscreen toggle). Each subscriber gets a void callback; the
// Titlebar uses this to re-query isMaximized() and update its icon.
const resizeSubs = new Set<() => void>();
ipcRenderer.on("decibell:window:resized", () => {
  for (const cb of resizeSubs) cb();
});

// PR7c: stream frame fan-out. Encoded video bytes arrive as Uint8Array
// (no JSON, no base64). Subscribers register for one specific
// streamer's frames, keyed by username — the dispatch below does an
// O(1) Map lookup instead of fanning every frame to every subscribed
// player. With M players watching N streamers, the old global Set
// path was O(N·M) closure invocations per second; this is O(1) per
// frame regardless of how many players are mounted.
type StreamFrame = {
  username: string;
  codec: number;
  keyframe: boolean;
  timestamp: number;
  data: Uint8Array;
  description: Uint8Array | null;
};
type StreamFrameHandler = (frame: StreamFrame) => void;
const streamFrameSubsByUser = new Map<string, Set<StreamFrameHandler>>();
ipcRenderer.on("decibell:stream_frame", (_e, frame: StreamFrame) => {
  const subs = streamFrameSubsByUser.get(frame.username);
  if (!subs) return;
  for (const cb of subs) cb(frame);
});

type StreamThumbnail = {
  ownerUsername: string;
  data: Uint8Array;
};
type StreamThumbnailHandler = (thumb: StreamThumbnail) => void;
const streamThumbnailSubs = new Set<StreamThumbnailHandler>();
ipcRenderer.on("decibell:stream_thumbnail", (_e, thumb: StreamThumbnail) => {
  for (const cb of streamThumbnailSubs) cb(thumb);
});

type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "not-available"; checkedAt: number }
  | { state: "available"; version: string }
  | { state: "downloading"; pct: number; version: string }
  | { state: "downloaded"; version: string }
  | { state: "error"; message: string };

type UpdateMode = "self-update" | "notify-only" | "disabled";

type UpdateSnapshot = {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
};

type CaptureSource = {
  id: string;
  name: string;
  displayId: string;
  appIcon: string;
  thumbnail: string;
  kind: "screen" | "window";
};

// Loopback media-server port — passed in via BrowserWindow
// additionalArguments at window-create time so it's available
// synchronously here (no IPC round-trip). Falls back to 0 if absent,
// which makes buildAttachmentUrl skip the http:// rewrite and the
// custom protocol still handles non-media attachments fine.
const mediaPortArg = process.argv.find((a) =>
  a.startsWith("--decibell-media-server-port="),
);
const mediaServerPort = mediaPortArg
  ? parseInt(mediaPortArg.split("=")[1], 10) || 0
  : 0;

const sentryEnabledArg = process.argv.find((a) =>
  a.startsWith("--decibell-sentry-enabled="),
);
const installIdArg = process.argv.find((a) =>
  a.startsWith("--decibell-install-id="),
);
const versionArg = process.argv.find((a) =>
  a.startsWith("--decibell-version="),
);
const sentryConfig = {
  enabled: sentryEnabledArg === "--decibell-sentry-enabled=1",
  installId: installIdArg ? installIdArg.split("=")[1] : "",
  version: versionArg ? versionArg.split("=")[1] : "unknown",
};

contextBridge.exposeInMainWorld("decibell", {
  /// Platform identifier copied from Node's process.platform so the
  /// renderer can branch UI without lying via navigator.userAgent. The
  /// screen-share picker uses this to render its own tabbed source list
  /// on Windows (no native Chromium picker until Electron 35) vs. let
  /// `getDisplayMedia` go through xdg-desktop-portal on Linux.
  platform: process.platform as NodeJS.Platform,
  /// Port the main process's loopback media server is listening on.
  /// Renderer constructs `http://127.0.0.1:${port}/attachments/<sid>/<aid>`
  /// for `<video>` / `<audio>` element sources. See electron/main/mediaServer.ts.
  mediaServerPort,
  sentryConfig,
  invoke: (method: string, args: unknown): Promise<unknown> =>
    ipcRenderer.invoke("decibell:invoke", method, args),
  listen: async (name: string, cb: Handler): Promise<() => void> => {
    const id = ++nextId;
    subs.set(id, { name, cb });
    return () => {
      subs.delete(id);
    };
  },
  dialog: {
    open: (args: {
      multiple?: boolean;
      filters?: { name: string; extensions: string[] }[];
    }): Promise<string[]> =>
      ipcRenderer.invoke("decibell:dialog:open", args ?? {}) as Promise<string[]>,
    save: (args: {
      defaultPath?: string;
      filters?: { name: string; extensions: string[] }[];
    }): Promise<string | null> =>
      ipcRenderer.invoke("decibell:dialog:save", args ?? {}) as Promise<string | null>,
  },
  fs: {
    readFile: (path: string): Promise<Uint8Array> =>
      ipcRenderer.invoke("decibell:fs:readFile", path) as Promise<Uint8Array>,
    stat: (
      path: string,
    ): Promise<{ size: number; isFile: boolean; isDirectory: boolean }> =>
      ipcRenderer.invoke("decibell:fs:stat", path) as Promise<{
        size: number;
        isFile: boolean;
        isDirectory: boolean;
      }>,
    writeFile: (path: string, data: Uint8Array): Promise<void> =>
      ipcRenderer.invoke("decibell:fs:writeFile", path, data) as Promise<void>,
  },
  file: {
    /// Register an absolute path with the file whitelist. Returns a
    /// `decibell-file://` URL the renderer can `fetch()` (with Range)
    /// or assign to `<video src=>`/`<img src=>` — Chromium streams it
    /// from disk via the protocol handler in main, so the bytes never
    /// land in renderer memory as a single buffer.
    register: (
      absolutePath: string,
    ): Promise<{ url: string; size: number; mime: string; name: string }> =>
      ipcRenderer.invoke("decibell:file:register", absolutePath) as Promise<{
        url: string;
        size: number;
        mime: string;
        name: string;
      }>,
    /// Drop the whitelist entry once the upload completes / aborts.
    /// Always call this — leaving entries around lets the renderer
    /// re-fetch the file later.
    unregister: (url: string): Promise<void> =>
      ipcRenderer.invoke("decibell:file:unregister", url) as Promise<void>,
    /// Resolve a File (typically from drag-drop) to the absolute disk
    /// path the OS handed Chromium. Returns "" if the File has no
    /// backing path (e.g. clipboard paste). Replaces the deprecated
    /// `file.path` property removed in newer Electron.
    pathOf: (file: File): string => {
      try {
        return webUtils.getPathForFile(file);
      } catch {
        return "";
      }
    },
  },
  attachmentRegistry: {
    set: (
      serverId: string,
      target: { host: string; port: number; jwt: string },
    ): Promise<void> =>
      ipcRenderer.invoke(
        "decibell:attachmentRegistry:set",
        serverId,
        target,
      ) as Promise<void>,
    clear: (serverId: string): Promise<void> =>
      ipcRenderer.invoke("decibell:attachmentRegistry:clear", serverId) as Promise<void>,
    clearAll: (): Promise<void> =>
      ipcRenderer.invoke("decibell:attachmentRegistry:clearAll") as Promise<void>,
  },
  netFetch: (
    url: string,
    init: {
      method?: string;
      headers?: Record<string, string>;
      body?: string | Uint8Array;
      attachmentTarget?: { serverId: string; path: string };
    },
  ): Promise<{
    ok: boolean;
    status: number;
    statusText: string;
    headers: Record<string, string>;
    body: ArrayBuffer;
  }> => ipcRenderer.invoke("decibell:net:fetch", url, init) as Promise<{
    ok: boolean;
    status: number;
    statusText: string;
    headers: Record<string, string>;
    body: ArrayBuffer;
  }>,
  window: {
    minimize: () => ipcRenderer.invoke("decibell:window:minimize"),
    maximize: () => ipcRenderer.invoke("decibell:window:maximize"),
    unmaximize: () => ipcRenderer.invoke("decibell:window:unmaximize"),
    toggleMaximize: () => ipcRenderer.invoke("decibell:window:toggleMaximize"),
    close: () => ipcRenderer.invoke("decibell:window:close"),
    isMaximized: (): Promise<boolean> =>
      ipcRenderer.invoke("decibell:window:isMaximized") as Promise<boolean>,
    setTitle: (title: string) =>
      ipcRenderer.invoke("decibell:window:setTitle", title),
    setFullscreen: (on: boolean) =>
      ipcRenderer.invoke("decibell:window:setFullscreen", on),
    onResized: (cb: () => void): (() => void) => {
      resizeSubs.add(cb);
      return () => {
        resizeSubs.delete(cb);
      };
    },
  },
  streamFrames: {
    /// Subscribe to encoded frames for ONE streamer (by username).
    /// Pass the streamer's username so the preload bridge can dispatch
    /// directly via Map lookup instead of fanning every frame to every
    /// player and making the player check `frame.username` itself.
    /// Returns an unsubscribe fn.
    subscribe: (username: string, cb: StreamFrameHandler): (() => void) => {
      let subs = streamFrameSubsByUser.get(username);
      if (!subs) {
        subs = new Set();
        streamFrameSubsByUser.set(username, subs);
      }
      subs.add(cb);
      return () => {
        const cur = streamFrameSubsByUser.get(username);
        if (!cur) return;
        cur.delete(cb);
        if (cur.size === 0) streamFrameSubsByUser.delete(username);
      };
    },
  },
  streamThumbnails: {
    /// Subscribe to per-stream JPEG thumbnails. Caller wraps the raw
    /// bytes in a blob: URL via URL.createObjectURL — main never
    /// base64-encodes them. Returns an unsubscribe fn.
    subscribe: (cb: StreamThumbnailHandler): (() => void) => {
      streamThumbnailSubs.add(cb);
      return () => {
        streamThumbnailSubs.delete(cb);
      };
    },
  },
  capture: {
    /// Enumerate screens + windows for the screen-share picker. Thumbnails
    /// arrive as PNG data URLs ready to assign to <img>. Chromium's
    /// desktopCapturer does the actual capture; this just serialises it.
    listSources: (opts?: {
      thumbnailWidth?: number;
      thumbnailHeight?: number;
    }): Promise<CaptureSource[]> =>
      ipcRenderer.invoke("decibell:capture:listSources", opts ?? {}) as Promise<
        CaptureSource[]
      >,
    /// Pre-stash the source id the next setDisplayMediaRequestHandler
    /// callback should pick. Call before navigator.mediaDevices.getDisplayMedia.
    /// Pass null to clear (e.g., after a cancelled getDisplayMedia call).
    setNextSource: (id: string | null): Promise<void> =>
      ipcRenderer.invoke("decibell:capture:setNextSource", id) as Promise<void>,
  },
  update: {
    /// Pull the current main-process snapshot. Called on AppLayout
    /// mount to cover the case where initUpdater()'s boot-time
    /// broadcast fired before the renderer attached its listener.
    getStatus: (): Promise<UpdateSnapshot> =>
      ipcRenderer.invoke("decibell:update:getStatus") as Promise<UpdateSnapshot>,
    /// Manually trigger a check. Resolves once the autoUpdater promise
    /// resolves — the actual update_status events stream over
    /// 'decibell:event' as usual, so a caller can fire-and-forget.
    check: (): Promise<void> =>
      ipcRenderer.invoke("decibell:update:check") as Promise<void>,
    /// Quit the app and install the downloaded update. Caller must
    /// have already seen status.state === "downloaded". No-op when
    /// mode !== "self-update".
    quitAndInstall: (): Promise<void> =>
      ipcRenderer.invoke("decibell:update:quitAndInstall") as Promise<void>,
    /// Open the GitHub releases page in the user's default browser.
    /// Used by the notify-only mode action button.
    openReleasePage: (): Promise<void> =>
      ipcRenderer.invoke("decibell:update:openReleasePage") as Promise<void>,
  },
});
