//! A field's text on screen: one [`Layout`] per hard line (a *block*), stacked.
//!
//! Splitting at `\n` is what keeps typing cheap: an edit re-shapes the line it happened in
//! and every other block keeps its paragraph and layout (matched by a hash of the line).
//! What a block shows is not always its text: an emoji atom is one inline box, a password
//! is bullets, and the IME's preedit is underlined; `Sub` records where display and
//! buffer offsets part, and everything public here speaks buffer offsets.

use std::hash::{Hash as _, Hasher as _};
use std::ops::Range;
use std::rc::Rc;

use decibell_richtext::emoji::EmojiAsset;
use gpui::{App, Bounds, Hsla, Pixels, Point, Window, fill, point, px, size};
use unicode_segmentation::UnicodeSegmentation;

use super::buffer::Buffer;
use crate::cache::KeyHasher;
use crate::{Affinity, Engine, Layout, OBJECT_REPLACEMENT, PaintOptions, ParagraphBuilder, Pos, TextStyle, Wrapping};

const BULLET: &str = "•";

/// How a field's text is drawn. No theme in here: the caller fills it.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct InputStyle {
    /// Face, size, colour, line height and letter-spacing of the text.
    pub text: TextStyle,
    pub placeholder_color: Hsla,
    pub selection_background: Hsla,
    /// The colour of selected text; `None` keeps its own.
    pub selection_color: Option<Hsla>,
    pub caret_color: Hsla,
    /// Caret width in px (Chromium: 1).
    pub caret_width: f32,
    /// How a multi-line field wraps (a single line never does).
    pub wrapping: Wrapping,
    /// An emoji atom: its side in em, how far it hangs under the baseline in em, and the
    /// margin on each side in px (`.rich-emoji`: 1.375em, `vertical-align: -0.3em`, 1px).
    pub atom_em: f32,
    pub atom_drop_em: f32,
    pub atom_margin: f32,
}

impl InputStyle {
    /// A field in `text`'s style with Chromium's default selection colours on a dark
    /// ground and a caret in the text colour.
    pub fn new(text: TextStyle) -> Self {
        Self {
            text,
            placeholder_color: text.color.opacity(0.5),
            selection_background: gpui::rgb(0x0538A4).into(),
            selection_color: Some(gpui::rgb(0xFFFFFF).into()),
            caret_color: text.color,
            caret_width: 1.,
            wrapping: Wrapping::PRE_WRAP,
            atom_em: 1.375,
            atom_drop_em: 0.3,
            atom_margin: 1.,
        }
    }
}

/// A piece of a block whose display differs from its text.
#[derive(Clone, Copy, Debug)]
struct Sub {
    /// Bytes in the block's text.
    text: (u32, u32),
    /// Bytes in the paragraph's text.
    shown: (u32, u32),
    asset: Option<EmojiAsset>,
}

struct Block {
    /// Buffer bytes of the line, without its `\n`.
    range: Range<usize>,
    key: u64,
    layout: Rc<Layout>,
    top: f32,
    subs: Vec<Sub>,
    /// The line has no text: the layout is a space, kept for the line box.
    empty: bool,
}

impl Block {
    fn height(&self) -> f32 {
        self.layout.height()
    }

    /// Block-local text offset → paragraph offset.
    fn shown(&self, local: usize) -> u32 {
        if self.empty {
            return 0;
        }
        let local = local as u32;
        let mut delta = 0i64;
        for sub in &self.subs {
            if local <= sub.text.0 {
                break;
            }
            if local < sub.text.1 {
                return sub.shown.0;
            }
            delta += (sub.shown.1 - sub.shown.0) as i64 - (sub.text.1 - sub.text.0) as i64;
        }
        (local as i64 + delta) as u32
    }

    /// Paragraph offset → block-local text offset (a place inside a replaced piece goes to
    /// its nearer end).
    fn local(&self, shown: u32) -> usize {
        if self.empty {
            return 0;
        }
        let mut delta = 0i64;
        for sub in &self.subs {
            if shown <= sub.shown.0 {
                break;
            }
            if shown < sub.shown.1 {
                let nearer_end = shown - sub.shown.0 >= sub.shown.1 - shown;
                return if nearer_end { sub.text.1 } else { sub.text.0 } as usize;
            }
            delta += (sub.text.1 - sub.text.0) as i64 - (sub.shown.1 - sub.shown.0) as i64;
        }
        ((shown as i64 + delta) as usize).min(self.range.len())
    }
}

/// See the module docs.
#[derive(Default)]
pub(crate) struct Display {
    blocks: Vec<Block>,
    height: f32,
    /// What the blocks were built for.
    built: Option<Built>,
}

