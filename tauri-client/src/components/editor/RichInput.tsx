import {
  forwardRef,
  useCallback,
  useEffect,
  useImperativeHandle,
  useRef,
} from "react";
import data from "@emoji-mart/data";
import emojiRegex from "emoji-regex";
import { twemojiUrl } from "../emoji/Twemoji";

// Lightweight contentEditable editor. The DOM is the source of truth — React
// does NOT drive innerHTML after mount. Parent receives serialized text via
// `onChange`, and reads/clears content through the imperative handle.
//
// Two kinds of nodes live inside the editor:
//   - text nodes
//   - <img data-emoji="🎉"> atoms (contenteditable=false so they behave as
//     single graphemes under the caret)
//
// `serialize()` walks the DOM producing the plain string that goes over the
// wire — identical to what the old textarea held. `MessageText` on the
// receiver end renders that same string back to Twemoji, so input and output
// stay visually in sync.

export interface RichInputHandle {
  clear(): void;
  insertEmoji(native: string): void;
  setValue(text: string): void;
  focus(): void;
  getValue(): string;
  isEmpty(): boolean;
}

interface RichInputProps {
  onChange?: (value: string) => void;
  onEnter?: () => void;
  placeholder?: string;
  disabled?: boolean;
  maxHeight?: number;
  className?: string;
}

let _shortcodeMap: Map<string, string> | null = null;
function getShortcodeMap(): Map<string, string> {
  if (_shortcodeMap) return _shortcodeMap;
  _shortcodeMap = new Map();
  const emojis = (data as any).emojis as Record<
    string,
    { skins?: { native?: string }[] }
  >;
  for (const [id, emoji] of Object.entries(emojis)) {
    const native = emoji.skins?.[0]?.native;
    if (native) _shortcodeMap.set(id, native);
  }
  return _shortcodeMap;
}

const SHORTCODE_RE = /:([a-z0-9_+-]+):/gi;

function makeEmojiNode(native: string): HTMLImageElement {
  const img = document.createElement("img");
  img.src = twemojiUrl(native);
  img.alt = native;
  img.className = "rich-emoji";
  img.dataset.emoji = native;
  img.draggable = false;
  img.setAttribute("contenteditable", "false");
  return img;
}

// Drop an invisible marker span at the current caret. Because tokenize only
// mutates text nodes and the marker is an element, it rides along through any
// text-node replacements unchanged — after tokenize we just place the caret
// after it and remove it. This side-steps having to reconcile serialized
// character offsets across replacements of differing lengths (the bug where
// `:smile:` → emoji shrinks the text by 5 chars and breaks offset math).
function insertCaretSentinel(root: HTMLElement): HTMLElement | null {
  const sel = window.getSelection();
  if (!sel || !sel.rangeCount) return null;
  const range = sel.getRangeAt(0);
  if (!root.contains(range.endContainer)) return null;
  const marker = document.createElement("span");
  marker.dataset.caretSentinel = "1";
  marker.setAttribute("contenteditable", "false");
  const insertAt = range.cloneRange();
  insertAt.collapse(false);
  insertAt.insertNode(marker);
  return marker;
}

function restoreCaretFromSentinel(marker: HTMLElement) {
  const parent = marker.parentNode;
  if (!parent) return;
  const range = document.createRange();
  range.setStartAfter(marker);
  range.collapse(true);
  const sel = window.getSelection();
  if (sel) {
    sel.removeAllRanges();
    sel.addRange(range);
  }
  parent.removeChild(marker);
}

// Replace the editor's children with a single empty text node. A truly empty
// contentEditable on webkit-gtk (Tauri's Linux renderer) routinely leaves the
// caret in a stale state after `innerHTML = ""` — non-blinking, won't clear
// on click — because there's no concrete text position to anchor to. Keeping
// one empty text node gives the caret a stable home and keeps keyboard events
// dispatching through the normal path.
function resetToEmptyTextNode(root: HTMLElement) {
  const wasFocused = document.activeElement === root;
  if (wasFocused) window.getSelection()?.removeAllRanges();
  root.replaceChildren(document.createTextNode(""));
  if (wasFocused && root.firstChild) {
    const sel = window.getSelection();
    if (sel) {
      const range = document.createRange();
      range.setStart(root.firstChild, 0);
      range.collapse(true);
      sel.addRange(range);
    }
  }
}

