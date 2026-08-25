import { useEffect } from "react";
import type { RefObject } from "react";
import type { RichInputHandle } from "../../components/editor/RichInput";
import { useUiStore } from "../../stores/uiStore";

/**
 * Discord-style "type anywhere to compose". While a text channel or DM is
 * open, pressing a printable key (when focus isn't already in a text field)
 * focuses the composer so the keystroke lands there — no click required.
 * ArrowUp with nothing focused starts editing the latest own message.
 *
 * Mount once per chat surface, passing its composer ref and the same
 * "edit latest own message" callback used by the composer's own ArrowUp.
 */
export function useTypeToFocusComposer(
  editorRef: RefObject<RichInputHandle | null>,
  onArrowUpEmpty?: () => void,
) {
  useEffect(() => {
    const onKeyDown = (e: KeyboardEvent) => {
      // Leave keyboard shortcuts and IME composition untouched.
      if (e.ctrlKey || e.metaKey || e.altKey || e.isComposing) return;
      // A modal/overlay owns the keyboard while it's up.
      if (useUiStore.getState().activeModal) return;
      // Already typing somewhere editable — the composer, an inline
      // message-edit box, a search field, etc. Don't steal focus.
      const el = document.activeElement as HTMLElement | null;
      if (
        el &&
        (el.tagName === "INPUT" || el.tagName === "TEXTAREA" || el.isContentEditable)
      ) {
        return;
      }

      // ArrowUp from an unfocused view → edit the latest own message.
      if (e.key === "ArrowUp" && onArrowUpEmpty) {
        e.preventDefault();
        onArrowUpEmpty();
        return;
      }

      // A single printable character (letter, digit, punctuation, space).
      // Focus the composer and let the browser route the character into the
      // now-focused contentEditable — do NOT preventDefault.
      if (e.key.length === 1) {
        editorRef.current?.focus();
      }
    };
    window.addEventListener("keydown", onKeyDown);
    return () => window.removeEventListener("keydown", onKeyDown);
  }, [editorRef, onArrowUpEmpty]);
}
