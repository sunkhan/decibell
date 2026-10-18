import { useEffect } from "react";
import { useChatStore } from "../stores/chatStore";
import { nameOf, useUserName, type Uid } from "../stores/usersStore";
import { requestMember } from "../features/members/memberLookup";
import type { ServerMember } from "../types";

/// uid → member, built once per roster array. Rosters are replaced (never
/// mutated) on every change, so keying on the array's identity keeps this
/// correct with no invalidation. Selectors run on every chatStore write for
/// every mounted row; an O(1) lookup here instead of a roster `find` is what
/// keeps a 150-row list cheap against a large community's member list.
/// Keys are uids, including a community's negative placeholders (unique
/// within that roster).
const memberIndexes = new WeakMap<ServerMember[], Map<Uid, ServerMember>>();

export function memberIndex(list: ServerMember[]): Map<Uid, ServerMember> {
  let idx = memberIndexes.get(list);
  if (!idx) {
    idx = new Map();
    for (const m of list) if (m.uid) idx.set(m.uid, m);
    memberIndexes.set(list, idx);
  }
  return idx;
}

/// The member record for `uid` in `serverId`'s loaded roster, if any. The
/// roster pages in (proto "Roster protocol"): besides our own row, a big
/// community's members may not be loaded yet. Views that show a member
/// use useMember / useMemberInterest so a missing one is looked up.
export function findMember(
  membersByServer: Record<string, ServerMember[]>,
  serverId: string,
  uid: Uid,
): ServerMember | undefined {
  const list = membersByServer[serverId];
  return list && uid ? memberIndex(list).get(uid) : undefined;
}

/// Looks `uid` up in `serverId` when the roster lacks them (paging hasn't
/// reached them): a message author, a voice participant, a profile. Only
/// for uids the community itself named (features/members/memberLookup.ts).
export function useMemberInterest(serverId: string | null | undefined, uid: Uid): void {
  // The roster generation while the member is missing, else -1: a new
  // generation (a first page replaced the roster) asks again.
  const generation = useChatStore((s) => {
    if (!serverId || !uid) return -1;
    const meta = s.memberRosterMeta[serverId];
    if (!meta || meta.refetching) return -1;
    return findMember(s.membersByServer, serverId, uid) ? -1 : meta.generation;
  });
  useEffect(() => {
    if (serverId && uid && generation >= 0) requestMember(serverId, uid, generation);
  }, [serverId, uid, generation]);
}

/// `uid`'s member record in `serverId`, looked up when not loaded yet
/// (undefined meanwhile, and for someone who isn't a member).
export function useMember(serverId: string | null | undefined, uid: Uid): ServerMember | undefined {
  useMemberInterest(serverId, uid);
  return useChatStore((s) => (serverId && uid ? findMember(s.membersByServer, serverId, uid) : undefined));
}

/// The name shown for `uid` within a server: the member's server nickname
/// when set, otherwise their current name (`name` is the one the caller's
/// record carries, used until the users store knows better). Reactive —
/// re-renders on a nickname change or a rename. A null/undefined serverId
/// (DMs, no server context) skips the nickname. A member the roster hasn't
/// loaded is looked up, so their nickname shows once it arrives.
///
/// Identity-derived visuals (avatar image, letter/gradient colour) key on
/// the uid — only the visible name text uses this.
export function useDisplayName(serverId: string | null | undefined, uid: Uid, name: string): string {
  useMemberInterest(serverId, uid);
  const nickname = useChatStore((s) =>
    serverId ? findMember(s.membersByServer, serverId, uid)?.nickname : undefined,
  );
  const current = useUserName(uid, name);
  return nickname || current;
}

/// Non-reactive resolver for imperative call sites (event handlers, sorting).
export function resolveDisplayName(serverId: string | null | undefined, uid: Uid, name: string): string {
  const nickname = serverId
    ? findMember(useChatStore.getState().membersByServer, serverId, uid)?.nickname
    : undefined;
  return nickname || nameOf(uid, name);
}
