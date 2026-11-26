//! Test support for the encoder harnesses (`encoder_harness.rs` on Linux,
//! `encoder.rs`'s tests on Windows): a synthetic screen to encode, and the
//! checks every backend's output must pass. No capture and no particular
//! GPU: the frames are generated, and the output is judged from the
//! bitstream and by decoding it with FFmpeg's software decoders (when the
//! linked FFmpeg has them: the development prefix does, the Electron
//! addon's does not, and the decode check then reports itself skipped).
#![cfg(all(test, any(target_os = "linux", target_os = "windows")))]

use ffmpeg_next as ff;

use super::caps::CodecKind;

/// Held by every test that opens a real encoder: one hardware session at a
/// time in the test process. With a dozen tests creating and destroying
/// NVENC sessions in parallel the NVIDIA driver (620.73.04) crashed about
/// one run in six, in one of its own threads inside libnvcuvid; the client
/// itself never has more than a stream's encoder and a probe open. It also
/// keeps the tests under a consumer card's session limit.
pub fn encoder_lock() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    LOCK.lock().unwrap_or_else(|e| e.into_inner())
}

/// 5×7 glyphs, digits then A–Z, one byte a row, bit 4 the left column.
#[rustfmt::skip]
const FONT: [[u8; 7]; 36] = [
    [0x0E,0x11,0x13,0x15,0x19,0x11,0x0E], [0x04,0x0C,0x04,0x04,0x04,0x04,0x0E],
    [0x0E,0x11,0x01,0x02,0x04,0x08,0x1F], [0x1F,0x02,0x04,0x02,0x01,0x11,0x0E],
    [0x02,0x06,0x0A,0x12,0x1F,0x02,0x02], [0x1F,0x10,0x1E,0x01,0x01,0x11,0x0E],
    [0x06,0x08,0x10,0x1E,0x11,0x11,0x0E], [0x1F,0x01,0x02,0x04,0x08,0x08,0x08],
    [0x0E,0x11,0x11,0x0E,0x11,0x11,0x0E], [0x0E,0x11,0x11,0x0F,0x01,0x02,0x0C],
    [0x0E,0x11,0x11,0x11,0x1F,0x11,0x11], [0x1E,0x11,0x11,0x1E,0x11,0x11,0x1E],
    [0x0E,0x11,0x10,0x10,0x10,0x11,0x0E], [0x1C,0x12,0x11,0x11,0x11,0x12,0x1C],
    [0x1F,0x10,0x10,0x1E,0x10,0x10,0x1F], [0x1F,0x10,0x10,0x1E,0x10,0x10,0x10],
    [0x0E,0x11,0x10,0x17,0x11,0x11,0x0F], [0x11,0x11,0x11,0x1F,0x11,0x11,0x11],
    [0x0E,0x04,0x04,0x04,0x04,0x04,0x0E], [0x07,0x02,0x02,0x02,0x02,0x12,0x0C],
    [0x11,0x12,0x14,0x18,0x14,0x12,0x11], [0x10,0x10,0x10,0x10,0x10,0x10,0x1F],
    [0x11,0x1B,0x15,0x15,0x11,0x11,0x11], [0x11,0x11,0x19,0x15,0x13,0x11,0x11],
    [0x0E,0x11,0x11,0x11,0x11,0x11,0x0E], [0x1E,0x11,0x11,0x1E,0x10,0x10,0x10],
    [0x0E,0x11,0x11,0x11,0x15,0x12,0x0D], [0x1E,0x11,0x11,0x1E,0x14,0x12,0x11],
    [0x0F,0x10,0x10,0x0E,0x01,0x01,0x1E], [0x1F,0x04,0x04,0x04,0x04,0x04,0x04],
    [0x11,0x11,0x11,0x11,0x11,0x11,0x0E], [0x11,0x11,0x11,0x11,0x11,0x0A,0x04],
    [0x11,0x11,0x11,0x15,0x15,0x15,0x0A], [0x11,0x11,0x0A,0x04,0x0A,0x11,0x11],
    [0x11,0x11,0x11,0x0A,0x04,0x04,0x04], [0x1F,0x01,0x02,0x04,0x08,0x10,0x1F],
];

pub(super) fn hash(a: u32, b: u32) -> u32 {
    let mut x = a.wrapping_mul(0x9E37_79B1) ^ b.wrapping_mul(0x85EB_CA6B) ^ 0x2545_F491;
    x ^= x >> 15;
    x = x.wrapping_mul(0x2C1B_3C6D);
    x ^= x >> 12;
    x
}

/// Draw one glyph at (`x`, `y`) of a `stride`-wide BGRA buffer, `scale`
/// pixels a font pixel, clipped to `w`×`h`.
#[allow(clippy::too_many_arguments)]
pub(super) fn glyph(buf: &mut [u8], stride: usize, w: usize, h: usize, x: usize, y: usize, scale: usize, g: usize, bgr: [u8; 3]) {
    for (row, bits) in FONT[g % FONT.len()].iter().enumerate() {
        for col in 0..5 {
            if bits & (0x10 >> col) == 0 {
                continue;
            }
            for dy in 0..scale {
                for dx in 0..scale {
                    let (px, py) = (x + col * scale + dx, y + row * scale + dy);
                    if px < w && py < h {
                        buf[py * stride + px * 4..][..3].copy_from_slice(&bgr);
                    }
                }
            }
        }
    }
}

