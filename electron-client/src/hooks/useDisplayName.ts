import { useChatStore } from "../stores/chatStore";
import type { ServerMember } from "../types";

/// username → member, built once per roster array. Rosters are replaced (never
/// mutated) on every change, so keying on the array's identity keeps this
/// correct with no invalidation. Selectors run on every chatStore write for
/// every mounted row; an O(1) lookup here instead of a roster `find` is what
/// keeps a 150-row list cheap against a large community's member list.
const memberIndexes = new WeakMap<ServerMember[], Map<string, ServerMember>>();

export function memberIndex(list: ServerMember[]): Map<string, ServerMember> {
  let idx = memberIndexes.get(list);
  if (!idx) {
    idx = new Map();
    for (const m of list) idx.set(m.username, m);
    memberIndexes.set(list, idx);
  }
  return idx;
}

/// The member record for `username` in `serverId`'s loaded roster, if any.
export function findMember(
  membersByServer: Record<string, ServerMember[]>,
  serverId: string,
  username: string,
): ServerMember | undefined {
  const list = membersByServer[serverId];
  return list ? memberIndex(list).get(username) : undefined;
}

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
    return findMember(s.membersByServer, serverId, username)?.nickname || username;
  });
}

/// Non-reactive resolver for imperative call sites (event handlers, sorting).
export function resolveDisplayName(
  serverId: string | null | undefined,
  username: string,
): string {
  if (!serverId) return username;
  return (
    findMember(useChatStore.getState().membersByServer, serverId, username)?.nickname ||
    username
  );
}
