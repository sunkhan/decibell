//! Positions in a [`Layout`]: point → position, position → geometry, word and line ranges.
//!
//! A position is a byte offset into the paragraph's text, between two clusters, plus an
//! affinity for the one place that is two places on screen: a soft line break (the end of
//! one line and the start of the next).
//!
//! Right-to-left text: a caret stop's x is computed inside its fragment in visual order, so
//! clicking Arabic or Hebrew lands on the right cluster; a selection that crosses a
//! direction boundary is drawn as one span from its first to its last x (not the split
//! spans a bidi-aware editor draws).

use unicode_segmentation::UnicodeSegmentation;

use crate::layout::{FIRST_MARK_ITEM, Layout};
use crate::paragraph::flag;

/// Which side of a soft line break a position belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub enum Affinity {
    /// With the text before it (the end of a line).
    Upstream,
    /// With the text after it (the start of a line).
    #[default]
    Downstream,
}

/// A caret position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct Pos {
    /// Byte offset into [`crate::Paragraph::text`].
    pub offset: u32,
    pub affinity: Affinity,
}

impl Pos {
    pub fn new(offset: u32) -> Self {
        Self {
            offset,
            affinity: Affinity::Downstream,
        }
    }
}

/// The answer to "what is at this point".
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Hit {
    /// The nearest caret position (always valid, also for points outside the text).
    pub pos: Pos,
    /// The cluster under the point, when the point is on one: its text range.
    pub cluster: Option<(u32, u32)>,
    /// The tag of the run or box under the point.
    pub tag: Option<u32>,
    pub line: usize,
}

/// One caret stop interval on a line: a unit and where it sits.
struct Cell {
    text: (u32, u32),
    left: f32,
    right: f32,
    rtl: bool,
    tag: u32,
    selectable: bool,
}

impl Layout {
    fn for_each_cell(&self, line_index: usize, mut visit: impl FnMut(Cell) -> bool) {
        let paragraph = &*self.paragraph;
        let line = &self.lines[line_index];
        for frag in &self.frags[line.frags.0 as usize..line.frags.1 as usize] {
            if frag.item >= FIRST_MARK_ITEM {
                continue;
            }
            let item = &paragraph.items[frag.item as usize];
            let rtl = item.flags & flag::RTL != 0;
            let selectable = paragraph.styles[item.style as usize].selectable;
            let total: f32 = paragraph.units[frag.units.0 as usize..frag.units.1 as usize]
                .iter()
                .map(|unit| unit.advance)
                .sum();
            // Tabs and re-shaped line ends make a fragment's width differ from its units'
            // advances: spread the difference so the stops still end at the fragment's edge.
            let stretch = if total > 0. { frag.width / total } else { 0. };
            let mut walked = 0.;
            for index in frag.units.0..frag.units.1 {
                let unit = &paragraph.units[index as usize];
                let advance = unit.advance * stretch;
                let (left, right) = if rtl {
                    (frag.x + frag.width - walked - advance, frag.x + frag.width - walked)
                } else {
                    (frag.x + walked, frag.x + walked + advance)
                };
                walked += advance;
                let cell = Cell {
                    text: (unit.text, paragraph.unit_end(index as usize)),
                    left,
                    right,
                    rtl,
                    tag: item.tag,
                    selectable,
                };
                if !visit(cell) {
                    return;
                }
            }
        }
    }

    /// The line a y coordinate (px from the layout's top) falls on, clamped to the first
    /// and last line.
    pub fn line_at_y(&self, y: f32) -> usize {
        let after = self.lines.partition_point(|line| line.top + line.height <= y);
        after.min(self.lines.len().saturating_sub(1))
    }

    /// What is at (`x`, `y`), px from the layout's top-left. Points outside the text give
    /// the nearest position: above the first line its start, below the last line its end,
    /// left or right of a line that line's start or end.
    pub fn hit(&self, x: f32, y: f32) -> Hit {
        if self.lines.is_empty() {
            return Hit {
                pos: Pos::default(),
                cluster: None,
                tag: None,
                line: 0,
            };
        }
        let line_index = self.line_at_y(y);
        let line = &self.lines[line_index];
        let (line_start, line_end) = self.line_text(line);
        let inside_y = y >= line.top && y < line.top + line.height;
        let mut best: Option<(f32, u32)> = None;
        let mut cluster = None;
        let mut tag = None;
        self.for_each_cell(line_index, |cell| {
            if cell.text.0 >= cell.text.1 {
                return true;
            }
            let newline = self.paragraph.text.as_bytes()[cell.text.0 as usize] == b'\n';
            if newline {
                return true;
            }
            if inside_y && x >= cell.left && x < cell.right {
                cluster = Some(cell.text);
                tag = Some(cell.tag);
            }
            let (before, after) = if cell.rtl { (cell.right, cell.left) } else { (cell.left, cell.right) };
            for (stop_x, offset) in [(before, cell.text.0), (after, cell.text.1)] {
                let distance = (stop_x - x).abs();
                if best.is_none_or(|(nearest, _)| distance < nearest) {
                    best = Some((distance, offset));
                }
            }
            true
        });
        let offset = best.map_or(line_start, |(_, offset)| offset);
        // The end of a soft-wrapped line and the start of the next are the same offset.
        let content_end = if line.hard { line_end - 1 } else { line_end };
        let affinity = if offset >= content_end && !line.hard && line_index + 1 < self.lines.len() && offset > line_start {
            Affinity::Upstream
        } else {
            Affinity::Downstream
        };
        Hit {
            pos: Pos {
                offset: offset.min(content_end.max(line_start)),
                affinity,
            },
            cluster,
            tag,
            line: line_index,
        }
    }

