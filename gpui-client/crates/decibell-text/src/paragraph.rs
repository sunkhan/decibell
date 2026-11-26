//! A paragraph: inline content shaped once, independent of the width it will wrap at.
//!
//! Built from styled text runs and inline boxes with [`ParagraphBuilder`]. Shaping is the
//! expensive half of text layout, so it happens here and never again for the same content:
//! a [`crate::Layout`] at any width only decides where lines break and points back at these
//! glyphs.

use std::borrow::Cow;

use gpui::FontId;
use unicode_segmentation::UnicodeSegmentation;

use crate::engine::RawGlyph;
use crate::{Engine, TextStyle, WordBreak, Wrapping};

/// The character an inline box occupies in a paragraph's text (U+FFFC, 3 bytes), as in
/// Chromium: line breaking and positions treat a box as one character.
pub const OBJECT_REPLACEMENT: &str = "\u{fffc}";

pub(crate) mod flag {
    /// Unit: a U+0020 space. Hangs at the end of a line.
    pub const SPACE: u8 = 1;
    /// Unit: a tab; its advance depends on where it lands.
    pub const TAB: u8 = 2;
    /// Unit: a `\n` that ends the line.
    pub const NEWLINE: u8 = 4;
    /// Unit: a line may break before it (UAX #14 with the paragraph's `word-break`).
    pub const BREAK_BEFORE: u8 = 8;
    /// Unit: an inline box.
    pub const BOX: u8 = 16;
    /// Unit: a soft hyphen (U+00AD). A line that ends after it shows a hyphen.
    pub const SOFT_HYPHEN: u8 = 32;

    /// Item: glyphs run right to left (visual order is the reverse of the text's).
    pub const RTL: u8 = 1;
    /// Item: drawn with a colour emoji font.
    pub const EMOJI_FONT: u8 = 2;
    /// Item: the first / last piece of its inline run (where margins and chip ends go).
    pub const FIRST: u8 = 4;
    pub const LAST: u8 = 8;
    /// Item: an inline box (one unit, no glyph).
    pub const IS_BOX: u8 = 16;
    /// Item: a hard line break (one unit, no glyph).
    pub const IS_BREAK: u8 = 32;
}

/// A stretch of one inline run in one font and one direction.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Item {
    /// Byte range in [`Paragraph::text`].
    pub text: (u32, u32),
    pub units: (u32, u32),
    pub glyphs: (u32, u32),
    pub font: FontId,
    pub style: u16,
    pub flags: u8,
    /// Which `text()` / `inline_box()` call it came from.
    pub inline: u32,
    /// The caller's tag for that call.
    pub tag: u32,
    /// Offset of `text.0` in the text that call was given.
    pub local: u32,
}

/// What a caret can stand before: a grapheme cluster (or several the font drew as one
/// ligature), a box, a line break. Units are in text order.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Unit {
    pub text: u32,
    /// px, letter-spacing included.
    pub advance: f32,
    /// First glyph of the unit in its item's glyphs (visual order).
    pub glyph: u32,
    pub flags: u8,
}

/// 12 bytes a glyph.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Glyph {
    /// From the item's left edge, px, letter-spacing included.
    pub x: f32,
    /// Byte offset of its unit in [`Paragraph::text`].
    pub cluster: u32,
    pub id: u16,
    /// Above (negative) or below the baseline, in 1/64 px.
    pub y: i16,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct InlineBox {
    pub width: f32,
    pub height: f32,
    /// How far the box's bottom sits below the baseline (CSS `vertical-align: -N`).
    pub below: f32,
    pub tag: u32,
    pub unit: u32,
}

#[derive(Clone, Copy, Debug)]
pub(crate) struct Mark {
    pub font: FontId,
    pub glyph: u16,
    pub advance: f32,
}

/// Shaped inline content. Immutable; share it with `Rc`.
#[derive(Debug)]
pub struct Paragraph {
    pub(crate) text: Box<str>,
    pub(crate) base: TextStyle,
    pub(crate) wrapping: Wrapping,
    pub(crate) scale: f32,
    pub(crate) styles: Box<[TextStyle]>,
    pub(crate) items: Box<[Item]>,
    pub(crate) units: Box<[Unit]>,
    pub(crate) glyphs: Box<[Glyph]>,
    pub(crate) boxes: Box<[InlineBox]>,
    /// The "…" of a cut line and the hyphen of a line that ends at a soft hyphen, in the
    /// base style.
    pub(crate) ellipsis: Option<Mark>,
    pub(crate) hyphen: Option<Mark>,
}

