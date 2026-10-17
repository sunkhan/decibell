import * as fs from "fs";
import * as path from "path";
import { randomBytes } from "crypto";
import { app, safeStorage } from "electron";

// At-rest key for the native E2EE key store (native/src/e2ee/keystore.rs)
// and, through a derived subkey, the saved sign-in (native/src/config.rs).
// A random 32-byte key, generated once and kept in <userData>/e2ee/local.key
// wrapped by Electron's safeStorage (OS keychain: Keychain / DPAPI /
// libsecret-or-kwallet). Handed to the addon at init as base64; the addon
// falls back to its hostname+user derivation when this returns null (no
// keychain backend, or the wrapped file no longer opens — in which case the
// user simply re-enters their encryption passphrase and the store is
// rewritten under whichever key is available).
//
// Must run after app.whenReady(): safeStorage isn't usable before that.
export function loadOrCreateE2eeLocalKey(): string | null {
  try {
    if (!safeStorage.isEncryptionAvailable()) return null;
  } catch {
    return null;
  }
  const dir = path.join(app.getPath("userData"), "e2ee");
  const file = path.join(dir, "local.key");
  try {
    if (fs.existsSync(file)) {
      const wrapped = fs.readFileSync(file);
      const key = safeStorage.decryptString(wrapped);
      if (Buffer.from(key, "base64").length === 32) return key;
      console.warn("[e2ee] local.key has an unexpected length; ignoring it");
      return null;
    }
    const key = randomBytes(32).toString("base64");
    fs.mkdirSync(dir, { recursive: true });
    fs.writeFileSync(file, safeStorage.encryptString(key), { mode: 0o600 });
    return key;
  } catch (e) {
    console.warn("[e2ee] local.key unavailable:", e);
    return null;
  }
}

// Whether that key is actually guarded by an OS keychain. Linux's
// `basic_text` safeStorage backend wraps with a hard-coded password —
// obfuscation, not protection — so the saved sign-in doesn't count on it
// (native falls back to its derived key and logs that). Call after
// loadOrCreateE2eeLocalKey, which settles the backend.
export function localKeyProtected(): boolean {
  try {
    if (!safeStorage.isEncryptionAvailable()) return false;
    if (process.platform === "linux" && safeStorage.getSelectedStorageBackend() === "basic_text") {
      console.warn("[e2ee] safeStorage uses basic_text: the local key is not keychain-protected");
      return false;
    }
    return true;
  } catch {
    return false;
  }
}
