// The picture editor: crop a picked file into a profile picture or a
// server picture (docs/superpowers/specs/2026-10-28-picture-editor-design.md).
//
// The stage shows the whole picture, veiled outside the frame, with the
// frame cut in the shape people will see: the theme's avatar shape, or the
// server tile's corners scaled up. The square's corners stay half lit,
// because other themes show them. The picture is an <img> moved by
// transforms, so the stage is as sharp as the screen at any zoom, and the
// previews beside it are the same image positioned again, live.
//
// Drag past an edge and it gives, then springs back. Wheel or pinch zooms
// toward the pointer, the slider and ± around the middle. With `fit` (server
// pictures, so a square logo can sit whole on the wide tile) zooming out
// goes past "fills the frame" down to the whole picture; the gap takes a
// fill (the picture blurred, or a colour) that is baked into the export.
// Without it (profile pictures, owner decision 2026-10-28) the picture
// always fills the frame, and only a transparent one shows a fill.
// Save renders the crop at the output size (picture.ts), uploads it, and
// flies the result into the element named by `target`.

import {
  createContext,
  useCallback,
  useContext,
  useEffect,
  useLayoutEffect,
  useMemo,
  useRef,
  useState,
  useSyncExternalStore,
  type ReactNode,
} from "react";
import { createPortal } from "react-dom";
import {
  clampView,
  coverScale,
  fromSlider,
  initialView,
  leavesGap,
  maxScale,
  minScale,
  rubberView,
  sameView,
  toSlider,
  zoomAt,
  type Size,
  type View,
} from "./geometry";
import { BLUR_SHARE, PICTURE_ACCEPT, PictureError, pictureFromList, readPicture, renderCrop, type Fill, type Picture } from "./picture";
import { AlertIcon, ImageIcon, ResetIcon, ZoomInIcon, ZoomOutIcon } from "./icons";

export interface PictureEditorProps {
  file: File;
  title: string;
  /// "avatar": a square frame, previews beside the stage. "banner": a wide
  /// frame, previews under it.
  kind: "avatar" | "banner";
  /// Frame width / height (the output's).
  aspect: number;
  /// Zooming out may go past "fills the frame", down to the whole picture.
  fit: boolean;
  /// The frame's corner as CSS, for the frame's on-screen size.
  radius: (frame: Size) => string;
  output: Size;
  /// Live previews, built from <Crop>.
  previews: ReactNode;
  /// Uploads the JPEG; resolves once the server has it, throws its refusal.
  onSave: (jpeg: Uint8Array) => Promise<void>;
  /// The saved picture (a blob URL), as soon as the server has it: show it
  /// in place of the old one while the new one is fetched.
  onSaved: (url: string) => void;
  onClose: () => void;
  /// `data-picture-target` of the element the saved picture flies into.
  target: string;
}

/// The view, outside React: it changes every frame of a drag, and only
/// the stage picture and the previews follow it.
class ViewStore {
  private view: View = { scale: 1, x: 0, y: 0 };
  private subs = new Set<() => void>();
  get = () => this.view;
  set = (v: View) => {
    this.view = v;
    this.subs.forEach((f) => f());
  };
  subscribe = (f: () => void) => {
    this.subs.add(f);
    return () => {
      this.subs.delete(f);
    };
  };
}

interface CropState {
  store: ViewStore;
  picture: Picture | null;
  frame: Size | null;
  fill: Fill;
}

const CropContext = createContext<CropState | null>(null);

function useView(store: ViewStore): View {
  return useSyncExternalStore(store.subscribe, store.get);
}

const STAGE_MARGIN = { avatar: { x: 48, y: 48 }, banner: { x: 40, y: 44 } };
const still = () => window.matchMedia?.("(prefers-reduced-motion: reduce)").matches ?? false;

