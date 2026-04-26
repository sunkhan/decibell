import { useMemo } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { formatBytes } from "./attachmentHelpers";

const TILE_SIZE = 128; // px

function KindIcon({ kind, size = 20 }: { kind: PendingAttachment["kind"]; size?: number }) {
  const stroke = "currentColor";
  const sw = 1.6;
  switch (kind) {
    case "image":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
          <circle cx="8.5" cy="8.5" r="1.5" />
          <polyline points="21 15 16 10 5 21" />
        </svg>
      );
    case "video":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round">
          <polygon points="23 7 16 12 23 17 23 7" />
          <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
        </svg>
      );
    case "audio":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="3" />
          <line x1="7" y1="10" x2="7" y2="14" />
          <line x1="10" y1="8" x2="10" y2="16" />
          <line x1="13" y1="11" x2="13" y2="13" />
          <line x1="16" y1="9" x2="16" y2="15" />
          <line x1="19" y1="11" x2="19" y2="13" />
        </svg>
      );
    default:
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke={stroke} strokeWidth={sw} strokeLinecap="round" strokeLinejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
      );
  }
}

function PendingTile({ a }: { a: PendingAttachment }) {
  const remove = useAttachmentsStore((s) => s.removePending);
  const pct = a.totalBytes > 0 ? Math.min(100, (a.transferredBytes / a.totalBytes) * 100) : 0;
  const showThumb = (a.kind === "image" || a.kind === "video") && !!a.thumbnailUrl;
  const isFailed = a.status === "failed" || a.status === "cancelled";

  const subLabel = (() => {
    switch (a.status) {
      case "queued":    return formatBytes(a.totalBytes);
      case "uploading": return `${Math.round(pct)}%`;
      case "ready":     return formatBytes(a.totalBytes);
      case "failed":    return "Failed";
      case "cancelled": return "Cancelled";
    }
  })();

  const onRemove = async (e: React.MouseEvent) => {
    e.stopPropagation();
    if (a.status === "uploading") {
      try { await invoke("cancel_attachment_upload", { pendingId: a.pendingId }); }
      catch (err) { console.error("cancel_attachment_upload", err); }
    }
    remove(a.pendingId);
  };

  return (
    <div className="group flex flex-col gap-1" style={{ width: TILE_SIZE }}>
      <div
        className={`relative overflow-hidden rounded-xl border bg-bg-darkest ${
          isFailed
            ? "border-error/40"
            : "border-border-divider"
        }`}
        style={{ width: TILE_SIZE, height: TILE_SIZE }}
        title={a.filename}
      >
        {showThumb ? (
          <div
            className="absolute inset-0 bg-cover bg-center"
            style={{ backgroundImage: `url(${a.thumbnailUrl})` }}
          />
        ) : (
          <div className="flex h-full w-full items-center justify-center text-text-muted">
            <KindIcon kind={a.kind} size={26} />
          </div>
        )}

        {a.kind === "video" && showThumb && (
          // Tiny play badge so a video tile reads as video at a glance.
          <div className="pointer-events-none absolute left-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded-md bg-black/60 text-white">
            <svg width="10" height="10" viewBox="0 0 24 24" fill="currentColor">
              <path d="M8 5v14l11-7z" />
            </svg>
          </div>
        )}

        {a.status === "uploading" && (
          // Bottom progress bar overlay. Sits on top of the thumbnail.
          <div className="absolute inset-x-0 bottom-0 h-1 bg-black/40">
            <div
              className="h-full bg-accent transition-[width] duration-150"
              style={{ width: `${pct}%` }}
            />
          </div>
        )}

        {a.status === "queued" && (
          <div className="pointer-events-none absolute inset-0 bg-black/30" />
        )}

        {isFailed && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-error/20 text-error">
            <svg width="28" height="28" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10" />
              <line x1="12" y1="8" x2="12" y2="12" />
              <line x1="12" y1="16" x2="12.01" y2="16" />
            </svg>
          </div>
        )}

        <button
          onClick={onRemove}
          title={a.status === "uploading" ? "Cancel upload" : "Remove"}
          className="absolute right-1 top-1 flex h-5 w-5 items-center justify-center rounded-md bg-black/60 text-white/85 opacity-0 transition-all hover:bg-error hover:text-white group-hover:opacity-100"
        >
          <svg width="11" height="11" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <line x1="18" y1="6" x2="6" y2="18" />
            <line x1="6" y1="6" x2="18" y2="18" />
          </svg>
        </button>
      </div>
      <div className="px-0.5">
        <div className="truncate text-[11px] font-medium text-text-primary" title={a.filename}>
          {a.filename}
        </div>
        <div
          className={`truncate text-[10px] ${
            isFailed ? "text-error" : "text-text-muted"
          }`}
          title={a.error ?? undefined}
        >
          {subLabel}
        </div>
      </div>
    </div>
  );
}

export default function PendingAttachmentsRow({ channelId }: { channelId: string }) {
  // Subscribe to raw slices so each selector returns an identity-stable
  // value when the slice hasn't changed. zustand v5 / React 18's
  // useSyncExternalStore enters an infinite-render loop if a selector
  // returns a freshly-derived array reference on every call.
  const orderByChannel = useAttachmentsStore((s) => s.orderByChannel);
  const byPendingId = useAttachmentsStore((s) => s.byPendingId);
  const items = useMemo(() => {
    const order = orderByChannel[channelId] ?? [];
    return order.map((id) => byPendingId[id]).filter((x): x is PendingAttachment => !!x);
  }, [orderByChannel, byPendingId, channelId]);

  if (items.length === 0) return null;
  return (
    <div className="flex flex-wrap gap-2.5 border-b border-border-divider px-1 pb-2.5">
      {items.map((a) => (
        <PendingTile key={a.pendingId} a={a} />
      ))}
    </div>
  );
}
