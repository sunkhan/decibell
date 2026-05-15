import { memo } from "react";
import type { Message } from "../../types";
import { stringToColor } from "../../utils/colors";
import { useUiStore } from "../../stores/uiStore";
import { UserAvatar } from "../../components/UserAvatar";
import MessageText from "./MessageText";
import AttachmentList from "./AttachmentList";
import BubbleInflightAttachments from "./BubbleInflightAttachments";

function parseTimestamp(ts: string): Date {
  const asEpoch = parseInt(ts, 10);
  return isNaN(asEpoch) ? new Date(ts) : new Date(asEpoch * 1000);
}

function formatTimestamp(ts: string): string {
  const date = parseTimestamp(ts);
  if (isNaN(date.getTime())) return ts;
  const now = new Date();
  const time = date.toLocaleTimeString([], { hour: "2-digit", minute: "2-digit" });
  if (date.toDateString() === now.toDateString()) return time;
  const yesterday = new Date(now);
  yesterday.setDate(yesterday.getDate() - 1);
  if (date.toDateString() === yesterday.toDateString()) {
    return `Yesterday, at ${time}`;
  }
  return `${date.toLocaleDateString()}, at ${time}`;
}

/** Same sender within 7 minutes of the previous → grouped row. */
export function shouldGroup(prev: Message | undefined, curr: Message): boolean {
  if (!prev || prev.sender !== curr.sender) return false;
  const prevDate = parseTimestamp(prev.timestamp);
  const currDate = parseTimestamp(curr.timestamp);
  if (isNaN(prevDate.getTime()) || isNaN(currDate.getTime())) return false;
  return Math.abs(currDate.getTime() - prevDate.getTime()) < 7 * 60 * 1000;
}

interface Props {
  message: Message;
  grouped: boolean;
  serverId?: string | null;
  isLast?: boolean;
  /// Override the bubble's left padding so the avatar aligns with the
  /// text-input field below. ChatPanel passes a value accounting for
  /// its attach button; DmChatPanel passes a smaller value matching
  /// its input-bar inner padding.
  paddingLeft?: number;
  /// True iff the local user is allowed to delete this message.
  /// Drives the hover-only trash icon visibility. Parents compute
  /// this — ChatPanel: sender-match OR owner; DmChatPanel: sender-match.
  canDelete?: boolean;
  /// Fired when the user clicks the trash icon. Parents open the
  /// DeleteMessageConfirmModal with the right context payload — or,
  /// when `options.skipConfirm` is true (set by holding Shift on
  /// click), delete immediately without prompting.
  onDelete?: (
    message: Message,
    options?: { skipConfirm?: boolean },
  ) => void;
}

function MessageBubble({
  message,
  grouped,
  serverId,
  isLast,
  paddingLeft = 8,
  canDelete = false,
  onDelete,
}: Props) {
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);

  // Shared sender-popup handlers used by both the avatar and the
  // username — clicking either opens the profile popup at the
  // element's right edge; right-click opens the context menu.
  const handleSenderClick = (e: React.MouseEvent) => {
    const rect = e.currentTarget.getBoundingClientRect();
    openProfilePopup(
      message.sender,
      { x: rect.right + 8, y: rect.top },
      serverId,
    );
  };
  const handleSenderContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    openContextMenu(message.sender, { x: e.clientX, y: e.clientY });
  };

  if (grouped) {
    return (
      <div
        className="group relative flex gap-3 rounded-xl py-px pr-2 hover:bg-white/[0.015]"
        style={{ paddingLeft }}
      >
        <div className="flex w-[38px] shrink-0 items-baseline justify-end">
          <span className="text-[10px] font-medium leading-none text-text-muted opacity-0 group-hover:opacity-100">
            {parseTimestamp(message.timestamp).toLocaleTimeString([], {
              hour: "2-digit",
              minute: "2-digit",
            })}
          </span>
        </div>
        <div className="select-text min-w-0 flex-1">
          {message.content && (
            <p className="break-all text-sm leading-snug text-text-primary [overflow-wrap:anywhere]">
              <MessageText content={message.content} />
            </p>
          )}
          <AttachmentList attachments={message.attachments} serverId={serverId ?? null} />
          {message.pendingAttachmentIds && message.pendingAttachmentIds.length > 0 && (
            <BubbleInflightAttachments pendingIds={message.pendingAttachmentIds} />
          )}
        </div>
        {canDelete && onDelete && (
          <button
            onClick={(e) => onDelete(message, { skipConfirm: e.shiftKey })}
            title="Delete message (Shift+click to skip confirmation)"
            className="absolute right-2 top-0 hidden h-6 w-6 items-center justify-center rounded-md bg-bg-secondary text-error hover:bg-error/10 group-hover:flex"
          >
            <svg
              className="h-3.5 w-3.5"
              viewBox="0 0 24 24"
              fill="none"
              stroke="currentColor"
              strokeWidth="2"
              strokeLinecap="round"
              strokeLinejoin="round"
            >
              <polyline points="3 6 5 6 21 6" />
              <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
              <path d="M10 11v6M14 11v6" />
              <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
            </svg>
          </button>
        )}
      </div>
    );
  }

  return (
    <div
      className={`group relative flex gap-3 rounded-xl pr-2 pt-2.5 pb-0.5 hover:bg-white/[0.015]${
        isLast ? " animate-[fadeUp_0.3s_ease_both]" : ""
      }`}
      style={{ paddingLeft }}
    >
      <div
        className="cursor-pointer"
        onClick={handleSenderClick}
        onContextMenu={handleSenderContextMenu}
      >
        <UserAvatar username={message.sender} size={38} />
      </div>

      <div className="select-text min-w-0 flex-1">
        <div className="flex items-baseline gap-2">
          <span
            className="cursor-pointer text-sm font-bold hover:underline"
            style={{ color: stringToColor(message.sender) }}
            onClick={handleSenderClick}
            onContextMenu={handleSenderContextMenu}
          >
            {message.sender}
          </span>
          <span className="text-[11px] font-medium text-text-muted">
            {formatTimestamp(message.timestamp)}
          </span>
        </div>
        {message.content && (
          <p className="mt-0.5 break-all text-sm leading-relaxed text-text-primary [overflow-wrap:anywhere]">
            <MessageText content={message.content} />
          </p>
        )}
        <AttachmentList attachments={message.attachments} serverId={serverId ?? null} />
        {message.pendingAttachmentIds && message.pendingAttachmentIds.length > 0 && (
          <BubbleInflightAttachments pendingIds={message.pendingAttachmentIds} />
        )}
      </div>
      {canDelete && onDelete && (
        <button
          onClick={(e) => onDelete(message, { skipConfirm: e.shiftKey })}
          title="Delete message (Shift+click to skip confirmation)"
          className="absolute right-2 top-1 hidden h-6 w-6 items-center justify-center rounded-md bg-bg-secondary text-error hover:bg-error/10 group-hover:flex"
        >
          <svg
            className="h-3.5 w-3.5"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="2"
            strokeLinecap="round"
            strokeLinejoin="round"
          >
            <polyline points="3 6 5 6 21 6" />
            <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
            <path d="M10 11v6M14 11v6" />
            <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
          </svg>
        </button>
      )}
    </div>
  );
}

export default memo(MessageBubble);
