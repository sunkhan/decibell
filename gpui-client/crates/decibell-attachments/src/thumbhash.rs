//! ThumbHash, a port of the `thumbhash` npm package (0.1.1, Evan Wallace, MIT) that the renderer
//! uses (`electron-client/src/features/chat/thumbhash.ts`). Bit-identical to it for the same
//! RGBA input: every expression keeps the JS evaluation order (no FMA, no reassociation) and
//! `Math.round` is reproduced exactly ([`js_round`]). The hash rides the attachment row as
//! base64 (`placeholder`); the server never decodes it.
//!
//! What is *not* identical across clients is the step before: the renderer downscales the
//! image to ≤100 px with Chromium's canvas `drawImage`, this crate with the `image` crate
//! (`thumbs.rs`), so two clients hashing the same photo can differ in the last bits.

use base64::Engine as _;
use std::f64::consts::PI;

/// The encoder's input limit (both edges).
pub const MAX_EDGE: u32 = 100;
/// Hashes longer than this are refused by the decoder (`thumbhash.ts` MAX_HASH_BYTES).
pub const MAX_HASH_BYTES: usize = 64;

/// JavaScript's `Math.round`: nearest integer, ties toward +∞.
#[inline]
pub(crate) fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 {
        f + 1.0
    } else {
        f
    }
}

/// `rgbaToThumbHash(w, h, rgba)`: RGBA (not premultiplied), `w, h <= 100`.
pub fn encode(w: usize, h: usize, rgba: &[u8]) -> Vec<u8> {
    assert!(w <= 100 && h <= 100, "{w}x{h} doesn't fit in 100x100");
    assert!(w > 0 && h > 0 && rgba.len() >= w * h * 4);
    let n = w * h;

    let (mut avg_r, mut avg_g, mut avg_b, mut avg_a) = (0.0f64, 0.0f64, 0.0f64, 0.0f64);
    for i in 0..n {
        let j = i * 4;
        let alpha = rgba[j + 3] as f64 / 255.0;
        avg_r += alpha / 255.0 * rgba[j] as f64;
        avg_g += alpha / 255.0 * rgba[j + 1] as f64;
        avg_b += alpha / 255.0 * rgba[j + 2] as f64;
        avg_a += alpha;
    }
    if avg_a != 0.0 {
        avg_r /= avg_a;
        avg_g /= avg_a;
        avg_b /= avg_a;
    }

    let has_alpha = avg_a < n as f64;
    let l_limit: f64 = if has_alpha { 5.0 } else { 7.0 };
    let wf = w as f64;
    let hf = h as f64;
    let maxwh = w.max(h) as f64;
    let lx = (js_round(l_limit * wf / maxwh) as i64).max(1);
    let ly = (js_round(l_limit * hf / maxwh) as i64).max(1);
    let mut l = vec![0.0f64; n];
    let mut p = vec![0.0f64; n];
    let mut q = vec![0.0f64; n];
    let mut a = vec![0.0f64; n];

    for i in 0..n {
        let j = i * 4;
        let alpha = rgba[j + 3] as f64 / 255.0;
        let r = avg_r * (1.0 - alpha) + alpha / 255.0 * rgba[j] as f64;
        let g = avg_g * (1.0 - alpha) + alpha / 255.0 * rgba[j + 1] as f64;
        let b = avg_b * (1.0 - alpha) + alpha / 255.0 * rgba[j + 2] as f64;
        l[i] = (r + g + b) / 3.0;
        p[i] = (r + g) / 2.0 - b;
        q[i] = r - g;
        a[i] = alpha;
    }

    let mut fx = vec![0.0f64; w];
    let mut encode_channel = |channel: &[f64], nx: i64, ny: i64| -> (f64, Vec<f64>, f64) {
        let mut dc = 0.0;
        let mut ac = Vec::new();
        let mut scale: f64 = 0.0;
        for cy in 0..ny {
            let mut cx = 0i64;
            while cx * ny < nx * (ny - cy) {
                let mut f = 0.0f64;
                for (x, slot) in fx.iter_mut().enumerate() {
                    *slot = (PI / wf * cx as f64 * (x as f64 + 0.5)).cos();
                }
                for y in 0..h {
                    let fy = (PI / hf * cy as f64 * (y as f64 + 0.5)).cos();
                    for x in 0..w {
                        f += channel[x + y * w] * fx[x] * fy;
                    }
                }
                f /= (w * h) as f64;
                if cx != 0 || cy != 0 {
                    ac.push(f);
                    scale = scale.max(f.abs());
                } else {
                    dc = f;
                }
                cx += 1;
            }
        }
        if scale != 0.0 {
            for v in ac.iter_mut() {
                *v = 0.5 + 0.5 / scale * *v;
            }
        }
        (dc, ac, scale)
    };
    let (l_dc, l_ac, l_scale) = encode_channel(&l, lx.max(3), ly.max(3));
    let (p_dc, p_ac, p_scale) = encode_channel(&p, 3, 3);
    let (q_dc, q_ac, q_scale) = encode_channel(&q, 3, 3);
    let alpha_part = if has_alpha { Some(encode_channel(&a, 5, 5)) } else { None };

    // JS bitwise operators work on ToInt32 of these (small, non-negative) values.
    let r = |x: f64| js_round(x) as i32;
    let is_landscape = w > h;
    let header24: i32 = r(63.0 * l_dc)
        | (r(31.5 + 31.5 * p_dc) << 6)
        | (r(31.5 + 31.5 * q_dc) << 12)
        | (r(31.0 * l_scale) << 18)
        | ((has_alpha as i32) << 23);
    let header16: i32 = (if is_landscape { ly as i32 } else { lx as i32 })
        | (r(63.0 * p_scale) << 3)
        | (r(63.0 * q_scale) << 9)
        | ((is_landscape as i32) << 15);
    let mut hash: Vec<i32> = vec![
        header24 & 255,
        (header24 >> 8) & 255,
        header24 >> 16,
        header16 & 255,
        header16 >> 8,
    ];
    let ac_start = if has_alpha { 6 } else { 5 };
    let mut ac_index = 0usize;
    if let Some((a_dc, _, a_scale)) = &alpha_part {
        hash.push(r(15.0 * a_dc) | (r(15.0 * a_scale) << 4));
    }
    let mut channels: Vec<&Vec<f64>> = vec![&l_ac, &p_ac, &q_ac];
    if let Some((_, a_ac, _)) = &alpha_part {
        channels.push(a_ac);
    }
    for ac in channels {
        for &f in ac.iter() {
            let idx = ac_start + (ac_index >> 1);
            if hash.len() <= idx {
                hash.resize(idx + 1, 0);
            }
            hash[idx] |= r(15.0 * f) << ((ac_index & 1) << 2);
            ac_index += 1;
        }
    }
    // `new Uint8Array(hash)`: ToUint8 of each element.
    hash.into_iter().map(|v| v as u8).collect()
}

