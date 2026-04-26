import { useEffect, useRef, useState } from "react";
import { pickSavePath } from "./filePicker";
import { invoke } from "@tauri-apps/api/core";
import type { Attachment } from "../../types";
import { useImageViewerStore } from "../../stores/imageViewerStore";
import { useImageContextMenuStore } from "../../stores/imageContextMenuStore";
import { copyAttachmentToClipboard, getCachedImage, getOrFetchImage } from "./imageCache";
import { fetchThumbnail, getCachedThumbnail, pickSize } from "./attachmentThumbnailCache";
import { toast } from "../../stores/toastStore";

const MIN_ZOOM = 0.1;
const MAX_ZOOM = 12;
const ZOOM_STEP = 1.25;

type Pan = { x: number; y: number };
const ZERO: Pan = { x: 0, y: 0 };

/** Clamp pan so the displayed image stays usable. At zoom <= 1 there's
 *  no slack (pan must be 0,0 — the image fits the area). When zoomed
 *  in, the strict overflow gives "image edge cannot leave area edge",
 *  but that traps the user in the image's bounds. We add a `slack`
 *  term that grows with zoom and is capped at half the area, which lets
 *  the user drag a corner of the image to the viewport center at high
 *  zoom. The slack is proportional to overflow rather than a step
 *  function, so zoom-out still drifts the image smoothly back to
 *  center as overflow shrinks toward zero. */
function clampPan(pan: Pan, zoom: number, img: HTMLImageElement | null, area: HTMLElement | null): Pan {
  if (!img || !area || zoom <= 1) return ZERO;
  const baseW = img.offsetWidth;
  const baseH = img.offsetHeight;
  const cw = area.offsetWidth;
  const ch = area.offsetHeight;
  const overflowX = Math.max(0, (baseW * zoom - cw) / 2);
  const overflowY = Math.max(0, (baseH * zoom - ch) / 2);
  // Slack tracks overflow until it reaches half the area's size, then
  // plateaus. That's the size at which a corner of the image can be
  // moved to the viewport center, which is exactly the maximum useful
  // pan distance.
  const slackX = Math.min(cw / 2, overflowX);
  const slackY = Math.min(ch / 2, overflowY);
  const maxX = overflowX + slackX;
  const maxY = overflowY + slackY;
  return {
    x: Math.max(-maxX, Math.min(maxX, pan.x)),
    y: Math.max(-maxY, Math.min(maxY, pan.y)),
  };
}

