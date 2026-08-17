// "Rich text mode" — structured composers for code blocks and math,
// opened from the button beside the emoji picker. Rather than making
// the chat contentEditable pretend to be an IDE (Tab handling and
// caret math inside emoji-atom DOM is a minefield), each mode is a
// floating panel with a real <textarea>: native selection, native
// undo, and simple keydown handling give the IDE feel. Insert
// serializes to the same marker syntax a user could type by hand
// (```lang fences / $$…$$), appends it to the draft, and the live
// send-preview shows the rendered result.
//
// Editing keys in code mode:
//   Tab / Shift+Tab   indent / outdent (line-wise when the selection
//                     spans lines; two-space unit)
//   Enter             newline + auto-indent to the current line's depth
//   Ctrl/Cmd+Enter    insert into the draft
//   Escape            cancel

import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import MathTex from "./MathTex";
import { highlightNodes } from "./CodeBlock";

/// Language choices for the dropdown — the registered grammar set from
/// CodeBlock.tsx keyed by their shortest common alias, labeled with
/// the languages' proper names.
const LANGS: { value: string; label: string }[] = [
  { value: "plain", label: "Plain text" },
  { value: "js", label: "JavaScript" },
  { value: "ts", label: "TypeScript" },
  { value: "python", label: "Python" },
  { value: "rust", label: "Rust" },
  { value: "c", label: "C" },
  { value: "cpp", label: "C++" },
  { value: "csharp", label: "C#" },
  { value: "java", label: "Java" },
  { value: "kotlin", label: "Kotlin" },
  { value: "swift", label: "Swift" },
  { value: "go", label: "Go" },
  { value: "php", label: "PHP" },
  { value: "ruby", label: "Ruby" },
  { value: "bash", label: "Bash" },
  { value: "powershell", label: "PowerShell" },
  { value: "sql", label: "SQL" },
  { value: "json", label: "JSON" },
  { value: "yaml", label: "YAML" },
  { value: "toml", label: "TOML" },
  { value: "html", label: "HTML" },
  { value: "css", label: "CSS" },
  { value: "scss", label: "SCSS" },
  { value: "markdown", label: "Markdown" },
  { value: "dockerfile", label: "Dockerfile" },
  { value: "diff", label: "Diff" },
  { value: "lua", label: "Lua" },
];

const INDENT = "  ";

