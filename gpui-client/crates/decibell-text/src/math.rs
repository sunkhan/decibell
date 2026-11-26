//! TeX formulas: `decibell-tex` lays them out (KaTeX's layout as Chromium renders it), this
//! module turns its display list into something that paints without allocating.
//!
//! - Glyphs go through GPUI's glyph atlas from the KaTeX faces (`assets/fonts/katex/`, 0.5 MB
//!   in the binary, handed to the text system the first time a formula is laid out). A face
//!   KaTeX asks for that does not exist (bold of an upright-only family, a slanted one) is
//!   synthesised by the text system, as Chromium does.
//! - Rules are filled rectangles.
//! - SVG parts (roots, stretchy arrows and braces, `\cancel`) are built as small SVG
//!   documents once per formula and painted with `Window::paint_svg`, which rasterises each
//!   once per size into the glyph-like mask atlas and tints it.
//!
//! Formulas are cached by (source, mode, text size, scale): a hit costs one hash.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::fmt::Write as _;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use decibell_tex::{Item, KatexFont, LayoutOptions, Rendered};
use gpui::{
    App, Bounds, ContentMask, Font, FontFeatures, FontId, FontStyle, FontWeight, GlyphId, Hsla, Pixels, Point,
    SharedString, TransformationMatrix, Window, fill, point, px, size,
};

use crate::cache::{Generations, KeyHasher};
use crate::Engine;

/// Bytes of laid-out formulas kept.
const CACHE_BYTES: usize = 2 << 20;

macro_rules! katex_fonts {
    ($($file:literal),* $(,)?) => {
        [$(include_bytes!(concat!("../../../assets/fonts/katex/", $file)).as_slice()),*]
    };
}

static KATEX_FONTS: [&[u8]; 20] = katex_fonts![
    "KaTeX_AMS-Regular.ttf",
    "KaTeX_Caligraphic-Bold.ttf",
    "KaTeX_Caligraphic-Regular.ttf",
    "KaTeX_Fraktur-Bold.ttf",
    "KaTeX_Fraktur-Regular.ttf",
    "KaTeX_Main-Bold.ttf",
    "KaTeX_Main-BoldItalic.ttf",
    "KaTeX_Main-Italic.ttf",
    "KaTeX_Main-Regular.ttf",
    "KaTeX_Math-BoldItalic.ttf",
    "KaTeX_Math-Italic.ttf",
    "KaTeX_SansSerif-Bold.ttf",
    "KaTeX_SansSerif-Italic.ttf",
    "KaTeX_SansSerif-Regular.ttf",
    "KaTeX_Script-Regular.ttf",
    "KaTeX_Size1-Regular.ttf",
    "KaTeX_Size2-Regular.ttf",
    "KaTeX_Size3-Regular.ttf",
    "KaTeX_Size4-Regular.ttf",
    "KaTeX_Typewriter-Regular.ttf",
];

/// A colour in a formula: resolved when it is painted, so a theme switch needs no new layout.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Ink {
    /// The surrounding text colour.
    Text,
    /// The theme's error colour (`\textcolor` of KaTeX's `errorColor`).
    Error,
    Fixed(Hsla),
}

impl Ink {
    fn from_tex(color: decibell_tex::layout::Color) -> Ink {
        match color {
            decibell_tex::layout::Color::Current => Ink::Text,
            decibell_tex::layout::Color::Error => Ink::Error,
            decibell_tex::layout::Color::Rgba(rgba) => Ink::Fixed(gpui::rgba(rgba).into()),
        }
    }

    fn resolve(self, colors: &MathColors) -> Hsla {
        match self {
            Ink::Text => colors.text,
            Ink::Error => colors.error,
            Ink::Fixed(color) => color,
        }
    }
}

/// The two colours a formula takes from its surroundings.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MathColors {
    pub text: Hsla,
    pub error: Hsla,
}

struct MathGlyph {
    font: FontId,
    glyph: GlyphId,
    x: f32,
    y: f32,
    size: f32,
    ink: Ink,
    /// Part of the equation tag, which sits at the container's right edge.
    tag: bool,
}

struct MathRect {
    bounds: [f32; 4],
    ink: Ink,
    tag: bool,
}

struct MathSvg {
    /// The atlas key: unique per document.
    key: SharedString,
    document: Box<[u8]>,
    bounds: [f32; 4],
    clip: Option<[f32; 4]>,
    ink: Ink,
    tag: bool,
}

