//! Decoding a picture to the pixels it is drawn with: pure functions, run off the UI thread.
//!
//! A picture is decoded once per box it is drawn in, at the device pixels of that box (never
//! larger than the file's own pixels: a small thumbnail in a big box is scaled up by the GPU,
//! as Chromium's `object-contain` scales an `<img>`). Grid cells and other `object-cover` boxes
//! are cropped here, so what reaches the GPU is exactly what shows.
//!
//! Every picture leaves with a one-texel border that repeats its edge pixels: GPUI's atlas packs
//! tiles edge to edge and samples them bilinearly, so a picture drawn at any size other than
//! 1:1 would otherwise bleed its neighbours' texels into its outermost device pixels. The
//! border is drawn outside the box ([`Placement::image_bounds`]) and never seen.

use std::time::Duration;

use gpui::{Bounds, Pixels, RenderImage, point, px, size};
use image::{DynamicImage, Frame, ImageBuffer, Rgba, RgbaImage};

/// How a picture fills its box (CSS `object-fit`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Fit {
    /// The whole picture, centred, letterboxed (`object-contain`).
    Contain,
    /// The box filled, the overflow cropped evenly (`object-cover`, `bg-cover bg-center`).
    Cover,
}

/// What a picture is decoded for: its box in logical px, the window's scale, how it fills the
/// box, and whether it may be drawn larger than its own pixels (`h-full w-full object-contain`
/// may; `max-h-full max-w-full` may not).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Spec {
    pub width: f32,
    pub height: f32,
    pub scale: f32,
    pub fit: Fit,
    pub upscale: bool,
}

impl Spec {
    /// The box in device pixels: the decoded cache's size bucket.
    pub fn device(&self) -> (u32, u32) {
        (((self.width * self.scale).round() as u32).max(1), ((self.height * self.scale).round() as u32).max(1))
    }
}

/// Where a decoded picture goes in its box: the drawn rectangle (logical px, from the box's
/// top-left) and the decode's own size in device pixels.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    /// The part of the file's pixels decoded (`Cover` crops), in its natural pixels.
    pub crop: (u32, u32, u32, u32),
    /// The decoded size, device px (without the border).
    pub out: (u32, u32),
}

impl Placement {
    /// The rectangle the picture is drawn in, at `origin` (the box's top-left).
    pub fn bounds(&self, origin: gpui::Point<Pixels>) -> Bounds<Pixels> {
        Bounds::new(point(origin.x + px(self.x), origin.y + px(self.y)), size(px(self.width), px(self.height)))
    }

    /// The bounds to give `paint_image` for the bordered image so that its inner texels land
    /// exactly on [`Self::bounds`].
    pub fn image_bounds(&self, origin: gpui::Point<Pixels>) -> Bounds<Pixels> {
        let texel_x = self.width / self.out.0.max(1) as f32;
        let texel_y = self.height / self.out.1.max(1) as f32;
        Bounds::new(
            point(origin.x + px(self.x - texel_x), origin.y + px(self.y - texel_y)),
            size(px(self.width + 2. * texel_x), px(self.height + 2. * texel_y)),
        )
    }
}

/// JavaScript's `Math.round`.
fn js_round(value: f32) -> f32 {
    (value + 0.5).floor()
}

/// Where a file of `natural` pixels goes for `spec`.
pub fn place(spec: Spec, natural: (u32, u32)) -> Placement {
    let (nw, nh) = (natural.0.max(1) as f32, natural.1.max(1) as f32);
    let (bw, bh) = (spec.width.max(1.), spec.height.max(1.));
    match spec.fit {
        Fit::Contain => {
            let mut s = (bw / nw).min(bh / nh);
            if !spec.upscale {
                // An image's natural size is in CSS px (one file pixel each).
                s = s.min(1.);
            }
            let (w, h) = (nw * s, nh * s);
            // The decode: the drawn size in device px, never more than the file has.
            let out_w = ((w * spec.scale).round() as u32).clamp(1, natural.0.max(1));
            let out_h = ((h * spec.scale).round() as u32).clamp(1, natural.1.max(1));
            Placement { x: (bw - w) / 2., y: (bh - h) / 2., width: w, height: h, crop: (0, 0, natural.0.max(1), natural.1.max(1)), out: (out_w, out_h) }
        }
        Fit::Cover => {
            // The centred part of the file with the box's aspect.
            let box_aspect = bw / bh;
            let (cw, ch) = if nw / nh > box_aspect { (js_round(nh * box_aspect).max(1.), nh) } else { (nw, js_round(nw / box_aspect).max(1.)) };
            let (cx, cy) = (((nw - cw) / 2.).floor(), ((nh - ch) / 2.).floor());
            let (dw, dh) = spec.device();
            let out = ((dw as f32).min(cw) as u32, (dh as f32).min(ch) as u32);
            Placement { x: 0., y: 0., width: bw, height: bh, crop: (cx as u32, cy as u32, cw as u32, ch as u32), out: (out.0.max(1), out.1.max(1)) }
        }
    }
}