/// Base64 of [`encode`], the form the `placeholder` field carries.
pub fn encode_b64(w: usize, h: usize, rgba: &[u8]) -> String {
    base64::engine::general_purpose::STANDARD.encode(encode(w, h, rgba))
}

/// A decoded placeholder: straight (not premultiplied) RGBA, about 32 px on the long edge.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Decoded {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

/// `thumbHashToRGBA(hash)`, bit-identical to the npm decoder.
pub fn decode(hash: &[u8]) -> Decoded {
    // Out-of-range reads are `undefined` in JS, which bitwise operators read as 0.
    let at = |i: usize| -> i32 { hash.get(i).copied().unwrap_or(0) as i32 };
    let header24 = at(0) | (at(1) << 8) | (at(2) << 16);
    let header16 = at(3) | (at(4) << 8);
    let l_dc = (header24 & 63) as f64 / 63.0;
    let p_dc = ((header24 >> 6) & 63) as f64 / 31.5 - 1.0;
    let q_dc = ((header24 >> 12) & 63) as f64 / 31.5 - 1.0;
    let l_scale = ((header24 >> 18) & 31) as f64 / 31.0;
    let has_alpha = (header24 >> 23) != 0;
    let p_scale = ((header16 >> 3) & 63) as f64 / 63.0;
    let q_scale = ((header16 >> 9) & 63) as f64 / 63.0;
    let is_landscape = (header16 >> 15) != 0;
    let lx = (if is_landscape { if has_alpha { 5 } else { 7 } } else { header16 & 7 }).max(3) as i64;
    let ly = (if is_landscape { header16 & 7 } else if has_alpha { 5 } else { 7 }).max(3) as i64;
    let a_dc = if has_alpha { (at(5) & 15) as f64 / 15.0 } else { 1.0 };
    let a_scale = (at(5) >> 4) as f64 / 15.0;

    let ac_start = if has_alpha { 6 } else { 5 };
    let mut ac_index = 0usize;
    let mut decode_channel = |nx: i64, ny: i64, scale: f64| -> Vec<f64> {
        let mut ac = Vec::new();
        for cy in 0..ny {
            let mut cx = if cy != 0 { 0 } else { 1 };
            while cx * ny < nx * (ny - cy) {
                let v = (at(ac_start + (ac_index >> 1)) >> ((ac_index & 1) << 2)) & 15;
                ac_index += 1;
                ac.push((v as f64 / 7.5 - 1.0) * scale);
                cx += 1;
            }
        }
        ac
    };
    let l_ac = decode_channel(lx, ly, l_scale);
    let p_ac = decode_channel(3, 3, p_scale * 1.25);
    let q_ac = decode_channel(3, 3, q_scale * 1.25);
    let a_ac = if has_alpha { decode_channel(5, 5, a_scale) } else { Vec::new() };

    let ratio = approximate_aspect_ratio(hash);
    let w = js_round(if ratio > 1.0 { 32.0 } else { 32.0 * ratio }) as usize;
    let h = js_round(if ratio > 1.0 { 32.0 / ratio } else { 32.0 }) as usize;
    let mut rgba = vec![0u8; w * h * 4];
    let nfx = lx.max(if has_alpha { 5 } else { 3 }) as usize;
    let nfy = ly.max(if has_alpha { 5 } else { 3 }) as usize;
    let mut fx = vec![0.0f64; nfx];
    let mut fy = vec![0.0f64; nfy];
    // `Uint8Array[i] = max(0, 255 * min(1, v))`: JS's min / max propagate NaN (stored as 0).
    let to_u8 = |v: f64| -> u8 {
        if v.is_nan() {
            return 0;
        }
        let m = 255.0 * if v < 1.0 { v } else { 1.0 };
        if m > 0.0 {
            m as u8
        } else {
            0
        }
    };
    let mut i = 0usize;
    for y in 0..h {
        for x in 0..w {
            let mut l = l_dc;
            let mut p = p_dc;
            let mut q = q_dc;
            let mut a = a_dc;
            for (cx, slot) in fx.iter_mut().enumerate() {
                *slot = (PI / w as f64 * (x as f64 + 0.5) * cx as f64).cos();
            }
            for (cy, slot) in fy.iter_mut().enumerate() {
                *slot = (PI / h as f64 * (y as f64 + 0.5) * cy as f64).cos();
            }
            let mut j = 0usize;
            for cy in 0..ly {
                let fy2 = fy[cy as usize] * 2.0;
                let mut cx = if cy != 0 { 0 } else { 1 };
                while cx * ly < lx * (ly - cy) {
                    l += l_ac[j] * fx[cx as usize] * fy2;
                    cx += 1;
                    j += 1;
                }
            }
            let mut j = 0usize;
            for (cy, fyv) in fy.iter().enumerate().take(3) {
                let fy2 = fyv * 2.0;
                let mut cx = if cy != 0 { 0 } else { 1 };
                while cx < 3 - cy {
                    let f = fx[cx] * fy2;
                    p += p_ac[j] * f;
                    q += q_ac[j] * f;
                    cx += 1;
                    j += 1;
                }
            }
            if has_alpha {
                let mut j = 0usize;
                for (cy, fyv) in fy.iter().enumerate().take(5) {
                    let fy2 = fyv * 2.0;
                    let mut cx = if cy != 0 { 0 } else { 1 };
                    while cx < 5 - cy {
                        a += a_ac[j] * fx[cx] * fy2;
                        cx += 1;
                        j += 1;
                    }
                }
            }
            let b = l - 2.0 / 3.0 * p;
            let r = (3.0 * l - b + q) / 2.0;
            let g = r - q;
            rgba[i] = to_u8(r);
            rgba[i + 1] = to_u8(g);
            rgba[i + 2] = to_u8(b);
            rgba[i + 3] = to_u8(a);
            i += 4;
        }
    }
    Decoded { width: w as u32, height: h as u32, rgba }
}

