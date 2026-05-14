// Twemoji renderer — inline SVG via dangerouslySetInnerHTML.
//
// Rationale: the previous <img src="/twemoji/X.svg"> approach made
// every emoji a Chromium image resource. Each one carried a parsed
// SVG DOM (~20-30 KB), a resource-scheduler entry (~10-20 KB), and a
// rasterized buffer per display size (~5 KB × 3 sizes the app uses).
// Scrolling through the picker mounted ~1800 of these and the
// renderer process retained ~300 MB of resource caches even after the
// picker closed, because Chromium intentionally holds decoded images
// warm for fast re-render.
//
// Inline SVG content sidesteps all of that. The SVG markup lives in a
// JSON bundle (~8 MB) generated at build time from @twemoji/svg by
// scripts/build-twemoji-map.cjs. Each Twemoji renders the right svg
// string as the body of a span — no <img>, no image resource, no
// resource cache. When the component unmounts, React drops the DOM
// node and the SVG string entry stays in the JS heap exactly once
// (the JSON bundle).
//
// The bundle adds ~8 MB to the JS heap permanently in exchange for
// eliminating the ~50-80 MB / ~3700 emojis residual cache. Net win.

import twemojiData from "./twemoji-data.json";

const TWEMOJI: Record<string, string> = twemojiData as Record<string, string>;

// Map an emoji grapheme to the codepoint-hyphenated lookup key.
// Twemoji strips the variation-selector-16 (FE0F) from all multi-char
// sequences; keep it for lone-base ones (a bare single codepoint) so
// the lookup matches the @twemoji/svg filenames the build script
// keyed the map with.
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
  const cp = toCodePoints(emoji);
  const svg = TWEMOJI[cp];
  const interactive = !!onClick;
  const interactiveClass = interactive
    ? "cursor-pointer hover:opacity-75 transition-opacity"
    : "";

  if (!svg) {
    // Fallback to native OS rendering — covers the gendered ZWJ
    // sequences twemoji 15.x retired and any unknown emoji.
    return (
      <span
        style={{ fontSize: size, lineHeight: 1 }}
        className={interactiveClass}
        onClick={onClick}
        role={interactive ? "button" : undefined}
      >
        {emoji}
      </span>
    );
  }

  return (
    <span
      className={`inline-block align-[-0.2em] ${interactiveClass} ${className ?? ""}`}
      // The SVG carries its own viewBox; the span's CSS width/height
      // drives display size and the SVG scales via the viewBox. No
      // separate raster per size variant in the resource cache.
      style={{ width: size, height: size }}
      onClick={onClick}
      role={interactive ? "button" : undefined}
      aria-label={emoji}
      // Safe — content comes from the vetted @twemoji/svg npm package,
      // not user input.
      dangerouslySetInnerHTML={{ __html: svg }}
    />
  );
}

/// Legacy export. Kept so any straggling imports of `twemojiUrl`
/// don't fail to compile, though there should be none now (only the
/// component imported Twemoji's named export historically).
export function twemojiUrl(emoji: string): string {
  return `/twemoji/${toCodePoints(emoji)}.svg`;
}
