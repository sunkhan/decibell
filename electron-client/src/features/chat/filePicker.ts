// Thin wrapper around Electron's showOpenDialog (exposed via the
// preload bridge as window.decibell.dialog.open). Mirrors the surface
// of @tauri-apps/plugin-dialog's open() so callers stay readable.

interface PickArgs {
  multiple?: boolean;
  filters?: { name: string; extensions: string[] }[];
}

/// Open the system file picker. Returns absolute paths on success,
/// null when the user cancelled.
export async function pickFiles(args: PickArgs = {}): Promise<string[] | null> {
  const paths = await window.decibell.dialog.open(args);
  if (paths.length === 0) return null;
  return paths;
}

/// Open the save-as dialog. Returns the chosen absolute path or null
/// when the user cancelled.
export async function pickSavePath(args: {
  defaultPath?: string;
  filters?: { name: string; extensions: string[] }[];
} = {}): Promise<string | null> {
  return window.decibell.dialog.save(args);
}

/// File-extension filters the chat composer offers when picking
/// attachments. Matches tauri-client's tauri.conf.json file-association
/// list.
export const ATTACHMENT_FILTERS = [
  { name: "All Files", extensions: ["*"] },
  {
    name: "Images",
    extensions: ["png", "jpg", "jpeg", "gif", "webp", "bmp", "tiff", "avif"],
  },
  {
    name: "Videos",
    extensions: ["mp4", "webm", "mov", "mkv", "avi"],
  },
  {
    name: "Audio",
    extensions: ["mp3", "wav", "ogg", "m4a", "flac", "opus"],
  },
];
