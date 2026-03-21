import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useFriendsStore } from "../../stores/friendsStore";

export function useFriendsEvents() {
  useEffect(() => {
    const unlistenList = listen<{ friends: { username: string; status: string }[] }>(
      "friend_list_received",
      (event) => {
        const friends = event.payload.friends.map((f) => ({
          username: f.username,
          status: f.status as "online" | "offline" | "pending_incoming" | "pending_outgoing" | "blocked",
        }));
        useFriendsStore.getState().setFriends(friends);
      }
    );

    const unlistenAction = listen<{ success: boolean; message: string }>(
      "friend_action_responded",
      (event) => {
        const { success, message } = event.payload;
        if (!success) {
          useFriendsStore.getState().setLastActionError(message);
        } else {
          useFriendsStore.getState().setLastActionError(null);
        }
      }
    );

    return () => {
      unlistenList.then((fn) => fn());
      unlistenAction.then((fn) => fn());
    };
  }, []);
}
