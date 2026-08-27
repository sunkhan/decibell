// Inline rich-text parser for message content — Discord's marker
// conventions plus TeX math:
//
//   **bold**   *italic* / _italic_   __underline__   ~~strikethrough~~
//   `inline code`   ``inline code with ` inside``
//   ```lang\nfenced code block```   $inline math$   $$display math$$
//   https://… autolinks   <https://…> autolinks without a preview card
//   decibell://invite/… autolinks (rendered as an invite card)
//   \* escapes any marker character
//
// Deliberately bespoke rather than remark/markdown-it: the grammar is
// seven markers, general markdown (headings, tables, HTML passthrough)
// is unwanted surface in chat, and this runs inside message rows where
// per-render cost matters. Parsing is a single left-to-right scan with
// recursion only into emphasis spans; code and math contents are
// verbatim leaves.
//
// The wire format is untouched — formatting is plain marker syntax in
// the existing message string, so old clients degrade to showing the
// literal markers and the C++ server never knows.

import { parseInviteLink } from "../servers/inviteLink";

export type RichNode =
  | { kind: "text"; text: string }
  /// An autolinked URL. `text` is what the sender typed (the href
  /// verbatim; angle brackets stripped); `embed` is false for the
  /// `<url>` form, which Discord defines as "link, but no preview".
  | { kind: "link"; href: string; text: string; embed: boolean }
  | { kind: "bold" | "italic" | "underline" | "strike"; children: RichNode[] }
  | { kind: "code"; text: string }
  | { kind: "codeblock"; lang: string | null; text: string }
  | { kind: "math"; tex: string; display: boolean };

/// Characters a backslash escapes. Anything else keeps the backslash
/// literal so Windows paths ("C:\Users") don't silently eat chars.
const ESCAPABLE = new Set(["*", "_", "~", "`", "$", "\\"]);

const WORD_CHAR = /[A-Za-z0-9]/;

function isWord(s: string, i: number): boolean {
  return i >= 0 && i < s.length && WORD_CHAR.test(s[i]);
}

function isSpaceAt(s: string, i: number): boolean {
  return i < 0 || i >= s.length || /\s/.test(s[i]);
}

// ── Autolinks ────────────────────────────────────────────────────────
// Scheme-only detection (http://, https://, and the app's own
// decibell://invite/), like Discord: bare domains stay text so
// "file.txt" and "e.g." never turn blue. A URL runs to the next
// whitespace / angle bracket / quote / backtick, then sheds trailing
// sentence punctuation and any unbalanced closing bracket — "(see
// https://x.y/z)." links "https://x.y/z" while a wiki path like
// /Foo_(bar) keeps its parenthesis.

