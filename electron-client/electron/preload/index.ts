import { contextBridge, ipcRenderer } from "electron";

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
// (no JSON, no base64). Each subscriber gets every frame; the
// StreamVideoPlayer filters on `frame.username` to match its watch
// target. Multiple StreamVideoPlayers (fullscreen + tile) coexist by
// each filtering independently.
type StreamFrame = {
  username: string;
  codec: number;
  keyframe: boolean;
  timestamp: number;
  data: Uint8Array;
  description: Uint8Array | null;
};
type StreamFrameHandler = (frame: StreamFrame) => void;
const streamFrameSubs = new Set<StreamFrameHandler>();
ipcRenderer.on("decibell:stream_frame", (_e, frame: StreamFrame) => {
  for (const cb of streamFrameSubs) cb(frame);
});

contextBridge.exposeInMainWorld("decibell", {
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
    /// Subscribe to every encoded video frame. Caller filters on
    /// `frame.username` to match its watch target. Returns an
    /// unsubscribe fn.
    subscribe: (cb: StreamFrameHandler): (() => void) => {
      streamFrameSubs.add(cb);
      return () => {
        streamFrameSubs.delete(cb);
      };
    },
  },
});
