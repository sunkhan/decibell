// "Set a custom status" (docs/superpowers/specs/2026-10-12-status-messages-
// design.md): an emoji and a line of text shown under the user's name while
// they're online, cleared after a chosen time. Opened from the status menu;
// rides `activeModal === "custom-status"`.

import { useCallback, useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { useUiStore } from "../../stores/uiStore";
import {
  normalizeStatusText,
  STATUS_TEXT_MAX,
  usePresenceStore,
  type StatusMessage,
} from "../../stores/presenceStore";
import Select from "../../components/Select";
import Twemoji from "../../components/emoji/Twemoji";
import EmojiPicker, { EMOJI_PICKER_SIZE } from "../chat/EmojiPicker";

type ClearAfter = "today" | "4h" | "1h" | "30m" | "never";

const CLEAR_AFTER_OPTIONS: { value: ClearAfter; label: string }[] = [
  { value: "today", label: "Today" },
  { value: "4h", label: "4 hours" },
  { value: "1h", label: "1 hour" },
  { value: "30m", label: "30 minutes" },
  { value: "never", label: "Don't clear" },
];

/// Seconds until the message should go; 0 = never. "Today" runs to local
/// midnight.
function clearAfterSeconds(choice: ClearAfter): number {
  switch (choice) {
    case "4h":
      return 4 * 3600;
    case "1h":
      return 3600;
    case "30m":
      return 30 * 60;
    case "never":
      return 0;
    case "today": {
      const midnight = new Date();
      midnight.setHours(24, 0, 0, 0);
      return Math.max(60, Math.round((midnight.getTime() - Date.now()) / 1000));
    }
  }
}

/// Optimistic, like the status itself: our own copy updates at once
/// (normalised the way central will), central saves it and tells everyone.
export function saveStatusMessage(message: StatusMessage | null, clearAfter: number) {
  const text = message ? normalizeStatusText(message.text) : "";
  const emoji = message?.emoji ?? "";
  const next = text || emoji ? { text, emoji } : null;
  const after = next ? clearAfter : 0;
  usePresenceStore.getState().setOwnMessage(next, after > 0 ? Date.now() + after * 1000 : null);
  invoke("set_status_message", { text, emoji, clearAfter: after }).catch(console.error);
}

const length = (s: string) => Array.from(s).length;

export default function CustomStatusModal() {
  const open = useUiStore((s) => s.activeModal === "custom-status");
  const closeModal = useUiStore((s) => s.closeModal);
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  // One form per opening, so it starts from the saved message; it stays
  // mounted (inert) through the fade-out.
  const [opening, setOpening] = useState(0);

  // Same open / close transition as ConfirmModal.
  useEffect(() => {
    if (open) {
      setOpening((n) => n + 1);
      setMounted(true);
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
    } else {
      setVisible(false);
    }
  }, [open]);

  if (!mounted) return null;
  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={() => open && closeModal()}
      onTransitionEnd={(e) => {
        if (e.target === e.currentTarget && !visible) setMounted(false);
      }}
    >
      <div
        className={`w-full max-w-[440px] rounded-xl border border-border bg-bg-secondary p-6 shadow-modal transition-[opacity,transform] duration-300 ${
          open ? "" : "pointer-events-none"
        }`}
        style={{ opacity: visible ? 1 : 0, transform: visible ? "scale(1)" : "scale(0.95)" }}
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label="Set a custom status"
      >
        <CustomStatusForm key={opening} active={open} onClose={closeModal} />
      </div>
    </div>
  );
}

