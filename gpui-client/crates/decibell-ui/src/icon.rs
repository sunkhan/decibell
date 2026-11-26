//! Icons: the bundled SVGs, rasterised by GPUI at device-pixel size and tinted when drawn.

use gpui::{App, Hsla, IntoElement, Pixels, RenderOnce, SharedString, Window, prelude::*, px, svg};

include!(concat!(env!("OUT_DIR"), "/icon_names.rs"));

/// An icon from `gpui-client/assets/icons`, by name: `Icon::new(IconName::Close)`.
///
/// 16 px and the inherited text colour unless told otherwise (`currentColor` in the SVG). The
/// renderer's glyphs are stroked at the width written in their file (lucide's 2, the Select
/// chevron's 2.5, …); `.stroke_width(w)` redraws one at another width in its 24-unit box
/// (lucide-react's `strokeWidth`, the voice stage's `bold` 2.5).
///
/// ```ignore
/// Icon::new(IconName::StageMic).size(px(18.)).stroke_width(2.5).color(theme.color_text_secondary)
/// ```
#[derive(IntoElement)]
pub struct Icon {
    name: IconName,
    size: Pixels,
    color: Option<Hsla>,
    stroke_width: Option<f32>,
    group_hover: Option<(SharedString, Hsla)>,
}

impl Icon {
    pub fn new(name: IconName) -> Self {
        Self {
            name,
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

    /// The stroke width in the icon's own units (24 for most), replacing the file's.
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

/// The asset path of `name` drawn at `stroke_width` (see [`crate::Assets`]).
pub(crate) fn icon_path(name: IconName, stroke_width: Option<f32>) -> SharedString {
    match stroke_width {
        Some(width) => format!("{}#stroke-width={width}", name.path()).into(),
        None => SharedString::new_static(name.path()),
    }
}

impl RenderOnce for Icon {
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        // GPUI's svg paints nothing without a colour of its own.
        let color = self.color.unwrap_or_else(|| window.text_style().color);
        svg()
            .path(icon_path(self.name, self.stroke_width))
            .size(self.size)
            .flex_none()
            .text_color(color)
            .when_some(self.group_hover, |this, (group, color)| {
                this.group_hover(group, move |style| style.text_color(color))
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_name_has_its_file() {
        for name in IconName::ALL {
            assert!(
                crate::Assets
                    .load_icon(name.path())
                    .is_some_and(|bytes| bytes.starts_with(b"<")),
                "{name:?}"
            );
        }
        assert_eq!(IconName::WindowClose.path(), "icons/window/close.svg");
    }

    /// Every icon goes through GPUI's own SVG rasteriser and leaves ink (a file usvg can't read,
    /// or that draws nothing, would be an invisible icon).
    #[test]
    fn every_icon_rasterises() {
        let renderer = gpui::SvgRenderer::new(std::sync::Arc::new(crate::Assets));
        let mut failures = Vec::new();
        for name in IconName::ALL {
            let bytes = crate::Assets.load_icon(name.path()).unwrap();
            match renderer.render_single_frame(bytes, 1.) {
                Ok(image) => {
                    let ink = image
                        .as_bytes(0)
                        .is_some_and(|pixels| pixels.as_chunks::<4>().0.iter().any(|bgra| bgra[3] > 0));
                    if !ink {
                        failures.push(format!("{name:?}: no ink"));
                    }
                }
                Err(error) => failures.push(format!("{name:?}: {error}")),
            }
        }
        assert!(failures.is_empty(), "{}", failures.join("\n"));
    }
}
