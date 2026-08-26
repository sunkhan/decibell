// Inline rich-text parser for message content — Discord's marker
// conventions plus TeX math:
//
//   **bold**   *italic* / _italic_   __underline__   ~~strikethrough~~
//   `inline code`   ``inline code with ` inside``
//   ```lang\nfenced code block```   $inline math$   $$display math$$
//   \* escapes any marker character
//
// Deliberately bespoke rather than remark/markdown-it: the grammar is
// seven markers, general markdown (headings, tables, HTML passthrough)
// is unwanted surface in chat, and this runs inside Virtuoso rows where
// per-render cost matters. Parsing is a single left-to-right scan with
// recursion only into emphasis spans; code and math contents are
// verbatim leaves.
//
// The wire format is untouched — formatting is plain marker syntax in
// the existing message string, so old clients degrade to showing the
// literal markers and the C++ server never knows.

export type RichNode =
  | { kind: "text"; text: string }
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

// Parse cache — chat re-renders the same visible window constantly
// (Virtuoso row churn, store updates), and message content is
// immutable, so cache by the content string. FIFO trim like the
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
