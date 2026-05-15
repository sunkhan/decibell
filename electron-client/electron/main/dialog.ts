import { app, dialog, BrowserWindow, ipcMain, type IpcMainInvokeEvent } from "electron";
import * as path from "node:path";
import * as fs from "node:fs";

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

// Append " (1)", " (2)" etc. until a filename that doesn't collide
// with an existing entry in `dir`. Stem and extension are preserved so
// "report.pdf" becomes "report (1).pdf", not "report.pdf (1)".
//
// Bounded at 1000 iterations as a paranoid backstop — if a user
// somehow accumulated a thousand variants in one folder, fall back to
// the original name and let the OS prompt handle the overwrite ask.
function uniquifyInDir(dir: string, filename: string): string {
  const initial = path.join(dir, filename);
  if (!fs.existsSync(initial)) return initial;
  const ext = path.extname(filename);
  const stem = path.basename(filename, ext);
  for (let i = 1; i < 1000; i++) {
    const candidate = path.join(dir, `${stem} (${i})${ext}`);
    if (!fs.existsSync(candidate)) return candidate;
  }
  return initial;
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
    // Pre-resolve filename conflicts against the target directory so
    // the save dialog opens with "file (1).ext" pre-filled instead of
    // forcing the user to type the suffix themselves. Renderer callers
    // pass either just a basename (e.g. "report.pdf") or an absolute
    // path (rare in practice but handled for forward-compat). Anchor
    // basename-only to Downloads since that's where downloads land
    // for our chat attachments.
    let defaultPath = args.defaultPath;
    if (defaultPath) {
      if (path.isAbsolute(defaultPath)) {
        const dir = path.dirname(defaultPath);
        const base = path.basename(defaultPath);
        defaultPath = uniquifyInDir(dir, base);
      } else {
        defaultPath = uniquifyInDir(app.getPath("downloads"), defaultPath);
      }
    }
    const result = win
      ? await dialog.showSaveDialog(win, {
          defaultPath,
          filters: args.filters,
        })
      : await dialog.showSaveDialog({
          defaultPath,
          filters: args.filters,
        });
    if (result.canceled || !result.filePath) return null;
    return result.filePath;
  });
}
