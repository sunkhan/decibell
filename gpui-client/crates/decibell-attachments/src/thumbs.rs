//! What the uploader computes from a picture before sending it (`uploadAttachment.ts`
//! `probeMetadata` + `generateThumbnail`, `thumbhash.ts` `encodeThumbHash`): its size as shown
//! (EXIF orientation applied, as Chromium's `<img>` and `createImageBitmap` do), a ThumbHash
//! placeholder, and JPEG thumbnails at 320 / 640 / 1280 px long edge. An animated GIF / WebP
//! contributes its first frame, as `createImageBitmap` did (the chat shows a GIF's original, so
//! its thumbnails only serve the Transfers panel and the like).
//!
//! All of it is CPU work: call it from `spawn_blocking` (the [`crate::upload::prepare`] path
//! does). Video and audio need a media decoder this crate doesn't carry: the app implements
//! [`MediaProbe`] (FFmpeg) and the rest of the flow works without it (zeros, no thumbnails, as
//! the renderer's failed probes gave).

use std::io::{BufRead, Cursor, Seek};
use std::path::Path;

use image::{DynamicImage, ImageDecoder, ImageReader, RgbImage, RgbaImage};

use crate::thumbhash::{self, js_round};
use crate::Error;

/// Thumbnail long edges, smallest first, and their `thumbnail_sizes_mask` bits.
pub const THUMB_SIZES: [u32; 3] = [320, 640, 1280];

/// The mask bit of a thumbnail size (server `attachment_http.cpp`: 320 → 1, 640 → 2, 1280 → 4).
pub fn thumb_bit(size: u32) -> u32 {
    match size {
        320 => 1,
        640 => 2,
        1280 => 4,
        _ => 0,
    }
}

/// `generateThumbnail`'s JPEG quality (canvas `convertToBlob` 0.82).
pub const THUMB_JPEG_QUALITY: u8 = 82;

/// The renderer's `pickThumbnailSize`: the smallest size ≥ `target` that exists, else the
/// largest that does.
pub fn pick_thumbnail_size(mask: u32, target: u32) -> Option<u32> {
    let (has320, has640, has1280) = (mask & 1 != 0, mask & 2 != 0, mask & 4 != 0);
    if target <= 320 && has320 {
        return Some(320);
    }
    if target <= 640 && has640 {
        return Some(640);
    }
    if target <= 1280 && has1280 {
        return Some(1280);
    }
    if has1280 {
        Some(1280)
    } else if has640 {
        Some(640)
    } else if has320 {
        Some(320)
    } else {
        None
    }
}

/// The size the server answers `?variant=thumb&size=requested` with, given the row's mask
/// (`handle_get`): the requested size if present, else the smallest larger one, else the
/// largest. `None` when the mask is empty (legacy single thumbnail).
pub fn served_thumbnail_size(mask: u32, requested: u32) -> Option<u32> {
    if mask & thumb_bit(requested) != 0 {
        return Some(requested);
    }
    THUMB_SIZES
        .iter()
        .copied()
        .find(|&s| s >= requested && mask & thumb_bit(s) != 0)
        .or_else(|| THUMB_SIZES.iter().rev().copied().find(|&s| mask & thumb_bit(s) != 0))
}

/// Thumbnail dimensions for a picture of `w × h` (`generateThumbnail`): the long edge is
/// `min(size, long edge)` (never upscaled, so a small picture gets three same-sized
/// thumbnails, as the renderer sends), the other edge rounds from the ratio. `None` when an
/// edge rounds to 0 (the canvas refused those).
pub fn thumbnail_dims(w: u32, h: u32, size: u32) -> Option<(u32, u32)> {
    if w == 0 || h == 0 {
        return None;
    }
    let ratio = w as f64 / h as f64;
    let (tw, th) = if w >= h {
        let tw = size.min(w);
        (tw, js_round(tw as f64 / ratio) as u32)
    } else {
        let th = size.min(h);
        (js_round(th as f64 * ratio) as u32, th)
    };
    (tw > 0 && th > 0).then_some((tw, th))
}

