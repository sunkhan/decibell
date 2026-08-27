// Real-DOM sliding-window message list — the Discord model. Design + the
// Virtuoso postmortem that motivated it:
// docs/superpowers/specs/2026-08-25-real-dom-message-list-plan.md
//
// The loaded slice renders as plain DOM in an overflow-y:auto scroller: no
// virtualization, therefore no ESTIMATED row height anywhere. Placement —
// jump landing, bottom-follow, prepend anchoring — is arithmetic over real
// offsets, done in one layout effect before paint. The DOM stays bounded
// because the parent trims the far end of the slice when it grows past
// MAX_ROWS (onOverflow): a 10M-message channel costs the same DOM as a new
// one. The component knows nothing about stores or the network; the parent
// owns pagination guards, trims, and position persistence.
//
// Invariants (each Virtuoso jump round was a violation of one of these):
//  - Measure with offsetTop/offsetHeight, never getBoundingClientRect: the
//    arrival slide and the last row's fadeUp are transforms and pollute
//    rects, and a prepend can land inside the 260ms slide.
//  - The placement effect has NO deps — it must run on every commit. A parent
//    re-render can change row heights (inline editor, grouping flip) without
//    a change to `items`.
//  - overflow-anchor:none — this component is the only thing that adjusts
//    scrollTop, so Chromium's own anchoring must not double-adjust. That
//    means we also own settles with no React commit (window resize rewraps
//    text, upload-progress chips): the inner-wrapper ResizeObserver does it.
//  - Trims cut by pixels, never by count: the trimmed edge stays >= KEEP_PX
//    from the viewport so it can't land inside the paging zone and ping-pong
//    (trimTail → nearBottom → appendNewer → trimHead → nearTop → …).
//  - Rows are keyed by message identity only (never by index): React would
//    reuse an index-keyed node for a different message under prepend, and
//    the anchor would then point at the wrong row.

import {
  forwardRef,
  useImperativeHandle,
  useLayoutEffect,
  useRef,
  type ForwardedRef,
  type ReactElement,
  type ReactNode,
  type Ref,
} from "react";

/// Row count past which the parent is asked to trim the far end of the slice.
const MAX_ROWS = 150;
/// Distance (px) from a scroll edge at which the next page is requested.
const NEAR_PX = 800;
/// A trim never cuts closer than this (px) to the viewport.
const KEEP_PX = 2 * NEAR_PX;
/// Slack (px) under which the viewport counts as "at the bottom".
const AT_BOTTOM_PX = 4;
/// How many viewport rows to snapshot as anchors. The first one that survives
/// a commit is used, so deleting the topmost visible row (or a prune above
/// it) doesn't lose the position.
const ANCHOR_ROWS = 40;
/// Arrival slide on a jump landing (mirrors the old jumpArriveUp/Down
/// keyframes; WAAPI so nothing remounts and no class needs re-triggering).
const SLIDE_PX = 28;
const SLIDE_MS = 260;

export interface JumpTarget {
  id: number;
  /// Bumps per jump so the same id can be jumped to twice.
  epoch: number;
  /// Travel direction — picks the slide keyframe ("up" = to an older row).
  dir: "up" | "down";
}

/// A restorable position: the topmost visible message and where its top edge
/// sat relative to the viewport top (px, usually <= 0). `atBottom` wins.
export interface ListPosition {
  anchorId: number;
  offset: number;
  atBottom: boolean;
}

export interface ScrollState extends ListPosition {
  firstVisible: number;
  lastVisible: number;
}

export interface RealMessageListHandle {
  scrollToBottom: (smooth: boolean) => void;
}

export interface RealMessageListProps<T> {
  items: T[];
  /// Stable, non-empty identity per item: a real id, else a client nonce.
  keyOf: (item: T) => string | number;
  renderItem: (item: T, index: number) => ReactNode;
  /// hasMoreAfter — newer messages exist below the slice. Disables
  /// bottom-follow (nothing appended is "the present").
  windowed: boolean;
  /// Set at click time, even before the target is loaded: the list lands on
  /// the first commit whose rows contain it, i.e. an around-window paints at
  /// the right position on its first frame.
  jumpTarget: JumpTarget | null;
  /// Applied once, at mount.
  initialPosition?: ListPosition;
  /// rAF-coalesced; fired after scrolls and commits.
  onScrollState: (s: ScrollState) => void;
  /// Within NEAR_PX of the top / bottom edge (fires repeatedly — the parent
  /// dedups per page boundary). Also checked after every commit so a page
  /// that doesn't fill the zone pulls the next one, and a lost response is
  /// retried on the next scroll.
  onNearTop: () => void;
  onNearBottom: () => void;
  /// Slice is over MAX_ROWS: drop rows from `side`, keeping `keep`.
  onOverflow: (side: "head" | "tail", keep: number) => void;
  onJumpLanded: (epoch: number, id: number) => void;
  className?: string;
}

interface Anchor {
  key: string;
  /// Row top edge relative to the viewport top at snapshot time.
  relTop: number;
}

type Rows = HTMLCollectionOf<HTMLElement>;

