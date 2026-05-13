import { app, BrowserWindow, desktopCapturer, ipcMain, session } from "electron";
import * as path from "path";
import * as fs from "node:fs";
import { registerInvokeHandler } from "./ipc";
import {
  registerProtocol,
  registerAttachmentProtocol,
  registerFileProtocol,
} from "./protocol";
import { initAddon, shutdownAddon } from "./addon";
import { registerWindowHandlers, attachWindowEvents } from "./window";
import { registerDialogHandlers } from "./dialog";
import { registerFsHandlers } from "./fs";
import { registerNetHandlers } from "./netFetch";
import { startMediaServer, stopMediaServer, getMediaServerPort } from "./mediaServer";

// Single-instance lock — second launches focus the existing window.
// Required for deep-link handling on Windows/Linux (so a second
// `decibell://invite/...` invocation forwards the URL to the running
// app rather than spawning a fresh one).
if (!app.requestSingleInstanceLock()) {
  app.quit();
  process.exit(0);
}

// Register `decibell://` as a default protocol handler. On macOS the
// system delivers the URL via the `open-url` event; on Windows/Linux
// it's appended to the launching process's argv.
if (process.defaultApp) {
  // dev: pass the script path explicitly so the OS knows which entry
  // point to invoke when handling the protocol.
  if (process.argv.length >= 2) {
    app.setAsDefaultProtocolClient("decibell", process.execPath, [
      path.resolve(process.argv[1]),
    ]);
  }
} else {
  app.setAsDefaultProtocolClient("decibell");
}

// Buffer deep-link URLs received before the renderer is ready and
// drain them once it finishes loading. Handles both first-launch
// (URL in argv) and macOS open-url-after-launch.
const pendingDeepLinks: string[] = [];
const findDeepLinkInArgv = (argv: string[]): string | undefined =>
  argv.find((a) => typeof a === "string" && a.startsWith("decibell://"));

const forwardDeepLink = (url: string): void => {
  const win = BrowserWindow.getAllWindows()[0];
  if (win && !win.webContents.isLoading()) {
    win.webContents.send("decibell:event", {
      name: "deep_link_received",
      payload: { url },
    });
  } else {
    pendingDeepLinks.push(url);
  }
};

// Capture an invite URL passed on first launch (Linux/Windows).
const launchUrl = findDeepLinkInArgv(process.argv);
if (launchUrl) pendingDeepLinks.push(launchUrl);

// macOS: deep links arrive as an event after launch.
app.on("open-url", (event, url) => {
  event.preventDefault();
  forwardDeepLink(url);
});

// Linux + NVIDIA: nudge libva to load the community
// nvidia-vaapi-driver if the user has it installed but hasn't set the
// env var. Without LIBVA_DRIVER_NAME=nvidia, libva tries the default
// (modesetting / iHD / radeonsi) and gets nothing useful on NVIDIA.
// /dev/nvidia0 only exists when the proprietary nvidia kernel module
// is loaded, so the check is a safe proxy for "this user has an NVIDIA
// GPU active". Must run before any GPU-process child is forked, so we
// set it before app.commandLine switches.
if (
  process.platform === "linux" &&
  fs.existsSync("/dev/nvidia0")
) {
  if (!process.env.LIBVA_DRIVER_NAME) {
    process.env.LIBVA_DRIVER_NAME = "nvidia";
    console.log(
      "[boot] auto-set LIBVA_DRIVER_NAME=nvidia (Linux + NVIDIA GPU detected)",
    );
  }
  // nvidia-vaapi-driver supports two backends:
  //   - "direct" (default since libva 1.18): hooks DRM directly,
  //     better performance, required for Wayland.
  //   - "egl" (legacy): goes through EGL imports, slower.
  // Setting NVD_BACKEND=direct in the GPU process's env avoids the
  // chance Chromium's stripped-down env causes the driver to fall
  // back to the slower path.
  if (!process.env.NVD_BACKEND) {
    process.env.NVD_BACKEND = "direct";
    console.log("[boot] auto-set NVD_BACKEND=direct (nvidia-vaapi-driver)");
  }
}

