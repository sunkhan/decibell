import {
  useLayoutEffect,
  useRef,
  useState,
  type CSSProperties,
  type RefObject,
} from "react";

export interface MenuAnchor {
  x: number;
  y: number;
}

export interface MenuPositionOptions {
  /// Side of the anchor to open on when both fit. "below" puts the menu's
  /// top-left corner at the anchor (a right-click menu at the cursor);
  /// "above" puts its bottom-left corner there (a menu over a control
  /// docked at the bottom of the window).
  prefer?: "below" | "above";
  /// Gap kept from every viewport edge.
  margin?: number;
}

const DEFAULT_MARGIN = 8;

/// Position a `position: fixed` menu at an anchor point so the whole menu
/// stays inside the viewport. The menu is measured once it has rendered
/// (layout effect, so the corrected position is what the first frame
/// paints), re-measured when its content resizes, and re-clamped when the
/// window does. When the menu doesn't fit on the preferred side it flips
/// to the other side of the anchor, like a desktop menu, and only slides
/// along the edge when neither side fits.
///
/// Pass a fresh anchor object per opening (the store's `contextMenuAnchor`,
/// a `useState` object, a `useMemo` on the coordinates): it doubles as the
/// key that re-measures a re-opened menu. Callers that used a hard-coded
/// "approximate height" for the clamp were the ones clipping at the bottom
/// edge once the menu grew a section.
export function useMenuPosition<T extends HTMLElement = HTMLDivElement>(
  anchor: MenuAnchor | null,
  { prefer = "below", margin = DEFAULT_MARGIN }: MenuPositionOptions = {},
): { ref: RefObject<T>; style: CSSProperties } {
  const ref = useRef<T>(null);
  const [pos, setPos] = useState<{ left: number; top: number } | null>(null);

  useLayoutEffect(() => {
    if (!anchor) {
      setPos(null);
      return;
    }
    const el = ref.current;
    if (!el) return;
    const place = () => {
      const { width, height } = el.getBoundingClientRect();
      const next = placeMenu(anchor, width, height, prefer, margin);
      setPos((prev) =>
        prev && prev.left === next.left && prev.top === next.top ? prev : next,
      );
    };
    place();
    const observer = new ResizeObserver(place);
    observer.observe(el);
    window.addEventListener("resize", place);
    return () => {
      observer.disconnect();
      window.removeEventListener("resize", place);
    };
  }, [anchor, prefer, margin]);

  // A menu taller than the viewport scrolls inside itself rather than
  // running off the bottom. Until the first measurement lands the menu is
  // laid out at the raw anchor but not shown — opacity, not visibility,
  // so a focusable child can still take focus in the same frame.
  const style: CSSProperties = {
    position: "fixed",
    left: pos ? pos.left : (anchor?.x ?? 0),
    top: pos ? pos.top : (anchor?.y ?? 0),
    maxHeight: `calc(100vh - ${margin * 2}px)`,
    overflowY: "auto",
    ...(pos ? {} : { opacity: 0, pointerEvents: "none" as const }),
  };
  return { ref, style };
}

/// Pure placement: prefer the requested side, flip to the other side when
/// the menu doesn't fit there, clamp to the edge when neither fits.
export function placeMenu(
  anchor: MenuAnchor,
  width: number,
  height: number,
  prefer: "below" | "above",
  margin: number,
): { left: number; top: number } {
  const vw = window.innerWidth;
  const vh = window.innerHeight;

  const fitsBelow = anchor.y + height <= vh - margin;
  const fitsAbove = anchor.y - height >= margin;
  const clampedTop = Math.max(margin, Math.min(anchor.y, vh - height - margin));
  let top: number;
  if (prefer === "below") {
    top = fitsBelow ? anchor.y : fitsAbove ? anchor.y - height : clampedTop;
  } else {
    top = fitsAbove ? anchor.y - height : fitsBelow ? anchor.y : clampedTop;
  }

  const fitsRight = anchor.x + width <= vw - margin;
  const fitsLeft = anchor.x - width >= margin;
  const clampedLeft = Math.max(margin, Math.min(anchor.x, vw - width - margin));
  const left = fitsRight ? anchor.x : fitsLeft ? anchor.x - width : clampedLeft;

  return { left: Math.round(left), top: Math.round(top) };
}
