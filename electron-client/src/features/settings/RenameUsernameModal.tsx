import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { toast } from "../../stores/toastStore";

// Change username (uid design, stage 5). Central confirms the password,
// applies the registration rules and reserves the old name for this
// account for 14 days; native saves the new name for auto-login and hands
// the fresh token to every community, which renames the live session in
// place (voice and streams carry on: the media plane goes by uid), and the
// `user_renamed` event re-keys the stores (features/users/applyRename.ts).
// Same chrome as PassphraseModal.

const NAME_RULE = /^[a-z0-9._-]{3,32}$/;

export default function RenameUsernameModal({ open, onClose }: { open: boolean; onClose: () => void }) {
  const current = useAuthStore((s) => s.username);

  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [name, setName] = useState("");
  const [password, setPassword] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setMounted(true);
      setName("");
      setPassword("");
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
    const next = name.trim();
    if (!NAME_RULE.test(next)) {
      setError("Use 3–32 lowercase letters, digits, '.', '_' or '-'.");
      return;
    }
    if (next === current) {
      setError("That's already your username.");
      return;
    }
    if (!password) {
      setError("Enter your password to confirm.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      const res = await invoke<{ username: string }>("rename_account", { newUsername: next, password });
      toast.success("Username changed", `You're now ${res.username}.`);
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
        aria-label="Change username"
      >
        <h2 className="text-[16px] font-semibold text-text-primary">Change username</h2>
        <p className="mt-2 text-[12px] leading-[1.55] text-text-muted">
          Friends, messages, servers and encryption stay with your account. Your old name stays reserved
          for you for 14 days, then anyone can take it. You can change your username twice a day.
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
            type="text"
            autoComplete="username"
            spellCheck={false}
            placeholder={current ? `New username (now ${current})` : "New username"}
            value={name}
            maxLength={32}
            onChange={(e) => setName(e.target.value.toLowerCase())}
            disabled={busy}
            className={inputClass}
          />
          <input
            type="password"
            autoComplete="current-password"
            placeholder="Current password"
            value={password}
            onChange={(e) => setPassword(e.target.value)}
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
              {busy ? "Changing…" : "Change username"}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
