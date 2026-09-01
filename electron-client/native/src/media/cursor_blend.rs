//! Mouse-pointer shape handling for the DXGI Desktop Duplication capture
//! path (capture_dxgi.rs). Desktop Duplication delivers the desktop image
//! *without* the hardware cursor — the pointer arrives separately as a
//! shape bitmap + position. The shape is converted here into a plain
//! straight-alpha BGRA image; cursor_gpu.rs then alpha-blends it onto
//! each frame with a tiny GPU draw (no CPU↔GPU sync in the capture loop —
//! an earlier CPU blend via a staging Map stalled behind a busy game's
//! GPU queue and halved the stream rate).
//!
//! Pure pixel math, no OS types: cross-platform so `cargo test --lib`
//! exercises it on the Linux dev box even though its only caller is
//! Windows-only.

// The only consumer is cfg(target_os = "windows"); keep the module
// warning-free on the other platforms where only the tests see it.
#![allow(dead_code)]

/// DXGI_OUTDUPL_POINTER_SHAPE_TYPE values.
pub const SHAPE_MONOCHROME: u32 = 1;
pub const SHAPE_COLOR: u32 = 2;
pub const SHAPE_MASKED_COLOR: u32 = 4;

/// A cursor shape as delivered by GetFramePointerShape.
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

/// Convert a cursor shape into tightly packed straight-alpha BGRA
/// (`width × visual_height × 4`), ready to upload as a texture and
/// src-over blend.
///
/// Exact for COLOR shapes (the Windows default cursors). The two legacy
/// formats carry XOR ("invert what's underneath") pixels that a plain
/// alpha blend can't express; those are approximated by an opaque pixel:
/// MASKED_COLOR XOR → the inverted colour, MONOCHROME invert → mid grey,
/// which stays visible on both light and dark backgrounds (the I-beam is
/// the common case).
pub fn cursor_to_bgra(c: &CursorImage) -> Vec<u8> {
    let w = c.width;
    let h = c.visual_height;
    let mut out = vec![0u8; w * h * 4];
    match c.shape_type {
        SHAPE_COLOR => {
            for y in 0..h {
                for x in 0..w {
                    let s = y * c.pitch + x * 4;
                    let d = (y * w + x) * 4;
                    if let Some(px) = c.data.get(s..s + 4) {
                        out[d..d + 4].copy_from_slice(px);
                    }
                }
            }
        }
        SHAPE_MASKED_COLOR => {
            for y in 0..h {
                for x in 0..w {
                    let s = y * c.pitch + x * 4;
                    let d = (y * w + x) * 4;
                    let Some(px) = c.data.get(s..s + 4) else { continue };
                    if px[3] == 0 {
                        out[d..d + 3].copy_from_slice(&px[..3]);
                    } else {
                        // XOR pixel: approximate as the inverted colour.
                        out[d] = !px[0];
                        out[d + 1] = !px[1];
                        out[d + 2] = !px[2];
                    }
                    out[d + 3] = 0xFF;
                }
            }
        }
        SHAPE_MONOCHROME => {
            let xor_base = h * c.pitch;
            for y in 0..h {
                for x in 0..w {
                    let byte = x / 8;
                    let bit = 7 - (x % 8);
                    let and_bit = c
                        .data
                        .get(y * c.pitch + byte)
                        .map(|b| (b >> bit) & 1 == 1)
                        .unwrap_or(true);
                    let xor_bit = c
                        .data
                        .get(xor_base + y * c.pitch + byte)
                        .map(|b| (b >> bit) & 1 == 1)
                        .unwrap_or(false);
                    let d = (y * w + x) * 4;
                    let (v, a) = match (and_bit, xor_bit) {
                        (true, false) => (0u8, 0u8),      // transparent
                        (false, false) => (0, 0xFF),      // black
                        (false, true) => (0xFF, 0xFF),    // white
                        (true, true) => (0x80, 0xFF),     // invert ≈ mid grey
                    };
                    out[d] = v;
                    out[d + 1] = v;
                    out[d + 2] = v;
                    out[d + 3] = a;
                }
            }
        }
        _ => {}
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_shape_passes_through_with_alpha() {
        let c = CursorImage {
            shape_type: SHAPE_COLOR,
            data: vec![1, 2, 3, 128, 9, 9, 9, 9, /* pad */ 0, 0, 0, 0],
            pitch: 12, // padded rows
            width: 2,
            visual_height: 1,
        };
        assert_eq!(cursor_to_bgra(&c), vec![1, 2, 3, 128, 9, 9, 9, 9]);
    }

    #[test]
    fn masked_color_replace_and_xor_approximation() {
        let c = CursorImage {
            shape_type: SHAPE_MASKED_COLOR,
            data: vec![1, 2, 3, 0, 0x0F, 0xF0, 0x00, 0xFF],
            pitch: 8,
            width: 2,
            visual_height: 1,
        };
        assert_eq!(
            cursor_to_bgra(&c),
            vec![1, 2, 3, 0xFF, 0xF0, 0x0F, 0xFF, 0xFF]
        );
    }

    #[test]
    fn monochrome_four_combinations() {
        // 4 px wide, 1 row: AND bits 1,0,0,1 ; XOR bits 0,0,1,1
        let c = CursorImage {
            shape_type: SHAPE_MONOCHROME,
            data: vec![0b1001_0000, 0b0011_0000],
            pitch: 1,
            width: 4,
            visual_height: 1,
        };
        let out = cursor_to_bgra(&c);
        assert_eq!(&out[0..4], &[0, 0, 0, 0]); // transparent
        assert_eq!(&out[4..8], &[0, 0, 0, 0xFF]); // black
        assert_eq!(&out[8..12], &[0xFF, 0xFF, 0xFF, 0xFF]); // white
        assert_eq!(&out[12..16], &[0x80, 0x80, 0x80, 0xFF]); // invert ≈ grey
    }

    #[test]
    fn short_buffer_is_safe() {
        let c = CursorImage {
            shape_type: SHAPE_COLOR,
            data: vec![1, 2, 3], // truncated
            pitch: 4,
            width: 2,
            visual_height: 2,
        };
        assert_eq!(cursor_to_bgra(&c).len(), 16);
    }

    #[test]
    fn unknown_shape_is_transparent() {
        let c = CursorImage {
            shape_type: 99,
            data: vec![0xFF; 16],
            pitch: 8,
            width: 2,
            visual_height: 2,
        };
        assert!(cursor_to_bgra(&c).iter().all(|&b| b == 0));
    }
}