export default function ImageViewer() {
  const open = useImageViewerStore((s) => s.open);
  const images = useImageViewerStore((s) => s.images);
  const serverId = useImageViewerStore((s) => s.serverId);
  const index = useImageViewerStore((s) => s.index);
  const close = useImageViewerStore((s) => s.close);
  const next = useImageViewerStore((s) => s.next);
  const prev = useImageViewerStore((s) => s.prev);
  const setIndex = useImageViewerStore((s) => s.setIndex);

  const current = images[index] ?? null;
  const hasPrev = index > 0;
  const hasNext = index < images.length - 1;

  // `fullUrl` is the high-res original; `thumbUrl` is the largest
  // server-side pre-generated thumbnail. We render the thumb the
  // moment it's available so the user gets immediate feedback while
  // the multi-megabyte original transits, then upgrade to the full
  // image once it lands. Looks like a brief blur-up.
  const [fullUrl, setFullUrl] = useState<string | null>(null);
  const [thumbUrl, setThumbUrl] = useState<string | null>(null);
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const url = fullUrl ?? thumbUrl;
  const showingThumb = !fullUrl && !!thumbUrl;

  // Zoom + pan, reset on image change.
  const [zoom, setZoom] = useState(1);
  const [pan, setPan] = useState<Pan>(ZERO);
  useEffect(() => {
    setZoom(1);
    setPan(ZERO);
  }, [current?.id]);

  // Fetch current image. Two parallel requests: the largest server
  // thumbnail (small, lands fast — used as immediate placeholder) and
  // the full original (slow, swapped in when ready). The thumbnail
  // request is skipped for legacy attachments (mask=0) since they'd
  // just hit the legacy 320 file with a single ~30 KB body anyway.
  useEffect(() => {
    if (!open || !current || !serverId) {
      setFullUrl(null);
      setThumbUrl(null);
      return;
    }
    let cancelled = false;
    setLoading(true);
    setError(null);

    // Seed from cache so a re-open of the same image is instant.
    const cachedFull = getCachedImage(current.id);
    if (cachedFull) {
      setFullUrl(cachedFull);
      setLoading(false);
    } else {
      setFullUrl(null);
      // Pick the largest pre-generated size we have for the placeholder.
      const thumbSize = current.thumbnailSizeBytes > 0
        ? pickSize(Number.MAX_SAFE_INTEGER, current.thumbnailSizesMask)
        : null;
      const cachedThumb = current.thumbnailSizeBytes > 0
        ? getCachedThumbnail(current.id, thumbSize)
        : null;
      setThumbUrl(cachedThumb);

      // Kick off the placeholder fetch (no-op if cached).
      if (current.thumbnailSizeBytes > 0 && !cachedThumb) {
        fetchThumbnail(serverId, current.id, thumbSize).then((u) => {
          if (cancelled || !u) return;
          // Only set if the full hasn't already arrived — otherwise
          // we'd briefly downgrade visible quality.
          setThumbUrl((prev) => prev ?? u);
        });
      }
    }

    // Always request the full bytes — they're what the viewer actually
    // shows. Dedup is handled by getOrFetchImage's inflight map.
    getOrFetchImage(serverId, current.id, current.mime)
      .then((u) => {
        if (cancelled) return;
        setFullUrl(u);
        setLoading(false);
      })
      .catch((e) => {
        if (cancelled) return;
        setError(String(e));
        setLoading(false);
      });
    return () => {
      cancelled = true;
    };
  }, [open, current?.id, current?.mime, serverId]);

  // Prefetch only the immediate next neighbour so arrow nav feels
  // instant without firing several full-image transfers in parallel
  // when the user opens a busy viewer.
  useEffect(() => {
    if (!open || !serverId) return;
    const target = images[index + 1];
    if (target) {
      getOrFetchImage(serverId, target.id, target.mime).catch(() => {});
    }
  }, [open, index, images, serverId]);

  // Refs used by clampPan and pointer/wheel handlers.
  const surfaceRef = useRef<HTMLDivElement>(null);
  const imageAreaRef = useRef<HTMLDivElement>(null);
  const imgRef = useRef<HTMLImageElement>(null);

  // ---- Zoom helpers ---------------------------------------------------
  // Wraps setZoom to also re-clamp pan against the new zoom — that's
  // what makes zooming out near a corner smoothly drift the image
  // back toward center instead of snapping at zoom = 1.
  const applyZoom = (newZoom: number, focal?: { x: number; y: number }) => {
    // Snap to 100% whenever a step would carry us across it. The wheel
    // and keyboard use a geometric ZOOM_STEP that won't otherwise land
    // on 1.0 exactly, so without this you end up scrolling past it and
    // having to click the "100%" button to recover.
    if ((zoom > 1 && newZoom < 1) || (zoom < 1 && newZoom > 1)) {
      newZoom = 1;
    }
    const clampedZoom = Math.max(MIN_ZOOM, Math.min(MAX_ZOOM, newZoom));
    if (clampedZoom === zoom) return;
    let nextPan = pan;
    if (focal && zoom > 0) {
      // Cursor-focused zoom: keep the image-local point under the
      // cursor at the same screen position after the zoom step. Math:
      //   newPan = focal - (focal - pan) * (newZoom / zoom)
      const ratio = clampedZoom / zoom;
      nextPan = {
        x: focal.x - (focal.x - pan.x) * ratio,
        y: focal.y - (focal.y - pan.y) * ratio,
      };
    } else {
      // Center zoom: scale pan in place.
      const ratio = clampedZoom / zoom;
      nextPan = { x: pan.x * ratio, y: pan.y * ratio };
    }
    nextPan = clampPan(nextPan, clampedZoom, imgRef.current, imageAreaRef.current);
    setZoom(clampedZoom);
    setPan(nextPan);
  };

  // Keyboard.
  useEffect(() => {
    if (!open) return;
    const onKey = (e: KeyboardEvent) => {
      // Ctrl/Cmd + C — copy the current image. Ignore the other arrow/
      // zoom shortcuts when a modifier is held so we don't double-fire.
      if ((e.ctrlKey || e.metaKey) && (e.key === "c" || e.key === "C")) {
        e.preventDefault();
        if (!current || !serverId) return;
        const filename = current.filename;
        copyAttachmentToClipboard(serverId, current.id, current.mime)
          .then(() => toast.success("Image copied", filename))
          .catch((err) => toast.error("Copy failed", String(err)));
        return;
      }
      if (e.ctrlKey || e.metaKey || e.altKey) return;
      if (e.key === "Escape") close();
      else if (e.key === "ArrowRight" && hasNext) next();
      else if (e.key === "ArrowLeft" && hasPrev) prev();
      else if (e.key === "+" || e.key === "=") applyZoom(zoom * ZOOM_STEP);
      else if (e.key === "-" || e.key === "_") applyZoom(zoom / ZOOM_STEP);
      else if (e.key === "0") {
        setZoom(1);
        setPan(ZERO);
      }
    };
    window.addEventListener("keydown", onKey);
    return () => window.removeEventListener("keydown", onKey);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, hasPrev, hasNext, zoom, pan, current?.id, serverId]);

  // Wheel zoom — focal point is the cursor.
  useEffect(() => {
    const el = surfaceRef.current;
    if (!el || !open) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const surface = surfaceRef.current;
      if (!surface) return;
      const r = surface.getBoundingClientRect();
      const focal = {
        x: e.clientX - (r.left + r.width / 2),
        y: e.clientY - (r.top + r.height / 2),
      };
      applyZoom(zoom * (e.deltaY < 0 ? ZOOM_STEP : 1 / ZOOM_STEP), focal);
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
    // eslint-disable-next-line react-hooks/exhaustive-deps
  }, [open, zoom, pan]);

  // Drag pan.
  const dragRef = useRef<{ startX: number; startY: number; basePan: Pan } | null>(null);
  const onMouseDown = (e: React.MouseEvent) => {
    if (zoom <= 1) return;
    if (e.button !== 0) return;
    dragRef.current = { startX: e.clientX, startY: e.clientY, basePan: { ...pan } };
    e.preventDefault();
  };
  useEffect(() => {
    const onMove = (e: MouseEvent) => {
      const d = dragRef.current;
      if (!d) return;
      const next = clampPan(
        { x: d.basePan.x + (e.clientX - d.startX), y: d.basePan.y + (e.clientY - d.startY) },
        zoom,
        imgRef.current,
        imageAreaRef.current,
      );
      setPan(next);
    };
    const onUp = () => {
      dragRef.current = null;
    };
    window.addEventListener("mousemove", onMove);
    window.addEventListener("mouseup", onUp);
    return () => {
      window.removeEventListener("mousemove", onMove);
      window.removeEventListener("mouseup", onUp);
    };
  }, [zoom]);

  const onBackgroundClick = (e: React.MouseEvent) => {
    if (zoom > 1) return;
    if (e.target !== e.currentTarget) return;
    close();
  };

  const onDownload = async () => {
    if (!current || !serverId) return;
    let dest: string | null = null;
    try {
      dest = await pickSavePath({
        title: "Save image",
        defaultName: current.filename || "image",
      });
    } catch (err) {
      toast.error("Save dialog failed", String(err));
      return;
    }
    if (!dest) return;
    try {
      await invoke("download_attachment", {
        req: { serverId, attachmentId: current.id, destinationPath: dest },
      });
      toast.success("Image saved", current.filename);
    } catch (err) {
      toast.error("Save failed", String(err));
    }
  };

  if (!open || !current) return null;

  const imageStyle: React.CSSProperties = {
    transform: `translate(${pan.x}px, ${pan.y}px) scale(${zoom})`,
    transition: "filter 200ms ease",
    opacity: !url ? 0.4 : 1,
    // Blur the low-res placeholder so the upgrade to full reads as a
    // sharpen; no-op once the full bytes land.
    filter: showingThumb ? "blur(8px)" : "none",
  };

  return (
    <div
      ref={surfaceRef}
      onClick={onBackgroundClick}
      onMouseDown={onMouseDown}
      className="fixed inset-0 z-[70] flex items-center justify-center bg-black/95 select-none animate-[fadeUp_0.18s_ease_both]"
      style={{ cursor: zoom > 1 ? (dragRef.current ? "grabbing" : "grab") : "default" }}
    >
      {/* Image area. Bottom padding is generous when a filmstrip is
          shown so that, at zoom = 1, the image always has visual breathing
          room above the strip. */}
      <div
        ref={imageAreaRef}
        className={`pointer-events-none flex h-full w-full items-center justify-center px-16 pt-16 ${
          images.length > 1 ? "pb-44" : "pb-20"
        }`}
      >
        {url && !error && (
          <img
            ref={imgRef}
            src={url}
            alt={current.filename}
            draggable={false}
            decoding="async"
            onContextMenu={(e) => {
              if (!serverId) return;
              e.preventDefault();
              e.stopPropagation();
              useImageContextMenuStore.getState().show({
                x: e.clientX,
                y: e.clientY,
                serverId,
                attachmentId: current.id,
                filename: current.filename,
              });
            }}
            className="pointer-events-auto max-h-full max-w-full object-contain"
            style={imageStyle}
          />
        )}
        {loading && !url && <div className="text-[12.5px] text-text-muted">Loading…</div>}
        {error && (
          <div className="max-w-md text-center text-[13px] text-error">
            Failed to load: {error}
          </div>
        )}
      </div>

      {/* Top bar */}
      <div className="pointer-events-none absolute inset-x-0 top-0 flex items-start justify-between gap-4 bg-gradient-to-b from-black/70 to-transparent px-4 py-3.5">
        <div className="pointer-events-auto flex min-w-0 flex-col">
          <span className="truncate text-[13px] font-semibold text-white [overflow-wrap:anywhere]">
            {current.filename}
          </span>
          <span className="text-[11px] text-white/60">
            {index + 1} of {images.length}
          </span>
        </div>
        <div className="pointer-events-auto flex shrink-0 gap-1.5">
          <ViewerIconButton title="Download" onClick={onDownload}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2" strokeLinecap="round" strokeLinejoin="round">
              <path d="M21 15v4a2 2 0 0 1-2 2H5a2 2 0 0 1-2-2v-4" />
              <polyline points="7 10 12 15 17 10" />
              <line x1="12" y1="15" x2="12" y2="3" />
            </svg>
          </ViewerIconButton>
          <ViewerIconButton title="Close (Esc)" onClick={close}>
            <svg width="16" height="16" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round" strokeLinejoin="round">
              <line x1="18" y1="6" x2="6" y2="18" />
              <line x1="6" y1="6" x2="18" y2="18" />
            </svg>
          </ViewerIconButton>
        </div>
      </div>

      {/* Side nav arrows */}
      {hasPrev && (
        <ViewerNavButton side="left" title="Previous (←)" onClick={(e) => { e.stopPropagation(); prev(); }} />
      )}
      {hasNext && (
        <ViewerNavButton side="right" title="Next (→)" onClick={(e) => { e.stopPropagation(); next(); }} />
      )}

      {/* Bottom: filmstrip + zoom controls */}
      <div className="pointer-events-none absolute inset-x-0 bottom-0 flex flex-col items-center gap-2 pb-4">
        {images.length > 1 && (
          <FilmStrip
            images={images}
            currentIndex={index}
            onSelect={setIndex}
            serverId={serverId}
            hidden={zoom > 1}
          />
        )}
        <div
          className="pointer-events-auto flex items-center gap-1 rounded-xl border border-white/10 bg-bg-darkest/95 px-2 py-1 backdrop-blur-md"
          onMouseDown={(e) => e.stopPropagation()}
        >
          <ZoomIconButton
            title="Zoom out (-)"
            onClick={(e) => { e.stopPropagation(); applyZoom(zoom / ZOOM_STEP); }}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="5" y1="12" x2="19" y2="12" />
            </svg>
          </ZoomIconButton>
          <button
            onClick={(e) => { e.stopPropagation(); setZoom(1); setPan(ZERO); }}
            className="min-w-[58px] rounded-md px-2.5 py-1 text-[11.5px] font-medium tabular-nums text-white/85 transition-colors hover:bg-white/10"
            title="Reset (0)"
          >
            {Math.round(zoom * 100)}%
          </button>
          <ZoomIconButton
            title="Zoom in (+)"
            onClick={(e) => { e.stopPropagation(); applyZoom(zoom * ZOOM_STEP); }}
          >
            <svg width="14" height="14" viewBox="0 0 24 24" fill="none" stroke="currentColor" strokeWidth="2.5" strokeLinecap="round">
              <line x1="5" y1="12" x2="19" y2="12" />
              <line x1="12" y1="5" x2="12" y2="19" />
            </svg>
          </ZoomIconButton>
        </div>
      </div>
    </div>
  );
}

