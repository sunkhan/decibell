//! Painting one picture box: the ThumbHash blur at once, the picture over it when it is decoded
//! (no fade: the renderer draws the `<img>` over its blurred background and the blur is simply
//! never seen again), Chromium's broken-image look when it failed.
//!
//! A tile is a `canvas` inside the element the slot laid out: it reads the pipeline while
//! painting (no element tree per picture), asks for its key if no list did yet (with the view
//! being painted as the one to redraw), and tells the pipeline an animated picture was painted.

use std::sync::Arc;

use decibell_ui::{ActiveTheme as _, IconName};
use gpui::{App, Bounds, Corners, Hsla, IntoElement, Pixels, RenderImage, SharedString, Window, canvas, point, prelude::*, px, size};

use crate::pictures::{Fit, Look, PicKey, Pictures, decode};

/// What a tile shows.
#[derive(Clone)]
pub struct Tile {
    /// None: no picture can be fetched (no attachment endpoint yet): the blur only.
    pub key: Option<PicKey>,
    /// base64 ThumbHash ("" = the flat fill only).
    pub placeholder: SharedString,
    pub corners: Corners<Pixels>,
    /// The fill under everything (`bg-bg-secondary`, a video's `bg-black`).
    pub background: Option<Hsla>,
    /// The `alt` Chromium shows beside its broken-image icon.
    pub alt: SharedString,
    /// A dark wash over a ready picture (`bg-black/30` on a video poster).
    pub veil: Option<Hsla>,
}

impl Tile {
    /// The canvas filling its parent.
    pub fn element(self) -> impl IntoElement {
        canvas(|_, _, _| {}, move |bounds, _, window, cx| self.paint(bounds, window, cx)).size_full()
    }

    pub fn paint(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        if let Some(background) = self.background {
            window.paint_quad(gpui::quad(bounds, self.corners, background, gpui::Edges::default(), gpui::transparent_black(), gpui::BorderStyle::default()));
        }
        let Some(pictures) = Pictures::global(cx) else {
            self.paint_placeholder(bounds, None, window, cx);
            return;
        };
        let Some(key) = &self.key else {
            self.paint_placeholder(bounds, Some(&pictures), window, cx);
            return;
        };
        let view = window.current_view();
        let look = pictures.update(cx, |p, cx| {
            let look = p.look(key);
            match &look {
                Look::Waiting => {
                    p.request(key, Some(view), cx);
                    (look, p.stand_in(key))
                }
                Look::Ready(_) => {
                    p.painted(key, window, cx);
                    (look, None)
                }
                Look::Failed(_) => (look, None),
            }
        });
        match look {
            (Look::Ready(picture), _) => {
                // Pictures with transparency show the blur through, as an `<img>` over its
                // background does.
                self.paint_placeholder(bounds, Some(&pictures), window, cx);
                picture.paint(bounds.origin, self.corners, window);
                if let Some(veil) = self.veil {
                    window.paint_quad(gpui::quad(bounds, self.corners, veil, gpui::Edges::default(), gpui::transparent_black(), gpui::BorderStyle::default()));
                }
            }
            (Look::Waiting, Some(stand_in)) if key.fit == Fit::Contain || aspect_close(&stand_in.placement, key) => {
                // A resize: the picture decoded for the old box, scaled, until the new one is in.
                let placement = decode::place(key.spec(), stand_in.natural);
                let placement = decode::Placement { out: stand_in.placement.out, ..placement };
                let _ = window.paint_image(
                    placement.bounds(bounds.origin),
                    placement.image_bounds(bounds.origin),
                    self.corners,
                    stand_in.image.clone(),
                    0,
                    false,
                );
            }
            (Look::Failed(_), _) => {
                self.paint_placeholder(bounds, Some(&pictures), window, cx);
                self.paint_broken(bounds, window, cx);
            }
            _ => self.paint_placeholder(bounds, Some(&pictures), window, cx),
        }
    }

