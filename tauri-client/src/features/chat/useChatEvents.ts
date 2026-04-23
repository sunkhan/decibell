import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";
import type { Attachment, AttachmentKind } from "../../types";

interface MessagePayload {
  context: string;
  sender: string;
  content: string;
  timestamp: string;
  id: number;
  attachments: Array<{
    id: number;
    messageId: number;
    kind: string;
    filename: string;
    mime: string;
    sizeBytes: number;
    url: string;
    position: number;
    createdAt: number;
    purgedAt: number;
  }>;
}

interface HistoryPayload {
  serverId: string;
  channelId: string;
  messages: Array<{
    id: number;
    sender: string;
    channelId: string;
    content: string;
    timestamp: number;
    attachments: MessagePayload["attachments"];
  }>;
  hasMore: boolean;
}

interface PrunedPayload {
  serverId: string;
  channelId: string;
  deletedMessageIds: number[];
  purgedAttachments: Array<{ attachmentId: number; purgedAt: number }>;
}

function normalizeKind(kind: string): AttachmentKind {
  if (kind === "image" || kind === "video" || kind === "document" || kind === "audio") return kind;
  // Default to document for unknown; shouldn't happen at runtime once the
  // server is the only producer of this enum.
  return "document";
}

function mapAttachment(a: MessagePayload["attachments"][number]): Attachment {
  return {
    id: a.id,
    messageId: a.messageId,
    kind: normalizeKind(a.kind),
    filename: a.filename,
    mime: a.mime,
    sizeBytes: a.sizeBytes,
    url: a.url,
    position: a.position,
    createdAt: a.createdAt,
    purgedAt: a.purgedAt,
  };
}

export function useChatEvents() {
  useEffect(() => {
    const unlistenMsg = listen<MessagePayload>("message_received", (event) => {
      if (event.payload.context === "dm") return;
      useChatStore.getState().addMessage({
        id: event.payload.id,
        sender: event.payload.sender,
        content: event.payload.content,
        timestamp: event.payload.timestamp,
        channelId: event.payload.context,
        attachments: event.payload.attachments.map(mapAttachment),
      });
    });

    const unlistenHistory = listen<HistoryPayload>("channel_history_received", (event) => {
      const { channelId, messages, hasMore } = event.payload;
      useChatStore.getState().prependHistory(
        channelId,
        messages.map((m) => ({
          id: m.id,
          sender: m.sender,
          content: m.content,
          timestamp: String(m.timestamp),
          channelId: m.channelId,
          attachments: m.attachments.map(mapAttachment),
        })),
        hasMore,
      );
      useChatStore.getState().setHistoryLoading(channelId, false);
    });

    const unlistenPruned = listen<PrunedPayload>("channel_pruned", (event) => {
      useChatStore.getState().applyChannelPruned(
        event.payload.channelId,
        event.payload.deletedMessageIds,
        event.payload.purgedAttachments,
      );
    });

    return () => {
      unlistenMsg.then((fn) => fn());
      unlistenHistory.then((fn) => fn());
      unlistenPruned.then((fn) => fn());
    };
  }, []);
}