impl Paragraph {
    /// The scale factor the paragraph was shaped for.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The paragraph's text: the runs concatenated, [`OBJECT_REPLACEMENT`] for each box.
    /// Positions ([`crate::Pos`]) are byte offsets into it.
    pub fn text(&self) -> &str {
        &self.text
    }

    pub fn is_empty(&self) -> bool {
        self.units.is_empty()
    }

    /// Heap bytes this paragraph holds.
    pub fn heap_bytes(&self) -> usize {
        self.text.len()
            + size_of_val(&*self.styles)
            + size_of_val(&*self.items)
            + size_of_val(&*self.units)
            + size_of_val(&*self.glyphs)
            + size_of_val(&*self.boxes)
    }

    /// The run a text offset lies in: the tag its `text()` / `inline_box()` call was given
    /// and the byte offset inside that call's text. At a boundary between runs the earlier
    /// one answers with its end unless `offset` is 0.
    pub fn locate(&self, offset: u32) -> Option<(u32, u32)> {
        let offset = offset.min(self.text.len() as u32);
        let at = self.items.partition_point(|item| item.text.1 < offset);
        let item = self.items.get(at).or(self.items.last())?;
        Some((item.tag, item.local + offset.saturating_sub(item.text.0)))
    }

    /// The text offset of byte `local` of the run tagged `tag` (the first run with that tag
    /// that reaches it).
    pub fn offset_of(&self, tag: u32, local: u32) -> Option<u32> {
        let mut last = None;
        for item in self.items.iter().filter(|item| item.tag == tag) {
            let len = item.text.1 - item.text.0;
            if local >= item.local && local <= item.local + len {
                return Some(item.text.0 + (local - item.local));
            }
            last = Some(item.text.1);
        }
        last
    }

    /// (text offset, advance in px, flags) of every caret unit: for tools and tests.
    #[doc(hidden)]
    pub fn debug_units(&self) -> Vec<(u32, f32, u8)> {
        self.units.iter().map(|unit| (unit.text, unit.advance, unit.flags)).collect()
    }

    pub(crate) fn item_of_unit(&self, unit: u32) -> usize {
        self.items.partition_point(|item| item.units.1 <= unit)
    }

    /// The end of a unit's text.
    pub(crate) fn unit_end(&self, unit: usize) -> u32 {
        match self.units.get(unit + 1) {
            Some(next) => next.text,
            None => self.text.len() as u32,
        }
    }

    /// The glyphs of units `from..to` of `item` (a contiguous slice: an item has one font
    /// and one direction).
    pub(crate) fn glyph_range(&self, item: &Item, from: u32, to: u32) -> (u32, u32) {
        if from >= to {
            return (item.glyphs.0, item.glyphs.0);
        }
        let glyph_at = |unit: u32| self.units[unit as usize].glyph;
        if item.flags & flag::RTL == 0 {
            let end = if to >= item.units.1 { item.glyphs.1 } else { glyph_at(to) };
            (glyph_at(from), end)
        } else {
            let end = if from <= item.units.0 { item.glyphs.1 } else { glyph_at(from - 1) };
            (glyph_at(to - 1), end)
        }
    }
}

/// Reused buffers: building a paragraph allocates only what the paragraph keeps.
#[derive(Default)]
pub(crate) struct Scratch {
    raw: Vec<RawGlyph>,
    unit_of_glyph: Vec<u32>,
    spacing: Vec<f32>,
    segments: Vec<Segment>,
    text: String,
    styles: Vec<TextStyle>,
    items: Vec<Item>,
    units: Vec<Unit>,
    glyphs: Vec<Glyph>,
    boxes: Vec<InlineBox>,
}

#[derive(Clone, Copy)]
struct Segment {
    glyphs: (usize, usize),
    font: FontId,
    emoji: bool,
    rtl: bool,
    start: u32,
}

/// Builds a [`Paragraph`]. Add runs in reading order, then [`build`](Self::build).
pub struct ParagraphBuilder<'a> {
    engine: &'a Engine,
    base: TextStyle,
    wrapping: Wrapping,
    scale: f32,
    scratch: Scratch,
    inline: u32,
    /// Collapsing white space: the last character added was a space (or nothing was added).
    after_space: bool,
}

