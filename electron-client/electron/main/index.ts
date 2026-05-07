import { app, BrowserWindow, desktopCapturer, session } from "electron";
import * as path from "path";
import { registerInvokeHandler } from "./ipc";
import { registerProtocol, registerAttachmentProtocol } from "./protocol";
import { initAddon, shutdownAddon } from "./addon";
import { registerWindowHandlers, attachWindowEvents } from "./window";
import { registerDialogHandlers } from "./dialog";
import { registerFsHandlers } from "./fs";
import { registerNetHandlers } from "./netFetch";

// Single-instance lock — second launches focus the existing window.
// Required for deep-link handling on Windows/Linux (PR2+).
if (!app.requestSingleInstanceLock()) {
  app.quit();
  process.exit(0);
}

// Two Chromium feature flags we need:
//
// - `WebRTCPipeWireCapturer` enables `getDisplayMedia` on Linux/Wayland
//   by routing the screen-share request through xdg-desktop-portal +
//   PipeWire. Without it `getDisplayMedia` rejects with NotSupportedError
//   on Wayland (Chromium falls back to legacy X11 capture which is gone
//   on pure-Wayland sessions).
//
// - `PlatformHEVCEncoderSupport` / `PlatformHEVCDecoderSupport` try to
//   light up HEVC hardware encode/decode where Chromium has the code
//   path compiled in. The bundled Electron Chromium build often leaves
//   HEVC encode off due to MPEG-LA royalty concerns; if that's the
//   case here, `VideoEncoder.isConfigSupported` for hev1/hvc1 still
//   returns false at runtime and encoderProbe hides HEVC. H.264 + AV1
//   hardware encode work unconditionally where the GPU supports them.
app.commandLine.appendSwitch(
  "enable-features",
  "WebRTCPipeWireCapturer,PlatformHEVCEncoderSupport,PlatformHEVCDecoderSupport",
);

const devServerUrl = process.env.VITE_DEV_SERVER_URL;
let mainWindow: BrowserWindow | null = null;

function createWindow(): void {
  mainWindow = new BrowserWindow({
    width: 1280,
    height: 800,
    minWidth: 800,
    minHeight: 600,
    backgroundColor: "#0e0f16",
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
    },
  });

  mainWindow.once("ready-to-show", () => mainWindow?.show());
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

app.on("second-instance", () => {
  if (mainWindow) {
    if (mainWindow.isMinimized()) mainWindow.restore();
    mainWindow.focus();
  }
});

app.whenReady().then(() => {
  registerProtocol();
  registerAttachmentProtocol();
  registerInvokeHandler();
  registerWindowHandlers();
  registerDialogHandlers();
  registerFsHandlers();
  registerNetHandlers();

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
          callback({ video: sources[0] });
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
  e.preventDefault();
  try {
    await shutdownAddon();
  } finally {
    app.exit(0);
  }
});

app.on("window-all-closed", () => {
  if (process.platform !== "darwin") app.quit();
});
