import { useCallback, useEffect, useLayoutEffect, useMemo, useRef, useState } from "react";
import { createPortal } from "react-dom";
import {
  useImageViewerStore,
  viewerThumbFor,
  type ViewerImage,
  type ViewerOrigin,
} from "../../stores/imageViewerStore";
import { useImageContextMenuStore } from "../../stores/imageContextMenuStore";
import { thumbHashToDataUrl } from "./thumbhash";
import {
  clampScale,
  clampView,
  fitArea,
  fitScale,
  MAX_SCALE,
  overflows,
  shown,
  stepScale,
  zoomAt,
  type Area,
  type Size,
  type View,
} from "./imageViewer/geometry";
import { CARD, Filmstrip, Minimap, Tool, ToolDivider, ViewerHeader } from "./imageViewer/parts";
import {
  ChevronIcon,
  CloseIcon,
  CopyIcon,
  DownloadIcon,
  ExternalIcon,
  RotateIcon,
  ZoomInIcon,
  ZoomOutIcon,
} from "./imageViewer/icons";
import { canSave, copyImage, openInBrowser, saveImage } from "./imageViewer/imageActions";

/// Fullscreen viewer for images: attachments (with the gallery of every
/// image around them) and link images. It opens out of the thumbnail and
/// closes back into it; the stage is the theme's chrome colour washed with
/// the picture's own colours; the chrome hides after a moment of stillness.
export default function ImageViewer() {
  const items = useImageViewerStore((s) => s.items);
  const index = useImageViewerStore((s) => s.index);
  const origin = useImageViewerStore((s) => s.origin);
  const open = items.length > 0;

  // The last open session, kept while it animates out (the store is
  // already empty by then). A new open mid-exit starts a fresh session.
  const [session, setSession] = useState<{ id: number; items: ViewerImage[]; index: number; origin: ViewerOrigin | null } | null>(null);
  const seq = useRef(0);
  const wasOpen = useRef(false);
  useLayoutEffect(() => {
    if (open && !wasOpen.current) {
      seq.current += 1;
      setSession({ id: seq.current, items, index, origin });
    } else if (open) {
      setSession((s) => (s ? { ...s, items, index } : s));
    }
    wasOpen.current = open;
  }, [open, items, index, origin]);

  if (!session) return null;
  return createPortal(
    <Session
      key={session.id}
      items={session.items}
      index={session.index}
      origin={session.origin}
      closing={!open}
      onClosed={() => setSession((s) => (s && s.id === session.id ? null : s))}
    />,
    document.body,
  );
}

const EASE = "cubic-bezier(0.2, 0.8, 0.2, 1)";
const FLY_MS = 300;
const IDLE_MS = 2500;
const TITLEBAR_PX = 32;

type Phase = "enter" | "open" | "exit";
type LiveView = View & { anim: number };

function naturalOf(item: ViewerImage, measured: Size | undefined, origin: ViewerOrigin | null): Size {
  if (item.width > 0 && item.height > 0) return { w: item.width, h: item.height };
  if (measured) return measured;
  if (origin) return { w: origin.rect.width, h: origin.rect.height };
  return { w: 640, h: 480 };
}

/// The thumbnail's box as a transform of the picture's fitted box (and the
/// crop that makes a grid cell's object-cover), for flying between them.
function flyFrom(
  target: { left: number; top: number; width: number; height: number },
  fit: "cover" | "contain",
  box: { left: number; top: number; w: number; h: number },
  radius: number,
) {
  const s = fit === "cover"
    ? Math.max(target.width / box.w, target.height / box.h)
    : Math.min(target.width / box.w, target.height / box.h);
  const dx = target.left + target.width / 2 - (box.left + box.w / 2);
  const dy = target.top + target.height / 2 - (box.top + box.h / 2);
  const ix = fit === "cover" ? Math.max(0, (box.w - target.width / s) / 2) : 0;
  const iy = fit === "cover" ? Math.max(0, (box.h - target.height / s) / 2) : 0;
  return {
    transform: `translate(${dx}px, ${dy}px) scale(${s})`,
    clipPath: `inset(${iy}px ${ix}px round ${radius / s}px)`,
  };
}

