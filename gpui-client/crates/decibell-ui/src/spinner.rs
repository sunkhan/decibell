//! The loading spinner: `animate-spin` (one turn per second, linear) on the renderer's spinner
//! glyph.

use std::time::Duration;

use gpui::{
    App, Hsla, IntoElement, Pixels, RenderOnce, Transformation, Window, prelude::*, px, svg,
};
use gpui::TimedTransformation;

use crate::IconName;
use crate::icon::icon_path;

/// A spinning glyph. It turns as a scene animation: the window re-presents the SVG rotated
/// without re-rendering any view, at most 60 times a second (Chromium turns `animate-spin` at
/// the display's rate; 60 is smooth for a spinner and spares a 280 Hz display most presents),
/// and only while the spinner is painted (removed, clipped out or in a hidden window, it costs
/// nothing). Reduced motion leaves it still.
///
/// ```ignore
/// Spinner::new().size(px(20.)).color(theme.color_accent)
/// ```
#[derive(IntoElement)]
pub struct Spinner {
    glyph: IconName,
    size: Pixels,
    color: Option<Hsla>,
}

impl Spinner {
    /// The 25 % ring with a 75 % quarter arc (LoginPage, ServerBrowseView, the stream player): 16 px.
    pub fn new() -> Self {
        Self {
            glyph: IconName::Spinner,
            size: px(16.),
            color: None,
        }
    }

    /// The image viewer's spinner: a thinner 25 % ring with a full-strength arc.
    pub fn ring() -> Self {
        Self {
            glyph: IconName::SpinnerRing,
            ..Self::new()
        }
    }

    pub fn size(mut self, size: Pixels) -> Self {
        self.size = size;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl RenderOnce for Spinner {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        svg()
            .path(icon_path(self.glyph, None))
            .size(self.size)
            .flex_none()
            .text_color(color)
            .with_timed_transformation(
                TimedTransformation::rotation(Duration::from_secs(1), Transformation::default())
                    .with_max_fps(60.),
            )
    }
}
