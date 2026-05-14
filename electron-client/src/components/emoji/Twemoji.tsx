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

// SVGs are copied from `@twemoji/svg@15.0.0` into `public/twemoji/`
// at build time by `scripts/copy-twemoji.cjs` (wired into the `dev`
// and `build` npm scripts). Vite serves `public/` from the app
// origin, so this is a same-origin file load — no network, no CDN
// 404s. Emojis missing from the local set (mostly gendered ZWJ
// sequences that were retired in twemoji 15.x) hit the `onError`
// branch below and fall back to native OS emoji rendering.
export function twemojiUrl(emoji: string): string {
  return `/twemoji/${toCodePoints(emoji)}.svg`;
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
