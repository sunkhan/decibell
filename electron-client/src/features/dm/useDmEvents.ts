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

      const isFromSelf = p.sender === localUsername;
      useDmStore.getState().addDmMessage(
        otherUser,
        {
          sender: p.sender,
          content: p.content,
          timestamp: p.timestamp,
          // id is set by central after insertDm; 0 means the packet
          // came from a pre-persistence server. The store handles
          // both — 0 is just ineligible for mark-read.
          id: p.id || undefined,
        },
        isFromSelf,
      );
    });

    // Server-truth conversation previews — fired once on login by
    // `request_dm_conversations` in useAuthEvents.
    const unlistenConv = listen<{
      conversations: {
        peer: string;
        lastMessageContent: string;
        lastMessageSender: string;
        lastMessageId: number;
        lastTimestamp: number;
        unreadCount: number;
      }[];
    }>("dm_conversations_received", (event) => {
      useDmStore.getState().hydrateConversations(event.payload.conversations);
    });

    // One page of messages for a specific peer — fired by
    // `request_dm_history` (DmChatPanel on mount + scroll-up).
    const unlistenHist = listen<{
      peer: string;
      messages: {
        id: number;
        sender: string;
        content: string;
        timestamp: number;
      }[];
      hasMore: boolean;
    }>("dm_history_received", (event) => {
      const { peer, messages, hasMore } = event.payload;
      useDmStore.getState().appendHistory(peer, messages, hasMore);
    });

    return () => {
      unlisten.then((fn) => fn());
      unlistenConv.then((fn) => fn());
      unlistenHist.then((fn) => fn());
    };
  }, []);
}
