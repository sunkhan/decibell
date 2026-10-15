import { useCallback, useEffect, useRef, useState } from "react";
import { invoke, listen } from "../../../lib/ipc";
import type {
  MemberRemovePayload,
  MemberSearchReceivedPayload,
  MemberUpsertPayload,
  ServerMember,
} from "../../../types";
import { toUid } from "../../../stores/usersStore";

/// 0 = any role, > 0 = holds that role, -1 = holds no role (wire contract).
export type RoleFilter = number;

/// Typing settles for this long before a search goes out. The community
/// rate-limits list fetches to 20 burst / 4 per s per session.
const DEBOUNCE_MS = 250;
const PAGE = 100;
/// A community that predates MEMBER_SEARCH_REQ ignores it; say so rather
/// than spin forever.
const NO_ANSWER_MS = 8000;
export const NO_ANSWER =
  "The server didn't answer. It may be running an older version that can't search members yet.";

/// Seqs are unique across every open search (several can be live at once:
/// the Members tab, a role's holders, the add-members picker), and each
/// one only takes answers to its own latest request.
let nextSeq = 1;

interface SearchState {
  members: ServerMember[];
  total: number;
  hasMore: boolean;
  nextAfter: string;
  /// A request is in flight.
  loading: boolean;
  /// The current query has answered at least once.
  loaded: boolean;
  error: string | null;
}

const EMPTY: SearchState = {
  members: [],
  total: 0,
  hasMore: false,
  nextAfter: "",
  loading: true,
  loaded: false,
  error: null,
};

const matches = (m: ServerMember, query: string, roleId: RoleFilter) => {
  if (roleId === -1 && m.roleIds.length > 0) return false;
  if (roleId > 0 && !m.roleIds.includes(roleId)) return false;
  if (!query) return true;
  const q = query.toLowerCase();
  return m.username.toLowerCase().includes(q) || m.nickname.toLowerCase().includes(q);
};

/// Server-side member search (MEMBER_SEARCH_REQ): every member whose name
/// or nickname contains `query` and who matches `roleId`, online or not,
/// by username, 100 per page. Separate from the roster, so results never
/// reach the member sidebar. Roster deltas keep the loaded pages live: a
/// changed member is replaced, dropped once they stop matching, and slots
/// in when they start to (if they sort inside what's loaded).
export function useMemberSearch(serverId: string, query: string, roleId: RoleFilter) {
  const [state, setState] = useState<SearchState>(EMPTY);
  const [applied, setApplied] = useState(() => query.trim());
  const seqRef = useRef(0);
  const answeredRef = useRef(0);
  const filterRef = useRef({ query: applied, roleId });
  filterRef.current = { query: applied, roleId };

  // Debounce the text; a role change applies at once.
  useEffect(() => {
    const q = query.trim();
    if (q === applied) return;
    const t = setTimeout(() => setApplied(q), DEBOUNCE_MS);
    return () => clearTimeout(t);
  }, [query, applied]);

  useEffect(() => {
    const un = listen<MemberSearchReceivedPayload>("member_search_received", (event) => {
      const p = event.payload;
      if (p.serverId !== serverId || p.seq !== seqRef.current) return;
      answeredRef.current = p.seq;
      if (!p.success) {
        setState((s) => ({ ...s, loading: false, loaded: true, error: p.message || "Couldn't search members." }));
        return;
      }
      setState((s) => ({
        members: p.firstPage ? p.members : [...s.members, ...p.members],
        total: Number(p.totalMatches),
        hasMore: p.hasMore,
        nextAfter: p.nextAfter,
        loading: false,
        loaded: true,
        error: null,
      }));
    });
    const unUp = listen<MemberUpsertPayload>("member_upsert", (event) => {
      const { serverId: sid, member } = event.payload;
      if (sid !== serverId) return;
      const { query: q, roleId: r } = filterRef.current;
      setState((s) => {
        if (!s.loaded) return s;
        const idx = s.members.findIndex((m) => m.uid === member.uid);
        const fits = matches(member, q, r);
        if (idx >= 0) {
          const members = s.members.slice();
          if (fits) members[idx] = member;
          else members.splice(idx, 1);
          return { ...s, members, total: fits ? s.total : Math.max(0, s.total - 1) };
        }
        if (!fits) return s;
        // Only inside the loaded range, so paging leaves no gap (pages run
        // in name order: the server's cursor is the member's name).
        const last = s.members[s.members.length - 1];
        if (s.hasMore && last && member.username > last.username) return s;
        const at = s.members.findIndex((m) => m.username > member.username);
        const members = s.members.slice();
        members.splice(at < 0 ? members.length : at, 0, member);
        return { ...s, members, total: s.total + 1 };
      });
    });
    const unRm = listen<MemberRemovePayload>("member_remove", (event) => {
      const { serverId: sid } = event.payload;
      const uid = toUid(event.payload.uid);
      if (sid !== serverId) return;
      setState((s) => {
        if (!s.members.some((m) => m.uid === uid)) return s;
        return { ...s, members: s.members.filter((m) => m.uid !== uid), total: Math.max(0, s.total - 1) };
      });
    });
    return () => {
      un.then((fn) => fn());
      unUp.then((fn) => fn());
      unRm.then((fn) => fn());
    };
  }, [serverId]);

  const send = useCallback(
    (after: string) => {
      const seq = after ? seqRef.current : (seqRef.current = nextSeq++);
      answeredRef.current = 0;
      invoke("search_members", { serverId, query: applied, roleId, after, limit: PAGE, seq }).catch((err) => {
        if (seq === seqRef.current) setState((s) => ({ ...s, loading: false, loaded: true, error: String(err) }));
      });
      setTimeout(() => {
        if (seq === seqRef.current && answeredRef.current !== seq) {
          setState((s) => (s.loading ? { ...s, loading: false, loaded: true, error: NO_ANSWER } : s));
        }
      }, NO_ANSWER_MS);
    },
    [serverId, applied, roleId],
  );

  // A new search keeps the old rows on screen until its answer lands.
  useEffect(() => {
    setState((s) => ({ ...s, loading: true }));
    send("");
  }, [send]);

  const loadMore = useCallback(() => {
    if (!state.hasMore || state.loading) return;
    setState((s) => ({ ...s, loading: true }));
    send(state.nextAfter);
  }, [state.hasMore, state.loading, state.nextAfter, send]);

  /// The typed text hasn't been searched yet.
  const settling = query.trim() !== applied;
  return { ...state, settling, loadMore };
}