/// A synthetic screen share: a page of text that scrolls (dense, hard
/// edged detail that moves every frame: the expensive case for a screen
/// encoder), a static dark side bar, a coloured rectangle that drifts like
/// a dragged window, and a frame counter in large digits.
///
/// The page is rendered once; a frame is composed by copying rows, so
/// generating frames costs little next to encoding them.
pub struct Screen {
    pub w: u32,
    pub h: u32,
    pub stride: usize,
    /// The scrolling text, twice the screen's height, `text_w` wide.
    page: Vec<u8>,
    page_h: usize,
    sidebar: Vec<u8>,
    sidebar_w: usize,
    /// Pixels the page moves a frame (0 = a still page).
    pub scroll_px: u32,
}

impl Screen {
    pub fn new(w: u32, h: u32, scroll_px: u32) -> Self {
        let (wu, hu) = (w as usize, h as usize);
        let stride = wu * 4 + 32; // a padded stride, as PipeWire's can be
        let sidebar_w = wu / 6;
        let text_w = wu - sidebar_w;
        let page_h = hu * 2;
        let page_stride = text_w * 4;
        let mut page = vec![0xF8u8; page_stride * page_h];
        // Lines of "words": 2x glyphs (about a UI's body text) with a band
        // of 1x small print every sixth line.
        let mut y = 12;
        let mut line = 0u32;
        while y + 16 < page_h {
            let small = line % 6 == 5;
            let scale = if small { 1 } else { 2 };
            let advance = 6 * scale;
            let mut x = 16;
            let mut col = 0u32;
            while x + advance < text_w.saturating_sub(16) {
                let r = hash(line, col);
                // Word gaps, and a short last word on some lines.
                if r % 7 != 0 && !(col > 40 && hash(line, 9999) % 3 == 0 && col as usize * advance > text_w / 2) {
                    let ink = if r % 23 == 0 { [0xC0, 0x30, 0x10] } else { [0x18, 0x18, 0x18] };
                    glyph(&mut page, page_stride, text_w, page_h, x, y, scale, (r >> 8) as usize, ink);
                }
                x += advance;
                col += 1;
            }
            y += if small { 12 } else { 20 };
            line += 1;
        }
        let mut sidebar = vec![0u8; sidebar_w * 4 * hu];
        for yy in 0..hu {
            for xx in 0..sidebar_w {
                let stripe = if (yy / 28) % 2 == 0 { 0x2A } else { 0x33 };
                sidebar[(yy * sidebar_w + xx) * 4..][..4].copy_from_slice(&[stripe, stripe - 4, stripe - 8, 0xFF]);
            }
            if yy % 28 == 8 {
                for (i, xx) in (10..sidebar_w.saturating_sub(8)).step_by(12).enumerate() {
                    if yy + 14 < hu {
                        glyph(&mut sidebar, sidebar_w * 4, sidebar_w, hu, xx, yy, 2, hash(yy as u32, i as u32) as usize, [0xD0, 0xD0, 0xD0]);
                    }
                }
            }
        }
        Self { w, h, stride, page, page_h, sidebar, sidebar_w, scroll_px }
    }

    pub fn frame_bytes(&self) -> usize {
        self.stride * self.h as usize
    }

    /// Render frame `n` into `out` (`frame_bytes()` long), BGRA.
    pub fn render(&self, n: u32, out: &mut [u8]) {
        let (w, h) = (self.w as usize, self.h as usize);
        let text_w = w - self.sidebar_w;
        let offset = (n as usize * self.scroll_px as usize) % (self.page_h - h);
        for y in 0..h {
            let row = &mut out[y * self.stride..][..w * 4];
            row[..self.sidebar_w * 4].copy_from_slice(&self.sidebar[y * self.sidebar_w * 4..][..self.sidebar_w * 4]);
            row[self.sidebar_w * 4..].copy_from_slice(&self.page[(y + offset) * text_w * 4..][..text_w * 4]);
        }
        // The dragged window: a flat rectangle with a border.
        let (bw, bh) = (w / 6, h / 6);
        let bx = self.sidebar_w + (n as usize * 5) % (text_w - bw).max(1);
        let by = h / 3 + ((n as usize * 2) % (h / 3).max(1));
        for y in by..(by + bh).min(h) {
            for x in bx..(bx + bw).min(w) {
                let edge = y < by + 3 || y + 3 >= by + bh || x < bx + 3 || x + 3 >= bx + bw;
                let px = if edge { [0x20, 0x20, 0x20, 0xFF] } else { [0xD8, 0x9A, 0x3C, 0xFF] };
                out[y * self.stride + x * 4..][..4].copy_from_slice(&px);
            }
        }
        // The frame counter, top right, on its own dark strip.
        let scale = (h / 90).max(2);
        let (cw, ch) = (6 * scale * 6 + 12, 7 * scale + 12);
        let cx = w - cw - 8;
        for y in 8..(8 + ch).min(h) {
            for x in cx..(cx + cw).min(w) {
                out[y * self.stride + x * 4..][..4].copy_from_slice(&[0x10, 0x10, 0x10, 0xFF]);
            }
        }
        let mut digits = [0usize; 6];
        let mut v = n;
        for d in digits.iter_mut().rev() {
            *d = (v % 10) as usize;
            v /= 10;
        }
        for (i, d) in digits.iter().enumerate() {
            glyph(out, self.stride, w, h, cx + 6 + i * 6 * scale, 14, scale, *d, [0xFF, 0xFF, 0xFF]);
        }
    }

