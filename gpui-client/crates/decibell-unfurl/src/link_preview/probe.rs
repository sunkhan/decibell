//! Header-only image dimensions (`probeImageDims` in `linkPreview.ts`): PNG, GIF, WebP (VP8X,
//! VP8L, VP8) and JPEG (the first SOF marker), so a preview can reserve its box before the pixels
//! arrive. A JPEG whose SOF sits past the bytes read (a large EXIF block) comes back unknown.

/// Width and height read from an image's first bytes, or `None` when the format or its header
/// isn't recognised.
pub(crate) fn probe_image_dims(b: &[u8]) -> Option<(u32, u32)> {
    let be16 = |i: usize| u32::from(u16::from_be_bytes([b[i], b[i + 1]]));
    let le16 = |i: usize| u32::from(u16::from_le_bytes([b[i], b[i + 1]]));
    let be32 = |i: usize| u32::from_be_bytes([b[i], b[i + 1], b[i + 2], b[i + 3]]);
    let le24 =
        |i: usize| u32::from(b[i]) | (u32::from(b[i + 1]) << 8) | (u32::from(b[i + 2]) << 16);

    if b.len() >= 24 && b[0] == 0x89 && &b[1..4] == b"PNG" {
        return Some((be32(16), be32(20)));
    }
    if b.len() >= 10 && &b[0..3] == b"GIF" {
        return Some((le16(6), le16(8)));
    }
    if b.len() >= 30 && &b[0..4] == b"RIFF" && &b[8..12] == b"WEBP" {
        return match &b[12..16] {
            b"VP8X" => Some((1 + le24(24), 1 + le24(27))),
            b"VP8L" if b[20] == 0x2f => {
                let bits = le24(21) | (u32::from(b[24]) << 24);
                Some(((bits & 0x3fff) + 1, ((bits >> 14) & 0x3fff) + 1))
            }
            b"VP8 " if b[23] == 0x9d && b[24] == 0x01 && b[25] == 0x2a => {
                Some((le16(26) & 0x3fff, le16(28) & 0x3fff))
            }
            _ => None,
        };
    }
    if b.len() >= 4 && b[0] == 0xff && b[1] == 0xd8 {
        let mut i = 2;
        while i + 9 < b.len() {
            if b[i] != 0xff {
                i += 1;
                continue;
            }
            let marker = b[i + 1];
            if marker == 0xff {
                i += 1;
                continue;
            }
            // Standalone markers carry no length.
            if marker == 0xd8 || marker == 0x01 || (0xd0..=0xd7).contains(&marker) {
                i += 2;
                continue;
            }
            let len = be16(i + 2) as usize;
            let is_sof = (0xc0..=0xcf).contains(&marker) && ![0xc4, 0xc8, 0xcc].contains(&marker);
            if is_sof {
                return Some((be16(i + 7), be16(i + 5)));
            }
            if marker == 0xda {
                break;
            }
            i += 2 + len;
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn png_gif_webp_jpeg() {
        let mut png = vec![
            0x89, b'P', b'N', b'G', 13, 10, 26, 10, 0, 0, 0, 13, b'I', b'H', b'D', b'R',
        ];
        png.extend_from_slice(&640u32.to_be_bytes());
        png.extend_from_slice(&480u32.to_be_bytes());
        assert_eq!(probe_image_dims(&png), Some((640, 480)));
        assert_eq!(probe_image_dims(&png[..23]), None);

        let gif = b"GIF89a\x40\x01\xf0\x00";
        assert_eq!(probe_image_dims(gif), Some((320, 240)));

        let mut vp8x = b"RIFF\0\0\0\0WEBPVP8X\x0a\0\0\0\0\0\0\0".to_vec();
        vp8x.extend_from_slice(&[0x7f, 0x02, 0x00, 0xdf, 0x01, 0x00]); // 640 x 480 (minus one)
        assert_eq!(probe_image_dims(&vp8x), Some((640, 480)));

        // A JPEG: APP0, then SOF0 with height 300, width 400.
        let jpeg = [
            0xff, 0xd8, 0xff, 0xe0, 0x00, 0x04, 0x00, 0x00, 0xff, 0xc0, 0x00, 0x11, 0x08, 0x01,
            0x2c, 0x01, 0x90, 0x03, 0x01, 0x22, 0x00,
        ];
        assert_eq!(probe_image_dims(&jpeg), Some((400, 300)));
        assert_eq!(probe_image_dims(b"not an image"), None);
    }
}
