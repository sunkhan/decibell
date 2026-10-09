import { useCallback, useEffect, useRef, useState, type ReactNode } from "react";

interface Props {
  /// Driven by the parent (typically `activeModal === "…"`). The shell
  /// stays mounted through the close transition and unmounts itself.
  open: boolean;
  title: string;
  /// Body copy. Frozen while the dialog fades out, so a parent clearing
  /// its own state on confirm doesn't blank the text mid-animation.
  children: ReactNode;
  confirmLabel: string;
  /// The destructive button and Enter.
  onConfirm: () => void;
  /// Cancel, Esc and a backdrop click.
  onCancel: () => void;
  /// "danger" (default) for destructive confirmations, "accent" when the
  /// confirm button is an ordinary choice.
  confirmTone?: "danger" | "accent";
  /// Greys the confirm button out (and Enter does nothing).
  confirmDisabled?: boolean;
  /// An optional middle choice between Cancel and the confirm button.
  secondaryLabel?: string;
  onSecondary?: () => void;
}

/// Shared shell for the small destructive-action confirmations (delete
/// message, leave server). Same chrome as the settings modals: a plain
/// darkening backdrop (no blur), 300ms fade + scale in and out. Render
/// it unconditionally and drive `open` — gating the mount on `open`
/// would unmount it the instant it closes and skip the fade-out.
export default function ConfirmModal({
  open,
  title,
  children,
  confirmLabel,
  onConfirm,
  onCancel,
  confirmTone = "danger",
  confirmDisabled = false,
  secondaryLabel,
  onSecondary,
}: Props) {
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);

  useEffect(() => {
    if (open) {
      setMounted(true);
      // Double rAF (as SettingsModal): a single rAF fires before the
      // next paint, so the opacity-0/scale-95 starting frame never
      // renders and the open transition pops instead of animating.
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
    } else {
      setVisible(false);
    }
  }, [open]);

  // Unmount once the backdrop's own fade-out ends. Only the backdrop's
  // transition counts: a button's colour transition inside the card
  // bubbles the same event and would cut the close short.
  const handleTransitionEnd = useCallback(
    (e: React.TransitionEvent) => {
      if (e.target === e.currentTarget && !visible) setMounted(false);
    },
    [visible],
  );

  // Ignore input during the close transition — the buttons are still
  // painted, and the parent may already have cleared its state.
  const confirm = () => {
    if (open && !confirmDisabled) onConfirm();
  };
  const cancel = () => {
    if (open) onCancel();
  };
  const secondary = () => {
    if (open) onSecondary?.();
  };

  // Esc cancels, Enter confirms — on the document so they work with
  // nothing inside the dialog focused.
  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        onCancel();
      } else if (e.key === "Enter") {
        e.preventDefault();
        if (!confirmDisabled) onConfirm();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
  }, [open, onConfirm, onCancel, confirmDisabled]);

  // Last body rendered while open; shown during the fade-out instead of
  // whatever the parent has now.
  const frozen = useRef<ReactNode>(null);
  if (open) frozen.current = children;

  if (!mounted) return null;

  return (
    <div
      className="fixed inset-0 z-50 flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={cancel}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className={`w-full ${secondaryLabel ? "max-w-lg" : "max-w-[400px]"} rounded-xl border border-border bg-bg-secondary p-6 shadow-modal transition-[opacity,transform] duration-300`}
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="text-[16px] font-semibold text-text-primary">{title}</h2>
        {open ? children : frozen.current}
        <div className="mt-5 flex flex-wrap justify-end gap-2">
          <button
            onClick={cancel}
            className="rounded-md border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
          >
            Cancel
          </button>
          {secondaryLabel && (
            <button
              onClick={secondary}
              className="rounded-md border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover"
            >
              {secondaryLabel}
            </button>
          )}
          <button
            onClick={confirm}
            disabled={confirmDisabled}
            className={
              confirmTone === "accent"
                ? "rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
                : "rounded-md bg-error px-4 py-2 text-[13px] font-semibold text-white transition-colors hover:bg-error/90 disabled:opacity-50"
            }
          >
            {confirmLabel}
          </button>
        </div>
      </div>
    </div>
  );
}
