import { useEffect } from "react";
import { useCallStore } from "../../stores/callStore";
import { UserAvatar } from "../../components/UserAvatar";
import { useEscapeToClose } from "../../hooks/useEscapeToClose";
import { acceptCall, declineCall } from "./callActions";
import { useUserName } from "../../stores/usersStore";

/// How long the card says "Answered on another device" before it goes.
const ENDED_MS = 3000;

/// Incoming DM call prompt. Mounted once in MainLayout, driven by
/// callStore.incoming (the CertMismatchModal nullable-field pattern).
/// Top-centred card without a backdrop so the user can keep working
/// while it rings; Escape declines. When another device of ours answers
/// or declines (multi-device), the card stays a moment to say so
/// (callStore.incomingEnded); Escape dismisses it.
export default function IncomingCallModal() {
  const status = useCallStore((s) => s.status);
  const incoming = useCallStore((s) => s.incoming);
  const ended = useCallStore((s) => s.incomingEnded);
  const ringing = status === "incoming" && incoming != null;
  const shown = ringing ? incoming : ended;
  const callerName = useUserName(shown?.fromUid, shown?.from ?? "");

  useEffect(() => {
    if (!ended) return;
    const timer = setTimeout(() => useCallStore.getState().clearIncomingEnded(), ENDED_MS);
    return () => clearTimeout(timer);
  }, [ended]);

  useEscapeToClose(() => {
    if (useCallStore.getState().status === "incoming") void declineCall();
    else useCallStore.getState().clearIncomingEnded();
  }, ringing || ended != null);
  if (!shown) return null;

  return (
    <div className="pointer-events-none fixed inset-x-0 top-14 z-[70] flex justify-center">
      <div
        className="pointer-events-auto flex w-full max-w-sm items-center gap-3.5 rounded-xl border border-border bg-bg-secondary px-4 py-3.5 shadow-modal"
        role={ringing ? "dialog" : "status"}
        aria-label={ringing ? `Incoming call from ${callerName}` : undefined}
        aria-live={ringing ? undefined : "polite"}
      >
        <div className="relative shrink-0">
          {ringing && <div className="absolute inset-0 animate-ping rounded-lg bg-success/30" />}
          <div className={ringing ? "relative" : "relative opacity-60"}>
            <UserAvatar uid={shown.fromUid} name={callerName} size={44} />
          </div>
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate font-display text-[14px] font-semibold text-text-bright">
            {callerName}
          </div>
          <div className="truncate text-[12px] text-text-muted">
            {ringing ? "Incoming call…" : ended?.reason}
          </div>
        </div>
        {ringing ? (
          <div className="flex shrink-0 gap-2">
            <button
              onClick={() => void declineCall()}
              title="Decline (Esc)"
              className="flex h-9 w-9 items-center justify-center rounded-md bg-error text-text-bright transition-opacity hover:opacity-90"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M10.68 13.31a16 16 0 0 0 3.41 2.6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.42 19.42 0 0 1-3.33-2.67m-2.67-3.34a19.79 19.79 0 0 1-3.07-8.63A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91" />
                <line x1="23" y1="1" x2="1" y2="23" />
              </svg>
            </button>
            <button
              onClick={() => void acceptCall()}
              title="Accept"
              className="flex h-9 w-9 items-center justify-center rounded-md bg-success text-bg-primary transition-opacity hover:opacity-90"
            >
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <path d="M22 16.92v3a2 2 0 0 1-2.18 2 19.79 19.79 0 0 1-8.63-3.07 19.5 19.5 0 0 1-6-6 19.79 19.79 0 0 1-3.07-8.67A2 2 0 0 1 4.11 2h3a2 2 0 0 1 2 1.72 12.84 12.84 0 0 0 .7 2.81 2 2 0 0 1-.45 2.11L8.09 9.91a16 16 0 0 0 6 6l1.27-1.27a2 2 0 0 1 2.11-.45 12.84 12.84 0 0 0 2.81.7A2 2 0 0 1 22 16.92z" />
              </svg>
            </button>
          </div>
        ) : (
          // Where the buttons were: a muted mark of how it ended, so the
          // card keeps its size while it fades out.
          <div
            className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-surface-hover text-text-muted"
            aria-hidden="true"
          >
            {ended?.answered ? (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <rect x="5" y="2" width="14" height="20" rx="2" />
                <path d="M9 11.5l2 2 4-4" />
              </svg>
            ) : (
              <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                <rect x="5" y="2" width="14" height="20" rx="2" />
                <path d="M9.5 9.5l5 5M14.5 9.5l-5 5" />
              </svg>
            )}
          </div>
        )}
      </div>
    </div>
  );
}
