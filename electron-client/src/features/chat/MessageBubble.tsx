import { memo, useEffect, useRef, useState } from "react";
import type { Message } from "../../types";
import { stringToColor } from "../../utils/colors";
import { useUiStore } from "../../stores/uiStore";
import { useDisplayName } from "../../hooks/useDisplayName";
import { UserAvatar } from "../../components/UserAvatar";
import MessageText from "./MessageText";
import AttachmentList from "./AttachmentList";
import BubbleInflightAttachments from "./BubbleInflightAttachments";
import { useRowHeightAudit } from "./devRowHeightAudit";

// Inline edit box shown in place of a message's content. Plain textarea over
// the raw wire string (MessageText re-renders it on save). Enter submits,
// Shift+Enter inserts a newline, Escape cancels. Auto-sized + auto-focused
// with the caret at the end.
function InlineEditor({
  initialContent,
  onSubmit,
  onCancel,
}: {
  initialContent: string;
  onSubmit: (content: string) => void;
  onCancel: () => void;
}) {
  const [value, setValue] = useState(initialContent);
  const ref = useRef<HTMLTextAreaElement>(null);

  useEffect(() => {
    const el = ref.current;
    if (!el) return;
    el.focus();
    el.setSelectionRange(el.value.length, el.value.length);
    el.style.height = "auto";
    el.style.height = `${el.scrollHeight}px`;
  }, []);

  return (
    <div className="mt-0.5">
      <textarea
        ref={ref}
        value={value}
        onChange={(e) => {
          setValue(e.target.value);
          e.target.style.height = "auto";
          e.target.style.height = `${e.target.scrollHeight}px`;
        }}
        onKeyDown={(e) => {
          if (e.key === "Enter" && !e.shiftKey) {
            e.preventDefault();
            const trimmed = value.trim();
            if (trimmed) onSubmit(trimmed);
          } else if (e.key === "Escape") {
            e.preventDefault();
            onCancel();
          }
        }}
        rows={1}
        className="w-full resize-none rounded-md border border-border bg-bg-lighter px-3 py-2 text-body leading-body text-text-primary outline-none focus:border-accent"
      />
      <div className="mt-1 text-meta text-text-muted">
        escape to <button className="text-accent hover:underline" onClick={onCancel}>cancel</button>
        {" • "}enter to <span className="text-accent">save</span>
      </div>
    </div>
  );
}

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

// Parsed-epoch cache keyed on message identity. shouldGroup runs in
// Virtuoso's itemContent for every visible row on every list render —
// intentionally lazy (see ChatPanel's no-precomputed-array note), but
// re-parsing the same timestamps each pass wastes Date churn during
// scroll. Message objects are stable in the stores, so a WeakMap makes
// each parse once-per-message; replaced objects just re-parse.
const epochCache = new WeakMap<Message, number>();

function messageEpoch(m: Message): number {
  let epoch = epochCache.get(m);
  if (epoch === undefined) {
    epoch = parseTimestamp(m.timestamp).getTime();
    epochCache.set(m, epoch);
  }
  return epoch;
}

/** Same sender within 7 minutes of the previous → grouped row. */
export function shouldGroup(prev: Message | undefined, curr: Message): boolean {
  if (!prev || prev.sender !== curr.sender) return false;
  const prevEpoch = messageEpoch(prev);
  const currEpoch = messageEpoch(curr);
  if (isNaN(prevEpoch) || isNaN(currEpoch)) return false;
  return Math.abs(currEpoch - prevEpoch) < 7 * 60 * 1000;
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
  /// True iff the local user may edit this message (sender-only, real id).
  canEdit?: boolean;
  /// True while THIS message is being edited — swaps content for an editor.
  editing?: boolean;
  /// Enter edit mode for this message (clicked the pencil).
  onStartEdit?: (message: Message) => void;
  /// Commit the edit with the new content.
  onSubmitEdit?: (message: Message, content: string) => void;
  /// Leave edit mode without saving.
  onCancelEdit?: () => void;
  /// True iff the local user may reply to this message (any real message).
  canReply?: boolean;
  /// Start replying to this message (clicked the reply arrow).
  onReply?: (message: Message) => void;
  /// Resolved parent (for message.replyTo) — the quoted-preview author and a
  /// one-line content snippet. Undefined when the parent isn't loaded /
  /// was deleted → a generic fallback is shown.
  replyToSender?: string;
  replyToContent?: string;
  /// Jump to the replied-to message (clicking the quoted preview). Given the
  /// parent id; a no-op if the parent isn't in the loaded window.
  onJumpToReply?: (messageId: number) => void;
  /// Briefly flag this row (a jump target) so it flashes a highlight.
  highlighted?: boolean;
}

