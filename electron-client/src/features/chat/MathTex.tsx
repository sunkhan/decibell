// KaTeX math renderer for $inline$ and $$display$$ message spans.
//
// KaTeX over MathJax: synchronous, milliseconds per formula, so rows
// have their final height in their first paint frame — an async
// renderer would resize rows after mount, the scroll-glitch class the
// row-height audit polices. Statically imported for the same reason.
//
// This is the renderer's ONE sanctioned dangerouslySetInnerHTML on
// user-derived content: renderToString escapes the input TeX and emits
// only KaTeX's own markup, with trust:false refusing the commands that
// could inject URLs/HTML (\href, \htmlClass, …). Everything else in
// the message pipeline stays React-element-only.

import katex from "katex";
import "katex/dist/katex.min.css";

// Render cache, FIFO-trimmed — same shape as the parse/highlight
// caches. Keyed by (display, tex); message content is immutable.
const rendered = new Map<string, string>();
const RENDERED_MAX = 300;

function renderTex(tex: string, display: boolean): string {
  const key = `${display ? "D" : "i"}${tex}`;
  const hit = rendered.get(key);
  if (hit !== undefined) return hit;
  let html: string;
  try {
    html = katex.renderToString(tex, {
      displayMode: display,
      throwOnError: false, // bad TeX renders as red literal, never throws
      errorColor: "var(--color-error)",
      trust: false,
      // Adversarial-input ceilings: \rule{9in}{9in} style blowups clamp
      // to 16em, and macro expansion stops well before it can stall a
      // render frame (KaTeX default is 1000).
      maxSize: 16,
      maxExpand: 256,
    });
  } catch {
    // throwOnError:false covers parse errors; this catch is for
    // internal KaTeX failures. Show the raw TeX rather than nothing.
    html = "";
  }
  if (rendered.size >= RENDERED_MAX) {
    let n = RENDERED_MAX / 4;
    for (const k of rendered.keys()) {
      rendered.delete(k);
      if (--n <= 0) break;
    }
  }
  rendered.set(key, html);
  return html;
}

export default function MathTex({ tex, display }: { tex: string; display: boolean }) {
  const html = renderTex(tex, display);
  if (!html) {
    return (
      <code className="box-decoration-clone rounded-sm bg-bg-darkest px-1.5 py-0.5 font-mono text-[0.85em]">
        {tex}
      </code>
    );
  }
  const Tag = display ? "div" : "span";
  return (
    <Tag
      className={
        display
          ? "rich-math-display my-1 overflow-x-auto py-1"
          : "rich-math-inline"
      }
      // eslint-disable-next-line react/no-danger
      dangerouslySetInnerHTML={{ __html: html }}
    />
  );
}
