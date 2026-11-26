//! The viewer's stage dressing (globals.css `.image-viewer`, `.viewer-wash`, `.viewer-vignette`):
//! the picture's own colours washed over the stage (`blur(64px) saturate(1.35)`, brighter in the
//! light themes, at the scope's opacity) and the vignette. GPUI has neither CSS filters nor
//! radial gradients: both are small bitmaps computed here (a ≤ 32 px ThumbHash, blurred by the
//! amount 64 CSS px is at the stage's scale; a 64 px elliptical ramp) and drawn stretched by the
//! GPU's bilinear filter, which is what the blur would have looked like anyway.

use std::cell::RefCell;
use std::sync::Arc;

use decibell_model::chat::viewers::ViewerImage;
use decibell_theme::FilterFn;
use decibell_ui::ActiveTheme as _;
use gpui::{App, Bounds, Corners, Hsla, Pixels, RenderImage, SharedString, Window};
use image::{Rgba, RgbaImage};

/// The scope's wash opacity.
pub fn wash_opacity(cx: &App) -> f32 {
    cx.theme().id.scopes().image_viewer.viewer_wash
}

/// The stage colour (`--viewer-stage`).
pub fn stage_color(cx: &App) -> Hsla {
    decibell_ui::hsla(cx.theme().id.scopes().image_viewer.viewer_stage)
}

/// The wash of `item`, from its ThumbHash; kept while the same item shows.
pub fn wash(item: &Option<ViewerImage>, current: &Option<(SharedString, Arc<RenderImage>)>, cx: &App) -> Option<(SharedString, Arc<RenderImage>)> {
    let item = item.as_ref()?;
    if let Some((key, image)) = current
        && *key == item.key
    {
        return Some((key.clone(), image.clone()));
    }
    let hash = item.placeholder.as_ref()?;
    let decoded = decibell_attachments::thumbhash::decode_b64(hash)?;
    let mut rgba = RgbaImage::from_raw(decoded.width as u32, decoded.height as u32, decoded.rgba)?;
    // 64 CSS px of blur on a picture stretched over ~1.2 × a ~1300 px stage: about two texels.
    let sigma = 64. / (1560. / rgba.width().max(1) as f32);
    rgba = image::imageops::blur(&rgba, sigma.max(0.5));
    for filter in cx.theme().id.scopes().image_viewer.viewer_wash_filter {
        match filter {
            FilterFn::Saturate(s) => saturate(&mut rgba, *s),
            FilterFn::Brightness(b) => brightness(&mut rgba, *b),
            _ => {}
        }
    }
    Some((item.key.clone(), Arc::new(crate::pictures::decode::render_image(&rgba))))
}

fn saturate(img: &mut RgbaImage, s: f32) {
    for p in img.pixels_mut() {
        let (r, g, b) = (p[0] as f32, p[1] as f32, p[2] as f32);
        let nr = (0.213 + 0.787 * s) * r + (0.715 - 0.715 * s) * g + (0.072 - 0.072 * s) * b;
        let ng = (0.213 - 0.213 * s) * r + (0.715 + 0.285 * s) * g + (0.072 - 0.072 * s) * b;
        let nb = (0.213 - 0.213 * s) * r + (0.715 - 0.715 * s) * g + (0.072 + 0.928 * s) * b;
        *p = Rgba([nr.clamp(0., 255.) as u8, ng.clamp(0., 255.) as u8, nb.clamp(0., 255.) as u8, p[3]]);
    }
}

fn brightness(img: &mut RgbaImage, b: f32) {
    for p in img.pixels_mut() {
        for c in 0..3 {
            p[c] = (p[c] as f32 * b).clamp(0., 255.) as u8;
        }
    }
}

/// `radial-gradient(ellipse 80% 75% at 50% 50%, transparent 45%, stage 75% 100%)`.
pub fn paint_vignette(bounds: Bounds<Pixels>, stage: Hsla, alpha: f32, window: &mut Window, cx: &mut App) {
    thread_local! {
        static CACHE: RefCell<Option<(u32, Arc<RenderImage>)>> = const { RefCell::new(None) };
    }
    let rgba: gpui::Rgba = stage.into();
    let stamp = ((rgba.r * 255.) as u32) << 16 | ((rgba.g * 255.) as u32) << 8 | (rgba.b * 255.) as u32;
    let image = CACHE.with(|cache| {
        if let Some((at, image)) = &*cache.borrow()
            && *at == stamp
        {
            return image.clone();
        }
        const N: u32 = 64;
        let img = RgbaImage::from_fn(N, N, |x, y| {
            let (u, v) = ((x as f32 + 0.5) / N as f32 - 0.5, (y as f32 + 0.5) / N as f32 - 0.5);
            let d = ((u / 0.8).powi(2) + (v / 0.75).powi(2)).sqrt();
            let t = ((d - 0.45) / 0.55).clamp(0., 1.);
            Rgba([(rgba.r * 255.) as u8, (rgba.g * 255.) as u8, (rgba.b * 255.) as u8, (t * 0.75 * 255.) as u8])
        });
        let image = Arc::new(crate::pictures::decode::render_image(&img));
        if let Some((_, old)) = cache.borrow_mut().replace((stamp, image.clone())) {
            cx.defer(move |cx| cx.drop_image(old, None));
        }
        image
    });
    // The caller's element carries the opacity (`alpha` is 1 there).
    let _ = alpha;
    crate::tile::paint_fill(&image, (64, 64), bounds, Corners::default(), window);
}