impl<'a> ParagraphBuilder<'a> {
    /// `base` is the paragraph's own style (the CSS block's font, size and line height): it
    /// sizes every line even where no text uses it, and draws the "…" of a cut line.
    /// `scale` is the display's device pixels per px: Chromium snaps font sizes and every
    /// length to 1/64 of a device pixel, so a paragraph is shaped for one scale factor.
    pub fn new(engine: &'a Engine, base: &TextStyle, wrapping: Wrapping, scale: f32) -> Self {
        let mut scratch = std::mem::take(&mut *engine.scratch.borrow_mut());
        scratch.text.clear();
        scratch.styles.clear();
        // Index 0 is the base style: boxes and the ellipsis use it.
        scratch.styles.push(*base);
        scratch.items.clear();
        scratch.units.clear();
        scratch.glyphs.clear();
        scratch.boxes.clear();
        Self {
            engine,
            base: *base,
            wrapping,
            scale,
            scratch,
            inline: 0,
            after_space: true,
        }
    }

    /// A run of text. `tag` comes back from hit-testing ([`Paragraph::locate`]): use it to
    /// name the run (a span index, a link id).
    pub fn text(&mut self, text: &str, style: &TextStyle, tag: u32) -> &mut Self {
        if text.is_empty() {
            return self;
        }
        let text = self.collapse(text);
        if text.is_empty() {
            return self;
        }
        let style_index = self.style_index(style);
        let inline = self.inline;
        self.inline += 1;
        let first_item = self.scratch.items.len();
        let mut local = 0u32;
        for piece in text.split_inclusive('\n') {
            let (piece, newline) = match piece.strip_suffix('\n') {
                Some(rest) if !self.wrapping.collapses() => (rest, true),
                _ => (piece, false),
            };
            if !piece.is_empty() {
                self.shape_piece(piece, style, style_index, inline, tag, local);
                local += piece.len() as u32;
            }
            if newline {
                let at = self.scratch.text.len() as u32;
                self.scratch.text.push('\n');
                let unit = self.scratch.units.len() as u32;
                self.scratch.units.push(Unit {
                    text: at,
                    advance: 0.,
                    glyph: self.scratch.glyphs.len() as u32,
                    flags: flag::NEWLINE,
                });
                self.scratch.items.push(Item {
                    text: (at, at + 1),
                    units: (unit, unit + 1),
                    glyphs: (self.scratch.glyphs.len() as u32, self.scratch.glyphs.len() as u32),
                    font: style.face.id,
                    style: style_index,
                    flags: flag::IS_BREAK,
                    inline,
                    tag,
                    local,
                });
                local += 1;
            }
        }
        let items = &mut self.scratch.items[first_item..];
        if let Some(first) = items.first_mut() {
            first.flags |= flag::FIRST;
        }
        if let Some(last) = items.last_mut() {
            last.flags |= flag::LAST;
        }
        self
    }

    /// A box that flows with the text (an emoji image, an inline formula): `width` ×
    /// `height` px, its bottom `below_baseline` px under the baseline.
    pub fn inline_box(&mut self, width: f32, height: f32, below_baseline: f32, tag: u32) -> &mut Self {
        let at = self.scratch.text.len() as u32;
        self.scratch.text.push_str(OBJECT_REPLACEMENT);
        let unit = self.scratch.units.len() as u32;
        self.scratch.units.push(Unit {
            text: at,
            advance: width,
            glyph: self.scratch.glyphs.len() as u32,
            flags: flag::BOX,
        });
        self.scratch.boxes.push(InlineBox {
            width,
            height,
            below: below_baseline,
            tag,
            unit,
        });
        let glyphs = self.scratch.glyphs.len() as u32;
        self.scratch.items.push(Item {
            text: (at, at + OBJECT_REPLACEMENT.len() as u32),
            units: (unit, unit + 1),
            glyphs: (glyphs, glyphs),
            font: self.base.face.id,
            style: 0,
            flags: flag::IS_BOX | flag::FIRST | flag::LAST,
            inline: self.inline,
            tag,
            local: 0,
        });
        self.inline += 1;
        self.after_space = false;
        self
    }

