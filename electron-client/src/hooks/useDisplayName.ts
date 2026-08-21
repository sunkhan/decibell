import { useChatStore } from "../stores/chatStore";

/// Resolve a username to the name shown within a server: the member's server
/// nickname when set, otherwise the username itself. Reactive — re-renders when
/// the roster (or a nickname) changes. Pass a null/undefined serverId (DMs, or
/// no server context) to always get the plain username.
///
/// Identity-derived visuals (avatar image, letter/gradient color) must stay
/// keyed on the real username — only the visible name text uses this.
export function useDisplayName(
  serverId: string | null | undefined,
  username: string,
): string {
  return useChatStore((s) => {
    if (!serverId) return username;
    const m = s.membersByServer[serverId]?.find(
      (mm) => mm.username === username,
    );
    return m?.nickname || username;
  });
}

/// Non-reactive resolver for imperative call sites (event handlers, sorting).
export function resolveDisplayName(
  serverId: string | null | undefined,
  username: string,
): string {
  if (!serverId) return username;
  const m = useChatStore
    .getState()
    .membersByServer[serverId]?.find((mm) => mm.username === username);
  return m?.nickname || username;
}
