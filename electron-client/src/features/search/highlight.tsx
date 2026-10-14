import type { JSX } from "react";
import { fold, type SearchWord } from "./searchQuery";

/// Splits plain text into strings and <mark>s, `key` prefixing the keys.
export type Highlighter = (text: string, key: string) => (string | JSX.Element)[];

// Letters and digits plus their combining marks, so a decomposed "café"
// stays one word.
const RUN_RE = /[\p{L}\p{N}][\p{L}\p{N}\p{M}]*/gu;

/// Marks the words a search matched, the way the index matched them: whole
/// words, case- and accent-insensitive, and words starting with a 3+ letter
/// search word. Undefined when there's nothing to mark, so callers skip the
/// work entirely.
export function makeHighlighter(words: SearchWord[]): Highlighter | undefined {
  if (words.length === 0) return undefined;
  const exact = new Set(words.filter((w) => !w.prefix).map((w) => w.folded));
  const prefixes = words.filter((w) => w.prefix).map((w) => w.folded);
  const hits = (run: string) => {
    const f = fold(run);
    return exact.has(f) || prefixes.some((p) => f.startsWith(p));
  };
  return (text, key) => {
    const out: (string | JSX.Element)[] = [];
    let last = 0;
    let n = 0;
    for (const m of text.matchAll(RUN_RE)) {
      const at = m.index ?? 0;
      if (!hits(m[0])) continue;
      if (at > last) out.push(text.slice(last, at));
      out.push(
        <mark key={`${key}-h${n++}`} className="box-decoration-clone rounded-sm bg-accent-mid text-text-bright">
          {m[0]}
        </mark>,
      );
      last = at + m[0].length;
    }
    if (n === 0) return [text];
    if (last < text.length) out.push(text.slice(last));
    return out;
  };
}
