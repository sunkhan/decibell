//! Mouse-pointer compositing for the DXGI Desktop Duplication capture
//! path (capture_dxgi.rs). Desktop Duplication delivers the desktop
//! image *without* the hardware cursor — the pointer arrives separately
//! as a shape bitmap + position — so the capture thread blends it onto
//! the frame itself, the same way OBS and Microsoft's duplication
//! sample do.
//!
//! Pure pixel math, no OS types: the module is cross-platform so
//! `cargo test --lib` exercises it on the Linux dev box even though its
//! only caller is Windows-only.

// The only consumer is cfg(target_os = "windows"); keep the module
// warning-free on the other platforms where only the tests see it.
#![allow(dead_code)]

/// DXGI_OUTDUPL_POINTER_SHAPE_TYPE values.
pub const SHAPE_MONOCHROME: u32 = 1;
pub const SHAPE_COLOR: u32 = 2;
pub const SHAPE_MASKED_COLOR: u32 = 4;

/// A cached cursor shape as delivered by GetFramePointerShape.
pub struct CursorImage {
    pub shape_type: u32,
    /// Raw shape buffer. COLOR / MASKED_COLOR: BGRA rows. MONOCHROME:
    /// 1bpp AND mask rows followed by 1bpp XOR mask rows.
    pub data: Vec<u8>,
    /// Bytes per shape row.
    pub pitch: usize,
    pub width: usize,
    /// Height of the *visual* cursor. For MONOCHROME shapes DXGI
    /// reports Height as 2× this (AND + XOR masks stacked); the caller
    /// halves it before constructing the CursorImage.
    pub visual_height: usize,
}

/// Blend the cursor onto a BGRA patch copied out of the frame.
///
/// `patch` is `patch_h` rows of `patch_pitch` bytes, each row holding
/// `patch_w` BGRA pixels; it corresponds to the clipped intersection of
/// the cursor rectangle with the frame. `(src_x, src_y)` is the texel
/// of the cursor image that lands on patch (0,0) — non-zero when the
/// cursor is partially off-screen.
pub fn composite_cursor(
    patch: &mut [u8],
    patch_pitch: usize,
    patch_w: usize,
    patch_h: usize,
    cursor: &CursorImage,
    src_x: usize,
    src_y: usize,
) {
    match cursor.shape_type {
        SHAPE_COLOR => blend_color(patch, patch_pitch, patch_w, patch_h, cursor, src_x, src_y),
        SHAPE_MONOCHROME => {
            blend_monochrome(patch, patch_pitch, patch_w, patch_h, cursor, src_x, src_y)
        }
        SHAPE_MASKED_COLOR => {
            blend_masked_color(patch, patch_pitch, patch_w, patch_h, cursor, src_x, src_y)
        }
        _ => {}
    }
}

/// 32bpp BGRA cursor with straight alpha: classic src-over.
fn blend_color(
    patch: &mut [u8],
    patch_pitch: usize,
    patch_w: usize,
    patch_h: usize,
    cursor: &CursorImage,
    src_x: usize,
    src_y: usize,
) {
    for y in 0..patch_h {
        let cy = src_y + y;
        if cy >= cursor.visual_height {
            break;
        }
        for x in 0..patch_w {
            let cx = src_x + x;
            if cx >= cursor.width {
                break;
            }
            let s = cy * cursor.pitch + cx * 4;
            if s + 3 >= cursor.data.len() {
                return;
            }
            let d = y * patch_pitch + x * 4;
            if d + 3 >= patch.len() {
                return;
            }
            let a = cursor.data[s + 3] as u32;
            if a == 0 {
                continue;
            }
            for c in 0..3 {
                let sv = cursor.data[s + c] as u32;
                let dv = patch[d + c] as u32;
                patch[d + c] = ((sv * a + dv * (255 - a) + 127) / 255) as u8;
            }
            patch[d + 3] = 0xFF;
        }
    }
}

