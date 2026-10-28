// Reading a picked picture and rendering the crop.
//
// The editor shows the file itself (an <img> moved by transforms), so the
// stage is as sharp as the screen at any zoom. The export redraws the same
// crop at the output size with high-quality resampling: a 4000 px photo
// shrunk to 256 in one bilinear step (what a plain drawImage does) aliases.

import type { Size, View } from "./geometry";

/// What's behind the picture where it doesn't fill the frame (zoomed out
/// past cover, or transparent pixels): its own colours blurred, or a flat
/// colour. Always painted into the export, so everyone sees the same thing.
export type Fill = { kind: "blur" } | { kind: "color"; color: string };

export interface Picture {
  url: string;
  img: HTMLImageElement;
  nat: Size;
  /// Has transparent pixels (a logo): a fill shows through them.
  alpha: boolean;
  /// GIF: only the first frame survives the JPEG.
  animated: boolean;
  /// Fill swatches: the picture's main colour and a deep shade of it.
  palette: [string, string];
  name: string;
}

/// Above this the decode alone takes hundreds of MB; no profile picture needs it.
const MAX_PIXELS = 80_000_000;

export class PictureError extends Error {}

export async function readPicture(file: File): Promise<Picture> {
  const url = URL.createObjectURL(file);
  try {
    const img = new Image();
    img.src = url;
    try {
      await img.decode();
    } catch {
      throw new PictureError(
        /\.(heic|heif)$/i.test(file.name)
          ? "HEIC photos can't be opened here yet. Export it as a JPEG first."
          : "This file isn't a picture Decibell can open.",
      );
    }
    const nat = { w: img.naturalWidth, h: img.naturalHeight };
    if (!nat.w || !nat.h) throw new PictureError("This file isn't a picture Decibell can open.");
    if (nat.w * nat.h > MAX_PIXELS) throw new PictureError("This picture is too large to edit (over 80 megapixels).");
    const { alpha, palette } = analyse(img, nat);
    return { url, img, nat, alpha, palette, animated: file.type === "image/gif", name: file.name };
  } catch (e) {
    URL.revokeObjectURL(url);
    throw e;
  }
}

/// Transparency and the main colour, from a 48 px copy.
function analyse(img: HTMLImageElement, nat: Size): { alpha: boolean; palette: [string, string] } {
  const n = 48;
  const w = nat.w >= nat.h ? n : Math.max(1, Math.round((n * nat.w) / nat.h));
  const h = nat.h > nat.w ? n : Math.max(1, Math.round((n * nat.h) / nat.w));
  const c = new OffscreenCanvas(w, h);
  const ctx = c.getContext("2d", { willReadFrequently: true });
  if (!ctx) return { alpha: false, palette: ["#808080", "#1a1a1a"] };
  ctx.drawImage(img, 0, 0, w, h);
  const px = ctx.getImageData(0, 0, w, h).data;
  // Most common 4-bit bucket among opaque pixels, saturated ones counted
  // double so a grey border doesn't win over the subject.
  const votes = new Map<number, { n: number; r: number; g: number; b: number }>();
  let alpha = false;
  for (let i = 0; i < px.length; i += 4) {
    const [r, g, b, a] = [px[i], px[i + 1], px[i + 2], px[i + 3]];
    if (a < 250) alpha = true;
    if (a < 128) continue;
    const key = ((r >> 4) << 8) | ((g >> 4) << 4) | (b >> 4);
    const sat = Math.max(r, g, b) - Math.min(r, g, b);
    const weight = sat > 48 ? 2 : 1;
    const v = votes.get(key) ?? { n: 0, r: 0, g: 0, b: 0 };
    v.n += weight;
    v.r += r * weight;
    v.g += g * weight;
    v.b += b * weight;
    votes.set(key, v);
  }
  let best = { n: 0, r: 128, g: 128, b: 128 };
  for (const v of votes.values()) if (v.n > best.n) best = v;
  const rgb = best.n ? [best.r / best.n, best.g / best.n, best.b / best.n] : [128, 128, 128];
  return { alpha, palette: [hex(rgb), hex(deep(rgb))] };
}

