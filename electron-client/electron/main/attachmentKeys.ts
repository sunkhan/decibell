// Keys of encrypted-channel attachments, registered by the renderer as
// it decrypts messages (the key travels inside the message envelope) and
// consulted by every attachment fetch path in main — the custom
// protocol (images, save-as), the loopback media server (video/audio)
// and netFetch (save-as). Per session, never persisted.

import { ipcMain } from "electron";
import type { AttachmentKeyInfo } from "./attachmentCrypto";

const keys = new Map<string, AttachmentKeyInfo>();

function keyOf(serverId: string, attachmentId: string | number): string {
  return `${serverId}/${attachmentId}`;
}

export function getAttachmentKey(serverId: string, attachmentId: string | number): AttachmentKeyInfo | null {
  return keys.get(keyOf(serverId, attachmentId)) ?? null;
}

export function clearAttachmentKeys(serverId?: string): void {
  if (!serverId) {
    keys.clear();
    return;
  }
  for (const k of [...keys.keys()]) {
    if (k.startsWith(`${serverId}/`)) keys.delete(k);
  }
}

export interface RegisterKeyArgs {
  serverId: string;
  attachmentId: number;
  /// base64 32-byte key
  keyB64: string;
  chunkBytes: number;
  sizeBytes: number;
  mime: string;
  filename: string;
}

export function registerAttachmentKeyHandlers(): void {
  ipcMain.handle("decibell:attachments:registerKeys", (_e, entries: RegisterKeyArgs[]) => {
    if (!Array.isArray(entries)) return;
    for (const a of entries.slice(0, 200)) {
      if (typeof a.serverId !== "string" || typeof a.attachmentId !== "number") continue;
      const key = Buffer.from(String(a.keyB64 ?? ""), "base64");
      if (key.length !== 32) continue;
      const chunkBytes = Number(a.chunkBytes) || 0;
      if (chunkBytes <= 0 || chunkBytes > 16 * 1024 * 1024) continue;
      keys.set(keyOf(a.serverId, a.attachmentId), {
        key,
        chunkBytes,
        sizeBytes: Math.max(0, Number(a.sizeBytes) || 0),
        mime: String(a.mime || "application/octet-stream"),
        filename: String(a.filename || "attachment"),
      });
    }
  });
  ipcMain.handle("decibell:attachments:clearKeys", (_e, serverId?: string) => {
    clearAttachmentKeys(serverId);
  });
}