/// `thumbHashToApproximateAspectRatio(hash)`.
pub fn approximate_aspect_ratio(hash: &[u8]) -> f64 {
    let at = |i: usize| hash.get(i).copied().unwrap_or(0);
    let header = at(3);
    let has_alpha = at(2) & 0x80 != 0;
    let is_landscape = at(4) & 0x80 != 0;
    let lx = if is_landscape { if has_alpha { 5 } else { 7 } } else { header & 7 };
    let ly = if is_landscape { header & 7 } else if has_alpha { 5 } else { 7 };
    lx as f64 / ly as f64
}

/// Decode the base64 `placeholder` of an attachment row (`thumbHashToDataUrl`'s input rules:
/// empty, malformed or longer than [`MAX_HASH_BYTES`] is `None`, the normal case for old
/// uploads and non-images).
pub fn decode_b64(b64: &str) -> Option<Decoded> {
    if b64.is_empty() {
        return None;
    }
    let bytes = base64::engine::general_purpose::STANDARD.decode(b64).ok()?;
    if bytes.is_empty() || bytes.len() > MAX_HASH_BYTES {
        return None;
    }
    Some(decode(&bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn js_round_matches_math_round() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(0.49999999999999994), 0.0);
        assert_eq!(js_round(1.4999), 1.0);
        assert_eq!(js_round(31.5), 32.0);
    }

    #[test]
    fn roundtrip_shape() {
        let (w, h) = (40, 20);
        let mut rgba = vec![0u8; w * h * 4];
        for y in 0..h {
            for x in 0..w {
                let i = (y * w + x) * 4;
                rgba[i] = (x * 6) as u8;
                rgba[i + 1] = (y * 12) as u8;
                rgba[i + 2] = 128;
                rgba[i + 3] = 255;
            }
        }
        let hash = encode(w, h, &rgba);
        assert!(hash.len() >= 5 && hash.len() <= 30);
        let d = decode(&hash);
        assert!(d.width > d.height);
        assert_eq!(d.rgba.len(), (d.width * d.height * 4) as usize);
        assert!(decode_b64("").is_none());
        assert!(decode_b64(&"A".repeat(200)).is_none());
    }
}
