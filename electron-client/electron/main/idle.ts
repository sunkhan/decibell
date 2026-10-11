// Auto-away (docs/superpowers/specs/2026-10-11-user-status-design.md): the
// renderer polls the system's idle state against the user's threshold and
// hears about lock / sleep the moment they happen. The renderer owns the
// timer and the setting; main only reads the OS.

import { BrowserWindow, ipcMain, powerMonitor } from "electron";

export function registerIdleHandlers(): void {
  // "active" | "idle" | "locked" | "unknown" for the given threshold.
  ipcMain.handle("decibell:idle:getState", (_e, thresholdSeconds: unknown) => {
    const t = Math.max(1, Math.floor(Number(thresholdSeconds) || 0));
    return powerMonitor.getSystemIdleState(t);
  });

  const send = (away: boolean) => {
    for (const w of BrowserWindow.getAllWindows()) {
      if (!w.isDestroyed()) w.webContents.send("decibell:idle:awayEvent", away);
    }
  };
  powerMonitor.on("lock-screen", () => send(true));
  powerMonitor.on("unlock-screen", () => send(false));
  powerMonitor.on("suspend", () => send(true));
  powerMonitor.on("resume", () => send(false));
}
