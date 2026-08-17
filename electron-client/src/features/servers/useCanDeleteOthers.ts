import { PERM, usePermission } from "./permissions";

/// Returns true if the local user may delete other members' messages in
/// the given server: the owner, or any role holding MANAGE_MESSAGES.
/// Mirrors the server-side gate (CommunityDb::can_delete_others).
///
/// Returns false if serverId is null (e.g. user is on the home or DM
/// view, not viewing a server).
export function useCanDeleteOthers(serverId: string | null): boolean {
  return usePermission(serverId, PERM.MANAGE_MESSAGES);
}
