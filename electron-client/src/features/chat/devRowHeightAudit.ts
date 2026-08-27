// Dev-only measurement aid for the attachment scroll glitch — suspect
// #1 in docs/reviews/2026-07-27-frontend-review.md ("row height
// changing after mount"). A post-mount height settle above the viewport
// shifts everything below it (scroll anchoring compensates, but only
// after the fact), so late-settling rows are visible-glitch candidates.
// This logs every such settle with the message identity and the delta.
//
// Zero cost in production: the flag is compile-time constant, so the
// observer branch is dead code outside dev builds.

import { useLayoutEffect, useRef } from "react";

const ENABLED = import.meta.env.DEV;

/**
 * Attach the returned ref to a message row's root element. Any height
 * change after the first observed paint logs a warning naming the row.
 */
export function useRowHeightAudit(label: string | number) {
  const ref = useRef<HTMLDivElement | null>(null);
  useLayoutEffect(() => {
    if (!ENABLED) return;
    const el = ref.current;
    if (!el) return;
    let first: number | null = null;
    const observer = new ResizeObserver((entries) => {
      const h = entries[entries.length - 1]?.contentRect.height;
      if (h === undefined) return;
      if (first === null) {
        first = h;
        return;
      }
      if (Math.abs(h - first) > 0.5) {
        // A settle on an off-screen row is absorbed by scroll anchoring
        // and is benign; only on-screen settles are visible-glitch
        // candidates.
        const rect = el.getBoundingClientRect();
        const where =
          rect.bottom > 0 && rect.top < window.innerHeight
            ? "ON-SCREEN"
            : "off-screen";
        // eslint-disable-next-line no-console
        console.warn(
          `[row-audit] message ${label}: height ${first.toFixed(1)} → ${h.toFixed(1)} after first paint (Δ${(h - first).toFixed(1)}px, ${where})`,
        );
        first = h;
      }
    });
    observer.observe(el);
    return () => observer.disconnect();
  }, [label]);
  return ref;
}
