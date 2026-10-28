// Picture editor geometry. Everything is in frame pixels: the crop frame's
// top-left is the origin, `scale` is frame pixels per natural pixel, and
// (x, y) is where the picture's top-left sits. The exported picture is the
// frame, so these numbers are the crop, scaled by output / frame.

export interface Size {
  w: number;
  h: number;
}

export interface View {
  scale: number;
  x: number;
  y: number;
}

/// The smallest scale that leaves no gap in the frame.
export function coverScale(nat: Size, frame: Size): number {
  return Math.max(frame.w / nat.w, frame.h / nat.h);
}

/// The scale that shows the whole picture inside the frame.
export function containScale(nat: Size, frame: Size): number {
  return Math.min(frame.w / nat.w, frame.h / nat.h);
}

/// How far in the slider and the wheel go: 8× past cover, or 1:1 with the
/// export if that is further (a small picture still zooms to its pixels).
export function maxScale(nat: Size, frame: Size, output: Size): number {
  return Math.max(coverScale(nat, frame) * 8, frame.w / output.w);
}

export function minScale(nat: Size, frame: Size, fit: boolean): number {
  return fit ? Math.min(containScale(nat, frame), coverScale(nat, frame)) : coverScale(nat, frame);
}

/// The range each axis may take: a picture wider than the frame keeps both
/// edges outside it; a narrower one stays inside it (a fitted logo can sit
/// left of centre, never half out of the frame).
export function bounds(nat: Size, frame: Size, scale: number) {
  const axis = (len: number, frameLen: number) => {
    const d = len * scale;
    return d >= frameLen ? { lo: frameLen - d, hi: 0 } : { lo: 0, hi: frameLen - d };
  };
  return { x: axis(nat.w, frame.w), y: axis(nat.h, frame.h) };
}

export function clampView(v: View, nat: Size, frame: Size, range: { min: number; max: number }): View {
  const scale = Math.max(range.min, Math.min(range.max, v.scale));
  const b = bounds(nat, frame, scale);
  return {
    scale,
    x: Math.max(b.x.lo, Math.min(b.x.hi, v.x)),
    y: Math.max(b.y.lo, Math.min(b.y.hi, v.y)),
  };
}

/// A drag past an edge moves the picture less and less the further it
/// goes (the overshoot approaches a third of the frame), then springs back
/// on release.
export function rubber(value: number, lo: number, hi: number, frameLen: number): number {
  const give = (over: number) => (over * frameLen * 0.33) / (over + frameLen * 0.33);
  if (value < lo) return lo - give(lo - value);
  if (value > hi) return hi + give(value - hi);
  return value;
}

export function rubberView(v: View, nat: Size, frame: Size): View {
  const b = bounds(nat, frame, v.scale);
  return { scale: v.scale, x: rubber(v.x, b.x.lo, b.x.hi, frame.w), y: rubber(v.y, b.y.lo, b.y.hi, frame.h) };
}

/// Zoom to `scale`, keeping the picture point under `at` (frame pixels)
/// where it is.
export function zoomAt(v: View, scale: number, at: { x: number; y: number }): View {
  const k = scale / v.scale;
  return { scale, x: at.x - (at.x - v.x) * k, y: at.y - (at.y - v.y) * k };
}

/// The starting crop: cover, centred.
export function initialView(nat: Size, frame: Size): View {
  const scale = coverScale(nat, frame);
  return { scale, x: (frame.w - nat.w * scale) / 2, y: (frame.h - nat.h * scale) / 2 };
}

/// The slider is logarithmic, so every step of it zooms by the same factor.
export function toSlider(scale: number, range: { min: number; max: number }): number {
  if (range.max <= range.min) return 0;
  return Math.log(scale / range.min) / Math.log(range.max / range.min);
}

export function fromSlider(t: number, range: { min: number; max: number }): number {
  return range.min * Math.pow(range.max / range.min, Math.max(0, Math.min(1, t)));
}

/// The frame no longer filled edge to edge (a fill shows behind it).
export function leavesGap(v: View, nat: Size, frame: Size): boolean {
  const e = 0.5;
  return v.x > e || v.y > e || v.x + nat.w * v.scale < frame.w - e || v.y + nat.h * v.scale < frame.h - e;
}

export function sameView(a: View, b: View): boolean {
  return Math.abs(a.scale - b.scale) < a.scale * 1e-4 && Math.abs(a.x - b.x) < 0.25 && Math.abs(a.y - b.y) < 0.25;
}
