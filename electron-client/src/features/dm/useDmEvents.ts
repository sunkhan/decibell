import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useDmStore } from "../../stores/dmStore";
import { useAuthStore } from "../../stores/authStore";
import type { MessageReceivedPayload } from "../../types";

// Pulls DMs out of the unified `message_received` bus event (the
// channel-message side is handled by useChatEvents). The native
// router emits the same event for both contexts; we filter by
// context === "dm" here. The "other user" in a conversation is the
// non-self side of the (sender, recipient) pair — useful when the
// message is the echo of one we sent ourselves.
export function useDmEvents() {
  useEffect(() => {
    const unlisten = listen<MessageReceivedPayload>("message_received", (event) => {
      const p = event.payload;
      if (p.context !== "dm") return;

      const localUsername = useAuthStore.getState().username;
      const otherUser =
        p.sender === localUsername ? p.recipient : p.sender;
      if (!otherUser) return;

      useDmStore.getState().addDmMessage(otherUser, {
        sender: p.sender,
        content: p.content,
        timestamp: p.timestamp,
      });
    });

    return () => {
      unlisten.then((fn) => fn());
    };
  }, []);
}
