//! Twemoji as images: the SVGs packed in the binary, rasterised with resvg at the exact
//! device-pixel size they are drawn at, and kept in a byte-bounded cache.
//!
//! GPUI draws SVGs as one-colour masks; emoji have colours, so each is rasterised here to
//! an image and painted with `Window::paint_image`. An emoji is rasterised when the layout
//! that shows it is built ([`EmojiStore::prepare`]), not when it is first painted, so a row
//! never appears without its emoji. Evicted images are handed back to GPUI at the next paint
//! (`App::drop_image`), which frees their atlas tiles.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::Arc;

use decibell_richtext::EmojiAsset;
use gpui::{App, Bounds, Corners, Pixels, RenderImage, Window};
use resvg::{tiny_skia, usvg};
use smallvec::SmallVec;

/// The pack `build.rs` writes: see its docs for the layout.
static PACK: &[u8] = include_bytes!(concat!(env!("OUT_DIR"), "/twemoji.pack"));

/// Decoded emoji pixels kept (both generations): about 3,000 emoji at 24 px, or 150 at
/// 56 px on a 2× display.
const CACHE_BYTES: usize = 8 << 20;

fn read_u32(at: usize) -> usize {
    u32::from_le_bytes(PACK[at..at + 4].try_into().unwrap()) as usize
}

/// How many emoji the pack holds (0 when the assets were missing at build time).
pub fn packed_emoji_count() -> usize {
    read_u32(0)
}

/// The pack's size in the binary, bytes.
pub fn packed_emoji_bytes() -> usize {
    PACK.len()
}

/// The SVG of an emoji, inflated from the pack: a binary search over the index (12 probes
/// for 2,900 entries) and one small inflate.
pub fn emoji_svg(asset: EmojiAsset) -> Option<Vec<u8>> {
    let count = packed_emoji_count();
    let keys = 4 + (count + 1) * 8;
    let data = keys + read_u32(4 + count * 8);
    let entry = |index: usize| (read_u32(4 + index * 8), read_u32(8 + index * 8));
    let key = asset.chars().as_bytes();
    let (mut low, mut high) = (0, count);
    while low < high {
        let middle = (low + high) / 2;
        let (key_start, data_start) = entry(middle);
        let (key_end, data_end) = entry(middle + 1);
        match PACK[keys + key_start..keys + key_end].cmp(key) {
            std::cmp::Ordering::Less => low = middle + 1,
            std::cmp::Ordering::Greater => high = middle,
            std::cmp::Ordering::Equal => {
                return miniz_oxide::inflate::decompress_to_vec(&PACK[data + data_start..data + data_end]).ok();
            }
        }
    }
    None
}

/// Rasterises an emoji's SVG to `size` × `size` device pixels, as GPUI wants image pixels:
/// BGRA, straight alpha. Thread-safe and self-contained: callers may run it off the UI thread.
pub fn rasterize_emoji(asset: EmojiAsset, size: u32) -> Option<Arc<RenderImage>> {
    let svg = emoji_svg(asset)?;
    let tree = usvg::Tree::from_data(&svg, &usvg::Options::default()).ok()?;
    let mut pixmap = tiny_skia::Pixmap::new(size, size)?;
    let scale = size as f32 / tree.size().width().max(tree.size().height());
    resvg::render(&tree, tiny_skia::Transform::from_scale(scale, scale), &mut pixmap.as_mut());
    let mut pixels = pixmap.take();
    for pixel in pixels.as_chunks_mut::<4>().0 {
        // Premultiplied RGBA → straight BGRA.
        let alpha = pixel[3] as u32;
        if alpha != 0 && alpha != 255 {
            for channel in &mut pixel[..3] {
                *channel = ((*channel as u32 * 255 + alpha / 2) / alpha).min(255) as u8;
            }
        }
        pixel.swap(0, 2);
    }
    let buffer = image::RgbaImage::from_raw(size, size, pixels)?;
    Some(Arc::new(RenderImage::new(SmallVec::from_elem(image::Frame::new(buffer), 1))))
}

type Key = (u16, u16);

/// The rasterised emoji, by (asset, device px). Two generations, like the layout cache:
/// what a frame uses moves to the young one; when that outgrows half the budget the old one
/// is dropped and its textures freed.
#[derive(Default)]
pub struct EmojiStore {
    inner: RefCell<Inner>,
}

#[derive(Default)]
struct Inner {
    young: HashMap<Key, Arc<RenderImage>>,
    old: HashMap<Key, Arc<RenderImage>>,
    young_bytes: usize,
    /// Images that left the cache and still hold an atlas tile.
    dropped: Vec<Arc<RenderImage>>,
}

impl EmojiStore {
    /// Makes sure the emoji is rasterised at `device_px` and returns it. `None` when the
    /// pack has no such emoji (the caller draws the characters with the system emoji font).
    pub fn prepare(&self, asset: EmojiAsset, device_px: u32) -> Option<Arc<RenderImage>> {
        let key = (asset.index(), device_px.min(u16::MAX as u32) as u16);
        let mut inner = self.inner.borrow_mut();
        if let Some(image) = inner.young.get(&key) {
            return Some(image.clone());
        }
        let image = match inner.old.remove(&key) {
            Some(image) => image,
            None => rasterize_emoji(asset, device_px.max(1))?,
        };
        let bytes = (device_px * device_px * 4) as usize;
        if inner.young_bytes + bytes > CACHE_BYTES / 2 && !inner.young.is_empty() {
            let young = std::mem::take(&mut inner.young);
            let old = std::mem::replace(&mut inner.old, young);
            inner.dropped.extend(old.into_values());
            inner.young_bytes = 0;
        }
        inner.young_bytes += bytes;
        inner.young.insert(key, image.clone());
        Some(image)
    }

    /// Paints an emoji into `bounds`. Paint phase only.
    pub fn paint(&self, asset: EmojiAsset, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        self.release_dropped(window, cx);
        let device_px = (f32::from(bounds.size.width) * window.scale_factor()).round().max(1.) as u32;
        if let Some(image) = self.prepare(asset, device_px) {
            let _ = window.paint_image(bounds, bounds, Corners::default(), image, 0, false);
        }
    }

    /// Frees the atlas tiles of evicted images.
    fn release_dropped(&self, window: &mut Window, cx: &mut App) {
        let dropped = std::mem::take(&mut self.inner.borrow_mut().dropped);
        for image in dropped {
            cx.drop_image(image, Some(window));
        }
    }

    /// (images, bytes of pixels) held.
    pub fn usage(&self) -> (usize, usize) {
        let inner = self.inner.borrow();
        let bytes = |map: &HashMap<Key, Arc<RenderImage>>| -> usize {
            map.keys().map(|(_, px)| *px as usize * *px as usize * 4).sum()
        };
        (inner.young.len() + inner.old.len(), bytes(&inner.young) + bytes(&inner.old))
    }
}