function MessageBubble({
  message,
  grouped,
  serverId,
  isLast,
  paddingLeft = 8,
  canDelete = false,
  onDelete,
  canEdit = false,
  editing = false,
  onStartEdit,
  onSubmitEdit,
  onCancelEdit,
  canReply = false,
  onReply,
  replyToSender,
  replyToContent,
  onJumpToReply,
  highlighted = false,
}: Props) {
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const openContextMenu = useUiStore((s) => s.openContextMenu);
  // Server nickname (or the username when none / in DMs). Identity visuals
  // (avatar, name color) stay keyed on message.sender.
  const displayName = useDisplayName(serverId, message.sender);
  // Dev-only: log any post-mount height settle (Virtuoso corrects the
  // scroll on each one, so these are candidate scroll-glitch causes).
  const auditRef = useRowHeightAudit(message.id > 0 ? message.id : message.nonce ?? "?");

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
    openContextMenu(message.sender, { x: e.clientX, y: e.clientY }, serverId);
  };

  // Content = inline editor when editing this message, else the rendered text
  // with an "(edited)" indicator. `marginClass` differs per branch (grouped
  // rows have no top margin; the first row in a group does).
  const renderContent = (marginClass: string) => {
    if (editing) {
      return (
        <InlineEditor
          initialContent={message.content}
          onSubmit={(c) => onSubmitEdit?.(message, c)}
          onCancel={() => onCancelEdit?.()}
        />
      );
    }
    if (!message.content) return null;
    return (
      <div
        className={`${marginClass} whitespace-pre-wrap break-all text-body leading-body text-text-primary [overflow-wrap:anywhere]`}
      >
        <MessageText content={message.content} />
        {message.editedAt ? (
          <span className="ml-1 select-none align-baseline text-meta text-text-muted">
            (edited)
          </span>
        ) : null}
      </div>
    );
  };

  // Hover actions (edit + delete). Hidden while editing. `topClass` aligns the
  // button cluster with each branch's top padding.
  const renderActions = (topClass: string) => {
    if (editing || (!canReply && !canEdit && !(canDelete && onDelete))) return null;
    return (
      <div className={`absolute right-2 ${topClass} hidden gap-1 group-hover:flex`}>
        {canReply && onReply && (
          <button
            onClick={() => onReply(message)}
            title="Reply"
            className="flex h-6 w-6 items-center justify-center rounded-sm bg-bg-secondary text-text-muted hover:bg-row-hover hover:text-text-primary"
          >
            <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="9 17 4 12 9 7" />
              <path d="M20 18v-2a4 4 0 0 0-4-4H4" />
            </svg>
          </button>
        )}
        {canEdit && onStartEdit && (
          <button
            onClick={() => onStartEdit(message)}
            title="Edit message"
            className="flex h-6 w-6 items-center justify-center rounded-sm bg-bg-secondary text-text-muted hover:bg-row-hover hover:text-text-primary"
          >
            <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M12 20h9" />
              <path d="M16.5 3.5a2.12 2.12 0 0 1 3 3L7 19l-4 1 1-4z" />
            </svg>
          </button>
        )}
        {canDelete && onDelete && (
          <button
            onClick={(e) => onDelete(message, { skipConfirm: e.shiftKey })}
            title="Delete message (Shift+click to skip confirmation)"
            className="flex h-6 w-6 items-center justify-center rounded-sm bg-bg-secondary text-error hover:bg-error/10"
          >
            <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <polyline points="3 6 5 6 21 6" />
              <path d="M19 6l-2 14a2 2 0 0 1-2 2H9a2 2 0 0 1-2-2L5 6" />
              <path d="M10 11v6M14 11v6" />
              <path d="M9 6V4a1 1 0 0 1 1-1h4a1 1 0 0 1 1 1v2" />
            </svg>
          </button>
        )}
      </div>
    );
  };

  // Quoted preview of the parent message, shown above a reply's content.
  const renderReplyPreview = () => {
    if (!message.replyTo) return null;
    const snippet = replyToContent
      ? replyToContent.replace(/\s+/g, " ").trim()
      : "";
    const parentId = message.replyTo;
    // Only clickable when the parent is actually resolvable in the loaded
    // window (we have its author) — otherwise a jump would be a no-op.
    const jumpable = !!onJumpToReply && !!replyToSender;
    return (
      <button
        type="button"
        disabled={!jumpable}
        onClick={jumpable ? () => onJumpToReply!(parentId) : undefined}
        className={`mb-0.5 flex min-w-0 max-w-full items-center gap-1 text-meta text-text-muted ${
          jumpable ? "cursor-pointer hover:text-text-secondary" : "cursor-default"
        }`}
      >
        <svg className="h-3 w-3 shrink-0 opacity-70" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polyline points="9 17 4 12 9 7" />
          <path d="M20 18v-2a4 4 0 0 0-4-4H4" />
        </svg>
        <span className="shrink-0 font-medium text-text-secondary">
          {replyToSender ? `@${replyToSender}` : "Original message"}
        </span>
        {snippet && <span className="truncate opacity-80">{snippet}</span>}
      </button>
    );
  };

  if (grouped) {
    return (
      <div
        ref={auditRef}
        className={`group relative flex gap-3 rounded-lg py-px pr-2 transition-colors ${
          highlighted ? "bg-accent-soft" : "hover:bg-row-hover"
        }`}
        style={{ paddingLeft }}
      >
        <div className="flex w-[38px] shrink-0 items-baseline justify-end">
          <span className="font-mono text-meta font-normal leading-none tabular-nums text-text-muted opacity-0 group-hover:opacity-100">
            {parseTimestamp(message.timestamp).toLocaleTimeString([], {
              hour: "2-digit",
              minute: "2-digit",
            })}
          </span>
        </div>
        <div className="select-text min-w-0 flex-1">
          {renderReplyPreview()}
          {/* div, not p: rich text renders block children (pre code
              blocks, display math) and a block inside <p> is invalid
              HTML — the browser force-closes the paragraph around it. */}
          {renderContent("")}
          <AttachmentList attachments={message.attachments} serverId={serverId ?? null} />
          {message.pendingAttachmentIds && message.pendingAttachmentIds.length > 0 && (
            <BubbleInflightAttachments pendingIds={message.pendingAttachmentIds} />
          )}
        </div>
        {renderActions("top-0")}
      </div>
    );
  }

  return (
    <div
      ref={auditRef}
      className={`group relative flex gap-3 rounded-lg pr-2 pt-2.5 pb-0.5 transition-colors ${
        highlighted ? "bg-accent-soft" : "hover:bg-row-hover"
      }${isLast ? " animate-[fadeUp_0.3s_ease_both]" : ""}`}
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
        {renderReplyPreview()}
        <div className="flex items-baseline gap-2">
          <span
            className="cursor-pointer font-channel text-sender font-emphasis hover:underline"
            style={{ color: stringToColor(message.sender) }}
            onClick={handleSenderClick}
            onContextMenu={handleSenderContextMenu}
          >
            {displayName}
          </span>
          <span className="font-mono text-meta font-normal tabular-nums text-text-muted">
            {formatTimestamp(message.timestamp)}
          </span>
        </div>
        {/* div, not p — see the grouped branch above. */}
        {renderContent("mt-0.5")}
        <AttachmentList attachments={message.attachments} serverId={serverId ?? null} />
        {message.pendingAttachmentIds && message.pendingAttachmentIds.length > 0 && (
          <BubbleInflightAttachments pendingIds={message.pendingAttachmentIds} />
        )}
      </div>
      {renderActions("top-1")}
    </div>
  );
}

export default memo(MessageBubble);