// --- Filmstrip --------------------------------------------------------

function FilmStrip({
  images,
  currentIndex,
  onSelect,
  serverId,
  hidden,
}: {
  images: Attachment[];
  currentIndex: number;
  onSelect: (i: number) => void;
  serverId: string | null;
  hidden: boolean;
}) {
  const itemRefs = useRef<(HTMLButtonElement | null)[]>([]);

  // Auto-center the active thumb whenever the user navigates. Skip while
  // hidden so we don't waste smooth-scroll work on something nobody sees.
  useEffect(() => {
    if (hidden) return;
    const item = itemRefs.current[currentIndex];
    item?.scrollIntoView({ behavior: "smooth", block: "nearest", inline: "center" });
  }, [currentIndex, hidden]);

  return (
    <div
      className={`pointer-events-auto flex max-w-[80%] gap-1.5 overflow-x-auto rounded-xl border border-white/10 bg-bg-darkest/95 px-2 py-1.5 backdrop-blur-md scrollbar-thin transition-all duration-200 ease-out ${
        hidden
          ? "pointer-events-none opacity-0 translate-y-3"
          : "opacity-100 translate-y-0"
      }`}
      onMouseDown={(e) => e.stopPropagation()}
      onClick={(e) => e.stopPropagation()}
    >
      {images.map((img, i) => (
        <FilmStripThumb
          key={img.id}
          image={img}
          active={i === currentIndex}
          onClick={() => onSelect(i)}
          serverId={serverId}
          buttonRef={(el) => {
            itemRefs.current[i] = el;
          }}
        />
      ))}
    </div>
  );
}

