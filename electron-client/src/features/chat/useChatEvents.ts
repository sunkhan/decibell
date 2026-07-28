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

/// TEMPORARY diagnostic (dev builds only) for the ThumbHash rollout.
/// Distinguishes the three ways a placeholder can fail to appear:
///   key absent      → the native .node is stale (the Rust payload
///                     struct predates the field). Rebuild with
///                     `npm run build:native:debug`.
///   key present, "" → the server has no hash for this row: either the
///                     uploader didn't compute one, or the community
///                     server didn't store/echo it.
///   key present, 28 → everything upstream works; the problem is
///                     downstream in rendering.
/// Remove once the rollout is confirmed.
let loggedAttachmentShape = false;
function logAttachmentShape(a: Record<string, unknown>): void {
  if (loggedAttachmentShape || !import.meta.env.DEV) return;
  loggedAttachmentShape = true;
  const has = Object.prototype.hasOwnProperty.call(a, "placeholder");
  const val = a.placeholder;
  // eslint-disable-next-line no-console
  console.log(
    `[thumbhash] first attachment received — key present: ${has}; ` +
      `length: ${typeof val === "string" ? val.length : "n/a"}; ` +
      `verdict: ${
        !has
          ? "STALE NATIVE MODULE — rebuild native"
          : typeof val === "string" && val.length > 0
            ? "OK, hash arrived"
            : "SERVER HAS NO HASH for this attachment"
      }`,
    a,
  );
}

function mapAttachment(
  a: MessageReceivedPayload["attachments"][number],
): Attachment {
  logAttachmentShape(a as unknown as Record<string, unknown>);
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
    placeholder: a.placeholder ?? "",
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
