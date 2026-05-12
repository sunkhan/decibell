// Square-crop modal for profile pictures.
//
// Flow: AccountTab picks a File → opens this modal → user pans + zooms
// the image inside a 320×320 viewport → Save draws the visible region
// onto a 256×256 OffscreenCanvas, JPEG-encodes at quality 0.85, ships
// bytes via the upload_avatar napi command → server validates magic +
// 200 KB cap → stores → broadcasts AvatarChanged → avatarStore on this
// renderer invalidates → UserAvatar across the app re-renders.
//
// Inline implementation, no external crop library (~200 LOC).

import { useEffect, useRef, useState } from "react";
import { createPortal } from "react-dom";
import { invoke } from "../../lib/ipc";

interface Props {
  file: File;
  onSave: () => void;
  onCancel: () => void;
}

const VIEWPORT = 320;
const OUTPUT = 256;
const JPEG_QUALITY = 0.85;

export function AvatarCropperModal({ file, onSave, onCancel }: Props) {
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
  // it inside the viewport on first paint.
  useEffect(() => {
    const url = URL.createObjectURL(file);
    const img = new Image();
    img.onload = () => {
      imgRef.current = img;
      // Cover: scale so the shorter edge exactly fills the viewport;
      // the longer edge overflows and the user can pan to choose
      // which region to keep.
      const initial = Math.max(
        VIEWPORT / img.width,
        VIEWPORT / img.height,
      );
      setScale(initial);
      setPos({
        x: (VIEWPORT - img.width * initial) / 2,
        y: (VIEWPORT - img.height * initial) / 2,
      });
      setImgLoaded(true);
    };
    img.onerror = () => setError("Couldn't load that file as an image.");
    img.src = url;
    return () => URL.revokeObjectURL(url);
  }, [file]);

  // Repaint preview canvas on every state change.
  useEffect(() => {
    if (!imgLoaded || !imgRef.current || !canvasRef.current) return;
    const ctx = canvasRef.current.getContext("2d");
    if (!ctx) return;
    ctx.fillStyle = "#0a0a0a";
    ctx.fillRect(0, 0, VIEWPORT, VIEWPORT);
    ctx.drawImage(
      imgRef.current,
      pos.x,
      pos.y,
      imgRef.current.width * scale,
      imgRef.current.height * scale,
    );
  }, [imgLoaded, pos, scale]);

  const onWheel = (e: React.WheelEvent) => {
    e.preventDefault();
    const factor = e.deltaY < 0 ? 1.1 : 1 / 1.1;
    setScale((s) => Math.max(0.1, Math.min(10, s * factor)));
  };
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
      // Draw the current viewport view onto a 256×256 OffscreenCanvas,
      // scaling pos + size by OUTPUT/VIEWPORT so the produced JPEG
      // matches exactly what the user sees in the modal.
      const out = new OffscreenCanvas(OUTPUT, OUTPUT);
      const octx = out.getContext("2d");
      if (!octx) throw new Error("OffscreenCanvas 2d context unavailable");
      const ratio = OUTPUT / VIEWPORT;
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
      const result = (await invoke("upload_avatar", {
        jpeg: new Uint8Array(buf),
      })) as { success: boolean; message: string; version: string };
      if (!result.success) {
        throw new Error(result.message || "Upload failed");
      }
      onSave();
    } catch (e) {
      setError(String(e instanceof Error ? e.message : e));
    } finally {
      setUploading(false);
    }
  };

  return createPortal(
    <div
      className="fixed inset-0 z-50 flex items-center justify-center bg-black/60"
      onClick={(e) => e.target === e.currentTarget && !uploading && onCancel()}
    >
      <div className="rounded-2xl border border-border bg-bg-dark p-6 shadow-[0_24px_80px_rgba(0,0,0,0.6)]">
        <p className="mb-1 font-display text-[15px] font-semibold text-text-primary">
          Crop your picture
        </p>
        <p className="mb-3 text-[12px] text-text-muted">
          Drag to position. Scroll to zoom.
        </p>
        <canvas
          ref={canvasRef}
          width={VIEWPORT}
          height={VIEWPORT}
          onWheel={onWheel}
          onMouseDown={onMouseDown}
          onMouseMove={onMouseMove}
          onMouseUp={onMouseUp}
          onMouseLeave={onMouseUp}
          className={`rounded-md bg-bg-darkest ${dragging ? "cursor-grabbing" : "cursor-grab"}`}
          style={{ width: VIEWPORT, height: VIEWPORT }}
        />
        {error && <p className="mt-3 max-w-[320px] text-[12px] text-error">{error}</p>}
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
