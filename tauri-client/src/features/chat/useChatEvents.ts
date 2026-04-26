import { useEffect } from "react";
import { listen } from "@tauri-apps/api/event";
import { useChatStore } from "../../stores/chatStore";
import { useAttachmentsStore } from "../../stores/attachmentsStore";
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
    width: number;
    height: number;
    thumbnailSizeBytes: number;
    thumbnailSizesMask: number;
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
    width: a.width ?? 0,
    height: a.height ?? 0,
    thumbnailSizeBytes: a.thumbnailSizeBytes ?? 0,
    thumbnailSizesMask: a.thumbnailSizesMask ?? 0,
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
        attachments: (event.payload.attachments ?? []).map(mapAttachment),
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
          attachments: (m.attachments ?? []).map(mapAttachment),
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

    // --- attachment upload progress / complete / failed ---

    interface UploadProgressPayload {
      pendingId: string;
      serverId: string;
      channelId: string;
      attachmentId: number;
      filename: string;
      transferredBytes: number;
      totalBytes: number;
    }
    interface UploadCompletePayload {
      pendingId: string;
      serverId: string;
      channelId: string;
      attachmentId: number;
      filename: string;
      mime: string;
      kind: string;
      sizeBytes: number;
    }
    interface UploadFailedPayload {
      pendingId: string;
      serverId: string;
      channelId: string;
      attachmentId: number;
      filename: string;
      message: string;
      cancelled: boolean;
    }

    const unlistenUploadProgress = listen<UploadProgressPayload>(
      "attachment_upload_progress",
      (event) => {
        useAttachmentsStore.getState().updateProgress(
          event.payload.pendingId,
          event.payload.transferredBytes,
        );
      },
    );

    const unlistenUploadComplete = listen<UploadCompletePayload>(
      "attachment_upload_complete",
      (event) => {
        useAttachmentsStore.getState().markReady(
          event.payload.pendingId,
          event.payload.attachmentId,
          normalizeKind(event.payload.kind),
          event.payload.mime,
          event.payload.filename,
        );
      },
    );

    const unlistenUploadFailed = listen<UploadFailedPayload>(
      "attachment_upload_failed",
      (event) => {
        useAttachmentsStore.getState().markFailed(
          event.payload.pendingId,
          event.payload.message,
          event.payload.cancelled,
        );
      },
    );

    return () => {
      unlistenMsg.then((fn) => fn());
      unlistenHistory.then((fn) => fn());
      unlistenPruned.then((fn) => fn());
      unlistenUploadProgress.then((fn) => fn());
      unlistenUploadComplete.then((fn) => fn());
      unlistenUploadFailed.then((fn) => fn());
    };
  }, []);
}
