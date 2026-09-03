import { LockGlyph } from "../chat/MessageBubble";

/// Channel-header pill for text channels with encryption switched on.
/// Per-message lock glyphs were dropped on purpose (2026-09-03): the
/// channel is either encrypted or it isn't, so say it once, up here.
export default function ChannelEncryptionBadge() {
  return (
    <div
      title="End-to-end encrypted channel — messages and attachments are sealed on members' devices; the server never holds the keys"
      className="flex shrink-0 items-center gap-[5px] rounded-sm bg-success/15 px-2 py-0.5 font-channel text-[11px] font-medium text-success"
    >
      <LockGlyph size={11} />
      E2EE
    </div>
  );
}
