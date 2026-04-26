import { useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { formatBytes } from "./attachmentHelpers";

// Custom MIME type — distinct from "Files" so the global Tauri
// drag-drop hook (which listens for OS-level file drags) doesn't
// react to internal tile-reorder drags.
const REORDER_MIME = "application/x-decibell-pending-attachment";

const TILE_SIZE = 128; // px (square)

function KindIcon({ kind, size = 18 }: { kind: PendingAttachment["kind"]; size?: number }) {
  switch (kind) {
    case "image":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <rect x="3" y="3" width="18" height="18" rx="2" ry="2" />
          <circle cx="8.5" cy="8.5" r="1.5" />
          <polyline points="21 15 16 10 5 21" />
        </svg>
      );
    case "video":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <polygon points="23 7 16 12 23 17 23 7" />
          <rect x="1" y="5" width="15" height="14" rx="2" ry="2" />
        </svg>
      );
    case "audio":
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
          <path d="M9 18V5l12-2v13" />
          <circle cx="6" cy="18" r="3" />
          <circle cx="18" cy="16" r="3" />
        </svg>
      );
    default:
      return (
        <svg width={size} height={size} viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
          <path d="M14 2H6a2 2 0 0 0-2 2v16a2 2 0 0 0 2 2h12a2 2 0 0 0 2-2V8z" />
          <polyline points="14 2 14 8 20 8" />
        </svg>
      );
  }
}

const KIND_ICON_STYLE: Record<PendingAttachment["kind"], { bg: string; text: string }> = {
  image:    { bg: "bg-success/15",      text: "text-success" },
  video:    { bg: "bg-warning/15",      text: "text-warning" },
  audio:    { bg: "bg-accent-soft",     text: "text-accent-bright" },
  document: { bg: "bg-text-muted/15",   text: "text-text-muted" },
};

interface DragState {
  draggingId: string | null;
  hoverId: string | null;
  side: "before" | "after" | null;
}