    pub fn build(mut self) -> Paragraph {
        self.engine.count(|stats| stats.paragraphs += 1);
        if self.wrapping.collapses() {
            self.trim_trailing_space();
        }
        self.mark_breaks();
        let ellipsis = (self.wrapping.ellipsis || self.wrapping.max_lines > 0)
            .then(|| self.shape_mark("…"))
            .flatten();
        // Blink's hyphen string: U+2010 when the font has it, else the hyphen-minus.
        let hyphen = (self.wrapping.wraps() && self.scratch.text.contains('\u{ad}'))
            .then(|| self.shape_mark("\u{2010}").filter(|mark| mark.glyph != 0).or_else(|| self.shape_mark("-")))
            .flatten();
        let scratch = &self.scratch;
        let paragraph = Paragraph {
            text: scratch.text.as_str().into(),
            base: self.base,
            wrapping: self.wrapping,
            scale: self.scale,
            styles: scratch.styles.as_slice().into(),
            items: scratch.items.as_slice().into(),
            units: scratch.units.as_slice().into(),
            glyphs: scratch.glyphs.as_slice().into(),
            boxes: scratch.boxes.as_slice().into(),
            ellipsis,
            hyphen,
        };
        *self.engine.scratch.borrow_mut() = std::mem::take(&mut self.scratch);
        paragraph
    }

    fn style_index(&mut self, style: &TextStyle) -> u16 {
        match self.scratch.styles.iter().position(|known| known == style) {
            Some(index) => index as u16,
            None => {
                self.scratch.styles.push(*style);
                (self.scratch.styles.len() - 1) as u16
            }
        }
    }