// Chromium feature flags. Each feature is tagged by the platform(s)
// where it matters; flags Chromium doesn't recognise on a given
// platform are silently ignored, but splitting per-platform keeps the
// "why is this here" easy to answer years later.
const enableFeatures: string[] = [
  // HEVC encode/decode. Both Linux (VA-API) and Windows (MF) need
  // these to surface HEVC profiles in WebCodecs. Castlabs builds ship
  // the platform HEVC encoder/decoder; without these flags Chromium
  // gates it off as proprietary.
  "PlatformHEVCEncoderSupport",
  "PlatformHEVCDecoderSupport",
];

if (process.platform === "linux") {
  enableFeatures.push(
    // `getDisplayMedia` on Wayland — without this it rejects with
    // NotSupportedError because there's no native Chromium picker.
    "WebRTCPipeWireCapturer",
    // VA-API integration in the GPU process. Without these Linux WebCodecs
    // has no hardware path even when libva drivers are installed.
    "VaapiVideoDecoder",
    "VaapiVideoEncoder",
    // Skips Chromium's VA-API driver allowlist — required for
    // community backends like nvidia-vaapi-driver that aren't on it.
    "VaapiIgnoreDriverChecks",
    // Required on top of VaapiVideoDecoder for the GPU process to
    // actually wire WebCodecs through VA-API on Linux desktop
    // (NVIDIA + nvidia-vaapi-driver in particular).
    "AcceleratedVideoDecodeLinuxGL",
    "AcceleratedVideoDecodeLinuxZeroCopyGL",
    // Route through the Ozone abstraction so Chromium picks up
    // Wayland when the session is Wayland. Without these Chromium
    // often runs through XWayland with a degraded GPU path that
    // disables hardware video acceleration entirely on NVIDIA.
    "UseOzonePlatform",
    "WaylandWindowDecorations",
  );
}

if (process.platform === "win32") {
  enableFeatures.push(
    // D3D11VA-backed hardware video decoder. Default-on in modern
    // Chromium but the Castlabs codec-restoration patches leave some
    // gates flipped — being explicit makes the activation deterministic
    // and survives upstream changes.
    "D3D11VideoDecoder",
    // Use shared D3D11 textures between the decoder and the renderer
    // so decoded frames stay GPU-side end-to-end (no readback to
    // system memory). Mirrors the zero-copy path we relied on in the
    // Tauri client.
    "D3D11VideoDecoderUseSharedHandle",
    // Required on Castlabs Electron 33 for the WebCodecs API
    // surface (`VideoDecoder` global) to be exposed — without it,
    // StreamVideoPlayer can't construct a decoder for received
    // stream frames. This flag also routes `<video>` / `<audio>`
    // playback through MF's renderer service, which is fine as
    // long as the sandbox can spawn the helper process (see
    // sandbox handling further down).
    "MediaFoundationClearPlayback",
    // Use GPU memory buffers (D3D11 textures) for WebCodecs encoder
    // input frames. Encoders that consume D3D11 textures directly
    // (NVENC via MFT) require this; without it the encoder asks
    // for system memory frames and Chromium gives up routing to MFT.
    "UseGpuMemoryBufferVideoFrames",
  );
}

app.commandLine.appendSwitch("enable-features", enableFeatures.join(","));
app.commandLine.appendSwitch("ignore-gpu-blocklist");
// Older Chromium switch — still respected on Windows and forces the
// D3D11VA hardware decoder on instead of relying on feature-flag gating.
// Cheap insurance.
app.commandLine.appendSwitch("enable-accelerated-video-decode");

