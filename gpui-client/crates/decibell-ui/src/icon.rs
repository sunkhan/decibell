//! Icons: the bundled SVGs, rasterised by GPUI at device-pixel size and tinted when drawn.

use gpui::{App, Hsla, IntoElement, Pixels, RenderOnce, SharedString, Window, prelude::*, px, svg};

/// An icon from `gpui-client/assets`, named by its path there:
/// `Icon::new("icons/lucide/settings.svg")`, `Icon::new("icons/window/close.svg")`.
///
/// 16 px and the inherited text colour unless told otherwise (`currentColor` in the SVG).
#[derive(IntoElement)]
pub struct Icon {
    path: &'static str,
    size: Pixels,
    color: Option<Hsla>,
    stroke_width: Option<f32>,
    group_hover: Option<(SharedString, Hsla)>,
}

impl Icon {
    pub fn new(path: &'static str) -> Self {
        Self {
            path,
            size: px(16.),
            color: None,
            stroke_width: None,
            group_hover: None,
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

    /// For lucide icons: the stroke width in the icon's 24-unit box (lucide-react's
    /// `strokeWidth`; the files are drawn at 2).
    pub fn stroke_width(mut self, stroke_width: f32) -> Self {
        self.stroke_width = Some(stroke_width);
        self
    }

    /// The colour while the pointer is over the ancestor that called `.group(group)`.
    pub fn group_hover(mut self, group: impl Into<SharedString>, color: Hsla) -> Self {
        self.group_hover = Some((group.into(), color));
        self
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let path = match self.stroke_width {
            Some(width) => format!("{}#stroke-width={width}", self.path).into(),
            None => SharedString::new_static(self.path),
        };
        // GPUI's svg paints nothing without a colour of its own.
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        svg()
            .path(path)
            .size(self.size)
            .flex_none()
            .text_color(color)
            .when_some(self.group_hover, |this, (group, color)| {
                this.group_hover(group, move |style| style.text_color(color))
            })
    }
}