    /// CSS white-space collapsing for `normal` / `nowrap`: every run of spaces, tabs and
    /// newlines becomes one space, none at the paragraph's start.
    fn collapse<'t>(&mut self, text: &'t str) -> Cow<'t, str> {
        if !self.wrapping.collapses() {
            return Cow::Borrowed(text);
        }
        let is_space = |c: char| matches!(c, ' ' | '\t' | '\n' | '\r');
        let mut previous_space = self.after_space;
        let unchanged = text.chars().all(|c| {
            let space = is_space(c);
            let keep = !(space && (previous_space || c != ' '));
            previous_space = space;
            keep
        });
        if unchanged {
            self.after_space = previous_space;
            return Cow::Borrowed(text);
        }
        let mut out = String::with_capacity(text.len());
        let mut previous_space = self.after_space;
        for c in text.chars() {
            if is_space(c) {
                if !previous_space {
                    out.push(' ');
                }
                previous_space = true;
            } else {
                out.push(c);
                previous_space = false;
            }
        }
        self.after_space = previous_space;
        Cow::Owned(out)
    }

    fn trim_trailing_space(&mut self) {
        let scratch = &mut self.scratch;
        let Some(last) = scratch.units.last() else { return };
        if last.flags & flag::SPACE == 0 || scratch.units.len() < 2 {
            return;
        }
        // The space stays in the text (offsets hold); it just takes no room.
        scratch.units.last_mut().unwrap().advance = 0.;
    }

    fn shape_piece(&mut self, piece: &str, style: &TextStyle, style_index: u16, inline: u32, tag: u32, local: u32) {
        let engine = self.engine;
        let scratch = &mut self.scratch;
        scratch.raw.clear();
        let font = engine.shaping_font(style.face, style.letter_spacing);
        let width = engine.shape(piece, font, shaping_size(style.size, self.scale), &mut scratch.raw);
        let base = scratch.text.len() as u32;
        scratch.text.push_str(piece);

        // Cut the glyphs where the font or the direction changes.
        scratch.segments.clear();
        let raw = &scratch.raw;
        let mut at = 0;
        while at < raw.len() {
            let font = raw[at].font;
            let mut end = at + 1;
            let mut direction = 0i8;
            while end < raw.len() && raw[end].font == font {
                let step = (raw[end].index as i64 - raw[end - 1].index as i64).signum() as i8;
                if step != 0 {
                    if direction != 0 && step != direction {
                        break;
                    }
                    direction = step;
                }
                end += 1;
            }
            let start = raw[at..end].iter().map(|glyph| glyph.index).min().unwrap_or(0);
            scratch.segments.push(Segment {
                glyphs: (at, end),
                font,
                emoji: raw[at].emoji,
                rtl: direction < 0,
                start,
            });
            at = end;
        }
        if scratch.segments.is_empty() {
            // Nothing to draw (only characters shapers drop): keep the text addressable.
            scratch.segments.push(Segment {
                glyphs: (0, 0),
                font: style.face.id,
                emoji: false,
                rtl: false,
                start: 0,
            });
        }
        scratch.segments.sort_by_key(|segment| segment.start);
        scratch.segments[0].start = 0;

        let letter_spacing = style.letter_spacing;
        for index in 0..scratch.segments.len() {
            let segment = scratch.segments[index];
            let text_end = scratch
                .segments
                .get(index + 1)
                .map_or(piece.len() as u32, |next| next.start);
            let text = &piece[segment.start as usize..text_end as usize];
            let raw = &scratch.raw[segment.glyphs.0..segment.glyphs.1];
            let first_unit = scratch.units.len();
            let first_glyph = scratch.glyphs.len() as u32;

            // Units, walking the glyphs in text order.
            scratch.unit_of_glyph.clear();
            scratch.unit_of_glyph.resize(raw.len(), 0);
            let mut graphemes = text.grapheme_indices(true).map(|(offset, _)| offset as u32).peekable();
            let mut grapheme = graphemes.next().unwrap_or(0);
            for step in 0..raw.len() {
                let position = if segment.rtl { raw.len() - 1 - step } else { step };
                let glyph = &raw[position];
                let index_in_text = glyph.index.saturating_sub(segment.start);
                while graphemes.peek().is_some_and(|next| *next <= index_in_text) {
                    grapheme = graphemes.next().unwrap();
                }
                let next_x = match scratch.raw.get(segment.glyphs.0 + position + 1) {
                    Some(next) => next.x,
                    None => width,
                };
                let advance = next_x - glyph.x;
                let unit_text = base + segment.start + grapheme;
                let unit_index = match scratch.units[first_unit..].last_mut() {
                    Some(unit) if unit.text == unit_text => {
                        unit.advance += advance;
                        unit.glyph = unit.glyph.min(first_glyph + position as u32);
                        scratch.units.len() - 1
                    }
                    _ => {
                        let character = text[grapheme as usize..].chars().next();
                        scratch.units.push(Unit {
                            text: unit_text,
                            advance,
                            glyph: first_glyph + position as u32,
                            flags: match character {
                                Some(' ') => flag::SPACE,
                                Some('\t') => flag::TAB,
                                Some('\u{ad}') => flag::SOFT_HYPHEN,
                                _ => 0,
                            },
                        });
                        scratch.units.len() - 1
                    }
                };
                scratch.unit_of_glyph[position] = (unit_index - first_unit) as u32;
            }
            if scratch.units.len() == first_unit {
                scratch.units.push(Unit {
                    text: base + segment.start,
                    advance: 0.,
                    glyph: first_glyph,
                    flags: 0,
                });
            }
            // A tab's advance is decided on the line; until then it holds a space's (shapers
            // differ in what they return for a tab).
            for unit in &mut scratch.units[first_unit..] {
                if unit.flags & flag::TAB != 0 {
                    unit.advance = engine.space_advance(segment.font, shaping_size(style.size, self.scale));
                }
            }
            // Characters before the first glyph's cluster belong to the first unit.
            scratch.units[first_unit].text = base + segment.start;
            let unit_count = scratch.units.len() - first_unit;
            // Letter-spacing goes after each unit: not after a tab, nor after a character
            // that has no width of its own (Blink's `TreatAsZeroWidthSpace`).
            // `spacing[u]` is what the units before `u` (in text order) received.
            scratch.spacing.clear();
            scratch.spacing.push(0.);
            for unit in &mut scratch.units[first_unit..] {
                let character = scratch.text[unit.text as usize..].chars().next();
                let spaced = letter_spacing != 0. && unit.flags & flag::TAB == 0 && !character.is_some_and(is_zero_width);
                let added = if spaced { letter_spacing } else { 0. };
                unit.advance += added;
                scratch.spacing.push(scratch.spacing.last().copied().unwrap_or(0.) + added);
            }
            let total_spacing = scratch.spacing[unit_count];

            // Glyphs, in visual order, from the segment's left edge.
            let origin = raw.first().map_or(0., |glyph| glyph.x);
            for (position, glyph) in raw.iter().enumerate() {
                let unit = scratch.unit_of_glyph[position] as usize;
                // The spacing of every unit to the glyph's left.
                let spacing_before = if segment.rtl {
                    total_spacing - scratch.spacing[unit + 1]
                } else {
                    scratch.spacing[unit]
                };
                scratch.glyphs.push(Glyph {
                    x: glyph.x - origin + spacing_before,
                    cluster: scratch.units[first_unit + unit].text,
                    id: glyph.id as u16,
                    y: (glyph.y * 64.).round().clamp(i16::MIN as f32, i16::MAX as f32) as i16,
                });
            }
            scratch.items.push(Item {
                text: (base + segment.start, base + text_end),
                units: (first_unit as u32, scratch.units.len() as u32),
                glyphs: (first_glyph, scratch.glyphs.len() as u32),
                font: segment.font,
                style: style_index,
                flags: if segment.rtl { flag::RTL } else { 0 }
                    | if segment.emoji { flag::EMOJI_FONT } else { 0 },
                inline,
                tag,
                local: local + segment.start,
            });
        }
    }

    /// Marks the units a line may break before: UAX #14 over the whole paragraph (so a box
    /// or a style change is no opportunity of its own), then Blink's own answers where it
    /// does not follow UAX #14 (`blink_breaks.rs`).
    fn mark_breaks(&mut self) {
        if !self.wrapping.wraps() {
            return;
        }
        let scratch = &mut self.scratch;
        let break_all = self.wrapping.word_break == WordBreak::BreakAll;
        let segmenter = self.engine.segmenter(self.wrapping.word_break);
        let mut unit = 0;
        for position in segmenter.segment_str(&scratch.text) {
            let position = position as u32;
            while unit < scratch.units.len() && scratch.units[unit].text < position {
                unit += 1;
            }
            if unit >= scratch.units.len() {
                break;
            }
            if scratch.units[unit].text == position && unit > 0 {
                scratch.units[unit].flags |= flag::BREAK_BEFORE;
            }
        }

        let text = scratch.text.as_str();
        let table = if break_all { &crate::blink_breaks::BREAK_ALL } else { &crate::blink_breaks::NORMAL };
        let printable = |c: char| ('!'..='~').contains(&c);
        for index in 1..scratch.units.len() {
            let at = scratch.units[index].text as usize;
            let mut before = text[..at].chars().rev();
            let (Some(previous), Some(next)) = (before.next(), text[at..].chars().next()) else {
                continue;
            };
            let breaks = if printable(previous) && printable(next) {
                // "-5": no break between a minus sign and its number, but "ABCD-1234" breaks.
                if previous == '-' && next.is_ascii_digit() && !break_all {
                    before.next().is_some_and(|c| c.is_ascii_alphanumeric())
                } else {
                    table[previous as usize - '!' as usize] >> (next as u32 - '!' as u32) & 1 == 1
                }
            } else if break_all && (previous as u32 > 0x7f || next as u32 > 0x7f) {
                // Blink's break-all table by line-break class, for the pairs UAX #14's own
                // break-all (letters and digits only) leaves out.
                scratch.units[index].flags & flag::BREAK_BEFORE != 0
                    || (break_all_before(previous) && break_all_after(next))
            } else {
                continue;
            };
            let flags = &mut scratch.units[index].flags;
            *flags = if breaks { *flags | flag::BREAK_BEFORE } else { *flags & !flag::BREAK_BEFORE };
        }
        for unit in scratch.units.iter_mut() {
            if unit.flags & flag::SOFT_HYPHEN != 0 {
                unit.flags &= !flag::BREAK_BEFORE;
            }
        }
    }

    fn shape_mark(&mut self, text: &str) -> Option<Mark> {
        let scratch = &mut self.scratch;
        scratch.raw.clear();
        let font = self.engine.shaping_font(self.base.face, self.base.letter_spacing);
        let width = self.engine.shape(text, font, shaping_size(self.base.size, self.scale), &mut scratch.raw);
        let glyph = scratch.raw.first()?;
        Some(Mark {
            font: glyph.font,
            glyph: glyph.id as u16,
            advance: width + self.base.letter_spacing,
        })
    }
}

/// `kBreakAllLineBreakClassTable`: with `break-all` a line also breaks between a character
/// of one of these classes …
fn break_all_before(c: char) -> bool {
    use unicode_linebreak::BreakClass::*;
    matches!(
        unicode_linebreak::break_property(c as u32),
        Ambiguous | Alphabetic | After | Hyphen | Numeric | ComplexContext | HebrewLetter
    )
}

/// … and one of these.
fn break_all_after(c: char) -> bool {
    use unicode_linebreak::BreakClass::*;
    matches!(
        unicode_linebreak::break_property(c as u32),
        Ambiguous | Alphabetic | After | Hyphen | Numeric | OpenPunctuation | Prefix | ComplexContext | HebrewLetter
    )
}

fn is_zero_width(c: char) -> bool {
    matches!(c, '\u{ad}' | '\u{200b}'..='\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2060}' | '\u{feff}' | '\u{fffc}')
}

/// The size text is shaped and drawn at: Blink truncates a font size to 1/64 of a device
/// pixel (12.325 px at scale 1 is 12.3125 px).
pub(crate) fn shaping_size(size: f32, scale: f32) -> f32 {
    (size * scale * 64.).floor() / 64. / scale
}
