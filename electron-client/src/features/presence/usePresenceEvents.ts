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

/// Flip accepted friends between online and offline to match presence,
/// leaving pending / blocked rows alone. Skips the store write (and every
/// friends subscriber's re-render) when nothing flipped.
function reconcileFriends(isOnline: (username: string) => boolean) {
  const { friends, setFriends } = useFriendsStore.getState();
  if (friends.length === 0) return;
  let changed = false;
  const updated = friends.map((f) => {
    if (f.status !== "online" && f.status !== "offline") return f;
    const online = isOnline(f.username);
    if ((f.status === "online") === online) return f;
    changed = true;
    return { ...f, status: online ? ("online" as const) : ("offline" as const) };
  });
  if (changed) setFriends(updated);
}

export function usePresenceEvents() {
  useEffect(() => {
    // Full snapshot: at login (and on every change from an older central).
    const unlistenSnapshot = listen<{ users: UserPresence[] }>("user_list_updated", (event) => {
      const { users } = event.payload;
      usePresenceStore.getState().applySnapshot(users);
      useAvatarStore
        .getState()
        .setVersions(users.map((u) => ({ username: u.username, version: u.avatarVersion })));
      const online = new Set(users.map((u) => u.username));
      reconcileFriends((u) => online.has(u));
    });

    // One user's full current presence.
    const unlistenDelta = listen<{
      username: string;
      online: boolean;
      status: string;
      avatarVersion: string;
      statusText: string;
      statusEmoji: string;
    }>("presence_delta", (event) => {
      const { username, online, status, avatarVersion, statusText, statusEmoji } = event.payload;
      usePresenceStore.getState().applyDelta(username, online, status, statusText, statusEmoji);
      if (online) useAvatarStore.getState().setVersion(username, avatarVersion);
      reconcileFriends((u) => (u === username ? online : !!usePresenceStore.getState().statuses[u]));
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
