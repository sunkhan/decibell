import { useEffect, useLayoutEffect, useMemo, useRef, useState, type RefObject } from "react";
import { useVoiceStore } from "../../../stores/voiceStore";

/// Layout maths + hooks for the voice stage (VoicePanel's tile grid and the
/// focused-stream filmstrip). Every tile is 16:9; the grid picks the column
/// count that gives the biggest tile for the space, and once tiles would
/// shrink below a floor the tail collapses into a "+N more" tile.

export const TILE_ASPECT = 16 / 9;
export const TILE_GAP = 12;
/// Smallest grid tile before people start collapsing into "+N more".
export const MIN_TILE_W = 200;
/// Biggest a participant tile grows (two people in a wide window).
export const MAX_TILE_W = 560;
/// The compact people row under live streams.
export const ROW_TILE_W = 176;
/// Filmstrip tiles under a focused stream.
export const STRIP_TILE_W = 128;
export const STRIP_GAP = 8;

/// A hidden speaker must talk this long before swapping into view (so a
/// cough doesn't reshuffle the grid)…
const SPEAK_MIN_MS = 600;
/// …and the grid swaps at most once per this window, so tiles never jump
/// around while a few hidden people trade sentences.
const SWAP_COOLDOWN_MS = 3000;

/// Best-fit tile width for `n` tiles in a `width`×`height` box: tries every
/// column count, keeps the one with the widest tile. `cols` is returned so
/// the caller can cap the row width — otherwise flex-wrap may pack an extra
/// tile per row when the height was the binding constraint (4 tiles as
/// 3 + 1 instead of 2 × 2).
export function fitGrid(
  n: number,
  width: number,
  height: number,
  gap: number,
  maxW: number,
): { cols: number; tileW: number } {
  if (n <= 0 || width <= 0 || height <= 0) return { cols: 1, tileW: 0 };
  let best = 0;
  let bestCols = 1;
  for (let cols = 1; cols <= n; cols++) {
    const rows = Math.ceil(n / cols);
    const byW = (width - gap * (cols - 1)) / cols;
    const byH = ((height - gap * (rows - 1)) / rows) * TILE_ASPECT;
    const w = Math.min(byW, byH, maxW);
    if (w > best) {
      best = w;
      bestCols = cols;
    }
  }
  return { cols: bestCols, tileW: Math.max(0, Math.floor(best)) };
}

/// How many tiles of at least `minW` fit in the box (always ≥ 1).
export function gridCapacity(width: number, height: number, gap: number, minW: number): number {
  const minH = minW / TILE_ASPECT;
  const cols = Math.max(1, Math.floor((width + gap) / (minW + gap)));
  const rows = Math.max(1, Math.floor((height + gap) / (minH + gap)));
  return cols * rows;
}

/// How many `tileW` tiles fit side by side in `width` (always ≥ 1).
export function rowCapacity(width: number, tileW: number, gap: number): number {
  return Math.max(1, Math.floor((width + gap) / (tileW + gap)));
}

/// Content-box size of an element, kept current by a ResizeObserver.
export function useElementSize(ref: RefObject<HTMLElement | null>): { width: number; height: number } {
  const [size, setSize] = useState({ width: 0, height: 0 });
  useLayoutEffect(() => {
    const el = ref.current;
    if (!el) return;
    const read = (w: number, h: number) =>
      setSize((prev) =>
        prev.width === Math.floor(w) && prev.height === Math.floor(h)
          ? prev
          : { width: Math.floor(w), height: Math.floor(h) },
      );
    const style = getComputedStyle(el);
    read(
      el.clientWidth - parseFloat(style.paddingLeft) - parseFloat(style.paddingRight),
      el.clientHeight - parseFloat(style.paddingTop) - parseFloat(style.paddingBottom),
    );
    const ro = new ResizeObserver(([entry]) => read(entry.contentRect.width, entry.contentRect.height));
    ro.observe(el);
    return () => ro.disconnect();
  }, [ref]);
  return size;
}

