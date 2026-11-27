//! Text. Every piece of UI text is a [`Label`]: the theme's fonts and sizes in, a
//! `decibell_text::Label` out (Chromium's line breaking and line boxes, letter-spacing,
//! synthesised italics, "…" and line clamp; `gpui-client/crates/decibell-text/README.md`).

use decibell_text::{Engine, FontSpec, LineHeight, TextStyle, Wrapping};
use gpui::{App, Hsla, IntoElement, RenderOnce, SharedString, Window};

use crate::{ActiveTheme as _, FontRole};

/// The renderer's `<body>` has no font size of its own, so its `letter-spacing:
/// var(--t-ls-body)` (an em value) is computed against the browser's 16 px and inherited as
/// that many px by all text, whatever its size.
const BODY_FONT_SIZE: f32 = 16.;

/// One run of text in one style.
///
/// Defaults: the theme's UI font at the body size, weight 400, the inherited colour, the
/// theme's body letter-spacing, and a line height of 1.5 × the size, which is what the
/// renderer inherits from Tailwind's preflight. Sizes are px: a role from
/// `cx.theme().scaled` (`text_body`, `text_meta`, …) follows the user's text size, a literal
/// (`text-[12px]` in the TSX) does not. The text wraps at its container's width unless
/// [`truncate`](Self::truncate), [`nowrap`](Self::nowrap) or [`clamp`](Self::clamp) says
/// otherwise.
#[derive(IntoElement)]
pub struct Label {
    text: SharedString,
    font: FontRole,
    size: Option<f32>,
    weight: f32,
    italic: bool,
    color: Option<Hsla>,
    line_height: Option<f32>,
    tracking: Option<f32>,
    wrapping: Wrapping,
    selectable: Option<gpui::ElementId>,
}

impl Label {
    pub fn new(text: impl Into<SharedString>) -> Self {
        Self {
            text: text.into(),
            font: FontRole::Sans,
            size: None,
            weight: 400.,
            italic: false,
            color: None,
            line_height: None,
            tracking: None,
            wrapping: Wrapping::NORMAL,
            selectable: None,
        }
    }

    /// Text the user can select and copy (`select-text`: an address, a fingerprint, a command
    /// line): drag, double click, triple click, Ctrl+C. `id` keeps its selection between frames;
    /// unique among the label's siblings, as any element id.
    pub fn selectable(mut self, id: impl Into<gpui::ElementId>) -> Self {
        self.selectable = Some(id.into());
        self
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

    /// `italic`. No bundled family has an italic face: the upright one is slanted, as
    /// Chromium does for the renderer.
    pub fn italic(mut self) -> Self {
        self.italic = true;
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

    /// Letter-spacing in em of the label's own size (`tracking-[0.07em]`, the theme's
    /// `tracking_section`), instead of the body's.
    pub fn tracking(mut self, em: f32) -> Self {
        self.tracking = Some(em);
        self
    }

    /// One line, cut with an ellipsis (`truncate`).
    pub fn truncate(mut self) -> Self {
        self.wrapping = Wrapping::TRUNCATE;
        self
    }

    /// One line, never cut (`whitespace-nowrap`).
    pub fn nowrap(mut self) -> Self {
        self.wrapping = Wrapping::NOWRAP;
        self
    }

    /// At most `lines` lines, the last ending in "…" (`line-clamp-N`).
    pub fn clamp(mut self, lines: u16) -> Self {
        self.wrapping = Wrapping::NORMAL.clamp(lines);
        self
    }
}

impl RenderOnce for Label {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let engine = Engine::global(cx);
        let theme = cx.theme();
        let size = self.size.unwrap_or(theme.scaled.text_body);
        let face = engine.face(&FontSpec {
            family: theme.font(self.font),
            weight: self.weight.round() as u16,
            italic: self.italic,
        });
        let mut style = TextStyle::new(face, size, self.color.unwrap_or_default());
        style.line_height = match self.line_height {
            Some(px) => LineHeight::Px(px),
            None => LineHeight::Factor(1.5),
        };
        style.letter_spacing = match self.tracking {
            Some(em) => em * size,
            None => theme.palette.t_ls_body * BODY_FONT_SIZE,
        };
        let mut label = decibell_text::Label::new(self.text, style).wrapping(self.wrapping);
        if let Some(id) = self.selectable {
            let selection = window.use_keyed_state(id, cx, |_, _| decibell_text::TextSelection::default());
            label = label.selectable(selection);
        }
        match self.color {
            Some(_) => label,
            None => label.inherit_color(),
        }
    }
}
