import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useDmStore } from "../../stores/dmStore";
import { useAuthStore } from "../../stores/authStore";
import { toast } from "../../stores/toastStore";
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

      // Central's error replies (friends-only recipient, persist failure)
      // come back as a self-DM with no id and our nonce: withdraw the
      // pending bubble and say why, rather than showing the error text
      // as a message of ours. (An older central sends them without the
      // nonce; those still render as before and the watchdog clears the
      // bubble.)
      if (isFromSelf && !p.id && p.nonce) {
        const dm = useDmStore.getState();
        const pending = dm.conversations[otherUser]?.messages.some(
          (m) => !m.id && m.nonce === p.nonce,
        );
        if (pending) {
          dm.removeDmMessageByNonce(otherUser, p.nonce);
          toast.error("Message not sent", p.content);
          return;
        }
      }

      useDmStore.getState().addDmMessage(
        otherUser,
        {
          sender: p.sender,
          content: p.content,
          timestamp: p.timestamp,
          nonce: p.nonce || undefined,
          // id is set by central after insertDm; 0 means the packet
          // came from a pre-persistence server. The store handles
          // both — 0 is just ineligible for mark-read.
          id: p.id || undefined,
          editedAt: p.editedAt || undefined,
          replyTo: p.replyTo || undefined,
          replyToSender: p.replyToSender || undefined,
          replyToContent: p.replyToContent || undefined,
          encrypted: p.encrypted || undefined,
          decryptError: p.decryptError || undefined,
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
        encrypted: boolean;
        decryptError: string;
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
        editedAt: number;
        replyTo: number;
        replyToSender: string;
        replyToContent: string;
        encrypted: boolean;
        decryptError: string;
      }[];
      hasMore: boolean;
      hasMoreAfter: boolean;
      aroundId: number;
      afterId: number;
    }>("dm_history_received", (event) => {
      const { peer, messages, hasMore, hasMoreAfter, aroundId, afterId } = event.payload;
      const dm = useDmStore.getState();
      // Route by the request mode the server echoed (mirrors useChatEvents):
      //  aroundId>0 → jump context window → replace the loaded slice.
      //  afterId>0  → downward pagination → append newer.
      //  both 0     → older page / most-recent → prepend (existing path).
      if (aroundId > 0) {
        dm.setDmWindow(peer, messages, hasMore, hasMoreAfter);
      } else if (afterId > 0) {
        dm.appendNewerDm(peer, messages, hasMoreAfter);
      } else {
        dm.appendHistory(peer, messages, hasMore);
      }
    });

    const unlistenDmDeleteRes = listen<{
      success: boolean;
      message: string;
      peer: string;
      messageId: number;
    }>("dm_message_delete_responded", (event) => {
      const p = event.payload;
      const dm = useDmStore.getState();
      if (!p.success) {
        dm.restorePendingDmDeletion(p.peer, p.messageId);
        toast.error(
          "Couldn't delete message",
          p.message || "Server rejected the request.",
        );
        return;
      }
      dm.clearPendingDmDeletion(p.peer, p.messageId);
    });

    const unlistenDmDeleted = listen<{
      peer: string;
      messageId: number;
      deletedAt: number;
    }>("dm_message_deleted", (event) => {
      const { peer, messageId } = event.payload;
      const dm = useDmStore.getState();
      dm.removeDmMessage(peer, messageId);
      dm.clearPendingDmDeletion(peer, messageId);
    });

    // Edit is non-optimistic (matches DM sends): the broadcast applies it,
    // _edit_responded only surfaces failures.
    const unlistenDmEditRes = listen<{
      success: boolean;
      message: string;
      peer: string;
      messageId: number;
    }>("dm_message_edit_responded", (event) => {
      const p = event.payload;
      if (!p.success) {
        toast.error("Couldn't edit message", p.message || "Server rejected the request.");
      }
    });

    const unlistenDmEdited = listen<{
      peer: string;
      messageId: number;
      content: string;
      editedAt: number;
      encrypted: boolean;
      decryptError: string;
    }>("dm_message_edited", (event) => {
      const { peer, messageId, content, editedAt, encrypted, decryptError } = event.payload;
      useDmStore
        .getState()
        .applyDmEdit(peer, messageId, content, editedAt, encrypted, decryptError);
    });

    return () => {
      unlisten.then((fn) => fn());
      unlistenConv.then((fn) => fn());
      unlistenHist.then((fn) => fn());
      unlistenDmDeleteRes.then((fn) => fn());
      unlistenDmDeleted.then((fn) => fn());
      unlistenDmEditRes.then((fn) => fn());
      unlistenDmEdited.then((fn) => fn());
    };
  }, []);
}