function CustomStatusForm({ active, onClose }: { active: boolean; onClose: () => void }) {
  const saved = usePresenceStore.getState().ownMessage;
  const savedClearsAt = usePresenceStore.getState().ownMessageClearsAt;
  const [text, setText] = useState(saved?.text ?? "");
  const [emoji, setEmoji] = useState(saved?.emoji ?? "");
  const [clearAfter, setClearAfter] = useState<ClearAfter>(
    saved && savedClearsAt === null ? "never" : "today",
  );
  // Where the emoji picker sits (viewport px); null = closed.
  const [pickerAt, setPickerAt] = useState<{ top: number; left: number } | null>(null);
  const pickerOpenRef = useRef(false);
  pickerOpenRef.current = pickerAt !== null;
  const inputRef = useRef<HTMLInputElement>(null);
  const fieldRef = useRef<HTMLDivElement>(null);
  const emojiButtonRef = useRef<HTMLButtonElement>(null);

  // Below the field, else above it, else as low as the window allows
  // (over the field) — the dialog sits mid-window, and the picker is
  // taller than the room on either side of a short one.
  const togglePicker = () => {
    if (pickerAt || !fieldRef.current) {
      setPickerAt(null);
      return;
    }
    const r = fieldRef.current.getBoundingClientRect();
    const { width, height } = EMOJI_PICKER_SIZE;
    const gap = 8;
    const top =
      window.innerHeight - r.bottom >= height + 2 * gap
        ? r.bottom + gap
        : r.top >= height + 2 * gap
          ? r.top - gap - height
          : Math.max(gap, window.innerHeight - height - gap);
    const left = Math.max(gap, Math.min(r.left, window.innerWidth - width - gap));
    setPickerAt({ top, left });
  };

  useEffect(() => {
    inputRef.current?.focus();
  }, []);

  const save = useCallback(() => {
    if (!active) return;
    saveStatusMessage({ text, emoji }, clearAfterSeconds(clearAfter));
    onClose();
  }, [active, text, emoji, clearAfter, onClose]);

  // Esc closes — unless the emoji picker is open, which closes itself.
  useEffect(() => {
    if (!active) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape" && !pickerOpenRef.current) {
        e.preventDefault();
        onClose();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [active, onClose]);

  const hasContent = text.trim().length > 0 || emoji.length > 0;

  return (
    <>
      <h2 className="text-[16px] font-semibold text-text-primary">Set a custom status</h2>
      <p className="mt-1 text-[13px] leading-snug text-text-muted">
        Shows under your name while you're online.
      </p>

      <label
        htmlFor="custom-status-text"
        className="mb-2 mt-5 block text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted"
      >
        What's on your mind?
      </label>
      <div
        ref={fieldRef}
        className="flex items-center gap-1 rounded-md border border-border bg-bg-dark px-1.5 transition-colors focus-within:border-accent focus-within:shadow-ring"
      >
        <button
          ref={emojiButtonRef}
          type="button"
          onClick={togglePicker}
          title={emoji ? "Change emoji" : "Add an emoji"}
          aria-label={emoji ? "Change emoji" : "Add an emoji"}
          className="flex h-8 w-8 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
        >
          {emoji ? (
            <Twemoji emoji={emoji} size={20} />
          ) : (
            <svg width="18" height="18" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" aria-hidden="true">
              <circle cx="12" cy="12" r="10" />
              <path d="M8 14s1.5 2 4 2 4-2 4-2" />
              <line x1="9" y1="9" x2="9.01" y2="9" />
              <line x1="15" y1="9" x2="15.01" y2="9" />
            </svg>
          )}
        </button>
        <input
          id="custom-status-text"
          ref={inputRef}
          type="text"
          value={text}
          onChange={(e) => setText(Array.from(e.target.value).slice(0, STATUS_TEXT_MAX).join(""))}
          onKeyDown={(e) => {
            if (e.key === "Enter") {
              e.preventDefault();
              save();
            }
          }}
          placeholder="What's happening?"
          className="min-w-0 flex-1 bg-transparent py-2.5 text-sm text-text-primary outline-none placeholder:text-text-muted"
        />
        {hasContent && (
          <button
            type="button"
            onClick={() => {
              setText("");
              setEmoji("");
              inputRef.current?.focus();
            }}
            title="Clear"
            aria-label="Clear"
            className="flex h-7 w-7 shrink-0 items-center justify-center rounded-sm text-text-muted transition-colors hover:bg-surface-hover hover:text-text-primary"
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" aria-hidden="true">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </button>
        )}
        {/* Portaled: the dialog's scale transform would make `fixed`
            relative to the card. */}
        {pickerAt &&
          createPortal(
            <EmojiPicker
              at={pickerAt}
              triggerRef={emojiButtonRef}
              onSelect={(picked) => {
                setEmoji(picked);
                setPickerAt(null);
                inputRef.current?.focus();
              }}
              onClose={() => setPickerAt(null)}
            />,
            document.body,
          )}
      </div>
      <div className="mt-1.5 text-right font-meta text-micro text-text-muted">
        {length(text)}/{STATUS_TEXT_MAX}
      </div>

      <div className="mb-2 mt-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
        Clear after
      </div>
      <Select
        aria-label="Clear after"
        value={clearAfter}
        onChange={setClearAfter}
        options={CLEAR_AFTER_OPTIONS}
      />

      <div className="mt-6 flex flex-wrap justify-end gap-2">
        <button
          onClick={onClose}
          className="rounded-md border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
        >
          Cancel
        </button>
        <button
          onClick={save}
          className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
        >
          Save
        </button>
      </div>
    </>
  );
}
