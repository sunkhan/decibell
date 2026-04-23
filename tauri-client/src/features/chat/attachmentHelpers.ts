import type { AttachmentKind } from "../../types";

export function kindFromMime(mime: string): AttachmentKind {
  const m = mime.toLowerCase();
  if (m.startsWith("image/")) return "image";
  if (m.startsWith("video/")) return "video";
  if (m.startsWith("audio/")) return "audio";
  return "document";
}

export function formatBytes(n: number): string {
  if (n <= 0) return "0 B";
  const units = ["B", "KB", "MB", "GB", "TB"];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) { v /= 1024; i += 1; }
  const decimals = v < 10 && i > 0 ? 1 : 0;
  return `${v.toFixed(decimals)} ${units[i]}`;
}

// Tauri's drag-and-drop gives us an OS file path. For the plain HTML file
// input we get a `File`; we use that to get a filename + mime and pass the
// resulting path via Tauri's save-to-temp if needed. For plain files picked
// from the native dialog (preferred), we already have the OS path.
export interface PickedFile {
  // Absolute path the Rust side can open. For the native Tauri picker this
  // comes straight from the dialog. For HTML drag-and-drop on Tauri v2, the
  // drop event payload includes `paths`.
  filePath: string;
  filename: string;
  mime: string;
  size: number;
}
