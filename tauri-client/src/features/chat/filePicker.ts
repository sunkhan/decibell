import { invoke } from "@tauri-apps/api/core";
import { open as openDialog, save as saveDialog } from "@tauri-apps/plugin-dialog";

// On Linux we prefer the XDG Desktop Portal so the user gets the dark/
// floating compositor-native picker that Firefox / Vesktop / others use.
// On Windows + macOS the plugin's native dialogs are already what you'd
// want, so we use them directly. The Linux portal can fail (no portal
// backend installed, D-Bus error, etc.) — in that case we fall back to
// the plugin so the file picker still works, just less pretty.
const isLinux = /linux/i.test(navigator.userAgent);

/** Open the user's "pick files" dialog. Returns absolute paths; empty
 *  array on cancel. */
export async function pickFiles(opts: {
  title: string;
  multiple: boolean;
}): Promise<string[]> {
  if (isLinux) {
    try {
      return await invoke<string[]>("pick_attachments_xdg", {
        title: opts.title,
        multiple: opts.multiple,
      });
    } catch (err) {
      console.warn("XDG file picker failed, falling back to plugin-dialog:", err);
    }
  }
  const sel = await openDialog({
    multiple: opts.multiple,
    directory: false,
    title: opts.title,
  });
  if (!sel) return [];
  return Array.isArray(sel) ? sel : [sel];
}

/** Open the user's "save file as" dialog. Returns the chosen absolute
 *  path, or null on cancel. */
export async function pickSavePath(opts: {
  title: string;
  defaultName: string;
}): Promise<string | null> {
  if (isLinux) {
    try {
      const result = await invoke<string | null>("pick_save_path_xdg", {
        title: opts.title,
        defaultName: opts.defaultName,
      });
      return result ?? null;
    } catch (err) {
      console.warn("XDG save dialog failed, falling back to plugin-dialog:", err);
    }
  }
  return await saveDialog({
    defaultPath: opts.defaultName,
    title: opts.title,
  });
}
