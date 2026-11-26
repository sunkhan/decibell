//! Line breaking and line boxes, the way Chromium's LayoutNG does them.
//!
//! A [`Layout`] is a [`Paragraph`] wrapped at one width for one scale factor: lines,
//! fragments (a stretch of one item on one line), decoration rectangles, and the total size.
//! It holds no glyphs of its own, so wrapping the same paragraph at another width costs a
//! pass over its units and a few small allocations.
//!
//! What is reproduced from Chromium, because row heights and line breaks depend on it
//! (all checked against the fidelity corpus, `tests/fidelity.rs`):
//!
//! - Arithmetic happens in device pixels on Blink's `LayoutUnit` grid (1/64 px): a line
//!   height is truncated to it, the width of each fragment is rounded up to it.
//! - A line box is the paragraph's strut plus every inline box on the line: ascent and
//!   descent are the font's, rounded to whole device pixels, with the half-leading above
//!   the text rounded down to a whole pixel. A font used only as a fallback changes nothing.
//! - Break opportunities come from UAX #14 over the whole paragraph (boxes count as U+FFFC),
//!   with `word-break: break-all` when asked; `overflow-wrap: anywhere` then breaks between
//!   any two clusters. Spaces and tabs at the end of a line hang past the edge.
//! - A line that ends inside a shaped run is as wide as Chromium's re-shaped line: the last
//!   glyph loses the kerning it had against the glyph that moved to the next line.
//! - `box-decoration-break: clone` chips have both paddings on every line they cover.

use std::rc::Rc;

use gpui::Hsla;

use crate::paragraph::{Item, Paragraph, flag};
use crate::{Engine, LineHeight, OverflowWrap, TextStyle};

/// A fragment that is the "…" of a cut line.
pub(crate) const ELLIPSIS_ITEM: u32 = u32::MAX;
/// A fragment that is the hyphen of a line ending at a soft hyphen.
pub(crate) const HYPHEN_ITEM: u32 = u32::MAX - 1;
/// Fragments from here up are marks, not text.
pub(crate) const FIRST_MARK_ITEM: u32 = HYPHEN_ITEM;

#[derive(Clone, Copy, Debug)]
pub(crate) struct Line {
    /// px from the layout's top.
    pub top: f32,
    pub height: f32,
    /// px from the layout's top.
    pub baseline: f32,
    /// Width without the spaces that hang at the end.
    pub width: f32,
    pub units: (u32, u32),
    pub frags: (u32, u32),
    /// The line ends with a `\n` (its last unit).
    pub hard: bool,
}

/// A stretch of one item on one line.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Frag {
    pub item: u32,
    pub units: (u32, u32),
    pub glyphs: (u32, u32),
    /// Left edge on the line, px.
    pub x: f32,
    pub width: f32,
    /// Add to a glyph's `x` for its position on the line.
    pub dx: f32,
    pub line: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DecoKind {
    Chip,
    Underline,
    Strike,
}

/// A rectangle to fill: a chip's background, an underline, a strike-through.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Deco {
    pub kind: DecoKind,
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
    pub color: Hsla,
    pub radius: f32,
    /// The text the rectangle belongs to, for recolouring under a selection.
    pub text: (u32, u32),
    pub line: u32,
}

/// A paragraph wrapped at a width. Immutable; share it with `Rc`.
#[derive(Debug)]
pub struct Layout {
    pub(crate) paragraph: Rc<Paragraph>,
    pub(crate) lines: Box<[Line]>,
    pub(crate) frags: Box<[Frag]>,
    pub(crate) decos: Box<[Deco]>,
    pub(crate) scale: f32,
    width: f32,
    height: f32,
    truncated: bool,
}

impl Layout {
    /// Wraps `paragraph` at `width` px (`f32::INFINITY` for no limit), for the scale factor
    /// the paragraph was shaped for.
    pub fn new(paragraph: Rc<Paragraph>, width: f32, engine: &Engine) -> Layout {
        engine.count(|stats| stats.layouts += 1);
        Breaker::new(&paragraph, width, paragraph.scale, engine).run(paragraph.clone())
    }

