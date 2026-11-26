//! Reading a picked picture and rendering the crop (`components/pictureEditor/picture.ts`).
//!
//! Everything here runs off the UI thread: [`read`] decodes the file (its EXIF orientation
//! applied, so a phone photo stored sideways comes out upright: the renderer's `cropUpright`),
//! analyses it, and prepares what the stage draws; [`render_crop`] renders the crop at the output
//! size with high-quality resampling and encodes it as JPEG under the server's 1 MB cap.

use std::io::Cursor;
use std::sync::Arc;

use gpui::RenderImage;
use image::imageops::FilterType;
use image::{DynamicImage, GenericImageView as _, ImageFormat, ImageReader, RgbImage, Rgba, RgbaImage};
use smallvec::SmallVec;

use crate::geometry::{Size, View};

/// An sRGB colour.
pub type Rgb8 = [u8; 3];

/// What's behind the picture where it doesn't fill the frame (zoomed out past cover, or
/// transparent pixels): its own colours blurred, or a flat colour. Always painted into the
/// export, so everyone sees the same thing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fill {
    Blur,
    Color(Rgb8),
}

/// Above this the decode alone takes hundreds of MB; no profile picture needs it.
pub const MAX_PIXELS: u64 = 80_000_000;

/// Blur radius of the "blur" fill, as a share of the frame's width.
pub const BLUR_SHARE: f32 = 0.06;

/// The server's cap, with the renderer's margin: `blob.size <= 1000 * 1024`.
pub const MAX_BYTES: usize = 1000 * 1024;

/// The longest side of the texture the stage and the previews draw (the export reads the
/// decoded original). 8× past cover on the largest frame needs about this much to stay sharp.
const STAGE_MAX_SIDE: u32 = 2560;

/// Why a file can't be edited, in the renderer's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PictureError {
    NotAPicture,
    Heic,
    TooLarge,
}

impl PictureError {
    pub fn message(self) -> &'static str {
        match self {
            PictureError::NotAPicture => "This file isn't a picture Decibell can open.",
            PictureError::Heic => "HEIC photos can't be opened here yet. Export it as a JPEG first.",
            PictureError::TooLarge => "This picture is too large to edit (over 80 megapixels).",
        }
    }
}

/// The decoded original, upright.
enum Pixels {
    Rgb(RgbImage),
    Rgba(RgbaImage),
}

impl Pixels {
    fn dims(&self) -> (u32, u32) {
        match self {
            Pixels::Rgb(i) => i.dimensions(),
            Pixels::Rgba(i) => i.dimensions(),
        }
    }

    /// The region `(x, y, w, h)` resampled to `out_w` × `out_h`, straight RGBA.
    fn resized_region(&self, x: u32, y: u32, w: u32, h: u32, out_w: u32, out_h: u32) -> RgbaImage {
        // Lanczos 3 for shrinking (what Chromium's high-quality scaler approximates), Catmull-Rom
        // when enlarging (no ringing halos on a small logo blown up).
        let filter = if out_w < w { FilterType::Lanczos3 } else { FilterType::CatmullRom };
        match self {
            Pixels::Rgb(i) => {
                let view = image::imageops::crop_imm(i, x, y, w, h);
                let resized = image::imageops::resize(&*view, out_w, out_h, filter);
                DynamicImage::ImageRgb8(resized).into_rgba8()
            }
            Pixels::Rgba(i) => {
                let view = image::imageops::crop_imm(i, x, y, w, h);
                image::imageops::resize(&*view, out_w, out_h, filter)
            }
        }
    }
}