/// The same hue, dark enough to sit behind a logo.
function deep([r, g, b]: number[]): number[] {
  const max = Math.max(r, g, b) || 1;
  const k = 46 / max;
  return [r * k + 10, g * k + 10, b * k + 12];
}

function hex(rgb: number[]): string {
  return "#" + rgb.map((v) => Math.round(Math.max(0, Math.min(255, v))).toString(16).padStart(2, "0")).join("");
}

/// Blur radius of the "blur" fill, as a share of the frame's width.
export const BLUR_SHARE = 0.06;

/// The crop as a JPEG of `out` pixels. Steps the quality down until it
/// fits under the server's 1 MB cap (a noisy 880 px banner can approach it).
export async function renderCrop(p: Picture, view: View, frame: Size, out: Size, fill: Fill): Promise<Uint8Array> {
  const canvas = new OffscreenCanvas(out.w, out.h);
  const ctx = canvas.getContext("2d");
  if (!ctx) throw new Error("Canvas unavailable");
  const k = out.w / frame.w;
  // Under everything: JPEG has no transparency, and an unpainted pixel
  // would turn black.
  ctx.fillStyle = fill.kind === "color" ? fill.color : p.palette[1];
  ctx.fillRect(0, 0, out.w, out.h);
  if (fill.kind === "blur") {
    // The picture covering the frame, blurred; drawn 10% large so the
    // blur's faded edge falls outside.
    const s = Math.max(out.w / p.nat.w, out.h / p.nat.h) * 1.1;
    ctx.filter = `blur(${Math.round(out.w * BLUR_SHARE)}px) saturate(1.2)`;
    ctx.drawImage(p.img, (out.w - p.nat.w * s) / 2, (out.h - p.nat.h * s) / 2, p.nat.w * s, p.nat.h * s);
    ctx.filter = "none";
  }
  // Only the part of the picture inside the frame, resampled once by the
  // browser's high-quality scaler.
  const s = view.scale * k;
  const dx0 = Math.max(0, view.x * k);
  const dy0 = Math.max(0, view.y * k);
  const dx1 = Math.min(out.w, (view.x + p.nat.w * view.scale) * k);
  const dy1 = Math.min(out.h, (view.y + p.nat.h * view.scale) * k);
  const dw = Math.round(dx1 - dx0);
  const dh = Math.round(dy1 - dy0);
  if (dw > 0 && dh > 0) {
    const sx = (dx0 - view.x * k) / s;
    const sy = (dy0 - view.y * k) / s;
    const bitmap = await createImageBitmap(p.img, sx, sy, (dx1 - dx0) / s, (dy1 - dy0) / s, {
      resizeWidth: dw,
      resizeHeight: dh,
      resizeQuality: "high",
    });
    ctx.drawImage(bitmap, Math.round(dx0), Math.round(dy0));
    bitmap.close();
  }
  for (const quality of [0.9, 0.82, 0.74, 0.66]) {
    const blob = await canvas.convertToBlob({ type: "image/jpeg", quality });
    if (blob.size <= 1000 * 1024 || quality === 0.66) return new Uint8Array(await blob.arrayBuffer());
  }
  throw new Error("unreachable");
}

/// A picture file, from a file input or a drop. Anything the browser
/// might decode gets a try; the editor says so if it can't.
export function pictureFromList(list: FileList | null | undefined): File | null {
  if (!list) return null;
  for (const f of Array.from(list)) {
    if (f.type.startsWith("image/") || /\.(jpe?g|png|webp|gif|avif|bmp|heic|heif)$/i.test(f.name)) return f;
  }
  return null;
}

export const PICTURE_ACCEPT = "image/png,image/jpeg,image/webp,image/gif,image/avif,image/bmp";
