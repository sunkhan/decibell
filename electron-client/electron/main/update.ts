import { app, BrowserWindow } from "electron";
import { autoUpdater } from "electron-updater";

// Owner of all electron-updater interaction. One state machine in the
// main process, broadcast to the renderer on every transition, with a
// single boot-time mode detection that decides whether we auto-
// download / auto-install or just notify.
//
// Lifecycle:
//   initUpdater()           — wires listeners, sets autoDownload +
//                             autoInstallOnAppQuit flags.
//   kickoffInitialCheck()   — schedules the first checkForUpdates() 5s
//                             after first window load.
//   manualCheck()           — bound to the AboutTab "Check now" button.
//   quitAndInstall()        — bound to the AboutTab "Restart" button +
//                             the persistent UserPanel chip.
//   getSnapshot()           — pulled by the renderer on AppLayout mount
//                             so a late-arriving listener doesn't miss
//                             the boot-time broadcast.

export type UpdateMode = "self-update" | "notify-only" | "disabled";

export type UpdateStatus =
  | { state: "idle" }
  | { state: "checking" }
  | { state: "not-available"; checkedAt: number }
  | { state: "available"; version: string }
  | { state: "downloading"; pct: number; version: string }
  | { state: "downloaded"; version: string }
  | { state: "error"; message: string };

let mode: UpdateMode = "disabled";
let status: UpdateStatus = { state: "idle" };
let pendingVersion = "";

function detectMode(): UpdateMode {
  // !isPackaged: in dev, electron-updater throws on first
  // checkForUpdates() because the bundled app-update.yml doesn't
  // exist. Disable explicitly so the renderer can render "Updates
  // disabled in development" and we don't surface a confusing error.
  if (!app.isPackaged) return "disabled";
  // Windows: NSIS installer → electron-updater drives Squirrel for
  // in-place upgrade. Castlabs Electron 33 ships a stock-Squirrel-
  // compatible binary so no fork-specific path is needed.
  if (process.platform === "win32") return "self-update";
  // Linux AppImage sets APPIMAGE to the absolute path of the running
  // .AppImage file. Without this env var, the running install is a
  // .deb / .pacman / AUR build whose upgrade is managed by the
  // distro's package manager.
  if (process.platform === "linux" && process.env.APPIMAGE) {
    return "self-update";
  }
  // macOS .dmg, Linux .deb / .pacman / AUR — surface only "new
  // version available", don't try to download.
  return "notify-only";
}

export function initUpdater(): void {
  mode = detectMode();
  console.log(`[update] mode=${mode}, version=${app.getVersion()}`);
  if (mode === "disabled") {
    broadcast();
    return;
  }

  autoUpdater.autoDownload = mode === "self-update";
  autoUpdater.autoInstallOnAppQuit = mode === "self-update";
  autoUpdater.logger = console;

  autoUpdater.on("checking-for-update", () => setStatus({ state: "checking" }));
  autoUpdater.on("update-not-available", () =>
    setStatus({ state: "not-available", checkedAt: Date.now() }),
  );
  autoUpdater.on("update-available", (info) => {
    pendingVersion = info.version;
    setStatus({ state: "available", version: info.version });
  });
  autoUpdater.on("download-progress", (p) =>
    setStatus({ state: "downloading", pct: p.percent, version: pendingVersion }),
  );
  autoUpdater.on("update-downloaded", (info) =>
    setStatus({ state: "downloaded", version: info.version }),
  );
  autoUpdater.on("error", (e) =>
    setStatus({ state: "error", message: String(e?.message ?? e) }),
  );

  broadcast();
}

export function kickoffInitialCheck(): void {
  if (mode === "disabled") return;
  // 5s gives the login flow + websocket handshake room to land
  // before we add another network operation.
  setTimeout(() => {
    autoUpdater.checkForUpdates().catch((err) => {
      console.error("[update] initial check failed:", err);
    });
  }, 5_000);
}

export async function manualCheck(): Promise<void> {
  if (mode === "disabled") return;
  await autoUpdater.checkForUpdates();
}

export function quitAndInstall(): void {
  if (mode !== "self-update") return;
  autoUpdater.quitAndInstall();
}

export function getSnapshot(): {
  status: UpdateStatus;
  mode: UpdateMode;
  currentVersion: string;
} {
  return { status, mode, currentVersion: app.getVersion() };
}

function setStatus(s: UpdateStatus): void {
  status = s;
  broadcast();
}

function broadcast(): void {
  const payload = { status, mode, currentVersion: app.getVersion() };
  for (const win of BrowserWindow.getAllWindows()) {
    win.webContents.send("decibell:event", { name: "update_status", payload });
  }
}
