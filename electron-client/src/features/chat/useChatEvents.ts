import { useEffect } from "react";
import { listen } from "../../lib/ipc";
import { useChatStore } from "../../stores/chatStore";
import { toast } from "../../stores/toastStore";
import { noteTypedRejection } from "./sendPacing";
import type {
  Attachment,
  AttachmentKind,
  ChannelHistoryReceivedPayload,
  ChannelMessageRejectedPayload,
  ChannelPrunedPayload,
  ChannelWipedPayload,
  EncryptedAttachmentMetaPayload,
  MessageReceivedPayload,
} from "../../types";

function normalizeKind(kind: string): AttachmentKind {
  if (kind === "image" || kind === "video" || kind === "document" || kind === "audio") {
    return kind;
  }
  return "document";
}

// Wire Attachment.Kind numbers (0 image, 1 video, 2 document, 3 audio) →
// renderer kind names, for the embedded reply-parent attachment kinds.
// Empty → undefined so a text-only parent carries no field at all.
const KIND_BY_NUMBER: AttachmentKind[] = ["image", "video", "document", "audio"];
function mapReplyKinds(kinds: number[] | undefined): AttachmentKind[] | undefined {
  if (!kinds || kinds.length === 0) return undefined;
  return kinds.map((k) => KIND_BY_NUMBER[k] ?? "document");
}

function mapAttachment(
  a: MessageReceivedPayload["attachments"][number],
  metas?: EncryptedAttachmentMetaPayload[],
): Attachment {
  const base: Attachment = {
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
  if (!a.encrypted) return base;
  // Encrypted channel: the server row is a stand-in ("encrypted",
  // octet-stream, ciphertext size); the real metadata came out of the
  // message envelope. Without it (undecryptable message) the stand-in
  // shows, flagged encrypted.
  const meta = metas?.find((m) => m.id === a.id);
  if (!meta) return { ...base, encrypted: true };
  return {
    ...base,
    encrypted: true,
    keyB64: meta.keyB64,
    chunkBytes: meta.chunkBytes,
    filename: meta.filename,
    mime: meta.mime,
    sizeBytes: meta.sizeBytes,
    width: meta.width,
    height: meta.height,
    durationMs: meta.durationMs,
    placeholder: meta.placeholder,
    thumbnailSizesMask: base.thumbnailSizesMask || meta.thumbnailSizesMask,
  };
}

/// Map a message's attachments and hand main the keys of the sealed ones
/// so decibell-attachment:// / media-server / netFetch fetches decrypt.
function mapAttachments(
  serverId: string,
  atts: MessageReceivedPayload["attachments"] | undefined,
  metas: EncryptedAttachmentMetaPayload[] | undefined,
): Attachment[] {
  const mapped = (atts ?? []).map((a) => mapAttachment(a, metas));
  const keyed = mapped.filter((a) => a.encrypted && a.keyB64);
  if (keyed.length > 0) {
    window.decibell.attachments
      ?.registerKeys(
        keyed.map((a) => ({
          serverId,
          attachmentId: a.id,
          keyB64: a.keyB64!,
          chunkBytes: a.chunkBytes ?? 65536,
          sizeBytes: a.sizeBytes,
          mime: a.mime,
          filename: a.filename,
        })),
      )
      .catch(() => {});
  }
  return mapped;
}

// Wires up channel-message and channel-lifecycle events. DM messages
// (context === 'dm') are ignored — DM rendering is a later PR's concern.
export function useChatEvents() {
  useEffect(() => {
    const unlistenMsg = listen<MessageReceivedPayload>("message_received", (event) => {
      const p = event.payload;
      if (p.context === "dm") return;
      // serverId namespaces the per-channel cache — without it a
      // channel message can't be attributed to a server (shouldn't
      // happen: the community router always stamps it).
      if (!p.serverId) return;

      useChatStore.getState().addMessage(p.serverId, {
        id: p.id,
        sender: p.sender,
        content: p.content,
        timestamp: p.timestamp,
        channelId: p.context,
        attachments: mapAttachments(p.serverId, p.attachments, p.encryptedAttachments),
        nonce: p.nonce || undefined,
        editedAt: p.editedAt || undefined,
        replyTo: p.replyTo || undefined,
        replyToSender: p.replyToSender || undefined,
        replyToContent: p.replyToContent || undefined,
        replyToAttachmentKinds: mapReplyKinds(p.replyToAttachmentKinds),
        encrypted: p.encrypted || undefined,
        decryptError: p.decryptError || undefined,
      });
    });

    const unlistenHistory = listen<ChannelHistoryReceivedPayload>(
      "channel_history_received",
      (event) => {
        const { serverId, channelId, messages, hasMore, hasMoreAfter, aroundId, afterId } =
          event.payload;
        const mapped = messages.map((m) => ({
          id: m.id,
          sender: m.sender,
          content: m.content,
          timestamp: String(m.timestamp),
          channelId: m.channelId,
          attachments: mapAttachments(serverId, m.attachments, m.encryptedAttachments),
          nonce: m.nonce || undefined,
          editedAt: m.editedAt || undefined,
          replyTo: m.replyTo || undefined,
          replyToSender: m.replyToSender || undefined,
          replyToContent: m.replyToContent || undefined,
          replyToAttachmentKinds: mapReplyKinds(m.replyToAttachmentKinds),
          encrypted: m.encrypted || undefined,
          decryptError: m.decryptError || undefined,
        }));
        const store = useChatStore.getState();
        // Route by the request mode the server echoed back:
        //  aroundId>0 → a jump context window → replace the loaded slice.
        //  afterId>0  → downward pagination    → append newer.
        //  both 0     → older page / most-recent → prepend (existing path).
        if (aroundId > 0) {
          store.setChannelWindow(serverId, channelId, mapped, hasMore, hasMoreAfter);
        } else if (afterId > 0) {
          store.appendNewer(serverId, channelId, mapped, hasMoreAfter);
        } else {
          store.prependHistory(serverId, channelId, mapped, hasMore);
        }
        store.setHistoryLoading(serverId, channelId, false);
        store.markHistoryFetched(serverId, channelId);
      },
    );

    const unlistenPruned = listen<ChannelPrunedPayload>(
      "channel_pruned",
      (event) => {
        useChatStore
          .getState()
          .applyChannelPruned(
            event.payload.serverId,
            event.payload.channelId,
            event.payload.deletedMessageIds,
          );
      },
    );

    // The community refused a message of ours (rate limit, slowmode,
    // permission, …). Withdraw exactly that optimistic bubble — left in
    // place it would anchor at the tail forever, with every later
    // message inserting above it — and say why.
    const unlistenRejected = listen<ChannelMessageRejectedPayload>(
      "channel_message_rejected",
      (event) => {
        const p = event.payload;
        noteTypedRejection();
        if (p.nonce) {
          useChatStore.getState().removeMessageByNonce(p.serverId, p.channelId, p.nonce);
        }
        toast.error("Message not sent", p.reason || "The server rejected it.");
      },
    );

    const unlistenWiped = listen<ChannelWipedPayload>("channel_wiped", (event) => {
      useChatStore
        .getState()
        .applyChannelWiped(event.payload.serverId, event.payload.channelId);
    });

    return () => {
      unlistenMsg.then((fn) => fn());
      unlistenHistory.then((fn) => fn());
      unlistenPruned.then((fn) => fn());
      unlistenRejected.then((fn) => fn());
      unlistenWiped.then((fn) => fn());
    };
  }, []);
}
