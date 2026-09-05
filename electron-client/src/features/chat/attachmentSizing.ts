// Shared image/video sizing logic for attachment previews. Ported
// from tauri-client and kept identical so the visual size + layout
// match across both apps.
//
// Image dimensions scale as the **square root** of chat-view dimensions,
// under a hard ceiling. A linear "75% of chat width" looks great on
// small/medium chats but produces unbearably large previews in fullscreen
// layouts; sqrt grows monotonically yet sub-linearly, so the curve
// flattens out as chats widen. The coefficients are tuned so a typical
// small panel (~500 × 600) renders images in the same neighbourhood
// (~400 × 390) the previous linear scaling produced. Floor reserves keep
// avatar + bubble padding intact on narrow panels.
//
// The ceiling is Discord's inline-media box (550 × 350). Without it a
// maximised 1080p window put the curve at ~790 × 530: a GIF or screenshot
// filled a third of the screen, and an attachment whose largest thumbnail
// was smaller than that box was blown up past its own pixels. Everything
// wider than ~930px now gets the same size; only cramped panels shrink.

import type { Attachment } from "../../types";

/// Hard ceiling for a single inline image/video preview, whatever the
/// chat panel's size. Discord's numbers; a 16:9 frame lands at 550 × 309,
/// a square one at 350 × 350.
export const IMAGE_MAX_W = 550;
export const IMAGE_MAX_H = 350;
/// GIFs get a tighter ceiling (owner call, 2026-09-05): they loop and pull
/// the eye, so at the still-image size one animation owns the channel. 80%
/// of the image box; a 16:9 GIF lands at 440 × 248. Picker GIFs (KLIPY
/// "hd") are ~500 px wide natively, so this trims them by about a tenth.
export const GIF_MAX_W = 440;
export const GIF_MAX_H = 280;

/// Which ceiling a preview gets. Attachments decide by mime, link previews
/// by the unfurler's `gif` flag.
export type MediaKind = "image" | "gif";
export function mediaKindOf(mime: string): MediaKind {
  return mime === "image/gif" ? "gif" : "image";
}

// Pre-measurement / unknown-dimensions fallback caps.
export const PREVIEW_FALLBACK_MAX_W = IMAGE_MAX_W;
export const PREVIEW_FALLBACK_MAX_H = IMAGE_MAX_H;
export const PREVIEW_FALLBACK_W = 260;
export const PREVIEW_FALLBACK_H = 180;

const HORIZONTAL_BUBBLE_RESERVE_MIN = 80;
const VERTICAL_BUBBLE_RESERVE_MIN = 60;
const IMAGE_WIDTH_SQRT_COEFF = 18;
const IMAGE_HEIGHT_SQRT_COEFF = 16;

export function maxImageWidth(viewWidth: number, kind: MediaKind = "image"): number {
  return Math.max(
    120,
    Math.min(
      kind === "gif" ? GIF_MAX_W : IMAGE_MAX_W,
      IMAGE_WIDTH_SQRT_COEFF * Math.sqrt(viewWidth),
      viewWidth - HORIZONTAL_BUBBLE_RESERVE_MIN,
    ),
  );
}

export function maxImageHeight(viewHeight: number, kind: MediaKind = "image"): number {
  return Math.max(
    120,
    Math.min(
      kind === "gif" ? GIF_MAX_H : IMAGE_MAX_H,
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
  return reserveBoxFor(attachment.width, attachment.height, viewSize, mediaKindOf(attachment.mime));
}

/// Same box from bare dimensions — link-preview images carry no
/// Attachment, only the size the page declared (or the probe found).
export function reserveBoxFor(
  w: number,
  h: number,
  viewSize: ChatViewSize | null,
  kind: MediaKind = "image",
): { width: number; height: number; known: boolean } {
  if (w <= 0 || h <= 0) {
    return { width: PREVIEW_FALLBACK_W, height: PREVIEW_FALLBACK_H, known: false };
  }
  const maxW = viewSize
    ? maxImageWidth(viewSize.width, kind)
    : kind === "gif" ? GIF_MAX_W : PREVIEW_FALLBACK_MAX_W;
  const maxH = viewSize
    ? maxImageHeight(viewSize.height, kind)
    : kind === "gif" ? GIF_MAX_H : PREVIEW_FALLBACK_MAX_H;
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