function PendingTile({
  a,
  drag,
  setDrag,
}: {
  a: PendingAttachment;
  drag: DragState;
  setDrag: (s: DragState) => void;
}) {
  const remove = useAttachmentsStore((s) => s.removePending);
  const reorder = useAttachmentsStore((s) => s.reorderPending);
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

  const iconStyle = KIND_ICON_STYLE[a.kind] ?? KIND_ICON_STYLE.document;

  // ---- HTML5 drag-to-reorder ----
  // The drag source is the tile itself. Drop targets are siblings.
  // Insertion side (before / after) is computed from the cursor's
  // position relative to the target's horizontal midpoint, so the
  // user can shove a tile into either edge of any other tile.
  const isDragging = drag.draggingId === a.pendingId;
  const isHover = drag.hoverId === a.pendingId && drag.draggingId !== a.pendingId;

  const onDragStart = (e: React.DragEvent<HTMLDivElement>) => {
    e.dataTransfer.effectAllowed = "move";
    e.dataTransfer.setData(REORDER_MIME, a.pendingId);
    // Clearing the text/plain shape too prevents downstream listeners
    // from misclassifying the drag as text.
    e.dataTransfer.setData("text/plain", a.pendingId);
    setDrag({ draggingId: a.pendingId, hoverId: null, side: null });
  };

  const onDragOver = (e: React.DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.types.includes(REORDER_MIME)) return;
    e.preventDefault();
    e.dataTransfer.dropEffect = "move";
    if (drag.draggingId === a.pendingId) return; // can't drop on self
    const r = e.currentTarget.getBoundingClientRect();
    const side: "before" | "after" =
      e.clientX - r.left < r.width / 2 ? "before" : "after";
    if (drag.hoverId !== a.pendingId || drag.side !== side) {
      setDrag({ ...drag, hoverId: a.pendingId, side });
    }
  };

  const onDrop = (e: React.DragEvent<HTMLDivElement>) => {
    if (!e.dataTransfer.types.includes(REORDER_MIME)) return;
    e.preventDefault();
    const fromId = e.dataTransfer.getData(REORDER_MIME);
    if (fromId && fromId !== a.pendingId) {
      // Compute side directly from the drop event rather than reading
      // it off React state — the state may not have flushed from the
      // last onDragOver before drop fires (especially on a quick flick
      // toward the rightmost tile), in which case drag.side would be
      // null and the reorder would silently skip.
      const r = e.currentTarget.getBoundingClientRect();
      const side: "before" | "after" =
        e.clientX - r.left < r.width / 2 ? "before" : "after";
      reorder(a.channelId, fromId, a.pendingId, side);
    }
    setDrag({ draggingId: null, hoverId: null, side: null });
  };

  const onDragEnd = () => {
    setDrag({ draggingId: null, hoverId: null, side: null });
  };

  return (
    <div
      data-pending-id={a.pendingId}
      draggable
      onDragStart={onDragStart}
      onDragOver={onDragOver}
      onDrop={onDrop}
      onDragEnd={onDragEnd}
      className={`group relative flex flex-col transition-opacity ${
        isDragging ? "opacity-40" : "opacity-100"
      }`}
      style={{ width: TILE_SIZE, cursor: "grab" }}
    >
      {/* Insertion cue — accent vertical bar pinned to the side the
          cursor is closest to. Sized to the tile's height. */}
      {isHover && drag.side && (
        <div
          className="pointer-events-none absolute top-0 z-10 w-[3px] rounded-full bg-accent shadow-[0_0_8px_rgba(56,143,255,0.7)]"
          style={{
            height: TILE_SIZE,
            ...(drag.side === "before" ? { left: -6 } : { right: -6 }),
          }}
        />
      )}
      <div
        className={`relative overflow-hidden rounded-[10px] border bg-bg-dark ${
          isFailed
            ? "border-error/40"
            : "border-border"
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
          <div className="flex h-full w-full items-center justify-center">
            <div className={`flex h-9 w-9 items-center justify-center rounded-lg ${iconStyle.bg} ${iconStyle.text}`}>
              <KindIcon kind={a.kind} size={18} />
            </div>
          </div>
        )}

        {a.kind === "video" && showThumb && (
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center">
            <div className="flex h-12 w-16 items-center justify-center rounded-xl bg-accent/85 shadow-[0_2px_10px_rgba(0,0,0,0.35)]">
              <svg width="20" height="20" viewBox="0 0 24 24" fill="white">
                <path d="M8 5v14l11-7z" />
              </svg>
            </div>
          </div>
        )}

        {a.status === "uploading" && (
          <div className="absolute inset-x-0 bottom-0 h-[3px] bg-black/40">
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
          <div className="pointer-events-none absolute inset-0 flex items-center justify-center bg-error/15 text-error">
            <svg width="24" height="24" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.2" strokeLinecap="round" strokeLinejoin="round">
              <circle cx="12" cy="12" r="10" />
              <line x1="12" y1="8" x2="12" y2="12" />
              <line x1="12" y1="16" x2="12.01" y2="16" />
            </svg>
          </div>
        )}

        <button
          onClick={onRemove}
          title={a.status === "uploading" ? "Cancel upload" : "Remove"}
          className="absolute right-1.5 top-1.5 flex h-5 w-5 items-center justify-center rounded-full bg-black/60 text-white/85 opacity-0 transition-all hover:bg-error hover:text-white group-hover:opacity-100"
        >
          <svg width="10" height="10" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
            <line x1="18" y1="6" x2="6" y2="18" />
            <line x1="6" y1="6" x2="18" y2="18" />
          </svg>
        </button>
      </div>
      <div className="px-1 pt-1.5">
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
  const orderByChannel = useAttachmentsStore((s) => s.orderByChannel);
  const byPendingId = useAttachmentsStore((s) => s.byPendingId);
  const items = useMemo(() => {
    const order = orderByChannel[channelId] ?? [];
    return order.map((id) => byPendingId[id]).filter((x): x is PendingAttachment => !!x);
  }, [orderByChannel, byPendingId, channelId]);
  // Drag state shared across tiles so each can render the right
  // insertion cue. Lives at the row level (not module-level) so
  // multiple composers in different channels don't cross-pollute.
  const [drag, setDrag] = useState<DragState>({
    draggingId: null,
    hoverId: null,
    side: null,
  });

  // FLIP-based reorder animation. Capture each tile's screen position
  // before paint, compare against the previous render's positions,
  // and for any tile whose position changed: instantly transform it
  // back to its old position (Invert), then transition to zero (Play).
  // Result: when the user drops a tile, every shifted tile slides
  // smoothly into its new spot rather than jumping.
  const containerRef = useRef<HTMLDivElement | null>(null);
  const prevPositions = useRef(new Map<string, { left: number; top: number }>());
  useLayoutEffect(() => {
    const container = containerRef.current;
    if (!container) return;
    const tiles = Array.from(
      container.querySelectorAll<HTMLDivElement>("[data-pending-id]"),
    );
    const newPositions = new Map<string, { left: number; top: number }>();
    const moved: HTMLDivElement[] = [];

    for (const tile of tiles) {
      const id = tile.dataset.pendingId;
      if (!id) continue;
      const r = tile.getBoundingClientRect();
      newPositions.set(id, { left: r.left, top: r.top });
      const prev = prevPositions.current.get(id);
      if (!prev) continue; // newly mounted tile — no entrance animation
      const dx = prev.left - r.left;
      const dy = prev.top - r.top;
      if (dx === 0 && dy === 0) continue;
      // Invert: jump back instantly. Disable transition for this
      // step so the snap-back doesn't itself animate.
      tile.style.transition = "none";
      tile.style.transform = `translate(${dx}px, ${dy}px)`;
      moved.push(tile);
    }

    if (moved.length > 0) {
      // Force the inverted styles to apply before we re-enable the
      // transition; without this read the browser may coalesce the
      // two style writes and skip the slide.
      void container.offsetHeight;
      requestAnimationFrame(() => {
        for (const tile of moved) {
          tile.style.transition =
            "transform 220ms cubic-bezier(0.2, 0, 0, 1), opacity 220ms ease";
          tile.style.transform = "";
        }
      });
      // Clear the inline styles after the animation so subsequent
      // renders aren't carrying stale inline transition strings.
      window.setTimeout(() => {
        for (const tile of moved) {
          tile.style.transition = "";
          tile.style.transform = "";
        }
      }, 260);
    }

    prevPositions.current = newPositions;
  }, [items]);

  if (items.length === 0) return null;
  return (
    <div
      ref={containerRef}
      className="flex flex-wrap gap-2 border-b border-border-divider px-1.5 pb-2.5"
    >
      {items.map((a) => (
        <PendingTile key={a.pendingId} a={a} drag={drag} setDrag={setDrag} />
      ))}
    </div>
  );
}