// Disable the out-of-process audio sandbox. Castlabs Electron's audio
// service can't spawn its sandboxed child on Windows ("sandbox_win.cc
// Sandbox cannot access executable. Access is denied.") in some
// environments, and a broken AudioService doesn't just kill audio
// playback — Chromium waits on it for any media element, so <video>
// also hangs silently on the first frame. We use both the feature
// flag (older Chromium path) and the explicit switch
// `audio-service-sandbox-type=none` (current Chromium path) because
// Castlabs Electron 33 doesn't always honor the feature flag.
const disableFeatures: string[] = ["AudioServiceSandbox"];
if (process.platform === "linux") {
  // Some Linux Chromium builds default to a ChromeOS-style direct video
  // decoder that bypasses the VAAPI integration entirely. Disable it so
  // VAAPI gets a chance to claim the codec.
  disableFeatures.push("UseChromeOSDirectVideoDecoder");
}
app.commandLine.appendSwitch("disable-features", disableFeatures.join(","));
if (process.platform === "win32") {
  app.commandLine.appendSwitch("audio-service-sandbox-type", "none");
  // Dev-only: drop the Chromium child-process sandbox so Media
  // Foundation's helper service can spawn. In dev, electron.exe
  // lives in node_modules under the user's profile directory, and
  // the sandbox's restricted token can be denied access to it on
  // fresh Windows installs with Defender Controlled Folder Access
  // — manifests as `sandbox_win.cc(850) Sandbox cannot access
  // executable. Access is denied.` which cascades to
  // `MediaFoundationRendererClient disconnected` and breaks all
  // `<video>` / `<audio>` attachment playback. Production builds
  // install the exe under %LOCALAPPDATA%/Programs/ with ACLs the
  // sandbox token can read, so they keep full sandboxing.
  if (!app.isPackaged) {
    app.commandLine.appendSwitch("no-sandbox");
  }
}

if (process.platform === "linux") {
  // Let Chromium auto-detect Wayland vs X11 from the running session.
  app.commandLine.appendSwitch("ozone-platform-hint", "auto");
  // Force ANGLE to use the native OpenGL backend (libGL.so / NVIDIA's
  // proprietary GL). nvidia-vaapi-driver exposes its codec profiles via
  // EGL extensions on this backend; the default ANGLE backend doesn't
  // surface those extensions, which is why videoDecodeAcceleratorSupportedProfile
  // comes back empty even with the GPU process working otherwise.
  app.commandLine.appendSwitch("use-angle", "gl");
}

// TEMPORARY DIAGNOSTIC: drop the GPU sandbox so VAAPI can probe the
// nvidia-vaapi-driver freely. Chromium's seccomp policy for the GPU
// process can block the syscalls or device opens the driver needs to
// enumerate codec profiles; running unsandboxed isolates that as the
// cause vs. a deeper integration mismatch. NEVER ship this — the GPU
// process is a privileged target and disabling its sandbox is a real
// security regression. Once the test answers our question we revert.
if (process.platform === "linux" && process.env.DECIBELL_GPU_SANDBOX_OFF === "1") {
  app.commandLine.appendSwitch("disable-gpu-sandbox");
  console.warn(
    "[boot] GPU sandbox DISABLED via DECIBELL_GPU_SANDBOX_OFF=1 (diagnostic)",
  );
}
// Surface Chromium's stderr to our terminal so GPU-init failures are
// visible. Default verbosity (just warnings + errors); add `--v=1` to
// the chain if we need deeper traces later.
app.commandLine.appendSwitch("enable-logging", "stderr");

const devServerUrl = process.env.VITE_DEV_SERVER_URL;
let mainWindow: BrowserWindow | null = null;

