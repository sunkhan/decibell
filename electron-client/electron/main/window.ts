import { ipcMain, shell, BrowserWindow, type IpcMainInvokeEvent } from "electron";

// Window controls — Tauri's `getCurrentWindow()` API mapped onto
// Electron's BrowserWindow. The renderer's `src/lib/window.ts` calls
// these via contextBridge; the Titlebar buttons call the shim, the
// shim calls these handlers, the handlers operate on the window that
// owns the calling webContents.

function senderWindow(event: IpcMainInvokeEvent): BrowserWindow | null {
  return BrowserWindow.fromWebContents(event.sender);
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

/// Forward the Electron-side resize / maximize / unmaximize lifecycle
/// to the renderer as a single 'decibell:window:resized' event so the
/// Titlebar can re-query isMaximized() and update its restore icon.
export function attachWindowEvents(win: BrowserWindow): void {
  const fire = () => win.webContents.send("decibell:window:resized");
  win.on("resize", fire);
  win.on("maximize", fire);
  win.on("unmaximize", fire);
  win.on("enter-full-screen", fire);
  win.on("leave-full-screen", fire);
}