function Session({
  items,
  index,
  origin,
  closing,
  onClosed,
}: {
  items: ViewerImage[];
  index: number;
  origin: ViewerOrigin | null;
  closing: boolean;
  onClosed: () => void;
}) {
  const close = useImageViewerStore((s) => s.close);
  const go = useImageViewerStore((s) => s.go);
  const item = items[Math.min(index, items.length - 1)];

  const rootRef = useRef<HTMLDivElement>(null);
  const stageRef = useRef<HTMLDivElement>(null);
  const flyRef = useRef<HTMLDivElement>(null);

  const [stage, setStage] = useState<Size>(() => ({ w: window.innerWidth, h: window.innerHeight - TITLEBAR_PX }));
  useLayoutEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const ro = new ResizeObserver(() => setStage({ w: el.clientWidth, h: el.clientHeight }));
    ro.observe(el);
    setStage({ w: el.clientWidth, h: el.clientHeight });
    return () => ro.disconnect();
  }, []);

  const strip = items.length > 1;
  const area: Area = useMemo(() => fitArea(stage, strip), [stage, strip]);
  const [measured, setMeasured] = useState<Record<string, Size>>({});
  const nat = naturalOf(item, measured[item.key], origin);
  // The picture's layout box is its unrotated fit; zoom and turns are
  // transforms on top, so neither ever relayouts (or re-rasters mid-way).
  const base = fitScale(nat, 0, area);
  const box = {
    w: nat.w * base,
    h: nat.h * base,
    left: area.left + area.w / 2 - (nat.w * base) / 2,
    top: area.top + area.h / 2 - (nat.h * base) / 2,
  };

  const [view, setView] = useState<LiveView>(() => ({ scale: base, x: 0, y: 0, rot: 0, anim: 0 }));
  const fit = fitScale(nat, view.rot, area);
  const atFit = Math.abs(view.scale - fit) < 1e-4;

  // A new picture starts at fit; a resize keeps fit at fit and re-clamps
  // anything zoomed.
  const shownKey = useRef(item.key);
  const lastFit = useRef(fit);
  useLayoutEffect(() => {
    if (shownKey.current !== item.key) {
      shownKey.current = item.key;
      setView({ scale: fitScale(nat, 0, area), x: 0, y: 0, rot: 0, anim: 0 });
    } else {
      setView((v) => {
        const wasFit = Math.abs(v.scale - lastFit.current) < 1e-4;
        const next = wasFit ? { ...v, scale: fit, x: 0, y: 0 } : v;
        return { ...clampView(next, nat, stage, area), anim: 0 };
      });
    }
    lastFit.current = fit;
    // nat/area/stage are inputs of fit; the key and fit are what change.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.key, fit, stage.w, stage.h]);

  const apply = useCallback(
    (next: View, anim: number) => {
      const s = clampScale(next.scale, nat, next.rot, area);
      setView({ ...clampView({ ...next, scale: s }, nat, stage, area), anim });
    },
    [nat.w, nat.h, area, stage], // eslint-disable-line react-hooks/exhaustive-deps
  );
  /// Zoom toward a point, by the zoom that can actually happen.
  const zoomToward = (v: View, scale: number, p: { x: number; y: number }, anim: number) => {
    const s = clampScale(scale, nat, v.rot, area);
    if (Math.abs(s - v.scale) < 1e-6) return;
    apply(zoomAt(v, s, p), anim);
  };

  const centre = { x: area.left + area.w / 2, y: area.top + area.h / 2 };
  const toArea = (clientX: number, clientY: number) => {
    const r = stageRef.current?.getBoundingClientRect();
    return { x: clientX - (r?.left ?? 0) - centre.x, y: clientY - (r?.top ?? 0) - centre.y };
  };

  // ── open / close ───────────────────────────────────────────────
  const [phase, setPhase] = useState<Phase>("enter");
  // Reduced motion: no flights, the picture and the stage only fade.
  const still = useRef(window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false).current;
  // The thumbnail hides while the picture flies out of it or back into it,
  // and always comes back, even if the session ends mid-flight.
  const hidden = useRef(new Set<HTMLElement>());
  const hideThumb = (key: string) => {
    const el = viewerThumbFor(key);
    if (!el) return () => {};
    el.style.opacity = "0";
    hidden.current.add(el);
    return () => {
      el.style.opacity = "";
      hidden.current.delete(el);
    };
  };
  useEffect(
    () => () => {
      for (const el of hidden.current) el.style.opacity = "";
    },
    [],
  );

  useLayoutEffect(() => {
    const fly = flyRef.current;
    const stageEl = stageRef.current;
    const raf = requestAnimationFrame(() => setPhase("open"));
    if (!fly || !stageEl || !origin || still) return () => cancelAnimationFrame(raf);
    const sr = stageEl.getBoundingClientRect();
    const thumb = viewerThumbFor(item.key);
    const radius = thumb ? parseFloat(getComputedStyle(thumb).borderTopLeftRadius) || 0 : 0;
    const from = flyFrom(origin.rect, origin.fit, { ...box, left: sr.left + box.left, top: sr.top + box.top }, radius);
    const restore = hideThumb(item.key);
    const anim = fly.animate([from, { transform: "none", clipPath: "inset(0px 0px round 0px)" }], {
      duration: FLY_MS,
      easing: EASE,
    });
    anim.onfinish = anim.oncancel = restore;
    return () => cancelAnimationFrame(raf);
    // Once, at mount: the open flight.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, []);

  useEffect(() => {
    if (!closing) return;
    setPhase("exit");
    const fly = flyRef.current;
    const stageEl = stageRef.current;
    if (!fly || !stageEl) {
      onClosed();
      return;
    }
    const el = viewerThumbFor(item.key);
    const r = el?.getBoundingClientRect();
    const visible =
      r && r.width > 0 && r.bottom > TITLEBAR_PX && r.top < window.innerHeight && r.right > 0 && r.left < window.innerWidth;
    let anim: Animation;
    if (visible && el && r && !still) {
      // The picture unwinds (zoom, pan, turns) while it flies home.
      setView((v) => ({ scale: base, x: 0, y: 0, rot: Math.round(v.rot / 360) * 360, anim: FLY_MS }));
      const sr = stageEl.getBoundingClientRect();
      const fitMode = getComputedStyle(el.querySelector("img") ?? el).objectFit === "cover" ? "cover" : "contain";
      const radius = parseFloat(getComputedStyle(el).borderTopLeftRadius) || 0;
      const to = flyFrom(r, fitMode, { ...box, left: sr.left + box.left, top: sr.top + box.top }, radius);
      const restore = hideThumb(item.key);
      anim = fly.animate([{ transform: "none", clipPath: "inset(0px 0px round 0px)" }, to], {
        duration: FLY_MS,
        easing: EASE,
        fill: "forwards",
      });
      anim.onfinish = () => {
        restore();
        onClosed();
      };
    } else {
      anim = fly.animate(still ? [{ opacity: 1 }, { opacity: 0 }] : [{ opacity: 1, transform: "none" }, { opacity: 0, transform: "scale(0.96)" }], {
        duration: 200,
        easing: EASE,
        fill: "forwards",
      });
      anim.onfinish = onClosed;
    }
    // Runs once, when the session starts closing.
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [closing]);

  // ── loading ────────────────────────────────────────────────────
  const [ready, setReady] = useState<Record<string, boolean>>({});
  const [failed, setFailed] = useState<Record<string, boolean>>({});
  const [retry, setRetry] = useState(0);
  const [slow, setSlow] = useState(false);
  const isReady = !!ready[item.key];
  useEffect(() => {
    setSlow(false);
    if (isReady) return;
    const t = window.setTimeout(() => setSlow(true), 400);
    return () => window.clearTimeout(t);
  }, [item.key, isReady, retry]);

  // The neighbours load (and decode) ahead, so stepping through is instant.
  const warm = useRef<HTMLImageElement[]>([]);
  useEffect(() => {
    const next: HTMLImageElement[] = [];
    for (const i of [index + 1, index - 1]) {
      const it = items[i];
      if (!it) continue;
      const im = new Image();
      im.src = it.url;
      im.decode().catch(() => {});
      next.push(im);
    }
    warm.current = next;
  }, [index, items]);

  // ── chrome ─────────────────────────────────────────────────────
  const [idle, setIdle] = useState(false);
  const idleTimer = useRef<number | null>(null);
  const holding = useRef(false);
  const wake = useCallback(() => {
    setIdle(false);
    if (idleTimer.current !== null) window.clearTimeout(idleTimer.current);
    idleTimer.current = window.setTimeout(() => {
      if (!holding.current) setIdle(true);
    }, IDLE_MS);
  }, []);
  useEffect(() => {
    wake();
    return () => {
      if (idleTimer.current !== null) window.clearTimeout(idleTimer.current);
    };
  }, [wake]);
  const hold = {
    onPointerEnter: () => {
      holding.current = true;
    },
    onPointerLeave: () => {
      holding.current = false;
      wake();
    },
  };
  const chromeOn = phase === "open" && !idle;

  // ── actions ────────────────────────────────────────────────────
  const zoomTo = (scale: number, p = { x: 0, y: 0 }) => zoomToward(view, scale, p, 220);
  const toggleZoom = (p = { x: 0, y: 0 }) => {
    if (!atFit) apply({ ...view, scale: fit, x: 0, y: 0 }, 260);
    // A big picture goes to its actual pixels; one already at (or near)
    // actual size doubles.
    else zoomTo(fit < 0.9 ? 1 : Math.min(MAX_SCALE, fit * 2), p);
  };
  const rotate = () => apply({ ...view, rot: view.rot + 90, scale: fitScale(nat, view.rot + 90, area), x: 0, y: 0 }, 260);
  const step = (d: 1 | -1) => go(index + d);

  // ── keys ───────────────────────────────────────────────────────
  // The viewer holds focus while open and hands it back (to the thumbnail
  // that opened it, usually) when it goes.
  useEffect(() => {
    const before = document.activeElement as HTMLElement | null;
    rootRef.current?.focus({ preventScroll: true });
    return () => {
      if (before?.isConnected) before.focus({ preventScroll: true });
    };
  }, []);
  useEffect(() => {
    if (closing) return;
    const onKey = (e: KeyboardEvent) => {
      // A right-click menu over the picture closes on its own Esc; no
      // other key reaches anything while it's up.
      if (useImageContextMenuStore.getState().open) {
        if (e.key !== "Escape") e.stopPropagation();
        return;
      }
      // Modal: nothing behind (type-to-focus, list shortcuts) sees a key.
      e.stopPropagation();
      const mod = e.ctrlKey || e.metaKey;
      wake();
      if (e.key === "Escape") close();
      else if (e.key === "ArrowLeft") step(-1);
      else if (e.key === "ArrowRight") step(1);
      else if (e.key === "Home") go(0);
      else if (e.key === "End") go(items.length - 1);
      else if (e.key === "+" || e.key === "=") zoomTo(stepScale(view.scale, fit, 1));
      else if (e.key === "-") zoomTo(stepScale(view.scale, fit, -1));
      else if (e.key === "0") apply({ ...view, scale: fit, x: 0, y: 0 }, 220);
      else if (e.key === "1") zoomTo(1);
      else if (e.key === "r" || e.key === "R") rotate();
      else if (mod && e.key === "c") void copyImage(item);
      else if (mod && e.key === "s" && canSave(item)) saveImage(item);
      else return;
      e.preventDefault();
    };
    window.addEventListener("keydown", onKey, true);
    return () => window.removeEventListener("keydown", onKey, true);
  });

  // ── wheel: zoom toward the pointer ─────────────────────────────
  const viewRef = useRef(view);
  viewRef.current = view;
  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      wake();
      const dy = e.deltaMode === 1 ? e.deltaY * 16 : e.deltaMode === 2 ? e.deltaY * 400 : e.deltaY;
      // Pinch (ctrl + small deltas) gets more gain than a wheel notch.
      const k = Math.exp(-dy * (e.ctrlKey ? 0.01 : 0.0022));
      const v = viewRef.current;
      const r = el.getBoundingClientRect();
      const p = { x: e.clientX - r.left - centre.x, y: e.clientY - r.top - centre.y };
      // A wheel notch eases; a trackpad's stream of small deltas tracks.
      zoomToward(v, v.scale * k, p, Math.abs(dy) >= 40 ? 120 : 0);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  });

  // ── pointer: click to zoom, drag to pan, click outside to close ─
  const drag = useRef<{
    id: number;
    sx: number;
    sy: number;
    vx: number;
    vy: number;
    onPicture: boolean;
    moved: boolean;
    samples: { t: number; x: number; y: number }[];
  } | null>(null);
  const glide = useRef<number | null>(null);
  const stopGlide = () => {
    if (glide.current !== null) cancelAnimationFrame(glide.current);
    glide.current = null;
  };

  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0 || closing) return;
    stopGlide();
    const onPicture = (e.target as HTMLElement).closest("[data-viewer-picture]") !== null;
    drag.current = {
      id: e.pointerId,
      sx: e.clientX,
      sy: e.clientY,
      vx: view.x,
      vy: view.y,
      onPicture,
      moved: false,
      samples: [{ t: performance.now(), x: e.clientX, y: e.clientY }],
    };
    e.currentTarget.setPointerCapture(e.pointerId);
  };
  const onPointerMove = (e: React.PointerEvent) => {
    wake();
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    const dx = e.clientX - d.sx;
    const dy = e.clientY - d.sy;
    if (!d.moved && Math.hypot(dx, dy) < 4) return;
    d.moved = true;
    d.samples.push({ t: performance.now(), x: e.clientX, y: e.clientY });
    if (d.samples.length > 6) d.samples.shift();
    if (overflows(view, nat, stage)) apply({ ...view, x: d.vx + dx, y: d.vy + dy }, 0);
  };
  const onPointerUp = (e: React.PointerEvent) => {
    const d = drag.current;
    drag.current = null;
    if (!d || d.id !== e.pointerId) return;
    if (!d.moved) {
      if (d.onPicture) toggleZoom(toArea(e.clientX, e.clientY));
      else close();
      return;
    }
    // Let a flick glide to a stop.
    const a = d.samples[0];
    const b = d.samples[d.samples.length - 1];
    const dt = b.t - a.t;
    if (dt <= 0 || performance.now() - b.t > 60 || !overflows(view, nat, stage)) return;
    let vx = (b.x - a.x) / dt;
    let vy = (b.y - a.y) / dt;
    let last = performance.now();
    let cur: View = { ...viewRef.current };
    const tick = (now: number) => {
      const step = now - last;
      last = now;
      cur = clampView({ ...cur, x: cur.x + vx * step, y: cur.y + vy * step }, nat, stage, area);
      setView({ ...cur, anim: 0 });
      const decay = Math.exp(-step / 160);
      vx *= decay;
      vy *= decay;
      glide.current = Math.hypot(vx, vy) > 0.02 ? requestAnimationFrame(tick) : null;
    };
    glide.current = requestAnimationFrame(tick);
  };
  useEffect(() => stopGlide, []);

  const onContextMenu = (e: React.MouseEvent) => {
    e.preventDefault();
    useImageContextMenuStore.getState().show({
      x: e.clientX,
      y: e.clientY,
      serverId: item.serverId,
      attachmentId: item.attachmentId,
      filename: item.filename,
      mime: item.mime,
      kind: "image",
      url: item.url,
      pageUrl: item.pageUrl,
    });
  };

  // ── ambient wash: the picture's colours, cross-faded per picture ─
  const ambientSrc = (it: ViewerImage) =>
    (it.placeholder ? thumbHashToDataUrl(it.placeholder) : null) ?? it.thumbUrl ?? it.previewUrl ?? it.url;
  const [washes, setWashes] = useState<{ key: string; src: string }[]>(() => [{ key: item.key, src: ambientSrc(item) }]);
  useEffect(() => {
    setWashes((w) => (w[w.length - 1]?.key === item.key ? w : [...w.slice(-1), { key: item.key, src: ambientSrc(item) }]));
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [item.key]);

  // Slide direction for a step through the gallery, kept for as long as
  // that picture shows: dropping the class on the next render (the full
  // size landing, say) would cut the slide short.
  const stepIn = useRef({ index, key: item.key, dir: 0 });
  if (stepIn.current.index !== index) {
    stepIn.current = { index, key: item.key, dir: Math.sign(index - stepIn.current.index) };
  }
  const dir = stepIn.current.key === item.key ? stepIn.current.dir : 0;

  // ── minimap ────────────────────────────────────────────────────
  const disp = shown(nat, view.rot);
  const dw = disp.w * view.scale;
  const dh = disp.h * view.scale;
  const left = centre.x + view.x - dw / 2;
  const top = centre.y + view.y - dh / 2;
  const pannable = overflows(view, nat, stage);
  const visibleFrac = {
    u0: Math.max(0, -left / dw),
    v0: Math.max(0, -top / dh),
    u1: Math.min(1, (stage.w - left) / dw),
    v1: Math.min(1, (stage.h - top) / dh),
  };
  const moveTo = (u: number, v: number) => {
    const l = stage.w / 2 - u * dw;
    const t = stage.h / 2 - v * dh;
    apply({ ...view, x: l + dw / 2 - centre.x, y: t + dh / 2 - centre.y }, 0);
  };

  const preview = item.previewUrl ?? item.url;
  const hasFull = item.url !== preview;
  const fade = `opacity 200ms ${EASE}`;
  // Chrome wrappers never take the pointer (the picture runs under them);
  // their controls do, while the chrome is up.
  const chrome = (extra = "") =>
    `pointer-events-none transition-opacity duration-200 ${chromeOn ? "opacity-100" : "opacity-0"} ${extra}`;
  const live = chromeOn ? "pointer-events-auto" : "pointer-events-none";

  return (
    <div
      ref={rootRef}
      role="dialog"
      aria-modal
      aria-label={`Image viewer: ${item.filename}`}
      tabIndex={-1}
      // z-[55]: over the app and its settings windows (z-50), under what
      // must reach you while you look: an incoming call, toasts, menus,
      // the Transfers panel.
      className="image-viewer chrome-scope fixed inset-x-0 bottom-0 z-[55] select-none overflow-hidden outline-none"
      style={{
        top: TITLEBAR_PX,
        cursor: idle && phase === "open" ? "none" : undefined,
        // Closing, it lets clicks through to the chat (another image can
        // open at once).
        pointerEvents: phase === "exit" ? "none" : undefined,
      }}
      onPointerMove={wake}
    >
      {/* Stage: the theme's chrome colour, the picture's wash, a vignette. */}
      <div
        aria-hidden
        className="absolute inset-0 bg-[var(--viewer-stage)]"
        style={{ opacity: phase === "open" ? 1 : 0, transition: `opacity ${phase === "exit" ? 220 : FLY_MS}ms ${EASE}` }}
      >
        {washes.map((w) => (
          <div
            key={w.key}
            className="viewer-wash absolute -inset-[10%] bg-cover bg-center"
            style={{ backgroundImage: `url(${w.src})` }}
          />
        ))}
        <div className="viewer-vignette absolute inset-0" />
      </div>

      <div
        ref={stageRef}
        className="absolute inset-0"
        style={{ cursor: atFit ? undefined : drag.current?.moved ? "grabbing" : "grab" }}
        onPointerDown={onPointerDown}
        onPointerMove={onPointerMove}
        onPointerUp={onPointerUp}
        onPointerCancel={() => (drag.current = null)}
      >
        <div
          ref={flyRef}
          className="absolute"
          style={{ left: box.left, top: box.top, width: box.w, height: box.h }}
        >
          <div
            key={item.key}
            data-viewer-picture
            className={`shadow-modal ${dir !== 0 ? "viewer-step-in" : ""}`}
            onContextMenu={onContextMenu}
            style={{
              position: "absolute",
              inset: 0,
              transform: `translate(${view.x}px, ${view.y}px) rotate(${view.rot}deg) scale(${view.scale / base})`,
              transition: view.anim ? `transform ${view.anim}ms ${EASE}` : "none",
              cursor: atFit ? "zoom-in" : undefined,
              ["--viewer-dir" as string]: dir,
              // Pixels, not smears, past 300 %: screenshots and pixel art.
              imageRendering: view.scale >= 3 ? "pixelated" : undefined,
            }}
          >
            <img
              src={preview}
              alt=""
              draggable={false}
              className="absolute inset-0 h-full w-full"
              onLoad={(e) => {
                const el = e.currentTarget;
                if (!(item.width > 0) && !measured[item.key])
                  setMeasured((m) => ({ ...m, [item.key]: { w: el.naturalWidth, h: el.naturalHeight } }));
                if (!hasFull) setReady((r) => ({ ...r, [item.key]: true }));
              }}
            />
            {hasFull && (
              <img
                key={retry}
                src={item.url}
                alt={item.filename}
                draggable={false}
                decoding="async"
                className="absolute inset-0 h-full w-full"
                style={{ opacity: isReady ? 1 : 0, transition: fade }}
                onLoad={(e) => {
                  const k = item.key;
                  e.currentTarget
                    .decode()
                    .catch(() => {})
                    .then(() => setReady((r) => ({ ...r, [k]: true })));
                }}
                onError={() => setFailed((f) => ({ ...f, [item.key]: true }))}
              />
            )}
          </div>
        </div>
      </div>

      {/* Header: who, when, where, what — then the tools and close. */}
      <div
        className={chrome("viewer-head absolute inset-x-0 top-0 flex items-start gap-3 px-4 pb-6 pt-3")}
      >
        <div className={`min-w-0 flex-1 pt-0.5`}>
          <ViewerHeader
            item={item}
            loading={slow && !isReady}
            failed={!!failed[item.key]}
            onRetry={() => {
              setFailed((f) => ({ ...f, [item.key]: false }));
              setRetry((n) => n + 1);
            }}
          />
        </div>
        <div className={`flex items-center gap-0.5 rounded-lg p-1 ${CARD} ${live}`} {...hold}>
          <Tool title="Zoom out (−)" onClick={() => zoomTo(stepScale(view.scale, fit, -1))} disabled={atFit}>
            <ZoomOutIcon />
          </Tool>
          <button
            type="button"
            onClick={() => toggleZoom()}
            title={atFit ? "Actual size (1)" : "Fit to screen (0)"}
            className="h-8 min-w-14 rounded-md px-1.5 font-meta text-[12px] font-medium tabular-nums text-text-secondary hover:bg-surface-hover hover:text-text-primary"
          >
            {Math.round(view.scale * 100)}%
          </button>
          <Tool title="Zoom in (+)" onClick={() => zoomTo(stepScale(view.scale, fit, 1))} disabled={view.scale >= MAX_SCALE}>
            <ZoomInIcon />
          </Tool>
          <ToolDivider />
          <Tool title="Rotate (R)" onClick={rotate}>
            <RotateIcon />
          </Tool>
          <ToolDivider />
          <Tool title="Copy image (Ctrl+C)" onClick={() => void copyImage(item)}>
            <CopyIcon />
          </Tool>
          {canSave(item) ? (
            <Tool title="Save (Ctrl+S)" onClick={() => saveImage(item)}>
              <DownloadIcon />
            </Tool>
          ) : (
            <Tool title="Open in browser" onClick={() => openInBrowser(item)}>
              <ExternalIcon />
            </Tool>
          )}
        </div>
        <button
          type="button"
          onClick={close}
          title="Close (Esc)"
          aria-label="Close"
          className={`flex h-10 w-10 shrink-0 items-center justify-center rounded-lg text-text-secondary hover:text-text-primary ${CARD} ${live}`}
          {...hold}
        >
          <CloseIcon />
        </button>
      </div>

      {/* Gallery: arrows at the sides, the strip at the bottom. */}
      {strip && (
        <>
          {(["left", "right"] as const).map((side) => {
            const d = side === "left" ? -1 : 1;
            const target = index + d;
            if (target < 0 || target >= items.length) return null;
            return (
              <button
                key={side}
                type="button"
                onClick={() => step(d)}
                title={side === "left" ? "Previous (←)" : "Next (→)"}
                aria-label={side === "left" ? "Previous image" : "Next image"}
                className={chrome(
                  `absolute flex h-11 w-11 -translate-y-1/2 items-center justify-center rounded-lg text-text-secondary hover:bg-bg-lighter hover:text-text-primary ${CARD} ${
                    side === "left" ? "left-4" : "right-4"
                  } ${live}`,
                )}
                style={{ top: centre.y }}
                {...hold}
              >
                <ChevronIcon dir={side} />
              </button>
            );
          })}
          <div className={chrome("absolute inset-x-0 bottom-4 flex justify-center px-4")}>
            <div className={live} {...hold}>
              <Filmstrip items={items} index={index} onPick={go} maxWidth={Math.min(760, stage.w - 32)} />
            </div>
          </div>
        </>
      )}

      {pannable && (
        <div className={chrome(`absolute right-4 ${strip ? "bottom-24" : "bottom-4"}`)}>
          <div className={live} {...hold}>
            <Minimap src={item.thumbUrl ?? preview} rot={view.rot} shownSize={disp} view={visibleFrac} onMove={moveTo} />
          </div>
        </div>
      )}
    </div>
  );
}
