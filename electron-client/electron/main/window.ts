import { app, dialog, ipcMain, shell, BrowserWindow, type IpcMainInvokeEvent } from "electron";

// Window controls — Tauri's `getCurrentWindow()` API mapped onto
// Electron's BrowserWindow. The renderer's `src/lib/window.ts` calls
// these via contextBridge; the Titlebar buttons call the shim, the
// shim calls these handlers, the handlers operate on the window that
// owns the calling webContents.

function senderWindow(event: IpcMainInvokeEvent): BrowserWindow | null {
  return BrowserWindow.fromWebContents(event.sender);
}

/// Unfinished uploads, as the renderer's Transfers panel reports them.
/// Closing the window loses them — and the message each belongs to — so
/// a close with any asks first. Downloads need no guard: they pause on
/// quit and resume next launch.
let uploadsInFlight = 0;
let lossConfirmed = false;

/// True when nothing would be lost or the user agreed to lose it. Asks
/// at most once per quit (the window close and before-quit both check).
export function confirmLosingUploads(win?: BrowserWindow | null): boolean {
  if (uploadsInFlight === 0 || lossConfirmed) return true;
  const opts: Electron.MessageBoxSyncOptions = {
    type: "warning",
    buttons: ["Keep uploading", "Quit anyway"],
    defaultId: 0,
    cancelId: 0,
    noLink: true,
    title: "Upload in progress",
    message: uploadsInFlight === 1 ? "A file is still uploading" : `${uploadsInFlight} files are still uploading`,
    detail: "Quitting now cancels the upload, and the message it belongs to won't be sent.",
  };
  const choice = win && !win.isDestroyed() ? dialog.showMessageBoxSync(win, opts) : dialog.showMessageBoxSync(opts);
  lossConfirmed = choice === 1;
  return lossConfirmed;
}

export function registerWindowHandlers(): void {
  ipcMain.handle("decibell:window:minimize", (e) => {
    senderWindow(e)?.minimize();
  });
  ipcMain.handle("decibell:window:maximize", (e) => {
    senderWindow(e)?.maximize();
  });
  ipcMain.handle("decibell:window:unmaximize", (e) => {
    senderWindow(e)?.unmaximize();
  });
  ipcMain.handle("decibell:window:toggleMaximize", (e) => {
    const w = senderWindow(e);
    if (!w) return;
    if (w.isMaximized()) w.unmaximize();
    else w.maximize();
  });
  ipcMain.handle("decibell:window:close", (e) => {
    senderWindow(e)?.close();
  });
  ipcMain.handle("decibell:window:isMaximized", (e) => {
    return senderWindow(e)?.isMaximized() ?? false;
  });
  ipcMain.handle("decibell:window:setTitle", (e, title: string) => {
    senderWindow(e)?.setTitle(title);
  });
  ipcMain.handle("decibell:window:setFullscreen", (e, on: boolean) => {
    senderWindow(e)?.setFullScreen(on);
  });
  // Transfers panel: taskbar / dock progress over everything running
  // (fraction < 0 clears it), and how many uploads a close would lose.
  ipcMain.handle(
    "decibell:window:setTransferProgress",
    (e, p: { fraction?: unknown; paused?: unknown; uploads?: unknown }) => {
      uploadsInFlight = Math.max(0, Math.floor(Number(p?.uploads) || 0));
      if (uploadsInFlight === 0) lossConfirmed = false;
      const w = senderWindow(e);
      if (!w) return;
      const f = Number(p?.fraction);
      if (!Number.isFinite(f) || f < 0) w.setProgressBar(-1);
      else w.setProgressBar(Math.min(1, f), { mode: p?.paused === true ? "paused" : "normal" });
    },
  );
  // Incoming DM call: get the user's attention without stealing focus —
  // taskbar flash (Windows / Linux) or a dock bounce (macOS). A no-op
  // when the window is already focused.
  ipcMain.handle("decibell:window:flash", (e) => {
    const w = senderWindow(e);
    if (!w || w.isFocused()) return;
    w.flashFrame(true);
    if (process.platform === "darwin") app.dock?.bounce("informational");
  });
}

/// Lock down navigation. The renderer only ever runs our own bundle
/// (the dev-server URL, or the packaged file://). Since it holds the
/// full `window.decibell` bridge with `webSecurity` off and the OS
/// sandbox off, a single navigation to attacker content would be a
/// straight path to RCE — so block top-frame navigations off-origin,
/// deny all `window.open` (route https links to the OS browser), and
/// refuse to attach any <webview>.
export function hardenNavigation(win: BrowserWindow, allowedOrigin: string): void {
  win.webContents.on("will-navigate", (e, url) => {
    let origin = "";
    try {
      origin = new URL(url).origin;
    } catch {
      /* unparseable → not allowed */
    }
    const ok =
      allowedOrigin === "file://"
        ? url.startsWith("file://")
        : origin === allowedOrigin;
    if (!ok) {
      e.preventDefault();
      console.warn("[nav] blocked top-frame navigation to", url);
    }
  });
  win.webContents.setWindowOpenHandler(({ url }) => {
    if (/^https:\/\//i.test(url)) void shell.openExternal(url);
    return { action: "deny" };
  });
  win.webContents.on("will-attach-webview", (e) => {
    // We never use <webview>; never let one attach (it could carry its
    // own preload or disable sandboxing).
    e.preventDefault();
  });

  // Mouse back/forward buttons (XButton1/2) arrive as browser-nav app
  // commands on Windows and Linux. The renderer is a state-driven SPA,
  // so walking webContents history lands on a stale document — going
  // "back" right after signing in dropped users onto the login screen.
  // Swallow the commands outright.
  win.on("app-command", (e, cmd) => {
    if (cmd === "browser-backward" || cmd === "browser-forward") {
      e.preventDefault();
    }
  });
  // Belt-and-suspenders for traversal paths app-command doesn't cover
  // (e.g. macOS trackpad swipe, or anything driving goBack directly —
  // which notably does NOT fire will-navigate): keep the back-stack
  // empty so there is never an entry to traverse to.
  win.webContents.on("did-finish-load", () => {
    win.webContents.navigationHistory.clear();
  });
}

/// Forward the Electron-side resize / maximize / unmaximize / fullscreen
/// lifecycle to the renderer as 'decibell:window:resized' carrying the
/// maximized state — and only when that state changed. The Titlebar used
/// to answer every resize step (dozens per second while dragging an edge)
/// with an isMaximized() invoke; now it just mirrors this payload. Every
/// event still re-evaluates isMaximized() here, so a WM that settles the
/// state a step late is still caught.
export function attachWindowEvents(win: BrowserWindow): void {
  let last: boolean | null = null;
  const fire = () => {
    if (win.isDestroyed()) return;
    const maximized = win.isMaximized();
    if (maximized === last) return;
    last = maximized;
    win.webContents.send("decibell:window:resized", maximized);
  };
  // A reloaded renderer re-reads isMaximized() on mount; forget what the
  // previous document was told so the next change is always sent.
  win.webContents.on("did-start-loading", () => {
    last = null;
  });
  win.on("close", (e) => {
    if (!confirmLosingUploads(win)) e.preventDefault();
  });
  win.on("resize", fire);
  win.on("maximize", fire);
  win.on("unmaximize", fire);
  win.on("enter-full-screen", fire);
  win.on("leave-full-screen", fire);
}