    /// The BT.709 studio-range luma of frame `n`, tightly packed: what a
    /// correct conversion of `render(n)` produces, for PSNR / SSIM.
    pub fn luma(&self, frame: &[u8]) -> Vec<u8> {
        let (w, h) = (self.w as usize, self.h as usize);
        let mut y = vec![0u8; w * h];
        for row in 0..h {
            let src = &frame[row * self.stride..][..w * 4];
            let dst = &mut y[row * w..][..w];
            for (o, px) in dst.iter_mut().zip(src.chunks_exact(4)) {
                let (b, g, r) = (px[0] as f32, px[1] as f32, px[2] as f32);
                *o = (16.0 + (0.2126 * r + 0.7152 * g + 0.0722 * b) * (219.0 / 255.0)).round() as u8;
            }
        }
        y
    }
}

/// PSNR of two luma planes of the same size, in dB (99 for identical).
pub fn psnr(a: &[u8], b: &[u8]) -> f64 {
    let sse: f64 = a.iter().zip(b).map(|(x, y)| (*x as f64 - *y as f64).powi(2)).sum();
    if sse == 0.0 {
        return 99.0;
    }
    10.0 * (255.0f64 * 255.0 / (sse / a.len() as f64)).log10()
}

/// Mean SSIM over non-overlapping 8×8 windows of two `w`×`h` luma planes.
pub fn ssim(a: &[u8], b: &[u8], w: usize, h: usize) -> f64 {
    const C1: f64 = 6.5025; // (0.01 * 255)^2
    const C2: f64 = 58.5225; // (0.03 * 255)^2
    let (mut total, mut n) = (0.0, 0usize);
    for by in (0..h.saturating_sub(7)).step_by(8) {
        for bx in (0..w.saturating_sub(7)).step_by(8) {
            let (mut sa, mut sb, mut saa, mut sbb, mut sab) = (0.0, 0.0, 0.0, 0.0, 0.0);
            for y in by..by + 8 {
                for x in bx..bx + 8 {
                    let (p, q) = (a[y * w + x] as f64, b[y * w + x] as f64);
                    sa += p;
                    sb += q;
                    saa += p * p;
                    sbb += q * q;
                    sab += p * q;
                }
            }
            let (ma, mb) = (sa / 64.0, sb / 64.0);
            let (va, vb, cov) = (saa / 64.0 - ma * ma, sbb / 64.0 - mb * mb, sab / 64.0 - ma * mb);
            total += ((2.0 * ma * mb + C1) * (2.0 * cov + C2)) / ((ma * ma + mb * mb + C1) * (va + vb + C2));
            n += 1;
        }
    }
    if n == 0 {
        1.0
    } else {
        total / n as f64
    }
}

/// One encoded frame as it goes on the wire.
#[derive(Clone, Debug)]
pub struct Packet {
    pub data: Vec<u8>,
    pub key: bool,
    pub pts: i64,
    /// The decoder configuration sent with a keyframe (Linux HEVC: hvcC;
    /// Linux NVENC AV1: the sequence header), if any.
    pub description: Option<Vec<u8>>,
}

/// How H.264 / HEVC frames are framed: Annex B start codes (H.264
/// everywhere, HEVC on Windows) or 4-byte lengths with the parameter sets
/// in an hvcC description (HEVC on Linux).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Framing {
    AnnexB,
    LengthPrefixed,
}

/// Does a low-overhead AV1 temporal unit contain a sequence header OBU?
/// (AV1 spec 5.3: obu_header is forbidden(1) type(4) extension(1)
/// has_size(1) reserved(1); type 1 is OBU_SEQUENCE_HEADER.) An OBU without
/// a size field runs to the end of the buffer.
pub fn av1_has_sequence_header(data: &[u8]) -> bool {
    let mut i = 0;
    while i < data.len() {
        let header = data[i];
        let obu_type = (header >> 3) & 0x0F;
        let has_ext = header & 0x04 != 0;
        let has_size = header & 0x02 != 0;
        if obu_type == 1 {
            return true;
        }
        i += 1 + has_ext as usize;
        if !has_size {
            return false;
        }
        // leb128
        let (mut size, mut shift) = (0usize, 0);
        loop {
            let Some(&b) = data.get(i) else {
                return false;
            };
            i += 1;
            size |= ((b & 0x7F) as usize) << shift;
            shift += 7;
            if b & 0x80 == 0 || shift > 28 {
                break;
            }
        }
        i += size;
    }
    false
}

