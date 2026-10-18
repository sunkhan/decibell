import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../../lib/ipc";
import { toast } from "../../stores/toastStore";

// Change password (docs/superpowers/specs/2026-10-19-devices-and-password-
// design.md). Central checks the current password (a wrong one counts as a
// failed sign-in) and signs every other device out; this device stays
// signed in, and native saves the new password for auto-login if a sign-in
// was saved. Same chrome as RenameUsernameModal.

/// The server's rule (AuthManager::MIN_PASSWORD), checked here first.
const MIN_PASSWORD = 8;

export default function ChangePasswordModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [current, setCurrent] = useState("");
  const [next, setNext] = useState("");
  const [repeat, setRepeat] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setMounted(true);
      setCurrent("");
      setNext("");
      setRepeat("");
      setError(null);
      setBusy(false);
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
      const t = window.setTimeout(() => inputRef.current?.focus(), 60);
      return () => window.clearTimeout(t);
    }
    setVisible(false);
    return undefined;
  }, [open]);

  const handleTransitionEnd = useCallback(
    (e: React.TransitionEvent) => {
      if (e.target === e.currentTarget && !visible) setMounted(false);
    },
    [visible],
  );

  const cancel = () => {
    if (open && !busy) onClose();
  };

  const submit = async () => {
    if (!open || busy) return;
    if (!current) {
      setError("Enter your current password.");
      return;
    }
    if (next.length < MIN_PASSWORD) {
      setError(`Your new password must be at least ${MIN_PASSWORD} characters.`);
      return;
    }
    if (next !== repeat) {
      setError("The new passwords don't match.");
      return;
    }
    if (next === current) {
      setError("That's already your password.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await invoke<{ signedOut: number }>("change_password", {
        currentPassword: current,
        newPassword: next,
      });
      const n = res.signedOut;
      toast.success(
        "Password changed",
        n === 0 ? undefined : n === 1 ? "1 other device was signed out." : `${n} other devices were signed out.`,
      );
      onClose();
    } catch (err) {
      setError(String(err).replace(/^Error:\s*/, ""));
      setBusy(false);
    }
  };

  useEffect(() => {
    if (!open) return;
    const onKeyDown = (e: KeyboardEvent) => {
      if (e.key === "Escape") {
        e.preventDefault();
        cancel();
      }
    };
    document.addEventListener("keydown", onKeyDown);
    return () => document.removeEventListener("keydown", onKeyDown);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, busy]);

  if (!mounted) return null;

  const inputClass =
    "w-full rounded-sm border border-border bg-bg-dark px-3 py-2 text-[13px] text-text-primary outline-none transition-colors placeholder:text-text-muted focus:border-accent";

  return (
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onClick={cancel}
      onTransitionEnd={handleTransitionEnd}
    >
      <div
        className="w-full max-w-[440px] rounded-xl border border-border bg-bg-secondary p-6 shadow-modal transition-[opacity,transform] duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label="Change password"
      >
        <h2 className="text-[16px] font-semibold text-text-primary">Change password</h2>
        <p className="mt-2 text-[12px] leading-[1.55] text-text-muted">
          Your other devices will be signed out and need the new password. This device stays signed in. Your
          encryption passphrase doesn't change.
        </p>

        <form
          className="mt-4 flex flex-col gap-2.5"
          onSubmit={(e) => {
            e.preventDefault();
            void submit();
          }}
        >
          <input
            ref={inputRef}
            type="password"
            autoComplete="current-password"
            placeholder="Current password"
            value={current}
            onChange={(e) => setCurrent(e.target.value)}
            disabled={busy}
            className={inputClass}
          />
          <input
            type="password"
            autoComplete="new-password"
            placeholder={`New password (at least ${MIN_PASSWORD} characters)`}
            value={next}
            onChange={(e) => setNext(e.target.value)}
            disabled={busy}
            className={inputClass}
          />
          <input
            type="password"
            autoComplete="new-password"
            placeholder="New password again"
            value={repeat}
            onChange={(e) => setRepeat(e.target.value)}
            disabled={busy}
            className={inputClass}
          />
          {error && <div className="text-[12px] text-error">{error}</div>}
          <div className="mt-3 flex justify-end gap-2">
            <button
              type="button"
              onClick={cancel}
              disabled={busy}
              className="rounded-md border border-border bg-transparent px-4 py-2 text-[13px] font-medium text-text-primary transition-colors hover:bg-surface-hover disabled:opacity-50"
            >
              Cancel
            </button>
            <button
              type="submit"
              disabled={busy}
              className="rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
            >
              {busy ? "Changing…" : "Change password"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