#[derive(Clone, Copy, PartialEq)]
struct Built {
    revision: u64,
    marked: Option<(usize, usize)>,
    style: InputStyle,
    width: f32,
    scale: f32,
    masked: bool,
    multiline: bool,
}

/// Counters for the tests and the measurements.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct DisplayStats {
    /// Blocks shaped (not reused) by the last update.
    pub shaped: usize,
    /// Blocks the last update kept.
    pub reused: usize,
    /// Blocks only wrapped again (a new width: no shaping).
    pub wrapped: usize,
}

impl Display {
    /// Brings the blocks up to date with the buffer at `width` px. Cheap when nothing
    /// changed; otherwise only changed lines are shaped.
    pub fn update(&mut self, buffer: &Buffer, style: &InputStyle, width: f32, scale: f32, engine: &Engine) -> DisplayStats {
        let rules = buffer.rules();
        let width = if rules.multiline { width } else { f32::INFINITY };
        let marked = buffer.marked().map(|range| (range.start, range.end));
        let built = Built {
            revision: buffer.revision(),
            marked,
            style: *style,
            width,
            scale,
            masked: rules.masked,
            multiline: rules.multiline,
        };
        if self.built == Some(built) {
            return DisplayStats::default();
        }
        // A new style or scale invalidates every paragraph; a new text only the lines whose
        // hash is new; a new width shapes nothing (the paragraphs are wrapped again).
        let reusable = self
            .built
            .is_some_and(|old| old.style == built.style && old.scale == built.scale && old.masked == built.masked);
        let same_width = self.built.is_some_and(|old| old.width == built.width);
        let mut stats = DisplayStats::default();
        if reusable && self.built.is_some_and(|old| old.revision == built.revision && old.marked == built.marked) {
            let mut top = 0.;
            for block in &mut self.blocks {
                block.layout = Rc::new(Layout::new(block.layout.paragraph().clone(), width, engine));
                block.top = top;
                top += block.layout.height();
                stats.wrapped += 1;
            }
            self.height = top;
            self.built = Some(built);
            return stats;
        }
        let mut old = std::mem::take(&mut self.blocks);
        if !reusable {
            old.clear();
        }
        let text = buffer.text();
        let wrapping = if rules.multiline { style.wrapping } else { Wrapping::PRE };
        let line_count = if rules.multiline { text.split('\n').count() } else { 1 };
        let shift = old.len() as isize - line_count as isize;
        let mut top = 0.0f32;
        let mut start = 0;
        let mut atoms = buffer.atoms();
        for index in 0..line_count {
            let end = if rules.multiline {
                text[start..].find('\n').map_or(text.len(), |at| start + at)
            } else {
                text.len()
            };
            let line = &text[start..end];
            let line_marked = marked
                .map(|(from, to)| (from.max(start), to.min(end)))
                .filter(|(from, to)| from < to)
                .map(|(from, to)| (from - start, to - start));
            let mut hasher = KeyHasher::default();
            if rules.masked {
                // Never the secret: a password's key is only how many characters it has and
                // how many bytes each is (what the bullets and their offsets depend on).
                for grapheme in line.graphemes(true) {
                    grapheme.len().hash(&mut hasher);
                }
            } else {
                line.hash(&mut hasher);
            }
            line_marked.hash(&mut hasher);
            let key = hasher.finish();

            // The same line as before sits at the same index (an edit further down) or at
            // the same distance from the end (an edit further up).
            let candidates = [index as isize, index as isize + shift];
            let found = candidates
                .into_iter()
                .filter(|at| *at >= 0 && (*at as usize) < old.len())
                .map(|at| at as usize)
                .find(|at| old[*at].key == key && old[*at].range.len() == line.len());
            while atoms.first().is_some_and(|atom| atom.start < start) {
                atoms = &atoms[1..];
            }
            let block = match found {
                Some(at) => {
                    let kept = &mut old[at];
                    kept.key = !key; // taken: not found twice
                    let layout = if same_width {
                        stats.reused += 1;
                        kept.layout.clone()
                    } else {
                        stats.wrapped += 1;
                        Rc::new(Layout::new(kept.layout.paragraph().clone(), width, engine))
                    };
                    Block {
                        range: start..end,
                        key,
                        layout,
                        top,
                        subs: std::mem::take(&mut kept.subs),
                        empty: kept.empty,
                    }
                }
                None => {
                    stats.shaped += 1;
                    let mut subs = Vec::new();
                    let mut builder = ParagraphBuilder::new(engine, &style.text, wrapping, scale);
                    if line.is_empty() {
                        builder.text(" ", &style.text, 0);
                    } else if rules.masked {
                        let mut bullets = String::new();
                        for (at, grapheme) in line.grapheme_indices(true) {
                            subs.push(Sub {
                                text: (at as u32, (at + grapheme.len()) as u32),
                                shown: (bullets.len() as u32, (bullets.len() + BULLET.len()) as u32),
                                asset: None,
                            });
                            bullets.push_str(BULLET);
                        }
                        builder.text(&bullets, &style.text, 0);
                    } else {
                        let underlined = TextStyle {
                            underline: true,
                            ..style.text
                        };
                        let mut shown = 0u32;
                        let run = |builder: &mut ParagraphBuilder, from: usize, to: usize, shown: &mut u32| {
                            // A run splits where the preedit starts and ends.
                            let mut cuts = [from, to, to, to];
                            if let Some((mark_from, mark_to)) = line_marked {
                                cuts[1] = mark_from.clamp(from, to);
                                cuts[2] = mark_to.clamp(from, to);
                            }
                            for (piece, in_mark) in [(0, false), (1, true), (2, false)] {
                                let (a, b) = (cuts[piece], cuts[piece + 1]);
                                if a < b {
                                    builder.text(&line[a..b], if in_mark { &underlined } else { &style.text }, 0);
                                    *shown += (b - a) as u32;
                                }
                            }
                        };
                        let mut at = 0;
                        let side = style.atom_em * style.text.size;
                        for atom in atoms.iter().take_while(|atom| atom.start < end) {
                            let Some(asset) = atom.asset else { continue };
                            let (from, to) = (atom.start - start, atom.end - start);
                            let device_px = (side * scale).round() as u32;
                            if engine.emoji.prepare(asset, device_px).is_none() {
                                continue;
                            }
                            run(&mut builder, at, from, &mut shown);
                            builder.inline_box(
                                side + 2. * style.atom_margin,
                                side,
                                style.atom_drop_em * style.text.size,
                                subs.len() as u32,
                            );
                            subs.push(Sub {
                                text: (from as u32, to as u32),
                                shown: (shown, shown + OBJECT_REPLACEMENT.len() as u32),
                                asset: Some(asset),
                            });
                            shown += OBJECT_REPLACEMENT.len() as u32;
                            at = to;
                        }
                        run(&mut builder, at, line.len(), &mut shown);
                    }
                    let layout = Rc::new(Layout::new(Rc::new(builder.build()), width, engine));
                    Block {
                        range: start..end,
                        key,
                        layout,
                        top,
                        subs,
                        empty: line.is_empty(),
                    }
                }
            };
            top += block.height();
            self.blocks.push(block);
            start = end + 1;
        }
        self.height = top;
        self.built = Some(built);
        stats
    }

