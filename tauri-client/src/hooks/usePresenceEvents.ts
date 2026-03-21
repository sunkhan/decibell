import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../stores/chatStore";

export function usePresenceEvents() {
  useEffect(() => {
    const unlisten = listen<{ onlineUsers: string[] }>(
      "user_list_updated",
      (event) => {
        useChatStore.getState().setOnlineUsers(event.payload.onlineUsers);
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