function createWindow(): void {
  // App icon: in dev we live under <repo>/electron-client and the
  // icon sits at resources/icon.png; in a packaged build, electron-
  // builder copies the buildResources/ contents into the app's
  // Resources directory at the root of resourcesPath.
  const iconPath = app.isPackaged
    ? path.join(process.resourcesPath, "icon.png")
    : path.join(__dirname, "..", "..", "..", "resources", "icon.png");

  mainWindow = new BrowserWindow({
    width: 1280,
    height: 800,
    minWidth: 800,
    minHeight: 600,
    backgroundColor: "#0e0f16",
    icon: iconPath,
    show: false,
    // Frameless so the renderer's custom Titlebar (h-8, min/max/close
    // SVG buttons, drag region) replaces the OS chrome. Matches the
    // tauri-client window decorations="custom" setup.
    frame: false,
    webPreferences: {
      preload: path.join(__dirname, "../preload/index.js"),
      contextIsolation: true,
      nodeIntegration: false,
      sandbox: false,
      // Off so the renderer can fetch the community server's
      // attachment HTTPS endpoints across origins. The community
      // server is a bespoke service that doesn't speak CORS, and
      // Chromium's CORS check would otherwise reject every
      // PATCH/POST. Trade-off: same as tauri-client's stance —
      // the renderer's only loaded code is our own app bundle, so
      // the cross-origin restriction adds no real defence here.
      webSecurity: false,
      // Smuggle the loopback media-server port to the renderer so
      // buildAttachmentUrl can construct http://127.0.0.1:PORT/...
      // URLs for `<video>` / `<audio>` elements. Synchronous
      // hand-off at window creation — no IPC round-trip needed.
      additionalArguments: [
        `--decibell-media-server-port=${getMediaServerPort()}`,
      ],
    },
  });

  mainWindow.once("ready-to-show", () => mainWindow?.show());
  // Dump GPU info once the GPU process has done real work (after the
  // first paint). Calling earlier yields a half-populated snapshot.
  // Also drain any deep-link URLs that arrived before the renderer
  // was ready (first-launch invite, macOS open-url before window).
  mainWindow.webContents.once("did-finish-load", () => {
    void dumpGpuInfo();
    if (pendingDeepLinks.length > 0 && mainWindow) {
      for (const url of pendingDeepLinks) {
        mainWindow.webContents.send("decibell:event", {
          name: "deep_link_received",
          payload: { url },
        });
      }
      pendingDeepLinks.length = 0;
    }
  });
  attachWindowEvents(mainWindow);

  if (devServerUrl) {
    mainWindow.loadURL(devServerUrl);
    mainWindow.webContents.openDevTools({ mode: "detach" });
  } else {
    // dist/electron/main/index.js → dist/renderer/index.html
    mainWindow.loadFile(path.join(__dirname, "..", "..", "renderer", "index.html"));
  }

  mainWindow.on("closed", () => {
    mainWindow = null;
  });
}

app.on("second-instance", (_event, argv) => {
  if (mainWindow) {
    if (mainWindow.isMinimized()) mainWindow.restore();
    mainWindow.focus();
  }
  // The second invocation may carry a `decibell://invite/...` URL that
  // the OS handed off via the protocol-client registration. Forward it
  // to the renderer so DeepLinkJoinModal can act on it.
  const url = findDeepLinkInArgv(argv);
  if (url) forwardDeepLink(url);
});

  // GPU info dump is deferred to after the first window's
  // did-finish-load — `getGPUInfo("basic")` returns immediately with
  // partial data (often everything `undefined` if the GPU process
  // hasn't initialized yet), and `"complete"` blocks waiting for the
  // GPU process to actually report. We schedule it inside
  // createWindow so it runs after the renderer has painted at least
  // once, by which point the GPU process has done real work.
  const dumpGpuInfo = async (): Promise<void> => {
    try {
      const features = app.getGPUFeatureStatus();
      const info = (await app.getGPUInfo("complete")) as Record<string, unknown>;
      const auxAttrs = (info.auxAttributes ?? {}) as Record<string, unknown>;
      const gpuDevice = (info.gpuDevice as Array<Record<string, unknown>> | undefined)?.[0] ?? {};
      console.log("[boot] GPU feature status:", features);
      console.log("[boot] GPU device:", {
        vendorId: gpuDevice.vendorId,
        deviceId: gpuDevice.deviceId,
        vendorString: gpuDevice.vendorString,
        deviceString: gpuDevice.deviceString,
        driverVendor: gpuDevice.driverVendor,
        driverVersion: gpuDevice.driverVersion,
      });
      console.log("[boot] GPU GL:", {
        glRenderer: auxAttrs.glRenderer,
        glVendor: auxAttrs.glVendor,
        glVersion: auxAttrs.glVersion,
        glExtensionsLength: typeof auxAttrs.glExtensions === "string"
          ? (auxAttrs.glExtensions as string).length
          : 0,
      });
      // The HW codec profile arrays (`videoDecodeAcceleratorSupportedProfiles`
      // etc.) used to live on Chromium's GPUInfo struct and surface on
      // Electron's getGPUInfo("complete") output. Electron 33 dropped
      // them from the JS-side serialisation — only `gpuDevice` /
      // `auxAttributes` / `featureStatus` survive. So the authoritative
      // signal on Electron 33+ is `featureStatus.video_decode` and
      // `featureStatus.video_encode`, plus the WebCodecs probe
      // (encoderProbe.ts / decoderProbe.ts) which speaks to the actual
      // encoder/decoder factory the renderer will use.
      const decodeProfiles =
        info.videoDecodeAcceleratorSupportedProfile ??
        info.videoDecodeAcceleratorSupportedProfiles;
      const encodeProfiles =
        info.videoEncodeAcceleratorSupportedProfile ??
        info.videoEncodeAcceleratorSupportedProfiles;
      console.log(
        "[boot] HW decode profiles:",
        decodeProfiles ?? "(not exposed by Electron 33 — see featureStatus)",
      );
      console.log(
        "[boot] HW encode profiles:",
        encodeProfiles ?? "(not exposed by Electron 33 — see featureStatus)",
      );
    } catch (e) {
      console.warn("[boot] GPU info dump failed:", e);
    }
  };