    /// Forgets the layouts (the secret's bullets, the glyphs): the next update rebuilds.
    pub fn clear(&mut self) {
        *self = Self::default();
    }

    pub fn height(&self) -> f32 {
        self.height
    }

    /// The width the text needs (a single line: for scrolling).
    pub fn width(&self) -> f32 {
        self.blocks.iter().map(|block| block.layout.width()).fold(0., f32::max)
    }

    /// The height of one line of text (the first block's first line).
    pub fn line_height(&self) -> f32 {
        self.blocks
            .first()
            .and_then(|block| block.layout.line(0))
            .map_or(0., |line| line.height)
    }

    /// What the blocks draw, joined with `\n` (an empty line's stand-in space left out).
    pub fn shown_text(&self) -> String {
        let lines: Vec<&str> = self
            .blocks
            .iter()
            .map(|block| if block.empty { "" } else { block.layout.paragraph().text() })
            .collect();
        lines.join("\n")
    }

    /// Heap bytes of the layouts and paragraphs held.
    pub fn heap_bytes(&self) -> usize {
        self.blocks
            .iter()
            .map(|block| block.layout.heap_bytes() + block.layout.paragraph().heap_bytes() + size_of::<Block>())
            .sum()
    }

    fn block_of(&self, offset: usize) -> usize {
        self.blocks
            .partition_point(|block| block.range.end < offset)
            .min(self.blocks.len().saturating_sub(1))
    }

    fn pos(&self, block: &Block, offset: usize, affinity: Affinity) -> Pos {
        Pos {
            offset: block.shown(offset.saturating_sub(block.range.start).min(block.range.len())),
            affinity,
        }
    }

    /// Where a caret at `offset` stands: (x, top, height) from the text's top-left.
    pub fn caret(&self, offset: usize, affinity: Affinity) -> (f32, f32, f32) {
        let Some(block) = self.blocks.get(self.block_of(offset)) else {
            return (0., 0., 0.);
        };
        let (x, top, height) = block.layout.caret(self.pos(block, offset, affinity));
        (if block.empty { 0. } else { x }, block.top + top, height)
    }

