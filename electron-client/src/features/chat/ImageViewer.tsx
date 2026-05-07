import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { useImageViewerStore } from "../../stores/imageViewerStore";

/// Fullscreen lightbox for image attachments. CSS transforms drive
/// pinch-zoom + click-drag pan; Chromium handles the rendering. Esc
/// or click-outside closes.
export default function ImageViewer() {
  const current = useImageViewerStore((s) => s.current);
  const close = useImageViewerStore((s) => s.close);
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState({ x: 0, y: 0 });
  const dragStart = useRef<{ x: number; y: number; px: number; py: number } | null>(
    null,
  );

  useEffect(() => {
    if (!current) return;
    setZoom(1);
    setPan({ x: 0, y: 0 });
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close();
      if (e.key === "+" || e.key === "=") setZoom((z) => Math.min(8, z + 0.25));
      if (e.key === "-") setZoom((z) => Math.max(0.25, z - 0.25));
      if (e.key === "0") {
        setZoom(1);
        setPan({ x: 0, y: 0 });
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
  }, [current, close]);

  if (!current) return null;

  const onWheel = (e: React.WheelEvent) => {
    const delta = e.deltaY < 0 ? 0.15 : -0.15;
    setZoom((z) => Math.max(0.25, Math.min(8, z + delta)));
  };

  const onMouseDown = (e: React.MouseEvent) => {
    if (zoom <= 1) return;
    dragStart.current = { x: e.clientX, y: e.clientY, px: pan.x, py: pan.y };
  };

  const onMouseMove = (e: React.MouseEvent) => {
    const start = dragStart.current;
    if (!start) return;
    setPan({
      x: start.px + (e.clientX - start.x),
      y: start.py + (e.clientY - start.y),
    });
  };

  const onMouseUp = () => {
    dragStart.current = null;
  };

  return createPortal(
    <div
      className="fixed inset-0 z-[200] flex items-center justify-center bg-black/90"
      onClick={close}
      onWheel={onWheel}
      onMouseDown={onMouseDown}
      onMouseMove={onMouseMove}
      onMouseUp={onMouseUp}
      onMouseLeave={onMouseUp}
    >
      <button
        type="button"
        onClick={(e) => {
          e.stopPropagation();
          close();
        }}
        className="absolute right-4 top-4 flex h-10 w-10 items-center justify-center rounded-full bg-white/10 text-white hover:bg-white/20"
        title="Close (Esc)"
      >
        <svg width="20" height="20" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
          <line x1="6" y1="6" x2="18" y2="18" />
          <line x1="18" y1="6" x2="6" y2="18" />
        </svg>
      </button>
      <div className="absolute bottom-4 left-1/2 -translate-x-1/2 rounded-full bg-white/10 px-4 py-1.5 text-[11px] font-medium text-white/80">
        {current.filename} · {Math.round(zoom * 100)}%
      </div>
      <img
        src={current.url}
        alt={current.filename}
        draggable={false}
        onClick={(e) => e.stopPropagation()}
        style={{
          transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
          cursor: zoom > 1 ? "grab" : "zoom-in",
          maxHeight: zoom <= 1 ? "90vh" : undefined,
          maxWidth: zoom <= 1 ? "90vw" : undefined,
          transition: dragStart.current ? "none" : "transform 0.12s ease-out",
        }}
        className="select-none"
      />
    </div>,
    document.body,
  );
}