/// A picture ready to edit. Cheap to clone (shared).
#[derive(Clone)]
pub struct Picture {
    /// Its size as shown (after orientation), natural pixels.
    pub nat: Size,
    /// Has transparent pixels (a logo): a fill shows through them.
    pub alpha: bool,
    /// GIF: only the first frame survives the JPEG.
    pub animated: bool,
    /// Fill swatches: the picture's main colour and a deep shade of it.
    pub palette: [Rgb8; 2],
    pub name: Arc<str>,
    source: Arc<Pixels>,
    /// What the stage and the previews draw: at most [`STAGE_MAX_SIDE`], BGRA.
    pub(crate) stage: Arc<RenderImage>,
    /// The "blur" fill: the whole picture, small, blurred and saturated (CSS `blur(…)
    /// saturate(1.2)`), BGRA for drawing and RGBA for the export.
    pub(crate) blur: Arc<RenderImage>,
    blur_rgba: Arc<RgbaImage>,
}

impl Picture {
    /// The images the GPU holds for this picture (to drop from the atlas when the editor closes).
    pub fn textures(&self) -> [Arc<RenderImage>; 2] {
        [self.stage.clone(), self.blur.clone()]
    }

    /// Bytes this picture keeps in memory (the original plus the textures' CPU copies).
    pub fn heap_bytes(&self) -> usize {
        let (w, h) = self.source.dims();
        let per = match &*self.source {
            Pixels::Rgb(_) => 3,
            Pixels::Rgba(_) => 4,
        };
        let texture = |image: &RenderImage| image.as_bytes(0).map_or(0, <[u8]>::len);
        w as usize * h as usize * per + texture(&self.stage) + texture(&self.blur) + self.blur_rgba.as_raw().len()
    }
}

fn is_heic(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower.ends_with(".heic") || lower.ends_with(".heif")
}

/// A picture file's name tells us to try it: the renderer's `pictureFromList` (any `image/*`, or
/// these extensions). Without a MIME type here, the extension (or none at all: then the decoder
/// decides).
pub fn looks_like_picture(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    let ext = lower.rsplit_once('.').map_or("", |(_, e)| e);
    matches!(ext, "jpg" | "jpeg" | "png" | "webp" | "gif" | "avif" | "bmp" | "heic" | "heif")
}

/// Decode, orient and analyse a picked file. `aspect`: the frame's (output width / height), for
/// the blur fill.
pub fn read(bytes: &[u8], name: &str, aspect: f32) -> Result<Picture, PictureError> {
    let fail = || if is_heic(name) { PictureError::Heic } else { PictureError::NotAPicture };
    let reader = ImageReader::new(Cursor::new(bytes)).with_guessed_format().map_err(|_| fail())?;
    let format = reader.format().ok_or_else(fail)?;
    let mut decoder = reader.into_decoder().map_err(|_| fail())?;
    let (w, h) = image::ImageDecoder::dimensions(&decoder);
    if w == 0 || h == 0 {
        return Err(PictureError::NotAPicture);
    }
    if w as u64 * h as u64 > MAX_PIXELS {
        return Err(PictureError::TooLarge);
    }
    let orientation = image::ImageDecoder::orientation(&mut decoder).unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|_| fail())?;
    img.apply_orientation(orientation);
    let (alpha, palette) = analyse(&img);
    let source = if img.color().has_alpha() { Pixels::Rgba(img.to_rgba8()) } else { Pixels::Rgb(img.to_rgb8()) };
    drop(img);
    let (w, h) = source.dims();
    let nat = Size::new(w as f32, h as f32);
    let stage = stage_texture(&source);
    let blur_rgba = blur_layer(&source, nat, aspect);
    let blur = bgra_texture(&blur_rgba);
    Ok(Picture {
        nat,
        alpha,
        animated: format == ImageFormat::Gif,
        palette,
        name: name.into(),
        source: Arc::new(source),
        stage,
        blur,
        blur_rgba: Arc::new(blur_rgba),
    })
}