/// A laid-out formula, ready to paint. Coordinates are px from the formula's left edge and
/// its first baseline.
pub struct Math {
    /// Width of the widest line.
    pub width: f32,
    /// Extent above the first baseline and below the last line.
    pub ascent: f32,
    pub descent: f32,
    /// Width of the equation tag (`\tag`), 0 without one.
    pub tag_width: f32,
    glyphs: Box<[MathGlyph]>,
    rects: Box<[MathRect]>,
    svgs: Box<[MathSvg]>,
}

/// What `decibell-tex` made of a source.
pub enum Formula {
    Math(Rc<Math>),
    /// A TeX error: the renderer shows the raw source in the error colour, with KaTeX's
    /// message as its tooltip.
    Error(Rc<str>),
    /// KaTeX itself failed (or the formula nests too deep): the raw source as inline code.
    Internal,
}

impl Clone for Formula {
    fn clone(&self) -> Self {
        match self {
            Formula::Math(math) => Formula::Math(math.clone()),
            Formula::Error(message) => Formula::Error(message.clone()),
            Formula::Internal => Formula::Internal,
        }
    }
}

pub(crate) struct MathStore {
    fonts_loaded: Cell<bool>,
    formulas: RefCell<Generations<Formula>>,
}

impl Default for MathStore {
    fn default() -> Self {
        Self {
            fonts_loaded: Cell::new(false),
            formulas: RefCell::new(Generations::new(CACHE_BYTES)),
        }
    }
}

impl Engine {
    /// Lays out a formula for text of `text_px` px (`display`: `$$…$$`). Cached.
    pub fn formula(&self, tex: &str, display: bool, text_px: f32, scale: f32) -> Formula {
        let mut hasher = KeyHasher::default();
        tex.hash(&mut hasher);
        (display, text_px.to_bits(), scale.to_bits()).hash(&mut hasher);
        let key = hasher.finish();
        if let Some(hit) = self.math.formulas.borrow_mut().get(key) {
            return hit;
        }
        if !self.math.fonts_loaded.replace(true) {
            let fonts = KATEX_FONTS.iter().map(|bytes| Cow::Borrowed(*bytes)).collect();
            if let Err(error) = self.text_system.add_fonts(fonts) {
                log::error!("loading the KaTeX fonts: {error:#}");
            }
        }
        let options = LayoutOptions {
            text_px,
            dsf: scale,
            ..LayoutOptions::default()
        };
        let (formula, bytes) = match decibell_tex::layout(tex, display, &options) {
            Rendered::Math(layout) => {
                let math = self.build_math(&layout, key);
                let bytes = size_of::<Math>()
                    + size_of_val(&*math.glyphs)
                    + size_of_val(&*math.rects)
                    + math.svgs.iter().map(|svg| size_of::<MathSvg>() + svg.document.len()).sum::<usize>();
                (Formula::Math(Rc::new(math)), bytes)
            }
            Rendered::Error { message } => (Formula::Error(message.into()), 64),
            Rendered::Internal { .. } => (Formula::Internal, 16),
        };
        self.math.formulas.borrow_mut().insert(key, formula.clone(), bytes);
        formula
    }

