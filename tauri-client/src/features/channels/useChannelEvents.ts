import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";

interface JoinChannelPayload {
  serverId: string;
  success: boolean;
  channelId: string;
  activeUsers: string[];
}

export function useChannelEvents() {
  useEffect(() => {
    const unlisten = listen<JoinChannelPayload>(
      "join_channel_responded",
      (event) => {
        const { success, channelId, activeUsers } = event.payload;
        if (success) {
          useChatStore.getState().setChannelMembers(channelId, activeUsers);
        }
      }
    );

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