    /// For the fidelity tools: the width, in device px, a line holding the text
    /// `from..to` would have: (unsnapped with shaped advances, as laid out).
    #[doc(hidden)]
    pub fn debug_line_width(paragraph: &Rc<Paragraph>, from: u32, to: u32, engine: &Engine) -> (f32, f32) {
        let scale = paragraph.scale;
        let breaker = Breaker::new(paragraph, f32::INFINITY, scale, engine);
        let start = paragraph.units.partition_point(|unit| unit.text < from);
        let end = paragraph.units.partition_point(|unit| unit.text < to);
        let mut last = end;
        while last > start && paragraph.units[last - 1].flags & (flag::SPACE | flag::TAB | flag::NEWLINE) != 0 {
            last -= 1;
        }
        let shaped: f32 = paragraph.units[start..last].iter().map(|unit| unit.advance * scale).sum();
        (shaped, breaker.walk(start, end, View::Drawn, None).fit)
    }

    pub fn paragraph(&self) -> &Rc<Paragraph> {
        &self.paragraph
    }

    /// The widest line, px, without hanging spaces. Rounded up to Chromium's 1/64 px grid.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// The sum of the line boxes, px: exactly what the same content is high in Chromium
    /// (fractional when the CSS line height is).
    pub fn height(&self) -> f32 {
        self.height
    }

    pub fn line_count(&self) -> usize {
        self.lines.len()
    }

    /// The first line's baseline, px from the top (0 for an empty paragraph).
    pub fn first_baseline(&self) -> f32 {
        self.lines.first().map_or(0., |line| line.baseline)
    }

    /// The last line's baseline, px from the top.
    pub fn last_baseline(&self) -> f32 {
        self.lines.last().map_or(0., |line| line.baseline)
    }

    /// Text was cut (line clamp or ellipsis): show the full text in a tooltip.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The scale factor the layout was made for.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// Heap bytes this layout holds, without its paragraph.
    pub fn heap_bytes(&self) -> usize {
        size_of_val(&*self.lines) + size_of_val(&*self.frags) + size_of_val(&*self.decos)
    }

    /// Where line `index` starts in the text, its top and height, and its width.
    pub fn line(&self, index: usize) -> Option<LineInfo> {
        let line = self.lines.get(index)?;
        Some(LineInfo {
            text: self.line_text(line),
            top: line.top,
            height: line.height,
            baseline: line.baseline,
            width: line.width,
        })
    }

    pub(crate) fn line_text(&self, line: &Line) -> (u32, u32) {
        let paragraph = &self.paragraph;
        if line.units.0 >= line.units.1 {
            let at = paragraph
                .units
                .get(line.units.0 as usize)
                .map_or(paragraph.text.len() as u32, |unit| unit.text);
            return (at, at);
        }
        (
            paragraph.units[line.units.0 as usize].text,
            paragraph.unit_end(line.units.1 as usize - 1),
        )
    }
}

/// A line of a [`Layout`], for callers that walk lines (tests, the fidelity harness).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct LineInfo {
    /// Byte range in the paragraph's text, the line's `\n` included.
    pub text: (u32, u32),
    pub top: f32,
    pub height: f32,
    pub baseline: f32,
    pub width: f32,
}

/// Blink's `LayoutUnit`: 1/64 px.
fn lu_floor(value: f32) -> f32 {
    (value * 64.).floor() / 64.
}

fn lu_ceil(value: f32) -> f32 {
    (value * 64.).ceil() / 64.
}

/// A style's share of a line box, device px.
#[derive(Clone, Copy)]
struct StyleMetrics {
    /// Line-box extent above / below the baseline (font + half-leading).
    above: f32,
    below: f32,
    /// The font's own rounded ascent / descent.
    ascent: f32,
    descent: f32,
}

