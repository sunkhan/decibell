import { useCallback, useEffect, useRef, useState } from "react";
import { invoke } from "../../lib/ipc";
import { useE2eeStore, type PassphraseMode } from "../../stores/e2eeStore";
import { toast } from "../../stores/toastStore";

// The one dialog behind every E2EE key operation — set up, unlock,
// change passphrase, reset — driven by e2eeStore.passphraseModal (the
// IncomingCallModal nullable-field pattern; mounted once in MainLayout).
// Same chrome as ConfirmModal: darkening backdrop, 300 ms fade + scale.
// Native does the actual work (`e2ee_setup` etc.) and pushes the new
// status; this only collects the passphrase and shows errors inline.

const MIN_CHARS = 10;

const COPY: Record<
  PassphraseMode,
  { title: string; body: string; confirm: string; needsConfirm: boolean; danger: boolean }
> = {
  setup: {
    title: "Set up end-to-end encryption",
    body:
      "Your direct messages will be encrypted on this device so only you and the person you're talking to can read them — not even the server. Choose a passphrase to protect your keys: you'll need it to read encrypted messages on another device. A lost passphrase cannot be recovered.",
    confirm: "Turn on encryption",
    needsConfirm: true,
    danger: false,
  },
  unlock: {
    title: "Unlock encrypted messages",
    body: "Enter your encryption passphrase to read and send encrypted messages on this device.",
    confirm: "Unlock",
    needsConfirm: false,
    danger: false,
  },
  change: {
    title: "Change encryption passphrase",
    body: "Choose a new passphrase. Your keys and message history are unaffected; other devices will need the new passphrase to unlock.",
    confirm: "Change passphrase",
    needsConfirm: true,
    danger: false,
  },
  reset: {
    title: "Reset encryption keys",
    body:
      "This creates new keys. Messages sent to your old keys can no longer be read on a device that doesn't have them, and people you message will see that your keys changed. Only do this if you've lost your passphrase.",
    confirm: "Reset keys",
    needsConfirm: true,
    danger: true,
  },
};

const COMMAND: Record<PassphraseMode, string> = {
  setup: "e2ee_setup",
  unlock: "e2ee_unlock",
  change: "e2ee_change_passphrase",
  reset: "e2ee_reset",
};

export default function PassphraseModal() {
  const mode = useE2eeStore((s) => s.passphraseModal);
  const close = useE2eeStore((s) => s.closePassphraseModal);
  const open = mode !== null;

  const [mounted, setMounted] = useState(false);
  const [visible, setVisible] = useState(false);
  const [passphrase, setPassphrase] = useState("");
  const [confirm, setConfirm] = useState("");
  const [error, setError] = useState<string | null>(null);
  const [busy, setBusy] = useState(false);
  const inputRef = useRef<HTMLInputElement>(null);

  useEffect(() => {
    if (open) {
      setMounted(true);
      setPassphrase("");
      setConfirm("");
      setError(null);
      setBusy(false);
      requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
      // Focus after the open transition starts — the input is painted by then.
      const t = window.setTimeout(() => inputRef.current?.focus(), 60);
      return () => window.clearTimeout(t);
    }
    setVisible(false);
    return undefined;
  }, [open, mode]);

  const handleTransitionEnd = useCallback(
    (e: React.TransitionEvent) => {
      if (e.target === e.currentTarget && !visible) setMounted(false);
    },
    [visible],
  );

  // Frozen copy for the fade-out (mode is null by then).
  const frozenMode = useRef<PassphraseMode>("setup");
  if (mode) frozenMode.current = mode;
  const m = mode ?? frozenMode.current;
  const copy = COPY[m];

  const cancel = () => {
    if (open && !busy) close();
  };

  const submit = async () => {
    if (!open || busy) return;
    const p = passphrase;
    if ([...p].length < MIN_CHARS) {
      setError(`Use at least ${MIN_CHARS} characters.`);
      return;
    }
    if (copy.needsConfirm && confirm !== p) {
      setError("The passphrases don't match.");
      return;
    }
    setBusy(true);
    setError(null);
    try {
      await invoke(COMMAND[m], { passphrase: p });
      if (m === "setup") toast.success("Encryption is on", "Your direct messages are now end-to-end encrypted.");
      else if (m === "unlock") toast.success("Encryption unlocked");
      else if (m === "change") toast.success("Passphrase changed");
      else toast.success("Encryption keys reset");
      close();
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
        className="w-full max-w-[440px] rounded-xl border border-border bg-bg-secondary p-6 shadow-modal transition-all duration-300"
        style={{
          opacity: visible ? 1 : 0,
          transform: visible ? "scale(1)" : "scale(0.95)",
        }}
        onClick={(e) => e.stopPropagation()}
        role="dialog"
        aria-label={copy.title}
      >
        <h2 className="text-[16px] font-semibold text-text-primary">{copy.title}</h2>
        <p className="mt-2 text-[12px] leading-[1.55] text-text-muted">{copy.body}</p>

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
            autoComplete={m === "unlock" ? "current-password" : "new-password"}
            placeholder={m === "unlock" ? "Passphrase" : `New passphrase (at least ${MIN_CHARS} characters)`}
            value={passphrase}
            onChange={(e) => setPassphrase(e.target.value)}
            disabled={busy}
            className={inputClass}
          />
          {copy.needsConfirm && (
            <input
              type="password"
              autoComplete="new-password"
              placeholder="Confirm passphrase"
              value={confirm}
              onChange={(e) => setConfirm(e.target.value)}
              disabled={busy}
              className={inputClass}
            />
          )}
          {error && <div className="text-[12px] text-error">{error}</div>}
          {m === "unlock" && (
            <button
              type="button"
              onClick={() => useE2eeStore.getState().openPassphraseModal("reset")}
              disabled={busy}
              className="self-start text-[12px] text-text-muted underline-offset-2 hover:text-text-secondary hover:underline"
            >
              Forgot your passphrase? Reset your keys
            </button>
          )}
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
              className={
                copy.danger
                  ? "rounded-md bg-error px-4 py-2 text-[13px] font-semibold text-white transition-colors hover:bg-error/90 disabled:opacity-50"
                  : "rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
              }
            >
              {busy ? "Working…" : copy.confirm}
            </button>
          </div>
        </form>
      </div>
    </div>
  );
}
