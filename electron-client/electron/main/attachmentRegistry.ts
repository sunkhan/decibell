// Registry of attachment HTTP endpoints keyed by serverId. Populated
// by the renderer via IPC whenever a community auth response arrives,
// refreshed here when central renews the token;
// consumed by the decibell-attachment:// protocol handler so the
// renderer can use a plain <img src="decibell-attachment://…"> tag
// that gets transparently authenticated server-side.
//
// Also serves as a one-stop look-up for `decibell:net:fetch` requests
// against an attachment endpoint, so the renderer doesn't have to
// thread the JWT through every upload call.

interface AttachmentTarget {
  host: string;
  port: number;
  jwt: string;
  /// sha256-hex of the certificate native accepted on the chat connection
  /// to this community (the attachment listener shares the certificate).
  /// setCertificateVerifyProc pins the HTTPS host to it. "" = unknown.
  certFingerprint?: string;
}

const targets = new Map<string, AttachmentTarget>();

export function setAttachmentTarget(serverId: string, target: AttachmentTarget): void {
  targets.set(serverId, target);
}

export function clearAttachmentTarget(serverId: string): void {
  targets.delete(serverId);
}

export function clearAllAttachmentTargets(): void {
  targets.clear();
}

export function getAttachmentTarget(serverId: string): AttachmentTarget | null {
  return targets.get(serverId) ?? null;
}

/// Central renewed the session token (native `session_token_updated`):
/// re-read every registered community's target from native, which holds
/// the newest token. Tokens live 24 h and are renewed every 6; a registry
/// that kept the token from community auth 401'd every attachment a day
/// into a connection (review B2). An entry cleared meanwhile (logout,
/// leave) stays cleared; a failed read keeps the current entry.
export async function refreshAttachmentTargets(
  read: (serverId: string) => Promise<AttachmentTarget | null>,
): Promise<void> {
  await Promise.all(
    [...targets.keys()].map(async (serverId) => {
      const fresh = await read(serverId).catch(() => null);
      if (fresh && targets.has(serverId)) targets.set(serverId, fresh);
    }),
  );
}

/// Pinning verdict for a TLS connection Chromium is about to trust, given
/// the hostname and the sha256-hex of the presented leaf certificate:
///  - "match":   a registered community on that host presented this cert
///  - "mismatch": the host is a registered community but the cert differs
///  - "unknown": not one of our communities (leave it to Chromium)
export function verifyHostFingerprint(
  hostname: string,
  fingerprintHex: string,
): "match" | "mismatch" | "unknown" {
  const host = hostname.toLowerCase();
  let sawHost = false;
  for (const t of targets.values()) {
    if (t.host.toLowerCase() !== host) continue;
    sawHost = true;
    if (t.certFingerprint && t.certFingerprint.toLowerCase() === fingerprintHex) {
      return "match";
    }
  }
  return sawHost ? "mismatch" : "unknown";
}
