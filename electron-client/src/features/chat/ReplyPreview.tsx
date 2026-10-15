// Reply chrome shared by the message rows and both composers:
//
//   ReplyHeader      the quoted parent above a reply row — a spine curls
//                    from the avatar column into a one-line preview of
//                    the parent (mini avatar, name, flattened snippet).
//   ReplyComposerBar "Replying to …" strip at the top of the composer.
//
// Both render the parent through MessageText's preview mode, so markers
// are dropped, shortcodes/emoji render as Twemoji and a lone GIF/image
// link reads "GIF" / "Image" instead of a CDN URL.

import { memo } from "react";
import type { AttachmentKind } from "../../types";
import { userColor } from "../../utils/colors";
import { useDisplayName } from "../../hooks/useDisplayName";
import { useUiStore } from "../../stores/uiStore";
import { UserAvatar } from "../../components/UserAvatar";
import { AVATAR_RADIUS } from "../../components/LetterAvatar";
import MessageText from "./MessageText";
import { legacyUid } from "../users/userDirectory";

// Row geometry the spine is drawn against: MessageBubble's 38px avatar
// and its gap-3 gutter.
const ROW_AVATAR_PX = 38;
const ROW_GUTTER_PX = 12;
const MINI_AVATAR_PX = 16;
// Emoji in the one-line preview: 1.15 × the reply text (15px at the default
// 14.5 text size), so they track the text-size setting.
const SNIPPET_EMOJI_SCALE = 0.9 * 1.15;
// The reply line's box: exactly its whole-pixel line height (never below
// the mini avatar). Fixed rather than content-sized, so an emoji or glyph
// that pokes out of the line can't give the row a fractional height —
// Chromium's scroll anchoring rounds that remainder into a sub-pixel nudge
// when a history page lands above the viewport.
const LINE_BOX = `max(${MINI_AVATAR_PX}px, var(--text-reply--line-height))`;

const KIND_LABEL: Record<AttachmentKind, string> = {
  image: "Image",
  video: "Video",
  document: "File",
  audio: "Audio",
};

/// Label for an attachment-only parent: "Image", "2 Videos", "3 Attachments".
function attachmentPreviewLabel(kinds: AttachmentKind[]): string {
  if (kinds.length === 1) return KIND_LABEL[kinds[0]];
  const first = kinds[0];
  return kinds.every((k) => k === first)
    ? `${kinds.length} ${KIND_LABEL[first]}s`
    : `${kinds.length} Attachments`;
}

function ReplyGlyph({ className }: { className?: string }) {
  return (
    <svg className={className} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      <polyline points="9 17 4 12 9 7" />
      <path d="M20 18v-2a4 4 0 0 0-4-4H4" />
    </svg>
  );
}

/// One glyph for the parent's attachments: the kind's own when they all
/// match, a paperclip for a mix.
function AttachmentGlyph({ kinds }: { kinds: AttachmentKind[] }) {
  const kind = kinds.every((k) => k === kinds[0]) ? kinds[0] : null;
  return (
    <svg className="h-3.5 w-3.5 shrink-0" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" aria-hidden="true">
      {kind === "image" ? (
        <>
          <rect x="3" y="3" width="18" height="18" rx="3" />
          <circle cx="9" cy="9" r="1.75" />
          <path d="M21 15l-5-5L5 21" />
        </>
      ) : kind === "video" ? (
        <>
          <rect x="2" y="5" width="14" height="14" rx="3" />
          <path d="M22 8l-6 4 6 4z" />
        </>
      ) : kind === "audio" ? (
        <>
          <path d="M9 18V5l12-2v13" />
          <circle cx="6" cy="18" r="3" />
          <circle cx="18" cy="16" r="3" />
        </>
      ) : kind === "document" ? (
        <>
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </>
      ) : (
        <path d="M21.44 11.05l-9.19 9.19a6 6 0 0 1-8.49-8.49l9.19-9.19a4 4 0 0 1 5.66 5.66l-9.2 9.19a2 2 0 0 1-2.83-2.83l8.49-8.48" />
      )}
    </svg>
  );
}

/// The parent's one-line body: flattened text, else the attachment label,
/// with the attachment glyph trailing when it has both.
function Snippet({
  content,
  attachmentKinds,
}: {
  content?: string;
  attachmentKinds?: AttachmentKind[];
}) {
  const textSizePx = useUiStore((s) => s.textSizePx);
  const hasText = !!content && content.trim().length > 0;
  const kinds = attachmentKinds && attachmentKinds.length > 0 ? attachmentKinds : null;
  return (
    <>
      {hasText ? (
        <span className="min-w-0 truncate">
          <MessageText content={content!} preview emojiSize={Math.round(textSizePx * SNIPPET_EMOJI_SCALE)} />
        </span>
      ) : kinds ? (
        <span className="min-w-0 truncate">{attachmentPreviewLabel(kinds)}</span>
      ) : null}
      {kinds && <AttachmentGlyph kinds={kinds} />}
    </>
  );
}

interface ReplyHeaderProps {
  serverId?: string | null;
  parentId: number;
  /// Undefined = the parent was deleted (or predates embedded previews).
  sender?: string;
  content?: string;
  attachmentKinds?: AttachmentKind[];
  onJump?: (messageId: number) => void;
}