/// Transparency and the main colour, from a 48 px copy. Most common 4-bit bucket among opaque
/// pixels, saturated ones counted double so a grey border doesn't win over the subject.
fn analyse(img: &DynamicImage) -> (bool, [Rgb8; 2]) {
    let n = 48u32;
    let (nw, nh) = img.dimensions();
    let round = |v: f32| (v + 0.5).floor().max(1.) as u32;
    let (w, h) = if nw >= nh { (n, round((n * nh) as f32 / nw as f32)) } else { (round((n * nw) as f32 / nh as f32), n) };
    let small = img.resize_exact(w, h, FilterType::Triangle).to_rgba8();
    let mut votes: std::collections::HashMap<u32, (u32, u32, u32, u32)> = std::collections::HashMap::new();
    let mut alpha = false;
    for p in small.pixels() {
        let [r, g, b, a] = p.0;
        if a < 250 {
            alpha = true;
        }
        if a < 128 {
            continue;
        }
        let key = ((r as u32 >> 4) << 8) | ((g as u32 >> 4) << 4) | (b as u32 >> 4);
        let sat = r.max(g).max(b) - r.min(g).min(b);
        let weight = if sat > 48 { 2 } else { 1 };
        let v = votes.entry(key).or_insert((0, 0, 0, 0));
        v.0 += weight;
        v.1 += r as u32 * weight;
        v.2 += g as u32 * weight;
        v.3 += b as u32 * weight;
    }
    // Ties keep the first bucket seen in pixel order, as the renderer's Map iteration does.
    let mut order: Vec<(u32, (u32, u32, u32, u32))> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for p in small.pixels() {
        let [r, g, b, a] = p.0;
        if a < 128 {
            continue;
        }
        let key = ((r as u32 >> 4) << 8) | ((g as u32 >> 4) << 4) | (b as u32 >> 4);
        if seen.insert(key) {
            order.push((key, votes[&key]));
        }
    }
    let mut best = (0u32, 128, 128, 128);
    let mut best_n = 0;
    for (_, v) in &order {
        if v.0 > best_n {
            best_n = v.0;
            best = *v;
        }
    }
    let rgb = if best_n > 0 {
        [best.1 as f32 / best.0 as f32, best.2 as f32 / best.0 as f32, best.3 as f32 / best.0 as f32]
    } else {
        [128., 128., 128.]
    };
    (alpha, [to_rgb8(rgb), to_rgb8(deep(rgb))])
}

/// The same hue, dark enough to sit behind a logo.
fn deep([r, g, b]: [f32; 3]) -> [f32; 3] {
    let max = r.max(g).max(b);
    let max = if max == 0. { 1. } else { max };
    let k = 46. / max;
    [r * k + 10., g * k + 10., b * k + 12.]
}

fn to_rgb8(c: [f32; 3]) -> Rgb8 {
    c.map(|v| (v.clamp(0., 255.) + 0.5).floor() as u8)
}

/// `#rrggbb`.
pub fn hex(c: Rgb8) -> String {
    format!("#{:02x}{:02x}{:02x}", c[0], c[1], c[2])
}

/// Straight RGBA → the BGRA GPUI draws.
fn bgra_texture(rgba: &RgbaImage) -> Arc<RenderImage> {
    let (w, h) = rgba.dimensions();
    let mut pixels = rgba.as_raw().clone();
    for p in pixels.as_chunks_mut::<4>().0 {
        p.swap(0, 2);
    }
    let buffer = RgbaImage::from_raw(w, h, pixels).expect("same size");
    Arc::new(RenderImage::new(SmallVec::from_elem(image::Frame::new(buffer), 1)))
}

fn stage_texture(source: &Pixels) -> Arc<RenderImage> {
    let (w, h) = source.dims();
    let longest = w.max(h);
    let (tw, th) = if longest > STAGE_MAX_SIDE {
        let k = STAGE_MAX_SIDE as f32 / longest as f32;
        (((w as f32 * k).round() as u32).max(1), ((h as f32 * k).round() as u32).max(1))
    } else {
        (w, h)
    };
    let rgba = if (tw, th) == (w, h) {
        match source {
            Pixels::Rgb(i) => DynamicImage::ImageRgb8(i.clone()).into_rgba8(),
            Pixels::Rgba(i) => i.clone(),
        }
    } else {
        source.resized_region(0, 0, w, h, tw, th)
    };
    bgra_texture(&rgba)
}