function FilmStripThumb({
  image,
  active,
  onClick,
  serverId,
  buttonRef,
}: {
  image: Attachment;
  active: boolean;
  onClick: () => void;
  serverId: string | null;
  buttonRef: (el: HTMLButtonElement | null) => void;
}) {
  // Filmstrip tiles are 56 × 56 px (≈ 112 px @2× DPR) — the smallest
  // pre-generated 320 size is the right pick. This was previously
  // fetching the FULL original (multi-MB per tile), which fanned out
  // to hundreds of MB whenever the user opened the viewer in a busy
  // channel. Falls back to the legacy thumbnail or full image only
  // when the attachment has no server thumbnail at all.
  const useServerThumb = image.thumbnailSizeBytes > 0;
  const initialThumb = useServerThumb
    ? getCachedThumbnail(image.id, pickSize(112, image.thumbnailSizesMask))
    : getCachedImage(image.id);
  const [url, setUrl] = useState<string | null>(initialThumb);
  const localRef = useRef<HTMLButtonElement | null>(null);

  useEffect(() => {
    if (url || !serverId) return;
    const el = localRef.current;
    if (!el) return;
    let cancelled = false;
    const io = new IntersectionObserver(
      (entries) => {
        if (entries.some((e) => e.isIntersecting)) {
          io.disconnect();
          const fetcher = useServerThumb
            ? fetchThumbnail(serverId, image.id, pickSize(112, image.thumbnailSizesMask))
            : getOrFetchImage(serverId, image.id, image.mime);
          fetcher
            .then((u) => {
              if (!cancelled && u) setUrl(u);
            })
            .catch(() => {});
        }
      },
      { root: el.parentElement, rootMargin: "100px 0px" },
    );
    io.observe(el);
    return () => {
      cancelled = true;
      io.disconnect();
    };
  }, [url, image.id, image.mime, image.thumbnailSizeBytes, image.thumbnailSizesMask, serverId, useServerThumb]);

  return (
    <button
      ref={(el) => {
        localRef.current = el;
        buttonRef(el);
      }}
      onClick={onClick}
      title={image.filename}
      className={`relative h-14 w-14 shrink-0 overflow-hidden rounded-md transition-all ${
        active
          ? "ring-2 ring-accent-bright ring-offset-1 ring-offset-black/95 scale-105"
          : "opacity-55 hover:opacity-100 hover:scale-[1.03]"
      }`}
    >
      {url ? (
        <img
          src={url}
          alt={image.filename}
          className="h-full w-full object-cover"
          draggable={false}
        />
      ) : (
        <div className="h-full w-full bg-white/5" />
      )}
    </button>
  );
}

