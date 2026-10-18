import type { MemberRosterMeta, RosterKey, ServerMember } from "../../types";

/// Roster order ("Roster order" in proto/messages.proto), mirrored exactly
/// from the community server: pages arrive in this order, so the sidebar
/// sorts by it too and a page only ever extends the list at its end. The
/// fold is search.cpp's search_fold; keep the two in step.

/// Members per roster page: the first holds every online member of all but
/// big communities; later ones load as the sidebar scrolls.
export const ROSTER_FIRST_PAGE = 500;
export const ROSTER_PAGE = 200;

// Folded code point of U+00C0..U+017F (search.cpp kLatinFold): lower case,
// accents removed, the Turkish dotless ı (U+0131) as i.
const LATIN_FOLD = [
  0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x00e6, 0x0063, // U+00C0
  0x0065, 0x0065, 0x0065, 0x0065, 0x0069, 0x0069, 0x0069, 0x0069, // U+00C8
  0x00f0, 0x006e, 0x006f, 0x006f, 0x006f, 0x006f, 0x006f, 0x00d7, // U+00D0
  0x00f8, 0x0075, 0x0075, 0x0075, 0x0075, 0x0079, 0x00fe, 0x00df, // U+00D8
  0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x00e6, 0x0063, // U+00E0
  0x0065, 0x0065, 0x0065, 0x0065, 0x0069, 0x0069, 0x0069, 0x0069, // U+00E8
  0x00f0, 0x006e, 0x006f, 0x006f, 0x006f, 0x006f, 0x006f, 0x00f7, // U+00F0
  0x00f8, 0x0075, 0x0075, 0x0075, 0x0075, 0x0079, 0x00fe, 0x0079, // U+00F8
  0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0061, 0x0063, 0x0063, // U+0100
  0x0063, 0x0063, 0x0063, 0x0063, 0x0063, 0x0063, 0x0064, 0x0064, // U+0108
  0x0111, 0x0111, 0x0065, 0x0065, 0x0065, 0x0065, 0x0065, 0x0065, // U+0110
  0x0065, 0x0065, 0x0065, 0x0065, 0x0067, 0x0067, 0x0067, 0x0067, // U+0118
  0x0067, 0x0067, 0x0067, 0x0067, 0x0068, 0x0068, 0x0127, 0x0127, // U+0120
  0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, 0x0069, // U+0128
  0x0069, 0x0069, 0x0133, 0x0133, 0x006a, 0x006a, 0x006b, 0x006b, // U+0130
  0x0138, 0x006c, 0x006c, 0x006c, 0x006c, 0x006c, 0x006c, 0x0140, // U+0138
  0x0140, 0x0142, 0x0142, 0x006e, 0x006e, 0x006e, 0x006e, 0x006e, // U+0140
  0x006e, 0x0149, 0x014b, 0x014b, 0x006f, 0x006f, 0x006f, 0x006f, // U+0148
  0x006f, 0x006f, 0x0153, 0x0153, 0x0072, 0x0072, 0x0072, 0x0072, // U+0150
  0x0072, 0x0072, 0x0073, 0x0073, 0x0073, 0x0073, 0x0073, 0x0073, // U+0158
  0x0073, 0x0073, 0x0074, 0x0074, 0x0074, 0x0074, 0x0167, 0x0167, // U+0160
  0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, 0x0075, // U+0168
  0x0075, 0x0075, 0x0075, 0x0075, 0x0077, 0x0077, 0x0079, 0x0079, // U+0170
  0x0079, 0x007a, 0x007a, 0x007a, 0x007a, 0x007a, 0x007a, 0x017f, // U+0178
];

function foldCodePoint(c: number): number {
  if (c < 0x80) return c >= 0x41 && c <= 0x5a ? c + 32 : c;
  if (c >= 0xc0 && c < 0x180) return LATIN_FOLD[c - 0xc0];
  if (c >= 0x391 && c <= 0x3a9 && c !== 0x3a2) return c + 0x20; // Greek capitals
  if (c >= 0x410 && c <= 0x42f) return c + 0x20; // Cyrillic А..Я
  if (c >= 0x400 && c <= 0x40f) return c + 0x50; // Cyrillic Ѐ..Џ
  return c;
}

/// search_fold: lower case, accents removed (combining marks dropped).
export function foldName(s: string): string {
  let out = "";
  for (const ch of s) {
    const c = ch.codePointAt(0)!;
    if (c >= 0x300 && c <= 0x36f) continue;
    out += String.fromCodePoint(foldCodePoint(c));
  }
  return out;
}

/// Code point order (the server compares UTF-8 bytes, which is the same).
/// UTF-16 code units already compare that way except surrogates against
/// U+E000..U+FFFF; the remap puts surrogates above them.
export function compareCodePoints(a: string, b: string): number {
  const n = Math.min(a.length, b.length);
  for (let i = 0; i < n; i++) {
    let x = a.charCodeAt(i);
    let y = b.charCodeAt(i);
    if (x === y) continue;
    if (x >= 0xd800) x = x >= 0xe000 ? x - 0x800 : x + 0x2000;
    if (y >= 0xd800) y = y >= 0xe000 ? y - 0x800 : y + 0x2000;
    return x - y;
  }
  return a.length - b.length;
}

export function compareRosterKeys(a: RosterKey, b: RosterKey): number {
  return compareCodePoints(a.fold, b.fold) || compareCodePoints(a.name, b.name) || a.uid - b.uid;
}

// Per member object: deltas replace a changed member's object, so the key
// is computed once per version of the member.
const keys = new WeakMap<ServerMember, RosterKey>();

/// The member's roster key. Uses the community's own names (nickname, else
/// the username it holds), never the users store's: the order must be the
/// server's, even where central knows a newer name (a rename the community
/// hasn't seen yet only changes the label).
export function rosterKey(m: ServerMember): RosterKey {
  let k = keys.get(m);
  if (!k) {
    const name = m.nickname || m.username;
    k = { fold: foldName(name), name, uid: m.uid };
    keys.set(m, k);
  }
  return k;
}

export function compareMembers(a: ServerMember, b: ServerMember): number {
  return compareRosterKeys(rosterKey(a), rosterKey(b));
}

/// The sidebar's rows: each part's members up to where the pages have
/// reached, in roster order. Members past that (an upsert for someone the
/// pages haven't got to, a lookup's find) are held but not drawn, so the
/// list never shows a gap; the offline part starts once the online part is
/// complete.
export function visibleRoster(
  roster: ServerMember[],
  meta: MemberRosterMeta | undefined,
): { online: ServerMember[]; offline: ServerMember[] } {
  const online: ServerMember[] = [];
  const offline: ServerMember[] = [];
  if (!meta) return { online, offline };
  const within = (m: ServerMember, edge: RosterKey | null) => !!edge && compareRosterKeys(rosterKey(m), edge) <= 0;
  for (const m of roster) {
    if (m.isOnline) {
      if (meta.onlineComplete || within(m, meta.onlineEdge)) online.push(m);
    } else if (meta.onlineComplete && (!meta.hasMore || within(m, meta.offlineEdge))) {
      offline.push(m);
    }
  }
  online.sort(compareMembers);
  offline.sort(compareMembers);
  return { online, offline };
}
