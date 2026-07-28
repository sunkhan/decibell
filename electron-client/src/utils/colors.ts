// Per-user identity colours.
//
// The ramps themselves live in globals.css as --color-av-* (avatar
// fills, always paired with --color-av-fg) and --color-name-* (sender
// names, graded for 15px text on that theme's content surface). Both
// are re-declared per theme, so an identity keeps its *slot* across a
// theme switch while the actual hue follows the palette — the mint
// themes would otherwise be speckled with GitHub-blue avatars.
//
// Both generators hash identically and index the same 8 slots, so a
// user's avatar fill and their name colour never drift apart.

const RAMP_SIZE = 8;

function rampIndex(str: string): number {
  let hash = 0;
  for (let i = 0; i < str.length; i++) {
    hash = str.charCodeAt(i) + ((hash << 5) - hash);
  }
  return (Math.abs(hash) % RAMP_SIZE) + 1;
}

/// Sender-name colour. Returns a `var()` reference, so it is only
/// valid in a style position (inline style, CSS property) — not as a
/// value you can parse or compare.
export function stringToColor(str: string): string {
  return `var(--color-name-${rampIndex(str)})`;
}

/// Avatar fill. The darker stop is derived rather than stored so the
/// ramp stays one value per slot per theme.
export function stringToGradient(str: string): string {
  const base = `var(--color-av-${rampIndex(str)})`;
  return `linear-gradient(135deg, ${base}, color-mix(in srgb, ${base} 72%, #000))`;
}