/// NAL types present in a length-prefixed (4-byte) HEVC access unit.
fn hevc_length_prefixed_types(data: &[u8]) -> Vec<u8> {
    let mut types = Vec::new();
    let mut i = 0;
    while i + 4 < data.len() {
        let len = u32::from_be_bytes([data[i], data[i + 1], data[i + 2], data[i + 3]]) as usize;
        if len == 0 || i + 4 + len > data.len() {
            break;
        }
        types.push((data[i + 4] >> 1) & 0x3F);
        i += 4 + len;
    }
    types
}

/// Can a watcher start decoding on this keyframe: does it carry its
/// parameter sets (in band, or in the description sent with it)?
pub fn keyframe_is_self_contained(codec: CodecKind, framing: Framing, p: &Packet) -> Result<(), String> {
    use super::encode_util::{contains_nal_type, NalCodec};
    match (codec, framing) {
        (CodecKind::H264Hw | CodecKind::H264Sw, _) => {
            for (name, t) in [("SPS", 7u8), ("PPS", 8), ("an IDR slice", 5)] {
                if !contains_nal_type(&p.data, NalCodec::H264, t) {
                    return Err(format!("H.264 keyframe at pts {} has no {name}", p.pts));
                }
            }
            Ok(())
        }
        (CodecKind::H265, Framing::AnnexB) => {
            for (name, t) in [("VPS", 32u8), ("SPS", 33), ("PPS", 34)] {
                if !contains_nal_type(&p.data, NalCodec::Hevc, t) {
                    return Err(format!("HEVC keyframe at pts {} has no {name}", p.pts));
                }
            }
            Ok(())
        }
        (CodecKind::H265, Framing::LengthPrefixed) => {
            // hvcC: byte 22 is numOfArrays; ours carries VPS, SPS and PPS.
            let desc = p.description.as_deref().ok_or_else(|| format!("HEVC keyframe at pts {} has no hvcC", p.pts))?;
            if desc.len() < 23 || desc[0] != 1 || desc[22] != 3 {
                return Err(format!("HEVC keyframe at pts {} has a malformed hvcC", p.pts));
            }
            // IRAP slices are NAL types 16..=23.
            if !hevc_length_prefixed_types(&p.data).iter().any(|t| (16..=23).contains(t)) {
                return Err(format!("HEVC keyframe at pts {} has no IRAP slice", p.pts));
            }
            Ok(())
        }
        (CodecKind::Av1, _) => {
            if av1_has_sequence_header(&p.data) {
                Ok(())
            } else {
                Err(format!("AV1 keyframe at pts {} has no sequence header OBU", p.pts))
            }
        }
        (CodecKind::Unknown, _) => Err("unknown codec".to_string()),
    }
}

/// What a harness run asked of the encoder.
pub struct Expect {
    pub codec: CodecKind,
    pub framing: Framing,
    pub frames: usize,
    pub fps: u32,
    pub bitrate_kbps: u32,
    /// Frame indices at which a keyframe was requested.
    pub forced_at: Vec<usize>,
    /// How many frames after the request the keyframe may come.
    pub forced_within: usize,
    /// Allowed overshoot of the average bitrate (0.15 = +15%).
    pub max_overshoot: f64,
    /// The share of the target the stream must at least reach. The test
    /// screen changes every frame, so a rate-controlled encoder fills its
    /// budget; one that undershoots a lot is not doing rate control.
    pub min_share: f64,
}

/// The numbers a run produced, for the report line.
#[derive(Debug, Default)]
pub struct Summary {
    pub packets: usize,
    pub keyframes: usize,
    pub kbps: f64,
    /// Largest packet as a multiple of the average one.
    pub peak_ratio: f64,
}

