//! Platform-neutral helpers for the native encode paths, kept out of the
//! `cfg(windows)` modules so their unit tests run everywhere:
//!
//!  - Annex B scanning + parameter-set insertion — the safety net that
//!    guarantees every H.264 / HEVC keyframe a watcher can join on carries
//!    its SPS/PPS (+VPS), whatever the encoder driver's defaults are.
//!  - The aspect-fit destination rect the D3D11 video processor scales the
//!    captured texture into.

// Linux uses only the Annex B half (H.264 parameter-set safety net).
#![cfg_attr(not(target_os = "windows"), allow(dead_code))]

/// NAL header layout of an Annex B bitstream.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NalCodec {
    H264,
    Hevc,
}

impl NalCodec {
    /// nal_unit_type from the first NAL header byte.
    pub fn nal_type(self, header: u8) -> u8 {
        match self {
            NalCodec::H264 => header & 0x1F,
            NalCodec::Hevc => (header >> 1) & 0x3F,
        }
    }

    /// Sequence parameter set NAL type (H.264 7, HEVC SPS_NUT 33).
    pub fn sps_type(self) -> u8 {
        match self {
            NalCodec::H264 => 7,
            NalCodec::Hevc => 33,
        }
    }

    /// Access unit delimiter NAL type (H.264 9, HEVC AUD_NUT 35).
    pub fn aud_type(self) -> u8 {
        match self {
            NalCodec::H264 => 9,
            NalCodec::Hevc => 35,
        }
    }
}

/// Byte offsets of every NAL header (the byte after each `00 00 01`; a
/// 4-byte `00 00 00 01` start code is found by the same scan). Emulation
/// prevention guarantees `00 00 01` never occurs inside a NAL payload.
fn nal_header_offsets(buf: &[u8]) -> impl Iterator<Item = usize> + '_ {
    let mut i = 0usize;
    std::iter::from_fn(move || {
        while i + 3 <= buf.len() {
            if buf[i] == 0 && buf[i + 1] == 0 && buf[i + 2] == 1 {
                let header = i + 3;
                i = header;
                return (header < buf.len()).then_some(header);
            }
            i += 1;
        }
        None
    })
}

/// Does `buf` begin with an Annex B start code?
pub fn starts_with_start_code(buf: &[u8]) -> bool {
    buf.starts_with(&[0, 0, 1]) || buf.starts_with(&[0, 0, 0, 1])
}

/// Does the Annex B buffer contain a NAL unit of type `nal_type`?
pub fn contains_nal_type(buf: &[u8], codec: NalCodec, nal_type: u8) -> bool {
    nal_header_offsets(buf).any(|h| codec.nal_type(buf[h]) == nal_type)
}

/// Is `extradata` an Annex B parameter-set blob we can splice in front of
/// a keyframe (starts with a start code and carries an SPS)? avcC / hvcC
/// extradata fails this and the safety net stays off.
pub fn usable_parameter_sets(extradata: &[u8], codec: NalCodec) -> bool {
    starts_with_start_code(extradata) && contains_nal_type(extradata, codec, codec.sps_type())
}

/// `packet` with `param_sets` (Annex B, own start codes) inserted in front
/// of its first non-AUD NAL. An access unit delimiter must stay the first
/// NAL of the access unit, so when the packet opens with one (AMF inserts
/// AUDs by default) the parameter sets go right after it; otherwise they
/// are prepended.
pub fn insert_parameter_sets(packet: &[u8], param_sets: &[u8], codec: NalCodec) -> Vec<u8> {
    let mut at = 0usize;
    let mut headers = nal_header_offsets(packet);
    if let Some(first) = headers.next() {
        if codec.nal_type(packet[first]) == codec.aud_type() {
            at = match headers.next() {
                Some(second) => {
                    // Back up over the 3-byte start code, then over any zero
                    // bytes in front of it (the leading zero of a 4-byte start
                    // code / trailing_zero_8bits). Never into the AUD itself:
                    // its last payload byte carries the RBSP stop bit.
                    let mut p = second - 3;
                    while p > first + 1 && packet[p - 1] == 0 {
                        p -= 1;
                    }
                    p
                }
                None => packet.len(),
            };
        }
    }
    let mut out = Vec::with_capacity(packet.len() + param_sets.len());
    out.extend_from_slice(&packet[..at]);
    out.extend_from_slice(param_sets);
    out.extend_from_slice(&packet[at..]);
    out
}

