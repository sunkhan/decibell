// Fenced code block renderer with syntax highlighting.
//
// lowlight (highlight.js core) rather than highlight.js directly: it
// returns a HAST tree we map to React elements, so highlighted user
// content never passes through innerHTML — same standard the rest of
// the renderer holds (the only dangerouslySetInnerHTML exception is
// KaTeX's own generated markup, see MathTex).
//
// Statically imported on purpose. Lazy-loading the highlighter would
// re-render code blocks after mount — a post-paint row-height change,
// which is exactly the class of scroll glitch the row-height audit
// exists to catch. The registered-grammar bundle is ~200KB raw; by the
// O2 boot measurements (8MB ≈ 26ms) that's well under a millisecond.

import { useEffect, useRef, useState, type JSX } from "react";
import { createLowlight } from "lowlight";
import type { Root, RootContent } from "hast";
import javascript from "highlight.js/lib/languages/javascript";
import typescript from "highlight.js/lib/languages/typescript";
import python from "highlight.js/lib/languages/python";
import rust from "highlight.js/lib/languages/rust";
import c from "highlight.js/lib/languages/c";
import cpp from "highlight.js/lib/languages/cpp";
import csharp from "highlight.js/lib/languages/csharp";
import java from "highlight.js/lib/languages/java";
import kotlin from "highlight.js/lib/languages/kotlin";
import swift from "highlight.js/lib/languages/swift";
import go from "highlight.js/lib/languages/go";
import php from "highlight.js/lib/languages/php";
import ruby from "highlight.js/lib/languages/ruby";
import bash from "highlight.js/lib/languages/bash";
import powershell from "highlight.js/lib/languages/powershell";
import sql from "highlight.js/lib/languages/sql";
import json from "highlight.js/lib/languages/json";
import yaml from "highlight.js/lib/languages/yaml";
import ini from "highlight.js/lib/languages/ini"; // also registers `toml`
import xml from "highlight.js/lib/languages/xml"; // also `html`, `svg`
import css from "highlight.js/lib/languages/css";
import scss from "highlight.js/lib/languages/scss";
import markdown from "highlight.js/lib/languages/markdown";
import dockerfile from "highlight.js/lib/languages/dockerfile";
import diff from "highlight.js/lib/languages/diff";
import lua from "highlight.js/lib/languages/lua";
import plaintext from "highlight.js/lib/languages/plaintext";

const lowlight = createLowlight();
lowlight.register({
  javascript,
  typescript,
  python,
  rust,
  c,
  cpp,
  csharp,
  java,
  kotlin,
  swift,
  go,
  php,
  ruby,
  bash,
  powershell,
  sql,
  json,
  yaml,
  ini,
  xml,
  css,
  scss,
  markdown,
  dockerfile,
  diff,
  lua,
  plaintext,
});
// TOML is an alias of the ini grammar in highlight.js but the alias
// isn't carried by the module import — register it explicitly.
lowlight.registerAlias({ ini: ["toml"] });

// Highlight cache — rows remount on channel switches, jump windows and
// slice trims, and message content is immutable, so cache the HAST per
// (lang, code).
// FIFO trim, same shape as the thumbhash/parse caches.
const highlighted = new Map<string, Root>();
const HIGHLIGHTED_MAX = 200;

// No language marker → plain text, deliberately. Auto-detection was
// tried and measured out (2026-08-16): highlight.js's highlightAuto is
// unusable at chat-snippet scale — `const a = 1;` detects as INI, a
// 3-line JS snippet as CSS at relevance 5 while *correct* Python
// detections score 3, and prose lands on random grammars at 1. There
// is no relevance threshold that separates right from wrong, and a
// curated grammar subset doesn't fix it. Wrong keyword colors are
// worse than none (which is why Discord doesn't auto-detect either).
// Highlighting is explicit: ```lang.

function highlight(lang: string | null, code: string): Root {
  // lang is regex-restricted (no spaces), so "lang code" can't collide.
  const key = `${lang ?? ""} ${code}`;
  const hit = highlighted.get(key);
  if (hit) return hit;
  let tree: Root;
  try {
    tree =
      lang && lowlight.registered(lang)
        ? lowlight.highlight(lang, code)
        : lowlight.highlight("plaintext", code);
  } catch {
    tree = lowlight.highlight("plaintext", code);
  }
  if (highlighted.size >= HIGHLIGHTED_MAX) {
    let n = HIGHLIGHTED_MAX / 4;
    for (const k of highlighted.keys()) {
      highlighted.delete(k);
      if (--n <= 0) break;
    }
  }
  highlighted.set(key, tree);
  return tree;
}

/// HAST → React. lowlight only emits nested spans with hljs-* class
/// names and text leaves, so the mapping is total with two cases.
function hastToReact(nodes: RootContent[], keyBase: string): (string | JSX.Element)[] {
  const out: (string | JSX.Element)[] = [];
  nodes.forEach((node, i) => {
    if (node.type === "text") {
      out.push(node.value);
    } else if (node.type === "element") {
      const className = Array.isArray(node.properties?.className)
        ? node.properties.className.join(" ")
        : undefined;
      out.push(
        <span key={`${keyBase}-${i}`} className={className}>
          {hastToReact(node.children as RootContent[], `${keyBase}-${i}`)}
        </span>,
      );
    }
  });
  return out;
}

