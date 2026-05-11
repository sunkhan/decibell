import { useCallback, useRef, useState } from "react";

const MIN_WIDTH = 180;
const MAX_WIDTH = 480;
const DEFAULT_WIDTH = 240;

/// Sidebar resize handle. The width lives in a ref + direct DOM
/// mutation during drag so each mousemove (60Hz) doesn't trigger a
/// full sidebar re-render — it just writes `style.width` on the
/// wrapper element. State is only set on mouseup so the final value
/// survives subsequent re-renders.
///
/// Usage:
///   const { wrapperRef, width, onResizeMouseDown } = useSidebarResize();
///   return (
///     <div ref={wrapperRef} style={{ width }}>
///       ...
///       <div onMouseDown={onResizeMouseDown} className="resize-handle" />
///     </div>
///   );
export function useSidebarResize(initialWidth: number = DEFAULT_WIDTH): {
  wrapperRef: React.RefObject<HTMLDivElement>;
  width: number;
  onResizeMouseDown: (e: React.MouseEvent) => void;
} {
  const wrapperRef = useRef<HTMLDivElement>(null);
  const [width, setWidth] = useState(initialWidth);
  const liveWidthRef = useRef(initialWidth);

  const onResizeMouseDown = useCallback((e: React.MouseEvent) => {
    e.preventDefault();
    const startX = e.clientX;
    const startWidth = liveWidthRef.current;
    const wrapper = wrapperRef.current;
    if (!wrapper) return;

    const handleMouseMove = (ev: MouseEvent) => {
      const next = Math.min(MAX_WIDTH, Math.max(MIN_WIDTH, startWidth + (ev.clientX - startX)));
      // Direct DOM write — no React re-render during drag. This was
      // the old hot path: state updates 60×/sec while resizing
      // re-rendered the entire 460-line sidebar (and every child
      // including the participant lists) on each tick.
      wrapper.style.width = `${next}px`;
      liveWidthRef.current = next;
    };

    const handleMouseUp = () => {
      document.removeEventListener("mousemove", handleMouseMove);
      document.removeEventListener("mouseup", handleMouseUp);
      document.body.style.cursor = "";
      document.body.style.userSelect = "";
      // Sync to state so the next legitimate React re-render keeps
      // the width the user dragged to (otherwise the inline style
      // would get overwritten by `style={{ width }}` on next render).
      setWidth(liveWidthRef.current);
    };

    document.addEventListener("mousemove", handleMouseMove);
    document.addEventListener("mouseup", handleMouseUp);
    document.body.style.cursor = "col-resize";
    document.body.style.userSelect = "none";
  }, []);

  return { wrapperRef, width, onResizeMouseDown };
}
