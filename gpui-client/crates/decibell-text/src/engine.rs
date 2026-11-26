//! The engine: the one object that owns what outlives a frame (resolved faces, line-break
//! tables, the emoji and math stores, the layout caches) and the only place that calls GPUI's
//! shaper.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use gpui::{App, Font, FontFeatures, FontId, FontRun, FontStyle, FontWeight, Global, GlyphId, TextSystem, px};
use icu_segmenter::options::{LineBreakOptions, LineBreakStrictness, LineBreakWordOption};
use icu_segmenter::{LineSegmenter, LineSegmenterBorrowed};

use crate::{Face, FontSpec, WordBreak};

/// One glyph as the shaper returned it, in visual order along the line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct RawGlyph {
    pub font: FontId,
    pub id: u32,
    pub x: f32,
    pub y: f32,
    /// Byte offset of the glyph's cluster in the shaped text.
    pub index: u32,
    /// The glyph comes from a colour emoji font.
    pub emoji: bool,
}

/// Counters for benches and tests.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Stats {
    /// Calls into the platform shaper.
    pub shapes: u64,
    /// Paragraphs built (each shapes its runs once).
    pub paragraphs: u64,
    /// Line-breaking passes.
    pub layouts: u64,
}

/// See the module docs. One per application, reached with [`Engine::global`]; tests and
/// benches build their own around a text system ([`Engine::new`]).
pub struct Engine {
    pub(crate) text_system: Arc<TextSystem>,
    faces: RefCell<HashMap<FontSpec, Face>>,
    /// A face → the same face with `liga` and `clig` off.
    unligated: RefCell<HashMap<FontId, FontId>>,
    /// (font, size bits) → the advance of a space.
    spaces: RefCell<HashMap<(FontId, u32), f32>>,
    break_normal: LineSegmenterBorrowed<'static>,
    break_all: LineSegmenterBorrowed<'static>,
    pub(crate) scratch: RefCell<crate::paragraph::Scratch>,
    /// The text being shaped with word joiners added, and where they were added.
    joined: RefCell<(String, Vec<u32>)>,
    pub(crate) stats: Cell<Stats>,
    #[cfg(any(target_os = "linux", target_os = "freebsd"))]
    fallback: crate::fallback::SystemFallback,
    /// The rasterised Twemoji.
    pub emoji: crate::emoji::EmojiStore,
    pub(crate) math: crate::math::MathStore,
    pub(crate) code: crate::code::CodeStore,
    pub(crate) labels: crate::cache::LayoutCache,
    pub(crate) rich: crate::rich::RichCache,
}

struct EngineGlobal(Rc<Engine>);
impl Global for EngineGlobal {}

impl Engine {
    /// The application's engine, created on first use.
    pub fn global(cx: &mut App) -> Rc<Engine> {
        if let Some(global) = cx.try_global::<EngineGlobal>() {
            return global.0.clone();
        }
        let engine = Engine::new(cx.text_system().clone());
        cx.set_global(EngineGlobal(engine.clone()));
        engine
    }

    /// An engine over a text system of your own (tests, benches, tools).
    pub fn new(text_system: Arc<TextSystem>) -> Rc<Engine> {
        // CSS `line-break: auto` is UAX #14's "normal" strictness in Blink: a line may
        // break before small kana and the prolonged sound mark.
        let mut normal = LineBreakOptions::default();
        normal.strictness = Some(LineBreakStrictness::Normal);
        let mut break_all = normal;
        break_all.word_option = Some(LineBreakWordOption::BreakAll);
        Rc::new(Engine {
            text_system,
            faces: RefCell::default(),
            unligated: RefCell::default(),
            spaces: RefCell::default(),
            break_normal: LineSegmenter::new_for_non_complex_scripts(normal),
            break_all: LineSegmenter::new_for_non_complex_scripts(break_all),
            scratch: RefCell::default(),
            joined: RefCell::default(),
            stats: Cell::default(),
            #[cfg(any(target_os = "linux", target_os = "freebsd"))]
            fallback: Default::default(),
            emoji: Default::default(),
            math: Default::default(),
            code: Default::default(),
            labels: crate::cache::LayoutCache::new(crate::cache::LABEL_CACHE_BYTES),
            rich: Default::default(),
        })
    }

