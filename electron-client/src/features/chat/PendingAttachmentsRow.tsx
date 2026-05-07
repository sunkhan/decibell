import { useAttachmentsStore } from "../../stores/attachmentsStore";
import { useChatStore } from "../../stores/chatStore";

/// Composer-side preview row: renders queued/in-flight attachments
/// for the active channel above the chat input, with cancel + remove
/// buttons. Empty when there's nothing pending.
export default function PendingAttachmentsRow() {
  const pendings = useAttachmentsStore((s) => s.pendings);
  const removePending = useAttachmentsStore((s) => s.removePending);
  const activeServerId = useChatStore((s) => s.activeServerId);
  const activeChannelId = useChatStore((s) => s.activeChannelId);

  if (!activeServerId || !activeChannelId) return null;

  const items = Object.values(pendings).filter(
    (p) => p.serverId === activeServerId && p.channelId === activeChannelId,
  );
  if (items.length === 0) return null;

  return (
    <div className="flex flex-wrap gap-2 border-t border-border-divider px-4 py-2">
      {items.map((p) => {
        const pct = p.totalBytes
          ? Math.min(100, Math.round((p.transferredBytes / p.totalBytes) * 100))
          : 0;
        const isImage = p.kind === "image" && p.previewUrl;
        return (
          <div
            key={p.pendingId}
            className="relative flex items-center gap-2 rounded-lg border border-border bg-bg-light p-2"
          >
            {isImage ? (
              <img
                src={p.previewUrl ?? undefined}
                alt={p.filename}
                className="h-12 w-12 rounded object-cover"
                draggable={false}
              />
            ) : (
              <div className="flex h-12 w-12 items-center justify-center rounded bg-bg-secondary text-text-muted">
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
                  <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                  <polyline points="14 2 14 8 20 8" />
                </svg>
              </div>
            )}
            <div className="min-w-0">
              <div className="max-w-[160px] truncate font-channel text-[11px] text-text-primary">
                {p.filename}
              </div>
              <div className="text-[10px] text-text-muted">
                {p.status === "failed"
                  ? p.errorMessage ?? "Failed"
                  : p.status === "ready"
                    ? "Ready"
                    : `${pct}% · ${p.status}`}
              </div>
            </div>
            <button
              type="button"
              onClick={() => {
                p.abortController.abort();
                removePending(p.pendingId);
              }}
              className="absolute -right-1 -top-1 flex h-5 w-5 items-center justify-center rounded-full bg-bg-darkest text-text-secondary hover:bg-error hover:text-white"
              title="Remove"
            >
              ×
            </button>
          </div>
        );
      })}
    </div>
  );
}