/// Every check that needs only the packets: one packet a frame, a keyframe
/// first, timestamps strictly increasing, every keyframe self-contained,
/// each requested keyframe honoured in time, the average bitrate within
/// tolerance of the target.
pub fn check_packets(packets: &[Packet], e: &Expect) -> Result<Summary, String> {
    if packets.len() != e.frames {
        return Err(format!("{} frames in, {} packets out", e.frames, packets.len()));
    }
    if !packets[0].key {
        return Err("the first packet is not a keyframe".to_string());
    }
    for pair in packets.windows(2) {
        if pair[1].pts <= pair[0].pts {
            return Err(format!("timestamps go {} -> {}", pair[0].pts, pair[1].pts));
        }
    }
    for p in packets.iter().filter(|p| p.key) {
        keyframe_is_self_contained(e.codec, e.framing, p)?;
    }
    if let Some(p) = packets.iter().find(|p| p.data.is_empty()) {
        return Err(format!("empty packet at pts {}", p.pts));
    }
    // The harness numbers frames from 0, so pts order is frame order.
    for &at in &e.forced_at {
        let window = &packets[at.min(packets.len())..(at + e.forced_within + 1).min(packets.len())];
        if !window.iter().any(|p| p.key) {
            return Err(format!(
                "a keyframe requested at frame {at} did not come within {} frames",
                e.forced_within
            ));
        }
    }
    let bytes: usize = packets.iter().map(|p| p.data.len()).sum();
    let seconds = e.frames as f64 / e.fps as f64;
    let kbps = bytes as f64 * 8.0 / 1000.0 / seconds;
    let target = e.bitrate_kbps as f64;
    if kbps > target * (1.0 + e.max_overshoot) {
        return Err(format!("{kbps:.0} kbps over {seconds:.1} s for a {target:.0} kbps target (over +{:.0}%)", e.max_overshoot * 100.0));
    }
    if kbps < target * e.min_share {
        return Err(format!("{kbps:.0} kbps over {seconds:.1} s for a {target:.0} kbps target (under {:.0}%)", e.min_share * 100.0));
    }
    let avg = bytes as f64 / packets.len() as f64;
    let peak = packets.iter().map(|p| p.data.len()).max().unwrap_or(0) as f64;
    Ok(Summary { packets: packets.len(), keyframes: packets.iter().filter(|p| p.key).count(), kbps, peak_ratio: peak / avg })
}

/// Why a stream was not decoded.
#[derive(Debug)]
pub enum DecodeError {
    /// The linked FFmpeg has no software decoder for the codec.
    NoDecoder(&'static str),
    Failed(String),
}

/// A decoded 4:2:0 picture and the colour description its bitstream
/// carries (FFmpeg's `AVCOL_*` numbers, which are the H.273 code points:
/// 1 = BT.709, 2 = unspecified, 5 / 6 = BT.601; range 1 = studio, 2 = full).
pub struct Picture<'a> {
    pub index: usize,
    pub w: usize,
    pub h: usize,
    /// Y, U, V; chroma is (w+1)/2 × (h+1)/2.
    pub planes: [&'a [u8]; 3],
    pub strides: [usize; 3],
    pub matrix: i32,
    pub primaries: i32,
    pub transfer: i32,
    pub range: i32,
}

/// Decode `packets` with FFmpeg's software decoder (h264, hevc, libdav1d)
/// and hand each picture's luma to `on_frame(index, y, stride, width,
/// height)`. Any decode error fails; the caller compares the count.
pub fn decode<F>(codec: CodecKind, framing: Framing, packets: &[Packet], mut on_frame: F) -> Result<usize, DecodeError>
where
    F: FnMut(usize, &[u8], usize, u32, u32),
{
    decode_pictures(codec, framing, packets, |p| on_frame(p.index, p.planes[0], p.strides[0], p.w as u32, p.h as u32))
}

/// `decode`, with the whole picture and its colour description.
pub fn decode_pictures<F>(codec: CodecKind, framing: Framing, packets: &[Packet], mut on_frame: F) -> Result<usize, DecodeError>
where
    F: FnMut(&Picture),
{
    ff::init().map_err(|e| DecodeError::Failed(e.to_string()))?;
    let name = match codec {
        CodecKind::H264Hw | CodecKind::H264Sw => "h264",
        CodecKind::H265 => "hevc",
        CodecKind::Av1 => "libdav1d",
        CodecKind::Unknown => return Err(DecodeError::Failed("unknown codec".to_string())),
    };
    let Some(decoder) = ff::decoder::find_by_name(name) else {
        return Err(DecodeError::NoDecoder(name));
    };
    let mut context = ff::codec::Context::new_with_codec(decoder);
    // Length-prefixed HEVC is decoded from its hvcC, as a receiver does.
    if framing == Framing::LengthPrefixed {
        let desc = packets
            .iter()
            .find_map(|p| p.description.as_deref())
            .ok_or_else(|| DecodeError::Failed("no description to configure the decoder with".to_string()))?;
        unsafe {
            let p = context.as_mut_ptr();
            let buf = ff::sys::av_mallocz(desc.len() + ff::sys::AV_INPUT_BUFFER_PADDING_SIZE as usize) as *mut u8;
            std::ptr::copy_nonoverlapping(desc.as_ptr(), buf, desc.len());
            (*p).extradata = buf;
            (*p).extradata_size = desc.len() as i32;
        }
    }
    // Fail on a damaged stream instead of concealing it.
    unsafe {
        (*context.as_mut_ptr()).err_recognition |= ff::sys::AV_EF_EXPLODE as i32;
    }
    let mut decoder = context.decoder().video().map_err(|e| DecodeError::Failed(format!("open {name}: {e}")))?;
    let mut count = 0usize;
    let mut frame = ff::frame::Video::empty();
    let mut drain = |decoder: &mut ff::decoder::Video, count: &mut usize| -> Result<(), DecodeError> {
        loop {
            match decoder.receive_frame(&mut frame) {
                Ok(()) => {
                    use ff::format::Pixel;
                    if !matches!(frame.format(), Pixel::YUV420P | Pixel::YUVJ420P) {
                        return Err(DecodeError::Failed(format!("decoded as {:?}, not 8-bit 4:2:0", frame.format())));
                    }
                    let (matrix, primaries, transfer, range) = unsafe {
                        let f = frame.as_ptr();
                        ((*f).colorspace as i32, (*f).color_primaries as i32, (*f).color_trc as i32, (*f).color_range as i32)
                    };
                    on_frame(&Picture {
                        index: *count,
                        w: frame.width() as usize,
                        h: frame.height() as usize,
                        planes: [frame.data(0), frame.data(1), frame.data(2)],
                        strides: [frame.stride(0), frame.stride(1), frame.stride(2)],
                        matrix,
                        primaries,
                        transfer,
                        // yuvj420p is full range whatever the field says.
                        range: if frame.format() == Pixel::YUVJ420P { 2 } else { range },
                    });
                    *count += 1;
                }
                Err(ff::Error::Other { errno }) if errno == ff::error::EAGAIN => return Ok(()),
                Err(ff::Error::Eof) => return Ok(()),
                Err(e) => return Err(DecodeError::Failed(format!("receive_frame after {count} frames: {e}"))),
            }
        }
    };
    for p in packets {
        let mut packet = ff::Packet::copy(&p.data);
        packet.set_pts(Some(p.pts));
        packet.set_dts(Some(p.pts));
        decoder
            .send_packet(&packet)
            .map_err(|e| DecodeError::Failed(format!("send_packet at pts {}: {e}", p.pts)))?;
        drain(&mut decoder, &mut count)?;
    }
    decoder.send_eof().map_err(|e| DecodeError::Failed(format!("send_eof: {e}")))?;
    drain(&mut decoder, &mut count)?;
    Ok(count)
}

/// The YCbCr matrix a description names, as a receiver reads it: BT.601
/// for code points 5 / 6, BT.709 for 1 and for "unspecified" on an HD
/// picture (what Chromium and the GPUI client's decoder assume).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Matrix {
    Bt601,
    Bt709,
}

