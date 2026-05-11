// Whitelist of absolute file paths the renderer is allowed to stream
// via the decibell-file:// protocol. Each entry is keyed by an opaque
// random token (crypto.randomUUID); the renderer never sees the real
// path. Tokens are issued when the renderer registers a path it just
// learned about (file picker result, drag-drop with file.path, etc.)
// and dropped when the upload completes — so there's no long-lived
// "renderer can read /etc/passwd" surface.

import { randomUUID } from "node:crypto";

interface FileEntry {
  path: string;
  size: number;
  mime: string;
  name: string;
  /// When the entry was registered. Used by the periodic GC sweep so a
  /// crashed/forgotten upload doesn't leave an entry around forever.
  registeredAt: number;
}

const entries = new Map<string, FileEntry>();
// Stale-entry GC: 1 hour. Long enough that even a giant upload over a
// flaky connection still completes; short enough that a renderer crash
// mid-upload doesn't leak the path indefinitely.
const STALE_AFTER_MS = 60 * 60 * 1000;

export function registerFile(entry: Omit<FileEntry, "registeredAt">): string {
  const token = randomUUID();
  entries.set(token, { ...entry, registeredAt: Date.now() });
  return token;
}

export function unregisterFile(token: string): void {
  entries.delete(token);
}

export function lookupFile(token: string): FileEntry | null {
  return entries.get(token) ?? null;
}

export function clearAllFiles(): void {
  entries.clear();
}

/// Drop entries older than STALE_AFTER_MS. Cheap to call on a timer.
export function sweepStale(): void {
  const cutoff = Date.now() - STALE_AFTER_MS;
  for (const [token, entry] of entries) {
    if (entry.registeredAt < cutoff) {
      entries.delete(token);
    }
  }
}