fn style_metrics(style: &TextStyle, scale: f32) -> StyleMetrics {
    let (ascent, descent) = style.face.line_metrics(style.size, scale);
    // Blink's `ComputedLineHeight`: a factor multiplies the font size snapped down to a
    // LayoutUnit, and the product is snapped down again.
    let line_height = match style.line_height {
        LineHeight::Factor(factor) => lu_floor(lu_floor(style.size * scale) * factor),
        LineHeight::Px(px) => lu_floor(px * scale),
    };
    let leading_above = ((line_height - (ascent + descent)) / 2.).floor();
    let above = ascent + leading_above;
    StyleMetrics {
        above,
        below: line_height - above,
        ascent,
        descent,
    }
}

/// Two views of the same line. Blink's line breaker counts a chip's closing padding only
/// where its run really ends; the fragments it then builds get the cloned padding at the
/// end of every line (`box-decoration-break: clone`), so a chip that is cut by a line end
/// pokes out of the box by its padding.
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Breaker,
    Drawn,
}

/// What walking a line's units found.
#[derive(Clone, Copy, Default)]
struct Walked {
    /// Position after the last unit that is not a hanging space, closing padding included.
    fit: f32,
    /// Position after everything.
    end: f32,
    above: f32,
    below: f32,
}

struct Breaker<'a> {
    paragraph: &'a Paragraph,
    engine: &'a Engine,
    scale: f32,
    /// Device px.
    available: f32,
    metrics: Vec<StyleMetrics>,
    strut: StyleMetrics,
}

/// What a layout pass produces.
#[derive(Default)]
struct Output {
    lines: Vec<Line>,
    frags: Vec<Frag>,
    decos: Vec<Deco>,
}

impl<'a> Breaker<'a> {
    fn new(paragraph: &'a Paragraph, width: f32, scale: f32, engine: &'a Engine) -> Self {
        Self {
            paragraph,
            engine,
            scale,
            available: if width.is_finite() { lu_floor(width * scale).max(0.) } else { f32::INFINITY },
            metrics: paragraph.styles.iter().map(|style| style_metrics(style, scale)).collect(),
            strut: style_metrics(&paragraph.base, scale),
        }
    }

    fn run(self, paragraph: Rc<Paragraph>) -> Layout {
        let mut out = Output::default();
        let unit_count = self.paragraph.units.len();
        let wrapping = self.paragraph.wrapping;
        let max_lines = if wrapping.max_lines == 0 { usize::MAX } else { wrapping.max_lines as usize };
        let mut start = 0;
        let mut top = 0.;
        let mut width: f32 = 0.;
        let mut truncated = false;
        while start < unit_count {
            let (mut end, hard) = self.next_break(start);
            let mut ellipsis = false;
            let clamped = out.lines.len() + 1 == max_lines && end < unit_count;
            if (clamped || (wrapping.ellipsis && !wrapping.wraps()))
                && let Some(mark) = self.paragraph.ellipsis
            {
                let room = self.available - lu_ceil(mark.advance * self.scale);
                let content_end = if hard { end - 1 } else { end };
                let overflows = self.walk(start, content_end, View::Drawn, None).fit > self.available;
                if clamped || overflows {
                    let mut cut = content_end;
                    while cut > start && self.walk(start, cut, View::Drawn, None).end > room {
                        cut -= 1;
                    }
                    end = cut;
                    ellipsis = true;
                    truncated = true;
                }
            }
            let line_index = out.lines.len() as u32;
            let first_frag = out.frags.len() as u32;
            let first_deco = out.decos.len();
            let mut walked = self.walk(start, end, View::Drawn, Some((&mut out, line_index)));
            if ellipsis {
                let mark = self.paragraph.ellipsis.unwrap();
                let advance = lu_ceil(mark.advance * self.scale);
                out.frags.push(Frag {
                    item: ELLIPSIS_ITEM,
                    units: (end as u32, end as u32),
                    glyphs: (0, 0),
                    x: walked.end / self.scale,
                    width: advance / self.scale,
                    dx: walked.end / self.scale,
                    line: line_index,
                });
                walked.fit = walked.end + advance;
            }
            let above = walked.above.max(self.strut.above);
            let below = walked.below.max(self.strut.below);
            let baseline = top + above;
            // Rectangles were emitted in device px relative to the baseline.
            for deco in &mut out.decos[first_deco..] {
                deco.y = (baseline + deco.y) / self.scale;
                deco.height /= self.scale;
                deco.line = line_index;
            }
            out.lines.push(Line {
                top: top / self.scale,
                height: (above + below) / self.scale,
                baseline: baseline / self.scale,
                width: walked.fit / self.scale,
                units: (start as u32, end as u32),
                frags: (first_frag, out.frags.len() as u32),
                hard: hard && !ellipsis,
            });
            width = width.max(walked.fit);
            top += above + below;
            if ellipsis && clamped {
                break;
            }
            if ellipsis {
                // A cut single line: skip to the next hard break.
                let mut next = end;
                while next < unit_count && self.paragraph.units[next].flags & flag::NEWLINE == 0 {
                    next += 1;
                }
                start = (next + 1).min(unit_count);
                continue;
            }
            start = end;
        }
        Layout {
            paragraph,
            lines: out.lines.into(),
            frags: out.frags.into(),
            decos: out.decos.into(),
            scale: self.scale,
            width: width / self.scale,
            height: top / self.scale,
            truncated,
        }
    }