// Find an emoji atom immediately adjacent to a collapsed caret position.
// webkit-gtk's native Backspace/Delete across contenteditable=false inline
// elements is unreliable (the caret visibly bounces to the line start/end,
// and the first keypress sometimes only inserts a newline instead of
// deleting). We intercept and remove the atom ourselves.
function findAdjacentEmojiAtom(
  range: Range,
  forward: boolean
): HTMLElement | null {
  if (!range.collapsed) return null;
  const { startContainer, startOffset } = range;

  if (startContainer.nodeType === Node.TEXT_NODE) {
    const text = startContainer as Text;
    if (forward) {
      if (startOffset !== (text.textContent?.length ?? 0)) return null;
      const next = text.nextSibling;
      if (next && next.nodeType === Node.ELEMENT_NODE) {
        const el = next as HTMLElement;
        if (el.dataset.emoji) return el;
      }
    } else {
      if (startOffset !== 0) return null;
      const prev = text.previousSibling;
      if (prev && prev.nodeType === Node.ELEMENT_NODE) {
        const el = prev as HTMLElement;
        if (el.dataset.emoji) return el;
      }
    }
  } else if (startContainer.nodeType === Node.ELEMENT_NODE) {
    const parent = startContainer as HTMLElement;
    const idx = forward ? startOffset : startOffset - 1;
    const child = parent.childNodes[idx];
    if (child && child.nodeType === Node.ELEMENT_NODE) {
      const el = child as HTMLElement;
      if (el.dataset.emoji) return el;
    }
  }
  return null;
}

// True when the editor has no user-visible content. A lone <br> is treated as
// empty — Chromium often leaves a "padding <br>" behind after Ctrl+A+Delete.
function isVisuallyEmpty(root: HTMLElement): boolean {
  for (const child of Array.from(root.childNodes)) {
    if (child.nodeType === Node.TEXT_NODE) {
      if ((child.textContent ?? "").length > 0) return false;
    } else if (child.nodeType === Node.ELEMENT_NODE) {
      const el = child as HTMLElement;
      if (el.dataset.emoji) return false;
      if (el.tagName === "BR") continue;
      return false;
    }
  }
  return true;
}

// Walk every text node (but not *inside* emoji atoms) and convert any matched
// Unicode emoji / :shortcode: to atom img nodes. Returns true if anything
// changed.
function tokenize(root: HTMLElement): boolean {
  const shortMap = getShortcodeMap();
  const textNodes: Text[] = [];

  (function walk(node: Node) {
    if (node.nodeType === Node.TEXT_NODE) {
      textNodes.push(node as Text);
    } else if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as HTMLElement;
      if (el.dataset.emoji) return;
      for (const child of Array.from(el.childNodes)) walk(child);
    }
  })(root);

  let changed = false;

  for (const textNode of textNodes) {
    const text = textNode.textContent ?? "";
    if (!text) continue;

    type Match = { start: number; end: number; native: string };
    const matches: Match[] = [];

    for (const m of text.matchAll(emojiRegex())) {
      const start = m.index ?? 0;
      matches.push({ start, end: start + m[0].length, native: m[0] });
    }

    for (const m of text.matchAll(SHORTCODE_RE)) {
      const start = m.index ?? 0;
      const native = shortMap.get(m[1].toLowerCase());
      if (native) matches.push({ start, end: start + m[0].length, native });
    }

    if (matches.length === 0) continue;

    matches.sort((a, b) => a.start - b.start);
    const cleaned: Match[] = [];
    let lastEnd = 0;
    for (const m of matches) {
      if (m.start >= lastEnd) {
        cleaned.push(m);
        lastEnd = m.end;
      }
    }

    const fragments: Node[] = [];
    let cursor = 0;
    for (const m of cleaned) {
      if (m.start > cursor) {
        fragments.push(document.createTextNode(text.slice(cursor, m.start)));
      }
      fragments.push(makeEmojiNode(m.native));
      cursor = m.end;
    }
    if (cursor < text.length) {
      fragments.push(document.createTextNode(text.slice(cursor)));
    }

    const parent = textNode.parentNode;
    if (!parent) continue;
    for (const frag of fragments) parent.insertBefore(frag, textNode);
    parent.removeChild(textNode);
    changed = true;
  }

  return changed;
}

function serialize(root: HTMLElement): string {
  let out = "";
  (function walk(node: Node) {
    if (node.nodeType === Node.TEXT_NODE) {
      out += node.textContent;
    } else if (node.nodeType === Node.ELEMENT_NODE) {
      const el = node as HTMLElement;
      if (el.dataset.emoji) {
        out += el.dataset.emoji;
      } else if (el.tagName === "BR") {
        out += "\n";
      } else if (el.tagName === "DIV" || el.tagName === "P") {
        if (out.length > 0 && !out.endsWith("\n")) out += "\n";
        for (const child of Array.from(el.childNodes)) walk(child);
      } else {
        for (const child of Array.from(el.childNodes)) walk(child);
      }
    }
  })(root);
  return out;
}