/// ThumbHash input size for a picture of `w × h` (`encodeThumbHash`): scaled to fit 100 px.
pub fn thumbhash_dims(w: u32, h: u32) -> (u32, u32) {
    let scale = (100.0 / w.max(h) as f64).min(1.0);
    let tw = (js_round(w as f64 * scale) as u32).max(1);
    let th = (js_round(h as f64 * scale) as u32).max(1);
    (tw, th)
}

/// Decode a picture with its EXIF orientation applied. Animated formats give their first frame.
pub fn decode_image(reader: impl BufRead + Seek) -> Result<DynamicImage, Error> {
    let reader = ImageReader::new(reader).with_guessed_format().map_err(|e| Error::Image(e.to_string()))?;
    let mut decoder = reader.into_decoder().map_err(|e| Error::Image(e.to_string()))?;
    let orientation = decoder.orientation().unwrap_or(image::metadata::Orientation::NoTransforms);
    let mut img = DynamicImage::from_decoder(decoder).map_err(|e| Error::Image(e.to_string()))?;
    img.apply_orientation(orientation);
    Ok(img)
}

pub fn decode_image_file(path: &Path) -> Result<DynamicImage, Error> {
    let f = std::fs::File::open(path)?;
    decode_image(std::io::BufReader::with_capacity(64 * 1024, f))
}

pub fn decode_image_bytes(bytes: &[u8]) -> Result<DynamicImage, Error> {
    decode_image(Cursor::new(bytes))
}

/// What an image contributes to its upload.
#[derive(Debug, Clone, Default)]
pub struct ImageFacts {
    /// Shown size (after orientation).
    pub width: u32,
    pub height: u32,
    /// base64 ThumbHash, "" when it couldn't be made.
    pub placeholder: String,
    /// `(size_px, jpeg)` for each of [`THUMB_SIZES`] that could be made.
    pub thumbnails: Vec<(u32, Vec<u8>)>,
}

/// A picture at one step of the downscale: 8-bit RGB when it has no alpha (no conversion of
/// a decoded photo), straight RGBA otherwise.
enum Step {
    Rgb(RgbImage),
    Rgba(RgbaImage),
}

impl Step {
    fn dims(&self) -> (u32, u32) {
        match self {
            Step::Rgb(i) => i.dimensions(),
            Step::Rgba(i) => i.dimensions(),
        }
    }

    /// Area-averaging downscale (`imageops::thumbnail`: integer box sampling, no float
    /// intermediates the size of the source).
    fn shrink(&self, w: u32, h: u32) -> Step {
        match self {
            Step::Rgb(i) => Step::Rgb(image::imageops::thumbnail(i, w, h)),
            Step::Rgba(i) => Step::Rgba(image::imageops::thumbnail(i, w, h)),
        }
    }

    fn jpeg(&self, quality: u8) -> Option<Vec<u8>> {
        match self {
            Step::Rgb(i) => encode_rgb_jpeg(i, quality),
            Step::Rgba(i) => encode_jpeg(i, quality),
        }
    }

    fn rgba(self) -> RgbaImage {
        match self {
            Step::Rgb(i) => DynamicImage::ImageRgb8(i).into_rgba8(),
            Step::Rgba(i) => i,
        }
    }
}