export default function RichComposer({ onInsert }: { onInsert: (snippet: string) => void }) {
  const [menuOpen, setMenuOpen] = useState(false);
  const [mode, setMode] = useState<null | "code" | "math">(null);
  const [code, setCode] = useState("");
  const [lang, setLang] = useState("plain");
  const [math, setMath] = useState("");
  const triggerWrapRef = useRef<HTMLDivElement>(null);
  const areaRef = useRef<HTMLTextAreaElement>(null);
  const highlightPreRef = useRef<HTMLPreElement>(null);

  // Live token overlay for code mode — re-highlighted per keystroke,
  // ~1-5ms at composer sizes. "plain" isn't a registered grammar, so
  // highlightNodes falls back to uncolored plaintext for it.
  const liveHighlight = useMemo(
    () => (mode === "code" ? highlightNodes(lang, code) : null),
    [mode, lang, code],
  );

  const syncHighlightScroll = () => {
    const ta = areaRef.current;
    const pre = highlightPreRef.current;
    if (!ta || !pre) return;
    pre.scrollTop = ta.scrollTop;
    pre.scrollLeft = ta.scrollLeft;
  };
  // Selection to restore after a line-wise indent/outdent rewrites the
  // controlled value (React resets the caret on programmatic value
  // changes; execCommand paths keep it natively and skip this).
  const pendingSelRef = useRef<[number, number] | null>(null);

  useLayoutEffect(() => {
    if (pendingSelRef.current && areaRef.current) {
      const [a, b] = pendingSelRef.current;
      areaRef.current.setSelectionRange(a, b);
      pendingSelRef.current = null;
    }
  }, [code]);

  // Menu closes on any click outside the trigger cluster.
  useEffect(() => {
    if (!menuOpen) return;
    const onDown = (e: MouseEvent) => {
      if (!triggerWrapRef.current?.contains(e.target as Node)) {
        setMenuOpen(false);
      }
    };
    document.addEventListener("mousedown", onDown);
    return () => document.removeEventListener("mousedown", onDown);
  }, [menuOpen]);

  const openMode = (m: "code" | "math") => {
    setMode(m);
    setMenuOpen(false);
  };

  const closePanel = () => {
    setMode(null);
    setCode("");
    setMath("");
  };

  const insert = () => {
    if (mode === "code") {
      const body = code.replace(/\n+$/, "");
      if (!body.trim()) return;
      onInsert(`\`\`\`${lang === "plain" ? "" : lang}\n${body}\n\`\`\``);
    } else if (mode === "math") {
      const tex = math.trim();
      if (!tex) return;
      onInsert(`$$${tex}$$`);
    }
    closePanel();
  };

  const onCodeKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    const ta = e.currentTarget;
    const { selectionStart: selS, selectionEnd: selE, value } = ta;

    if (e.key === "Escape") {
      e.preventDefault();
      closePanel();
      return;
    }
    if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      insert();
      return;
    }
    if (e.key === "Enter") {
      // Auto-indent: carry the current line's leading whitespace.
      // execCommand keeps the native undo stack intact.
      e.preventDefault();
      const lineStart = value.lastIndexOf("\n", selS - 1) + 1;
      const indent = value.slice(lineStart, selS).match(/^[ \t]*/)?.[0] ?? "";
      document.execCommand("insertText", false, "\n" + indent);
      return;
    }
    if (e.key === "Tab") {
      e.preventDefault();
      const multiline = value.slice(selS, selE).includes("\n");
      if (!e.shiftKey && !multiline) {
        document.execCommand("insertText", false, INDENT);
        return;
      }
      // Line-wise indent/outdent over every line the selection touches.
      const blockStart = value.lastIndexOf("\n", selS - 1) + 1;
      const head = value.slice(0, blockStart);
      const lines = value.slice(blockStart, selE).split("\n");
      const tail = value.slice(selE);
      let deltaFirst = 0;
      let deltaTotal = 0;
      const out = lines.map((line, idx) => {
        if (e.shiftKey) {
          const cut = line.match(/^ {1,2}/)?.[0].length ?? 0;
          if (idx === 0) deltaFirst = -cut;
          deltaTotal -= cut;
          return line.slice(cut);
        }
        if (idx === 0) deltaFirst = INDENT.length;
        deltaTotal += INDENT.length;
        return INDENT + line;
      });
      pendingSelRef.current = [
        Math.max(blockStart, selS + deltaFirst),
        Math.max(blockStart, selE + deltaTotal),
      ];
      setCode(head + out.join("\n") + tail);
      return;
    }
  };

  const onMathKeyDown = (e: React.KeyboardEvent<HTMLTextAreaElement>) => {
    if (e.key === "Escape") {
      e.preventDefault();
      closePanel();
    } else if ((e.ctrlKey || e.metaKey) && e.key === "Enter") {
      e.preventDefault();
      insert();
    }
  };

  const insertDisabled =
    mode === "code" ? !code.trim() : !math.trim();

  return (
    <>
      {/* The composer panel anchors to the input card (the nearest
          positioned ancestor), floating above it like the emoji
          picker — no layout shift in the card itself. */}
      {mode && (
        <div className="absolute inset-x-0 bottom-full z-10 mb-2 rounded-lg border border-border bg-bg-light p-3 shadow-float">
          <div className="mb-2 flex items-center justify-between gap-3">
            <div className="flex items-center gap-2">
              <span className="text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
                {mode === "code" ? "Code block" : "Math (LaTeX)"}
              </span>
              {mode === "code" && (
                // Native select (keyboard + a11y for free) with the
                // chrome hidden via appearance-none and our own chevron.
                <span className="relative inline-flex items-center">
                  <select
                    value={lang}
                    onChange={(e) => setLang(e.target.value)}
                    className="cursor-pointer appearance-none rounded-md border border-border bg-bg-darkest py-1 pl-2.5 pr-7 font-channel text-[12px] text-text-secondary outline-none transition-colors hover:text-text-primary focus:border-accent"
                  >
                    {LANGS.map((l) => (
                      <option key={l.value} value={l.value}>{l.label}</option>
                    ))}
                  </select>
                  <svg
                    width="10"
                    height="10"
                    viewBox="0 0 24 24"
                    fill="none"
                    stroke="currentColor"
                    strokeWidth="2.5"
                    strokeLinecap="round"
                    strokeLinejoin="round"
                    className="pointer-events-none absolute right-2.5 text-text-muted"
                  >
                    <polyline points="6 9 12 15 18 9" />
                  </svg>
                </span>
              )}
            </div>
            <button
              onClick={closePanel}
              title="Cancel"
              className="flex h-6 w-6 shrink-0 cursor-pointer items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
            >
              <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                <line x1="18" y1="6" x2="6" y2="18" />
                <line x1="6" y1="6" x2="18" y2="18" />
              </svg>
            </button>
          </div>

          {mode === "code" ? (
            // Overlay editor: the textarea owns input (native caret,
            // selection, undo) with transparent text; the pre behind it
            // renders the lowlight tokens in the exact same metrics
            // (same font/size/leading/padding, both non-wrapping), and
            // scroll positions are mirrored on every scroll event. The
            // trailing "\n" in the pre keeps a final empty line from
            // collapsing its height out of sync. Zero new dependencies
            // — the highlighter is the one messages already use.
            <div className="relative h-44 w-full overflow-hidden rounded-md border border-border bg-bg-darkest focus-within:border-accent">
              <pre
                ref={highlightPreRef}
                aria-hidden
                className="rich-codeblock pointer-events-none absolute inset-0 overflow-hidden whitespace-pre p-2.5 font-mono text-[12.5px] leading-[1.5] text-text-primary"
              >
                <code>
                  {liveHighlight}
                  {"\n"}
                </code>
              </pre>
              <textarea
                ref={areaRef}
                value={code}
                onChange={(e) => setCode(e.target.value)}
                onKeyDown={onCodeKeyDown}
                onScroll={syncHighlightScroll}
                autoFocus
                spellCheck={false}
                wrap="off"
                placeholder="// paste or type code — Tab indents, Ctrl+Enter inserts"
                className="absolute inset-0 h-full w-full resize-none overflow-auto whitespace-pre bg-transparent p-2.5 font-mono text-[12.5px] leading-[1.5] text-transparent outline-none placeholder:text-text-muted"
                style={{ caretColor: "var(--color-text-primary)" }}
              />
            </div>
          ) : (
            <>
              <textarea
                value={math}
                onChange={(e) => setMath(e.target.value)}
                onKeyDown={onMathKeyDown}
                autoFocus
                spellCheck={false}
                rows={2}
                placeholder={"\\int_0^1 x^2\\,dx — Ctrl+Enter inserts"}
                className="w-full resize-none rounded-md border border-border bg-bg-darkest p-2.5 font-mono text-[14px] leading-[1.5] text-text-primary outline-none placeholder:text-text-muted focus:border-accent"
              />
              {math.trim() && (
                // KaTeX sizes at 1.21× the surrounding font, so the
                // 16px here renders formulas at ~19px — comfortably
                // larger than chat text for proofreading a formula.
                <div className="mt-2 overflow-x-auto rounded-md border border-border-divider bg-bg-mid px-3 py-2.5 text-[16px]">
                  <MathTex tex={math.trim()} display />
                </div>
              )}
            </>
          )}

          <div className="mt-2 flex items-center justify-between gap-3">
            <span className="select-none text-[10.5px] text-text-muted">
              {mode === "code"
                ? "Tab indents · Shift+Tab outdents · Ctrl+Enter inserts"
                : "Rendered live above · Ctrl+Enter inserts"}
            </span>
            <div className="flex items-center gap-1.5">
              <button
                onClick={closePanel}
                className="cursor-pointer rounded-md px-2.5 py-1 text-[12px] text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
              >
                Cancel
              </button>
              <button
                onClick={insert}
                disabled={insertDisabled}
                className="cursor-pointer rounded-md bg-accent px-3 py-1 text-[12px] font-medium text-on-accent transition-colors hover:bg-accent-hover disabled:cursor-not-allowed disabled:opacity-40"
              >
                Insert
              </button>
            </div>
          </div>
        </div>
      )}

      <div className="relative" ref={triggerWrapRef}>
        <button
          onClick={() => (mode ? closePanel() : setMenuOpen((v) => !v))}
          title="Insert code or math"
          className={`flex h-[34px] w-[34px] cursor-pointer items-center justify-center rounded-md transition-colors ${
            menuOpen || mode
              ? "bg-surface-hover text-text-secondary"
              : "text-text-muted hover:text-text-secondary"
          }`}
        >
          <svg width="17" height="17" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
            <polyline points="16 18 22 12 16 6" />
            <polyline points="8 6 2 12 8 18" />
          </svg>
        </button>
        {menuOpen && (
          <div className="absolute bottom-full right-0 z-20 mb-2 w-40 rounded-md border border-border bg-bg-light p-1 shadow-float">
            {/* Accent-tinted icons, same treatment as ImageContextMenu's
                menu items. */}
            <button
              onClick={() => openMode("code")}
              className="flex w-full cursor-pointer items-center gap-2.5 rounded-sm px-2.5 py-1.5 text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
            >
              <span className="text-accent">
                <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <polyline points="16 18 22 12 16 6" />
                  <polyline points="8 6 2 12 8 18" />
                </svg>
              </span>
              Code
            </button>
            <button
              onClick={() => openMode("math")}
              className="flex w-full cursor-pointer items-center gap-2.5 rounded-sm px-2.5 py-1.5 text-[13px] text-text-secondary transition-colors hover:bg-surface-hover hover:text-text-primary"
            >
              <span className="flex w-[14px] items-center justify-center font-serif text-[14px] leading-none text-accent">Σ</span>
              Math
            </button>
          </div>
        )}
      </div>
    </>
  );
}
