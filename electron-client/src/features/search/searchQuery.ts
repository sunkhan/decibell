/// Message search queries (docs/superpowers/specs/2026-10-14-message-search-design.md):
/// the filters the search box holds as chips, the free text beside them, and
/// how both turn into one MESSAGE_SEARCH_REQ. Pure functions only.

export type HasKind = "link" | "file" | "image" | "video" | "audio" | "document";
export const HAS_KINDS: HasKind[] = ["link", "file", "image", "video", "audio", "document"];
/// MessageSearchRequest.Has bits.
export const HAS_BIT: Record<HasKind, number> = {
  file: 1,
  image: 2,
  video: 4,
  audio: 8,
  document: 16,
  link: 32,
};
export const HAS_LABEL: Record<HasKind, string> = {
  link: "Link",
  file: "File",
  image: "Image",
  video: "Video",
  audio: "Audio",
  document: "Document",
};

export type DateOp = "before" | "during" | "after";
export const DATE_OPS: DateOp[] = ["before", "during", "after"];

/// One chip. Dates are local calendar days, "YYYY-MM-DD". "inside" isn't
/// a filter but an option shown the same way: match the words anywhere
/// inside words (MessageSearchRequest.match_within_words).
export type SearchFilter =
  /// A member by account; `username` is the name it was picked under
  /// (shown until the users store knows better).
  | { kind: "from"; uid: number; username: string }
  | { kind: "in"; channelId: string }
  | { kind: "has"; has: HasKind }
  | { kind: DateOp; date: string }
  | { kind: "inside" };

export type FilterKind = SearchFilter["kind"];

export interface SearchQuery {
  text: string;
  filters: SearchFilter[];
}

export type SearchSort = "newest" | "oldest" | "relevance";

export const EMPTY_QUERY: SearchQuery = { text: "", filters: [] };

export function sameFilter(a: SearchFilter, b: SearchFilter): boolean {
  if (a.kind !== b.kind) return false;
  switch (a.kind) {
    case "from":
      return a.uid === (b as typeof a).uid;
    case "in":
      return a.channelId === (b as typeof a).channelId;
    case "has":
      return a.has === (b as typeof a).has;
    default:
      // One date per operator (a second "before:" replaces the first), and
      // "inside" is on or off.
      return true;
  }
}

/// Adds a chip, replacing an equal one (and the same date operator).
export function withFilter(filters: SearchFilter[], f: SearchFilter): SearchFilter[] {
  return [...filters.filter((x) => !sameFilter(x, f)), f];
}

export function matchesInside(q: SearchQuery): boolean {
  return q.filters.some((f) => f.kind === "inside");
}

/// Something to search for: words, or at least one filter (the inside
/// option alone isn't one).
export function isSearchable(q: SearchQuery): boolean {
  if (q.filters.some((f) => f.kind !== "inside")) return true;
  return matchesInside(q) ? searchFragments(q.text).length > 0 : searchWords(q.text).length > 0;
}

// ── Dates ─────────────────────────────────────────────────────────

const DAY_RE = /^(\d{4})-(\d{2})-(\d{2})$/;

/// "YYYY-MM-DD" for a local Date.
export function dayKey(d: Date): string {
  const p = (n: number) => String(n).padStart(2, "0");
  return `${d.getFullYear()}-${p(d.getMonth() + 1)}-${p(d.getDate())}`;
}

/// Local midnight of `day` plus `addDays`, as a Date (null if malformed).
export function dayStart(day: string, addDays = 0): Date | null {
  const m = DAY_RE.exec(day);
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]) + addDays);
  return Number.isNaN(d.getTime()) ? null : d;
}

/// A typed date ("2026-10-03", "2026-10-3", "today", "yesterday") as a day
/// key, or null.
export function parseDay(input: string, now = new Date()): string | null {
  const t = input.trim().toLowerCase();
  if (t === "today") return dayKey(now);
  if (t === "yesterday") return dayKey(new Date(now.getFullYear(), now.getMonth(), now.getDate() - 1));
  const m = /^(\d{4})-(\d{1,2})-(\d{1,2})$/.exec(t);
  if (!m) return null;
  const d = new Date(Number(m[1]), Number(m[2]) - 1, Number(m[3]));
  if (d.getMonth() !== Number(m[2]) - 1 || d.getDate() !== Number(m[3])) return null;
  return dayKey(d);
}

