// Registry of attachment HTTP endpoints keyed by serverId. Populated
// by the renderer via IPC whenever a community auth response arrives;
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