// Source id pre-stashed by the renderer right before its getDisplayMedia
// call when the user picked from our custom Screens/Windows grid
// (Windows path). The next setDisplayMediaRequestHandler invocation
// consumes this and clears it; if not set, the handler falls back to
// auto-picking sources[0] — that's the Linux xdg-desktop-portal path,
// where the portal narrows getSources to the user's choice already.
let pendingCaptureSourceId: string | null = null;

ipcMain.handle(
  "decibell:capture:setNextSource",
  (_e, id: string | null) => {
    pendingCaptureSourceId = id;
  },
);

// Enumerate desktop capture sources for the renderer's custom screen-share
// picker (Windows; macOS uses useSystemPicker; Linux uses xdg-desktop-portal).
// Chromium does the capture + thumbnail rendering inside desktopCapturer —
// we just serialise its output for the renderer. Thumbnails are returned
// as PNG data URLs (cheap to ship over the IPC; Chromium decodes them
// straight to a paintable bitmap on assignment to <img>).
ipcMain.handle(
  "decibell:capture:listSources",
  async (
    _e,
    opts: { thumbnailWidth?: number; thumbnailHeight?: number } | undefined,
  ) => {
    const w = opts?.thumbnailWidth ?? 320;
    const h = opts?.thumbnailHeight ?? 180;
    const sources = await desktopCapturer.getSources({
      types: ["screen", "window"],
      thumbnailSize: { width: w, height: h },
      fetchWindowIcons: true,
    });
    return sources.map((s) => ({
      id: s.id,
      name: s.name,
      // `display_id` is set on screens (matches Electron's `screen.getAllDisplays()`
      // ids) and empty on windows. Useful for ordering screens by the user's
      // primary-monitor preference if we ever want it.
      displayId: s.display_id ?? "",
      // Windows-only: app icon as PNG data URL. Empty string when no icon
      // is available (screens never have one) so the renderer can fall
      // back to a generic glyph without a null check.
      appIcon: s.appIcon && !s.appIcon.isEmpty() ? s.appIcon.toDataURL() : "",
      thumbnail: s.thumbnail.toDataURL(),
      // Discriminator for UI tabs — id prefix is stable across Chromium versions.
      kind: s.id.startsWith("screen:") ? "screen" : "window",
    }));
  },
);

