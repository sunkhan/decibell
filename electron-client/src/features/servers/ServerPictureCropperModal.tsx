// Rectangular crop modal for community-server pictures.
//
// Mirrors AvatarCropperModal in structure but with a rectangular
// viewport matching the ServerBar tile's aspect ratio (see
// serverTileDimensions.ts). The user pans + zooms an image inside
// the cropper viewport; Save draws the visible region onto an
// OffscreenCanvas sized at 4× the tile (520×152 for the current
// 130×38 tile), JPEG-encodes at quality 0.85, ships bytes via the
// update_server_picture napi command → community validates owner +
// size → forwards to central → central broadcasts SERVER_PICTURE_
// CHANGED → tile updates on every member's ServerBar.
//
// Inline implementation, no external crop library — same pattern as
// AvatarCropperModal.

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";
import { TILE_ASPECT } from "./serverTileDimensions";

interface Props {
  serverId: string;
  file: File;
  onSave: () => void;
  onCancel: () => void;
}

// Cropper viewport — comfortable working size, ~3.7× the tile width.
const VIEWPORT_W = 480;
const VIEWPORT_H = Math.round(VIEWPORT_W / TILE_ASPECT); // 140 at 3.42 aspect
// Output canvas — 4× the tile width so renderers downscale rather
// than upscale on retina/HiDPI displays. The tile is 130×38; output
// is 520×152.
const OUTPUT_W = 520;
const OUTPUT_H = Math.round(OUTPUT_W / TILE_ASPECT); // 152
const JPEG_QUALITY = 0.85;

