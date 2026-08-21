import { useCallback, useEffect, useLayoutEffect, useRef } from "react";
import { invoke } from "../../lib/ipc";
import { useAuthStore } from "../../stores/authStore";
import { useUiStore } from "../../stores/uiStore";
import { useVoiceStore } from "../../stores/voiceStore";
import {
  getFullViewRect,
  placeStreamPip,
  recordMiniRect,
} from "./streamPipHost";

type Corner = "top-left" | "top-right" | "bottom-left" | "bottom-right";

const WIDTH = 320;
const HEIGHT = 180;
const MARGIN = 16;
// How far the pointer must move before a press counts as a drag (vs a click).
const DRAG_THRESHOLD = 4;
// Drag-release spring: STIFFNESS pulls toward the corner, RETENTION keeps most
// of the frame's velocity so a flick carries momentum and overshoots a touch
// (the bounce) before settling.
const SPRING_STIFFNESS = 0.02;
const SPRING_RETENTION = 0.85;
const ENTRANCE_MS = 340;
// Slightly overshooting ease → the mini bounces as it shrinks into place.
const ENTRANCE_EASE = "cubic-bezier(0.34, 1.32, 0.64, 1)";

// Chrome the mini must clear, measured from the layout (not hardcoded) so it
// tracks theme/width/view changes:
//  - top: below the top bar — the content row's top ([data-pip-content-row]).
//  - left: right of the DM rail only ([data-pip-dm-rail]); covering the channel
//    list to its right is fine.
function chromeInsets(): { top: number; left: number } {
  const row = document.querySelector("[data-pip-content-row]");
  const dmRail = document.querySelector("[data-pip-dm-rail]");
  return {
    top: row ? row.getBoundingClientRect().top : MARGIN,
    left: dmRail ? dmRail.getBoundingClientRect().right : MARGIN,
  };
}

function cornerTopLeft(corner: Corner): { x: number; y: number } {
  const isTop = corner.startsWith("top");
  const isLeft = corner.endsWith("left");
  const insets = chromeInsets();
  // Left corners (top AND bottom) clear the DM rail but may cover the channel
  // list. Top corners sit below the top bar. Right corners keep to the window
  // edge (may cover the members list). Bottom edge is unconstrained vertically.
  const x = isLeft ? insets.left + MARGIN : window.innerWidth - WIDTH - MARGIN;
  const y = isTop ? insets.top + MARGIN : window.innerHeight - HEIGHT - MARGIN;
  // Clamp so a narrow/short window can't push the box off-screen.
  return {
    x: Math.max(MARGIN, Math.min(x, window.innerWidth - WIDTH - MARGIN)),
    y: Math.max(MARGIN, Math.min(y, window.innerHeight - HEIGHT - MARGIN)),
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
  const setActiveView = useUiStore((s) => s.setActiveView);

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
    const { x, y } = cornerTopLeft(pipCorner);
    el.style.left = `${x}px`;
    el.style.top = `${y}px`;
    recordMiniRect(el);
  }, [pipCorner]);

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
    const { x, y } = cornerTopLeft(pipCorner);
    const dx = from.left - x;
    const dy = from.top - y;
    const sx = from.width / WIDTH;
    const sy = from.height / HEIGHT;
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
      let didMove = false;
      draggingRef.current = true;

      const onMove = (ev: PointerEvent) => {
        const nx = ev.clientX - grabX;
        const ny = ev.clientY - grabY;
        vx = nx - x; // per-frame delta = velocity for the release spring
        vy = ny - y;
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
        const cx = x + WIDTH / 2;
        const cy = y + HEIGHT / 2;
        const vert = cy > window.innerHeight / 2 ? "bottom" : "top";
        const horiz = cx > window.innerWidth / 2 ? "right" : "left";
        const targetCorner = `${vert}-${horiz}` as Corner;
        const target = cornerTopLeft(targetCorner);

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
    [setActiveView, setPipCorner, stopSpring],
  );

  if (!visible || !pipStream) return null;

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
      className="fixed z-40 cursor-grab select-none overflow-hidden rounded-lg border border-white/10 bg-black shadow-modal active:cursor-grabbing"
      style={{ width: WIDTH, height: HEIGHT, touchAction: "none" }}
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
    </div>
  );
}
