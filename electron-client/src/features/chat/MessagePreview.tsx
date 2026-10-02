// Live send-preview for the message input. Appears inside the input
// card only when the draft actually contains rich-text formatting —
// plain messages (including plain multiline, emoji and bare links) get
// no preview because it would add nothing. Rendering goes through the same
// MessageText the bubbles use, so what you see is exactly what will be
// sent, code highlighting, KaTeX and all.
//
// Kept off the keystroke's critical path: the preview renders from a
// deferred copy of the draft, so a long code block re-highlighting can't
// delay the typed character from painting — React renders it after, and
// drops it if the next key arrives first. The "is it formatted?" check
// parses uncached: every draft string is new, and caching them flushed
// the shared parse cache that the message list relies on.

import { memo, useDeferredValue, useMemo, useState } from "react";
import { parseRichTextUncached, hasFormatting } from "./richText";
import MessageText from "./MessageText";

// Memoised so the urgent render (deferred value unchanged) skips it.
const PreviewBody = memo(function PreviewBody({ content }: { content: string }) {
  return <MessageText content={content} />;
});

export default function MessagePreview({ draft: liveDraft }: { draft: string }) {
  const [collapsed, setCollapsed] = useState(false);
  const draft = useDeferredValue(liveDraft);

  const formatted = useMemo(() => {
    if (!draft) return false;
    // Autolinks alone don't count: a bare URL renders as itself.
    return hasFormatting(parseRichTextUncached(draft));
  }, [draft]);

  if (!formatted) return null;

  return (
    <div className="border-b border-border-divider pb-2">
      <button
        onClick={() => setCollapsed((v) => !v)}
        className="mb-1 flex cursor-pointer items-center gap-1 text-[10px] font-semibold uppercase tracking-[0.07em] text-text-muted transition-colors hover:text-text-secondary"
        title={collapsed ? "Show preview" : "Hide preview"}
      >
        <svg
          width="10"
          height="10"
          viewBox="0 0 24 24"
          fill="none"
          stroke="currentColor"
          strokeWidth="3"
          strokeLinecap="round"
          strokeLinejoin="round"
          className={collapsed ? "" : "rotate-90"}
        >
          <polyline points="9 18 15 12 9 6" />
        </svg>
        Preview
      </button>
      {!collapsed && (
        // Same classes as the bubble content wrapper in MessageBubble,
        // so line wrapping and spacing match the sent result exactly.
        <div className="max-h-44 overflow-y-auto whitespace-pre-wrap break-all text-body leading-body text-text-primary [overflow-wrap:anywhere]">
          <PreviewBody content={draft} />
        </div>
      )}
    </div>
  );
}
