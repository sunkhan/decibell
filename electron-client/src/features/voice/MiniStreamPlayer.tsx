import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore, PIP_WIDTH_MIN, PIP_WIDTH_MAX, type PipCorner } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import {
  getFullViewRect,
  placeStreamPip,
  recordMiniRect,
} from "./streamPipHost";

type Corner = PipCorner;

const MARGIN = 16;
// The mini is aspect-locked to 16:9; the user resizes its width between these
// bounds (height follows). The chosen width and corner live in uiStore
// (pipWidth / pipCorner), which owns the bounds, clamps what it reads back
// from localStorage, and remembers both per install.
const MIN_WIDTH = PIP_WIDTH_MIN;
const MAX_WIDTH = PIP_WIDTH_MAX;
const ASPECT = 9 / 16; // height / width
const heightFor = (w: number) => Math.round(w * ASPECT);
// How far the pointer must move before a press counts as a drag (vs a click).
const DRAG_THRESHOLD = 4;

// The resize grip lives on the box corner facing the screen interior (opposite
// the corner it's docked to), so dragging it grows the box inward while the
// docked corner stays pinned. `rot` orients the grip glyph toward that corner.
const OPPOSITE_CORNER: Record<Corner, Corner> = {
  "top-left": "bottom-right",
  "top-right": "bottom-left",
  "bottom-left": "top-right",
  "bottom-right": "top-left",
};
const HANDLE: Record<Corner, { pos: string; cursor: string; rot: number }> = {
  "top-left": { pos: "left-0 top-0", cursor: "cursor-nwse-resize", rot: 180 },
  "top-right": { pos: "right-0 top-0", cursor: "cursor-nesw-resize", rot: 270 },
  "bottom-left": { pos: "bottom-0 left-0", cursor: "cursor-nesw-resize", rot: 90 },
  "bottom-right": { pos: "bottom-0 right-0", cursor: "cursor-nwse-resize", rot: 0 },
};
// Drag-release spring: STIFFNESS pulls toward the corner, RETENTION keeps most
// of the frame's velocity so a flick carries momentum and overshoots a touch
// (the bounce) before settling.
const SPRING_STIFFNESS = 0.02;
const SPRING_RETENTION = 0.85;
// A "throw": if the (smoothed) release SPEED (magnitude across both axes) clears
// this many px per frame, the flick's DIRECTION picks the corner instead of the
// box's position — so a hard fling reaches the corner it was thrown toward even
// if the cursor never crossed the midline. Gentler releases stay position-based.
const FLICK_SPEED = 7;
// Using magnitude (not per-axis) means a 45° diagonal counts even though each
// axis carries only ~70% of the speed. An axis then joins the throw only if it
// carries at least this share of it — so a mostly-sideways fling doesn't also
// flip the near-still vertical axis, but a real diagonal engages both.
const FLICK_AXIS_SHARE = 0.4;
const ENTRANCE_MS = 340;
// Slightly overshooting ease → the mini bounces as it shrinks into place.
const ENTRANCE_EASE = "cubic-bezier(0.34, 1.32, 0.64, 1)";

// The workspace panel the mini docks inside ([data-pip-content-row]): below
// the top bar and inside the chrome gutter on the other three sides. Measured
// from the layout (not hardcoded) so it tracks theme/view changes; falls back
// to the window when the panel isn't mounted. Covering the channel list or the
// members list inside the panel is fine.
function panelBounds(): { top: number; left: number; right: number; bottom: number } {
  const row = document.querySelector("[data-pip-content-row]");
  const r = row?.getBoundingClientRect();
  return r
    ? { top: r.top, left: r.left, right: r.right, bottom: r.bottom }
    : { top: 0, left: 0, right: window.innerWidth, bottom: window.innerHeight };
}

