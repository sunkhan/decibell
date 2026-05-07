import { dialog, BrowserWindow, ipcMain, type IpcMainInvokeEvent } from "electron";

interface OpenDialogArgs {
  multiple?: boolean;
  filters?: { name: string; extensions: string[] }[];
}

interface SaveDialogArgs {
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
}

function senderWindow(event: IpcMainInvokeEvent): BrowserWindow | null {
  return BrowserWindow.fromWebContents(event.sender);
}

// File-dialog handlers. Renderer calls these via the preload bridge
// when the user clicks the file-picker button or confirms a save
// destination. Returns absolute path strings on success, null/empty
// on cancel.
//
// We intentionally use Electron's native dialog rather than porting
// tauri-client's XDG-portal client (ashpd) because the platform
// dialog is what users expect on every OS, and it's free with
// Chromium.
export function registerDialogHandlers(): void {
  ipcMain.handle("decibell:dialog:open", async (e, args: OpenDialogArgs) => {
    const win = senderWindow(e);
    const properties: ("openFile" | "multiSelections")[] = ["openFile"];
    if (args.multiple) properties.push("multiSelections");
    const result = win
      ? await dialog.showOpenDialog(win, {
          properties,
          filters: args.filters,
        })
      : await dialog.showOpenDialog({ properties, filters: args.filters });
    if (result.canceled) return [];
    return result.filePaths;
  });

  ipcMain.handle("decibell:dialog:save", async (e, args: SaveDialogArgs) => {
    const win = senderWindow(e);
    const result = win
      ? await dialog.showSaveDialog(win, {
          defaultPath: args.defaultPath,
          filters: args.filters,
        })
      : await dialog.showSaveDialog({
          defaultPath: args.defaultPath,
          filters: args.filters,
        });
    if (result.canceled || !result.filePath) return null;
    return result.filePath;
  });
}
