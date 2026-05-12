import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useAvatarStore } from "../../stores/avatarStore";
import { useFriendsStore } from "../../stores/friendsStore";
import { useChatStore } from "../../stores/chatStore";
import type { UserPresence } from "../../types";

export function useFriendsEvents() {
  useEffect(() => {
    const unlistenList = listen<{
      friends: { username: string; status: string; avatarVersion: string }[];
    }>("friend_list_received", (event) => {
      const friends = event.payload.friends.map((f) => ({
        username: f.username,
        status: f.status as
          | "online"
          | "offline"
          | "pending_incoming"
          | "pending_outgoing"
          | "blocked",
      }));
      useFriendsStore.getState().setFriends(friends);
      // Feed every friend's known avatar version into the cache so
      // UserAvatar renders trigger fetches only when versions differ
      // from what we already have on disk for that user.
      const avatars = useAvatarStore.getState();
      for (const f of event.payload.friends) {
        avatars.setVersion(f.username, f.avatarVersion);
      }
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

    // Global online-user list update from central. Updates the chat
    // store's `onlineUsers` (used by DmSidebar dots) and reconciles
    // friend statuses (online/offline transitions) without touching
    // pending/blocked rows. Also primes the avatar cache with every
    // online user's current version.
    const unlistenPresence = listen<{ users: UserPresence[] }>(
      "user_list_updated",
      (event) => {
        const usernames = event.payload.users.map((u) => u.username);
        const onlineSet = new Set(usernames);
        useChatStore.getState().setOnlineUsers(usernames);

        const avatars = useAvatarStore.getState();
        for (const u of event.payload.users) {
          avatars.setVersion(u.username, u.avatarVersion);
        }

        const { friends, setFriends } = useFriendsStore.getState();
        if (friends.length === 0) return;
        const updated = friends.map((f) => {
          if (f.status === "online" || f.status === "offline") {
            const shouldBeOnline = onlineSet.has(f.username);
            if ((f.status === "online") !== shouldBeOnline) {
              return {
                ...f,
                status: shouldBeOnline ? ("online" as const) : ("offline" as const),
              };
            }
          }
          return f;
        });
        setFriends(updated);
      },
    );

    return () => {
      unlistenList.then((fn) => fn());
      unlistenAction.then((fn) => fn());
      unlistenPresence.then((fn) => fn());
    };
  }, []);
}