    /// Where the line that starts at unit `start` ends: the unit the next line starts at,
    /// and whether a `\n` ended it.
    fn next_break(&self, start: usize) -> (usize, bool) {
        let paragraph = self.paragraph;
        let units = &paragraph.units;
        let wraps = paragraph.wrapping.wraps() && self.available.is_finite();
        let anywhere = paragraph.wrapping.overflow_wrap == OverflowWrap::Anywhere;

        // The last few places the line could end.
        let mut candidates = [0usize; 4];
        let mut candidate_count = 0usize;
        // An over-long unbreakable stretch is on the line: end at the next opportunity.
        let mut overflowing = false;

        let mut item_index = paragraph.item_of_unit(start as u32);
        let mut position = self.open_item(item_index, start, 0.);
        let mut fragment = 0.0f32;
        let mut unit_index = start;
        loop {
            let Some(item) = paragraph.items.get(item_index) else {
                return (units.len(), false);
            };
            if unit_index >= item.units.1 as usize {
                position = lu_ceil(position + fragment);
                if item.flags & flag::LAST != 0 {
                    position += self.chip_pad(item);
                }
                item_index += 1;
                position = self.open_item(item_index, unit_index, position);
                fragment = 0.;
                continue;
            }
            let unit = &units[unit_index];
            if unit.flags & flag::NEWLINE != 0 {
                return (unit_index + 1, true);
            }
            let can_break = unit_index > start && unit.flags & flag::BREAK_BEFORE != 0;
            if wraps && can_break {
                if overflowing {
                    return (unit_index, false);
                }
                candidates[candidate_count % candidates.len()] = unit_index;
                candidate_count += 1;
            }
            if unit.flags & (flag::SPACE | flag::TAB) != 0 {
                fragment += self.advance(unit, position + fragment);
                unit_index += 1;
                continue;
            }
            let advance = unit.advance * self.scale;
            if wraps && !overflowing {
                // What fits, measured on the corpus: a line may pass the box's edge by
                // one LayoutUnit, both as shaped and after its end is re-shaped (Blink
                // compares positions that were snapped up to the unit).
                let slack = self.available + 1. / 64.;
                let mut end = lu_ceil(position + fragment + advance);
                if unit_index + 1 == item.units.1 as usize && item.flags & flag::LAST != 0 {
                    end += self.chip_pad(item);
                }
                if end > slack {
                    let stored = candidate_count.min(candidates.len());
                    for back in 1..=stored {
                        let candidate = candidates[(candidate_count - back) % candidates.len()];
                        if back == stored || self.walk(start, candidate, View::Breaker, None).fit <= slack {
                            return (candidate, false);
                        }
                    }
                    if anywhere {
                        if unit_index > start {
                            let mut cut = unit_index;
                            while cut > start + 1 && self.walk(start, cut, View::Breaker, None).fit > slack {
                                cut -= 1;
                            }
                            return (cut, false);
                        }
                        // One cluster wider than the line: it goes alone, with the spaces
                        // after it.
                        unit_index += 1;
                        while unit_index < units.len()
                            && units[unit_index].flags & (flag::SPACE | flag::TAB) != 0
                            && unit_index < paragraph.items[item_index].units.1 as usize
                        {
                            unit_index += 1;
                        }
                        if unit_index < units.len() && units[unit_index].flags & flag::NEWLINE != 0 {
                            return (unit_index + 1, true);
                        }
                        return (unit_index, false);
                    }
                    overflowing = true;
                }
            }
            fragment += advance;
            unit_index += 1;
        }
    }