export function PictureEditor(props: PictureEditorProps) {
  const { kind, aspect, fit, output, title, previews, onSave, onSaved, onClose, target } = props;
  const [file, setFile] = useState(props.file);
  const [picture, setPicture] = useState<Picture | null>(null);
  const [error, setError] = useState<string | null>(null);
  const [frame, setFrame] = useState<Size | null>(null);
  const [origin, setOrigin] = useState({ left: 0, top: 0 });
  const [fill, setFill] = useState<Fill>({ kind: "blur" });
  const [visible, setVisible] = useState(false);
  const [saving, setSaving] = useState(false);
  const [saveError, setSaveError] = useState<string | null>(null);
  const [nudge, setNudge] = useState(0);
  const [interacting, setInteracting] = useState(false);
  const [dragging, setDragging] = useState(false);
  const [dropOver, setDropOver] = useState(false);
  const adjusted = useRef(false);
  const store = useMemo(() => new ViewStore(), []);
  const stageRef = useRef<HTMLDivElement>(null);
  const shapeRef = useRef<HTMLDivElement>(null);
  const inputRef = useRef<HTMLInputElement>(null);
  const closing = useRef(false);

  // Open: double rAF so the starting frame paints (as ConfirmModal).
  useEffect(() => {
    requestAnimationFrame(() => requestAnimationFrame(() => setVisible(true)));
  }, []);

  // Read the file; the previous picture's URL goes when the next one is in.
  useEffect(() => {
    let cancelled = false;
    setError(null);
    readPicture(file).then(
      (p) => {
        if (cancelled) return URL.revokeObjectURL(p.url);
        adjusted.current = false;
        setFill(p.alpha ? { kind: "color", color: p.palette[1] } : { kind: "blur" });
        setPicture((old) => {
          if (old) URL.revokeObjectURL(old.url);
          return p;
        });
      },
      (e) => {
        if (cancelled) return;
        setPicture((old) => {
          if (old) URL.revokeObjectURL(old.url);
          return null;
        });
        setError(e instanceof PictureError ? e.message : "This file isn't a picture Decibell can open.");
      },
    );
    return () => {
      cancelled = true;
    };
  }, [file]);
  const pictureRef = useRef(picture);
  pictureRef.current = picture;
  useEffect(() => () => {
    if (pictureRef.current) URL.revokeObjectURL(pictureRef.current.url);
  }, []);

  // The frame: the output's aspect, as large as the stage allows inside
  // its margin. A resize keeps the crop (the view scales with the frame).
  const frameRef = useRef<Size | null>(null);
  useLayoutEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const measure = () => {
      const m = STAGE_MARGIN[kind];
      const sw = el.clientWidth;
      const sh = el.clientHeight;
      const w = Math.max(40, Math.min(sw - m.x * 2, (sh - m.y * 2) * aspect));
      const next = { w: Math.round(w), h: Math.round(w / aspect) };
      const prev = frameRef.current;
      if (prev && prev.w === next.w && prev.h === next.h) return;
      if (prev) {
        const k = next.w / prev.w;
        const v = store.get();
        store.set({ scale: v.scale * k, x: v.x * k, y: v.y * k });
      }
      frameRef.current = next;
      setFrame(next);
      setOrigin({ left: Math.round((sw - next.w) / 2), top: Math.round((sh - next.h) / 2) });
    };
    measure();
    const ro = new ResizeObserver(measure);
    ro.observe(el);
    return () => ro.disconnect();
  }, [kind, aspect, store]);

  const range = useMemo(
    () => (picture && frame ? { min: minScale(picture.nat, frame, fit), max: maxScale(picture.nat, frame, output) } : null),
    [picture, frame, output, fit],
  );
  const live = useRef({ picture, frame, range, origin });
  live.current = { picture, frame, range, origin };

  // ── motion ──────────────────────────────────────────────────────
  const anim = useRef<{ to: View; raf: number } | null>(null);
  const stopAnim = useCallback(() => {
    if (anim.current) cancelAnimationFrame(anim.current.raf);
    anim.current = null;
  }, []);
  const animateTo = useCallback(
    (to: View, ms: number) => {
      stopAnim();
      const from = store.get();
      if (still() || ms <= 0 || sameView(from, to)) {
        store.set(to);
        return;
      }
      const t0 = performance.now();
      const step = (now: number) => {
        const t = Math.min(1, (now - t0) / ms);
        const e = 1 - Math.pow(1 - t, 3);
        store.set({
          scale: from.scale * Math.pow(to.scale / from.scale, e),
          x: from.x + (to.x - from.x) * e,
          y: from.y + (to.y - from.y) * e,
        });
        if (t < 1 && anim.current) anim.current.raf = requestAnimationFrame(step);
        else anim.current = null;
      };
      anim.current = { to, raf: requestAnimationFrame(step) };
    },
    [store, stopAnim],
  );
  /// Where the picture is headed: wheel notches add up during a glide.
  const aim = () => anim.current?.to ?? store.get();

  // The thirds grid shows while you work and fades shortly after.
  const idle = useRef(0);
  const touch = useCallback(() => {
    adjusted.current = true;
    setInteracting(true);
    window.clearTimeout(idle.current);
    idle.current = window.setTimeout(() => setInteracting(false), 700);
  }, []);
  useEffect(() => () => window.clearTimeout(idle.current), []);

  // A new picture settles into the frame from slightly closer.
  const placed = useRef<Picture | null>(null);
  useEffect(() => {
    if (!picture || !frame || placed.current === picture) return;
    placed.current = picture;
    const start = initialView(picture.nat, frame);
    store.set(zoomAt(start, start.scale * 1.06, { x: frame.w / 2, y: frame.h / 2 }));
    animateTo(start, 420);
  }, [picture, frame, store, animateTo]);

  const zoomBy = useCallback(
    (factor: number, at?: { x: number; y: number }, ms = 180) => {
      const { picture: p, frame: f, range: r } = live.current;
      if (!p || !f || !r) return;
      const base = aim();
      const scale = Math.max(r.min, Math.min(r.max, base.scale * factor));
      animateTo(clampView(zoomAt(base, scale, at ?? { x: f.w / 2, y: f.h / 2 }), p.nat, f, r), ms);
      touch();
    },
    // eslint-disable-next-line react-hooks/exhaustive-deps
    [animateTo, touch],
  );
  const reset = useCallback(() => {
    const { picture: p, frame: f } = live.current;
    if (!p || !f) return;
    animateTo(initialView(p.nat, f), 300);
    setFill(p.alpha ? { kind: "color", color: p.palette[1] } : { kind: "blur" });
    adjusted.current = false;
  }, [animateTo]);

  // ── pointer ─────────────────────────────────────────────────────
  const drag = useRef<{ id: number; px: number; py: number; from: View } | null>(null);
  const onPointerDown = (e: React.PointerEvent) => {
    if (e.button !== 0 || !picture || !frame || saving) return;
    e.currentTarget.setPointerCapture(e.pointerId);
    stopAnim();
    drag.current = { id: e.pointerId, px: e.clientX, py: e.clientY, from: store.get() };
    setDragging(true);
    touch();
  };
  const onPointerMove = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId || !picture || !frame) return;
    const raw = { scale: d.from.scale, x: d.from.x + e.clientX - d.px, y: d.from.y + e.clientY - d.py };
    store.set(rubberView(raw, picture.nat, frame));
    touch();
  };
  const endDrag = (e: React.PointerEvent) => {
    const d = drag.current;
    if (!d || d.id !== e.pointerId) return;
    drag.current = null;
    setDragging(false);
    if (picture && frame && range) animateTo(clampView(store.get(), picture.nat, frame, range), 320);
  };

  // Wheel: proportional zoom toward the pointer. A mouse notch glides, a
  // trackpad's stream of small deltas (and a pinch, which arrives as
  // ctrl + wheel) tracks directly. Bound by hand: React's wheel listener is
  // passive, so preventDefault would be ignored.
  useEffect(() => {
    const el = stageRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const { picture: p, frame: f, range: r, origin: o } = live.current;
      if (!p || !f || !r || drag.current) return;
      const dy = e.deltaMode === 1 ? e.deltaY * 33 : e.deltaMode === 2 ? e.deltaY * 400 : e.deltaY;
      const factor = Math.exp(-dy * (e.ctrlKey ? 0.012 : 0.0025));
      const box = el.getBoundingClientRect();
      const at = { x: e.clientX - box.left - o.left, y: e.clientY - box.top - o.top };
      const base = aim();
      const scale = Math.max(r.min, Math.min(r.max, base.scale * factor));
      const next = clampView(zoomAt(base, scale, at), p.nat, f, r);
      if (!e.ctrlKey && Math.abs(dy) >= 40) animateTo(next, 160);
      else {
        stopAnim();
        store.set(next);
      }
      touch();
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [animateTo, stopAnim, store, touch]);

  // ── open / close / save ─────────────────────────────────────────
  const close = useCallback(() => {
    if (closing.current) return;
    closing.current = true;
    setVisible(false);
    window.setTimeout(onClose, still() ? 0 : 300);
  }, [onClose]);
  /// Esc or a click outside: closes, unless you've worked on the crop; then
  /// the footer shakes (as an unsaved settings tab does). Cancel always closes.
  const requestClose = useCallback(() => {
    if (saving) return;
    if (adjusted.current && picture) setNudge((n) => n + 1);
    else close();
  }, [saving, picture, close]);

  const fly = useCallback(
    (url: string) => {
      const shape = shapeRef.current;
      const dest = document.querySelector<HTMLElement>(`[data-picture-target="${target}"]`);
      if (!shape || !dest || still()) return close();
      const from = shape.getBoundingClientRect();
      const to = dest.getBoundingClientRect();
      const img = document.createElement("img");
      img.src = url;
      img.alt = "";
      Object.assign(img.style, {
        position: "fixed",
        left: `${from.left}px`,
        top: `${from.top}px`,
        width: `${from.width}px`,
        height: `${from.height}px`,
        borderRadius: getComputedStyle(shape).borderRadius,
        zIndex: "61",
        pointerEvents: "none",
        objectFit: "cover",
        boxShadow: "var(--t-shadow-float)",
      });
      document.body.appendChild(img);
      close();
      img
        .animate(
          [
            {},
            {
              left: `${to.left}px`,
              top: `${to.top}px`,
              width: `${to.width}px`,
              height: `${to.height}px`,
              borderRadius: getComputedStyle(dest).borderRadius,
              boxShadow: "0 0 0 0 transparent",
            },
          ],
          { duration: 560, easing: "cubic-bezier(0.5, 0, 0.15, 1)", fill: "forwards" },
        )
        .finished.catch(() => {})
        .finally(() => img.remove());
    },
    [target, close],
  );

  const save = useCallback(async () => {
    const { picture: p, frame: f, range: r } = live.current;
    if (!p || !f || !r || saving) return;
    setSaving(true);
    setSaveError(null);
    stopAnim();
    const view = clampView(store.get(), p.nat, f, r);
    store.set(view);
    try {
      const jpeg = await renderCrop(p, view, f, output, fill);
      await onSave(jpeg);
      const url = URL.createObjectURL(new Blob([jpeg as BlobPart], { type: "image/jpeg" }));
      onSaved(url);
      fly(url);
    } catch (e) {
      setSaving(false);
      setSaveError(e instanceof Error ? e.message : String(e) || "Couldn't save the picture.");
    }
  }, [saving, stopAnim, store, output, fill, onSave, onSaved, fly]);

  // Keys, on the document so they work with nothing focused. Esc is marked
  // handled so the settings window under the editor stays open.
  useEffect(() => {
    const onKey = (e: KeyboardEvent) => {
      if (closing.current) return;
      if (e.key === "Escape") {
        e.preventDefault();
        requestClose();
        return;
      }
      if (saving || !live.current.picture) return;
      const el = e.target as HTMLElement;
      const onSlider = el instanceof HTMLInputElement && el.type === "range";
      if (e.key === "Enter" && el.tagName !== "BUTTON") {
        e.preventDefault();
        void save();
        return;
      }
      const step = e.shiftKey ? 40 : 8;
      const pan = { ArrowLeft: [-step, 0], ArrowRight: [step, 0], ArrowUp: [0, -step], ArrowDown: [0, step] }[e.key];
      if (pan && !onSlider) {
        e.preventDefault();
        const { picture: p, frame: f, range: r } = live.current;
        if (!p || !f || !r) return;
        const v = aim();
        animateTo(clampView({ ...v, x: v.x + pan[0], y: v.y + pan[1] }, p.nat, f, r), 90);
        touch();
      } else if (e.key === "+" || e.key === "=") {
        e.preventDefault();
        zoomBy(1.25);
      } else if (e.key === "-" || e.key === "_") {
        e.preventDefault();
        zoomBy(1 / 1.25);
      } else if (e.key === "0") {
        e.preventDefault();
        reset();
      }
    };
    document.addEventListener("keydown", onKey);
    return () => document.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [requestClose, saving, save, animateTo, touch, zoomBy, reset]);

  // Keep Tab inside the dialog.
  const cardRef = useRef<HTMLDivElement>(null);
  const trapTab = (e: React.KeyboardEvent) => {
    if (e.key !== "Tab" || !cardRef.current) return;
    const items = Array.from(
      cardRef.current.querySelectorAll<HTMLElement>("button:not([disabled]), input:not([type=file]), [tabindex='0']"),
    );
    if (!items.length) return;
    const first = items[0];
    const last = items[items.length - 1];
    if (e.shiftKey && document.activeElement === first) {
      e.preventDefault();
      last.focus();
    } else if (!e.shiftKey && document.activeElement === last) {
      e.preventDefault();
      first.focus();
    }
  };
  useEffect(() => {
    if (picture) stageRef.current?.focus({ preventScroll: true });
  }, [picture]);

  // A press that starts and ends on the backdrop closes; a drag that ends
  // there doesn't.
  const backdropDown = useRef(false);

  const replace = (f: File | null) => {
    if (!f || saving) return;
    placed.current = null;
    setSaveError(null);
    setFile(f);
  };

  const ctx = useMemo<CropState>(() => ({ store, picture, frame, fill }), [store, picture, frame, fill]);
  const radius = frame ? props.radius(frame) : "0";
  const ready = !!picture && !!frame;

  return createPortal(
    <div
      className="picture-editor fixed inset-0 z-[60] flex items-center justify-center p-4 transition-colors duration-300"
      style={{ backgroundColor: visible ? "rgba(0,0,0,0.65)" : "rgba(0,0,0,0)" }}
      onPointerDown={(e) => {
        backdropDown.current = e.target === e.currentTarget;
      }}
      onClick={(e) => {
        if (backdropDown.current && e.target === e.currentTarget) requestClose();
        backdropDown.current = false;
      }}
    >
      <div
        ref={cardRef}
        role="dialog"
        aria-modal="true"
        aria-label={title}
        onKeyDown={trapTab}
        onDragOver={(e) => {
          if (!e.dataTransfer.types.includes("Files")) return;
          e.preventDefault();
          setDropOver(true);
        }}
        onDragLeave={(e) => {
          if (!e.currentTarget.contains(e.relatedTarget as Node | null)) setDropOver(false);
        }}
        onDrop={(e) => {
          e.preventDefault();
          setDropOver(false);
          replace(pictureFromList(e.dataTransfer.files));
        }}
        className={`flex max-h-full max-w-full flex-col overflow-hidden rounded-xl border border-border bg-bg-dark shadow-modal transition-[opacity,transform] duration-300 ${
          kind === "avatar" ? "w-162" : "w-172"
        }`}
        style={{ opacity: visible ? 1 : 0, transform: visible ? "scale(1)" : "scale(0.96)" }}
      >
        <header className="flex items-start justify-between gap-4 px-6 pb-4 pt-5">
          <div className="min-w-0">
            <h2 className="font-display text-[18px] font-semibold text-text-primary">{title}</h2>
            <p className="mt-0.5 text-[13px] text-text-muted">
              {picture?.animated ? "GIFs keep their first frame. " : ""}Drag to move, scroll to zoom.
            </p>
          </div>
          {picture && (
            <span className="mt-1 max-w-56 shrink-0 truncate rounded-sm bg-bg-lighter px-2 py-1 font-meta text-[11px] tabular-nums text-text-muted" title={picture.name}>
              {picture.nat.w} × {picture.nat.h}
            </span>
          )}
        </header>

        <CropContext.Provider value={ctx}>
          <div className={`flex min-h-0 gap-6 px-6 ${kind === "avatar" ? "flex-row" : "flex-col gap-4"}`}>
            <div className="flex min-w-0 flex-col gap-3">
              <div
                ref={stageRef}
                tabIndex={0}
                role="group"
                aria-roledescription="crop area"
                aria-label="Picture. Drag or use the arrow keys to move it, plus and minus to zoom."
                onPointerDown={onPointerDown}
                onPointerMove={onPointerMove}
                onPointerUp={endDrag}
                onPointerCancel={endDrag}
                onDoubleClick={reset}
                className={`relative shrink-0 touch-none select-none overflow-hidden rounded-lg bg-bg-darkest outline-none focus-visible:shadow-ring ${
                  kind === "avatar" ? "size-88 [@media(max-height:680px)]:size-72" : "h-68 w-full [@media(max-height:680px)]:h-56"
                } ${!ready ? "" : dragging ? "cursor-grabbing" : "cursor-grab"} ${saving ? "pointer-events-none" : ""}`}
              >
                {ready && (
                  <>
                    <FrameFill picture={picture} frame={frame} origin={origin} fill={fill} />
                    <StageImage store={store} picture={picture} origin={origin} />
                    <Veil frame={frame} origin={origin} radius={radius} grid={interacting} shapeRef={shapeRef} />
                    <FillPicker
                      picture={picture}
                      frame={frame}
                      range={range!}
                      store={store}
                      fill={fill}
                      onFill={(f) => {
                        setFill(f);
                        adjusted.current = true;
                      }}
                    />
                  </>
                )}
                {!picture && !error && <div className="absolute inset-0 animate-pulse bg-bg-light/40" />}
                {error && (
                  <div className="absolute inset-0 flex flex-col items-center justify-center gap-2 px-8 text-center">
                    <span className="text-text-muted">
                      <AlertIcon />
                    </span>
                    <p className="text-[14px] font-medium text-text-primary">Can't open this file</p>
                    <p className="text-[12px] leading-[1.5] text-text-muted">{error}</p>
                    <button
                      type="button"
                      onClick={() => inputRef.current?.click()}
                      className="mt-2 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover"
                    >
                      Choose another picture
                    </button>
                  </div>
                )}
                {dropOver && (
                  <div className="pointer-events-none absolute inset-2 flex items-center justify-center rounded-md border-2 border-dashed border-accent bg-accent-soft text-[13px] font-semibold text-text-primary">
                    Drop to use this picture instead
                  </div>
                )}
              </div>
              <Toolbar store={store} picture={picture} frame={frame} range={range} disabled={!ready || saving} onZoom={(f) => zoomBy(f)}
                onSlide={(scale) => {
                  if (!picture || !frame || !range) return;
                  stopAnim();
                  store.set(clampView(zoomAt(store.get(), scale, { x: frame.w / 2, y: frame.h / 2 }), picture.nat, frame, range));
                  touch();
                }}
                onReset={reset}
              />
            </div>
            <div className={kind === "avatar" ? "w-56 shrink-0" : "min-w-0"}>{previews}</div>
          </div>
        </CropContext.Provider>

        {saveError && (
          <div className="mx-6 mt-4 rounded-md border border-error/25 bg-error/10 px-3 py-2 text-[12px] leading-[1.5] text-error">
            {saveError}
          </div>
        )}
        <footer
          key={nudge}
          className={`mt-5 flex items-center gap-2 border-t border-border-divider px-6 py-4 ${nudge ? "animate-[barNudge_0.36s_ease]" : ""}`}
        >
          <button
            type="button"
            onClick={() => inputRef.current?.click()}
            disabled={saving}
            className="flex items-center gap-2 rounded-sm px-2 py-2 text-[13px] font-medium text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
          >
            <ImageIcon />
            Choose another
          </button>
          <span className="min-w-0 flex-1 truncate text-[12px] text-text-muted">{nudge ? "Save the picture, or Cancel to drop it." : ""}</span>
          <button
            type="button"
            onClick={close}
            disabled={saving}
            className="rounded-sm px-4 py-2 text-[13px] font-semibold text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            type="button"
            onClick={() => void save()}
            disabled={!ready || saving}
            className="flex items-center gap-2 rounded-sm bg-accent px-4 py-2 text-[13px] font-semibold text-on-accent hover:bg-accent-hover disabled:opacity-50"
          >
            {saving && <span className="size-3.5 animate-spin rounded-full border-2 border-current border-r-transparent" aria-hidden />}
            {saving ? "Saving…" : "Save"}
          </button>
        </footer>
        <input
          ref={inputRef}
          type="file"
          accept={PICTURE_ACCEPT}
          className="hidden"
          onChange={(e) => {
            replace(pictureFromList(e.target.files));
            e.target.value = "";
          }}
        />
      </div>
    </div>,
    document.body,
  );
}

/// The picture on the stage, at its place relative to the frame.
function StageImage({ store, picture, origin }: { store: ViewStore; picture: Picture; origin: { left: number; top: number } }) {
  const v = useView(store);
  return (
    <img
      src={picture.url}
      alt=""
      draggable={false}
      className="pointer-events-none absolute left-0 top-0 max-w-none"
      style={{
        width: picture.nat.w * v.scale,
        height: picture.nat.h * v.scale,
        transform: `translate3d(${origin.left + v.x}px, ${origin.top + v.y}px, 0)`,
      }}
    />
  );
}

/// What shows in the frame where the picture doesn't: always there, so a
/// gap opening mid-zoom is already filled.
function FrameFill({ picture, frame, origin, fill }: { picture: Picture; frame: Size; origin: { left: number; top: number }; fill: Fill }) {
  return (
    <div
      className="pointer-events-none absolute overflow-hidden"
      style={{ ...origin, width: frame.w, height: frame.h, background: fill.kind === "color" ? fill.color : picture.palette[1] }}
    >
      {fill.kind === "blur" && <BlurFill picture={picture} w={frame.w} h={frame.h} />}
    </div>
  );
}

function BlurFill({ picture: p, w, h, ox = 0, oy = 0 }: { picture: Picture; w: number; h: number; ox?: number; oy?: number }) {
  const s = Math.max(w / p.nat.w, h / p.nat.h) * 1.1;
  return (
    <img
      src={p.url}
      alt=""
      aria-hidden
      draggable={false}
      className="pointer-events-none absolute left-0 top-0 max-w-none"
      style={{
        width: p.nat.w * s,
        height: p.nat.h * s,
        transform: `translate(${ox + (w - p.nat.w * s) / 2}px, ${oy + (h - p.nat.h * s) / 2}px)`,
        filter: `blur(${BLUR_SHARE * w}px) saturate(1.2)`,
      }}
    />
  );
}

/// The veil outside the frame, the half-lit corners between the square and
/// the shape, the frame's edge and the thirds grid.
function Veil({
  frame,
  origin,
  radius,
  grid,
  shapeRef,
}: {
  frame: Size;
  origin: { left: number; top: number };
  radius: string;
  grid: boolean;
  shapeRef: React.RefObject<HTMLDivElement>;
}) {
  const box = { ...origin, width: frame.w, height: frame.h };
  return (
    <>
      <div className="pointer-events-none absolute" style={{ ...box, boxShadow: "0 0 0 100vmax var(--crop-veil)" }} />
      <div className="pointer-events-none absolute overflow-hidden" style={box}>
        <div
          ref={shapeRef}
          className="absolute inset-0"
          style={{ borderRadius: radius, boxShadow: "0 0 0 100vmax var(--crop-corner), inset 0 0 0 1.5px var(--crop-edge)" }}
        />
        <div className={`absolute inset-0 transition-opacity duration-300 ${grid ? "opacity-100" : "opacity-0"}`}>
          {[1, 2].map((i) => (
            <div key={`v${i}`} className="absolute inset-y-0 w-px bg-[var(--crop-grid)]" style={{ left: `${(i * 100) / 3}%` }} />
          ))}
          {[1, 2].map((i) => (
            <div key={`h${i}`} className="absolute inset-x-0 h-px bg-[var(--crop-grid)]" style={{ top: `${(i * 100) / 3}%` }} />
          ))}
        </div>
      </div>
    </>
  );
}

/// The fill's swatches, floating under the frame while the crop shows any
/// of it: a gap after zooming out, or a transparent picture. Judged on where
/// the picture comes to rest, so a drag past an edge doesn't flash them.
function FillPicker({
  picture,
  frame,
  range,
  store,
  fill,
  onFill,
}: {
  picture: Picture;
  frame: Size;
  range: { min: number; max: number };
  store: ViewStore;
  fill: Fill;
  onFill: (f: Fill) => void;
}) {
  const v = useView(store);
  const shown = picture.alpha || leavesGap(clampView(v, picture.nat, frame, range), picture.nat, frame);
  const options: { fill: Fill; label: string }[] = [
    { fill: { kind: "blur" }, label: "Blurred picture" },
    { fill: { kind: "color", color: picture.palette[0] }, label: "Picture colour" },
    { fill: { kind: "color", color: picture.palette[1] }, label: "Deep shade" },
    { fill: { kind: "color", color: "#ffffff" }, label: "White" },
    { fill: { kind: "color", color: "#0b0a0d" }, label: "Black" },
  ];
  const same = (a: Fill, b: Fill) => a.kind === b.kind && (a.kind === "blur" || (b.kind === "color" && a.color === b.color));
  return (
    <div
      onPointerDown={(e) => e.stopPropagation()}
      onDoubleClick={(e) => e.stopPropagation()}
      className={`absolute bottom-2 left-1/2 flex -translate-x-1/2 items-center gap-1.5 rounded-full border border-border bg-bg-light py-1 pl-3 pr-1 shadow-float transition-[opacity,translate] duration-200 ${
        shown ? "opacity-100" : "pointer-events-none translate-y-1 opacity-0"
      }`}
      aria-hidden={!shown}
    >
      <span className="mr-0.5 text-[11px] font-medium text-text-muted">Background</span>
      {options.map((o) => (
        <button
          key={o.label}
          type="button"
          tabIndex={shown ? 0 : -1}
          title={o.label}
          aria-label={o.label}
          aria-pressed={same(o.fill, fill)}
          onClick={() => onFill(o.fill)}
          className={`relative size-5.5 overflow-hidden rounded-full border border-border ${
            same(o.fill, fill) ? "ring-2 ring-accent ring-offset-2 ring-offset-bg-light" : "hover:scale-110"
          } transition-transform duration-150`}
          style={{ background: o.fill.kind === "color" ? o.fill.color : undefined }}
        >
          {o.fill.kind === "blur" && (
            <img src={picture.url} alt="" draggable={false} className="absolute inset-0 size-full scale-150 object-cover blur-[3px]" />
          )}
        </button>
      ))}
    </div>
  );
}

function Toolbar({
  store,
  picture,
  frame,
  range,
  disabled,
  onZoom,
  onSlide,
  onReset,
}: {
  store: ViewStore;
  picture: Picture | null;
  frame: Size | null;
  range: { min: number; max: number } | null;
  disabled: boolean;
  onZoom: (factor: number) => void;
  onSlide: (scale: number) => void;
  onReset: () => void;
}) {
  const v = useView(store);
  const t = range ? toSlider(v.scale, range) : 0;
  // "Fills the frame" sits partway along when the picture can zoom out past it.
  const cover = picture && frame && range ? toSlider(coverScale(picture.nat, frame), range) : 0;
  const atStart = picture && frame ? sameView(v, initialView(picture.nat, frame)) : true;
  const btn =
    "flex size-8 shrink-0 items-center justify-center rounded-sm text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:pointer-events-none disabled:opacity-40";
  return (
    <div className="flex items-center gap-1">
      <button type="button" className={btn} onClick={() => onZoom(1 / 1.25)} disabled={disabled || t <= 0.001} aria-label="Zoom out" title="Zoom out (−)">
        <ZoomOutIcon />
      </button>
      <div className="relative mx-1 flex h-8 min-w-0 flex-1 items-center">
        {cover > 0.02 && (
          <span
            aria-hidden
            className="pointer-events-none absolute top-1/2 h-2.5 w-0.5 -translate-x-1/2 -translate-y-1/2 rounded-full bg-text-muted/60"
            style={{ left: `calc(7px + (100% - 14px) * ${cover})` }}
          />
        )}
        <input
          type="range"
          aria-label="Zoom"
          min={0}
          max={1}
          step={0.001}
          value={t}
          disabled={disabled}
          onChange={(e) => {
            if (!range || !picture || !frame) return;
            let s = fromSlider(Number(e.target.value), range);
            // A detent at "fills the frame".
            const c = coverScale(picture.nat, frame);
            if (Math.abs(Math.log(s / c)) < 0.035) s = c;
            onSlide(s);
          }}
          className="picture-zoom relative h-1 w-full cursor-pointer appearance-none rounded-full disabled:cursor-default"
          style={{ ["--fill" as string]: `${t * 100}%` }}
        />
      </div>
      <button type="button" className={btn} onClick={() => onZoom(1.25)} disabled={disabled || t >= 0.999} aria-label="Zoom in" title="Zoom in (+)">
        <ZoomInIcon />
      </button>
      <button
        type="button"
        onClick={onReset}
        disabled={disabled || atStart}
        title="Reset (0)"
        className="ml-1 flex h-8 shrink-0 items-center gap-1.5 rounded-sm px-2.5 text-[12px] font-medium text-text-secondary hover:bg-surface-hover hover:text-text-primary disabled:pointer-events-none disabled:opacity-40"
      >
        <ResetIcon />
        Reset
      </button>
    </div>
  );
}

/// A live preview: the crop scaled into a `width` × `height` box. A box of
/// another aspect shows the middle of the crop, as `object-cover` will (an
/// invite card's banner is wider than the tile).
export function Crop({ width, height, radius, className = "" }: { width: number; height: number; radius?: string; className?: string }) {
  const c = useContext(CropContext);
  if (!c) throw new Error("Crop outside PictureEditor");
  const v = useView(c.store);
  const { picture: p, frame, fill } = c;
  if (!p || !frame) {
    return <div className={`shrink-0 bg-bg-lighter ${className}`} style={{ width, height, borderRadius: radius }} />;
  }
  const k = Math.max(width / frame.w, height / frame.h);
  const ox = (width - frame.w * k) / 2;
  const oy = (height - frame.h * k) / 2;
  return (
    <div
      className={`relative shrink-0 overflow-hidden ${className}`}
      style={{ width, height, borderRadius: radius, background: fill.kind === "color" ? fill.color : p.palette[1] }}
    >
      {fill.kind === "blur" && <BlurFill picture={p} w={frame.w * k} h={frame.h * k} ox={ox} oy={oy} />}
      <img
        src={p.url}
        alt=""
        draggable={false}
        className="pointer-events-none absolute left-0 top-0 max-w-none"
        style={{
          width: p.nat.w * v.scale * k,
          height: p.nat.h * v.scale * k,
          transform: `translate(${ox + v.x * k}px, ${oy + v.y * k}px)`,
        }}
      />
    </div>
  );
}

/// The small uppercase label over each preview.
export function PreviewLabel({ children }: { children: ReactNode }) {
  return <div className="mb-2 font-section text-section font-medium uppercase leading-none tracking-section text-text-muted">{children}</div>;
}
