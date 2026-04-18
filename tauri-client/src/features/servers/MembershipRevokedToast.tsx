import { useEffect } from "react";
import { useUiStore } from "../../stores/uiStore";
import { useChatStore } from "../../stores/chatStore";

export default function MembershipRevokedToast() {
  const notice = useUiStore((s) => s.membershipRevocationNotice);
  const clearNotice = useUiStore((s) => s.setMembershipRevocationNotice);
  const serverMeta = useChatStore((s) => s.serverMeta);
  const servers = useChatStore((s) => s.servers);

  useEffect(() => {
    if (!notice) return;
    const t = setTimeout(() => clearNotice(null), 8000);
    return () => clearTimeout(t);
  }, [notice, clearNotice]);

  if (!notice) return null;

  const meta = serverMeta[notice.serverId];
  const fallback = servers.find((s) => s.id === notice.serverId);
  const serverName =
    meta?.name ?? fallback?.name ?? notice.serverId;

  const headline =
    notice.action === "ban"
      ? `You have been banned from ${serverName}`
      : notice.action === "kick"
        ? `You have been kicked from ${serverName}`
        : `You have been removed from ${serverName}`;

  return (
    <div className="pointer-events-none fixed inset-x-0 top-6 z-[60] flex justify-center">
      <div className="pointer-events-auto flex w-full max-w-md items-start gap-3 rounded-xl border border-error/50 bg-bg-secondary p-4 shadow-2xl">
        <div className="flex h-8 w-8 shrink-0 items-center justify-center rounded-lg bg-error/15 text-error">
          <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
            <circle cx="12" cy="12" r="10" />
            <line x1="12" y1="8" x2="12" y2="12" />
            <line x1="12" y1="16" x2="12.01" y2="16" />
          </svg>
        </div>
        <div className="min-w-0 flex-1">
          <p className="text-sm font-semibold text-text-bright">{headline}</p>
          {notice.actor && (
            <p className="mt-0.5 text-xs text-text-secondary">
              By {notice.actor}
            </p>
          )}
          {notice.reason && (
            <p className="mt-0.5 text-xs text-text-muted">
              Reason: {notice.reason}
            </p>
          )}
        </div>
        <button
          onClick={() => clearNotice(null)}
          className="shrink-0 text-text-muted transition-colors hover:text-text-primary"
        >
          ×
        </button>
      </div>
    </div>
  );
}
