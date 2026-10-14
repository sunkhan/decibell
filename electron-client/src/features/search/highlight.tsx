import type { JSX } from "react";
import { fold, type SearchWord } from "./searchQuery";

/// Splits plain text into strings and <mark>s, `key` prefixing the keys.
export type Highlighter = (text: string, key: string) => (string | JSX.Element)[];

const MARK = "box-decoration-clone rounded-sm bg-accent-mid text-text-bright";

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
        <mark key={`${key}-h${n++}`} className={MARK}>
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

/// Marks every place a fragment occurs, inside words too (the inside-words
/// option). Text and fragments are folded alike; marks map back to the
/// original characters.
export function makeFragmentHighlighter(fragments: string[]): Highlighter | undefined {
  const frags = fragments.filter((f) => f.length > 0);
  if (frags.length === 0) return undefined;
  return (text, key) => {
    // folded[i] came from text[at[i]]; at[folded.length] = text.length.
    let folded = "";
    const at: number[] = [];
    for (let i = 0; i < text.length; ) {
      const ch = String.fromCodePoint(text.codePointAt(i)!);
      const f = fold(ch);
      for (let k = 0; k < f.length; k++) at.push(i);
      folded += f;
      i += ch.length;
    }
    at.push(text.length);
    const spans: [number, number][] = [];
    for (const frag of frags) {
      for (let p = folded.indexOf(frag); p >= 0; p = folded.indexOf(frag, p + 1)) {
        spans.push([at[p], at[p + frag.length]]);
      }
    }
    if (spans.length === 0) return [text];
    spans.sort((a, b) => a[0] - b[0]);
    const out: (string | JSX.Element)[] = [];
    let last = 0;
    let n = 0;
    for (const [s0, e0] of spans) {
      const s = Math.max(s0, last);
      if (e0 <= s) continue;
      if (s > last) out.push(text.slice(last, s));
      out.push(
        <mark key={`${key}-f${n++}`} className={MARK}>
          {text.slice(s, e0)}
        </mark>,
      );
      last = e0;
    }
    if (last < text.length) out.push(text.slice(last));
    return out;
  };
}