app.whenReady().then(async () => {
  registerProtocol();
  registerAttachmentProtocol();
  registerFileProtocol();
  registerInvokeHandler();
  registerWindowHandlers();
  registerDialogHandlers();
  registerFsHandlers();
  registerNetHandlers();

  // Loopback HTTP proxy for `<video>` / `<audio>` element sources. See
  // mediaServer.ts header for the why; the port the OS picks is passed
  // into the renderer through BrowserWindow additionalArguments below.
  await startMediaServer();

  // PR8: getDisplayMedia handler.
  //
  // Electron rejects renderer-initiated `getDisplayMedia` with
  // NotSupportedError unless we register a handler — even with
  // WebRTCPipeWireCapturer enabled, the request still has to hit a
  // handler that returns a source. On Linux (Wayland), invoking
  // `desktopCapturer.getSources` triggers xdg-desktop-portal's screen
  // picker — the user's system dialog opens, they choose a window or
  // screen, and the portal returns just the selected source. On X11
  // and Windows we get back the full enumerated list; passing the
  // first source means the renderer's CaptureSourcePicker is the
  // visible UI surface (today: it just kicks the request and accepts
  // whatever Electron returns — fine for the Wayland-portal path).
  //
  // `useSystemPicker: true` is macOS-only (15+); other platforms ignore it.
  session.defaultSession.setDisplayMediaRequestHandler(
    (_request, callback) => {
      desktopCapturer
        .getSources({ types: ["screen", "window"] })
        .then((sources) => {
          if (sources.length === 0) {
            callback({});
            return;
          }
          // If the renderer pre-stashed a specific source id (Windows
          // custom picker), honour it. Clear the pending id so a later
          // stray getDisplayMedia request doesn't accidentally reuse
          // the previous choice. Falls back to sources[0] when nothing
          // was pre-picked — that's the Linux portal path where the
          // OS already narrowed the list to the user's selection.
          let chosen = sources[0];
          if (pendingCaptureSourceId) {
            const found = sources.find((s) => s.id === pendingCaptureSourceId);
            if (found) chosen = found;
            pendingCaptureSourceId = null;
          }
          callback({ video: chosen });
        })
        .catch(() => callback({}));
    },
    { useSystemPicker: process.platform === "darwin" },
  );

  // Community + central servers use self-signed TLS certs (the Rust
  // chat path uses NoVerifier in net/tls.rs to match). Tell Chromium's
  // network stack to do the same so renderer-side fetch() can hit the
  // attachment HTTP endpoints. Verification result 0 = trust this
  // certificate. Default-pass anything else through (-3 = use
  // Chromium's default verification result).
  //
  // This is strictly a "trust on first use, never verify" stance,
  // matching the C++ client's `ssl::verify_none` baseline. Production
  // hardening (cert pinning per joined community) is a separate piece
  // of work not in this PR.
  session.defaultSession.setCertificateVerifyProc((_req, callback) => {
    callback(0);
  });

  // Belt-and-suspenders: even with the verify proc above, some
  // Electron builds raise certificate errors for self-signed certs
  // through this event path before the verify proc kicks in (notably
  // for `net.fetch` from main during early connection setup). Trust
  // the cert here too, matching the ssl::verify_none baseline of the
  // C++ client.
  app.on(
    "certificate-error",
    (event, _webContents, _url, _error, _certificate, callback) => {
      event.preventDefault();
      callback(true);
    },
  );

  createWindow();
  // initAddon must come AFTER createWindow so the bus broadcaster has
  // a window to dispatch to. Events fired before the renderer attaches
  // its 'decibell:event' listener are dropped — Electron only buffers
  // webContents.send calls after did-finish-load. The PR2 demo emit
  // sleeps 3s for exactly this reason.
  initAddon();

  app.on("activate", () => {
    if (BrowserWindow.getAllWindows().length === 0) createWindow();
  });
});

app.on("before-quit", async (e) => {
  // Give Rust a chance to drop engines + TSFNs cleanly. Tauri papered
  // over this via its async_runtime; here we have to do it ourselves.
  //
  // 3-second timeout is a backstop: even if shutdownAddon hangs for
  // some unforeseen reason (a deadlocked Mutex, a thread that can't
  // see its stop flag, etc.) we still force-exit the process. Without
  // this fallback, an unresponsive shutdown would leave decibell.exe
  // running and the user would have to kill it via Task Manager —
  // exactly the bug we're fixing here.
  e.preventDefault();
  const timeout = new Promise<void>((resolve) => setTimeout(resolve, 3000));
  try {
    await Promise.race([shutdownAddon(), timeout]);
  } finally {
    stopMediaServer();
    app.exit(0);
  }
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