impl Matrix {
    pub fn of(code: i32, height: usize) -> Matrix {
        match code {
            5 | 6 => Matrix::Bt601,
            1 => Matrix::Bt709,
            _ if height < 720 => Matrix::Bt601,
            _ => Matrix::Bt709,
        }
    }

    /// (Kr, Kb).
    fn k(self) -> (f64, f64) {
        match self {
            Matrix::Bt601 => (0.299, 0.114),
            Matrix::Bt709 => (0.2126, 0.0722),
        }
    }

    /// Full-range R'G'B' to 8-bit Y'CbCr, unrounded (studio range unless `full`).
    pub fn to_yuv(self, rgb: [f64; 3], full: bool) -> [f64; 3] {
        let (kr, kb) = self.k();
        let y = kr * rgb[0] + (1.0 - kr - kb) * rgb[1] + kb * rgb[2];
        let (cb, cr) = ((rgb[2] - y) / (2.0 * (1.0 - kb)), (rgb[0] - y) / (2.0 * (1.0 - kr)));
        if full {
            [y, 128.0 + cb, 128.0 + cr]
        } else {
            [16.0 + y * 219.0 / 255.0, 128.0 + cb * 224.0 / 255.0, 128.0 + cr * 224.0 / 255.0]
        }
    }

    /// 8-bit Y'CbCr back to full-range R'G'B', unclamped.
    pub fn to_rgb(self, yuv: [f64; 3], full: bool) -> [f64; 3] {
        let (kr, kb) = self.k();
        let (y, cb, cr) = if full {
            (yuv[0], yuv[1] - 128.0, yuv[2] - 128.0)
        } else {
            ((yuv[0] - 16.0) * 255.0 / 219.0, (yuv[1] - 128.0) * 255.0 / 224.0, (yuv[2] - 128.0) * 255.0 / 224.0)
        };
        let r = y + 2.0 * (1.0 - kr) * cr;
        let b = y + 2.0 * (1.0 - kb) * cb;
        let g = (y - kr * r - kb * b) / (1.0 - kr - kb);
        [r, g, b]
    }
}

/// What a correct conversion of a BGRA frame gives: tightly packed Y, and
/// U / V averaged over each 2×2 block (chroma sited in the block's middle;
/// an encoder that sites it elsewhere differs slightly on hard colour
/// edges, the same for every setting of that encoder).
pub fn reference_yuv(bgra: &[u8], stride: usize, w: usize, h: usize, matrix: Matrix, full: bool) -> [Vec<u8>; 3] {
    let (cw, ch) = (w.div_ceil(2), h.div_ceil(2));
    let mut y = vec![0u8; w * h];
    let mut sums = vec![[0f32; 2]; cw * ch];
    for row in 0..h {
        let src = &bgra[row * stride..][..w * 4];
        for (x, px) in src.chunks_exact(4).enumerate() {
            let yuv = matrix.to_yuv([px[2] as f64, px[1] as f64, px[0] as f64], full);
            y[row * w + x] = yuv[0].round().clamp(0.0, 255.0) as u8;
            let c = &mut sums[(row / 2) * cw + x / 2];
            c[0] += yuv[1] as f32;
            c[1] += yuv[2] as f32;
        }
    }
    let mut u = vec![0u8; cw * ch];
    let mut v = vec![0u8; cw * ch];
    for cy in 0..ch {
        for cx in 0..cw {
            // Blocks on an odd right / bottom edge hold fewer pixels.
            let n = ((w - cx * 2).min(2) * (h - cy * 2).min(2)) as f32;
            let c = sums[cy * cw + cx];
            u[cy * cw + cx] = (c[0] / n).round().clamp(0.0, 255.0) as u8;
            v[cy * cw + cx] = (c[1] / n).round().clamp(0.0, 255.0) as u8;
        }
    }
    [y, u, v]
}

