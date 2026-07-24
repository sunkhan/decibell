import { useEffect, useRef } from "react";

/// Close a modal/overlay when Escape is pressed while `enabled` (default
/// true). `onClose` is held in a ref so passing an inline handler doesn't
/// re-subscribe the listener on every render.
export function useEscapeToClose(onClose: () => void, enabled = true): void {
  const ref = useRef(onClose);
  ref.current = onClose;
  useEffect(() => {
    if (!enabled) return;
    const handler = (e: KeyboardEvent) => {
      if (e.key === "Escape") ref.current();
    };
    window.addEventListener("keydown", handler);
    return () => window.removeEventListener("keydown", handler);
  }, [enabled]);
}