function ViewerIconButton({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: (e: React.MouseEvent) => void;
  children: React.ReactNode;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      className="flex h-8 w-8 items-center justify-center rounded-md text-white/80 transition-all hover:bg-white/10 hover:text-white active:scale-95"
    >
      {children}
    </button>
  );
}

/** Zoom-pill flavour of the icon button — rounded-md to match the
 *  pill's new rounded-xl outer corner radius. */
function ZoomIconButton({
  title,
  onClick,
  children,
}: {
  title: string;
  onClick: (e: React.MouseEvent) => void;
  children: React.ReactNode;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      className="flex h-8 w-8 items-center justify-center rounded-md text-white/80 transition-all hover:bg-white/10 hover:text-white active:scale-95"
    >
      {children}
    </button>
  );
}

function ViewerNavButton({
  side,
  title,
  onClick,
}: {
  side: "left" | "right";
  title: string;
  onClick: (e: React.MouseEvent) => void;
}) {
  return (
    <button
      title={title}
      onClick={onClick}
      onMouseDown={(e) => e.stopPropagation()}
      className={`absolute top-1/2 -translate-y-1/2 ${side === "left" ? "left-4" : "right-4"} flex h-12 w-12 items-center justify-center rounded-xl border border-white/10 bg-bg-darkest/95 text-white/80 transition-all hover:bg-bg-darkest hover:text-white active:scale-95 backdrop-blur-md`}
    >
      <svg
        width="22"
        height="22"
        viewBox="0 0 24 24"
        fill="none"
        stroke="currentColor"
        strokeWidth="2.5"
        strokeLinecap="round"
        strokeLinejoin="round"
        style={{ transform: side === "left" ? undefined : "rotate(180deg)" }}
      >
        <polyline points="15 18 9 12 15 6" />
      </svg>
    </button>
  );
}
