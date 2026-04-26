import { useEffect } from "react";
import { useToastStore, type Toast as ToastT, type ToastSeverity } from "../stores/toastStore";

const SEVERITY_BORDER: Record<ToastSeverity, string> = {
  error: "border-error/15",
  warning: "border-warning/15",
  info: "border-accent-bright/15",
  success: "border-success/15",
};

const SEVERITY_ICON_BG: Record<ToastSeverity, string> = {
  error: "bg-error/10 text-error",
  warning: "bg-warning/10 text-warning",
  info: "bg-accent-bright/10 text-accent-bright",
  success: "bg-success/10 text-success",
};

function SeverityIcon({ severity }: { severity: ToastSeverity }) {
  if (severity === "error" || severity === "warning") {
    return (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
        <circle cx="12" cy="12" r="10" />
        <line x1="12" y1="8" x2="12" y2="12" />
        <line x1="12" y1="16" x2="12.01" y2="16" />
      </svg>
    );
  }
  if (severity === "success") {
    return (
      <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
        <polyline points="20 6 9 17 4 12" />
      </svg>
    );
  }
  return (
    <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
      <circle cx="12" cy="12" r="10" />
      <line x1="12" y1="16" x2="12" y2="12" />
      <line x1="12" y1="8" x2="12.01" y2="8" />
    </svg>
  );
}

function ToastCard({ toast }: { toast: ToastT }) {
  const dismiss = useToastStore((s) => s.dismiss);

  useEffect(() => {
    const id = setTimeout(() => dismiss(toast.id), toast.duration);
    return () => clearTimeout(id);
  }, [toast.id, toast.duration, dismiss]);

  return (
    <div
      className={`pointer-events-auto flex w-full max-w-md items-center gap-3 rounded-xl border bg-bg-light px-4 py-3 shadow-[0_8px_32px_rgba(0,0,0,0.45),0_0_0_1px_rgba(255,255,255,0.02)] animate-[toastIn_0.25s_ease_both] ${SEVERITY_BORDER[toast.severity]}`}
    >
      <div className={`flex h-8 w-8 shrink-0 items-center justify-center rounded-full ${SEVERITY_ICON_BG[toast.severity]}`}>
        <SeverityIcon severity={toast.severity} />
      </div>
      <div className="min-w-0 flex-1">
        <p className="text-[13px] font-medium text-text-primary">{toast.title}</p>
        {toast.body && (
          <p className="mt-0.5 truncate text-[11px] leading-relaxed text-text-muted [overflow-wrap:anywhere]">
            {toast.body}
          </p>
        )}
      </div>
      <button
        onClick={() => dismiss(toast.id)}
        aria-label="Dismiss"
        className="flex h-6 w-6 shrink-0 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-surface-hover hover:text-text-secondary"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
    </div>
  );
}

export default function ToastStack() {
  const toasts = useToastStore((s) => s.toasts);
  if (toasts.length === 0) return null;
  return (
    <div className="pointer-events-none fixed inset-x-0 top-6 z-[90] flex flex-col items-center gap-2 px-4">
      {toasts.map((t) => (
        <ToastCard key={t.id} toast={t} />
      ))}
    </div>
  );
}
