import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { formatBytes } from "./attachmentHelpers";

// Renders the in-flight upload state inside an optimistic message
// bubble. Kept distinct from the completed-attachment look (which
// AttachmentList paints) so the user clearly sees an attachment is
// still uploading vs. already landed. When the server's broadcast
// arrives carrying the matching nonce, useChatEvents reaps the
// pending entries and chatStore.mergeMessage drops the optimistic —
// the bubble re-renders with the real AttachmentList.

function KindIcon({ kind }: { kind: PendingAttachment["kind"] }) {
  const stroke = "currentColor";
  switch (kind) {
    case "image":
      return (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
          <circle cx="8.5" cy="8.5" r="1.5" />
          <polyline points="21 15 16 10 5 21" />
        </svg>
      );
    case "video":
      return (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polygon points="23 7 16 12 23 17 23 7" />
          <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
        </svg>
      );
    case "audio":
      return (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round">
          <line x1="6" y1="9" x2="6" y2="15" />
          <line x1="10" y1="6" x2="10" y2="18" />
          <line x1="14" y1="3" x2="14" y2="21" />
          <line x1="18" y1="6" x2="18" y2="18" />
        </svg>
      );
    default:
      return (
        <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
      );
  }
}

export default function BubbleInflightAttachments({ pendingIds }: { pendingIds: string[] }) {
  // Subscribing to byPendingId re-renders this component on every
  // upload tick — fine since each tick changes the visible progress.
  const byPendingId = useAttachmentsStore((s) => s.byPendingId);
  const items = pendingIds
    .map((id) => byPendingId[id])
    .filter((a): a is PendingAttachment => !!a);
  if (items.length === 0) return null;

  return (
    <div className="mt-2 flex max-w-[420px] flex-col gap-1.5">
      {items.map((a) => {
        const pct = a.totalBytes > 0
          ? Math.min(100, (a.transferredBytes / a.totalBytes) * 100)
          : 0;
        const failed = a.status === "failed" || a.status === "cancelled";
        const showThumb = (a.kind === "image" || a.kind === "video") && !!a.thumbnailUrl;
        return (
          <div
            key={a.pendingId}
            className={`flex items-center gap-2.5 rounded-[10px] border px-2.5 py-2 ${
              failed ? "border-error/40 bg-error/5" : "border-border-divider bg-bg-light"
            }`}
          >
            {showThumb ? (
              <div
                className="h-9 w-9 shrink-0 rounded-md bg-cover bg-center"
                style={{ backgroundImage: `url(${a.thumbnailUrl})` }}
              />
            ) : (
              <div className="flex h-9 w-9 shrink-0 items-center justify-center rounded-md bg-accent-soft text-accent-bright">
                <KindIcon kind={a.kind} />
              </div>
            )}
            <div className="min-w-0 flex-1">
              <div className="truncate text-[12.5px] font-medium text-text-primary" title={a.filename}>
                {a.filename}
              </div>
              <div className="mt-1 h-[3px] overflow-hidden rounded-full bg-bg-lighter">
                <div
                  className={`h-full rounded-full transition-[width] duration-150 ${failed ? "bg-error" : "bg-accent"}`}
                  style={{ width: `${failed ? 100 : pct}%` }}
                />
              </div>
              <div className={`mt-1 text-[10.5px] tabular-nums ${failed ? "text-error" : "text-text-muted"}`}>
                {failed
                  ? a.error ?? "Failed"
                  : a.totalBytes > 0
                    ? `${formatBytes(a.transferredBytes)} / ${formatBytes(a.totalBytes)}`
                    : "Uploading…"}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
