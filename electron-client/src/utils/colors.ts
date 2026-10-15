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
// user's avatar fill and their name colour never drift apart. People hash
// their account uid (`userColor` & co.), so a rename keeps their colour;
// the plain string versions are for things that aren't people (servers)
// and for senders with no account (uid 0), which hash the name.

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

/// The flat avatar colour (`var(--color-av-N)`), for surfaces tinted
/// with a user's identity — the voice tiles mix it into bg-light.
export function avatarColor(str: string): string {
  return `var(--color-av-${rampIndex(str)})`;
}

/// Avatar fill. The darker stop is derived rather than stored so the
/// ramp stays one value per slot per theme.
export function stringToGradient(str: string): string {
  const base = `var(--color-av-${rampIndex(str)})`;
  return `linear-gradient(135deg, ${base}, color-mix(in srgb, ${base} 72%, #000))`;
}

/// The string a person's colour hashes: their uid when they have an
/// account, else the name they were shown under.
function personKey(uid: number | null | undefined, name: string): string {
  return uid && uid > 0 ? `uid:${uid}` : name;
}

/// A person's name colour (see `stringToColor`).
export function userColor(uid: number | null | undefined, name: string): string {
  return stringToColor(personKey(uid, name));
}

/// A person's flat avatar colour (see `avatarColor`).
export function userAvatarColor(uid: number | null | undefined, name: string): string {
  return avatarColor(personKey(uid, name));
}

/// A person's avatar fill (see `stringToGradient`).
export function userGradient(uid: number | null | undefined, name: string): string {
  return stringToGradient(personKey(uid, name));
}