    /// Position after the opening inset of `item` when the line reaches it at `unit`.
    fn open_item(&self, item_index: usize, unit: usize, position: f32) -> f32 {
        let Some(item) = self.paragraph.items.get(item_index) else {
            return position;
        };
        if item.flags & (flag::IS_BOX | flag::IS_BREAK) != 0 {
            return position;
        }
        let style = &self.paragraph.styles[item.style as usize];
        let begins_run = item.flags & flag::FIRST != 0 && unit == item.units.0 as usize;
        let mut position = position;
        if begins_run {
            position += lu_floor(style.margin_start * self.scale);
        }
        // The chip's opening padding: where the run begins and, cloned, wherever it
        // continues from the line before (the line breaker counts both).
        let continues = unit != item.units.0 as usize || position == 0.;
        if begins_run || continues {
            position += self.chip_pad(item);
        }
        position
    }

    /// The horizontal padding of the chip `item` is in, device px (0 without one).
    fn chip_pad(&self, item: &Item) -> f32 {
        if item.flags & (flag::IS_BOX | flag::IS_BREAK) != 0 {
            return 0.;
        }
        match &self.paragraph.styles[item.style as usize].chip {
            Some(chip) => lu_floor(chip.pad_x * self.scale),
            None => 0.,
        }
    }

    /// A unit's advance in device px when it starts at `position` on the line.
    fn advance(&self, unit: &crate::paragraph::Unit, position: f32) -> f32 {
        if unit.flags & flag::TAB == 0 {
            return unit.advance * self.scale;
        }
        // Blink's `Font::TabWidth`: to the next multiple of tab-size spaces, and a stop
        // closer than half a space is skipped.
        let space = unit.advance * self.scale;
        let stop = space * self.paragraph.wrapping.tab_size as f32;
        if stop <= 0. {
            return 0.;
        }
        let mut distance = stop - position % stop;
        if distance < space / 2. {
            distance += stop;
        }
        distance
    }