/// A decoded picture as the RGB a viewer sees (chroma bilinearly
/// upsampled, the stream's own matrix and range), tightly packed RGB24.
pub fn picture_rgb(p: &Picture) -> Vec<u8> {
    let matrix = Matrix::of(p.matrix, p.h);
    let full = p.range == 2;
    let (cw, ch) = (p.w.div_ceil(2), p.h.div_ceil(2));
    let chroma = |plane: usize, x: usize, y: usize| -> f64 {
        // Sample position in chroma coordinates, sited mid-block.
        let fx = ((x as f64 - 0.5) / 2.0).clamp(0.0, (cw - 1) as f64);
        let fy = ((y as f64 - 0.5) / 2.0).clamp(0.0, (ch - 1) as f64);
        let (x0, y0) = (fx as usize, fy as usize);
        let (x1, y1) = ((x0 + 1).min(cw - 1), (y0 + 1).min(ch - 1));
        let (ax, ay) = (fx - x0 as f64, fy - y0 as f64);
        let at = |xx: usize, yy: usize| p.planes[plane][yy * p.strides[plane] + xx] as f64;
        (at(x0, y0) * (1.0 - ax) + at(x1, y0) * ax) * (1.0 - ay) + (at(x0, y1) * (1.0 - ax) + at(x1, y1) * ax) * ay
    };
    let mut rgb = vec![0u8; p.w * p.h * 3];
    for y in 0..p.h {
        for x in 0..p.w {
            let yuv = [p.planes[0][y * p.strides[0] + x] as f64, chroma(1, x, y), chroma(2, x, y)];
            let c = matrix.to_rgb(yuv, full);
            for i in 0..3 {
                rgb[(y * p.w + x) * 3 + i] = c[i].round().clamp(0.0, 255.0) as u8;
            }
        }
    }
    rgb
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_screen_moves_and_counts() {
        let screen = Screen::new(320, 180, 3);
        let mut a = vec![0u8; screen.frame_bytes()];
        let mut b = vec![0u8; screen.frame_bytes()];
        screen.render(0, &mut a);
        screen.render(1, &mut b);
        assert_ne!(a, b);
        let mut again = vec![0u8; screen.frame_bytes()];
        screen.render(0, &mut again);
        assert_eq!(a, again, "deterministic");
        // Text is dark on light: both ends of the range are in the luma.
        let y = screen.luma(&a);
        assert_eq!(y.len(), 320 * 180);
        assert!(y.iter().any(|v| *v < 40) && y.iter().any(|v| *v > 200));
        // A still screen only changes its counter and its window.
        let still = Screen::new(320, 180, 0);
        still.render(0, &mut a);
        still.render(1, &mut b);
        let changed = a.chunks(4).zip(b.chunks(4)).filter(|(p, q)| p != q).count();
        assert!(changed > 0 && changed < 320 * 180 / 4, "{changed} pixels changed");
    }

    #[test]
    fn psnr_and_ssim_tell_same_from_different() {
        let a: Vec<u8> = (0..64 * 64).map(|i| ((i * 7) % 251) as u8).collect();
        assert_eq!(psnr(&a, &a), 99.0);
        assert!((ssim(&a, &a, 64, 64) - 1.0).abs() < 1e-9);
        let b: Vec<u8> = a.iter().map(|v| v.wrapping_add(40)).collect();
        assert!(psnr(&a, &b) < 25.0);
        let noise: Vec<u8> = (0..64 * 64).map(|i| (hash(i, 3) & 0xFF) as u8).collect();
        assert!(ssim(&a, &noise, 64, 64) < 0.3);
    }

    #[test]
    fn av1_sequence_header_is_found_behind_a_temporal_delimiter() {
        // TD (type 2, has_size, size 0), then a sequence header (type 1,
        // size 3), then a frame OBU (type 6).
        let tu = [0x12, 0x00, 0x0A, 0x03, 1, 2, 3, 0x32, 0x02, 9, 9];
        assert!(av1_has_sequence_header(&tu));
        // TD + frame only: a delta frame.
        assert!(!av1_has_sequence_header(&[0x12, 0x00, 0x32, 0x02, 9, 9]));
        // An OBU with an extension byte and a two-byte leb128 size is skipped whole.
        let mut long = vec![0x36, 0x00, 0x81, 0x01];
        long.extend(std::iter::repeat_n(0u8, 129));
        long.extend([0x0A, 0x01, 0]);
        assert!(av1_has_sequence_header(&long));
        assert!(!av1_has_sequence_header(&[]));
        // A truncated size never reads past the end.
        assert!(!av1_has_sequence_header(&[0x12, 0x80]));
    }

    fn packet(data: &[u8], key: bool, pts: i64) -> Packet {
        Packet { data: data.to_vec(), key, pts, description: None }
    }

    #[test]
    fn keyframes_must_carry_their_parameter_sets() {
        let h264_key = [0, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3];
        let ok = packet(&h264_key, true, 0);
        assert!(keyframe_is_self_contained(CodecKind::H264Hw, Framing::AnnexB, &ok).is_ok());
        let bare = packet(&[0, 0, 1, 0x65, 3], true, 0);
        assert!(keyframe_is_self_contained(CodecKind::H264Hw, Framing::AnnexB, &bare).unwrap_err().contains("SPS"));
        // HEVC Annex B: VPS (0x40), SPS (0x42), PPS (0x44).
        let hevc = packet(&[0, 0, 1, 0x40, 1, 0, 0, 1, 0x42, 1, 0, 0, 1, 0x44, 1, 0, 0, 1, 0x26, 1], true, 0);
        assert!(keyframe_is_self_contained(CodecKind::H265, Framing::AnnexB, &hevc).is_ok());
        // HEVC length-prefixed: needs the hvcC and an IRAP slice (type 19 = 0x26).
        let mut lp = packet(&[0, 0, 0, 2, 0x26, 1], true, 0);
        assert!(keyframe_is_self_contained(CodecKind::H265, Framing::LengthPrefixed, &lp).is_err());
        let mut hvcc = vec![0u8; 23];
        hvcc[0] = 1;
        hvcc[22] = 3;
        lp.description = Some(hvcc);
        assert!(keyframe_is_self_contained(CodecKind::H265, Framing::LengthPrefixed, &lp).is_ok());
        let av1 = packet(&[0x12, 0x00, 0x32, 0x01, 9], true, 0);
        assert!(keyframe_is_self_contained(CodecKind::Av1, Framing::AnnexB, &av1).is_err());
    }

    #[test]
    fn packet_checks_catch_each_fault() {
        let key = [0u8, 0, 0, 1, 0x67, 1, 0, 0, 0, 1, 0x68, 2, 0, 0, 1, 0x65, 3];
        let delta = [0u8, 0, 1, 0x41, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3, 3];
        let stream = |keys: &[usize], n: usize| -> Vec<Packet> {
            (0..n).map(|i| if keys.contains(&i) { packet(&key, true, i as i64) } else { packet(&delta, false, i as i64) }).collect()
        };
        // 17 bytes a frame at 10 fps = 1.36 kbps.
        let expect = |forced: Vec<usize>| Expect {
            codec: CodecKind::H264Hw,
            framing: Framing::AnnexB,
            frames: 20,
            fps: 10,
            bitrate_kbps: 1,
            forced_at: forced,
            forced_within: 2,
            max_overshoot: 0.5,
            min_share: 0.5,
        };
        let good = stream(&[0, 11], 20);
        let s = check_packets(&good, &expect(vec![10])).expect("a sound stream");
        assert_eq!((s.packets, s.keyframes), (20, 2));
        assert!((s.kbps - 1.36).abs() < 0.01);
        // The requested keyframe came too late.
        assert!(check_packets(&stream(&[0, 14], 20), &expect(vec![10])).unwrap_err().contains("requested at frame 10"));
        // A frame is missing.
        assert!(check_packets(&good[..19], &expect(vec![])).unwrap_err().contains("19 packets"));
        // Not starting on a keyframe.
        assert!(check_packets(&stream(&[5], 20), &expect(vec![])).is_err());
        // Timestamps going back.
        let mut back = good.clone();
        back[7].pts = 3;
        assert!(check_packets(&back, &expect(vec![])).unwrap_err().contains("timestamps"));
        // Over and under the bitrate.
        let mut tight = expect(vec![]);
        tight.max_overshoot = 0.1;
        assert!(check_packets(&good, &tight).unwrap_err().contains("over"));
        let mut starved = expect(vec![]);
        starved.bitrate_kbps = 10;
        assert!(check_packets(&good, &starved).unwrap_err().contains("under"));
    }

    #[test]
    fn decoding_without_a_decoder_says_so() {
        // Whatever the build has, an empty H.264 stream decodes to nothing
        // or reports the missing decoder: never a panic.
        match decode(CodecKind::H264Hw, Framing::AnnexB, &[], |_, _, _, _, _| {}) {
            Ok(n) => assert_eq!(n, 0),
            Err(DecodeError::NoDecoder(name)) => assert_eq!(name, "h264"),
            Err(DecodeError::Failed(e)) => panic!("{e}"),
        }
    }
}