/// Uncached highlight for live consumers (the RichComposer overlay
/// re-highlights on every keystroke). Deliberately bypasses the LRU
/// above: keystroke intermediate states would churn real message
/// blocks out of it, and live input has no reuse to cache anyway.
export function highlightNodes(
  lang: string | null,
  code: string,
): (string | JSX.Element)[] {
  let tree: Root;
  try {
    tree =
      lang && lowlight.registered(lang)
        ? lowlight.highlight(lang, code)
        : lowlight.highlight("plaintext", code);
  } catch {
    tree = lowlight.highlight("plaintext", code);
  }
  return hastToReact(tree.children, "e");
}

/// Blocks taller than this many lines render clamped with an expand
/// bar. Line-count-based rather than measured on purpose: it's
/// synchronous and deterministic (the pre never soft-wraps — long
/// lines scroll horizontally), so row height is final in the first
/// paint frame and a pasted 500-line file can't swallow the chat.
const COLLAPSE_THRESHOLD_LINES = 14;

export default function CodeBlock({ lang, text }: { lang: string | null; text: string }) {
  const [expanded, setExpanded] = useState(false);
  const [copied, setCopied] = useState(false);
  const copiedTimerRef = useRef<number | undefined>(undefined);
  useEffect(() => () => window.clearTimeout(copiedTimerRef.current), []);

  const tree = highlight(lang, text);
  const langResolved = lang !== null && lowlight.registered(lang);
  let lineCount = 1;
  for (let i = 0; i < text.length; i++) if (text[i] === "\n") lineCount++;
  const collapsible = lineCount > COLLAPSE_THRESHOLD_LINES;

  const onCopy = () => {
    navigator.clipboard
      .writeText(text)
      .then(() => {
        setCopied(true);
        window.clearTimeout(copiedTimerRef.current);
        copiedTimerRef.current = window.setTimeout(() => setCopied(false), 1500);
      })
      .catch(() => {});
  };

  // .rich-codeblock is the hljs scope root — token colors live in
  // globals.css mapped onto the design-system palette variables, so
  // highlighting follows all five themes automatically.
  //
  // The header bar (language label + copy button) participates in
  // layout — it replaced an absolutely-positioned badge that floated
  // over the first code line once blocks became fit-width. Being in
  // flow means w-fit widens the block to accommodate it, so nothing
  // can overlap the code.
  //
  // w-fit + min-w: the block hugs its content instead of spanning the
  // whole chat width; the minimum keeps two-word snippets from
  // rendering as a postage stamp, and max-w-full still defers to the
  // bubble (overflowing lines scroll inside the pre).
  return (
    <div className="my-1 w-fit min-w-48 max-w-full overflow-hidden rounded-md border border-border bg-bg-darkest">
      <div className="flex items-center justify-between gap-3 border-b border-border-divider px-3 py-1">
        <span className="select-none font-channel text-[10.5px] font-semibold uppercase tracking-[0.07em] text-accent-bright">
          {langResolved ? lang : "code"}
        </span>
        {/* 14px icon in a 24px button, matching the app's other icon
            buttons — at 12px a 24-viewBox stroke lands on half-pixel
            boundaries and anti-aliases into a blur. */}
        <button
          onClick={onCopy}
          title="Copy code"
          className="flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
        >
          {copied ? (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round" className="text-success">
              <polyline points="20 6 9 17 4 12" />
            </svg>
          ) : (
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <rect x="9" y="9" width="13" height="13" rx="2" ry="2" />
              <path d="M5 15H4a2 2 0 0 1-2-2V4a2 2 0 0 1 2-2h9a2 2 0 0 1 2 2v1" />
            </svg>
          )}
        </button>
      </div>
      {/* Collapsed blocks scroll internally (wheel over the block moves
          the code, not the chat). Scroll chaining is deliberately left
          at the browser default (overscroll-behavior: auto): a block
          already at its top chains an upward wheel straight to the
          channel — and same at the bottom going down — so a block under
          the cursor doesn't trap someone who's just scrolling the
          channel. (A previous overscroll-contain here blocked that.)
          The expand bar remains for reading at full height. */}
      <pre
        className={`rich-codeblock overflow-x-auto px-3 py-2.5 font-mono text-[12.5px] leading-[1.5] text-text-primary ${
          collapsible && !expanded ? "max-h-60 overflow-y-auto" : ""
        }`}
      >
        <code>{hastToReact(tree.children, "h")}</code>
      </pre>
      {collapsible && (
        <button
          onClick={() => setExpanded((v) => !v)}
          className="flex w-full cursor-pointer items-center justify-center gap-1.5 border-t border-border-divider px-3 py-1.5 font-mono text-[10.5px] uppercase tracking-wide text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
        >
          <svg
            width="10"
            height="10"
            viewBox="0 0 24 24"
            fill="none"
            stroke="currentColor"
            strokeWidth="3"
            strokeLinecap="round"
            strokeLinejoin="round"
            className={expanded ? "rotate-180" : ""}
          >
            <polyline points="6 9 12 15 18 9" />
          </svg>
          {expanded ? "Collapse" : `Expand · ${lineCount} lines`}
        </button>
      )}
    </div>
  );
}