const RichInput = forwardRef<RichInputHandle, RichInputProps>(function RichInput(
  { onChange, onEnter, placeholder, disabled, maxHeight = 160, className },
  ref
) {
  const editorRef = useRef<HTMLDivElement>(null);
  const composingRef = useRef(false);
  // Set by keydown when it handled an Enter-send; read by beforeinput to
  // preempt webkit-gtk's line-break insertion even if keydown's
  // preventDefault was ignored (seen intermittently on Tauri Linux builds).
  const enterHandledRef = useRef(false);
  // Last-known caret range while the editor was focused. Used to re-anchor
  // after the picker's search input steals focus — without this, inserting an
  // emoji falls back to "end of editor" and the visible caret snaps away.
  const lastRangeRef = useRef<Range | null>(null);

  // Seed the editor with a single empty text node so the caret has a concrete
  // anchor from first render. See resetToEmptyTextNode for rationale.
  useEffect(() => {
    const el = editorRef.current;
    if (!el) return;
    if (el.childNodes.length === 0) {
      el.appendChild(document.createTextNode(""));
    }
    el.setAttribute("data-empty", "true");
  }, []);

  // Track the last selection that lived inside the editor. Selections outside
  // the editor (picker search input, clicks elsewhere) are ignored — so on a
  // subsequent insertEmoji we can restore the caret to where the user left it.
  useEffect(() => {
    const handle = () => {
      const el = editorRef.current;
      if (!el) return;
      const sel = window.getSelection();
      if (!sel || !sel.rangeCount) return;
      const range = sel.getRangeAt(0);
      if (
        el.contains(range.startContainer) &&
        el.contains(range.endContainer)
      ) {
        lastRangeRef.current = range.cloneRange();
      }
    };
    document.addEventListener("selectionchange", handle);
    return () => document.removeEventListener("selectionchange", handle);
  }, []);

  const syncEmptyState = useCallback((el: HTMLElement) => {
    if (isVisuallyEmpty(el)) {
      el.setAttribute("data-empty", "true");
    } else {
      el.removeAttribute("data-empty");
    }
  }, []);

  const notifyChange = useCallback(() => {
    const el = editorRef.current;
    if (!el) return;
    syncEmptyState(el);
    onChange?.(serialize(el));
  }, [onChange, syncEmptyState]);

  const handleInput = useCallback(() => {
    const el = editorRef.current;
    if (!el) return;
    if (composingRef.current) {
      notifyChange();
      return;
    }
    const marker = insertCaretSentinel(el);
    tokenize(el);
    if (marker) restoreCaretFromSentinel(marker);
    // Normalize back to a single empty text node whenever the editor is
    // visually empty — strips padding <br>s and keeps a stable caret anchor.
    if (isVisuallyEmpty(el)) {
      const normalized =
        el.childNodes.length === 1 &&
        el.firstChild?.nodeType === Node.TEXT_NODE;
      if (!normalized) resetToEmptyTextNode(el);
    }
    notifyChange();
  }, [notifyChange]);

  const handleKeyDown = useCallback(
    (e: React.KeyboardEvent<HTMLDivElement>) => {
      if (e.key === "Enter" && !e.shiftKey && !composingRef.current) {
        e.preventDefault();
        enterHandledRef.current = true;
        onEnter?.();
        return;
      }

      if (
        (e.key === "Backspace" || e.key === "Delete") &&
        !composingRef.current
      ) {
        const el = editorRef.current;
        if (!el) return;
        const sel = window.getSelection();
        if (!sel || !sel.rangeCount) return;
        const range = sel.getRangeAt(0);
        if (!range.collapsed) return;
        if (!el.contains(range.startContainer)) return;

        const atom = findAdjacentEmojiAtom(range, e.key === "Delete");
        if (!atom) return;

        e.preventDefault();
        const atomParent = atom.parentNode;
        if (!atomParent) return;
        const atomIndex = Array.prototype.indexOf.call(
          atomParent.childNodes,
          atom
        );
        atomParent.removeChild(atom);

        const newRange = document.createRange();
        newRange.setStart(atomParent, atomIndex);
        newRange.collapse(true);
        sel.removeAllRanges();
        sel.addRange(newRange);

        if (isVisuallyEmpty(el)) resetToEmptyTextNode(el);
        notifyChange();
      }
    },
    [onEnter, notifyChange]
  );

  const handleBeforeInput = useCallback(
    (e: React.FormEvent<HTMLDivElement>) => {
      const native = e.nativeEvent as InputEvent;
      const t = native.inputType;
      if (
        (t === "insertParagraph" || t === "insertLineBreak") &&
        enterHandledRef.current
      ) {
        e.preventDefault();
      }
      enterHandledRef.current = false;
    },
    []
  );

  const handlePaste = useCallback(
    (e: React.ClipboardEvent<HTMLDivElement>) => {
      e.preventDefault();
      const text = e.clipboardData.getData("text/plain");
      if (!text) return;
      const sel = window.getSelection();
      const el = editorRef.current;
      if (!el) return;
      el.focus();
      if (sel && sel.rangeCount && el.contains(sel.getRangeAt(0).startContainer)) {
        const range = sel.getRangeAt(0);
        range.deleteContents();
        const node = document.createTextNode(text);
        range.insertNode(node);
        range.setStartAfter(node);
        range.collapse(true);
        sel.removeAllRanges();
        sel.addRange(range);
      } else {
        el.appendChild(document.createTextNode(text));
      }
      handleInput();
    },
    [handleInput]
  );

  useImperativeHandle(
    ref,
    () => ({
      clear: () => {
        const el = editorRef.current;
        if (!el) return;
        resetToEmptyTextNode(el);
        syncEmptyState(el);
        onChange?.("");
      },
      setValue: (text: string) => {
        const el = editorRef.current;
        if (!el) return;
        if (!text) {
          resetToEmptyTextNode(el);
          syncEmptyState(el);
          onChange?.("");
          return;
        }
        el.replaceChildren(document.createTextNode(text));
        tokenize(el);
        if (document.activeElement === el) {
          const sel = window.getSelection();
          if (sel) {
            const range = document.createRange();
            range.selectNodeContents(el);
            range.collapse(false);
            sel.removeAllRanges();
            sel.addRange(range);
          }
        }
        syncEmptyState(el);
        onChange?.(serialize(el));
      },
      insertEmoji: (native: string) => {
        const el = editorRef.current;
        if (!el) return;
        const sel = window.getSelection();
        let range: Range;
        // Prefer cached editor range over live selection: calling el.focus()
        // on webkit-gtk fabricates a collapsed selection at the editor start,
        // which would otherwise masquerade as "live selection in editor" and
        // clobber the user's real caret position from before the picker took
        // focus.
        if (
          lastRangeRef.current &&
          el.contains(lastRangeRef.current.startContainer) &&
          el.contains(lastRangeRef.current.endContainer)
        ) {
          range = lastRangeRef.current.cloneRange();
        } else if (
          sel &&
          sel.rangeCount &&
          el.contains(sel.getRangeAt(0).startContainer)
        ) {
          range = sel.getRangeAt(0);
        } else {
          range = document.createRange();
          range.selectNodeContents(el);
          range.collapse(false);
        }
        el.focus();
        if (sel) {
          sel.removeAllRanges();
          sel.addRange(range);
        }
        range.deleteContents();
        const img = makeEmojiNode(native);
        range.insertNode(img);
        range.setStartAfter(img);
        range.collapse(true);
        if (sel) {
          sel.removeAllRanges();
          sel.addRange(range);
        }
        lastRangeRef.current = range.cloneRange();
        notifyChange();
      },
      focus: () => {
        editorRef.current?.focus();
      },
      getValue: () => {
        const el = editorRef.current;
        return el ? serialize(el) : "";
      },
      isEmpty: () => {
        const el = editorRef.current;
        return el ? serialize(el).trim().length === 0 : true;
      },
    }),
    [onChange, notifyChange, syncEmptyState]
  );

  return (
    <div
      ref={editorRef}
      contentEditable={!disabled}
      suppressContentEditableWarning
      role="textbox"
      aria-multiline="true"
      data-placeholder={placeholder ?? ""}
      onInput={handleInput}
      onBeforeInput={handleBeforeInput}
      onKeyDown={handleKeyDown}
      onPaste={handlePaste}
      onCompositionStart={() => {
        composingRef.current = true;
      }}
      onCompositionEnd={() => {
        composingRef.current = false;
        handleInput();
      }}
      className={`rich-input outline-none ${disabled ? "opacity-50" : ""} ${className ?? ""}`}
      style={{ maxHeight, overflowY: "auto" }}
    />
  );
});

export default RichInput;
