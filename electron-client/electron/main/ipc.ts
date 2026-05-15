import { ipcMain, shell } from "electron";
import { callCommand } from "./addon";
import {
  manualCheck,
  quitAndInstall,
  getSnapshot,
} from "./update";

export function registerInvokeHandler(): void {
  // Existing native-command bridge. Renderer's invoke() from
  // src/lib/ipc.ts routes here, then dispatches into the Rust addon.
  ipcMain.handle(
    "decibell:invoke",
    async (_event, method: string, args: unknown) => {
      const result = callCommand(method, args);
      // Napi async fns return Promises; sync fns return values. await
      // collapses both — contextBridge structured-clones whatever resolves.
      return await Promise.resolve(result);
    },
  );

  // Auto-update commands. Renderer reaches these via the
  // window.decibell.update.* namespace exposed in the preload bridge —
  // NOT via the Tauri-shim invoke(), which would route to the Rust
  // addon and miss these handlers entirely.
  ipcMain.handle("decibell:update:getStatus", () => {
    return getSnapshot();
  });
  ipcMain.handle("decibell:update:check", async () => {
    await manualCheck();
  });
  ipcMain.handle("decibell:update:quitAndInstall", () => {
    quitAndInstall();
  });
  ipcMain.handle("decibell:update:openReleasePage", async () => {
    await shell.openExternal(
      "https://github.com/sunkhan/decibell/releases/latest",
    );
  });
}