/// The blur fill's picture: the whole picture, small enough that the part covering the frame
/// is about 160 px wide, blurred by the share of the frame's width the renderer blurs by (in
/// that scale), then saturated by 1.2 (CSS `saturate`'s matrix).
fn blur_layer(source: &Pixels, nat: Size, aspect: f32) -> RgbaImage {
    // The part of the picture the frame's cover × 1.1 spans, in natural px.
    let spanned = nat.w.min(aspect * nat.h) / 1.1;
    let b = (176. / (spanned * 1.1)).min(1.).min(1024. / nat.w.max(nat.h));
    let (w, h) = (((nat.w * b).round() as u32).max(1), ((nat.h * b).round() as u32).max(1));
    let (sw, sh) = source.dims();
    let small = source.resized_region(0, 0, sw, sh, w, h);
    let sigma = (BLUR_SHARE * spanned * b).max(0.5);
    let mut blurred = image::imageops::blur(&small, sigma);
    saturate(&mut blurred, 1.2);
    blurred
}

/// CSS `saturate(s)`.
fn saturate(img: &mut RgbaImage, s: f32) {
    let m = [
        [0.213 + 0.787 * s, 0.715 - 0.715 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 + 0.285 * s, 0.072 - 0.072 * s],
        [0.213 - 0.213 * s, 0.715 - 0.715 * s, 0.072 + 0.928 * s],
    ];
    for p in img.pixels_mut() {
        let [r, g, b, a] = p.0;
        let (r, g, b) = (r as f32, g as f32, b as f32);
        let c = |row: [f32; 3]| (row[0] * r + row[1] * g + row[2] * b).round().clamp(0., 255.) as u8;
        *p = Rgba([c(m[0]), c(m[1]), c(m[2]), a]);
    }
}

/// `src` over `dst`, straight alpha.
fn over(dst: &mut Rgba<u8>, src: Rgba<u8>) {
    let a = src.0[3] as u32;
    if a == 0 {
        return;
    }
    if a == 255 {
        *dst = src;
        return;
    }
    let da = dst.0[3] as u32;
    // The canvas is opaque under everything we draw (the fill is painted first).
    for c in 0..3 {
        dst.0[c] = ((src.0[c] as u32 * a + dst.0[c] as u32 * (255 - a) + 127) / 255) as u8;
    }
    dst.0[3] = (a + da * (255 - a) / 255).min(255) as u8;
}

/// Draw `img` stretched over the rectangle `(x, y, w, h)` of `canvas` (bilinear).
fn draw_scaled(canvas: &mut RgbaImage, img: &RgbaImage, x: f32, y: f32, w: f32, h: f32) {
    let (cw, ch) = canvas.dimensions();
    let (iw, ih) = img.dimensions();
    let x0 = x.max(0.).floor() as u32;
    let y0 = y.max(0.).floor() as u32;
    let x1 = ((x + w).min(cw as f32)).ceil().max(0.) as u32;
    let y1 = ((y + h).min(ch as f32)).ceil().max(0.) as u32;
    for py in y0..y1.min(ch) {
        let v = ((py as f32 + 0.5 - y) / h * ih as f32 - 0.5).clamp(0., ih as f32 - 1.);
        let (v0, fv) = (v.floor() as u32, v.fract());
        let v1 = (v0 + 1).min(ih - 1);
        for px in x0..x1.min(cw) {
            let u = ((px as f32 + 0.5 - x) / w * iw as f32 - 0.5).clamp(0., iw as f32 - 1.);
            let (u0, fu) = (u.floor() as u32, u.fract());
            let u1 = (u0 + 1).min(iw - 1);
            let s = |xx, yy| img.get_pixel(xx, yy).0.map(|c| c as f32);
            let (a, b, c, d) = (s(u0, v0), s(u1, v0), s(u0, v1), s(u1, v1));
            let mut out = [0u8; 4];
            for i in 0..4 {
                let top = a[i] + (b[i] - a[i]) * fu;
                let bottom = c[i] + (d[i] - c[i]) * fu;
                out[i] = (top + (bottom - top) * fv).round().clamp(0., 255.) as u8;
            }
            over(canvas.get_pixel_mut(px, py), Rgba(out));
        }
    }
}

