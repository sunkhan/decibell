import { invoke } from "../../lib/ipc";
import type { Uid } from "../../stores/usersStore";

/// Member lookups ("Roster protocol" in proto/messages.proto): the roster
/// pages in as the sidebar scrolls, so a big community's roster lacks most
/// members at first. Views that show one of them anyway (a message author,
/// a voice participant, a profile; see useMember / useMemberInterest in
/// hooks/useDisplayName.ts) ask for that member by uid. Requests are
/// batched per server; each uid is asked once per roster generation (every
/// first page replaces the roster and takes a new one), and the answer stays
/// current through the deltas every session gets. Only for uids the
/// community itself named: a lookup tells it whom we're looking at, so the
/// friend list (InviteFriendsModal) never goes through here.

const BATCH = 200; // the server's cap per lookup
const DELAY_MS = 50; // gathers one render's worth of rows into one request
const RETRY_MS = 1500;
const MAX_TRIES = 3;

interface Asked {
  generation: number;
  uids: Set<Uid>;
  tries: Map<Uid, number>;
}

const asked = new Map<string, Asked>();
const queued = new Map<string, Set<Uid>>();
let timer: ReturnType<typeof setTimeout> | null = null;

function askedFor(serverId: string, generation: number): Asked {
  let a = asked.get(serverId);
  if (!a || a.generation !== generation) {
    a = { generation, uids: new Set(), tries: new Map() };
    asked.set(serverId, a);
  }
  return a;
}

/// Ask for `uid`'s member record in `serverId` (no-op if already asked
/// under this roster generation).
export function requestMember(serverId: string, uid: Uid, generation: number): void {
  if (!uid) return;
  const a = askedFor(serverId, generation);
  if (a.uids.has(uid)) return;
  a.uids.add(uid);
  let q = queued.get(serverId);
  if (!q) queued.set(serverId, (q = new Set()));
  q.add(uid);
  if (!timer) timer = setTimeout(flush, DELAY_MS);
}

function flush(): void {
  timer = null;
  for (const [serverId, uids] of queued) {
    const list = [...uids];
    for (let i = 0; i < list.length; i += BATCH) {
      const batch = list.slice(i, i + BATCH);
      // Not sent (the connection is gone): the reconnect's first page
      // starts a new generation, which asks again.
      invoke("list_members", { serverId, uids: batch }).catch((err) => console.error("member lookup:", err));
    }
  }
  queued.clear();
}

/// The server refused a lookup (rate limited): ask again shortly, a few
/// times at most.
export function lookupRefused(serverId: string, uids: Uid[]): void {
  const a = asked.get(serverId);
  if (!a) return;
  const again: Uid[] = [];
  for (const uid of uids) {
    if (!a.uids.delete(uid)) continue;
    const n = (a.tries.get(uid) ?? 0) + 1;
    a.tries.set(uid, n);
    if (n < MAX_TRIES) again.push(uid);
  }
  if (again.length === 0) return;
  const generation = a.generation;
  setTimeout(() => {
    for (const uid of again) requestMember(serverId, uid, generation);
  }, RETRY_MS);
}
