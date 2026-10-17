import { invoke } from "../../lib/ipc";

// FRIEND_LIST_REQ, asked again when no list comes back (review B4). The
// reply can be lost, with central's connection or without it (native skips
// a frame over 2 MiB), and the friends list then stayed empty or stale
// until a restart.

const REPLY_TIMEOUT_MS = 10_000;
const ATTEMPTS = 3;

let received = 0;
let generation = 0;

/// useFriendsEvents, on every friend_list_received (asked for or pushed).
export function noteFriendListReceived(): void {
  received++;
}

/// Ask central for the friend list; up to three times while none arrives.
/// A newer call takes over from an older one's retries.
export function requestFriendList(): void {
  const gen = ++generation;
  const before = received;
  const attempt = (n: number) => {
    invoke("request_friend_list").catch(console.error);
    window.setTimeout(() => {
      if (gen !== generation || received > before || n + 1 >= ATTEMPTS) return;
      attempt(n + 1);
    }, REPLY_TIMEOUT_MS);
  };
  attempt(0);
}
