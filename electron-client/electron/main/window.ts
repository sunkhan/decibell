import { ipcMain, BrowserWindow, type IpcMainInvokeEvent } from "electron";

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
