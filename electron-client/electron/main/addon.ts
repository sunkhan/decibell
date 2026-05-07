import * as path from "path";
import * as os from "os";
import { app, BrowserWindow } from "electron";

type EventEnvelope = { name: string; payload: unknown };
type StreamFrame = {
  username: string;
  codec: number;
  keyframe: boolean;
  timestamp: number;
  data: Uint8Array;
  description: Uint8Array | null;
};
type InitOptions = {
  userDataDir: string;
  cacheDir: string;
  appVersion: string;
};
type AddonInit = (
  opts: InitOptions,
  bus: (env: EventEnvelope) => void,
  streamBus: (frame: StreamFrame) => void,
) => void;
type AddonShutdown = () => Promise<void>;

// .node files cannot live inside app.asar — electron-builder unpacks
// native/ via asarUnpack. In dev we resolve from native/ at the repo
// root; in prod from app.asar.unpacked/native/.
function addonDir(): string {
  if (app.isPackaged) {
    return path.join(process.resourcesPath, "app.asar.unpacked", "native");
  }
  return path.join(__dirname, "..", "..", "..", "native");
}

// Mirror napi-rs's index.js platform-arch-libc detection, but bypass
// require() so we can load with RTLD_DEEPBIND (see comment below).
function platformBinaryName(): string {
  const platform = process.platform;
  const arch = process.arch;
  if (platform === "linux") {
    // Best-effort musl detection. Glibc systems (the common case)
    // expose `glibcVersionRuntime` in the report header.
    let isMusl = false;
    try {
      const header = (process.report?.getReport() as { header?: { glibcVersionRuntime?: string } } | undefined)?.header;
      isMusl = !header?.glibcVersionRuntime;
    } catch {
      isMusl = false;
    }
    return `index.linux-${arch}-${isMusl ? "musl" : "gnu"}.node`;
  }
  if (platform === "darwin") return `index.darwin-${arch}.node`;
  if (platform === "win32") return `index.win32-${arch}-msvc.node`;
  throw new Error(`Unsupported platform: ${platform}-${arch}`);
}

// Standard load. We previously tried RTLD_DEEPBIND so our addon's
// `avcodec_*` calls would bind to /usr/lib/libavcodec.so.62 instead of
// Electron's bundled libffmpeg.so (which has only ~15 codecs and
// shadowed our refs). It worked for symbol resolution but crashed
// voice channel join — the cross-loading of shared libs (libpulse /
// libasound for cpal, etc.) created allocator-mismatch SIGSEGVs.
// Streaming-via-FFmpeg-from-the-renderer-process is its own follow-up;
// see project_electron_migration.md for the dual-libffmpeg analysis.
const addonModule: { exports: Record<string, unknown> } = { exports: {} };
const binaryPath = path.join(addonDir(), platformBinaryName());
process.dlopen(
  addonModule as unknown as NodeJS.Module,
  binaryPath,
  os.constants.dlopen.RTLD_NOW,
);
export const addon: Record<string, unknown> = addonModule.exports;
// eslint-disable-next-line no-console
console.log("[decibell] addon loaded, exports:", Object.keys(addon));

export function callCommand(method: string, args: unknown): unknown {
  const fn = addon[method];
  if (typeof fn !== "function") {
    throw new Error(`unknown command: ${method}`);
  }
  return (fn as (a: unknown) => unknown)(args ?? {});
}

/// Call exactly once after `app.whenReady()`. Hands the addon its boot
/// config and two broadcasters:
/// - `bus` for normal named events (login, presence, etc.) — JSON over
///   `decibell:event`.
/// - `streamBus` for encoded video frames — binary `Uint8Array` payloads
///   over `decibell:stream_frame`. Per-stream filtering happens in the
///   renderer (StreamVideoPlayer matches on `frame.username`); the wire
///   carries one fanout per active watcher.
export function initAddon(): void {
  const init = addon.init as AddonInit;
  init(
    {
      userDataDir: app.getPath("userData"),
      cacheDir: path.join(app.getPath("userData"), "media-cache"),
      appVersion: app.getVersion(),
    },
    (env) => {
      for (const w of BrowserWindow.getAllWindows()) {
        w.webContents.send("decibell:event", env);
      }
    },
    (frame) => {
      // Electron's IPC structured-clone handles Uint8Array natively
      // (zero-copy on the send side, structured-clone on the receive
      // side). No JSON, no base64.
      for (const w of BrowserWindow.getAllWindows()) {
        w.webContents.send("decibell:stream_frame", frame);
      }
    },
  );
}

export async function shutdownAddon(): Promise<void> {
  const shutdown = addon.shutdown as AddonShutdown | undefined;
  if (shutdown) await shutdown();
}