/// Destination rect `(left, top, right, bottom)` that fits a `src_w`×`src_h`
/// image inside a `dst_w`×`dst_h` target without distorting it, centred.
/// Edges that don't touch the target border are even-aligned: NV12 chroma
/// is subsampled 2×2, and an odd edge smears half a chroma row/column.
pub fn aspect_fit_rect(src_w: u32, src_h: u32, dst_w: u32, dst_h: u32) -> (i32, i32, i32, i32) {
    if src_w == 0 || src_h == 0 || dst_w == 0 || dst_h == 0 {
        return (0, 0, dst_w as i32, dst_h as i32);
    }
    let (sw, sh, dw, dh) = (src_w as u64, src_h as u64, dst_w as u64, dst_h as u64);
    let (mut w, mut h) = if sw * dh >= sh * dw {
        // Source is relatively wider (or equal): full width, letterboxed.
        (dw, (sh * dw + sw / 2) / sw)
    } else {
        // Source is relatively taller: full height, pillarboxed.
        ((sw * dh + sh / 2) / sh, dh)
    };
    w = w.clamp(1, dw);
    h = h.clamp(1, dh);
    if w < dw {
        w = (w & !1).max(2).min(dw);
    }
    if h < dh {
        h = (h & !1).max(2).min(dh);
    }
    let left = ((dw - w) / 2) & !1;
    let top = ((dh - h) / 2) & !1;
    (left as i32, top as i32, (left + w) as i32, (top + h) as i32)
}

#[cfg(test)]
mod tests {
    use super::*;