    /// Resolves a face. Cached: a hit is one hash of the spec.
    pub fn face(&self, spec: &FontSpec) -> Face {
        if let Some(face) = self.faces.borrow().get(spec) {
            return *face;
        }
        let id = self.text_system.resolve_font(&Font {
            family: spec.family.clone(),
            features: FontFeatures::default(),
            fallbacks: None,
            weight: FontWeight(spec.weight as f32),
            style: if spec.italic {
                FontStyle::Italic
            } else {
                FontStyle::Normal
            },
        });
        let face = self.face_for_id(id);
        self.faces.borrow_mut().insert(spec.clone(), face);
        face
    }

    /// A face by family name and weight (`face(&FontSpec::new(family, weight))`).
    pub fn face_named(&self, family: &'static str, weight: u16, italic: bool) -> Face {
        self.face(&FontSpec {
            family: gpui::SharedString::new_static(family),
            weight,
            italic,
        })
    }

    pub(crate) fn face_for_id(&self, id: FontId) -> Face {
        let metrics = self.text_system.font_metrics(id);
        let em = metrics.units_per_em.max(1) as f32;
        Face {
            id,
            ascent: metrics.ascent.abs() / em,
            descent: metrics.descent.abs() / em,
            x_height: metrics.x_height.abs() / em,
        }
    }

    /// The GPUI text system the engine shapes with.
    pub fn text_system(&self) -> &Arc<TextSystem> {
        &self.text_system
    }

    pub fn stats(&self) -> Stats {
        self.stats.get()
    }

    pub(crate) fn count(&self, bump: impl FnOnce(&mut Stats)) {
        let mut stats = self.stats.get();
        bump(&mut stats);
        self.stats.set(stats);
    }

