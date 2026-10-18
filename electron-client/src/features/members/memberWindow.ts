// The member sidebar draws only the items near the viewport: paging adds
// 200 members per scroll, and keeping every row scrolled past piled up DOM
// in a big community. Every item has a fixed whole-pixel height (measured
// from the CSS by MembersList), so the layout is arithmetic: the Online
// header, the online rows, then, once the online part is complete, the
// Offline header and the offline rows. Spacers above and below the drawn
// items keep the scroll height. The loading row ends the list and is
// always drawn (its button can hold keyboard focus), so it stays out of
// the window. Pure, so it can be checked outside the app.

export type MemberPart = "online" | "offline";

/// Item heights in px.
export interface MemberListMetrics {
  row: number;
  onlineHeader: number;
  offlineHeader: number;
}

/// What the list holds: rows drawn per part, and whether the offline part
/// (header and rows) is drawn at all.
export interface MemberListShape {
  online: number;
  offline: number;
  showOffline: boolean;
}

export type MemberListItem =
  | { kind: "header"; part: MemberPart; top: number }
  | { kind: "member"; part: MemberPart; index: number; top: number };

interface Segment {
  kind: "header" | "member";
  part: MemberPart;
  top: number;
  count: number;
  height: number;
}

export interface MemberListLayout {
  segments: Segment[];
  /// Height of everything the window covers; the loading row starts here.
  total: number;
}

export function memberListLayout(shape: MemberListShape, m: MemberListMetrics): MemberListLayout {
  const segments: Segment[] = [];
  let top = 0;
  const add = (kind: Segment["kind"], part: MemberPart, count: number, height: number) => {
    segments.push({ kind, part, top, count, height });
    top += count * height;
  };
  add("header", "online", 1, m.onlineHeader);
  add("member", "online", shape.online, m.row);
  if (shape.showOffline) {
    add("header", "offline", 1, m.offlineHeader);
    add("member", "offline", shape.offline, m.row);
  }
  return { segments, total: top };
}

/// The items overlapping [from, to) (px from the list's top), in order,
/// with the spacer heights above and below them.
export function memberListWindow(
  layout: MemberListLayout,
  from: number,
  to: number,
): { items: MemberListItem[]; before: number; after: number } {
  const items: MemberListItem[] = [];
  let first = -1;
  let end = 0;
  for (const s of layout.segments) {
    if (s.count <= 0 || s.height <= 0) continue;
    if (s.top + s.count * s.height <= from || s.top >= to) continue;
    const i0 = Math.max(0, Math.floor((from - s.top) / s.height));
    const i1 = Math.min(s.count, Math.ceil((to - s.top) / s.height));
    for (let i = i0; i < i1; i++) {
      const top = s.top + i * s.height;
      if (first < 0) first = top;
      end = top + s.height;
      items.push(s.kind === "header" ? { kind: "header", part: s.part, top } : { kind: "member", part: s.part, index: i, top });
    }
  }
  if (first < 0) return { items, before: layout.total, after: 0 };
  return { items, before: first, after: layout.total - end };
}

/// Snapped to `step`, so the stretch worth drawing (and the list) changes
/// once per `step` px scrolled, with `overscan` px drawn either side of the
/// viewport. `scrollTop` is from the list's top.
export const WINDOW_STEP = 240;
export const WINDOW_OVERSCAN = 480;

export function windowRange(
  scrollTop: number,
  viewport: number,
  step = WINDOW_STEP,
  overscan = WINDOW_OVERSCAN,
): { from: number; to: number } {
  return {
    from: Math.max(0, Math.floor(scrollTop / step) * step - overscan),
    to: Math.ceil((scrollTop + Math.max(0, viewport)) / step) * step + overscan,
  };
}

/// The next page loads once the loading row (at `total`) comes within
/// `margin` px of the viewport's bottom.
export const PAGE_MARGIN = 200;

export function nearLoadedEnd(total: number, scrollTop: number, viewport: number, margin = PAGE_MARGIN): boolean {
  return total < scrollTop + viewport + margin;
}
