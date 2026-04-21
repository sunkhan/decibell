import { useState } from "react";

// Map an emoji grapheme to the Twemoji asset filename. Twemoji strips the
// variation-selector-16 (FE0F) from all multi-char sequences; keep it for
// lone-base ones (a bare single codepoint) so the asset name stays correct.
function toCodePoints(emoji: string): string {
  const chars = Array.from(emoji);
  const hasMultipleChars = chars.length > 1;
  const codes: string[] = [];
  for (const char of chars) {
    const cp = char.codePointAt(0);
    if (cp === undefined) continue;
    if (cp === 0xfe0f && hasMultipleChars) continue;
    codes.push(cp.toString(16));
  }
  return codes.join("-");
}

export function twemojiUrl(emoji: string): string {
  return `https://cdn.jsdelivr.net/gh/jdecked/twemoji@15.1.0/assets/svg/${toCodePoints(
    emoji
  )}.svg`;
}

interface TwemojiProps {
  emoji: string;
  size?: number;
  className?: string;
  onClick?: (e: React.MouseEvent<HTMLElement>) => void;
}

export default function Twemoji({
  emoji,
  size = 20,
  className,
  onClick,
}: TwemojiProps) {
  const [failed, setFailed] = useState(false);
  const interactive = !!onClick;
  const interactiveClass = interactive
    ? "cursor-pointer hover:opacity-75 transition-opacity"
    : "";
  if (failed) {
    return (
      <span
        style={{ fontSize: size }}
        className={interactiveClass}
        onClick={onClick}
        role={interactive ? "button" : undefined}
      >
        {emoji}
      </span>
    );
  }
  return (
    <img
      src={twemojiUrl(emoji)}
      alt={emoji}
      loading="lazy"
      className={`inline-block align-[-0.2em] ${interactiveClass} ${className ?? ""}`}
      style={{ width: size, height: size }}
      draggable={false}
      onError={() => setFailed(true)}
      onClick={onClick}
      role={interactive ? "button" : undefined}
    />
  );
}