/// Unix-second bounds for the date chips: `after` inclusive, `before`
/// exclusive, 0 = unbounded. before: a day = earlier than that day;
/// after: a day = later than that day; during: that day.
export function dateBounds(filters: SearchFilter[]): { after: number; before: number } {
  let after = 0;
  let before = 0;
  const sec = (d: Date | null) => (d ? Math.floor(d.getTime() / 1000) : 0);
  const lower = (v: number) => {
    if (v > after) after = v;
  };
  const upper = (v: number) => {
    if (v > 0 && (before === 0 || v < before)) before = v;
  };
  for (const f of filters) {
    if (f.kind === "before") upper(sec(dayStart(f.date)));
    else if (f.kind === "after") lower(sec(dayStart(f.date, 1)));
    else if (f.kind === "during") {
      lower(sec(dayStart(f.date)));
      upper(sec(dayStart(f.date, 1)));
    }
  }
  return { after, before };
}

// ── The request ───────────────────────────────────────────────────

export const PAGE_SIZE = 25;

export interface SearchMessagesArgs {
  serverId: string;
  seq: number;
  query: string;
  channelIds: string[];
  authorUids: number[];
  after: number;
  before: number;
  has: number;
  sort: SearchSort;
  cursor: number;
  offset: number;
  limit: number;
  matchWithinWords: boolean;
}

export function toSearchArgs(
  serverId: string,
  q: SearchQuery,
  sort: SearchSort,
  seq: number,
  page: { cursor?: number; offset?: number } = {},
): SearchMessagesArgs {
  const { after, before } = dateBounds(q.filters);
  let has = 0;
  // Senders by account: every name they posted under.
  const authorUids: number[] = [];
  const channelIds: string[] = [];
  for (const f of q.filters) {
    if (f.kind === "has") has |= HAS_BIT[f.has];
    else if (f.kind === "from") authorUids.push(f.uid);
    else if (f.kind === "in") channelIds.push(f.channelId);
  }
  return {
    serverId,
    seq,
    query: q.text.trim(),
    channelIds,
    authorUids,
    after,
    before,
    has,
    sort,
    cursor: page.cursor ?? 0,
    offset: page.offset ?? 0,
    limit: PAGE_SIZE,
    matchWithinWords: matchesInside(q),
  };
}

// ── Words, as the server reads them ───────────────────────────────

export interface SearchWord {
  /// Lower-cased, accents removed.
  folded: string;
  /// Matches words that start with it (3+ letters, not in a phrase).
  prefix: boolean;
}

/// Lower-case and strip accents, the way the server folds text (unicode61
/// remove_diacritics for words, search_fold inside words), with the Turkish
/// dotless ı as i so ı and i match each other.
export function fold(s: string): string {
  return s.normalize("NFD").replace(/\p{M}+/gu, "").toLowerCase().replace(/ı/g, "i");
}

const WORD_RE = /[\p{L}\p{N}]+/gu;

/// The words a query's text matches, mirroring the server's
/// build_fts_query: whitespace-separated words and "quoted phrases", a
/// leading `-` excludes (excluded words aren't returned), a word of 3+
/// letters also matches as a prefix. Punctuation splits words.
/// The words and "phrases" of a query as inside-word fragments (folded,
/// punctuation kept, excluded ones left out), mirroring the server's
/// split_fragments.
export function searchFragments(text: string): string[] {
  const out: string[] = [];
  const re = /(-?)(?:"([^"]*)"?|(\S+))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m[1] === "-") continue;
    const f = fold(m[2] !== undefined ? m[2] : m[3]);
    if (f) out.push(f);
  }
  return out;
}

export function searchWords(text: string): SearchWord[] {
  const out: SearchWord[] = [];
  const re = /(-?)(?:"([^"]*)"?|(\S+))/g;
  let m: RegExpExecArray | null;
  while ((m = re.exec(text)) !== null) {
    if (m[1] === "-") continue;
    const phrase = m[2] !== undefined;
    const raw = phrase ? m[2] : m[3];
    const parts = fold(raw).match(WORD_RE) ?? [];
    const letters = parts.join("").length;
    parts.forEach((w, i) => {
      // In a bare word the prefix applies to its last piece ("e-mail" →
      // "e" then "mail…"), like FTS5's phrase*.
      out.push({ folded: w, prefix: !phrase && letters >= 3 && i === parts.length - 1 });
    });
  }
  return out;
}