/// A still decoded for its box.
pub struct Still {
    pub image: RenderImage,
    pub placement: Placement,
    pub natural: (u32, u32),
    /// Bytes of the decoded pixels (CPU copy; the GPU atlas holds a twin while drawn).
    pub bytes: usize,
}

/// Shrinks (or crops and shrinks) `rgba` to the placement's decode.
pub fn fit_pixels(rgba: &RgbaImage, placement: &Placement) -> RgbaImage {
    let (cx, cy, cw, ch) = placement.crop;
    let cropped;
    let source = if (cx, cy, cw, ch) == (0, 0, rgba.width(), rgba.height()) {
        rgba
    } else {
        cropped = image::imageops::crop_imm(rgba, cx, cy, cw.min(rgba.width() - cx), ch.min(rgba.height() - cy)).to_image();
        &cropped
    };
    let (w, h) = placement.out;
    if (w, h) == source.dimensions() {
        return source.clone();
    }
    // Area averaging for large reductions (it reads every source pixel once and is the
    // fastest), a triangle filter for small ones (sharper than a box at ratios near 1).
    if source.width() >= 2 * w && source.height() >= 2 * h {
        image::imageops::thumbnail(source, w, h)
    } else {
        image::imageops::resize(source, w, h, image::imageops::FilterType::Triangle)
    }
}

/// `rgba` as GPUI's straight-alpha BGRA, with a one-texel border repeating the edges.
pub fn bordered_bgra(rgba: &RgbaImage) -> ImageBuffer<Rgba<u8>, Vec<u8>> {
    let (w, h) = rgba.dimensions();
    let (bw, bh) = (w + 2, h + 2);
    let mut out = vec![0u8; (bw * bh * 4) as usize];
    let src = rgba.as_raw();
    for y in 0..bh {
        let sy = y.saturating_sub(1).min(h - 1);
        let row = &src[(sy * w * 4) as usize..((sy + 1) * w * 4) as usize];
        let dst = &mut out[(y * bw * 4) as usize..((y + 1) * bw * 4) as usize];
        // Inner pixels, swapped to BGRA.
        for (d, s) in dst[4..(4 + w as usize * 4)].as_chunks_mut::<4>().0.iter_mut().zip(row.as_chunks::<4>().0) {
            *d = [s[2], s[1], s[0], s[3]];
        }
        // The left and right border texels repeat the row's ends.
        let (first, last) = ([row[2], row[1], row[0], row[3]], {
            let l = &row[row.len() - 4..];
            [l[2], l[1], l[0], l[3]]
        });
        dst[..4].copy_from_slice(&first);
        let end = dst.len();
        dst[end - 4..].copy_from_slice(&last);
    }
    ImageBuffer::from_raw(bw, bh, out).expect("sized above")
}

/// One frame for GPUI.
pub fn render_image(rgba: &RgbaImage) -> RenderImage {
    RenderImage::new(vec![Frame::new(bordered_bgra(rgba))])
}

/// Decodes `bytes` (any still format, EXIF orientation applied) for `spec`.
pub fn decode_still(bytes: &[u8], spec: Spec) -> Result<Still, String> {
    let img = decibell_attachments::thumbs::decode_image_bytes(bytes).map_err(|e| e.to_string())?;
    Ok(still_from(img, spec))
}

