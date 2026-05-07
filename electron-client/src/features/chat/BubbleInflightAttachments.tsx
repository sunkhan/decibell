import { useAttachmentsStore } from "../../stores/attachmentsStore";

interface Props {
  pendingIds: string[];
}

/// Renders a small progress strip in the optimistic bubble for each
/// in-flight attachment upload. Reads from attachmentsStore — when
/// the server echoes the broadcast back, the bubble itself stops
/// referencing pendingIds and the canonical AttachmentList renders.
export default function BubbleInflightAttachments({ pendingIds }: Props) {
  const pendings = useAttachmentsStore((s) => s.pendings);
  const visible = pendingIds
    .map((id) => pendings[id])
    .filter((p): p is NonNullable<typeof p> => Boolean(p));
  if (visible.length === 0) return null;

  return (
    <div className="mt-2 flex flex-col gap-2">
      {visible.map((p) => {
        const pct = p.totalBytes
          ? Math.min(100, Math.round((p.transferredBytes / p.totalBytes) * 100))
          : 0;
        const isImage = p.kind === "image" && p.previewUrl;
        return (
          <div
            key={p.pendingId}
            className="flex max-w-[420px] items-center gap-3 rounded-xl border border-border bg-bg-secondary p-3"
          >
            {isImage ? (
              <img
                src={p.previewUrl ?? undefined}
                alt={p.filename}
                className="h-12 w-12 shrink-0 rounded-lg object-cover opacity-70"
                draggable={false}
              />
            ) : (
              <div className="flex h-10 w-10 shrink-0 items-center justify-center rounded-lg bg-bg-light text-text-muted">
                <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
                  <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
                  <polyline points="14 2 14 8 20 8" />
                </svg>
              </div>
            )}
            <div className="min-w-0 flex-1">
              <div className="truncate font-channel text-sm font-medium text-text-primary">
                {p.filename}
              </div>
              <div className="mt-1 h-1 w-full overflow-hidden rounded-full bg-bg-light">
                <div
                  className={`h-full transition-all ${
                    p.status === "failed" ? "bg-error" : "bg-accent"
                  }`}
                  style={{ width: `${pct}%` }}
                />
              </div>
              <div className="mt-1 text-[10px] text-text-muted">
                {p.status === "failed"
                  ? p.errorMessage ?? "Upload failed"
                  : p.status === "ready"
                    ? "Ready"
                    : `${pct}%`}
              </div>
            </div>
          </div>
        );
      })}
    </div>
  );
}