    pub(crate) fn segmenter(&self, word_break: WordBreak) -> LineSegmenterBorrowed<'static> {
        match word_break {
            WordBreak::Normal => self.break_normal,
            WordBreak::BreakAll => self.break_all,
        }
    }

    /// The advance of a space in `font` at `size` px (tab stops are multiples of it).
    pub(crate) fn space_advance(&self, font: FontId, size: f32) -> f32 {
        let key = (font, size.to_bits());
        if let Some(advance) = self.spaces.borrow().get(&key) {
            return *advance;
        }
        let advance = match self.text_system.glyph_for_char(font, ' ') {
            Some(glyph) => self.nominal_advance(font, glyph.0 as u16, size).unwrap_or(size / 4.),
            None => size / 4.,
        };
        self.spaces.borrow_mut().insert(key, advance);
        advance
    }

    /// The advance of a glyph without kerning, px at `size`.
    pub(crate) fn nominal_advance(&self, font: FontId, glyph: u16, size: f32) -> Option<f32> {
        self.text_system
            .glyph_advance(font, GlyphId(glyph as u32), px(size))
            .ok()
            .map(f32::from)
    }

    /// The font to shape with: the face itself, or with common ligatures off when the text
    /// is letter-spaced. CSS says a ligature must not swallow the spacing between its
    /// letters, and Blink turns `liga` and `clig` off for any non-zero `letter-spacing`.
    pub(crate) fn shaping_font(&self, face: Face, letter_spacing: f32) -> FontId {
        if letter_spacing == 0. {
            return face.id;
        }
        if let Some(id) = self.unligated.borrow().get(&face.id) {
            return *id;
        }
        let id = match self.text_system.get_font_for_id(face.id) {
            Some(mut font) => {
                font.features = FontFeatures(Arc::new(vec![("liga".into(), 0), ("clig".into(), 0)]));
                self.text_system.resolve_font(&font)
            }
            None => face.id,
        };
        self.unligated.borrow_mut().insert(face.id, id);
        id
    }

    /// Shapes `text` (no `\n`) in one font and size. Appends the glyphs in visual order and
    /// returns the width. A character no loaded font has is looked up among the installed
    /// fonts, once, and the text is shaped again with the font that covers it.
    pub(crate) fn shape(&self, text: &str, font: FontId, size: f32, out: &mut Vec<RawGlyph>) -> f32 {
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        let start = out.len();
        #[cfg_attr(not(any(target_os = "linux", target_os = "freebsd")), expect(unused_mut))]
        let mut width = self.shape_once(text, font, size, out);
        #[cfg(any(target_os = "linux", target_os = "freebsd"))]
        for _ in 0..crate::fallback::MAX_FONTS_PER_RUN {
            let missing = out[start..]
                .iter()
                .find(|glyph| glyph.id == 0 && needs_glyph(text, glyph.index as usize));
            let Some(missing) = missing else { break };
            let character = text[missing.index as usize..].chars().next().unwrap_or(' ');
            if !self.fallback.load_font_for(character, &self.text_system) {
                break;
            }
            out.truncate(start);
            width = self.shape_once(text, font, size, out);
        }
        width
    }

    fn shape_once(&self, text: &str, font: FontId, size: f32, out: &mut Vec<RawGlyph>) -> f32 {
        self.count(|stats| stats.shapes += 1);
        let mut joined = self.joined.borrow_mut();
        let (joined_text, joins) = &mut *joined;
        self.join_words(text, joined_text, joins);
        let shaped_text = if joins.is_empty() { text } else { joined_text.as_str() };
        let line = self.text_system.shape_line_uncached(
            shaped_text,
            px(size),
            &[FontRun {
                len: shaped_text.len(),
                font_id: font,
            }],
        );
        const JOINER_LEN: u32 = '\u{2060}'.len_utf8() as u32;
        let first = out.len();
        for run in &line.runs {
            for glyph in &run.glyphs {
                // Back to offsets in `text`; the joiners' own (empty) glyphs go.
                let mut index = glyph.index as u32;
                let before = joins.partition_point(|join| *join < index);
                if joins.get(before) == Some(&index) {
                    continue;
                }
                index -= before as u32 * JOINER_LEN;
                out.push(RawGlyph {
                    font: run.font_id,
                    id: glyph.id.0,
                    x: glyph.position.x.into(),
                    y: glyph.position.y.into(),
                    index,
                    emoji: glyph.is_emoji,
                });
            }
        }
        // Visual order, left to right. It already is, except for a line that starts with
        // right-to-left text: the shaper then takes the whole line as right-to-left and
        // returns its glyphs from the right edge.
        let shaped = &mut out[first..];
        if !shaped.is_sorted_by(|a, b| a.x <= b.x) {
            shaped.sort_by(|a, b| a.x.total_cmp(&b.x));
        }
        line.width.into()
    }

    /// GPUI's Linux shaper (cosmic-text) shapes a line word by word, cut at every UAX #14
    /// break opportunity, so a pair of glyphs across such a cut is never kerned: `/` + letter,
    /// `-` + letter, `://`. Chromium shapes the run whole. A WORD JOINER (U+2060, no width,
    /// no glyph) at each cut that is not next to white space removes the opportunity, and
    /// the shaper's pair lookups step over it: the widths then match Chromium's (a URL was
    /// 1.4 px wider without it).
    ///
    /// `joined` gets the text with joiners and `joins` their byte offsets in it; both are
    /// left empty when there is nothing to join.
    fn join_words(&self, text: &str, joined: &mut String, joins: &mut Vec<u32>) {
        joined.clear();
        joins.clear();
        if !cfg!(any(target_os = "linux", target_os = "freebsd")) {
            // DirectWrite and Core Text shape the whole run.
            return;
        }
        if text.bytes().all(|byte| byte.is_ascii_alphanumeric() || byte == b' ') {
            return;
        }
        let joinable = |c: char| !c.is_whitespace() && (c as u32) < 0x2E80 && c != '\u{2060}' && c != '\u{ad}';
        let mut copied = 0;
        for position in self.break_normal.segment_str(text) {
            if position == 0 || position >= text.len() {
                continue;
            }
            let before = text[..position].chars().next_back();
            let after = text[position..].chars().next();
            if before.is_some_and(joinable) && after.is_some_and(joinable) {
                joined.push_str(&text[copied..position]);
                joins.push(joined.len() as u32);
                joined.push('\u{2060}');
                copied = position;
            }
        }
        if !joins.is_empty() {
            joined.push_str(&text[copied..]);
        }
    }
}

/// Whether the character at `index` should have drawn something: not white space, a
/// control or a character shapers drop on purpose.
#[cfg(any(target_os = "linux", target_os = "freebsd"))]
fn needs_glyph(text: &str, index: usize) -> bool {
    text.get(index..)
        .and_then(|rest| rest.chars().next())
        .is_some_and(|c| {
            !c.is_whitespace()
                && !c.is_control()
                && !matches!(c, '\u{200b}'..='\u{200f}' | '\u{2028}'..='\u{202e}' | '\u{2060}'..='\u{206f}' | '\u{fe00}'..='\u{fe0f}' | '\u{feff}' | '\u{ad}' | '\u{e0000}'..='\u{e0fff}')
        })
}