    /// The tag of the run or box at a point, if the point is on one (for links: hover,
    /// cursor, click).
    pub fn tag_at(&self, x: f32, y: f32) -> Option<u32> {
        self.hit(x, y).tag
    }

    /// The line a position is on.
    pub fn line_of(&self, pos: Pos) -> usize {
        let offset = pos.offset;
        let mut index = self
            .lines
            .partition_point(|line| self.line_text(line).1 <= offset)
            .min(self.lines.len().saturating_sub(1));
        if pos.affinity == Affinity::Upstream
            && index > 0
            && self.line_text(&self.lines[index]).0 == offset
            && !self.lines[index - 1].hard
        {
            index -= 1;
        }
        index
    }

    /// Where a caret at `pos` stands: x, the line's top and the line's height, px from the
    /// layout's top-left.
    pub fn caret(&self, pos: Pos) -> (f32, f32, f32) {
        if self.lines.is_empty() {
            return (0., 0., self.paragraph.base.line_height.px(self.paragraph.base.size));
        }
        let line_index = self.line_of(pos);
        let line = &self.lines[line_index];
        (self.x_on_line(line_index, pos.offset, pos.affinity), line.top, line.height)
    }

    /// The x of a text offset on a line (clamped to the line's ends). Where two runs meet
    /// with space between them (a chip's padding, a margin) the affinity picks the side:
    /// upstream the end of the earlier run, downstream the start of the later one.
    pub(crate) fn x_on_line(&self, line_index: usize, offset: u32, affinity: Affinity) -> f32 {
        let mut starts_here = None;
        let mut ends_here = None;
        let mut end = 0.0f32;
        let mut first = None;
        self.for_each_cell(line_index, |cell| {
            if first.is_none() {
                first = Some(if cell.rtl { cell.right } else { cell.left });
            }
            end = end.max(cell.right);
            if offset == cell.text.1 {
                ends_here = Some(if cell.rtl { cell.left } else { cell.right });
            }
            if offset >= cell.text.0 && offset < cell.text.1 {
                // Inside a unit (a ligature of several clusters): share its width out.
                let part = (offset - cell.text.0) as f32 / (cell.text.1 - cell.text.0) as f32;
                let part = part * (cell.right - cell.left);
                starts_here = Some(if cell.rtl { cell.right - part } else { cell.left + part });
                return false;
            }
            true
        });
        let (line_start, _) = self.line_text(&self.lines[line_index]);
        let found = match affinity {
            Affinity::Upstream => ends_here.or(starts_here),
            Affinity::Downstream => starts_here.or(ends_here),
        };
        match found {
            Some(x) => x,
            None if offset <= line_start => first.unwrap_or(0.),
            None => end,
        }
    }

    /// The horizontal span of a selection on one line, if it touches the line. A selected
    /// line break shows as a sliver past the line's end, as in Chromium.
    pub(crate) fn selection_span(&self, line_index: usize, from: u32, to: u32) -> Option<(f32, f32)> {
        let line = &self.lines[line_index];
        let (line_start, line_end) = self.line_text(line);
        if to <= line_start || from >= line_end {
            return None;
        }
        // Text a selection skips at the line's end (the "(edited)" marker) stays unpainted.
        let mut selectable_end = None;
        let mut content_end = 0.0f32;
        self.for_each_cell(line_index, |cell| {
            if cell.selectable {
                selectable_end = None;
                content_end = content_end.max(cell.right);
            } else if selectable_end.is_none() {
                selectable_end = Some(cell.left);
            }
            true
        });
        let limit = selectable_end.unwrap_or(f32::INFINITY);
        let left = if from <= line_start { 0. } else { self.x_on_line(line_index, from, Affinity::Downstream) };
        let mut right = if to >= line_end {
            let newline = if line.hard { self.paragraph.base.size * 0.3 } else { 0. };
            content_end + newline
        } else {
            self.x_on_line(line_index, to, Affinity::Upstream)
        };
        right = right.min(limit.max(left));
        (right > left).then_some((left, right))
    }

    /// The rectangles of a text range, one per line: (x, y, width, height) px from the
    /// layout's top-left, each as high as its line box.
    pub fn rects_for_range(&self, from: u32, to: u32, mut visit: impl FnMut([f32; 4])) {
        for (index, line) in self.lines.iter().enumerate() {
            if let Some((left, right)) = self.selection_span(index, from, to) {
                visit([left, line.top, right - left, line.height]);
            }
        }
    }

    /// The word at a text offset (a double click): a UAX #29 word, a run of white space, or
    /// one punctuation mark.
    pub fn word_at(&self, offset: u32) -> (u32, u32) {
        let text = &*self.paragraph.text;
        let offset = (offset as usize).min(text.len());
        let mut last = (0, 0);
        for (start, word) in text.split_word_bound_indices() {
            let end = start + word.len();
            last = (start as u32, end as u32);
            if offset < end {
                break;
            }
        }
        last
    }

    /// The text of the line a position is on (a visual line; a triple click in Chromium
    /// takes the whole block: use `0..text().len()` for that).
    pub fn line_range(&self, pos: Pos) -> (u32, u32) {
        if self.lines.is_empty() {
            return (0, 0);
        }
        self.line_text(&self.lines[self.line_of(pos)])
    }
}
