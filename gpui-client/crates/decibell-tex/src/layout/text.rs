//! Text as Chromium (Blink + Skia + FreeType + HarfBuzz) measures it with KaTeX's fonts: CSS
//! font matching within a family's `@font-face`s, advances from `hmtx` at the size FreeType
//! actually uses, and the integer-rounded ascent/descent that size line boxes.

use crate::data::font_advances;
use crate::fonts::KatexFont;
use crate::layout::css::Family;

/// The face CSS font matching picks for (family, bold, italic), plus the synthesis Chromium
/// then applies (a weight or style the family lacks).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub struct Face {
    pub font: KatexFont,
    pub synthetic_bold: bool,
    pub synthetic_italic: bool,
}

/// CSS Fonts 4 §5.2 matching over KaTeX's `@font-face` rules (katex.css).
pub fn select_face(family: Family, bold: bool, italic: bool) -> Face {
    use KatexFont as K;
    // (face, weight 700?, italic?)
    let faces: &[(K, bool, bool)] = match family {
        Family::Main => &[
            (K::MainRegular, false, false),
            (K::MainBold, true, false),
            (K::MainItalic, false, true),
            (K::MainBoldItalic, true, true),
        ],
        Family::Math => &[
            (K::MathItalic, false, true),
            (K::MathBoldItalic, true, true),
        ],
        Family::Ams => &[(K::AmsRegular, false, false)],
        Family::Caligraphic => &[
            (K::CaligraphicRegular, false, false),
            (K::CaligraphicBold, true, false),
        ],
        Family::Fraktur => &[
            (K::FrakturRegular, false, false),
            (K::FrakturBold, true, false),
        ],
        Family::SansSerif => &[
            (K::SansSerifRegular, false, false),
            (K::SansSerifBold, true, false),
            (K::SansSerifItalic, false, true),
        ],
        Family::Script => &[(K::ScriptRegular, false, false)],
        Family::Size1 => &[(K::Size1Regular, false, false)],
        Family::Size2 => &[(K::Size2Regular, false, false)],
        Family::Size3 => &[(K::Size3Regular, false, false)],
        Family::Size4 => &[(K::Size4Regular, false, false)],
        Family::Typewriter => &[(K::TypewriterRegular, false, false)],
    };
    // Style first: italic prefers italic faces, normal prefers normal ones; then weight.
    let style_ok: Vec<&(K, bool, bool)> = {
        let same: Vec<_> = faces.iter().filter(|f| f.2 == italic).collect();
        if same.is_empty() {
            faces.iter().collect()
        } else {
            same
        }
    };
    let pick = style_ok
        .iter()
        .find(|f| f.1 == bold)
        .or_else(|| style_ok.first())
        .copied()
        .unwrap();
    Face {
        font: pick.0,
        synthetic_bold: bold && !pick.1,
        synthetic_italic: italic && !pick.2,
    }
}

/// The size FreeType renders and measures at: Skia passes the size as 26.6 fixed point
/// (truncated), in device pixels.
pub fn shaping_size(font_px: f32) -> f64 {
    ((font_px as f64) * 64.0).floor() / 64.0
}

/// Advance of `c` in device px, or `None` when the face has no glyph for it (Chromium then
/// falls back to a system font).
pub fn advance(font: KatexFont, c: char, size: f64) -> Option<f64> {
    font_advances::advance(font, c).map(|u| u as f64 / 1000.0 * size)
}

/// Whether Chromium draws nothing and advances by nothing for `c` (default-ignorable code
/// points such as U+200B in `.vlist-s`).
pub fn is_ignorable(c: char) -> bool {
    matches!(
        c,
        '\u{200b}' | '\u{200c}' | '\u{200d}' | '\u{2060}' | '\u{feff}' | '\u{fe00}'..='\u{fe0f}'
    )
}

/// Blink's integer ascent/descent for a face at `font_px` device px (SimpleFontData: Skia's
/// hhea-based metrics rounded, then one pixel moved from the ascent to the descent when the
/// descent was rounded down, as Linux does with subpixel positioning).
pub fn ascent_descent(font: KatexFont, font_px: f32) -> (i32, i32) {
    let (asc, desc, _gap) = font_advances::hhea(font);
    let a = asc as f64 / 1000.0 * font_px as f64;
    let d = -(desc as f64) / 1000.0 * font_px as f64;
    let mut ar = a.round() as i32;
    let mut dr = d.round() as i32;
    if (dr as f64) < d && ar >= 1 {
        dr += 1;
        ar -= 1;
    }
    (ar, dr)
}
