// User statuses (docs/superpowers/specs/2026-10-11-user-status-design.md):
// central's presence → presenceStore, plus the friend rows' online/offline
// flag and the avatar cache, which both ride on presence.

import { useEffect } from "react";
import { invoke, listen } from "../../lib/ipc";
import { useAvatarStore } from "../../stores/avatarStore";
import { useDmStore } from "../../stores/dmStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { usePresenceStore } from "../../stores/presenceStore";
import type { UserPresence } from "../../types";
import { toUid, type Uid } from "../../stores/usersStore";

/// Flip accepted friends between online and offline to match presence,
/// leaving pending / blocked rows (and friends `isOnline` doesn't know
/// about: undefined) alone. Skips the store write (and every friends
/// subscriber's re-render) when nothing flipped.
function reconcileFriends(isOnline: (uid: Uid) => boolean | undefined) {
  const { friends, setFriends } = useFriendsStore.getState();
  if (friends.length === 0) return;
  let changed = false;
  const updated = friends.map((f) => {
    if (f.status !== "online" && f.status !== "offline") return f;
    const online = isOnline(f.uid);
    if (online === undefined || (f.status === "online") === online) return f;
    changed = true;
    return { ...f, status: online ? ("online" as const) : ("offline" as const) };
  });
  if (changed) setFriends(updated);
}

export function usePresenceEvents() {
  useEffect(() => {
    // A subscription answer: the newly watched users' presence (those not
    // in `users` are offline) and every one's picture: from the presence
    // entry, else from `avatars`, else none.
    const unlistenSnapshot = listen<{
      users: UserPresence[];
      covered?: (number | bigint)[];
      avatars?: { uid: number | bigint; version: string }[];
    }>(
      "user_list_updated",
      (event) => {
        const users = event.payload.users.map((u) => ({ ...u, uid: toUid(u.uid) }));
        const covered = (event.payload.covered ?? []).map(toUid);
        usePresenceStore.getState().applyCovered(covered, users);
        const versions = new Map<Uid, string>();
        for (const a of event.payload.avatars ?? []) versions.set(toUid(a.uid), a.version);
        for (const u of users) versions.set(u.uid, u.avatarVersion);
        useAvatarStore
          .getState()
          .setVersions(covered.map((uid) => ({ uid, version: versions.get(uid) ?? "" })));
        const online = new Set(users.map((u) => u.uid));
        const answered = new Set(covered);
        reconcileFriends((u) => (answered.has(u) ? online.has(u) : undefined));
      },
    );

    // One user's full current presence.
    const unlistenDelta = listen<{
      username: string;
      uid?: number | bigint;
      online: boolean;
      status: string;
      avatarVersion: string;
      statusText: string;
      statusEmoji: string;
    }>("presence_delta", (event) => {
      const { online, status, avatarVersion, statusText, statusEmoji } = event.payload;
      const uid = toUid(event.payload.uid);
      if (!(uid > 0)) return;
      usePresenceStore.getState().applyDelta(uid, online, status, statusText, statusEmoji);
      if (online) useAvatarStore.getState().setVersion(uid, avatarVersion);
      reconcileFriends((u) => (u === uid ? online : !!usePresenceStore.getState().statuses[u]));
    });

    // Our own saved status, just before login_succeeded. Also re-push the
    // friends-only DM setting: central keeps it on the account now, and
    // this client's local copy is what the user last chose.
    const unlistenOwn = listen<{
      supported: boolean;
      status: string;
      statusText: string;
      statusEmoji: string;
      statusClearIn: number;
    }>("own_presence", (event) => {
      const { supported, status, statusText, statusEmoji, statusClearIn } = event.payload;
      usePresenceStore
        .getState()
        .setOwnFromLogin(
          supported,
          status,
          statusText || statusEmoji ? { text: statusText, emoji: statusEmoji } : null,
          Number(statusClearIn) || 0,
        );
      invoke("set_dm_privacy", { friendsOnly: useDmStore.getState().friendsOnlyDms }).catch(
        console.error,
      );
    });

    const unlistenLost = listen<{ serverType: string }>("connection_lost", (event) => {
      if (event.payload.serverType === "central") usePresenceStore.getState().markDisconnected();
    });

    return () => {
      unlistenSnapshot.then((fn) => fn());
      unlistenDelta.then((fn) => fn());
      unlistenOwn.then((fn) => fn());
      unlistenLost.then((fn) => fn());
    };
  }, []);
}