/// 1bpp AND + XOR masks (I-beam and friends):
/// dst = (dst AND and_bit) XOR xor_bit, per channel.
fn blend_monochrome(
    patch: &mut [u8],
    patch_pitch: usize,
    patch_w: usize,
    patch_h: usize,
    cursor: &CursorImage,
    src_x: usize,
    src_y: usize,
) {
    let xor_base = cursor.visual_height * cursor.pitch;
    for y in 0..patch_h {
        let cy = src_y + y;
        if cy >= cursor.visual_height {
            break;
        }
        for x in 0..patch_w {
            let cx = src_x + x;
            if cx >= cursor.width {
                break;
            }
            let byte = cx / 8;
            let bit = 7 - (cx % 8);
            let and_idx = cy * cursor.pitch + byte;
            let xor_idx = xor_base + cy * cursor.pitch + byte;
            if xor_idx >= cursor.data.len() {
                return;
            }
            let and_mask = if (cursor.data[and_idx] >> bit) & 1 == 1 { 0xFFu8 } else { 0x00 };
            let xor_mask = if (cursor.data[xor_idx] >> bit) & 1 == 1 { 0xFFu8 } else { 0x00 };
            let d = y * patch_pitch + x * 4;
            if d + 3 >= patch.len() {
                return;
            }
            for c in 0..3 {
                patch[d + c] = (patch[d + c] & and_mask) ^ xor_mask;
            }
            patch[d + 3] = 0xFF;
        }
    }
}

/// 32bpp masked color: alpha byte 0 ⇒ replace, 0xFF ⇒ XOR with dst.
fn blend_masked_color(
    patch: &mut [u8],
    patch_pitch: usize,
    patch_w: usize,
    patch_h: usize,
    cursor: &CursorImage,
    src_x: usize,
    src_y: usize,
) {
    for y in 0..patch_h {
        let cy = src_y + y;
        if cy >= cursor.visual_height {
            break;
        }
        for x in 0..patch_w {
            let cx = src_x + x;
            if cx >= cursor.width {
                break;
            }
            let s = cy * cursor.pitch + cx * 4;
            if s + 3 >= cursor.data.len() {
                return;
            }
            let d = y * patch_pitch + x * 4;
            if d + 3 >= patch.len() {
                return;
            }
            if cursor.data[s + 3] == 0 {
                for c in 0..3 {
                    patch[d + c] = cursor.data[s + c];
                }
            } else {
                for c in 0..3 {
                    patch[d + c] ^= cursor.data[s + c];
                }
            }
            patch[d + 3] = 0xFF;
        }
    }
}

