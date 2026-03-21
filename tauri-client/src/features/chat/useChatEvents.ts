import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";

interface MessagePayload {
  context: string;
  sender: string;
  content: string;
  timestamp: string;
}

export function useChatEvents() {
  useEffect(() => {
    const unlisten = listen<MessagePayload>("message_received", (event) => {
      useChatStore.getState().addMessage({
        sender: event.payload.sender,
        content: event.payload.content,
        timestamp: event.payload.timestamp,
        channelId: event.payload.context,
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
