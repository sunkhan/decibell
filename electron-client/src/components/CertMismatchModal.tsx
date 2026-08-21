import { useState } from "react";
import { invoke } from "../lib/ipc";
import { useUiStore } from "../stores/uiStore";
import { toast } from "../stores/toastStore";

/// Shown when a server's TLS certificate doesn't match what this client
/// (or the central directory) knows for it. Refusing is the safe default;
/// "Trust new certificate" is for the case where the operator confirmed
/// they re-generated their certificate.
export default function CertMismatchModal() {
  const notice = useUiStore((s) => s.certMismatch);
  const setNotice = useUiStore((s) => s.setCertMismatch);
  const [busy, setBusy] = useState(false);
  if (!notice) return null;

  const pretty = notice.fingerprint.replace(/(.{2})(?=.)/g, "$1:").toUpperCase();

  const trust = async () => {
    setBusy(true);
    try {
      await invoke("trust_certificate", { host: notice.host, port: notice.port });
      setNotice(null);
      await notice.retry?.();
    } catch (err) {
      toast.error("Couldn't re-trust certificate", String(err));
    } finally {
      setBusy(false);
    }
  };

  return (
    <div
      className="fixed inset-0 z-[70] flex items-center justify-center bg-black/60 backdrop-blur-sm"
      onClick={() => setNotice(null)}
    >
      <div
        className="w-full max-w-md rounded-xl border border-error/50 bg-bg-secondary p-6 shadow-modal"
        onClick={(e) => e.stopPropagation()}
      >
        <h2 className="mb-2 font-display text-[16px] font-semibold text-text-bright">
          Certificate changed
        </h2>
        <p className="text-[13px] leading-[1.55] text-text-secondary">
          <span className="font-mono text-text-primary">
            {notice.host}:{notice.port}
          </span>{" "}
          presented a TLS certificate that doesn't match the one this client
          trusts for it. That happens when the operator re-generated their
          certificate — or when someone is intercepting the connection.
        </p>
        <p className="mt-3 text-[11px] font-semibold uppercase tracking-[0.07em] text-text-muted">
          Presented fingerprint (SHA-256)
        </p>
        <p className="mt-1 break-all font-mono text-[11px] text-text-secondary">{pretty}</p>
        <p className="mt-3 text-[12px] leading-[1.5] text-text-muted">
          Only continue if the server operator confirms this fingerprint.
        </p>
        <div className="mt-5 flex gap-2.5">
          <button
            onClick={() => setNotice(null)}
            className="flex-1 rounded-md bg-bg-light py-2.5 text-[13px] font-medium text-text-primary transition-colors hover:bg-bg-lighter"
          >
            Cancel
          </button>
          <button
            onClick={trust}
            disabled={busy}
            className="flex-1 rounded-md bg-error py-2.5 text-[13px] font-semibold text-white transition-colors hover:bg-error/85 disabled:opacity-50"
          >
            {busy ? "Working…" : "Trust new certificate"}
          </button>
        </div>
      </div>
    </div>
  );
}