/// The crop as a JPEG of `out` pixels. Steps the quality down until it fits under the server's
/// 1 MB cap (a noisy 880 px banner can approach it).
pub fn render_crop(p: &Picture, view: View, frame: Size, out: (u32, u32), fill: Fill) -> Result<Vec<u8>, String> {
    let canvas = render_pixels(p, view, frame, out, fill);
    let rgb = DynamicImage::ImageRgba8(canvas).into_rgb8();
    let mut last = Vec::new();
    for quality in [90u8, 82, 74, 66] {
        let mut bytes = Vec::new();
        let encoder = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut bytes, quality);
        rgb.write_with_encoder(encoder).map_err(|e| e.to_string())?;
        if bytes.len() <= MAX_BYTES || quality == 66 {
            return Ok(bytes);
        }
        last = bytes;
    }
    Ok(last)
}

/// The crop's pixels at the output size (what [`render_crop`] encodes).
pub fn render_pixels(p: &Picture, view: View, frame: Size, out: (u32, u32), fill: Fill) -> RgbaImage {
    let (ow, oh) = (out.0.max(1), out.1.max(1));
    // Under everything: JPEG has no transparency, and an unpainted pixel would turn black.
    let bg = match fill {
        Fill::Color(c) => c,
        Fill::Blur => p.palette[1],
    };
    let mut canvas = RgbaImage::from_pixel(ow, oh, Rgba([bg[0], bg[1], bg[2], 255]));
    let (fw, fh) = (ow as f32, oh as f32);
    if fill == Fill::Blur {
        // The picture covering the frame, blurred; drawn 10% large so the blur's faded edge
        // falls outside.
        let s = (fw / p.nat.w).max(fh / p.nat.h) * 1.1;
        draw_scaled(&mut canvas, &p.blur_rgba, (fw - p.nat.w * s) / 2., (fh - p.nat.h * s) / 2., p.nat.w * s, p.nat.h * s);
    }
    // Only the part of the picture inside the frame, resampled once.
    let k = fw / frame.w;
    let s = view.scale * k;
    let (left, top) = (view.x * k, view.y * k);
    let dx0 = left.max(0.);
    let dy0 = top.max(0.);
    let dx1 = fw.min(left + p.nat.w * s);
    let dy1 = fh.min(top + p.nat.h * s);
    if dx1 - dx0 < 0.5 || dy1 - dy0 < 0.5 {
        return canvas;
    }
    let (nw, nh) = (p.nat.w as u32, p.nat.h as u32);
    let sx0 = (((dx0 - left) / s).floor().max(0.) as u32).min(nw - 1);
    let sy0 = (((dy0 - top) / s).floor().max(0.) as u32).min(nh - 1);
    let sx1 = (((dx1 - left) / s).ceil() as u32).clamp(sx0 + 1, nw);
    let sy1 = (((dy1 - top) / s).ceil() as u32).clamp(sy0 + 1, nh);
    let rw = (((sx1 - sx0) as f32 * s).round() as u32).max(1);
    let rh = (((sy1 - sy0) as f32 * s).round() as u32).max(1);
    let region = p.source.resized_region(sx0, sy0, sx1 - sx0, sy1 - sy0, rw, rh);
    let ox = (left + sx0 as f32 * s).round() as i64;
    let oy = (top + sy0 as f32 * s).round() as i64;
    // Clipped to the visible part (and the canvas).
    let (cx0, cy0) = (dx0.round() as i64, dy0.round() as i64);
    let (cx1, cy1) = ((dx1.round() as i64).min(ow as i64), (dy1.round() as i64).min(oh as i64));
    for (rx, ry, px) in region.enumerate_pixels() {
        let (x, y) = (ox + rx as i64, oy + ry as i64);
        if x < cx0 || y < cy0 || x >= cx1 || y >= cy1 {
            continue;
        }
        over(canvas.get_pixel_mut(x as u32, y as u32), *px);
    }
    canvas
}

