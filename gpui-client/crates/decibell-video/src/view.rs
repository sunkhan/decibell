//! The element that shows a stream.
//!
//! It paints a backdrop (the letterbox colour) and one scene surface. The surface is drawn by
//! the renderer with whatever picture is newest when it draws, so the element is painted once
//! and the stream's pictures cost a present each, never a layout or a paint of this element.
//! Any number of views can show one stream (a grid tile and the mini-player): they paint the
//! same surface, fed by one decoder.

use gpui::{
    App, Corners, Hsla, IntoElement, Pixels, RenderOnce, Styled as _, Window, canvas, fill,
};

use crate::hub::Viewer;

/// How the picture sits in the element's bounds (CSS `object-fit`).
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Fit {
    /// The whole picture, centred; the backdrop shows in the bars. The renderer's
    /// `object-contain` (the stream stage, the mini-player).
    #[default]
    Contain,
    /// Fill the bounds, cropping the overflow. The renderer's `object-cover` (grid tiles).
    Cover,
    /// Stretch.
    Fill,
}

/// Shows a stream's newest picture, filling its parent.
///
/// ```ignore
/// div().size_full().child(VideoView::new(&self.viewer).fit(Fit::Cover).rounded(theme.radius_md))
/// ```
#[derive(IntoElement)]
pub struct VideoView {
    viewer: Viewer,
    fit: Fit,
    corner_radii: Corners<Pixels>,
    backdrop: Option<Hsla>,
}

impl VideoView {
    pub fn new(viewer: &Viewer) -> Self {
        Self {
            viewer: viewer.clone(),
            fit: Fit::Contain,
            corner_radii: Corners::default(),
            backdrop: None,
        }
    }

    pub fn fit(mut self, fit: Fit) -> Self {
        self.fit = fit;
        self
    }

    /// Rounds the element's box (the picture and the backdrop behind it).
    pub fn rounded(mut self, radius: impl Into<Pixels>) -> Self {
        self.corner_radii = Corners::all(radius.into());
        self
    }

    pub fn corner_radii(mut self, radii: Corners<Pixels>) -> Self {
        self.corner_radii = radii;
        self
    }

    /// What is behind the picture: the bars of [`Fit::Contain`], and the whole box until the
    /// first picture arrives. A theme token.
    pub fn backdrop(mut self, color: impl Into<Hsla>) -> Self {
        self.backdrop = Some(color.into());
        self
    }
}

impl RenderOnce for VideoView {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        canvas(
            |_, _, _| (),
            move |bounds, _, window, _| {
                if let Some(backdrop) = self.backdrop {
                    window.paint_quad(fill(bounds, backdrop).corner_radii(self.corner_radii));
                }
                self.viewer.shown_in(window.window_handle());
                #[cfg(vk)]
                window.paint_external_surface(
                    bounds,
                    self.corner_radii,
                    gpui::ExternalSurface::new(self.viewer.surface_view(self.fit)),
                );
            },
        )
        .size_full()
    }
}
