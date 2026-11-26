//! Styles. The engine knows no theme: every font, size and colour arrives in these structs.

use gpui::Hsla;

use crate::Face;

/// The style of a run of text.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct TextStyle {
    pub face: Face,
    /// Font size in px.
    pub size: f32,
    pub color: Hsla,
    pub line_height: LineHeight,
    /// CSS `letter-spacing` in px, added after every character.
    pub letter_spacing: f32,
    pub underline: bool,
    pub strikethrough: bool,
    /// A background box behind the run (inline code), one per line the run covers.
    pub chip: Option<Chip>,
    /// Space before the run's first character (CSS `margin-left` on an inline), px.
    pub margin_start: f32,
    /// `false` for text a selection skips (`select-none`: the "(edited)" marker).
    pub selectable: bool,
}

impl TextStyle {
    /// A plain style: no spacing, no decoration, line height 1.5 × the size (what the
    /// renderer inherits from Tailwind's preflight).
    pub fn new(face: Face, size: f32, color: Hsla) -> Self {
        Self {
            face,
            size,
            color,
            line_height: LineHeight::Factor(1.5),
            letter_spacing: 0.,
            underline: false,
            strikethrough: false,
            chip: None,
            margin_start: 0.,
            selectable: true,
        }
    }

    /// A unitless CSS `line-height` (`leading-body`, `leading-[1.5]`).
    pub fn leading(mut self, factor: f32) -> Self {
        self.line_height = LineHeight::Factor(factor);
        self
    }

    /// A CSS `line-height` in px (`leading-[22px]`).
    pub fn line_height_px(mut self, px: f32) -> Self {
        self.line_height = LineHeight::Px(px);
        self
    }

    pub fn letter_spacing(mut self, px: f32) -> Self {
        self.letter_spacing = px;
        self
    }

    pub fn color(mut self, color: Hsla) -> Self {
        self.color = color;
        self
    }

    pub fn face(mut self, face: Face) -> Self {
        self.face = face;
        self
    }
}

/// CSS `line-height`. The two forms round differently in Chromium (a factor multiplies the
/// font size after it was snapped to 1/64 px), and a row's exact height depends on it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum LineHeight {
    /// A multiple of the run's own font size (a unitless CSS number, inherited as such).
    Factor(f32),
    /// A length in px.
    Px(f32),
}

impl LineHeight {
    /// In px for a font of `size` px (unrounded).
    pub fn px(self, size: f32) -> f32 {
        match self {
            LineHeight::Factor(factor) => factor * size,
            LineHeight::Px(px) => px,
        }
    }
}

/// The box of an inline code run: CSS `background`, `border-radius`, `padding` with
/// `box-decoration-break: clone` (every line fragment gets the whole box).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Chip {
    pub background: Hsla,
    pub radius: f32,
    pub pad_x: f32,
    pub pad_y: f32,
}

/// CSS `white-space`, as far as the client uses it.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WhiteSpace {
    /// `normal`: runs of spaces, tabs and newlines collapse to one space; lines wrap.
    #[default]
    Normal,
    /// `nowrap`: collapses like `Normal`; one line.
    Nowrap,
    /// `pre-wrap`: everything is kept, `\n` breaks, lines wrap, spaces at the end of a
    /// line hang past the edge instead of wrapping.
    PreWrap,
    /// `pre`: everything is kept, only `\n` breaks (code blocks).
    Pre,
}

/// CSS `word-break`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum WordBreak {
    #[default]
    Normal,
    /// Letters and digits break anywhere (`break-all`: message bodies).
    BreakAll,
}

/// CSS `overflow-wrap`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum OverflowWrap {
    /// A word longer than the line overflows.
    #[default]
    Normal,
    /// A word longer than the line breaks between any two characters (`anywhere` and
    /// `break-word`: they differ only in min-content sizing, which is not modelled).
    Anywhere,
}

/// How a paragraph wraps and where it stops.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct Wrapping {
    pub white_space: WhiteSpace,
    pub word_break: WordBreak,
    pub overflow_wrap: OverflowWrap,
    /// CSS `tab-size` (Tailwind's preflight sets 4).
    pub tab_size: u8,
    /// `-webkit-line-clamp`: at most this many lines, the last one ending in "…" when text
    /// was cut. 0 = no limit.
    pub max_lines: u16,
    /// `text-overflow: ellipsis` for a line that does not fit (with `Nowrap` / `Pre`).
    pub ellipsis: bool,
}

impl Default for Wrapping {
    fn default() -> Self {
        Self::NORMAL
    }
}

impl Wrapping {
    /// Ordinary UI text.
    pub const NORMAL: Self = Self {
        white_space: WhiteSpace::Normal,
        word_break: WordBreak::Normal,
        overflow_wrap: OverflowWrap::Normal,
        tab_size: 4,
        max_lines: 0,
        ellipsis: false,
    };
    /// One line, cut with "…" (Tailwind `truncate`).
    pub const TRUNCATE: Self = Self {
        white_space: WhiteSpace::Nowrap,
        ellipsis: true,
        ..Self::NORMAL
    };
    /// One line, never cut.
    pub const NOWRAP: Self = Self {
        white_space: WhiteSpace::Nowrap,
        ..Self::NORMAL
    };
    /// A message body: `whitespace-pre-wrap break-all [overflow-wrap:anywhere]`.
    pub const MESSAGE: Self = Self {
        white_space: WhiteSpace::PreWrap,
        word_break: WordBreak::BreakAll,
        overflow_wrap: OverflowWrap::Anywhere,
        ..Self::NORMAL
    };
    /// Pre-wrapped text that breaks at words, long words anywhere (`whitespace-pre-wrap
    /// break-words`).
    pub const PRE_WRAP: Self = Self {
        white_space: WhiteSpace::PreWrap,
        overflow_wrap: OverflowWrap::Anywhere,
        ..Self::NORMAL
    };
    /// A code block's lines.
    pub const PRE: Self = Self {
        white_space: WhiteSpace::Pre,
        ..Self::NORMAL
    };

    /// `line-clamp-N`.
    pub const fn clamp(mut self, lines: u16) -> Self {
        self.max_lines = lines;
        self
    }

    pub(crate) fn wraps(&self) -> bool {
        matches!(self.white_space, WhiteSpace::Normal | WhiteSpace::PreWrap)
    }

    pub(crate) fn collapses(&self) -> bool {
        matches!(self.white_space, WhiteSpace::Normal | WhiteSpace::Nowrap)
    }
}

/// Bit-exact hashing of the float fields, for cache keys.
pub(crate) fn hash_style<H: std::hash::Hasher>(style: &TextStyle, state: &mut H) {
    use std::hash::Hash;
    style.face.hash(state);
    match style.line_height {
        LineHeight::Factor(factor) => (0u8, factor.to_bits()).hash(state),
        LineHeight::Px(px) => (1u8, px.to_bits()).hash(state),
    }
    for value in [
        style.size,
        style.letter_spacing,
        style.margin_start,
        style.color.h,
        style.color.s,
        style.color.l,
        style.color.a,
    ] {
        value.to_bits().hash(state);
    }
    (style.underline, style.strikethrough, style.selectable).hash(state);
    if let Some(chip) = &style.chip {
        for value in [
            chip.radius,
            chip.pad_x,
            chip.pad_y,
            chip.background.h,
            chip.background.s,
            chip.background.l,
            chip.background.a,
        ] {
            value.to_bits().hash(state);
        }
    }
}