function cornerTopLeft(corner: Corner, w: number, h: number): { x: number; y: number } {
  const isTop = corner.startsWith("top");
  const isLeft = corner.endsWith("left");
  const b = panelBounds();
  // Every corner sits MARGIN inside the panel edge (top corners are therefore
  // below the top bar).
  const x = isLeft ? b.left + MARGIN : b.right - w - MARGIN;
  const y = isTop ? b.top + MARGIN : b.bottom - h - MARGIN;
  // Clamp so a narrow/short window can't push the box off-screen.
  return {
    x: Math.max(MARGIN, Math.min(x, window.innerWidth - w - MARGIN)),
    y: Math.max(MARGIN, Math.min(y, window.innerHeight - h - MARGIN)),
  };
}

/// Floating pop-out stream player (Discord-style picture-in-picture). Shows the
/// focused stream while the user is in any non-voice view. Reuses the single
/// persistent StreamVideoPlayer (reparented in via placeStreamPip), so playback
/// is seamless. Position is driven imperatively (refs) so drag + the release
/// spring stay smooth without re-rendering every frame; React only owns the
/// resting corner (uiStore.pipCorner).
export default function MiniStreamPlayer() {
  const activeView = useUiStore((s) => s.activeView);
  const pipCorner = useUiStore((s) => s.pipCorner);
  const setPipCorner = useUiStore((s) => s.setPipCorner);
  const pipWidth = useUiStore((s) => s.pipWidth);
  const setPipWidth = useUiStore((s) => s.setPipWidth);
  const setActiveView = useUiStore((s) => s.setActiveView);
  const pipHeight = heightFor(pipWidth);

  const ownUsername = useAuthStore((s) => s.username);
  const pipStream = useVoiceStore((s) => s.pipStream);
  const activeStreams = useVoiceStore((s) => s.activeStreams);
  const watchingStreams = useVoiceStore((s) => s.watchingStreams);
  const connectedServerId = useVoiceStore((s) => s.connectedServerId);
  const connectedChannelId = useVoiceStore((s) => s.connectedChannelId);

  const containerRef = useRef<HTMLDivElement>(null);
  const slotRef = useRef<HTMLDivElement>(null);
  const rafRef = useRef<number | null>(null);
  const draggingRef = useRef(false);

  const streamLive =
    pipStream != null &&
    watchingStreams.includes(pipStream) &&
    activeStreams.some((s) => s.ownerUsername === pipStream);
  const visible = streamLive && activeView !== "voice";

  const stopSpring = useCallback(() => {
    if (rafRef.current != null) {
      cancelAnimationFrame(rafRef.current);
      rafRef.current = null;
    }
  }, []);

  // Pin the box at the current resting corner and remember the rect (so the full
  // view can grow back out of it).
  const applyRestingPos = useCallback(() => {
    const el = containerRef.current;
    if (!el) return;
    const { x, y } = cornerTopLeft(pipCorner, pipWidth, pipHeight);
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    recordMiniRect(el);
  }, [pipCorner, pipWidth, pipHeight]);

  // Reparent the shared video node into this slot when showing.
  useLayoutEffect(() => {
    if (visible && slotRef.current) placeStreamPip(slotRef.current);
  }, [visible, pipStream]);

  // Rest at the corner when a drag/spring isn't positioning it. Re-runs on view
  // change too, since the left-sidebar width (and thus the top-corner inset) can
  // differ between views (e.g. browse drops the channel sidebar).
  useLayoutEffect(() => {
    if (!draggingRef.current && rafRef.current == null) applyRestingPos();
  }, [pipCorner, visible, activeView, applyRestingPos]);

  // Keep the resting corner correct across window resizes and content-row
  // geometry changes (e.g. the reconnecting banner appearing shifts the top
  // inset), unless a drag/spring is currently positioning it.
  useEffect(() => {
    const reposition = () => {
      if (!draggingRef.current && rafRef.current == null) applyRestingPos();
    };
    window.addEventListener("resize", reposition);
    const row = document.querySelector("[data-pip-content-row]");
    let ro: ResizeObserver | null = null;
    if (row) {
      ro = new ResizeObserver(reposition);
      ro.observe(row);
    }
    return () => {
      window.removeEventListener("resize", reposition);
      ro?.disconnect();
    };
  }, [applyRestingPos]);

  // Entrance: shrink from where the full-view player was into the corner.
  useLayoutEffect(() => {
    if (!visible) return;
    const el = containerRef.current;
    const from = getFullViewRect();
    if (!el || !from || from.width < 1) return;
    const { x, y } = cornerTopLeft(pipCorner, pipWidth, pipHeight);
    const dx = from.left - x;
    const dy = from.top - y;
    const sx = from.width / pipWidth;
    const sy = from.height / pipHeight;
    if (Math.abs(dx) < 2 && Math.abs(dy) < 2 && Math.abs(sx - 1) < 0.02) return;
    el.style.transformOrigin = "top left";
    el.style.transition = "none";
    el.style.transform = `translate(${dx}px, ${dy}px) scale(${sx}, ${sy})`;
    void el.offsetWidth; // commit the pre-animation transform
    const raf = requestAnimationFrame(() => {
      el.style.transition = `transform ${ENTRANCE_MS}ms ${ENTRANCE_EASE}`;
      el.style.transform = "translate(0px, 0px) scale(1, 1)";
    });
    const done = setTimeout(() => {
      el.style.transition = "";
      el.style.transform = "";
    }, ENTRANCE_MS + 80);
    return () => {
      cancelAnimationFrame(raf);
      clearTimeout(done);
    };
  }, [visible]);

  // Cancel any running spring on unmount.
  useEffect(() => () => stopSpring(), [stopSpring]);

  const onPointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      // Buttons handle their own clicks — never start a drag from one.
      if ((e.target as HTMLElement).closest("[data-pip-control]")) return;
      const el = containerRef.current;
      if (!el) return;
      stopSpring();
      // Freeze in place: cancel any entrance transform.
      el.style.transition = "";
      el.style.transform = "";
      const rect = el.getBoundingClientRect();
      const grabX = e.clientX - rect.left;
      const grabY = e.clientY - rect.top;
      const startCX = e.clientX;
      const startCY = e.clientY;
      let x = rect.left;
      let y = rect.top;
      let vx = 0;
      let vy = 0;
      // Lightly smoothed velocity (EMA) used only to detect a throw, so a soft
      // final frame at release doesn't drop a genuine flick below the threshold.
      let flingVX = 0;
      let flingVY = 0;
      let didMove = false;
      draggingRef.current = true;

      const onMove = (ev: PointerEvent) => {
        const nx = ev.clientX - grabX;
        const ny = ev.clientY - grabY;
        vx = nx - x; // per-frame delta = velocity for the release spring
        vy = ny - y;
        flingVX = flingVX * 0.6 + vx * 0.4;
        flingVY = flingVY * 0.6 + vy * 0.4;
        x = nx;
        y = ny;
        if (
          Math.abs(ev.clientX - startCX) > DRAG_THRESHOLD ||
          Math.abs(ev.clientY - startCY) > DRAG_THRESHOLD
        ) {
          didMove = true;
        }
        el.style.left = `${x}px`;
        el.style.top = `${y}px`;
      };

      const onUp = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        draggingRef.current = false;
        if (!didMove) {
          // Click → expand (focus so we land on the video, not the grid).
          const s = useVoiceStore.getState();
          if (s.pipStream) s.setFullscreenStream(s.pipStream);
          setActiveView("voice");
          return;
        }
        // Snap to the nearest corner, springing from the release point with the
        // flick's momentum so it glides and bounces into place.
        // A real throw (total speed over threshold) picks the corner in the
        // fling's direction; each axis joins only if it carries a real share of
        // the motion. Otherwise fall back to whichever half the center sits in.
        const cx = x + pipWidth / 2;
        const cy = y + pipHeight / 2;
        const speed = Math.hypot(flingVX, flingVY);
        const throwing = speed > FLICK_SPEED;
        const horiz =
          throwing && Math.abs(flingVX) > speed * FLICK_AXIS_SHARE
            ? flingVX > 0
              ? "right"
              : "left"
            : cx > window.innerWidth / 2
              ? "right"
              : "left";
        const vert =
          throwing && Math.abs(flingVY) > speed * FLICK_AXIS_SHARE
            ? flingVY > 0
              ? "bottom"
              : "top"
            : cy > window.innerHeight / 2
              ? "bottom"
              : "top";
        const targetCorner = `${vert}-${horiz}` as Corner;
        const target = cornerTopLeft(targetCorner, pipWidth, pipHeight);

        const tick = () => {
          vx += (target.x - x) * SPRING_STIFFNESS;
          vx *= SPRING_RETENTION;
          x += vx;
          vy += (target.y - y) * SPRING_STIFFNESS;
          vy *= SPRING_RETENTION;
          y += vy;
          el.style.left = `${x}px`;
          el.style.top = `${y}px`;
          if (
            Math.abs(target.x - x) < 0.4 &&
            Math.abs(vx) < 0.4 &&
            Math.abs(target.y - y) < 0.4 &&
            Math.abs(vy) < 0.4
          ) {
            el.style.left = `${target.x}px`;
            el.style.top = `${target.y}px`;
            rafRef.current = null;
            recordMiniRect(el);
            setPipCorner(targetCorner); // commit; resting effect keeps it here
            return;
          }
          rafRef.current = requestAnimationFrame(tick);
        };
        rafRef.current = requestAnimationFrame(tick);
      };

      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    },
    [pipWidth, pipHeight, setActiveView, setPipCorner, stopSpring],
  );

  // Resize by dragging the interior-corner grip. The docked corner stays pinned
  // to its screen anchor; we translate the pointer's distance from that anchor
  // into a new width (16:9-locked), clamped to [MIN_WIDTH, MAX_WIDTH] and to the
  // room available before the box would run off-screen. We only push pipWidth to
  // the store — the resting layout effect re-pins position for the new size, so
  // the docked corner doesn't move.
  const onResizePointerDown = useCallback(
    (e: React.PointerEvent<HTMLDivElement>) => {
      e.stopPropagation();
      e.preventDefault();
      const el = containerRef.current;
      if (!el) return;
      stopSpring();
      // Cancel any entrance transform so geometry reads the true resting rect.
      el.style.transition = "";
      el.style.transform = "";

      const b = panelBounds();
      const isRight = pipCorner.endsWith("right");
      const isBottom = pipCorner.startsWith("bottom");
      const anchorX = isRight ? b.right - MARGIN : b.left + MARGIN;
      const anchorY = isBottom ? b.bottom - MARGIN : b.top + MARGIN;
      const availW = isRight
        ? anchorX - (b.left + MARGIN)
        : b.right - MARGIN - anchorX;
      const availH = isBottom
        ? anchorY - (b.top + MARGIN)
        : b.bottom - MARGIN - anchorY;
      const hi = Math.max(
        MIN_WIDTH,
        Math.min(MAX_WIDTH, availW, availH / ASPECT),
      );

      const onMove = (ev: PointerEvent) => {
        const wFromX = isRight ? anchorX - ev.clientX : ev.clientX - anchorX;
        const wFromY = (isBottom ? anchorY - ev.clientY : ev.clientY - anchorY) / ASPECT;
        const w = Math.max(wFromX, wFromY);
        setPipWidth(Math.round(Math.min(hi, Math.max(MIN_WIDTH, w))));
      };
      const onUp = () => {
        window.removeEventListener("pointermove", onMove);
        window.removeEventListener("pointerup", onUp);
        const el2 = containerRef.current;
        if (el2) recordMiniRect(el2);
      };
      window.addEventListener("pointermove", onMove);
      window.addEventListener("pointerup", onUp);
    },
    [pipCorner, setPipWidth, stopSpring],
  );

  if (!visible || !pipStream) return null;

  // Grip sits on the corner facing the screen interior (opposite the dock).
  const grip = HANDLE[OPPOSITE_CORNER[pipCorner]];

  const handleExpand = (e: React.MouseEvent) => {
    e.stopPropagation();
    useVoiceStore.getState().setFullscreenStream(pipStream);
    setActiveView("voice");
  };

  const handleClose = async (e: React.MouseEvent) => {
    e.stopPropagation();
    const user = pipStream;
    if (user !== ownUsername && connectedServerId && connectedChannelId) {
      await invoke("stop_watching", {
        serverId: connectedServerId,
        channelId: connectedChannelId,
        targetUsername: user,
      }).catch(() => {});
    }
    // removeWatching also clears pipStream/fullscreenStream when they match,
    // which unmounts the persistent player.
    useVoiceStore.getState().removeWatching(user);
  };

  return (
    <div
      ref={containerRef}
      onPointerDown={onPointerDown}
      className="group/pip fixed z-40 cursor-grab select-none overflow-hidden rounded-lg border border-white/10 bg-black shadow-modal active:cursor-grabbing"
      style={{ width: pipWidth, height: pipHeight, touchAction: "none" }}
      title="Drag to move · click to return to the stream"
    >
      {/* The shared persistent stream player is reparented in here (it is
          pointer-events:none, so drags/clicks fall through to this box). */}
      <div ref={slotRef} className="absolute inset-0 z-0" />

      {/* Top control bar. The bar is click-through (so dragging works from the
          top edge); only the buttons capture clicks. */}
      <div className="pointer-events-none absolute inset-x-0 top-0 z-10 flex items-center justify-between gap-2 bg-gradient-to-b from-black/70 to-transparent px-2 py-1.5">
        <span className="truncate text-[11px] font-medium text-white/90">
          {pipStream}
        </span>
        <div
          className="pointer-events-auto flex items-center gap-1"
          // Never let a button press start a window drag.
          onPointerDown={(e) => e.stopPropagation()}
        >
          <button
            data-pip-control
            onClick={handleExpand}
            title="Return to stream"
            className="rounded bg-black/40 p-1.5 text-white/90 transition-colors hover:bg-white/20 hover:text-white"
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M15 3h6v6M9 21H3v-6M21 3l-7 7M3 21l7-7" />
            </svg>
          </button>
          <button
            data-pip-control
            onClick={handleClose}
            title="Stop watching"
            className="rounded bg-black/40 p-1.5 text-white/90 transition-colors hover:bg-error hover:text-white"
          >
            <svg width="15" height="15" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round">
              <path d="M18 6L6 18M6 6l12 12" />
            </svg>
          </button>
        </div>
      </div>

      {/* LIVE tag — always visible bottom-left */}
      <div className="pointer-events-none absolute bottom-1.5 left-1.5 z-10 flex items-center gap-1 rounded-sm bg-black/55 px-1.5 py-0.5 text-[9px] font-semibold uppercase tracking-wide text-red-400">
        <span className="h-1.5 w-1.5 rounded-full bg-red-500" />
        Live
      </div>

      {/* Resize grip — on the interior-facing corner. Faint until hovered. */}
      <div
        data-pip-control
        onPointerDown={onResizePointerDown}
        title="Drag to resize"
        style={{ touchAction: "none" }}
        className={`absolute z-20 flex h-5 w-5 items-center justify-center text-white/40 opacity-60 transition-opacity hover:text-white group-hover/pip:opacity-100 ${grip.pos} ${grip.cursor}`}
      >
        <svg
          width="13"
          height="13"
          viewBox="0 0 16 16"
          fill="none"
          stroke="currentColor"
          strokeWidth="1.6"
          strokeLinecap="round"
          style={{ transform: `rotate(${grip.rot}deg)` }}
        >
          <path d="M14 6 L6 14 M14 11 L11 14" />
        </svg>
      </div>
    </div>
  );
}