/// Everything the uploader computes from a decoded picture, in one pass: the largest
/// thumbnail is shrunk from the original, each smaller one (and the ThumbHash input) from the
/// step before, so a large photo is read at full size once.
pub fn image_facts(img: DynamicImage) -> ImageFacts {
    let (w, h) = (img.width(), img.height());
    let mut facts = ImageFacts { width: w, height: h, ..Default::default() };
    if w == 0 || h == 0 {
        return facts;
    }
    let original = if img.color().has_alpha() { Step::Rgba(img.into_rgba8()) } else { Step::Rgb(img.into_rgb8()) };
    let mut prev: Option<Step> = None;
    for &size in THUMB_SIZES.iter().rev() {
        let Some((tw, th)) = thumbnail_dims(w, h, size) else { continue };
        let from = prev.as_ref().unwrap_or(&original);
        let step = if from.dims() == (tw, th) { None } else { Some(from.shrink(tw, th)) };
        let jpeg = step.as_ref().unwrap_or(from).jpeg(THUMB_JPEG_QUALITY);
        if let Some(jpeg) = jpeg {
            facts.thumbnails.push((size, jpeg));
        }
        if let Some(s) = step {
            prev = Some(s);
        }
    }
    facts.thumbnails.reverse();
    let (hw, hh) = thumbhash_dims(w, h);
    let from = prev.as_ref().unwrap_or(&original);
    let small = if from.dims() == (hw, hh) { None } else { Some(from.shrink(hw, hh)) };
    let small = match small {
        Some(s) => s.rgba(),
        None => match prev {
            Some(p) => p.rgba(),
            None => original.rgba(),
        },
    };
    facts.placeholder = thumbhash::encode_b64(hw as usize, hh as usize, small.as_raw());
    facts
}

fn encode_rgb_jpeg(rgb: &RgbImage, quality: u8) -> Option<Vec<u8>> {
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    rgb.write_with_encoder(enc).ok()?;
    Some(out)
}

/// JPEG of straight RGBA, composited onto black as a canvas `convertToBlob("image/jpeg")` does.
pub fn encode_jpeg(rgba: &RgbaImage, quality: u8) -> Option<Vec<u8>> {
    let (w, h) = rgba.dimensions();
    let mut rgb = RgbImage::new(w, h);
    for (dst, src) in rgb.pixels_mut().zip(rgba.pixels()) {
        let a = src[3] as u32;
        for c in 0..3 {
            dst[c] = ((src[c] as u32 * a + 127) / 255) as u8;
        }
    }
    let mut out = Vec::new();
    let enc = image::codecs::jpeg::JpegEncoder::new_with_quality(&mut out, quality);
    rgb.write_with_encoder(enc).ok()?;
    Some(out)
}

