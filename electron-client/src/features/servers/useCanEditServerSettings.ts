import { PERM, usePermission } from "./permissions";

/// Returns true if the local user is allowed to edit server-wide
/// settings (name/description/picture) for the given server: the
/// owner, or any role holding MANAGE_SERVER. Mirrors the server-side
/// gate on UPDATE_SERVER_PICTURE_REQ.
///
/// Returns false if serverId is null (e.g. user is on the home or DM
/// view, not viewing a specific server).
export function useCanEditServerSettings(serverId: string | null): boolean {
  return usePermission(serverId, PERM.MANAGE_SERVER);
}
