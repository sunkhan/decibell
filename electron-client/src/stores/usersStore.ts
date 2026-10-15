import { create } from "zustand";

// Users by uid (docs/superpowers/specs/2026-10-15-uid-identity-design.md).
// Every store keys people on their central account uid; names are only what
// gets drawn. This store is where a uid's current name comes from: fed by
// every payload that carries a name with its uid (features/users/
// userDirectory.ts) and by renames, so a rename changes one entry here and
// nothing else.
//
// Only real accounts (uid > 0) live here. A community's placeholder for a
// name it never saw with a uid is negative and only unique within that
// community, so callers draw those from the member record they came with.

/// A central account uid. 0 = no account (system messages, deleted
/// senders); negative = a community's placeholder for a legacy name.
export type Uid = number;

/// A uid from a payload field (i64 fields can arrive as BigInt); 0 when absent.
export function toUid(v: number | bigint | null | undefined): Uid {
  const n = Number(v ?? 0);
  return Number.isSafeInteger(n) ? n : 0;
}

interface UsersState {
  /// uid → the latest name the servers sent for it.
  names: Map<Uid, string>;
  /// Record (uid, name) pairs; one notify for the batch, none when nothing changed.
  remember: (pairs: Iterable<readonly [Uid, string | undefined]>) => void;
}

export const useUsersStore = create<UsersState>((set, get) => ({
  names: new Map(),
  remember: (pairs) => {
    const cur = get().names;
    let next: Map<Uid, string> | null = null;
    for (const [uid, name] of pairs) {
      if (!(uid > 0) || !name) continue;
      if ((next ?? cur).get(uid) === name) continue;
      if (!next) next = new Map(cur);
      next.set(uid, name);
    }
    if (next) set({ names: next });
  },
}));

/// The current name of `uid`, or `fallback` (the name a payload carried)
/// while the servers haven't told us one. Non-reactive.
export function nameOf(uid: Uid | null | undefined, fallback = ""): string {
  if (uid === null || uid === undefined || !(uid > 0)) return fallback;
  return useUsersStore.getState().names.get(uid) ?? fallback;
}

/// Reactive `nameOf`: re-renders when the user is renamed.
export function useUserName(uid: Uid | null | undefined, fallback = ""): string {
  return useUsersStore((s) => (uid && uid > 0 ? s.names.get(uid) : undefined) ?? fallback);
}

/// The account currently named `name`, as far as the servers have said.
/// Only for names saved before uids (old per-user volume settings);
/// everything else carries the uid.
export function uidNamed(name: string): Uid | undefined {
  for (const [uid, n] of useUsersStore.getState().names) if (n === name) return uid;
  return undefined;
}
