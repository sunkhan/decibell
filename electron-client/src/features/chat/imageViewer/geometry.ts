// Viewer geometry. Everything is in stage pixels; `scale` is rendered
// pixels per natural pixel, so 1 is the picture's actual size and the
// zoom label reads true (a 4000 px photo fitted to a laptop says ~30%).

export interface Size {
  w: number;
  h: number;
}

export interface Area {
  left: number;
  top: number;
  w: number;
  h: number;
}

export interface View {
  scale: number;
  /// Offset of the picture's centre from the fit area's centre.
  x: number;
  y: number;
  /// Clockwise quarter turns, in degrees (0, 90, 180, 270, … unbounded so
  /// a turn always animates the short way round).
  rot: number;
}

/// Room kept free for the header and the filmstrip at fit (the picture
/// may go under them once zoomed). Narrow windows trade the side margin
/// for picture.
export function fitArea(stage: Size, strip: boolean): Area {
  const side = stage.w < 720 ? 16 : 80;
  const top = 72;
  const bottom = strip ? 104 : 40;
  return { left: side, top, w: Math.max(1, stage.w - side * 2), h: Math.max(1, stage.h - top - bottom) };
}

export function turned(rot: number): boolean {
  return Math.abs(rot) % 180 === 90;
}

/// Natural size as displayed (a quarter turn swaps the sides).
export function shown(nat: Size, rot: number): Size {
  return turned(rot) ? { w: nat.h, h: nat.w } : nat;
}

/// Fit scale: the whole picture inside the area, never above actual size
/// (a small image stays crisp instead of being blown up).
export function fitScale(nat: Size, rot: number, area: Area): number {
  const s = shown(nat, rot);
  return Math.min(1, area.w / s.w, area.h / s.h);
}

export const MAX_SCALE = 8;

/// A scale the picture can take: no smaller than fit, no larger than 800%.
export function clampScale(scale: number, nat: Size, rot: number, area: Area): number {
  return Math.max(fitScale(nat, rot, area), Math.min(MAX_SCALE, scale));
}
const LEVELS = [0.1, 0.25, 0.33, 0.5, 0.67, 1, 1.5, 2, 3, 4, 6, 8];

export function stepScale(current: number, fit: number, dir: 1 | -1): number {
  if (dir > 0) {
    const next = LEVELS.find((l) => l > current * 1.01 && l > fit * 1.01);
    return next ?? MAX_SCALE;
  }
  const prev = [...LEVELS].reverse().find((l) => l < current * 0.99);
  return prev === undefined || prev < fit ? fit : prev;
}

/// Keep the picture covering the stage on every side it overflows, and
/// centred in the fit area on every side it doesn't. Zooming out slides
/// a panned picture back to the middle and lands exactly on it at fit.
export function clampView(v: View, nat: Size, stage: Size, area: Area): View {
  const s = shown(nat, v.rot);
  const axis = (pan: number, d: number, centre: number, stageLen: number, areaLen: number) => {
    if (d <= areaLen + 0.5) return 0;
    const a = stageLen - centre - d / 2;
    const b = d / 2 - centre;
    return Math.max(Math.min(a, b), Math.min(Math.max(a, b), pan));
  };
  const x = axis(v.x, s.w * v.scale, area.left + area.w / 2, stage.w, area.w);
  const y = axis(v.y, s.h * v.scale, area.top + area.h / 2, stage.h, area.h);
  return x === v.x && y === v.y ? v : { ...v, x, y };
}

/// Zoom to `scale` keeping the stage point `p` (relative to the fit
/// area's centre) over the same spot of the picture. `scale` must already
/// be clamped: the pan follows the zoom that actually happens, so a wheel
/// pushed past 800% (or below fit) doesn't slide the picture.
export function zoomAt(v: View, scale: number, p: { x: number; y: number }): View {
  const k = scale / v.scale;
  return { ...v, scale, x: p.x - (p.x - v.x) * k, y: p.y - (p.y - v.y) * k };
}

/// Does the picture overflow the stage (pannable, minimap shown)?
export function overflows(v: View, nat: Size, stage: Size): boolean {
  const s = shown(nat, v.rot);
  return s.w * v.scale > stage.w + 1 || s.h * v.scale > stage.h + 1;
}