/// The quoted parent above a reply. Sits on its own line above the
/// avatar + name, indented to the content column, with a spine that
/// leaves the avatar's centre line and curls into the mini avatar.
export const ReplyHeader = memo(function ReplyHeader({
  serverId,
  parentId,
  sender,
  content,
  attachmentKinds,
  onJump,
}: ReplyHeaderProps) {
  const openProfilePopup = useUiStore((s) => s.openProfilePopup);
  const displayName = useDisplayName(serverId, legacyUid(sender), sender ?? "");
  // The panels resolve the sender from the loaded window, falling back to
  // the server-embedded parent preview — so no sender means the parent was
  // deleted (or a pre-embed server): the tombstone, not clickable.
  // Everything else is jumpable; the panels' jumpToMessage fetches an
  // around-window when the target isn't loaded.
  const jumpable = !!onJump && !!sender;

  return (
    <div className="relative mb-1" style={{ marginLeft: ROW_AVATAR_PX + ROW_GUTTER_PX, height: LINE_BOX }}>
      {/* Spine: from the avatar's centre line (2px stroke centred on it),
          up to the header's midline, then right to 4px short of the mini
          avatar; stops 2px above the avatar. */}
      <span
        aria-hidden="true"
        className="pointer-events-none absolute -bottom-0.5 top-1/2 -mt-px rounded-tl-sm border-l-2 border-t-2 border-text-muted/35"
        style={{
          left: -(ROW_GUTTER_PX + ROW_AVATAR_PX / 2 + 1),
          width: ROW_GUTTER_PX + ROW_AVATAR_PX / 2 + 1 - 4,
        }}
      />
      {sender ? (
        <button
          type="button"
          disabled={!jumpable}
          onClick={jumpable ? () => onJump!(parentId) : undefined}
          title={jumpable ? "Jump to message" : undefined}
          className={`group/reply flex h-full min-w-0 max-w-full items-center gap-1.5 text-left text-reply text-text-muted ${
            jumpable ? "cursor-pointer hover:text-text-primary" : "cursor-default"
          }`}
        >
          <UserAvatar uid={legacyUid(sender)} name={sender} size={MINI_AVATAR_PX} />
          <span
            // A fixed cap, not a share of the line: this button is only as
            // wide as its content, so a percentage shrank with a short
            // snippet and clipped even 10-letter names.
            className="max-w-56 shrink-0 truncate font-channel font-emphasis hover:underline"
            style={{ color: userColor(legacyUid(sender), sender) }}
            onClick={(e) => {
              // The name opens the profile like everywhere else; the rest
              // of the line jumps to the parent.
              e.stopPropagation();
              const rect = e.currentTarget.getBoundingClientRect();
              openProfilePopup(sender, { x: rect.right + 8, y: rect.top }, serverId);
            }}
          >
            {displayName}
          </span>
          <Snippet content={content} attachmentKinds={attachmentKinds} />
        </button>
      ) : (
        <div className="flex h-full min-w-0 items-center gap-1.5 text-reply text-text-muted">
          <span
            className="flex shrink-0 items-center justify-center bg-surface-active"
            style={{ width: MINI_AVATAR_PX, height: MINI_AVATAR_PX, borderRadius: AVATAR_RADIUS }}
          >
            <svg className="h-2.5 w-2.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" aria-hidden="true">
              <circle cx="12" cy="12" r="9" />
              <line x1="5.6" y1="5.6" x2="18.4" y2="18.4" />
            </svg>
          </span>
          <span className="truncate">Original message was deleted</span>
        </div>
      )}
    </div>
  );
});

interface ReplyComposerBarProps {
  serverId?: string | null;
  sender: string;
  content?: string;
  attachmentKinds?: AttachmentKind[];
  onCancel: () => void;
}

/// "Replying to <avatar> Name  snippet…  ×" — the first line inside the
/// composer card while a reply is pending, ruled off from the input.
export function ReplyComposerBar({
  serverId,
  sender,
  content,
  attachmentKinds,
  onCancel,
}: ReplyComposerBarProps) {
  const displayName = useDisplayName(serverId, legacyUid(sender), sender);
  return (
    <div className="flex min-w-0 items-center gap-1.5 border-b border-border-divider pb-2 text-reply text-text-muted">
      <ReplyGlyph className="h-3.5 w-3.5 shrink-0 text-accent" />
      <span className="shrink-0">Replying to</span>
      <UserAvatar uid={legacyUid(sender)} name={sender} size={MINI_AVATAR_PX} />
      <span className="max-w-1/2 shrink-0 truncate font-channel font-emphasis" style={{ color: userColor(legacyUid(sender), sender) }}>
        {displayName}
      </span>
      <span className="flex min-w-0 flex-1 items-center gap-1.5">
        <Snippet content={content} attachmentKinds={attachmentKinds} />
      </span>
      <button
        onClick={onCancel}
        title="Cancel reply (Esc)"
        className="flex h-6 w-6 shrink-0 items-center justify-center rounded-sm text-text-muted hover:bg-surface-hover hover:text-text-primary"
      >
        <svg className="h-3.5 w-3.5" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </div>
  );
}