/// Where a [`MediaProbe`] reads from.
#[derive(Debug, Clone)]
pub enum ProbeSource<'a> {
    Path(&'a Path),
    Bytes(&'a [u8]),
}

/// What a media probe learned about a video or audio file.
#[derive(Debug, Clone, Default)]
pub struct ProbedMedia {
    /// Display size (rotation applied); 0 for audio or unknown.
    pub width: u32,
    pub height: u32,
    /// Duration in ms, 0 when unknown (the renderer's `finiteDurationMs`).
    pub duration_ms: u32,
    /// A poster frame for video: the renderer captured the frame `min(0.5 s, 10 %)` in. Its
    /// thumbnails and ThumbHash are made from it.
    pub poster: Option<RgbaImage>,
}

/// Video / audio probing, implemented by the app on its media decoder (FFmpeg). Called from a
/// blocking thread. `None` when the file can't be read.
pub trait MediaProbe: Send + Sync {
    fn probe(&self, source: ProbeSource<'_>, kind: crate::AttachmentKind) -> Option<ProbedMedia>;
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dims_follow_the_renderer() {
        assert_eq!(thumbnail_dims(4000, 3000, 320), Some((320, 240)));
        assert_eq!(thumbnail_dims(3000, 4000, 1280), Some((960, 1280)));
        assert_eq!(thumbnail_dims(200, 100, 1280), Some((200, 100)));
        assert_eq!(thumbnail_dims(10_000, 1, 320), None);
        assert_eq!(thumbnail_dims(333, 1000, 320), Some((js_round(320.0 * 0.333) as u32, 320)));
        assert_eq!(thumbhash_dims(4000, 3000), (100, 75));
        assert_eq!(thumbhash_dims(50, 20), (50, 20));
        assert_eq!(thumbhash_dims(10_000, 1), (100, 1));
    }

    #[test]
    fn thumbnail_choice() {
        assert_eq!(pick_thumbnail_size(0b111, 500), Some(640));
        assert_eq!(pick_thumbnail_size(0b001, 2000), Some(320));
        assert_eq!(pick_thumbnail_size(0, 2000), None);
        assert_eq!(served_thumbnail_size(0b101, 640), Some(1280));
        assert_eq!(served_thumbnail_size(0b001, 640), Some(320));
        assert_eq!(served_thumbnail_size(0b111, 640), Some(640));
        assert_eq!(served_thumbnail_size(0, 640), None);
    }

    #[test]
    fn facts_of_a_picture() {
        let img = DynamicImage::ImageRgba8(RgbaImage::from_fn(1500, 1000, |x, y| {
            image::Rgba([(x % 256) as u8, (y % 256) as u8, 90, if x < 100 { 0 } else { 255 }])
        }));
        let f = image_facts(img);
        assert_eq!((f.width, f.height), (1500, 1000));
        let sizes: Vec<u32> = f.thumbnails.iter().map(|t| t.0).collect();
        assert_eq!(sizes, vec![320, 640, 1280]);
        for (size, jpeg) in &f.thumbnails {
            let d = decode_image_bytes(jpeg).unwrap();
            assert_eq!((d.width(), d.height()), thumbnail_dims(1500, 1000, *size).unwrap());
        }
        assert!(!f.placeholder.is_empty());
    }

    #[test]
    fn exif_orientation_is_applied() {
        // A 4x2 JPEG tagged Orientation=6 (rotate 90° clockwise) shows as 2x4.
        let jpeg = encode_jpeg(&RgbaImage::from_pixel(4, 2, image::Rgba([200, 10, 10, 255])), 90).unwrap();
        let tagged = with_exif_orientation(&jpeg, 6);
        let img = decode_image_bytes(&tagged).unwrap();
        assert_eq!((img.width(), img.height()), (2, 4));
    }

    #[test]
    fn animated_gif_gives_its_first_frame() {
        use image::codecs::gif::GifEncoder;
        use image::{Delay, Frame};
        let mut gif = Vec::new();
        {
            let mut enc = GifEncoder::new(&mut gif);
            for color in [[250u8, 0, 0, 255], [0, 0, 250, 255]] {
                let f = Frame::from_parts(RgbaImage::from_pixel(400, 300, image::Rgba(color)), 0, 0, Delay::from_numer_denom_ms(100, 1));
                enc.encode_frame(f).unwrap();
            }
        }
        let img = decode_image_bytes(&gif).unwrap();
        let px = img.to_rgba8().get_pixel(10, 10).0;
        assert!(px[0] > 200 && px[2] < 50, "first frame is the red one: {px:?}");
        let f = image_facts(img);
        assert_eq!((f.width, f.height, f.thumbnails.len()), (400, 300, 3));
    }

    /// Insert an APP1 Exif segment holding only an Orientation tag after SOI.
    pub(crate) fn with_exif_orientation(jpeg: &[u8], orientation: u16) -> Vec<u8> {
        let mut tiff = Vec::new();
        tiff.extend_from_slice(b"MM\0\x2a\0\0\0\x08"); // big-endian TIFF, IFD at 8
        tiff.extend_from_slice(&1u16.to_be_bytes()); // one entry
        tiff.extend_from_slice(&0x0112u16.to_be_bytes()); // Orientation
        tiff.extend_from_slice(&3u16.to_be_bytes()); // SHORT
        tiff.extend_from_slice(&1u32.to_be_bytes()); // count
        tiff.extend_from_slice(&orientation.to_be_bytes());
        tiff.extend_from_slice(&[0, 0]);
        tiff.extend_from_slice(&0u32.to_be_bytes()); // no next IFD
        let mut app1 = b"Exif\0\0".to_vec();
        app1.extend_from_slice(&tiff);
        let mut out = jpeg[..2].to_vec();
        out.extend_from_slice(&[0xFF, 0xE1]);
        out.extend_from_slice(&((app1.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&app1);
        out.extend_from_slice(&jpeg[2..]);
        out
    }
}
