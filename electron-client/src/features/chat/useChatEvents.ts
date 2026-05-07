import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import type {
  Attachment,
  AttachmentKind,
  ChannelHistoryReceivedPayload,
  ChannelPrunedPayload,
  ChannelWipedPayload,
  MessageReceivedPayload,
} from "../../types";

function normalizeKind(kind: string): AttachmentKind {
  if (kind === "image" || kind === "video" || kind === "document" || kind === "audio") {
    return kind;
  }
  return "document";
}

function mapAttachment(
  a: MessageReceivedPayload["attachments"][number],
): Attachment {
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
    width: a.width ?? 0,
    height: a.height ?? 0,
    thumbnailSizeBytes: a.thumbnailSizeBytes ?? 0,
    thumbnailSizesMask: a.thumbnailSizesMask ?? 0,
    durationMs: a.durationMs ?? 0,
  };
}

// Wires up channel-message and channel-lifecycle events. DM messages
// (context === 'dm') are ignored — DM rendering is a later PR's concern.
export function useChatEvents() {
  useEffect(() => {
    const unlistenMsg = listen<MessageReceivedPayload>("message_received", (event) => {
      const p = event.payload;
      if (p.context === "dm") return;

      useChatStore.getState().addMessage({
        id: p.id,
        sender: p.sender,
        content: p.content,
        timestamp: p.timestamp,
        channelId: p.context,
        attachments: (p.attachments ?? []).map(mapAttachment),
        nonce: p.nonce || undefined,
      });
    });

    const unlistenHistory = listen<ChannelHistoryReceivedPayload>(
      "channel_history_received",
      (event) => {
        const { channelId, messages, hasMore } = event.payload;
        useChatStore.getState().prependHistory(
          channelId,
          messages.map((m) => ({
            id: m.id,
            sender: m.sender,
            content: m.content,
            timestamp: String(m.timestamp),
            channelId: m.channelId,
            attachments: (m.attachments ?? []).map(mapAttachment),
            nonce: m.nonce || undefined,
          })),
          hasMore,
        );
        useChatStore.getState().setHistoryLoading(channelId, false);
        useChatStore.getState().markHistoryFetched(channelId);
      },
    );

    const unlistenPruned = listen<ChannelPrunedPayload>(
      "channel_pruned",
      (event) => {
        useChatStore
          .getState()
          .applyChannelPruned(event.payload.channelId, event.payload.deletedMessageIds);
      },
    );

    const unlistenWiped = listen<ChannelWipedPayload>("channel_wiped", (event) => {
      useChatStore.getState().applyChannelWiped(event.payload.channelId);
    });

    return () => {
      unlistenMsg.then((fn) => fn());
      unlistenHistory.then((fn) => fn());
      unlistenPruned.then((fn) => fn());
      unlistenWiped.then((fn) => fn());
    };
  }, []);
}