/// A picture from a solid RGB buffer, for tests and the screens: no decode.
#[cfg(any(test, feature = "fixtures"))]
pub fn from_rgba(rgba: RgbaImage, name: &str, aspect: f32) -> Picture {
    let img = DynamicImage::ImageRgba8(rgba);
    let (alpha, palette) = analyse(&img);
    let source = if alpha { Pixels::Rgba(img.to_rgba8()) } else { Pixels::Rgb(img.to_rgb8()) };
    let (w, h) = source.dims();
    let nat = Size::new(w as f32, h as f32);
    let stage = stage_texture(&source);
    let blur_rgba = blur_layer(&source, nat, aspect);
    let blur = bgra_texture(&blur_rgba);
    Picture {
        nat,
        alpha,
        animated: false,
        palette,
        name: name.into(),
        source: Arc::new(source),
        stage,
        blur,
        blur_rgba: Arc::new(blur_rgba),
    }
}

/// A test card: a diagonal gradient with a dark disc and a light band, `w` × `h`. Deterministic.
#[cfg(any(test, feature = "fixtures"))]
pub fn sample_rgba(w: u32, h: u32) -> RgbaImage {
    RgbaImage::from_fn(w, h, |x, y| {
        let t = (x as f32 / w as f32 + y as f32 / h as f32) / 2.;
        let lerp = |a: f32, b: f32| (a + (b - a) * t).round() as u8;
        let (cx, cy) = (w as f32 * 0.38, h as f32 * 0.45);
        let r = (w.min(h) as f32) * 0.22;
        let d = ((x as f32 - cx).powi(2) + (y as f32 - cy).powi(2)).sqrt();
        if d < r {
            Rgba([36, 28, 52, 255])
        } else if (y as f32 / h as f32 - 0.72).abs() < 0.04 {
            Rgba([246, 236, 214, 255])
        } else {
            Rgba([lerp(232., 58.), lerp(120., 74.), lerp(84., 168.), 255])
        }
    })
}

#[cfg(test)]
mod tests {
    use image::Rgb;
    use super::*;
    use crate::geometry::{initial_view, range, clamp_view, cover_scale, contain_scale};

    fn png(img: &RgbaImage) -> Vec<u8> {
        let mut out = Vec::new();
        DynamicImage::ImageRgba8(img.clone()).write_to(&mut Cursor::new(&mut out), ImageFormat::Png).unwrap();
        out
    }