    /// Walks units `start..end` as one line. With a sink (the line's index) it emits the
    /// fragments and decoration rectangles; without, it only measures.
    fn walk(&self, start: usize, end: usize, view: View, mut sink: Option<(&mut Output, u32)>) -> Walked {
        let paragraph = self.paragraph;
        let units = &paragraph.units;
        let scale = self.scale;
        let mut walked = Walked::default();
        if start >= end {
            return walked;
        }
        let mut item_index = paragraph.item_of_unit(start as u32);
        let mut position = self.open_item(item_index, start, 0.);
        // A chip being drawn: (inline, left edge, style).
        let mut chip: Option<(u32, f32, u16)> = None;
        let mut unit_index = start;
        while unit_index < end {
            let item = &paragraph.items[item_index];
            let item_end = (item.units.1 as usize).min(end);
            if item.flags & flag::IS_BREAK != 0 {
                unit_index = item_end;
                item_index += 1;
                if unit_index < end {
                    position = self.open_item(item_index, unit_index, position);
                }
                continue;
            }
            if item.flags & flag::IS_BOX != 0 {
                let inline_box = paragraph.boxes.iter().find(|b| b.unit == unit_index as u32);
                let (width, above, below) = match inline_box {
                    Some(b) => {
                        let below = lu_floor(b.below * scale);
                        (lu_floor(b.width * scale), lu_floor(b.height * scale) - below, below)
                    }
                    None => (0., 0., 0.),
                };
                if let Some((out, line)) = sink.as_mut() {
                    let line = *line;
                    out.frags.push(Frag {
                        item: item_index as u32,
                        units: (unit_index as u32, unit_index as u32 + 1),
                        glyphs: (0, 0),
                        x: position / scale,
                        width: width / scale,
                        dx: position / scale,
                        line,
                    });
                }
                position += width;
                walked.fit = position;
                walked.above = walked.above.max(above);
                walked.below = walked.below.max(below);
                unit_index = item_end;
                item_index += 1;
                if unit_index < end {
                    position = self.open_item(item_index, unit_index, position);
                }
                continue;
            }

            let style_index = item.style;
            let style = &paragraph.styles[style_index as usize];
            let metrics = self.metrics[style_index as usize];
            walked.above = walked.above.max(metrics.above);
            walked.below = walked.below.max(metrics.below);
            if sink.is_some() && style.chip.is_some() && chip.is_none_or(|open| open.0 != item.inline) {
                chip = Some((item.inline, position - self.chip_pad(item), style_index));
            }

            // The item's units on this line, cut into fragments at tabs.
            let fragment_origin = position;
            let mut fragment = 0.0f32;
            let mut fit_in_fragment: Option<f32> = None;
            let mut cursor = unit_index;
            while cursor < item_end {
                let unit = &units[cursor];
                if unit.flags & flag::TAB != 0 {
                    let tab = self.advance(unit, fragment_origin + fragment);
                    fragment += tab;
                } else {
                    let mut advance = unit.advance * scale;
                    // The line ends inside the run after this unit: Chromium re-shapes the
                    // line, and the last glyph loses its kerning against what moved on.
                    if cursor + 1 == end && end < item.units.1 as usize && unit.flags & flag::SPACE == 0 {
                        advance = self.reshaped_advance(item, cursor, advance);
                    }
                    fragment += advance;
                    if unit.flags & flag::SPACE == 0 {
                        fit_in_fragment = Some(fragment);
                    }
                }
                cursor += 1;
            }
            let fragment_start = unit_index;
            let fragment_width = lu_ceil(fragment_origin + fragment) - fragment_origin;
            if let Some((out, line)) = sink.as_mut() {
                self.emit_fragment(out, item_index, item, fragment_start, item_end, fragment_origin, fragment_width, *line);
            }
            let closes_run = item_end == item.units.1 as usize && item.flags & flag::LAST != 0;
            let ends_line_inside = item_end == end && !closes_run;
            if let Some(fit) = fit_in_fragment {
                walked.fit = walked.fit.max(lu_ceil(fragment_origin + fit));
            }
            position = fragment_origin + fragment_width;
            // The chip's closing padding: where its run ends, and on the drawn line also
            // where the line ends inside it (the breaker does not reserve room for that
            // one, so such a chip pokes out of the box by its padding, as in Chromium).
            if style.chip.is_some() && (closes_run || (ends_line_inside && view == View::Drawn)) {
                position += self.chip_pad(item);
                if let (Some((out, _)), Some((_, left, chip_style))) = (sink.as_mut(), chip.take()) {
                    self.emit_chip(out, left, position, chip_style, item);
                }
                walked.fit = walked.fit.max(position);
            }
            unit_index = item_end;
            if unit_index < end {
                item_index += 1;
                position = self.open_item(item_index, unit_index, position);
            }
        }
        // A line that ends at a soft hyphen shows a hyphen.
        if end < units.len()
            && units[end - 1].flags & flag::SOFT_HYPHEN != 0
            && let Some(hyphen) = paragraph.hyphen
        {
            let advance = lu_ceil(hyphen.advance * scale);
            if let Some((out, line)) = sink.as_mut() {
                out.frags.push(Frag {
                    item: HYPHEN_ITEM,
                    units: (end as u32, end as u32),
                    glyphs: (0, 0),
                    x: position / scale,
                    width: advance / scale,
                    dx: position / scale,
                    line: *line,
                });
            }
            position += advance;
            walked.fit = walked.fit.max(position);
        }
        walked.end = position;
        walked
    }

