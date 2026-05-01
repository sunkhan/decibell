import { useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { invoke } from "@tauri-apps/api/core";
import { useAttachmentsStore, type PendingAttachment } from "../../stores/attachmentsStore";
import { formatBytes } from "./attachmentHelpers";

// Pointer-events-based reorder. We deliberately do NOT use HTML5 drag-and-
// drop here: Tauri 2's per-window OS drag-drop interception (kept enabled
// so dropping a file from the OS into the chat still works) collides
// with WebView2's HTML5 D&D pipeline on Windows, producing dragstart
// events that never propagate through to subsequent dragover/drop. Pointer
// events live entirely inside the DOM, sidestep that pipeline, and behave
// identically on WebKitGTK and WebView2.

// Pixel distance from pointerdown before we treat the gesture as a drag
// (rather than a click on the remove button etc). Matches typical native
// drag thresholds (~3-6px on Windows, ~5px on macOS).
const DRAG_THRESHOLD_PX = 5;

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
          <line x1="4" y1="9" x2="4" y2="15" />
          <line x1="8" y1="6" x2="8" y2="18" />
          <line x1="12" y1="3" x2="12" y2="21" />
          <line x1="16" y1="6" x2="16" y2="18" />
          <line x1="20" y1="9" x2="20" y2="15" />
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

const EMPTY_DRAG: DragState = { draggingId: null, hoverId: null, side: null };
// Sentinel for the implicit "after the last tile" zone used when the
// user releases past the right edge of the row.
const END_ZONE_ID = "__end_zone__";

function PendingTile({
  a,
  drag,
  onPointerDown,
}: {
  a: PendingAttachment;
  drag: DragState;
  onPointerDown: (id: string, e: React.PointerEvent<HTMLDivElement>) => void;
}) {
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

  const iconStyle = KIND_ICON_STYLE[a.kind] ?? KIND_ICON_STYLE.document;
  const isDragging = drag.draggingId === a.pendingId;
  const isHover = drag.hoverId === a.pendingId && drag.draggingId !== a.pendingId;

  return (
    <div
      data-pending-id={a.pendingId}
      onPointerDown={(e) => onPointerDown(a.pendingId, e)}
      className={`group relative flex flex-col transition-opacity ${
        isDragging ? "opacity-40" : "opacity-100"
      }`}
      style={{
        width: TILE_SIZE,
        cursor: isDragging ? "grabbing" : "grab",
        // Pointer events stay enabled on the dragged tile so the live
        // ghost can still hit-test through it; we use elementFromPoint
        // inside pointermove and early-return when the hit is the
        // source itself.
        touchAction: "none",
        // Suppress browser-native drag (image lift, text selection drag)
        // — pointer events do all the work and the native drag overlay
        // would just confuse the visuals.
        userSelect: "none",
      }}
      // Belt-and-braces: even though we don't use HTML5 D&D, suppress
      // any default drag the browser might still try to start on the
      // tile or its image children (Chromium auto-drags <img> tags).
      onDragStart={(e) => e.preventDefault()}
    >
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
          isFailed ? "border-error/40" : "border-border"
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
            <div className="flex h-9 w-9 items-center justify-center rounded-md bg-accent/85 shadow-[0_2px_10px_rgba(0,0,0,0.35)]">
              <svg width="14" height="14" viewBox="0 0 24 24" fill="white">
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
          // The tile owner intercepts pointerdown for drag-start, so
          // stop the close button's own pointer event from bubbling up
          // and starting a phantom drag on the tile.
          onPointerDown={(e) => e.stopPropagation()}
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
          className={`truncate text-[10px] ${isFailed ? "text-error" : "text-text-muted"}`}
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
  const reorder = useAttachmentsStore((s) => s.reorderPending);
  const items = useMemo(() => {
    const order = orderByChannel[channelId] ?? [];
    return order
      .map((id) => byPendingId[id])
      .filter((x): x is PendingAttachment => !!x && !x.outbound);
  }, [orderByChannel, byPendingId, channelId]);

  const [drag, setDrag] = useState<DragState>(EMPTY_DRAG);
  const containerRef = useRef<HTMLDivElement | null>(null);
  // Pending-pointer state: tracks pointerdown coords + id BEFORE the drag
  // threshold is crossed. Lives in a ref so we mutate synchronously
  // without forcing a re-render for every pointermove. Only when the
  // drag actually starts do we flip React state to render the cue.
  const pending = useRef<{
    pointerId: number;
    fromId: string;
    startX: number;
    startY: number;
    started: boolean;
  } | null>(null);

  const onPointerDown = (id: string, e: React.PointerEvent<HTMLDivElement>) => {
    // Left-click only — don't start drags on right-click context menus
    // or middle-click auto-scroll.
    if (e.button !== 0) return;
    pending.current = {
      pointerId: e.pointerId,
      fromId: id,
      startX: e.clientX,
      startY: e.clientY,
      started: false,
    };
  };

  // Document-level pointermove + pointerup. Anchored to document rather
  // than the row so that releasing OUTSIDE the row (the browser chrome,
  // a different panel) still cleanly cancels the drag instead of leaving
  // the UI stuck in a "ghost dragging" state.
  useEffect(() => {
    const onMove = (e: PointerEvent) => {
      const p = pending.current;
      if (!p || e.pointerId !== p.pointerId) return;

      // Threshold gate: small wiggles after pointerdown (or a click that
      // happens to start the gesture and never moves enough) shouldn't
      // promote into a drag. Once we cross the threshold, we lock in.
      if (!p.started) {
        const dx = e.clientX - p.startX;
        const dy = e.clientY - p.startY;
        if (dx * dx + dy * dy < DRAG_THRESHOLD_PX * DRAG_THRESHOLD_PX) return;
        p.started = true;
        setDrag({ draggingId: p.fromId, hoverId: null, side: null });
      }

      // Hit-test under the pointer. Walking up to the nearest
      // [data-pending-id] ancestor finds the tile; everything past the
      // tiles falls back to the row container's "end zone".
      const el = document.elementFromPoint(e.clientX, e.clientY);
      const tile = el?.closest("[data-pending-id]") as HTMLElement | null;

      if (tile) {
        const targetId = tile.dataset.pendingId ?? null;
        if (!targetId || targetId === p.fromId) {
          // Hovering self — clear any insertion cue. (We could keep the
          // last cue, but clearing makes the "no-op" state explicit.)
          if (drag.hoverId !== null) setDrag({ draggingId: p.fromId, hoverId: null, side: null });
          return;
        }
        const r = tile.getBoundingClientRect();
        const side: "before" | "after" =
          e.clientX - r.left < r.width / 2 ? "before" : "after";
        if (drag.hoverId !== targetId || drag.side !== side) {
          setDrag({ draggingId: p.fromId, hoverId: targetId, side });
        }
        return;
      }

      // Cursor is over the row but past the last tile — drop will land
      // at the end. Sentinel id paints the bar to the right of the
      // last tile; reorder() resolves it on pointerup.
      const container = containerRef.current;
      if (container) {
        const cr = container.getBoundingClientRect();
        const inRowVertically = e.clientY >= cr.top && e.clientY <= cr.bottom;
        if (inRowVertically && e.clientX >= cr.left) {
          if (drag.hoverId !== END_ZONE_ID) {
            setDrag({ draggingId: p.fromId, hoverId: END_ZONE_ID, side: "after" });
          }
          return;
        }
      }
      // Cursor wandered outside the row — clear the cue but keep the
      // drag alive so the user can re-enter.
      if (drag.hoverId !== null) setDrag({ draggingId: p.fromId, hoverId: null, side: null });
    };

    const onUp = (e: PointerEvent) => {
      const p = pending.current;
      if (!p || e.pointerId !== p.pointerId) return;
      pending.current = null;

      if (!p.started) {
        // Never crossed the threshold — treat as a click; nothing to do.
        return;
      }

      // Resolve the drop. We re-hit-test at pointerup rather than
      // trusting React state because the last pointermove may have been
      // skipped (e.g. user lifted on the same frame as a fast move).
      const el = document.elementFromPoint(e.clientX, e.clientY);
      const tile = el?.closest("[data-pending-id]") as HTMLElement | null;

      if (tile && tile.dataset.pendingId && tile.dataset.pendingId !== p.fromId) {
        const targetId = tile.dataset.pendingId;
        const r = tile.getBoundingClientRect();
        const side: "before" | "after" =
          e.clientX - r.left < r.width / 2 ? "before" : "after";
        reorder(channelId, p.fromId, targetId, side);
      } else if (
        // End-zone fallback: cursor in row vertically and past the last
        // tile horizontally → drop after last item.
        items.length > 0 &&
        containerRef.current &&
        (() => {
          const cr = containerRef.current.getBoundingClientRect();
          return (
            e.clientY >= cr.top &&
            e.clientY <= cr.bottom &&
            e.clientX >= cr.left
          );
        })()
      ) {
        const lastId = items[items.length - 1].pendingId;
        if (lastId !== p.fromId) {
          reorder(channelId, p.fromId, lastId, "after");
        }
      }

      setDrag(EMPTY_DRAG);
    };

    const onCancel = (e: PointerEvent) => {
      const p = pending.current;
      if (!p || e.pointerId !== p.pointerId) return;
      pending.current = null;
      setDrag(EMPTY_DRAG);
    };

    document.addEventListener("pointermove", onMove);
    document.addEventListener("pointerup", onUp);
    document.addEventListener("pointercancel", onCancel);
    return () => {
      document.removeEventListener("pointermove", onMove);
      document.removeEventListener("pointerup", onUp);
      document.removeEventListener("pointercancel", onCancel);
    };
    // drag is intentionally in deps so the latest closure sees current
    // hoverId/side when deciding whether to skip a setState (avoids
    // re-rendering the row on every mousemove while hovering one tile).
  }, [channelId, drag, items, reorder]);

  // FLIP-based reorder animation. Capture each tile's screen position
  // before paint, compare against the previous render's positions, and
  // for any tile whose position changed: instantly transform it back to
  // its old position (Invert), then transition to zero (Play). Result:
  // when the user drops a tile, every shifted tile slides smoothly into
  // its new spot rather than jumping.
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
      if (!prev) continue;
      const dx = prev.left - r.left;
      const dy = prev.top - r.top;
      if (dx === 0 && dy === 0) continue;
      tile.style.transition = "none";
      tile.style.transform = `translate(${dx}px, ${dy}px)`;
      moved.push(tile);
    }

    if (moved.length > 0) {
      void container.offsetHeight;
      requestAnimationFrame(() => {
        for (const tile of moved) {
          tile.style.transition =
            "transform 220ms cubic-bezier(0.2, 0, 0, 1), opacity 220ms ease";
          tile.style.transform = "";
        }
      });
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
        <PendingTile
          key={a.pendingId}
          a={a}
          drag={drag}
          onPointerDown={onPointerDown}
        />
      ))}
      {/* End-of-row visual cue (not a drop target — drop is detected via
          elementFromPoint and the row bbox in pointermove/pointerup). */}
      {drag.draggingId && drag.hoverId === END_ZONE_ID && (
        <div
          className="pointer-events-none relative shrink-0"
          style={{ width: 16 }}
        >
          <div
            className="absolute left-0 top-0 w-[3px] rounded-full bg-accent shadow-[0_0_8px_rgba(56,143,255,0.7)]"
            style={{ height: TILE_SIZE }}
          />
        </div>
      )}
    </div>
  );
}
