import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useDmStore } from "../../stores/dmStore";
import { useAuthStore } from "../../stores/authStore";

interface MessagePayload {
  context: string;
  sender: string;
  recipient: string;
  content: string;
  timestamp: string;
}

export function useDmEvents() {
  useEffect(() => {
    const unlisten = listen<MessagePayload>("message_received", (event) => {
      if (event.payload.context !== "dm") return;

      const localUsername = useAuthStore.getState().username;
      const otherUser =
        event.payload.sender === localUsername
          ? event.payload.recipient
          : event.payload.sender;

      if (!otherUser) return;

      useDmStore.getState().addDmMessage(otherUser, {
        sender: event.payload.sender,
        content: event.payload.content,
        timestamp: event.payload.timestamp,
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