    /// The advance of unit `index` without kerning against the next glyph, device px.
    fn reshaped_advance(&self, item: &Item, index: usize, shaped: f32) -> f32 {
        if item.flags & flag::RTL != 0 {
            return shaped;
        }
        let paragraph = self.paragraph;
        let style = &paragraph.styles[item.style as usize];
        let (from, to) = paragraph.glyph_range(item, index as u32, index as u32 + 1);
        let mut nominal = style.letter_spacing;
        for glyph in &paragraph.glyphs[from as usize..to as usize] {
            match self.engine.nominal_advance(item.font, glyph.id, crate::paragraph::shaping_size(style.size, self.scale)) {
                Some(advance) => nominal += advance,
                None => return shaped,
            }
        }
        nominal * self.scale
    }

    #[allow(clippy::too_many_arguments)]
    fn emit_fragment(&self, out: &mut Output, item_index: usize, item: &Item, from: usize, to: usize, origin: f32, width: f32, line: u32) {
        let paragraph = self.paragraph;
        let scale = self.scale;
        let style = &paragraph.styles[item.style as usize];
        let metrics = self.metrics[item.style as usize];
        // Tabs move what follows them: a fragment per stretch between tabs.
        let mut start = from;
        let mut x = origin;
        let mut cursor = from;
        let flush = |out: &mut Output, start: usize, end: usize, x: f32, width: f32| {
            if start >= end {
                return;
            }
            let glyphs = paragraph.glyph_range(item, start as u32, end as u32);
            let first_x = paragraph.glyphs.get(glyphs.0 as usize).map_or(0., |glyph| glyph.x);
            let first_x = if glyphs.0 < glyphs.1 { first_x } else { 0. };
            out.frags.push(Frag {
                item: item_index as u32,
                units: (start as u32, end as u32),
                glyphs,
                x: x / scale,
                width: width / scale,
                dx: x / scale - first_x,
                line,
            });
        };
        let mut run = 0.0f32;
        while cursor < to {
            let unit = &paragraph.units[cursor];
            if unit.flags & flag::TAB != 0 {
                flush(out, start, cursor, x, run);
                let tab = self.advance(unit, x + run);
                flush(out, cursor, cursor + 1, x + run, tab);
                x += run + tab;
                run = 0.;
                start = cursor + 1;
            } else {
                run += unit.advance * scale;
            }
            cursor += 1;
        }
        let rest = (origin + width - x).max(0.);
        flush(out, start, to, x, rest);

        // Decorations: positions relative to the baseline for now (`finish_decos`).
        if style.underline || style.strikethrough {
            let text = (paragraph.units[from].text, paragraph.unit_end(to - 1));
            // Blink: `auto` thickness is a tenth of the font size, at least one pixel,
            // whole device pixels.
            let thickness = (style.size * scale / 10.).floor().max(1.);
            if style.underline {
                let gap = (thickness / 2.).ceil().max(1.);
                out.decos.push(Deco {
                    kind: DecoKind::Underline,
                    x: origin / scale,
                    y: gap,
                    width: width / scale,
                    height: thickness,
                    color: style.color,
                    radius: 0.,
                    text,
                    line: 0,
                });
            }
            if style.strikethrough {
                out.decos.push(Deco {
                    kind: DecoKind::Strike,
                    x: origin / scale,
                    y: -(metrics.ascent / 3.).round(),
                    width: width / scale,
                    height: thickness,
                    color: style.color,
                    radius: 0.,
                    text,
                    line: 0,
                });
            }
        }
    }

    fn emit_chip(&self, out: &mut Output, left: f32, right: f32, style_index: u16, item: &Item) {
        let style = &self.paragraph.styles[style_index as usize];
        let Some(chip) = style.chip else { return };
        let metrics = self.metrics[style_index as usize];
        let pad_y = lu_floor(chip.pad_y * self.scale);
        out.decos.push(Deco {
            kind: DecoKind::Chip,
            x: left / self.scale,
            y: -(metrics.ascent + pad_y),
            width: (right - left) / self.scale,
            height: metrics.ascent + metrics.descent + 2. * pad_y,
            color: chip.background,
            radius: chip.radius,
            text: item.text,
            line: 0,
        });
    }
}
