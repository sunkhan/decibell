// Scroll anchoring for message lists that page older history in at the
// top (ChatPanel and DmChatPanel both do).
//
// react-virtuoso anchors on `firstItemIndex`. Without it, prepending N
// items shifts every index by N and the library has no way to know the
// content above the viewport grew — it holds the same *index*, which is
// now a different message, so the list jumps by roughly the height of
// the page that just loaded.
//
// The contract is: start high, and decrement by exactly the number of
// items prepended. We derive that count from the data rather than from
// the fetch, because messages can also arrive at the tail while a
// history page is in flight, and only the array knows what actually
// landed where.

import { useState } from "react";

/// Virtuoso's documented starting point. Large enough that a session
/// can page back effectively forever without reaching 0.
export const VIRTUOSO_START_INDEX = 1_000_000;

interface Tracked {
  firstItemIndex: number;
  /// Identity of items[0] last time we looked. `null` before the first
  /// non-empty render.
  firstKey: string | number | null;
  length: number;
  /// Which list this state describes. Paging is per-channel/peer, so a
  /// switch has to reset rather than carry a stale offset over.
  resetKey: string | null;
}

/**
 * Returns the `firstItemIndex` to hand to `<Virtuoso>`.
 *
 * `keyOf` must return a stable identity per item — the same function
 * you give `computeItemKey`, so the two agree on what "the same
 * message" means.
 *
 * `resetKey` is the channel / peer id: when it changes the offset
 * resets, matching the `key=` remount on the Virtuoso instance.
 */
export function useVirtuosoPrepend<T>(
  items: T[],
  keyOf: (item: T) => string | number,
  resetKey: string | null,
): number {
  const [tracked, setTracked] = useState<Tracked>({
    firstItemIndex: VIRTUOSO_START_INDEX,
    firstKey: null,
    length: 0,
    resetKey,
  });

  const firstKey = items.length > 0 ? keyOf(items[0]) : null;

  // Derived state adjusted during render — the sanctioned React pattern
  // for "state that depends on props". React discards this render and
  // re-runs before painting, so Virtuoso never sees the longer array
  // paired with the stale offset. A layout effect would be one commit
  // too late and the jump would show.
  if (
    tracked.resetKey !== resetKey ||
    tracked.firstKey !== firstKey ||
    tracked.length !== items.length
  ) {
    if (tracked.resetKey !== resetKey) {
      setTracked({
        firstItemIndex: VIRTUOSO_START_INDEX,
        firstKey,
        length: items.length,
        resetKey,
      });
    } else {
      // A prepend is: the array grew AND the item that used to be first
      // is now somewhere further down. Its new position is exactly how
      // many items went in above it. Anything else — appends, edits,
      // deletions — leaves the offset alone.
      let prepended = 0;
      if (
        tracked.firstKey !== null &&
        firstKey !== tracked.firstKey &&
        items.length > tracked.length
      ) {
        const idx = items.findIndex((item) => keyOf(item) === tracked.firstKey);
        if (idx > 0) prepended = idx;
      }
      setTracked({
        firstItemIndex: tracked.firstItemIndex - prepended,
        firstKey,
        length: items.length,
        resetKey,
      });
    }
  }

  return tracked.firstItemIndex;
}
