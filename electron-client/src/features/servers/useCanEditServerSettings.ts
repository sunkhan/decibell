import { useChatStore } from "../../stores/chatStore";
import { useAuthStore } from "../../stores/authStore";

/// Returns true if the local user is allowed to edit server-wide
/// settings for the given server. Today: owner-only. When roles
/// ship, this extends to:
///   || hasRolePermission(serverId, "EDIT_SERVER_SETTINGS").
///
/// Returns false if serverId is null (e.g. user is on the home or DM
/// view, not viewing a specific server).
export function useCanEditServerSettings(serverId: string | null): boolean {
  const localUsername = useAuthStore((s) => s.username);
  const owner = useChatStore((s) =>
    serverId ? s.serverOwner[serverId] : undefined,
  );
  if (!serverId || !localUsername || !owner) return false;
  return owner === localUsername;
}