const URL_MAX_LEN = 2048;
const URL_STOP = /[\s<>"`]/;
const URL_TRAIL_PUNCT = /[.,:;!?'"]$/;
const URL_CLOSERS: Record<string, string> = { ")": "(", "]": "[", "}": "{" };

const INVITE_SCHEME = "decibell://invite/";

function startsWithScheme(s: string, i: number): boolean {
  const c = s[i];
  if (c === "h" || c === "H") {
    const head = s.slice(i, i + 8).toLowerCase();
    return head.startsWith("https://") || head.startsWith("http://");
  }
  if (c === "d" || c === "D") {
    return s.slice(i, i + INVITE_SCHEME.length).toLowerCase() === INVITE_SCHEME;
  }
  return false;
}

/// A parseable http(s) URL with a dotted host (or localhost), or a
/// well-formed invite link. Anything else — "http://" alone,
/// "https://foo", "decibell://invite/x" — stays literal text.
function isLinkable(candidate: string): boolean {
  if (candidate.length > URL_MAX_LEN) return false;
  if (/^decibell:/i.test(candidate)) return parseInviteLink(candidate) !== null;
  try {
    const u = new URL(candidate);
    if (u.protocol !== "http:" && u.protocol !== "https:") return false;
    return u.hostname.includes(".") || u.hostname === "localhost";
  } catch {
    return false;
  }
}

/// Length of the autolink starting at `src[i]` (which must satisfy
/// startsWithScheme), bounded by `end`; 0 when nothing linkable.
function urlExtent(src: string, i: number, end: number): number {
  let j = i;
  while (j < end && !URL_STOP.test(src[j])) j++;
  let candidate = src.slice(i, j);
  // Trim until stable: "…/Foo_(bar))." sheds ".", then the unbalanced
  // ")", and keeps the balanced one.
  for (;;) {
    const before = candidate;
    if (URL_TRAIL_PUNCT.test(candidate)) candidate = candidate.slice(0, -1);
    const last = candidate[candidate.length - 1];
    const opener = last !== undefined ? URL_CLOSERS[last] : undefined;
    if (opener !== undefined) {
      let opens = 0;
      let closes = 0;
      for (const ch of candidate) {
        if (ch === opener) opens++;
        else if (ch === last) closes++;
      }
      if (closes > opens) candidate = candidate.slice(0, -1);
    }
    if (candidate === before) break;
  }
  return isLinkable(candidate) ? candidate.length : 0;
}

/// Find the next unescaped occurrence of `marker` at or after `from`.
/// Returns -1 if none before `end`.
function findCloser(src: string, from: number, end: number, marker: string): number {
  let i = from;
  while (i <= end - marker.length) {
    const at = src.indexOf(marker, i);
    if (at === -1 || at > end - marker.length) return -1;
    // Count preceding backslashes — an odd run escapes the marker.
    let bs = 0;
    for (let j = at - 1; j >= 0 && src[j] === "\\"; j--) bs++;
    if (bs % 2 === 0) return at;
    i = at + 1;
  }
  return -1;
}

/// Block constructs (fenced code, display math) already break the
/// line, so with the bubble rendering white-space: pre-wrap, a marker
/// typed on its own line would otherwise leave a blank line on each
/// side. Swallow exactly one adjacent newline, like Discord does.
function trimOneTrailingNewline(out: RichNode[]): void {
  const last = out[out.length - 1];
  if (last && last.kind === "text" && last.text.endsWith("\n")) {
    last.text = last.text.slice(0, -1);
    if (last.text.length === 0) out.pop();
  }
}

function pushText(out: RichNode[], text: string): void {
  if (text.length === 0) return;
  const last = out[out.length - 1];
  if (last && last.kind === "text") {
    last.text += text;
  } else {
    out.push({ kind: "text", text });
  }
}

/// Parse `src[start..end)` into nodes. `depth` bounds emphasis nesting
/// so pathological marker soup can't recurse deep.
function parseRange(src: string, start: number, end: number, depth: number): RichNode[] {
  const out: RichNode[] = [];
  let i = start;
  let plainFrom = start;

  const flush = (upTo: number) => {
    if (upTo > plainFrom) pushText(out, src.slice(plainFrom, upTo));
  };

  while (i < end) {
    const c = src[i];

    // Backslash escape.
    if (c === "\\" && i + 1 < end && ESCAPABLE.has(src[i + 1])) {
      flush(i);
      pushText(out, src[i + 1]);
      i += 2;
      plainFrom = i;
      continue;
    }

    // Links come before every marker so a URL's own `_`, `*` and `~`
    // (wiki paths, query strings) can never open an emphasis span.
    // `<https://…>` is the no-preview form; the brackets are dropped.
    if (c === "<" && startsWithScheme(src, i + 1)) {
      const close = src.indexOf(">", i + 1);
      if (close !== -1 && close < end) {
        const inner = src.slice(i + 1, close);
        if (!/\s/.test(inner) && isLinkable(inner)) {
          flush(i);
          out.push({ kind: "link", href: inner, text: inner, embed: false });
          i = close + 1;
          plainFrom = i;
          continue;
        }
      }
    }
    if (startsWithScheme(src, i) && !isWord(src, i - 1)) {
      const len = urlExtent(src, i, end);
      if (len > 0) {
        flush(i);
        const href = src.slice(i, i + len);
        out.push({ kind: "link", href, text: href, embed: true });
        i += len;
        plainFrom = i;
        continue;
      }
    }

    // Code first — code spans protect their contents from every other
    // marker, so they must win precedence.
    if (c === "`") {
      // Fenced block: ```[lang]\n...``` (or ```one-liner``` with no lang).
      if (src.startsWith("```", i)) {
        const close = findCloser(src, i + 3, end, "```");
        if (close !== -1 && close > i + 3) {
          flush(i);
          let body = src.slice(i + 3, close);
          let lang: string | null = null;
          const nl = body.indexOf("\n");
          const langToken = nl === -1 ? null : body.slice(0, nl);
          if (
            langToken !== null &&
            /^[A-Za-z0-9#+._-]*$/.test(langToken)
          ) {
            lang = langToken.length > 0 ? langToken.toLowerCase() : null;
            body = body.slice(nl + 1);
          }
          // Drop one trailing newline so ```…\n``` doesn't render an
          // empty final line.
          if (body.endsWith("\n")) body = body.slice(0, -1);
          if (body.length > 0) {
            trimOneTrailingNewline(out);
            out.push({ kind: "codeblock", lang, text: body });
          }
          i = close + 3;
          if (src[i] === "\n") i += 1;
          plainFrom = i;
          continue;
        }
      }
      // Double-backtick inline code (allows a single ` inside).
      const marker = src.startsWith("``", i) ? "``" : "`";
      const close = findCloser(src, i + marker.length, end, marker);
      if (close !== -1 && close > i + marker.length) {
        flush(i);
        const inner = src.slice(i + marker.length, close);
        if (inner.includes("\n")) {
          // Multiline content in inline backticks upgrades to a real
          // code block — an inline <code> spanning lines renders as a
          // ragged per-line background instead of a box, which is
          // never what the sender meant. (Discord keeps it inline;
          // ours is deliberately nicer.) No language marker, so the
          // renderer's confidence-gated auto-detect decides colors.
          let body = inner;
          if (body.startsWith("\n")) body = body.slice(1);
          if (body.endsWith("\n")) body = body.slice(0, -1);
          if (body.length > 0) {
            trimOneTrailingNewline(out);
            out.push({ kind: "codeblock", lang: null, text: body });
          }
          i = close + marker.length;
          if (src[i] === "\n") i += 1;
        } else {
          out.push({ kind: "code", text: inner });
          i = close + marker.length;
        }
        plainFrom = i;
        continue;
      }
      i += 1;
      continue;
    }

    // Math. Display first, then Pandoc-style inline rules to keep
    // "$5 and $10" literal: content must not start or end with a
    // space, and the closer must not be immediately followed by a
    // digit. Inline math stays on one line.
    if (c === "$") {
      if (src.startsWith("$$", i)) {
        const close = findCloser(src, i + 2, end, "$$");
        if (close !== -1 && close > i + 2) {
          const tex = src.slice(i + 2, close).trim();
          if (tex.length > 0) {
            flush(i);
            trimOneTrailingNewline(out);
            out.push({ kind: "math", tex, display: true });
            i = close + 2;
            if (src[i] === "\n") i += 1;
            plainFrom = i;
            continue;
          }
        }
        i += 2;
        continue;
      }
      const close = findCloser(src, i + 1, end, "$");
      if (
        close !== -1 &&
        close > i + 1 &&
        !isSpaceAt(src, i + 1) &&
        !isSpaceAt(src, close - 1) &&
        !/\d/.test(src[close + 1] ?? "") &&
        src.slice(i + 1, close).indexOf("\n") === -1
      ) {
        flush(i);
        out.push({ kind: "math", tex: src.slice(i + 1, close), display: false });
        i = close + 1;
        plainFrom = i;
        continue;
      }
      i += 1;
      continue;
    }

    // Emphasis spans. Each marker requires non-empty content whose
    // edges aren't whitespace (so "a * b * c" stays literal), and `_`
    // additionally requires non-word neighbours so snake_case_names
    // survive. Longest marker first at each position.
    //
    // Triple runs come first as an explicit compound: the naive closer
    // search would pair `***x***`'s opening `**` with the *first* two
    // stars of the closing run, leaving a stray star outside the span.
    if (depth < 8 && (c === "*" || c === "_")) {
      const triple = c === "*" ? "***" : "___";
      if (src.startsWith(triple, i)) {
        const close = findCloser(src, i + 3, end, triple);
        if (
          close !== -1 &&
          close > i + 3 &&
          !isSpaceAt(src, i + 3) &&
          !isSpaceAt(src, close - 1) &&
          (c !== "_" || (!isWord(src, i - 1) && !isWord(src, close + 3)))
        ) {
          flush(i);
          out.push({
            kind: c === "*" ? "bold" : "underline",
            children: [
              {
                kind: "italic",
                children: parseRange(src, i + 3, close, depth + 1),
              },
            ],
          });
          i = close + 3;
          plainFrom = i;
          continue;
        }
      }
    }
    if (depth < 8 && (c === "*" || c === "_" || c === "~")) {
      let marker: string | null = null;
      let kind: "bold" | "italic" | "underline" | "strike" | null = null;
      if (src.startsWith("**", i)) {
        marker = "**";
        kind = "bold";
      } else if (src.startsWith("__", i)) {
        marker = "__";
        kind = "underline";
      } else if (src.startsWith("~~", i)) {
        marker = "~~";
        kind = "strike";
      } else if (c === "*") {
        marker = "*";
        kind = "italic";
      } else if (c === "_") {
        marker = "_";
        kind = "italic";
      }
      if (marker && kind && !(c === "~" && !src.startsWith("~~", i))) {
        const close = findCloser(src, i + marker.length, end, marker);
        const contentOk =
          close !== -1 &&
          close > i + marker.length &&
          !isSpaceAt(src, i + marker.length) &&
          !isSpaceAt(src, close - 1);
        const underscoreOk =
          marker !== "_" ||
          (!isWord(src, i - 1) && !isWord(src, close + marker.length));
        if (contentOk && underscoreOk) {
          flush(i);
          out.push({
            kind,
            children: parseRange(src, i + marker.length, close, depth + 1),
          });
          i = close + marker.length;
          plainFrom = i;
          continue;
        }
      }
      i += 1;
      continue;
    }

    i += 1;
  }

  flush(end);
  return out;
}

// Parse cache — chat re-renders the same rows constantly (store updates,
// row remounts on channel switches / jump windows), and message content
// is immutable, so cache by the content string. FIFO trim like the
// thumbhash decode cache; a re-parse costs microseconds.
const parsed = new Map<string, RichNode[]>();
const PARSED_MAX = 500;

export function parseRichText(content: string): RichNode[] {
  const hit = parsed.get(content);
  if (hit) return hit;
  const nodes = parseRange(content, 0, content.length, 0);
  if (parsed.size >= PARSED_MAX) {
    let n = PARSED_MAX / 4;
    for (const k of parsed.keys()) {
      parsed.delete(k);
      if (--n <= 0) break;
    }
  }
  parsed.set(content, nodes);
  return nodes;
}

/// True when parsing found no formatting at all — the message is one
/// plain text run. Drives the jumbo-emoji path, which only applies to
/// unformatted messages (Discord behaves the same way).
export function isPlainText(nodes: RichNode[]): boolean {
  return nodes.length === 1 && nodes[0].kind === "text";
}

/// True when the message carries formatting worth previewing while
/// typing — anything beyond plain text and autolinks. A draft that is
/// just "look at https://…" reads the same rendered as typed, so the
/// composer's live preview stays out of the way for it.
export function hasFormatting(nodes: RichNode[]): boolean {
  return nodes.some((n) => n.kind !== "text" && n.kind !== "link");
}

/// The href when the whole message is one autolink (whitespace aside) —
/// a GIF or image pasted on its own. The bubble hides that text once
/// the link resolves to an image embed, so a sent GIF is just the GIF
/// (Discord does the same for media links).
export function loneLink(content: string): string | null {
  let link: string | null = null;
  for (const n of parseRichText(content)) {
    if (n.kind === "link") {
      if (link !== null || !n.embed) return null;
      link = n.href;
    } else if (n.kind === "text") {
      if (n.text.trim().length > 0) return null;
    } else {
      return null;
    }
  }
  return link;
}

/// What a lone media link reads as in a one-line preview (the DM list):
/// "GIF" / "Image" by extension, else null → show the URL.
export function mediaLabelFor(href: string): "GIF" | "Image" | null {
  try {
    const p = new URL(href).pathname;
    if (/\.gif$/i.test(p)) return "GIF";
    if (/\.(png|jpe?g|webp|avif)$/i.test(p)) return "Image";
  } catch {
    /* not a URL */
  }
  return null;
}

/// The URLs eligible for a preview card, in order of appearance:
/// autolinks outside code and math, not in the `<url>` form, deduped,
/// at most `max`. Content is immutable, so callers memoise on it.
export function extractLinks(content: string, max: number): string[] {
  const out: string[] = [];
  const seen = new Set<string>();
  const walk = (nodes: RichNode[]) => {
    for (const n of nodes) {
      if (out.length >= max) return;
      if (n.kind === "link") {
        if (n.embed && !seen.has(n.href)) {
          seen.add(n.href);
          out.push(n.href);
        }
      } else if ("children" in n) {
        walk(n.children);
      }
    }
  };
  walk(parseRichText(content));
  return out;
}

/// Flatten to plain text for single-line previews (the DM sidebar's
/// last-message row): markers dropped, emphasis children inlined, code
/// and math contents kept verbatim (a fenced block becomes its code,
/// display math its TeX source). Newlines collapse to spaces so the
/// truncating row shows as much content as possible.
export function richTextToPlain(content: string): string {
  const out: string[] = [];
  const walk = (nodes: RichNode[]) => {
    for (const n of nodes) {
      switch (n.kind) {
        case "text":
        case "code":
        case "codeblock":
        case "link":
          out.push(n.text);
          break;
        case "math":
          out.push(n.tex);
          break;
        default:
          walk(n.children);
      }
    }
  };
  walk(parseRichText(content));
  return out.join("").replace(/\s+/g, " ").trim();
}
