//! Faces: a (family, weight, italic) resolved once to GPUI's `FontId`, with the metrics line
//! boxes are built from.

use gpui::{FontId, SharedString};

/// What a caller asks for. The family is one GPUI knows (the bundled `Decibell …` families, a
/// KaTeX family, or a system family on platforms that have them loaded).
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct FontSpec {
    pub family: SharedString,
    /// CSS weight. The nearest face of the family is used; at 600 and above a family whose
    /// faces are all lighter than 600 is emboldened, as Chromium does.
    pub weight: u16,
    /// Italic. No bundled family has an italic face: the upright one is slanted by 0.25
    /// (Skia's synthetic italic), as Chromium does.
    pub italic: bool,
}

impl FontSpec {
    pub fn new(family: impl Into<SharedString>, weight: u16) -> Self {
        Self {
            family: family.into(),
            weight,
            italic: false,
        }
    }

    pub fn italic(mut self) -> Self {
        self.italic = true;
        self
    }
}

/// A resolved face. `Copy`, 24 bytes: keep it in your style structs and resolve once per
/// theme or text-size change ([`crate::Engine::face`]), not per frame.
#[derive(Clone, Copy, Debug)]
pub struct Face {
    pub(crate) id: FontId,
    /// hhea ascent and descent in em (both positive).
    pub(crate) ascent: f32,
    pub(crate) descent: f32,
    /// OS/2 x-height in em.
    pub(crate) x_height: f32,
}

impl PartialEq for Face {
    fn eq(&self, other: &Self) -> bool {
        self.id == other.id
    }
}
impl Eq for Face {}
impl std::hash::Hash for Face {
    fn hash<H: std::hash::Hasher>(&self, state: &mut H) {
        self.id.0.hash(state);
    }
}

impl Face {
    /// GPUI's id for the face (a slanted or emboldened face has its own).
    pub fn font_id(&self) -> FontId {
        self.id
    }

    /// The ascent and descent Chromium sizes line boxes with, in device px, for a font of
    /// `size` px on a display of `scale` device pixels per px: the hhea metrics at the
    /// device size, each rounded to a whole pixel. Above scale 1 Blink positions glyphs at
    /// sub-pixel offsets and then makes sure descenders have room: a descent that was
    /// rounded down takes one pixel from the ascent (measured for every bundled face at
    /// 8–41 px and scales 1, 1.25, 1.5, 1.75 and 2: `tools/chromium/metrics.html`).
    pub fn line_metrics(&self, size: f32, scale: f32) -> (f32, f32) {
        let ascent = self.ascent * size * scale;
        let descent = self.descent * size * scale;
        let mut rounded_ascent = ascent.round();
        let mut rounded_descent = descent.round();
        if scale > 1. && rounded_descent < descent && rounded_ascent >= 1. {
            rounded_descent += 1.;
            rounded_ascent -= 1.;
        }
        (rounded_ascent, rounded_descent)
    }

    /// x-height in px at `size`.
    pub fn x_height(&self, size: f32) -> f32 {
        self.x_height * size
    }
}
