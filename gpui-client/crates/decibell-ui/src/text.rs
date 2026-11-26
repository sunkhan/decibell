//! Text. Every piece of UI text is a [`Label`], so that the element behind it can change in
//! one place (GPUI's built-in text today; `decibell-text` for rich text later).

use gpui::{
    App, FontWeight, Hsla, IntoElement, RenderOnce, SharedString, Window, div, prelude::*, px,
};

use crate::{ActiveTheme as _, FontRole};

/// One run of text in one style.
///
/// Defaults: the theme's UI font at the body size, weight 400, the inherited colour, and a
/// line height of 1.5 × the size, which is what the renderer inherits from Tailwind's
/// preflight. Sizes are px: a role from `cx.theme().scaled` (`text_body`, `text_meta`, …)
/// follows the user's text size, a literal (`text-[12px]` in the TSX) does not.
#[derive(IntoElement)]
pub struct Label {
    text: SharedString,
    font: FontRole,
    size: Option<f32>,
    weight: f32,
    color: Option<Hsla>,
    line_height: Option<f32>,
    truncate: bool,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            font: FontRole::Sans,
            size: None,
            weight: 400.,
            color: None,
            line_height: None,
            truncate: false,
        }
    }

    pub fn font(mut self, font: FontRole) -> Self {
        self.font = font;
        self
    }

    /// Font size in px.
    pub fn size(mut self, size: f32) -> Self {
        self.size = Some(size);
        self
    }

    /// CSS weight: 400, 500 (`font-medium`), 600 (`font-semibold`), 700 (`font-bold`).
    pub fn weight(mut self, weight: f32) -> Self {
        self.weight = weight;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Line height in px (a CSS line-height is a multiple of the font size: multiply first).
    pub fn line_height(mut self, line_height: f32) -> Self {
        self.line_height = Some(line_height);
        self
    }

    /// One line, cut with an ellipsis (`truncate`).
    pub fn truncate(mut self) -> Self {
        self.truncate = true;
        self
    }
}

impl RenderOnce for Label {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let theme = cx.theme();
        let size = self.size.unwrap_or(theme.scaled.text_body);
        div()
            .font_family(theme.font(self.font))
            .font_weight(FontWeight(self.weight))
            .text_size(px(size))
            .line_height(px(self.line_height.unwrap_or(size * 1.5)))
            .when_some(self.color, |this, color| this.text_color(color))
            .when(self.truncate, |this| this.min_w_0().truncate())
            .child(self.text)
    }
}
