import { useEffect } from "react";
import { invoke } from "../../lib/ipc";

// Dedupes in-flight picture fetches across re-renders. Keyed by
// "<serverId>:<version>" so a new version triggers a fresh fetch.
// Module-level so it survives component remounts within a session.
const inflightFetches = new Set<string>();

/// Lazy-fetch a server picture the store doesn't hold yet. Central
/// serves any server's picture to any authenticated user (the same
/// public-fetch model as avatars), so this works for servers the user
/// hasn't joined — the browse directory and invite cards rely on it.
/// The bytes land through the `server_picture_received` event, which
/// only accepts them if `serverPictureVersions[serverId]` still equals
/// `version` — so callers register the version first.
export function useFetchServerPictureIfMissing(
  serverId: string,
  version: string,
  cachedDataUrl: string | undefined,
): void {
  useEffect(() => {
    if (!serverId || !version || cachedDataUrl) return;
    const key = `${serverId}:${version}`;
    if (inflightFetches.has(key)) return;
    inflightFetches.add(key);
    invoke("fetch_server_picture", { serverId: parseInt(serverId, 10) })
      .catch(console.error)
      .finally(() => inflightFetches.delete(key));
  }, [serverId, version, cachedDataUrl]);
}
