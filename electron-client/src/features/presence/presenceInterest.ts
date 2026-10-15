// Presence by subscription (docs/superpowers/specs/2026-10-15-central-
// scaling-design.md). Every hook that shows a user's status or profile
// watches that user while mounted (ref-counted); the union of what's on
// screen goes to central, which then sends statuses and profile changes for
// those users only. Native keeps the last set and re-sends it after every
// central login, so a reconnect doesn't lose it.

import { useEffect } from "react";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import type { Uid } from "../../stores/usersStore";

/// Central's cap per connection (SessionManager::MAX_WATCH).
const MAX_WATCH = 5000;
/// At most one subscription request per interval: a list mounting or a
/// server switch changes many rows at once, and central rate-limits the
/// requests (10 burst, 2 per second).
const FLUSH_INTERVAL_MS = 600;

const counts = new Map<Uid, number>();
let timer: ReturnType<typeof setTimeout> | null = null;
let lastFlushAt = 0;
let lastSent = "";

function flush(): void {
  timer = null;
  lastFlushAt = Date.now();
  // Our own changes are ours already: no need to watch ourselves.
  const me = useAuthStore.getState().uid;
  const uids = [...counts.keys()]
    .filter((uid) => uid !== me)
    .sort((a, b) => a - b)
    .slice(0, MAX_WATCH);
  const key = uids.join(",");
  if (key === lastSent) return;
  lastSent = key;
  invoke("presence_subscribe", { uids }).catch((err) => {
    lastSent = "";  // retried with the next change
    console.error("presence_subscribe:", err);
  });
}

function schedule(): void {
  if (timer) return;
  timer = setTimeout(flush, Math.max(0, lastFlushAt + FLUSH_INTERVAL_MS - Date.now()));
}

export function retainPresence(uids: Iterable<Uid>): void {
  let changed = false;
  for (const uid of uids) {
    if (!(uid > 0)) continue;
    const n = counts.get(uid) ?? 0;
    counts.set(uid, n + 1);
    if (n === 0) changed = true;
  }
  if (changed) schedule();
}

export function releasePresence(uids: Iterable<Uid>): void {
  let changed = false;
  for (const uid of uids) {
    const n = counts.get(uid);
    if (n === undefined) continue;
    if (n <= 1) {
      counts.delete(uid);
      changed = true;
    } else {
      counts.set(uid, n - 1);
    }
  }
  if (changed) schedule();
}

/// Watch `uid` while the calling component is mounted.
export function usePresenceInterest(uid: Uid): void {
  useEffect(() => {
    if (!(uid > 0)) return;
    retainPresence([uid]);
    return () => releasePresence([uid]);
  }, [uid]);
}
