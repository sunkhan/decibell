import { getCurrentWindow } from "@tauri-apps/api/window";

// Start edge/corner resize on mousedown. Invisible 5px strips; zero runtime cost
// when idle (no listeners beyond native DOM events), small fixed DOM footprint.
const win = getCurrentWindow();

type Dir = "Top" | "Bottom" | "Left" | "Right" | "TopLeft" | "TopRight" | "BottomLeft" | "BottomRight";

function handle(dir: Dir, className: string, cursor: string) {
  return (
    <div
      key={dir}
      onMouseDown={(e) => {
        if (e.button !== 0) return;
        win.startResizeDragging(dir as never).catch(() => {});
      }}
      className={`absolute z-[9999] ${className}`}
      style={{ cursor }}
    />
  );
}

export default function ResizeHandles() {
  return (
    <>
      {handle("Top", "top-0 left-2 right-2 h-[4px]", "ns-resize")}
      {handle("Bottom", "bottom-0 left-2 right-2 h-[4px]", "ns-resize")}
      {handle("Left", "left-0 top-2 bottom-2 w-[4px]", "ew-resize")}
      {handle("Right", "right-0 top-2 bottom-2 w-[4px]", "ew-resize")}
      {handle("TopLeft", "left-0 top-0 w-2 h-2", "nwse-resize")}
      {handle("TopRight", "right-0 top-0 w-2 h-2", "nesw-resize")}
      {handle("BottomLeft", "left-0 bottom-0 w-2 h-2", "nesw-resize")}
      {handle("BottomRight", "right-0 bottom-0 w-2 h-2", "nwse-resize")}
    </>
  );
}
