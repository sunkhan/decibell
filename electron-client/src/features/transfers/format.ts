import { formatFileSize } from "../chat/attachmentHelpers";

// Status-line formatting for the Transfers panel.

const UNITS: Array<[number, string]> = [
  [1024 * 1024 * 1024, "GB"],
  [1024 * 1024, "MB"],
  [1024, "KB"],
];

/// "18.4 of 44.0 MB" — both numbers in the total's unit.
export function formatProgress(done: number, total: number): string {
  if (total <= 0) return formatFileSize(done);
  const unit = UNITS.find(([size]) => total >= size);
  if (!unit) return `${done} of ${total} B`;
  const [size, label] = unit;
  return `${(done / size).toFixed(1)} of ${(total / size).toFixed(1)} ${label}`;
}

export function formatSpeed(bps: number): string {
  return `${formatFileSize(Math.round(bps))}/s`;
}

export function formatEta(seconds: number): string {
  if (seconds < 0) return "";
  if (seconds < 60) return `${Math.max(1, Math.round(seconds))} s left`;
  const min = Math.round(seconds / 60);
  if (min < 60) return `${min} min left`;
  const h = Math.floor(min / 60);
  const m = min % 60;
  return m > 0 ? `${h} h ${m} min left` : `${h} h left`;
}

export function formatAgo(epochMs: number, nowMs: number): string {
  const s = Math.max(0, (nowMs - epochMs) / 1000);
  if (s < 60) return "just now";
  if (s < 3600) return `${Math.floor(s / 60)} min ago`;
  if (s < 86400) return `${Math.floor(s / 3600)} h ago`;
  if (s < 2 * 86400) return "yesterday";
  return new Date(epochMs).toLocaleDateString(undefined, { month: "short", day: "numeric" });
}

/// Files that can run code when opened. Opening one from the panel asks
/// first — it came from another user.
const RISKY = new Set([
  "exe", "msi", "msix", "appx", "bat", "cmd", "com", "scr", "pif", "cpl", "ps1", "psm1",
  "vbs", "vbe", "js", "jse", "wsf", "wsh", "hta", "lnk", "reg", "jar", "sh", "bash",
  "zsh", "command", "app", "dmg", "pkg", "run", "bin", "appimage", "deb", "rpm",
  "desktop", "py", "pl", "rb",
]);

export function isRiskyFile(filename: string): boolean {
  const dot = filename.lastIndexOf(".");
  return dot >= 0 && RISKY.has(filename.slice(dot + 1).toLowerCase());
}