    /// The caret stop nearest to a point (from the text's top-left).
    pub fn hit(&self, x: f32, y: f32) -> (usize, Affinity) {
        let at = self
            .blocks
            .partition_point(|block| block.top + block.height() <= y)
            .min(self.blocks.len().saturating_sub(1));
        let Some(block) = self.blocks.get(at) else {
            return (0, Affinity::Downstream);
        };
        let hit = block.layout.hit(x, y - block.top);
        (block.range.start + block.local(hit.pos.offset), hit.pos.affinity)
    }

    /// One line up or down from `offset`, keeping `goal_x`. `None` at the first / last
    /// line (the caller goes to the start / end of the text, as Chromium does).
    pub fn vertical(&self, offset: usize, affinity: Affinity, goal_x: f32, down: bool) -> Option<(usize, Affinity)> {
        let at = self.block_of(offset);
        let block = self.blocks.get(at)?;
        let line = block.layout.line_of(self.pos(block, offset, affinity));
        let (target, target_line) = if down {
            if line + 1 < block.layout.line_count() {
                (block, line + 1)
            } else {
                (self.blocks.get(at + 1)?, 0)
            }
        } else if line > 0 {
            (block, line - 1)
        } else {
            let above = self.blocks.get(at.checked_sub(1)?)?;
            (above, above.layout.line_count() - 1)
        };
        let info = target.layout.line(target_line)?;
        let hit = target.layout.hit(goal_x, info.top + info.height / 2.);
        Some((target.range.start + target.local(hit.pos.offset), hit.pos.affinity))
    }

    /// The ends of the visual line `offset` is on: (start, end), each with the affinity
    /// that keeps the caret on that line.
    pub fn visual_line(&self, offset: usize, affinity: Affinity) -> ((usize, Affinity), (usize, Affinity)) {
        let Some(block) = self.blocks.get(self.block_of(offset)) else {
            return ((0, Affinity::Downstream), (0, Affinity::Downstream));
        };
        if block.empty {
            let at = (block.range.start, Affinity::Downstream);
            return (at, at);
        }
        let pos = self.pos(block, offset, affinity);
        let line = block.layout.line_of(pos);
        let (from, to) = block.layout.line_range(pos);
        let last = line + 1 == block.layout.line_count();
        (
            (block.range.start + block.local(from), Affinity::Downstream),
            (
                block.range.start + block.local(to),
                if last { Affinity::Downstream } else { Affinity::Upstream },
            ),
        )
    }

    /// Paints the text with its top-left at `origin`: the selection, the glyphs, the
    /// atoms. Blocks outside `visible` (window coordinates) are skipped.
    #[allow(clippy::too_many_arguments)]
    pub fn paint(
        &self,
        origin: Point<Pixels>,
        visible: Bounds<Pixels>,
        selection: Range<usize>,
        style: &InputStyle,
        engine: &Engine,
        window: &mut Window,
        cx: &mut App,
    ) {
        let visible_top = f32::from(visible.origin.y - origin.y);
        let visible_bottom = visible_top + f32::from(visible.size.height);
        for block in &self.blocks {
            if block.top + block.height() < visible_top || block.top > visible_bottom {
                continue;
            }
            let at = point(origin.x, origin.y + px(block.top));
            let selected = (selection.start <= block.range.end && selection.end >= block.range.start)
                .then(|| {
                    (
                        self.pos(block, selection.start.max(block.range.start), Affinity::Downstream).offset,
                        self.pos(block, selection.end.min(block.range.end), Affinity::Downstream).offset,
                    )
                })
                .filter(|_| !selection.is_empty());
            // A selected line break shows as a sliver after the line, as in Chromium.
            if !selection.is_empty() && selection.start <= block.range.end && selection.end > block.range.end {
                let last = block.layout.line_count().saturating_sub(1);
                if let Some(line) = block.layout.line(last) {
                    let x = if block.empty { 0. } else { line.width };
                    window.paint_quad(fill(
                        Bounds::new(
                            point(at.x + px(x), at.y + px(line.top)),
                            size(px(style.text.size * 0.3), px(line.height)),
                        ),
                        style.selection_background,
                    ));
                }
            }
            if block.empty {
                continue;
            }
            let options = PaintOptions {
                selection: selected,
                selection_background: style.selection_background,
                selection_color: style.selection_color,
                ..PaintOptions::default()
            };
            block.layout.paint(at, &options, window, cx);
            if block.subs.iter().any(|sub| sub.asset.is_some()) {
                for (tag, [x, y, width, height]) in block.layout.boxes() {
                    let Some(asset) = block.subs.get(tag as usize).and_then(|sub| sub.asset) else {
                        continue;
                    };
                    let side = width - 2. * style.atom_margin;
                    let bounds = Bounds::new(
                        point(at.x + px(x + style.atom_margin), at.y + px(y)),
                        size(px(side), px(height)),
                    );
                    engine.emoji.paint(asset, bounds, window, cx);
                }
            }
        }
    }
}
