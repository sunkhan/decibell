import { useCallback, useRef } from "react";
import { useUiStore, SIDEBAR_WIDTH_MIN, SIDEBAR_WIDTH_MAX } from "../../stores/uiStore";

/// Sidebar resize handle. The width is `uiStore.sidebarWidth` — one
/// value shared by ServerChannelsSidebar and ConversationSidebar (only
/// one is mounted at a time, and "the left sidebar is this wide" is one
/// preference), remembered per install so it survives view switches
/// and restarts. During a drag the width lives in a ref + direct DOM
/// mutation so each mousemove (60Hz) doesn't trigger a full sidebar
/// re-render — it just writes `style.width` on the wrapper element.
/// The store is written once on mouseup so the final value survives
/// subsequent re-renders.
///
/// Usage:
///   const { wrapperRef, width, onResizeMouseDown } = useSidebarResize();
///   return (
///     <div ref={wrapperRef} style={{ width }}>
///       ...
///       <div onMouseDown={onResizeMouseDown} className="resize-handle" />
///     </div>
///   );
export function useSidebarResize(): {
  wrapperRef: React.RefObject<HTMLDivElement>;
  width: number;
  onResizeMouseDown: (e: React.MouseEvent) => void;
} {
  const wrapperRef = useRef<HTMLDivElement>(null);
  const width = useUiStore((s) => s.sidebarWidth);
  const setSidebarWidth = useUiStore((s) => s.setSidebarWidth);
  // Seeded once from the store; only the drag handlers write it after
  // that. Syncing it on every render would let a re-render mid-drag
  // (a message arriving, say) reset it to the pre-drag width, which
  // mouseup would then commit.
  const liveWidthRef = useRef(width);

  const onResizeMouseDown = useCallback(
    (e: React.MouseEvent) => {
      e.preventDefault();
      const startX = e.clientX;
      const startWidth = liveWidthRef.current;
      const wrapper = wrapperRef.current;
      if (!wrapper) return;

      const handleMouseMove = (ev: MouseEvent) => {
        const next = Math.min(
          SIDEBAR_WIDTH_MAX,
          Math.max(SIDEBAR_WIDTH_MIN, startWidth + (ev.clientX - startX)),
        );
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
        // Commit to the store (and localStorage) so the next legitimate
        // React re-render keeps the width the user dragged to —
        // otherwise the inline style would get overwritten by
        // `style={{ width }}` on the next render.
        setSidebarWidth(liveWidthRef.current);
      };

      document.addEventListener("mousemove", handleMouseMove);
      document.addEventListener("mouseup", handleMouseUp);
      document.body.style.cursor = "col-resize";
      document.body.style.userSelect = "none";
    },
    [setSidebarWidth],
  );

  return { wrapperRef, width, onResizeMouseDown };
}
