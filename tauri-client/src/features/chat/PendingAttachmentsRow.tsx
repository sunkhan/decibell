import { useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { formatBytes } from "./attachmentHelpers";

function KindIcon({ kind }: { kind: PendingAttachment["kind"] }) {
  const stroke = "currentColor";
  switch (kind) {
    case "image":
      return (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
          <circle cx="8.5" cy="8.5" r="1.5" />
          <polyline points="21 15 16 10 5 21" />
        </svg>
      );
    case "video":
      return (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polygon points="23 7 16 12 23 17 23 7" />
          <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
        </svg>
      );
    case "audio":
      return (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M9 18V5l12-2v13" />
          <circle cx="6" cy="18" r="3" />
          <circle cx="18" cy="16" r="3" />
        </svg>
      );
    default:
      return (
        <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
      );
  }
}

function PendingCard({ a }: { a: PendingAttachment }) {
  const remove = useAttachmentsStore((s) => s.removePending);
  const pct = a.totalBytes > 0 ? Math.min(100, (a.transferredBytes / a.totalBytes) * 100) : 0;

  const label = (() => {
    switch (a.status) {
      case "queued":    return `${formatBytes(a.totalBytes)} • queued`;
      case "uploading": return `${formatBytes(a.transferredBytes)} / ${formatBytes(a.totalBytes)}`;
      case "ready":     return `${formatBytes(a.totalBytes)}`;
      case "failed":    return a.error ?? "Upload failed";
      case "cancelled": return "Cancelled";
    }
  })();

  const onRemove = async () => {
    if (a.status === "uploading") {
      try { await invoke("cancel_attachment_upload", { pendingId: a.pendingId }); }
      catch (err) { console.error("cancel_attachment_upload", err); }
    }
    remove(a.pendingId);
  };

  return (
    <div className="relative flex w-[220px] flex-col gap-1.5 rounded-[10px] border border-border-divider bg-bg-light px-3 py-2.5">
      <button
        onClick={onRemove}
        title={a.status === "uploading" ? "Cancel upload" : "Remove"}
        className="absolute right-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-error/20 hover:text-error"
      >
        <svg width="12" height="12" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
          <line x1="18" y1="6" x2="6" y2="18" />
          <line x1="6" y1="6" x2="18" y2="18" />
        </svg>
      </button>
      <div className="flex items-center gap-2 pr-5">
        <div className={`flex h-7 w-7 shrink-0 items-center justify-center rounded-md ${
          a.status === "failed" || a.status === "cancelled"
            ? "bg-error/15 text-error"
            : a.status === "ready"
              ? "bg-success/15 text-success"
              : "bg-accent-soft text-accent-bright"
        }`}>
          <KindIcon kind={a.kind} />
        </div>
        <div className="min-w-0 flex-1">
          <div className="truncate text-[12px] font-medium text-text-primary">
            {a.filename}
          </div>
          <div className="truncate text-[10.5px] text-text-muted">{label}</div>
        </div>
      </div>
      {a.status === "uploading" && (
        <div className="h-1 overflow-hidden rounded-full bg-bg-dark">
          <div
            className="h-full rounded-full bg-accent transition-[width] duration-150"
            style={{ width: `${pct}%` }}
          />
        </div>
      )}
    </div>
  );
}

export default function PendingAttachmentsRow({ channelId }: { channelId: string }) {
  // Subscribe to raw slices so each selector returns an identity-stable
  // value when the slice hasn't changed. zustand v5 / React 18's
  // useSyncExternalStore enters an infinite-render loop if a selector
  // returns a freshly-derived array reference on every call (the previous
  // version of this component did exactly that, which tripped React #185
  // the moment a channel became active).
  const orderByChannel = useAttachmentsStore((s) => s.orderByChannel);
  const byPendingId = useAttachmentsStore((s) => s.byPendingId);
  const items = useMemo(() => {
    const order = orderByChannel[channelId] ?? [];
    return order.map((id) => byPendingId[id]).filter((x): x is PendingAttachment => !!x);
  }, [orderByChannel, byPendingId, channelId]);

  if (items.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-2 px-3 pb-2">
      {items.map((a) => (
        <PendingCard key={a.pendingId} a={a} />
      ))}
    </div>
  );
}
