import type { Attachment, Message } from "../../../types";
import type { ViewerImage } from "../../../stores/imageViewerStore";
import { useChatStore } from "../../../stores/chatStore";
import { buildAttachmentUrl, pickThumbnailSize } from "../attachmentHelpers";
import { previewUrlFor } from "../attachmentPreviewUrl";

/// The message an image was posted in: the viewer's header (who, when,
/// where). Its key also tells apart one link posted in several messages.
export type ViewerSource = Pick<Message, "id" | "channelId" | "senderUid" | "sender" | "timestamp">;

export function attachmentViewerKey(serverId: string, attachmentId: number): string {
  return `a:${serverId}:${attachmentId}`;
}

/// The same link can sit in several messages: the message keeps each
/// thumbnail's key apart.
export function urlViewerKey(url: string, messageId = 0): string {
  return `u:${messageId}:${url}`;
}

export function viewerImageForAttachment(
  attachment: Attachment,
  serverId: string,
  source: ViewerSource | undefined,
): ViewerImage | null {
  const url = buildAttachmentUrl(serverId, attachment);
  if (!url) return null;
  const view = useChatStore.getState().chatViewSize;
  const small = pickThumbnailSize(attachment.thumbnailSizesMask, 160);
  return {
    key: attachmentViewerKey(serverId, Number(attachment.id)),
    url,
    previewUrl: previewUrlFor(attachment, serverId, view) ?? undefined,
    thumbUrl:
      attachment.mime !== "image/gif" && small !== null && attachment.thumbnailSizeBytes > 0
        ? buildAttachmentUrl(serverId, attachment, { thumb: true, size: small }) ?? undefined
        : url,
    placeholder: attachment.placeholder || undefined,
    filename: attachment.filename,
    width: attachment.width,
    height: attachment.height,
    sizeBytes: attachment.sizeBytes,
    mime: attachment.mime,
    serverId,
    attachmentId: Number(attachment.id),
    channelId: source?.channelId,
    senderUid: source?.senderUid,
    sender: source?.sender,
    timestamp: source?.timestamp,
  };
}

/// The message's images, in the order the sender attached them (owner
/// decision, 2026-10-28: the message only, as on Discord, never the whole
/// conversation), and where the clicked one sits among them.
export function galleryFor(
  serverId: string,
  source: ViewerSource | undefined,
  attachments: Attachment[],
  clickedId: number,
): { items: ViewerImage[]; index: number } {
  const images = attachments
    .filter((a) => a.kind === "image" && a.purgedAt === 0)
    .sort((a, b) => a.position - b.position);
  const items: ViewerImage[] = [];
  let index = 0;
  for (const att of images) {
    const item = viewerImageForAttachment(att, serverId, source);
    if (!item) continue;
    if (Number(att.id) === clickedId) index = items.length;
    items.push(item);
  }
  return { items, index };
}