    // H.264: AUD(9) SPS(7) PPS(8) IDR(5); HEVC: VPS(32) SPS(33) PPS(34) IDR_W_RADL(19) AUD(35).
    const H264_AUD: &[u8] = &[0, 0, 0, 1, 0x09, 0x10];
    const H264_SPS_PPS: &[u8] = &[0, 0, 0, 1, 0x67, 0x42, 0xC0, 0x1F, 0, 0, 0, 1, 0x68, 0xCE, 0x3C, 0x80];
    const H264_IDR: &[u8] = &[0, 0, 1, 0x65, 0x88, 0x84, 0x21];
    const HEVC_VPS_SPS_PPS: &[u8] = &[
        0, 0, 0, 1, 0x40, 0x01, 0x0C, 0, 0, 0, 1, 0x42, 0x01, 0x01, 0, 0, 0, 1, 0x44, 0x01, 0xC1,
    ];
    const HEVC_AUD: &[u8] = &[0, 0, 0, 1, 0x46, 0x01, 0x10];
    const HEVC_IDR: &[u8] = &[0, 0, 0, 1, 0x26, 0x01, 0xAF];

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.iter().flat_map(|p| p.iter().copied()).collect()
    }

    #[test]
    fn nal_type_extraction() {
        assert_eq!(NalCodec::H264.nal_type(0x67), 7);
        assert_eq!(NalCodec::H264.nal_type(0x65), 5);
        assert_eq!(NalCodec::Hevc.nal_type(0x42), 33);
        assert_eq!(NalCodec::Hevc.nal_type(0x26), 19);
        assert_eq!(NalCodec::Hevc.nal_type(0x46), 35);
    }

    #[test]
    fn finds_sps_behind_three_and_four_byte_start_codes() {
        let four = cat(&[H264_SPS_PPS, H264_IDR]);
        assert!(contains_nal_type(&four, NalCodec::H264, 7));
        let three: Vec<u8> = cat(&[&[0, 0, 1, 0x67, 0x42], H264_IDR]);
        assert!(contains_nal_type(&three, NalCodec::H264, 7));
        assert!(contains_nal_type(&three, NalCodec::H264, 5));
        assert!(!contains_nal_type(H264_IDR, NalCodec::H264, 7));
    }

    #[test]
    fn hevc_sps_detection() {
        let with = cat(&[HEVC_VPS_SPS_PPS, HEVC_IDR]);
        assert!(contains_nal_type(&with, NalCodec::Hevc, 33));
        assert!(!contains_nal_type(HEVC_IDR, NalCodec::Hevc, 33));
        // An H.264 SPS header byte (0x67) is HEVC type 51, not 33.
        assert!(!contains_nal_type(H264_SPS_PPS, NalCodec::Hevc, 33));
    }

    #[test]
    fn degenerate_buffers() {
        assert!(!contains_nal_type(&[], NalCodec::H264, 7));
        assert!(!contains_nal_type(&[0, 0, 1], NalCodec::H264, 7));
        assert!(!contains_nal_type(&[0x67, 0x42], NalCodec::H264, 7));
        assert!(!starts_with_start_code(&[0, 1]));
        assert!(starts_with_start_code(&[0, 0, 1, 0x67]));
        assert!(starts_with_start_code(&[0, 0, 0, 1, 0x67]));
    }

    #[test]
    fn usable_parameter_sets_requires_annex_b_with_sps() {
        assert!(usable_parameter_sets(H264_SPS_PPS, NalCodec::H264));
        assert!(usable_parameter_sets(HEVC_VPS_SPS_PPS, NalCodec::Hevc));
        // avcC (starts with configurationVersion = 1) is not Annex B.
        assert!(!usable_parameter_sets(&[1, 0x42, 0xC0, 0x1F, 0xFF, 0xE1], NalCodec::H264));
        // Annex B without an SPS.
        assert!(!usable_parameter_sets(&[0, 0, 0, 1, 0x68, 0xCE], NalCodec::H264));
    }

    #[test]
    fn prepends_when_no_aud() {
        let out = insert_parameter_sets(H264_IDR, H264_SPS_PPS, NalCodec::H264);
        assert_eq!(out, cat(&[H264_SPS_PPS, H264_IDR]));
    }

    #[test]
    fn inserts_after_leading_aud() {
        let pkt = cat(&[H264_AUD, H264_IDR]);
        let out = insert_parameter_sets(&pkt, H264_SPS_PPS, NalCodec::H264);
        assert_eq!(out, cat(&[H264_AUD, H264_SPS_PPS, H264_IDR]));
        assert!(contains_nal_type(&out, NalCodec::H264, 7));

        let pkt = cat(&[HEVC_AUD, HEVC_IDR]);
        let out = insert_parameter_sets(&pkt, HEVC_VPS_SPS_PPS, NalCodec::Hevc);
        // HEVC_IDR opens with a 4-byte start code: its leading zero stays
        // with it, after the inserted parameter sets.
        assert_eq!(out, cat(&[HEVC_AUD, HEVC_VPS_SPS_PPS, HEVC_IDR]));
    }

    #[test]
    fn aud_only_packet_appends() {
        let out = insert_parameter_sets(H264_AUD, H264_SPS_PPS, NalCodec::H264);
        assert_eq!(out, cat(&[H264_AUD, H264_SPS_PPS]));
    }

    #[test]
    fn aspect_fit_same_aspect_fills() {
        assert_eq!(aspect_fit_rect(1920, 1080, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(aspect_fit_rect(2560, 1440, 1920, 1080), (0, 0, 1920, 1080));
        assert_eq!(aspect_fit_rect(3840, 2160, 1280, 720), (0, 0, 1280, 720));
    }

    #[test]
    fn aspect_fit_pillarbox_and_letterbox() {
        // 16:10 into 16:9 → pillarbox.
        assert_eq!(aspect_fit_rect(1920, 1200, 1920, 1080), (96, 0, 1824, 1080));
        // Portrait into landscape.
        assert_eq!(aspect_fit_rect(1080, 1920, 1920, 1080), (656, 0, 1264, 1080));
        // Ultrawide into 16:9 → letterbox.
        let (l, t, r, b) = aspect_fit_rect(3440, 1440, 1920, 1080);
        assert_eq!((l, r), (0, 1920));
        assert_eq!(t % 2, 0);
        assert_eq!((b - t) % 2, 0);
        assert!(b <= 1080 && t > 0);
    }

    #[test]
    fn aspect_fit_odd_window_is_even_aligned() {
        let (l, t, r, b) = aspect_fit_rect(1001, 701, 1920, 1080);
        assert_eq!((t, b), (0, 1080));
        assert_eq!(l % 2, 0);
        assert_eq!((r - l) % 2, 0);
        assert!(r <= 1920);
    }

    #[test]
    fn aspect_fit_degenerate_input_fills() {
        assert_eq!(aspect_fit_rect(0, 0, 1280, 720), (0, 0, 1280, 720));
        let (l, t, r, b) = aspect_fit_rect(1, 5000, 1280, 720);
        assert!(r - l >= 2 && b - t == 720 && l >= 0 && r <= 1280);
    }
}