export function ServerPictureCropperModal({
  serverId,
  file,
  onSave,
  onCancel,
}: Props) {
  const imgRef = useRef<HTMLImageElement | null>(null);
  const canvasRef = useRef<HTMLCanvasElement | null>(null);
  const [imgLoaded, setImgLoaded] = useState(false);
  const [pos, setPos] = useState({ x: 0, y: 0 });
  const [scale, setScale] = useState(1);
  const [dragging, setDragging] = useState(false);
  const dragStart = useRef<{ x: number; y: number } | null>(null);
  const [uploading, setUploading] = useState(false);
  const [error, setError] = useState<string | null>(null);

  // Load the picked file as an HTMLImageElement; centre + fit-to-cover
  // it inside the viewport on first paint. Revoke deferred to
  // onload/onerror so React 18 StrictMode's double-invoke can't kill
  // the blob URL mid-load.
  useEffect(() => {
    let cancelled = false;
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      if (cancelled) {
        URL.revokeObjectURL(url);
        return;
      }
      imgRef.current = img;
      // Fit-to-cover: pick the scale that ensures the image covers
      // the entire (rectangular) viewport on both axes.
      const initial = Math.max(
        VIEWPORT_W / img.width,
        VIEWPORT_H / img.height,
      );
      setScale(initial);
      setPos({
        x: (VIEWPORT_W - img.width * initial) / 2,
        y: (VIEWPORT_H - img.height * initial) / 2,
      });
      setImgLoaded(true);
      URL.revokeObjectURL(url);
    };
    img.onerror = () => {
      URL.revokeObjectURL(url);
      if (!cancelled) setError("Couldn't load that file as an image.");
    };
    img.src = url;
    return () => {
      cancelled = true;
    };
  }, [file]);

  // Repaint preview canvas on every state change.
  useEffect(() => {
    if (!imgLoaded || !imgRef.current || !canvasRef.current) return;
    const ctx = canvasRef.current.getContext("2d");
    if (!ctx) return;
    ctx.fillStyle = "#0a0a0a";
    ctx.fillRect(0, 0, VIEWPORT_W, VIEWPORT_H);
    ctx.drawImage(
      imgRef.current,
      pos.x,
      pos.y,
      imgRef.current.width * scale,
      imgRef.current.height * scale,
    );
  }, [imgLoaded, pos, scale]);

  // Bind wheel manually with passive:false so preventDefault works.
  useEffect(() => {
    const el = canvasRef.current;
    if (!el) return;
    const onWheel = (e: WheelEvent) => {
      e.preventDefault();
      const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
      setScale((s) => Math.max(0.1, Math.min(10, s * factor)));
    };
    el.addEventListener("wheel", onWheel, { passive: false });
    return () => el.removeEventListener("wheel", onWheel);
  }, []);

  const onMouseDown = (e: React.MouseEvent) => {
    setDragging(true);
    dragStart.current = { x: e.clientX - pos.x, y: e.clientY - pos.y };
  };
  const onMouseMove = (e: React.MouseEvent) => {
    if (!dragging || !dragStart.current) return;
    setPos({
      x: e.clientX - dragStart.current.x,
      y: e.clientY - dragStart.current.y,
    });
  };
  const onMouseUp = () => {
    setDragging(false);
    dragStart.current = null;
  };

  const handleSave = async () => {
    if (!imgLoaded || !imgRef.current) return;
    setUploading(true);
    setError(null);
    try {
      // Draw the current viewport view onto the output canvas,
      // scaling pos + size by OUTPUT_W/VIEWPORT_W (same ratio on both
      // axes since both share TILE_ASPECT).
      const out = new OffscreenCanvas(OUTPUT_W, OUTPUT_H);
      const octx = out.getContext("2d");
      if (!octx) throw new Error("OffscreenCanvas 2d context unavailable");
      const ratio = OUTPUT_W / VIEWPORT_W;
      octx.drawImage(
        imgRef.current,
        pos.x * ratio,
        pos.y * ratio,
        imgRef.current.width * scale * ratio,
        imgRef.current.height * scale * ratio,
      );
      const blob = await out.convertToBlob({
        type: "image/jpeg",
        quality: JPEG_QUALITY,
      });
      const buf = await blob.arrayBuffer();
      // Server-side cap is 1 MB; cropper output at 520×152 JPEG q0.85
      // is typically well under 30 KB so this never triggers.
      await invoke("update_server_picture", {
        serverId,
        data: new Uint8Array(buf),
      });
      onSave();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setUploading(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-[60] flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && !uploading && onCancel()}
    >
      <div className="rounded-2xl border border-border bg-bg-dark p-6 shadow-[0_24px_80px_rgba(0,0,0,0.6)]">
        <p className="mb-1 font-display text-[15px] font-semibold text-text-primary">
          Crop your server picture
        </p>
        <p className="mb-3 text-[12px] text-text-muted">
          Drag to position. Scroll to zoom. The crop will fill the
          rectangle that appears on the server bar.
        </p>
        <canvas
          ref={canvasRef}
          width={VIEWPORT_W}
          height={VIEWPORT_H}
          onMouseDown={onMouseDown}
          onMouseMove={onMouseMove}
          onMouseUp={onMouseUp}
          onMouseLeave={onMouseUp}
          className={`rounded-md bg-bg-darkest ${dragging ? "cursor-grabbing" : "cursor-grab"}`}
          style={{ width: VIEWPORT_W, height: VIEWPORT_H }}
        />
        {error && (
          <p
            className="mt-3 text-[12px] text-error"
            style={{ maxWidth: VIEWPORT_W }}
          >
            {error}
          </p>
        )}
        <div className="mt-4 flex justify-end gap-2">
          <button
            onClick={onCancel}
            disabled={uploading}
            className="rounded-md border border-border px-4 py-2 text-[13px] text-text-secondary hover:bg-bg-light disabled:opacity-50"
          >
            Cancel
          </button>
          <button
            onClick={handleSave}
            disabled={uploading || !imgLoaded}
            className="rounded-md bg-accent px-4 py-2 text-[13px] font-semibold text-white hover:bg-accent-hover disabled:opacity-50"
          >
            {uploading ? "Saving…" : "Save"}
          </button>
        </div>
      </div>
    </div>,
    document.body,
  );
}
