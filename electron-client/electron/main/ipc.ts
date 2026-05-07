import { ipcMain } from "electron";
import { callCommand } from "./addon";

export function registerInvokeHandler(): void {
  ipcMain.handle(
    "decibell:invoke",
    async (_event, method: string, args: unknown) => {
      const result = callCommand(method, args);
      // Napi async fns return Promises; sync fns return values. await
      // collapses both — contextBridge structured-clones whatever resolves.
      return await Promise.resolve(result);
    },
  );
}