    fn build_math(&self, layout: &decibell_tex::MathLayout, key: u64) -> Math {
        let mut glyphs = Vec::new();
        let mut rects = Vec::new();
        let mut svgs = Vec::new();
        for (items, tag) in [(&layout.items, false), (&layout.tag_items, true)] {
            for item in items {
                match item {
                    Item::Glyph(glyph) => {
                        let Some((font, id)) = self.math_glyph(glyph) else { continue };
                        glyphs.push(MathGlyph {
                            font,
                            glyph: id,
                            x: glyph.x,
                            y: glyph.y,
                            size: glyph.size,
                            ink: Ink::from_tex(glyph.color),
                            tag,
                        });
                    }
                    Item::Rect { x, y, w, h, color, .. } => rects.push(MathRect {
                        bounds: [*x, *y, *w, *h],
                        ink: Ink::from_tex(*color),
                        tag,
                    }),
                    Item::Path(path) => {
                        let [sx, sy, tx, ty] = path.transform;
                        let mut document = String::with_capacity(256);
                        let _ = write!(
                            document,
                            r#"<svg xmlns="http://www.w3.org/2000/svg" width="{w}" height="{h}" viewBox="0 0 {w} {h}"><g transform="matrix({sx} 0 0 {sy} {tx} {ty})">"#,
                            w = path.w,
                            h = path.h,
                        );
                        for data in &path.paths {
                            let _ = write!(document, r#"<path d="{}"/>"#, data.as_str());
                        }
                        document.push_str("</g>");
                        for [x1, y1, x2, y2, width] in &path.lines {
                            let _ = write!(
                                document,
                                r#"<line x1="{x1}" y1="{y1}" x2="{x2}" y2="{y2}" stroke="black" stroke-width="{width}"/>"#
                            );
                        }
                        document.push_str("</svg>");
                        svgs.push(MathSvg {
                            key: format!("decibell-math/{key:016x}/{}", svgs.len()).into(),
                            document: document.into_bytes().into(),
                            bounds: [path.x, path.y, path.w, path.h],
                            clip: path.clip,
                            ink: Ink::from_tex(path.color),
                            tag,
                        });
                    }
                }
            }
        }
        let first = layout.lines.first();
        let last = layout.lines.last();
        Math {
            width: layout.width,
            ascent: first.map_or(0., |line| line.ascent),
            descent: last.map_or(0., |line| line.baseline + line.descent),
            tag_width: layout.tag_width,
            glyphs: glyphs.into(),
            rects: rects.into(),
            svgs: svgs.into(),
        }
    }

    /// The font and glyph of one display-list glyph. KaTeX's faces have no shaping tables
    /// worth running (no kerning, ligatures or substitutions), so a glyph is its character's.
    fn math_glyph(&self, glyph: &decibell_tex::layout::Glyph) -> Option<(FontId, GlyphId)> {
        let font = self.katex_font(glyph.font, glyph.synthetic_bold, glyph.synthetic_italic);
        if !glyph.fallback
            && let Some(id) = self.text_system.glyph_for_char(font, glyph.ch)
        {
            return Some((font, id));
        }
        // The face lacks the character: whatever font the system has for it.
        let mut raw = Vec::new();
        let mut text = [0u8; 4];
        self.shape(glyph.ch.encode_utf8(&mut text), font, glyph.size, &mut raw);
        raw.first().filter(|shaped| shaped.id != 0).map(|shaped| (shaped.font, GlyphId(shaped.id)))
    }

    fn katex_font(&self, font: KatexFont, synthetic_bold: bool, synthetic_italic: bool) -> FontId {
        // Asking for the weight and style: the text system picks the face Chromium's font
        // matching picks, and synthesises what the family does not have.
        self.text_system.resolve_font(&Font {
            family: SharedString::new_static(font.family()),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: if font.is_bold() || synthetic_bold { FontWeight::BOLD } else { FontWeight::NORMAL },
            style: if font.is_italic() || synthetic_italic { FontStyle::Italic } else { FontStyle::Normal },
        })
    }
}

impl Math {
    /// Total height: from the top of the first line to the bottom of the last.
    pub fn height(&self) -> f32 {
        self.ascent + self.descent
    }

    /// Paints the formula with its left edge and first baseline at `origin`. `width` is
    /// the container's (an equation tag sits at its right edge). Paint phase only; nothing
    /// is allocated.
    pub fn paint(&self, origin: Point<Pixels>, width: f32, colors: &MathColors, window: &mut Window, cx: &mut App) {
        let scale = window.scale_factor();
        let left = f32::from(origin.x);
        // One snapped baseline for the whole formula: parts keep their distances.
        let baseline = (f32::from(origin.y) * scale).round() / scale;
        let tag_shift = (width - self.tag_width).max(self.width);
        let shift = |tag: bool| if tag { left + tag_shift } else { left };

        for rect in &self.rects {
            let [x, y, w, h] = rect.bounds;
            window.paint_quad(fill(
                Bounds::new(point(px(shift(rect.tag) + x), px(baseline + y)), size(px(w), px(h))),
                rect.ink.resolve(colors),
            ));
        }
        for svg in &self.svgs {
            let [x, y, w, h] = svg.bounds;
            let bounds = Bounds::new(point(px(shift(svg.tag) + x), px(baseline + y)), size(px(w), px(h)));
            let color = svg.ink.resolve(colors);
            let paint = |window: &mut Window| {
                let _ = window.paint_svg(
                    bounds,
                    svg.key.clone(),
                    Some(&svg.document),
                    TransformationMatrix::unit(),
                    color,
                    cx,
                );
            };
            match svg.clip {
                Some([cx_, cy, cw, ch]) => {
                    let clip = Bounds::new(point(px(shift(svg.tag) + cx_), px(baseline + cy)), size(px(cw), px(ch)));
                    window.with_content_mask(Some(ContentMask { bounds: clip }), paint);
                }
                None => paint(window),
            }
        }
        for glyph in &self.glyphs {
            let at = point(px(shift(glyph.tag) + glyph.x), px(baseline + glyph.y));
            let _ = window.paint_glyph(at, glyph.font, glyph.glyph, px(glyph.size), glyph.ink.resolve(colors));
        }
    }
}