/// Which participants get a tile when there are more people than `slots`.
///
/// Pinned users (you, anyone streaming) always have one. The rest fill in
/// roster order; when a hidden user talks for SPEAK_MIN_MS they take the
/// slot of the visible, unpinned, silent user who has been quiet longest —
/// in place, so nobody else moves — at most once per SWAP_COOLDOWN_MS.
///
/// Speaking is read with a store subscription, not a selector: a selector on
/// speakingUsers would re-render the whole grid on every VAD flip. React
/// only hears about it when a swap actually happens.
///
/// Returns the usernames in display order. Below the cap it is simply `all`.
export function useVisibleParticipants(
  all: readonly string[],
  slots: number,
  pinned: ReadonlySet<string>,
): readonly string[] {
  const [swapTick, setSwapTick] = useState(0);
  const visibleRef = useRef<readonly string[]>([]);
  const lastSpoke = useRef(new Map<string, number>());
  const speakStart = useRef(new Map<string, number>());
  // -Infinity, not 0: performance.now() starts at page load, so a 0 here
  // held the very first swap back until 3 s after the app started.
  const lastSwap = useRef(-Infinity);
  // Latest inputs for the store callback, which outlives renders.
  const inputs = useRef({ all, slots, pinned });
  inputs.current = { all, slots, pinned };

  // Reconcile the kept set with the roster. Idempotent (a re-run from the
  // same inputs returns the same list), so StrictMode's double render is
  // harmless even though it writes the ref.
  const visible = useMemo(() => {
    void swapTick;
    if (all.length <= slots) {
      visibleRef.current = all;
      return all;
    }
    const present = new Set(all);
    const next = visibleRef.current.filter((u) => present.has(u));
    for (const u of all) if (pinned.has(u) && !next.includes(u)) next.push(u);
    for (const u of all) {
      if (next.length >= slots) break;
      if (!next.includes(u)) next.push(u);
    }
    // Too many (the window shrank, or someone started streaming): drop the
    // unpinned user who has been quiet longest, one at a time.
    while (next.length > slots) {
      let victim = -1;
      let oldest = Infinity;
      next.forEach((u, i) => {
        if (pinned.has(u)) return;
        const t = lastSpoke.current.get(u) ?? 0;
        // `<=`: on a tie (nobody has spoken yet) drop the latest joiner,
        // not the earliest.
        if (t <= oldest) {
          oldest = t;
          victim = i;
        }
      });
      if (victim < 0) break;
      next.splice(victim, 1);
    }
    const result = next.length > slots ? next.slice(0, slots) : next;
    visibleRef.current = result;
    return result;
  }, [all, slots, pinned, swapTick]);

  useEffect(() => {
    let timer: ReturnType<typeof setTimeout> | undefined;
    const schedule = (ms: number) => {
      if (timer) clearTimeout(timer);
      timer = setTimeout(check, Math.max(0, ms) + 16);
    };
    function check() {
      timer = undefined;
      const { all: roster, slots: cap, pinned: pins } = inputs.current;
      if (roster.length <= cap) return;
      const speaking = useVoiceStore.getState().speakingUsers;
      const shown = new Set(visibleRef.current);
      const now = performance.now();
      let candidate: string | null = null;
      let wait = Infinity;
      for (const u of speaking) {
        if (shown.has(u) || !roster.includes(u)) continue;
        const started = speakStart.current.get(u) ?? now;
        const remaining = Math.max(
          SPEAK_MIN_MS - (now - started),
          SWAP_COOLDOWN_MS - (now - lastSwap.current),
        );
        if (remaining <= 0) {
          candidate = u;
          break;
        }
        wait = Math.min(wait, remaining);
      }
      if (!candidate) {
        if (wait < Infinity) schedule(wait);
        return;
      }
      let victim = -1;
      let oldest = Infinity;
      visibleRef.current.forEach((u, i) => {
        if (pins.has(u) || speaking.has(u)) return;
        const t = lastSpoke.current.get(u) ?? 0;
        if (t <= oldest) {
          oldest = t;
          victim = i;
        }
      });
      if (victim < 0) {
        // Every visible tile is pinned or talking — try again later.
        schedule(SWAP_COOLDOWN_MS);
        return;
      }
      const next = [...visibleRef.current];
      next[victim] = candidate;
      visibleRef.current = next;
      lastSwap.current = now;
      setSwapTick((t) => t + 1);
      // Another hidden speaker may be queued behind this one.
      schedule(SWAP_COOLDOWN_MS);
    }
    const unsubscribe = useVoiceStore.subscribe((state, prev) => {
      if (state.speakingUsers === prev.speakingUsers) return;
      const now = performance.now();
      for (const u of state.speakingUsers) {
        if (!prev.speakingUsers.has(u)) speakStart.current.set(u, now);
        lastSpoke.current.set(u, now);
      }
      for (const u of prev.speakingUsers) {
        if (!state.speakingUsers.has(u)) {
          speakStart.current.delete(u);
          lastSpoke.current.set(u, now);
        }
      }
      check();
    });
    return () => {
      unsubscribe();
      if (timer) clearTimeout(timer);
    };
  }, []);

  return visible;
}
