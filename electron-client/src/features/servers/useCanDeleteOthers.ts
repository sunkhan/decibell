import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";

/// Returns true if the local user has the "delete others' messages"
/// permission in the given server. Today: owner-only. When roles
/// ship, extend with: || hasRolePermission(serverId, "DELETE_MESSAGES").
///
/// Returns false if serverId is null (e.g. user is on the home or DM
/// view, not viewing a server).
export function useCanDeleteOthers(serverId: string | null): boolean {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  if (!serverId || !localUsername || !owner) return false;
  return owner === localUsername;
}
