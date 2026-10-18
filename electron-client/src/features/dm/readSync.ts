// A conversation read on another device of ours (DM_READ_SYNC, multi-device:
// docs/superpowers/specs/2026-10-18-multi-device-design.md). Pure, so it can
// be checked without the store.

interface SyncedConversation {
  unreadCount: number;
  lastReadId: number;
  /// Newer messages exist below the loaded slice (a jump window).
  hasMoreAfter: boolean;
  messages: { id?: number; senderUid: number }[];
  lastMessage: { id?: number } | null;
}

/// The unread count once the conversation is read up to `upToId`. Read up to
/// the newest message known here: none. Otherwise, when the loaded rows reach
/// down to the cursor and up to the present, exactly the peer's rows above
/// it (never more than before). Otherwise there's no telling: unchanged.
export function unreadAfterReadSync(conv: SyncedConversation, upToId: number, me: number): number {
  if (conv.unreadCount <= 0) return 0;
  let newest = conv.lastMessage?.id ?? 0;
  let coversCursor = false;
  for (const m of conv.messages) {
    const id = m.id ?? 0;
    if (id > newest) newest = id;
    if (id > 0 && id <= upToId) coversCursor = true;
  }
  if (newest > 0 && upToId >= newest) return 0;
  if (!coversCursor || conv.hasMoreAfter) return conv.unreadCount;
  let above = 0;
  for (const m of conv.messages) {
    if ((m.id ?? 0) > upToId && m.senderUid !== me) above++;
  }
  return Math.min(conv.unreadCount, above);
}

/// A live message at or below what we've read (here or on another device)
/// isn't unread.
export function alreadyRead(lastReadId: number, messageId: number | undefined): boolean {
  return typeof messageId === "number" && messageId > 0 && messageId <= lastReadId;
}
