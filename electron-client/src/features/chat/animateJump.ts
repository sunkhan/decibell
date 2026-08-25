// Discord-style animated jump for virtualized message lists.
//
// Native `scrollTo({behavior: "smooth"})` eases toward a destination pixel
// computed ONCE, from estimated row heights — rows that mount or measure
// mid-animation shift the true destination and the glide lands wrong (the
// repeated first-click mis-landing). Discord instead drives the scroll
// itself, re-deriving the destination from the target row's live DOM rect
// every frame, so mid-flight resizes and history prepends are absorbed: the
// animation converges on wherever the row actually IS, not where it was
// estimated to be.
//
// `findEl` must re-query the DOM each call (the row can remount as Virtuoso
// recycles). Returns a cancel function; the caller must invoke it on a new
// jump, channel switch, or unmount.

export function animateJumpToElement(
  scroller: HTMLElement,
  findEl: () => HTMLElement | null,
  onDone?: () => void,
): () => void {
  let raf = 0;
  let cancelled = false;

  const step = () => {
    if (cancelled) return;
    const el = findEl();
    if (!el) {
      // Row unmounted (scrolled out of Virtuoso's window mid-flight) —
      // nothing sane to ease toward; stop and let the caller's fallback
      // (remount path) own correctness.
      onDone?.();
      return;
    }
    // Rect-based, not offsetTop — immune to whichever ancestor is the
    // offsetParent inside Virtuoso's scroller/viewport nesting.
    const scRect = scroller.getBoundingClientRect();
    const elRect = el.getBoundingClientRect();
    const elCenter = elRect.top - scRect.top + scroller.scrollTop + elRect.height / 2;
    const target = Math.max(
      0,
      Math.min(
        elCenter - scroller.clientHeight / 2,
        scroller.scrollHeight - scroller.clientHeight,
      ),
    );
    const delta = target - scroller.scrollTop;
    if (Math.abs(delta) < 1) {
      scroller.scrollTop = target;
      onDone?.();
      return;
    }
    // Exponential ease: cover 18% of the REMAINING distance per frame.
    // ~95% covered in ~16 frames (~270ms at 60Hz) — quick and snappy, and
    // because `target` is re-derived every frame it cannot mis-land.
    scroller.scrollTop += delta * 0.18;
    raf = requestAnimationFrame(step);
  };

  raf = requestAnimationFrame(step);
  return () => {
    cancelled = true;
    cancelAnimationFrame(raf);
  };
}