    fn jpeg(img: &RgbImage) -> Vec<u8> {
        let mut out = Vec::new();
        let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, 95);
        img.write_with_encoder(enc).unwrap();
        out
    }

    /// Insert an APP1 Exif segment holding only an Orientation tag after SOI (the attachments
    /// crate's helper).
    fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM\0\x2a\0\0\0\x08");
        tiff.extend_from_slice(&1u16.to_be_bytes());
        tiff.extend_from_slice(&0x0112u16.to_be_bytes());
        tiff.extend_from_slice(&3u16.to_be_bytes());
        tiff.extend_from_slice(&1u32.to_be_bytes());
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes());
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }

    fn near(a: [u8; 3], b: [u8; 3], tol: i32) -> bool {
        a.iter().zip(b.iter()).all(|(x, y)| (*x as i32 - *y as i32).abs() <= tol)
    }

    #[test]
    fn a_sideways_phone_photo_comes_out_upright() {
        // Stored 400 × 200: the left half red, the right half blue. Orientation 6 means "rotate
        // 90° clockwise to show": shown 200 × 400 with red on top.
        let stored = RgbImage::from_fn(400, 200, |x, _| if x < 200 { Rgb([220, 20, 20]) } else { Rgb([20, 20, 220]) });
        let bytes = with_exif_orientation(&jpeg(&stored), 6);
        let p = read(&bytes, "IMG_0001.jpg", 1.).unwrap();
        assert_eq!((p.nat.w, p.nat.h), (200., 400.), "the size as shown");
        // Crop the whole picture at its own size (fit allows it): top red, bottom blue.
        let frame = Size::new(200., 400.);
        let view = initial_view(p.nat, frame);
        let pixels = render_pixels(&p, view, frame, (100, 200), Fill::Color([0, 0, 0]));
        let top = pixels.get_pixel(50, 20).0;
        let bottom = pixels.get_pixel(50, 180).0;
        assert!(near([top[0], top[1], top[2]], [220, 20, 20], 30), "top is red: {top:?}");
        assert!(near([bottom[0], bottom[1], bottom[2]], [20, 20, 220], 30), "bottom is blue: {bottom:?}");
        // And as a square crop of the top half (the area the user framed, in display layout).
        let frame = Size::new(256., 256.);
        let mut view = initial_view(p.nat, frame);
        view.y = 0.;
        let jpeg = render_crop(&p, view, frame, (256, 256), Fill::Blur).unwrap();
        let decoded = image::load_from_memory(&jpeg).unwrap().to_rgb8();
        assert_eq!(decoded.dimensions(), (256, 256));
        assert!(near(decoded.get_pixel(128, 128).0, [220, 20, 20], 30), "the top half is all red");
    }

    #[test]
    fn a_png_reads_its_size_transparency_and_main_colour() {
        let mut img = sample_rgba(300, 120);
        let p = read(&png(&img), "card.png", 128. / 44.).unwrap();
        assert_eq!((p.nat.w, p.nat.h), (300., 120.));
        assert!(!p.alpha);
        assert!(!p.animated);
        // A transparent corner makes it a logo.
        for y in 0..20 {
            for x in 0..20 {
                img.put_pixel(x, y, Rgba([0, 0, 0, 0]));
            }
        }
        let p = read(&png(&img), "logo.png", 1.).unwrap();
        assert!(p.alpha);
        // The deep shade is dark.
        let deep = p.palette[1];
        assert!(deep.iter().all(|c| *c < 80), "{deep:?}");
    }

    #[test]
    fn unreadable_files_say_why_in_the_renderers_words() {
        assert_eq!(read(b"not a picture at all", "notes.txt", 1.).err(), Some(PictureError::NotAPicture));
        assert_eq!(read(b"\0\0\0\x18ftypheic", "IMG_1234.HEIC", 1.).err(), Some(PictureError::Heic));
        assert_eq!(PictureError::NotAPicture.message(), "This file isn't a picture Decibell can open.");
        assert!(looks_like_picture("a.JPG") && looks_like_picture("b.webp") && !looks_like_picture("c.txt"));
    }

    #[test]
    fn the_export_is_the_output_size_and_under_the_cap() {
        let p = from_rgba(sample_rgba(1600, 900), "wide.png", 880. / 302.);
        let frame = Size::new(535., 184.);
        let r = range(p.nat, frame, Size::new(880., 302.), true);
        // Zoomed out past cover: the blur fill shows on both sides.
        let view = clamp_view(initial_view(p.nat, frame), p.nat, frame, r);
        let jpeg = render_crop(&p, view, frame, (880, 302), Fill::Blur).unwrap();
        assert!(jpeg.len() <= MAX_BYTES);
        let decoded = image::load_from_memory(&jpeg).unwrap();
        assert_eq!((decoded.width(), decoded.height()), (880, 302));
        // A contain view of a tall picture leaves the fill on the sides.
        let tall = from_rgba(sample_rgba(300, 900), "tall.png", 880. / 302.);
        let contain = contain_scale(tall.nat, frame);
        let v = clamp_view(
            crate::geometry::View { scale: contain, x: 0., y: 0. },
            tall.nat,
            frame,
            range(tall.nat, frame, Size::new(880., 302.), true),
        );
        let px = render_pixels(&tall, v, frame, (880, 302), Fill::Color([255, 255, 255]));
        assert_eq!(px.get_pixel(870, 150).0, [255, 255, 255, 255], "the white fill on the right");
        assert!(cover_scale(tall.nat, frame) > contain);
    }

    #[test]
    fn hex_is_lower_case_six_digits() {
        assert_eq!(hex([255, 0, 16]), "#ff0010");
    }
}
