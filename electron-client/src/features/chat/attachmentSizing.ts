// Shared image/video sizing logic for attachment previews. Ported
// from tauri-client and kept identical so the visual size + layout
// match across both apps.
//
// Image dimensions scale as the **square root** of chat-view dimensions.
// A linear "75% of chat width" looks great on small/medium chats but
// produces unbearably large previews in fullscreen layouts; sqrt grows
// monotonically yet sub-linearly, so the curve flattens out as chats
// widen — without resorting to a hard cap. The coefficients are tuned
// so a typical small panel (~500 × 600) renders images in the same
// neighbourhood (~400 × 390) the previous linear scaling produced.
// Floor reserves keep avatar + bubble padding intact on narrow panels.

import type { Attachment } from "../../types";

// Pre-measurement / unknown-dimensions fallback caps.
export const PREVIEW_FALLBACK_MAX_W = 400;
export const PREVIEW_FALLBACK_MAX_H = 360;
export const PREVIEW_FALLBACK_W = 260;
export const PREVIEW_FALLBACK_H = 180;

const HORIZONTAL_BUBBLE_RESERVE_MIN = 80;
const VERTICAL_BUBBLE_RESERVE_MIN = 60;
const IMAGE_WIDTH_SQRT_COEFF = 18;
const IMAGE_HEIGHT_SQRT_COEFF = 16;

export function maxImageWidth(viewWidth: number): number {
  return Math.max(
    120,
    Math.min(
      IMAGE_WIDTH_SQRT_COEFF * Math.sqrt(viewWidth),
      viewWidth - HORIZONTAL_BUBBLE_RESERVE_MIN,
    ),
  );
}

export function maxImageHeight(viewHeight: number): number {
  return Math.max(
    120,
    Math.min(
      IMAGE_HEIGHT_SQRT_COEFF * Math.sqrt(viewHeight),
      viewHeight - VERTICAL_BUBBLE_RESERVE_MIN,
    ),
  );
}

export interface ChatViewSize {
  width: number;
  height: number;
}

/// Compute the pixel box to reserve for a single image/video preview.
/// Scales down so the image fits within the sqrt-derived caps with
/// aspect ratio preserved. Small images render at natural size — we
/// never upscale.
export function reserveBox(
  attachment: Attachment,
  viewSize: ChatViewSize | null,
): { width: number; height: number; known: boolean } {
  const w = attachment.width;
  const h = attachment.height;
  if (w <= 0 || h <= 0) {
    return { width: PREVIEW_FALLBACK_W, height: PREVIEW_FALLBACK_H, known: false };
  }
  const maxW = viewSize ? maxImageWidth(viewSize.width) : PREVIEW_FALLBACK_MAX_W;
  const maxH = viewSize ? maxImageHeight(viewSize.height) : PREVIEW_FALLBACK_MAX_H;
  const scale = Math.min(1, maxW / w, maxH / h);
  return {
    width: Math.max(1, Math.round(w * scale)),
    height: Math.max(1, Math.round(h * scale)),
    known: true,
  };
}

// ── Multi-attachment grid layout (Discord/tauri parity) ───────────
// Row counts for n attachments. Sum of each entry equals n. Picked to
// keep cells from getting too narrow at high counts while matching
// what readers visually expect from chat-attachment grids.

export function gridRowCounts(n: number): number[] {
  switch (n) {
    case 2: return [2];
    case 3: return [3];
    case 4: return [2, 2];
    case 5: return [2, 3];
    case 6: return [3, 3];
    case 7: return [1, 3, 3];
    case 8: return [2, 3, 3];
    case 9: return [3, 3, 3];
    case 10: return [1, 3, 3, 3];
    default: return [n];
  }
}

export const GRID_GAP_PX = 4;
export const GRID_ROW_HEIGHT_PX = 180;
export const GRID_MAX_WIDTH_PX = 540;
export const GRID_MIN_WIDTH_PX = 320;
