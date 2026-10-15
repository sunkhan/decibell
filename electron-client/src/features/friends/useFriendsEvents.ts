import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useAvatarStore } from "../../stores/avatarStore";
import { useFriendsStore } from "../../stores/friendsStore";

export function useFriendsEvents() {
  useEffect(() => {
    const unlistenList = listen<{
      friends: { username: string; uid?: number | bigint; status: string; avatarVersion: string }[];
    }>("friend_list_received", (event) => {
      const friends = event.payload.friends.map((f) => ({
        username: f.username,
        uid: Number(f.uid ?? 0) || 0,
        status: f.status as
          | "online"
          | "offline"
          | "pending_incoming"
          | "pending_outgoing"
          | "blocked",
        // Required by FriendInfo (was silently omitted → undefined at
        // runtime); the payload carries it and avatarStore uses it for
        // cache invalidation.
        avatarVersion: f.avatarVersion,
      }));
      useFriendsStore.getState().setFriends(friends);
      // Feed every friend's known avatar version into the cache so
      // UserAvatar renders trigger fetches only when versions differ
      // from what we already have on disk for that user.
      useAvatarStore.getState().setVersions(
        event.payload.friends.map((f) => ({ uid: Number(f.uid ?? 0) || 0, version: f.avatarVersion })),
      );
    });

    const unlistenAction = listen<{ success: boolean; message: string }>(
      "friend_action_responded",
      (event) => {
        const { success, message } = event.payload;
        if (!success) {
          useFriendsStore.getState().setLastActionError(message);
        } else {
          useFriendsStore.getState().setLastActionError(null);
        }
      },
    );

    return () => {
      unlistenList.then((fn) => fn());
      unlistenAction.then((fn) => fn());
    };
  }, []);
}