pub fn still_from(img: DynamicImage, spec: Spec) -> Still {
    let natural = (img.width(), img.height());
    let placement = place(spec, natural);
    // Crop and shrink in the file's own pixel format first: a large photo (8000 × 6000) is never
    // converted to RGBA at full size, only its decode's own buffer exists at that size.
    let (cx, cy, cw, ch) = placement.crop;
    let img = if (cx, cy, cw, ch) == (0, 0, natural.0, natural.1) { img } else { img.crop_imm(cx, cy, cw, ch) };
    let (w, h) = placement.out;
    let img = if (img.width(), img.height()) == (w, h) {
        img
    } else if img.width() >= 2 * w && img.height() >= 2 * h {
        img.thumbnail_exact(w, h)
    } else {
        img.resize_exact(w, h, image::imageops::FilterType::Triangle)
    };
    let fitted = img.into_rgba8();
    let bytes = ((fitted.width() + 2) * (fitted.height() + 2) * 4) as usize;
    Still { image: render_image(&fitted), placement, natural, bytes }
}

/// What a file is, from its first bytes: animated formats go to [`super::anim`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Kind {
    Gif,
    WebP,
    Other,
}

pub fn sniff(bytes: &[u8]) -> Kind {
    if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") {
        Kind::Gif
    } else if bytes.len() >= 12 && &bytes[..4] == b"RIFF" && &bytes[8..12] == b"WEBP" {
        Kind::WebP
    } else {
        Kind::Other
    }
}

/// Chromium's frame delay: a delay of 10 ms or less plays as 100 ms (Blink's
/// `ImageDecoder::FrameDurationAtIndex`, which follows Firefox: "many annoying ads specify a 0
/// duration to make an image flash as quickly as possible").
pub fn chromium_delay(delay: Duration) -> Duration {
    if delay <= Duration::from_millis(10) { Duration::from_millis(100) } else { delay }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn spec(w: f32, h: f32, fit: Fit) -> Spec {
        Spec { width: w, height: h, scale: 1., fit, upscale: true }
    }

    #[test]
    fn contain_never_decodes_past_the_file() {
        // A 320 thumbnail in a 478 × 269 box: drawn full-box (upscaled by the GPU), decoded at 320.
        let p = place(spec(478., 269., Fit::Contain), (320, 180));
        assert_eq!((p.width.round(), p.height.round()), (478., 269.));
        assert_eq!(p.out, (320, 180));
        // A 1280 one in the same box: decoded at the box.
        let p = place(spec(478., 269., Fit::Contain), (1280, 720));
        assert_eq!(p.out, (478, 269));
        // `max-w-full max-h-full`: never drawn larger than the file.
        let p = place(Spec { upscale: false, ..spec(478., 269., Fit::Contain) }, (200, 100));
        assert_eq!((p.width, p.height, p.x, p.y), (200., 100., 139., 84.5));
    }

    #[test]
    fn cover_crops_the_middle() {
        let p = place(spec(180., 180., Fit::Cover), (1600, 900));
        assert_eq!(p.crop, (350, 0, 900, 900));
        assert_eq!(p.out, (180, 180));
        let p = place(Spec { scale: 2., ..spec(180., 180., Fit::Cover) }, (900, 1600));
        assert_eq!(p.crop, (0, 350, 900, 900));
        assert_eq!(p.out, (360, 360));
    }

    #[test]
    fn the_border_repeats_the_edges() {
        let img = RgbaImage::from_fn(3, 2, |x, y| Rgba([x as u8 * 10, y as u8 * 10, 7, 255]));
        let out = bordered_bgra(&img);
        assert_eq!(out.dimensions(), (5, 4));
        // BGRA: blue is the source's red.
        assert_eq!(out.get_pixel(0, 0).0, [7, 0, 0, 255]);
        assert_eq!(out.get_pixel(4, 3).0, [7, 10, 20, 255]);
        assert_eq!(out.get_pixel(2, 1).0, [7, 0, 10, 255]);
        let p = Placement { x: 0., y: 0., width: 30., height: 20., crop: (0, 0, 3, 2), out: (3, 2) };
        let b = p.image_bounds(point(px(0.), px(0.)));
        assert_eq!((f32::from(b.origin.x), f32::from(b.size.width)), (-10., 50.));
    }

    #[test]
    fn tiny_delays_play_as_chromium_plays_them() {
        assert_eq!(chromium_delay(Duration::ZERO), Duration::from_millis(100));
        assert_eq!(chromium_delay(Duration::from_millis(10)), Duration::from_millis(100));
        assert_eq!(chromium_delay(Duration::from_millis(20)), Duration::from_millis(20));
    }
}
