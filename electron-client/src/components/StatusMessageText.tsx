// A custom status message (docs/superpowers/specs/2026-10-12-status-
// messages-design.md): its emoji, then its text, with any emoji inside the
// text drawn as Twemoji like everywhere else. Plain text: no markdown, no
// shortcodes — it reads exactly as typed.

import { useMemo, type JSX } from "react";
import emojiRegex from "emoji-regex";
import Twemoji from "./emoji/Twemoji";
import type { StatusMessage } from "../stores/presenceStore";

interface Props {
  message: StatusMessage;
  /// Emoji size in px; the default suits the 11px list lines.
  emojiSize?: number;
  /// Wrap instead of truncating to one line (profile cards).
  wrap?: boolean;
  className?: string;
}

export function StatusMessageText({ message, emojiSize = 14, wrap = false, className = "" }: Props) {
  const parts = useMemo(() => {
    const out: (string | JSX.Element)[] = [];
    let last = 0;
    for (const m of message.text.matchAll(emojiRegex())) {
      const at = m.index ?? 0;
      if (at > last) out.push(message.text.slice(last, at));
      out.push(<Twemoji key={at} emoji={m[0]} size={emojiSize} />);
      last = at + m[0].length;
    }
    if (last < message.text.length) out.push(message.text.slice(last));
    return out;
  }, [message.text, emojiSize]);

  return (
    <span
      className={`${wrap ? "block whitespace-pre-wrap break-words" : "block truncate"} ${className}`}
      // A truncated line still reads in full on hover.
      title={wrap ? undefined : message.text || undefined}
    >
      {message.emoji && <Twemoji emoji={message.emoji} size={emojiSize} />}
      {message.emoji && message.text && " "}
      {parts}
    </span>
  );
}