/// First row whose bottom edge is below `y` (scroller-content px). Rows are
/// stacked top-to-bottom, so this is a binary search.
function firstRowBelow(rows: Rows, y: number): number {
  let lo = 0;
  let hi = rows.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    const r = rows[mid];
    if (r.offsetTop + r.offsetHeight > y) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

/// First row whose top edge is at or below `y`.
function firstRowAtOrBelow(rows: Rows, y: number): number {
  let lo = 0;
  let hi = rows.length;
  while (lo < hi) {
    const mid = (lo + hi) >> 1;
    if (rows[mid].offsetTop >= y) hi = mid;
    else lo = mid + 1;
  }
  return lo;
}

function keyMap(rows: Rows): Map<string, HTMLElement> {
  const map = new Map<string, HTMLElement>();
  for (let i = 0; i < rows.length; i++) map.set(rows[i].dataset.key ?? "", rows[i]);
  return map;
}

function RealMessageListInner<T>(
  {
    items,
    keyOf,
    renderItem,
    windowed,
    jumpTarget,
    initialPosition,
    onScrollState,
    onNearTop,
    onNearBottom,
    onOverflow,
    onJumpLanded,
    className,
  }: RealMessageListProps<T>,
  ref: ForwardedRef<RealMessageListHandle>,
) {
  const scrollerRef = useRef<HTMLDivElement>(null);
  const innerRef = useRef<HTMLDivElement>(null);
  // Latest props for the listeners created once at mount (ResizeObserver,
  // the rAF callback).
  const latest = useRef({ windowed, onScrollState, onNearTop, onNearBottom });
  latest.current = { windowed, onScrollState, onNearTop, onNearBottom };

  const anchorsRef = useRef<Anchor[]>([]);
  const atBottomRef = useRef(true);
  const visibleRef = useRef({ first: 0, last: -1 });
  const mountedRef = useRef(false);
  const landedEpochRef = useRef(0);
  const prevFirstKeyRef = useRef<string | null>(null);
  const rafRef = useRef(0);

  useImperativeHandle(
    ref,
    () => ({
      scrollToBottom: (smooth) => {
        const el = scrollerRef.current;
        if (!el) return;
        el.scrollTo({ top: el.scrollHeight, behavior: smooth ? "smooth" : "auto" });
      },
    }),
    [],
  );

  // Snapshot the viewport: atBottom, the visible index range, and the anchor
  // rows. Layout is clean during a scroll event and after a commit's forced
  // layout, so these reads are cheap.
  const measure = () => {
    const scroller = scrollerRef.current;
    const inner = innerRef.current;
    if (!scroller || !inner) return;
    const rows = inner.children as Rows;
    const st = scroller.scrollTop;
    const vh = scroller.clientHeight;
    atBottomRef.current = scroller.scrollHeight - st - vh <= AT_BOTTOM_PX;
    const first = firstRowBelow(rows, st);
    const anchors: Anchor[] = [];
    let last = first - 1;
    for (let i = first; i < rows.length; i++) {
      const r = rows[i];
      if (r.offsetTop >= st + vh) break;
      last = i;
      if (anchors.length < ANCHOR_ROWS) {
        anchors.push({ key: r.dataset.key ?? "", relTop: r.offsetTop - st });
      }
    }
    anchorsRef.current = anchors;
    visibleRef.current = { first, last };
  };

  const scheduleScrollState = () => {
    if (rafRef.current) return;
    rafRef.current = requestAnimationFrame(() => {
      rafRef.current = 0;
      const a = anchorsRef.current[0];
      latest.current.onScrollState({
        anchorId: a && /^\d+$/.test(a.key) ? Number(a.key) : 0,
        offset: a ? a.relTop : 0,
        atBottom: atBottomRef.current,
        firstVisible: visibleRef.current.first,
        lastVisible: visibleRef.current.last,
      });
    });
  };

  const checkEdges = () => {
    const scroller = scrollerRef.current;
    if (!scroller) return;
    if (scroller.scrollTop < NEAR_PX) latest.current.onNearTop();
    if (scroller.scrollHeight - scroller.scrollTop - scroller.clientHeight < NEAR_PX) {
      latest.current.onNearBottom();
    }
  };

  // Put the first surviving anchor row back where it was. Returns false when
  // none survived (the slice was replaced).
  const restoreAnchor = (scroller: HTMLDivElement, byKey: Map<string, HTMLElement>) => {
    for (const a of anchorsRef.current) {
      const row = byKey.get(a.key);
      if (!row) continue;
      const delta = row.offsetTop - scroller.scrollTop - a.relTop;
      if (Math.abs(delta) > 0.5) scroller.scrollTop += delta;
      return true;
    }
    return false;
  };

  // Placement pass — every commit (no deps). The DOM is fully mutated and
  // nothing has painted, so a scrollTop write here is flicker-free.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    const inner = innerRef.current;
    if (!scroller || !inner) return;
    const rows = inner.children as Rows;
    const byKey = keyMap(rows);
    const firstKey = rows.length > 0 ? rows[0].dataset.key ?? null : null;
    const prevFirst = prevFirstKeyRef.current;
    // The head grew (older page prepended) when the row that used to be
    // first is still here but no longer first.
    const grewHead =
      mountedRef.current && prevFirst !== null && firstKey !== prevFirst && byKey.has(prevFirst);
    prevFirstKeyRef.current = firstKey;

    const jt = jumpTarget;
    const jumpRow =
      jt && jt.epoch !== landedEpochRef.current ? byKey.get(String(jt.id)) : undefined;
    if (jt && jumpRow) {
      // Centered — exact by construction, every height is real.
      landedEpochRef.current = jt.epoch;
      const max = Math.max(0, scroller.scrollHeight - scroller.clientHeight);
      const top = jumpRow.offsetTop - (scroller.clientHeight - jumpRow.offsetHeight) / 2;
      scroller.scrollTop = Math.max(0, Math.min(max, top));
      const from = jt.dir === "up" ? -SLIDE_PX : SLIDE_PX;
      inner.animate(
        [
          { transform: `translateY(${from}px)`, opacity: 0.65 },
          { transform: "none", opacity: 1 },
        ],
        { duration: SLIDE_MS, easing: "ease-out" },
      );
      onJumpLanded(jt.epoch, jt.id);
    } else if (!mountedRef.current) {
      const ip = initialPosition;
      const row =
        ip && !ip.atBottom && ip.anchorId > 0 ? byKey.get(String(ip.anchorId)) : undefined;
      scroller.scrollTop = row ? row.offsetTop - (ip?.offset ?? 0) : scroller.scrollHeight;
    } else if (atBottomRef.current && !windowed) {
      // Bottom-follow: instant, never smooth. A smooth scroll targets an
      // offset computed at start, so a second append mid-flight lands short
      // (Virtuoso round 1); the last row's fadeUp supplies the motion.
      scroller.scrollTop = scroller.scrollHeight;
    } else if (!restoreAnchor(scroller, byKey) && anchorsRef.current.length > 0) {
      // Slice replaced with no jump (present reload) — land at the bottom.
      scroller.scrollTop = scroller.scrollHeight;
    }
    mountedRef.current = true;

    if (rows.length > MAX_ROWS) {
      const st = scroller.scrollTop;
      const vh = scroller.clientHeight;
      // Tail cut: first row that starts KEEP_PX below the viewport. Head
      // cut: rows that end KEEP_PX above it. Prefer the side opposite the
      // growth; either side is safe by the pixel rule.
      const tailCut = firstRowAtOrBelow(rows, st + vh + KEEP_PX);
      const headEnd = firstRowBelow(rows, st - KEEP_PX);
      const tail = tailCut < rows.length ? () => onOverflow("tail", tailCut) : null;
      const head = headEnd > 0 ? () => onOverflow("head", rows.length - headEnd) : null;
      (grewHead ? tail ?? head : head ?? tail)?.();
    }

    checkEdges();
    measure();
    scheduleScrollState();
  });

  // Height changes with no React commit — AttachmentList re-rendering on
  // chatViewSize, upload chips, font load, text rewrap on a panel resize —
  // and viewport changes (composer growth, window resize). ResizeObserver
  // runs after layout and before paint; after a commit the placement pass
  // already handled, the residual is 0, so this is idempotent.
  useLayoutEffect(() => {
    const scroller = scrollerRef.current;
    const inner = innerRef.current;
    if (!scroller || !inner) return;
    const ro = new ResizeObserver(() => {
      if (atBottomRef.current && !latest.current.windowed) {
        scroller.scrollTop = scroller.scrollHeight;
      } else {
        restoreAnchor(scroller, keyMap(inner.children as Rows));
      }
      measure();
      scheduleScrollState();
    });
    ro.observe(inner);
    ro.observe(scroller);
    return () => {
      ro.disconnect();
      if (rafRef.current) cancelAnimationFrame(rafRef.current);
      rafRef.current = 0;
    };
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  const handleScroll = () => {
    // Synchronous: an IPC prepend can land between a wheel tick and its rAF,
    // and it must anchor on this tick's position.
    measure();
    checkEdges();
    scheduleScrollState();
  };

  return (
    <div
      ref={scrollerRef}
      className={`relative flex-1 overflow-y-auto ${className ?? ""}`}
      style={{ overflowAnchor: "none" }}
      onScroll={handleScroll}
    >
      {/* Unpositioned so every row's offsetParent is the scroller (offsetTop
          includes the justify-end gap); min-h-full + justify-end stacks a
          short list against the composer, Discord-style, and is inert once
          content outgrows the viewport. */}
      <div ref={innerRef} className="flex min-h-full flex-col justify-end">
        {items.map((item, i) => {
          const key = keyOf(item);
          if (import.meta.env.DEV && key === "") {
            console.warn("RealMessageList: empty key at index", i);
          }
          return (
            <div key={key} data-key={String(key)}>
              {renderItem(item, i)}
            </div>
          );
        })}
      </div>
    </div>
  );
}

const RealMessageList = forwardRef(RealMessageListInner) as unknown as <T>(
  props: RealMessageListProps<T> & { ref?: Ref<RealMessageListHandle> },
) => ReactElement | null;

export default RealMessageList;
