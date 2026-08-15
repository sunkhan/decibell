// Dev-only measurement aid for the attachment scroll glitch — suspect
// #1 in docs/reviews/2026-07-27-frontend-review.md ("row height
// changing after mount"). Virtuoso re-measures and corrects the scroll
// offset on every post-mount height change, so if rows are settling
// late, each settle is a visible jump. This logs every such settle with
// the message identity and the delta, which is the evidence that
// hypothesis has never had.
//
// Zero cost in production: the flag is compile-time constant, so the
// observer branch is dead code outside dev builds.

import { useLayoutEffect, useRef } from "react";

const ENABLED = import.meta.env.DEV;

/**
 * Attach the returned ref to a Virtuoso row's root element. Any height
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
        // A settle on an off-screen row is absorbed by Virtuoso's
        // anchoring and is benign (the eager-pagination fix works by
        // pushing the page-boundary group-flip into that category);
        // only on-screen settles are visible-glitch candidates.
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