/// Clip a cursor rectangle at (`pos_x`, `pos_y`) sized `cw`×`ch`
/// against a `fw`×`fh` frame. Returns the frame-space patch rect
/// (x, y, w, h) plus the cursor-space offset (src_x, src_y), or None
/// when fully off-screen.
pub fn clip_cursor_rect(
    pos_x: i32,
    pos_y: i32,
    cw: usize,
    ch: usize,
    fw: usize,
    fh: usize,
) -> Option<(u32, u32, usize, usize, usize, usize)> {
    let x0 = pos_x.max(0) as i64;
    let y0 = pos_y.max(0) as i64;
    let x1 = (pos_x as i64 + cw as i64).min(fw as i64);
    let y1 = (pos_y as i64 + ch as i64).min(fh as i64);
    if x0 >= x1 || y0 >= y1 {
        return None;
    }
    let src_x = (x0 - pos_x as i64) as usize;
    let src_y = (y0 - pos_y as i64) as usize;
    Some((
        x0 as u32,
        y0 as u32,
        (x1 - x0) as usize,
        (y1 - y0) as usize,
        src_x,
        src_y,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn patch(w: usize, h: usize, fill: [u8; 4]) -> Vec<u8> {
        let mut p = vec![0u8; w * h * 4];
        for px in p.chunks_exact_mut(4) {
            px.copy_from_slice(&fill);
        }
        p
    }

    #[test]
    fn color_cursor_opaque_pixel_replaces() {
        let cursor = CursorImage {
            shape_type: SHAPE_COLOR,
            data: vec![10, 20, 30, 255], // one opaque BGRA pixel
            pitch: 4,
            width: 1,
            visual_height: 1,
        };
        let mut p = patch(1, 1, [100, 100, 100, 255]);
        composite_cursor(&mut p, 4, 1, 1, &cursor, 0, 0);
        assert_eq!(&p[..3], &[10, 20, 30]);
    }

    #[test]
    fn color_cursor_transparent_pixel_keeps_dst() {
        let cursor = CursorImage {
            shape_type: SHAPE_COLOR,
            data: vec![10, 20, 30, 0],
            pitch: 4,
            width: 1,
            visual_height: 1,
        };
        let mut p = patch(1, 1, [100, 100, 100, 255]);
        composite_cursor(&mut p, 4, 1, 1, &cursor, 0, 0);
        assert_eq!(&p[..3], &[100, 100, 100]);
    }

    #[test]
    fn color_cursor_half_alpha_blends() {
        let cursor = CursorImage {
            shape_type: SHAPE_COLOR,
            data: vec![255, 255, 255, 128],
            pitch: 4,
            width: 1,
            visual_height: 1,
        };
        let mut p = patch(1, 1, [0, 0, 0, 255]);
        composite_cursor(&mut p, 4, 1, 1, &cursor, 0, 0);
        // 255 * 128/255 ≈ 128.
        assert!(p[0] >= 127 && p[0] <= 129, "got {}", p[0]);
    }

    #[test]
    fn monochrome_inverts_where_xor_set() {
        // 8×1 visual cursor: AND row all-ones (keep dst), XOR row has
        // the leftmost bit set (invert pixel 0).
        let cursor = CursorImage {
            shape_type: SHAPE_MONOCHROME,
            data: vec![0b1111_1111, 0b1000_0000],
            pitch: 1,
            width: 8,
            visual_height: 1,
        };
        let mut p = patch(8, 1, [0x0F, 0x0F, 0x0F, 255]);
        composite_cursor(&mut p, 32, 8, 1, &cursor, 0, 0);
        assert_eq!(&p[..3], &[0xF0, 0xF0, 0xF0]); // inverted
        assert_eq!(&p[4..7], &[0x0F, 0x0F, 0x0F]); // untouched
    }

    #[test]
    fn monochrome_black_where_and_clear() {
        // AND row all-zero + XOR all-zero ⇒ solid black cursor body.
        let cursor = CursorImage {
            shape_type: SHAPE_MONOCHROME,
            data: vec![0b0000_0000, 0b0000_0000],
            pitch: 1,
            width: 8,
            visual_height: 1,
        };
        let mut p = patch(8, 1, [200, 200, 200, 255]);
        composite_cursor(&mut p, 32, 8, 1, &cursor, 0, 0);
        assert_eq!(&p[..3], &[0, 0, 0]);
    }

    #[test]
    fn masked_color_replaces_and_xors() {
        // Two pixels: alpha 0 ⇒ replace with color; alpha FF ⇒ XOR.
        let cursor = CursorImage {
            shape_type: SHAPE_MASKED_COLOR,
            data: vec![1, 2, 3, 0, 0xFF, 0xFF, 0xFF, 0xFF],
            pitch: 8,
            width: 2,
            visual_height: 1,
        };
        let mut p = patch(2, 1, [0x0F, 0x0F, 0x0F, 255]);
        composite_cursor(&mut p, 8, 2, 1, &cursor, 0, 0);
        assert_eq!(&p[..3], &[1, 2, 3]);
        assert_eq!(&p[4..7], &[0xF0, 0xF0, 0xF0]);
    }

    #[test]
    fn clip_fully_inside() {
        assert_eq!(
            clip_cursor_rect(10, 20, 32, 32, 1920, 1080),
            Some((10, 20, 32, 32, 0, 0))
        );
    }

    #[test]
    fn clip_partially_off_left_top() {
        assert_eq!(
            clip_cursor_rect(-8, -4, 32, 32, 1920, 1080),
            Some((0, 0, 24, 28, 8, 4))
        );
    }

    #[test]
    fn clip_partially_off_right_bottom() {
        assert_eq!(
            clip_cursor_rect(1900, 1060, 32, 32, 1920, 1080),
            Some((1900, 1060, 20, 20, 0, 0))
        );
    }

    #[test]
    fn clip_fully_off_screen() {
        assert_eq!(clip_cursor_rect(-64, 10, 32, 32, 1920, 1080), None);
        assert_eq!(clip_cursor_rect(2000, 10, 32, 32, 1920, 1080), None);
    }

    #[test]
    fn src_offset_reads_correct_cursor_texel() {
        // 2×1 color cursor; patch covers only its second texel.
        let cursor = CursorImage {
            shape_type: SHAPE_COLOR,
            data: vec![9, 9, 9, 255, 40, 50, 60, 255],
            pitch: 8,
            width: 2,
            visual_height: 1,
        };
        let mut p = patch(1, 1, [0, 0, 0, 255]);
        composite_cursor(&mut p, 4, 1, 1, &cursor, 1, 0);
        assert_eq!(&p[..3], &[40, 50, 60]);
    }
}
