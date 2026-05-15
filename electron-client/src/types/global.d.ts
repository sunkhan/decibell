import type { Event, UnlistenFn } from "../lib/ipc";
import type { CaptureSource } from "./index";

declare global {
  interface Window {
    decibell: {
      platform: NodeJS.Platform;
      mediaServerPort: number;
      invoke: (method: string, args: unknown) => Promise<unknown>;
      listen: (
        name: string,
        cb: (event: Event<unknown>) => void,
      ) => Promise<UnlistenFn>;
      dialog: {
        open: (args: {
          multiple?: boolean;
          filters?: { name: string; extensions: string[] }[];
        }) => Promise<string[]>;
        save: (args: {
          defaultPath?: string;
          filters?: { name: string; extensions: string[] }[];
        }) => Promise<string | null>;
      };
      fs: {
        readFile: (path: string) => Promise<Uint8Array>;
        stat: (path: string) => Promise<{
          size: number;
          isFile: boolean;
          isDirectory: boolean;
        }>;
        writeFile: (path: string, data: Uint8Array) => Promise<void>;
      };
      file: {
        register: (absolutePath: string) => Promise<{
          url: string;
          size: number;
          mime: string;
          name: string;
        }>;
        unregister: (url: string) => Promise<void>;
        pathOf: (file: File) => string;
      };
      attachmentRegistry: {
        set: (
          serverId: string,
          target: { host: string; port: number; jwt: string },
        ) => Promise<void>;
        clear: (serverId: string) => Promise<void>;
        clearAll: () => Promise<void>;
      };
      netFetch: (
        url: string,
        init: {
          method?: string;
          headers?: Record<string, string>;
          body?: string | Uint8Array;
          attachmentTarget?: { serverId: string; path: string };
        },
      ) => Promise<{
        ok: boolean;
        status: number;
        statusText: string;
        headers: Record<string, string>;
        body: ArrayBuffer;
      }>;
      window: {
        minimize: () => Promise<void>;
        maximize: () => Promise<void>;
        unmaximize: () => Promise<void>;
        toggleMaximize: () => Promise<void>;
        close: () => Promise<void>;
        isMaximized: () => Promise<boolean>;
        setTitle: (title: string) => Promise<void>;
        setFullscreen: (on: boolean) => Promise<void>;
        onResized: (cb: () => void) => () => void;
      };
      streamFrames: {
        subscribe: (
          username: string,
          cb: (frame: {
            username: string;
            codec: number;
            keyframe: boolean;
            timestamp: number;
            data: Uint8Array;
            description: Uint8Array | null;
          }) => void,
        ) => () => void;
      };
      streamThumbnails: {
        subscribe: (
          cb: (thumb: { ownerUsername: string; data: Uint8Array }) => void,
        ) => () => void;
      };
      capture: {
        listSources: (opts?: {
          thumbnailWidth?: number;
          thumbnailHeight?: number;
        }) => Promise<CaptureSource[]>;
        setNextSource: (id: string | null) => Promise<void>;
      };
      update: {
        getStatus: () => Promise<{
          status:
            | { state: "idle" }
            | { state: "checking" }
            | { state: "not-available"; checkedAt: number }
            | { state: "available"; version: string }
            | { state: "downloading"; pct: number; version: string }
            | { state: "downloaded"; version: string }
            | { state: "error"; message: string };
          mode: "self-update" | "notify-only" | "disabled";
          currentVersion: string;
        }>;
        check: () => Promise<void>;
        quitAndInstall: () => Promise<void>;
        openReleasePage: () => Promise<void>;
      };
    };
  }
}

export {};