    /// The blur, covering the box (`bg-cover bg-center`), scaled up by the GPU's bilinear filter.
    fn paint_placeholder(&self, bounds: Bounds<Pixels>, pictures: Option<&gpui::Entity<Pictures>>, window: &mut Window, cx: &mut App) {
        if self.placeholder.is_empty() {
            return;
        }
        let Some(pictures) = pictures else { return };
        let Some((image, (w, h))) = pictures.update(cx, |p, cx| p.placeholder(&self.placeholder, cx)) else { return };
        paint_cover(&image, (w, h), bounds, self.corners, window);
    }

    /// Chromium's broken image: its icon at the top-left and the alt text beside it.
    fn paint_broken(&self, bounds: Bounds<Pixels>, window: &mut Window, cx: &mut App) {
        let color = cx.theme().color_text_muted;
        let icon = Bounds::new(bounds.origin + point(px(2.), px(2.)), size(px(16.), px(16.)));
        let _ = window.paint_svg(icon, SharedString::new_static(IconName::ContentBrokenImage.path()), None, gpui::TransformationMatrix::unit(), color, cx);
        if self.alt.is_empty() {
            return;
        }
        let style = crate::style::ContentStyle::get(cx, window.scale_factor());
        let width = (f32::from(bounds.size.width) - 22.).max(1.);
        let layout = style.engine.label_layout(&self.alt, &style.alt, decibell_text::Wrapping::TRUNCATE, width, style.scale);
        window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
            layout.paint(bounds.origin + point(px(20.), px(1.)), &decibell_text::PaintOptions::default(), window, cx);
        });
    }
}

fn aspect_close(placement: &decode::Placement, key: &PicKey) -> bool {
    let a = placement.width / placement.height.max(1.);
    let b = key.w as f32 / key.h.max(1) as f32;
    (a - b).abs() / b.max(0.01) < 0.02
}

/// Paints a bordered bitmap of `inner` texels covering `bounds` (the overflow cut evenly).
pub fn paint_cover(image: &Arc<RenderImage>, inner: (u32, u32), bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window) {
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (iw, ih) = (inner.0.max(1) as f32, inner.1.max(1) as f32);
    let s = (bw / iw).max(bh / ih);
    let (w, h) = (iw * s, ih * s);
    let placement = decode::Placement { x: (bw - w) / 2., y: (bh - h) / 2., width: w, height: h, crop: (0, 0, inner.0, inner.1), out: inner };
    window.with_content_mask(Some(gpui::ContentMask { bounds }), |window| {
        let _ = window.paint_image(bounds, placement.image_bounds(bounds.origin), corners, image.clone(), 0, false);
    });
}

/// Paints a bordered bitmap of `inner` texels inside `bounds` (`object-contain`, centred).
pub fn paint_contain(image: &Arc<RenderImage>, inner: (u32, u32), bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window) {
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let (iw, ih) = (inner.0.max(1) as f32, inner.1.max(1) as f32);
    let s = (bw / iw).min(bh / ih);
    let (w, h) = (iw * s, ih * s);
    let placement = decode::Placement { x: (bw - w) / 2., y: (bh - h) / 2., width: w, height: h, crop: (0, 0, inner.0, inner.1), out: inner };
    let _ = window.paint_image(placement.bounds(bounds.origin), placement.image_bounds(bounds.origin), corners, image.clone(), 0, false);
}

/// Paints a bordered bitmap of `inner` texels stretched over `bounds` (`object-fill`).
pub fn paint_fill(image: &Arc<RenderImage>, inner: (u32, u32), bounds: Bounds<Pixels>, corners: Corners<Pixels>, window: &mut Window) {
    let (bw, bh) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
    let placement = decode::Placement { x: 0., y: 0., width: bw, height: bh, crop: (0, 0, inner.0, inner.1), out: inner };
    let _ = window.paint_image(bounds, placement.image_bounds(bounds.origin), corners, image.clone(), 0, false);
}
