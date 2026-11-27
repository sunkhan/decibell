//! Rich text: a `decibell-richtext` [`Document`] laid out and drawn as the Electron client's
//! `MessageText` draws it.
//!
//! A [`RichLayout`] is the whole message body at one width: its paragraphs as
//! [`Layout`]s, with emoji and inline formulas as boxes in the text, and a *slot* for every
//! fenced code block and display formula. A slot is only a reserved rectangle: its height
//! comes from the caller and its content is the caller's element (the chat view owns those
//! blocks' chrome; [`crate::Engine::code_paragraph`] and [`crate::Math`] are what it puts
//! inside).
//!
//! The layout is immutable and shared (`Rc`): the list asks it for the row's height, the
//! row's [`RichText`] element paints it, and neither shapes anything twice.
//!
//! Selection: a [`RichText`] paints the range it is told and never owns one. Positions are
//! [`RichPos`] (block, offset); a list-level controller turns pointer events into them with
//! [`RichLayout::hit`], keeps the anchor and focus (which may be in different rows), and
//! hands each visible row its share as a [`RichSelection`].

use std::cell::Cell;
use std::hash::{Hash as _, Hasher as _};
use std::rc::Rc;

use decibell_richtext::plain::TextPos;
use decibell_richtext::{Block, Document, EmojiAsset, Span, SpanKind};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, CursorStyle, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, Hsla, InspectorElementId, LayoutId, MouseButton, MouseDownEvent, MouseMoveEvent,
    MouseUpEvent, Pixels, Point, SharedString, Style, Window, point, px, size,
};

use crate::cache::{Generations, KeyHasher, LayoutCache, layout_key};
use crate::math::{Formula, Math, MathColors};
use crate::{Affinity, Chip, Engine, FontSpec, Layout, LineHeight, PaintOptions, ParagraphBuilder, Pos, TextStyle, Wrapping};

/// The tag of the "(edited)" marker's run (every other run's tag is its span's index).
pub const EDITED_TAG: u32 = u32::MAX - 8;

/// Bytes of message layouts kept beyond what views hold themselves.
const CACHE_BYTES: usize = 12 << 20;

/// Everything `MessageText` takes from CSS and the theme.
#[derive(Clone, Debug, PartialEq)]
pub struct RichStyle {
    /// `font-sans` and `font-mono`: family names the text system knows.
    pub family: SharedString,
    pub mono_family: SharedString,
    /// `text-body`, px.
    pub size: f32,
    /// `leading-body`, the unitless factor.
    pub leading: f32,
    /// The body's `letter-spacing`, px.
    pub letter_spacing: f32,
    pub color: Hsla,
    /// `text-accent`: links.
    pub link: Hsla,
    /// `text-text-muted` and `text-meta`: the "(edited)" marker.
    pub muted: Hsla,
    pub meta_size: f32,
    /// `--color-error`: TeX errors.
    pub error: Hsla,
    /// Inline code: `bg-bg-darkest rounded-sm`.
    pub code_background: Hsla,
    pub code_radius: f32,
}

impl RichStyle {
    fn key(&self) -> u64 {
        let mut hasher = KeyHasher::default();
        self.family.hash(&mut hasher);
        self.mono_family.hash(&mut hasher);
        for value in [self.size, self.leading, self.letter_spacing, self.meta_size, self.code_radius] {
            value.to_bits().hash(&mut hasher);
        }
        for color in [self.color, self.link, self.muted, self.error, self.code_background] {
            for value in [color.h, color.s, color.l, color.a] {
                value.to_bits().hash(&mut hasher);
            }
        }
        hasher.finish()
    }

    /// The body's own style: `text-body leading-body`.
    pub fn body(&self, engine: &Engine) -> TextStyle {
        self.run(engine, decibell_richtext::Style::NONE, self.color)
    }

    /// A run with the grammar's style bits, as the renderer's elements compute it:
    /// `<strong>` is 700, `<em>` slants, `<code>` is mono at 0.85 em in a chip.
    fn run(&self, engine: &Engine, bits: decibell_richtext::Style, color: Hsla) -> TextStyle {
        let code = bits.is_code();
        let face = engine.face(&FontSpec {
            family: if code { self.mono_family.clone() } else { self.family.clone() },
            weight: if bits.is_bold() { 700 } else { 400 },
            italic: bits.is_italic(),
        });
        let mut style = TextStyle::new(face, if code { self.size * 0.85 } else { self.size }, color)
            .leading(self.leading)
            .letter_spacing(self.letter_spacing);
        style.underline = bits.is_underline();
        style.strikethrough = bits.is_strike();
        if code {
            style.chip = Some(Chip {
                background: self.code_background,
                radius: self.code_radius,
                pad_x: 6.,
                pad_y: 2.,
            });
        }
        style
    }

    /// `ml-1 select-none align-baseline text-meta text-text-muted`.
    fn edited(&self, engine: &Engine) -> TextStyle {
        let mut style = self.run(engine, decibell_richtext::Style::NONE, self.muted);
        style.size = self.meta_size;
        style.margin_start = 4.;
        style.selectable = false;
        style
    }
}

/// A fenced code block or a display formula: a rectangle of the caller's.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Slot {
    /// Index of the block's span in the document.
    pub span: u32,
    pub kind: SlotKind,
    /// px from the layout's top.
    pub y: f32,
    pub height: f32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotKind {
    Code,
    Math,
}

enum BoxContent {
    Emoji(EmojiAsset),
    Math(Rc<Math>),
}

enum BlockKind {
    Text {
        layout: Rc<Layout>,
        /// What each inline box shows, by tag.
        boxes: Box<[(u32, BoxContent)]>,
    },
    Slot(Slot),
}

struct RichBlock {
    y: f32,
    height: f32,
    kind: BlockKind,
}

/// A position in a rich layout: a block and, in a text block, a caret position.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Default)]
pub struct RichPos {
    pub block: u32,
    pub offset: u32,
    pub affinity: Affinity,
}

impl PartialOrd for RichPos {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RichPos {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (self.block, self.offset).cmp(&(other.block, other.offset))
    }
}

/// The part of a selection one [`RichText`] shows.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum RichSelection {
    #[default]
    None,
    /// The selection starts above this text and ends below it.
    All,
    /// From the first position to the second (use the layout's [`RichLayout::start`] /
    /// [`RichLayout::end`] for a selection that enters or leaves the row).
    Range(RichPos, RichPos),
}

/// What is at a point of a rich layout.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct RichHit {
    /// The nearest caret position.
    pub pos: RichPos,
    /// The span under the point (a link, an emoji, text), when the point is on one.
    pub span: Option<u32>,
    /// The slot under the point.
    pub slot: Option<Slot>,
}

/// A message body laid out at a width. See the module docs.
pub struct RichLayout {
    blocks: Box<[RichBlock]>,
    width: f32,
    height: f32,
    math_colors: MathColors,
    /// Span indices that are links, sorted.
    links: Box<[u32]>,
}

pub(crate) struct RichCache {
    /// Paragraphs (shaped once per message) and their layouts per width.
    texts: LayoutCache,
    layouts: std::cell::RefCell<Generations<Rc<RichLayout>>>,
}

impl Default for RichCache {
    fn default() -> Self {
        Self {
            texts: LayoutCache::new(CACHE_BYTES),
            layouts: std::cell::RefCell::new(Generations::new(CACHE_BYTES / 4)),
        }
    }
}

/// The cache key of one paragraph of a message.
fn block_key(content_key: u64, block: u32) -> u64 {
    let mut hasher = KeyHasher::default();
    (content_key, block).hash(&mut hasher);
    hasher.finish()
}

/// What to lay out: a parsed message and the string it was parsed from.
#[derive(Clone, Copy)]
pub struct RichContent<'a> {
    pub document: &'a Document,
    pub source: &'a str,
    /// Append the "(edited)" marker.
    pub edited: bool,
}

impl Engine {
    /// The layout of a message body at `width` px, from the cache or built now.
    ///
    /// `key` names the content: anything that is the same for the same message text and
    /// slot heights (a message id mixed with its edit stamp and, if code blocks can
    /// collapse, their state). The style, the width and the scale are added to it here.
    ///
    /// `slot_height` is asked for the height of each code block and display formula at
    /// this width (span index, kind, width → px), margins included.
    pub fn rich_layout(
        &self,
        key: u64,
        content: RichContent<'_>,
        style: &RichStyle,
        width: f32,
        scale: f32,
        slot_height: &mut dyn FnMut(u32, SlotKind, f32) -> f32,
    ) -> Rc<RichLayout> {
        let mut hasher = KeyHasher::default();
        (key, style.key(), content.edited).hash(&mut hasher);
        let content_key = hasher.finish();
        let cache_key = layout_key(content_key, width, scale);
        if let Some(hit) = self.rich.layouts.borrow_mut().get(cache_key) {
            return hit;
        }
        let layout = Rc::new(self.build_rich(content_key, content, style, width, scale, slot_height));
        let bytes = size_of::<RichLayout>() + size_of_val(&*layout.blocks);
        self.rich.layouts.borrow_mut().insert(cache_key, layout.clone(), bytes);
        layout
    }

    /// (entries, bytes) of the cached message paragraphs and of their wrapped layouts.
    pub fn rich_cache_usage(&self) -> ((usize, usize), (usize, usize)) {
        self.rich.texts.usage()
    }

    fn build_rich(
        &self,
        content_key: u64,
        content: RichContent<'_>,
        style: &RichStyle,
        width: f32,
        scale: f32,
        slot_height: &mut dyn FnMut(u32, SlotKind, f32) -> f32,
    ) -> RichLayout {
        let RichContent { document, source, edited } = content;
        let mut blocks = Vec::new();
        let mut links = Vec::new();
        let mut y = 0.0f32;
        let mut span_index = 0u32;
        let block_count = document.blocks().count();
        let mut marker_placed = !edited;
        for (block_index, block) in document.blocks().enumerate() {
            let last = block_index + 1 == block_count;
            match block {
                Block::Paragraph(spans) => {
                    let first_span = span_index;
                    span_index += spans.len() as u32;
                    let marker = last && edited;
                    marker_placed |= marker;
                    let mut boxes = Vec::new();
                    let key = block_key(content_key, block_index as u32);
                    // Boxes are found again on a cache hit: they are cheap, and the
                    // emoji must be rasterised before the row is shown either way.
                    self.collect_boxes(document, source, spans, first_span, style, scale, &mut boxes, &mut links);
                    let layout = self.rich.texts.layout(key, width, scale, || {
                        let paragraph = self.rich.texts.paragraph(key, || {
                            self.rich_paragraph(document, source, spans, first_span, marker, style, scale)
                        });
                        Layout::new(paragraph, width, self)
                    });
                    let height = layout.height();
                    blocks.push(RichBlock {
                        y,
                        height,
                        kind: BlockKind::Text {
                            layout,
                            boxes: boxes.into(),
                        },
                    });
                    y += height;
                }
                Block::Code(_) | Block::Math(_) => {
                    let kind = if matches!(block, Block::Code(_)) { SlotKind::Code } else { SlotKind::Math };
                    let height = slot_height(span_index, kind, width).max(0.);
                    blocks.push(RichBlock {
                        y,
                        height,
                        kind: BlockKind::Slot(Slot {
                            span: span_index,
                            kind,
                            y,
                            height,
                        }),
                    });
                    span_index += 1;
                    y += height;
                }
            }
        }
        if !marker_placed {
            // The marker after a block (or alone): a line of its own.
            let key = block_key(content_key, u32::MAX);
            let layout = self.rich.texts.layout(key, width, scale, || {
                let paragraph = self.rich.texts.paragraph(key, || self.rich_paragraph(document, source, &[], span_index, true, style, scale));
                Layout::new(paragraph, width, self)
            });
            let height = layout.height();
            blocks.push(RichBlock {
                y,
                height,
                kind: BlockKind::Text {
                    layout,
                    boxes: Box::default(),
                },
            });
            y += height;
        }
        RichLayout {
            blocks: blocks.into(),
            width,
            height: y,
            math_colors: MathColors {
                text: style.color,
                error: style.error,
            },
            links: links.into(),
        }
    }

    /// The emoji size of a document: 24 px in running text, larger for a message that is
    /// only emoji (`jumboSizeFor`).
    fn emoji_size(document: &Document) -> f32 {
        document.jumbo_size().unwrap_or(decibell_richtext::INLINE_EMOJI_SIZE) as f32
    }

    #[allow(clippy::too_many_arguments)]
    fn collect_boxes(
        &self,
        document: &Document,
        source: &str,
        spans: &[Span],
        first_span: u32,
        style: &RichStyle,
        scale: f32,
        boxes: &mut Vec<(u32, BoxContent)>,
        links: &mut Vec<u32>,
    ) {
        let emoji_size = Self::emoji_size(document);
        for (index, span) in spans.iter().enumerate() {
            let tag = first_span + index as u32;
            match span.kind {
                SpanKind::Emoji { asset: Some(asset) } => {
                    let device_px = (emoji_size * scale).round() as u32;
                    if self.emoji.prepare(asset, device_px).is_some() {
                        boxes.push((tag, BoxContent::Emoji(asset)));
                    }
                }
                SpanKind::Math => {
                    if let Formula::Math(math) = self.formula(document.text(source, span), false, style.size, scale) {
                        boxes.push((tag, BoxContent::Math(math)));
                    }
                }
                SpanKind::Link { .. } => links.push(tag),
                _ => {}
            }
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn rich_paragraph(
        &self,
        document: &Document,
        source: &str,
        spans: &[Span],
        first_span: u32,
        edited: bool,
        style: &RichStyle,
        scale: f32,
    ) -> crate::Paragraph {
        let base = style.body(self);
        let emoji_size = Self::emoji_size(document);
        let mut builder = ParagraphBuilder::new(self, &base, Wrapping::MESSAGE, scale);
        for (index, span) in spans.iter().enumerate() {
            let tag = first_span + index as u32;
            let text = document.text(source, span);
            match span.kind {
                SpanKind::Text => {
                    builder.text(text, &style.run(self, span.style, style.color), tag);
                }
                SpanKind::Link { .. } => {
                    builder.text(text, &style.run(self, span.style, style.link), tag);
                }
                SpanKind::Emoji { asset } => {
                    let device_px = (emoji_size * scale).round() as u32;
                    if asset.is_some_and(|asset| self.emoji.prepare(asset, device_px).is_some()) {
                        // Twemoji.tsx: an inline-block of the emoji size, `align-[-0.2em]`.
                        builder.inline_box(emoji_size, emoji_size, 0.2 * style.size, tag);
                    } else {
                        // No Twemoji drawing: the characters at the emoji size, `line-height: 1`,
                        // in whatever emoji font the system has.
                        let mut run = style.run(self, span.style, style.color);
                        run.size = emoji_size;
                        run.line_height = LineHeight::Factor(1.);
                        builder.text(text, &run, tag);
                    }
                }
                SpanKind::Math => match self.formula(text, false, style.size, scale) {
                    Formula::Math(math) => {
                        builder.inline_box(math.width, math.height(), math.descent, tag);
                    }
                    // KaTeX's error span: the source in the error colour.
                    Formula::Error(_) => {
                        builder.text(text, &style.run(self, span.style, style.error), tag);
                    }
                    // MathTex.tsx: the source as inline code.
                    Formula::Internal => {
                        builder.text(text, &style.run(self, span.style | decibell_richtext::Style::CODE, style.color), tag);
                    }
                },
                SpanKind::CodeBlock { .. } | SpanKind::DisplayMath => {}
            }
        }
        if edited {
            builder.text("(edited)", &style.edited(self), EDITED_TAG);
        }
        builder.build()
    }
}

impl RichLayout {
    /// The width the layout was made for.
    pub fn width(&self) -> f32 {
        self.width
    }

    /// The body's height, px: Chromium's, unrounded.
    pub fn height(&self) -> f32 {
        self.height
    }

    /// The first position.
    pub fn start(&self) -> RichPos {
        RichPos::default()
    }

    /// The last position.
    pub fn end(&self) -> RichPos {
        let block = self.blocks.len().saturating_sub(1);
        RichPos {
            block: block as u32,
            offset: self.block_len(block),
            affinity: Affinity::Upstream,
        }
    }

    fn block_len(&self, block: usize) -> u32 {
        match self.blocks.get(block).map(|block| &block.kind) {
            Some(BlockKind::Text { layout, .. }) => layout.paragraph().text().len() as u32,
            Some(BlockKind::Slot(_)) => 1,
            None => 0,
        }
    }

    /// The slots, top to bottom.
    pub fn slots(&self) -> impl Iterator<Item = Slot> + '_ {
        self.blocks.iter().filter_map(|block| match &block.kind {
            BlockKind::Slot(slot) => Some(*slot),
            BlockKind::Text { .. } => None,
        })
    }

    /// The paragraph layouts with their tops, for callers that need line geometry.
    pub fn texts(&self) -> impl Iterator<Item = (f32, &Rc<Layout>)> + '_ {
        self.blocks.iter().filter_map(|block| match &block.kind {
            BlockKind::Text { layout, .. } => Some((block.y, layout)),
            BlockKind::Slot(_) => None,
        })
    }

    /// What is at (`x`, `y`), px from the layout's top-left; the nearest position for a
    /// point outside (a drag that left the text).
    pub fn hit(&self, x: f32, y: f32) -> RichHit {
        let Some(last) = self.blocks.len().checked_sub(1) else {
            return RichHit {
                pos: RichPos::default(),
                span: None,
                slot: None,
            };
        };
        let index = self.blocks.partition_point(|block| block.y + block.height <= y).min(last);
        let block = &self.blocks[index];
        match &block.kind {
            BlockKind::Text { layout, .. } => {
                let hit = layout.hit(x, y - block.y);
                RichHit {
                    pos: RichPos {
                        block: index as u32,
                        offset: hit.pos.offset,
                        affinity: hit.pos.affinity,
                    },
                    span: hit.tag.filter(|tag| *tag != EDITED_TAG),
                    slot: None,
                }
            }
            BlockKind::Slot(slot) => RichHit {
                pos: RichPos {
                    block: index as u32,
                    offset: if y - block.y > block.height / 2. { 1 } else { 0 },
                    affinity: Affinity::Downstream,
                },
                span: Some(slot.span),
                slot: Some(*slot),
            },
        }
    }

    /// The link (its span index) at a point, if any.
    /// The Twemoji drawn at a point (an inline box): its span and its box `[x, y, w, h]` in this
    /// layout's coordinates. Emoji drawn as text (no Twemoji drawing) aren't boxes.
    pub fn emoji_at(&self, x: f32, y: f32) -> Option<(u32, [f32; 4])> {
        self.emoji_boxes().find(|(_, [bx, by, bw, bh])| x >= *bx && x < bx + bw && y >= *by && y < by + bh)
    }

    /// The box of the Twemoji of span `span`, if it is drawn as one.
    pub fn emoji_box(&self, span: u32) -> Option<[f32; 4]> {
        self.emoji_boxes().find(|(tag, _)| *tag == span).map(|(_, bounds)| bounds)
    }

    fn emoji_boxes(&self) -> impl Iterator<Item = (u32, [f32; 4])> + '_ {
        self.blocks.iter().flat_map(|block| {
            let (layout, boxes) = match &block.kind {
                BlockKind::Text { layout, boxes } if !boxes.is_empty() => (Some(layout), &boxes[..]),
                _ => (None, &[][..]),
            };
            layout.into_iter().flat_map(move |layout| {
                layout.boxes().filter_map(move |(tag, [x, y, w, h])| {
                    boxes
                        .iter()
                        .any(|(known, content)| *known == tag && matches!(content, BoxContent::Emoji(_)))
                        .then_some((tag, [x, y + block.y, w, h]))
                })
            })
        })
    }

    pub fn link_at(&self, x: f32, y: f32) -> Option<u32> {
        if self.links.is_empty() {
            return None;
        }
        let span = self.hit(x, y).span?;
        self.links.binary_search(&span).ok().map(|_| span)
    }

    /// The word around a position (a double click).
    pub fn word_at(&self, pos: RichPos) -> (RichPos, RichPos) {
        self.range_in_block(pos, |layout| layout.word_at(pos.offset))
    }

    /// The visual line of a position.
    pub fn line_at(&self, pos: RichPos) -> (RichPos, RichPos) {
        self.range_in_block(pos, |layout| {
            layout.line_range(Pos {
                offset: pos.offset,
                affinity: pos.affinity,
            })
        })
    }

    fn range_in_block(&self, pos: RichPos, range: impl FnOnce(&Layout) -> (u32, u32)) -> (RichPos, RichPos) {
        let (from, to) = match self.blocks.get(pos.block as usize).map(|block| &block.kind) {
            Some(BlockKind::Text { layout, .. }) => range(layout),
            _ => (0, 1),
        };
        let at = |offset, affinity| RichPos {
            block: pos.block,
            offset,
            affinity,
        };
        (at(from, Affinity::Downstream), at(to, Affinity::Upstream))
    }

    /// The position in the document's displayed text, for
    /// `decibell_richtext::plain::as_displayed_range` (copy) and `Document::source_offset`.
    pub fn text_pos(&self, pos: RichPos, document: &Document) -> TextPos {
        let end = TextPos {
            span: document.spans().len(),
            offset: 0,
        };
        match self.blocks.get(pos.block as usize).map(|block| &block.kind) {
            Some(BlockKind::Text { layout, .. }) => match layout.paragraph().locate(pos.offset) {
                Some((tag, _)) if tag == EDITED_TAG => end,
                Some((tag, local)) => TextPos {
                    span: tag as usize,
                    offset: local as usize,
                },
                None => end,
            },
            Some(BlockKind::Slot(slot)) => TextPos {
                span: slot.span as usize + pos.offset.min(1) as usize,
                offset: 0,
            },
            None => end,
        }
    }

    /// The text a selection copies: the message as displayed between two positions.
    pub fn copy_text(&self, from: RichPos, to: RichPos, document: &Document, source: &str) -> String {
        let (from, to) = if from <= to { (from, to) } else { (to, from) };
        decibell_richtext::plain::as_displayed_range(document, source, self.text_pos(from, document), self.text_pos(to, document))
    }

    /// The selection's share of one block, as offsets into its text.
    fn selection_in_block(&self, index: usize, selection: &RichSelection) -> Option<(u32, u32)> {
        match *selection {
            RichSelection::None => None,
            RichSelection::All => Some((0, u32::MAX)),
            RichSelection::Range(a, b) => {
                let (from, to) = if a <= b { (a, b) } else { (b, a) };
                let index = index as u32;
                if index < from.block || index > to.block {
                    return None;
                }
                Some((
                    if index == from.block { from.offset } else { 0 },
                    if index == to.block { to.offset } else { u32::MAX },
                ))
            }
        }
    }

    /// Paints the text blocks with their top-left corner at `origin` (slots are the
    /// caller's elements). Paint phase only; nothing is allocated.
    pub fn paint(&self, origin: Point<Pixels>, paint: &RichPaint, window: &mut Window, cx: &mut App) {
        let engine = Engine::global(cx);
        for (index, block) in self.blocks.iter().enumerate() {
            let BlockKind::Text { layout, boxes } = &block.kind else { continue };
            let block_origin = point(origin.x, origin.y + px(block.y));
            let options = PaintOptions {
                selection: self.selection_in_block(index, &paint.selection),
                selection_background: paint.selection_background,
                selection_color: paint.selection_color,
                underline_tag: paint.hovered_link,
                color: None,
            };
            layout.paint(block_origin, &options, window, cx);
            if boxes.is_empty() {
                continue;
            }
            for (tag, [x, y, width, height]) in layout.boxes() {
                let Some((_, content)) = boxes.iter().find(|(known, _)| *known == tag) else { continue };
                let at = point(block_origin.x + px(x), block_origin.y + px(y));
                match content {
                    BoxContent::Emoji(asset) => {
                        engine.emoji.paint(*asset, Bounds::new(at, size(px(width), px(height))), window, cx);
                    }
                    BoxContent::Math(math) => {
                        let baseline = point(at.x, at.y + px(math.ascent));
                        math.paint(baseline, width, &self.math_colors, window, cx);
                    }
                }
            }
        }
    }
}

/// What varies between two paints of the same rich layout.
#[derive(Clone, Copy, Debug, Default)]
pub struct RichPaint {
    pub selection: RichSelection,
    pub selection_background: Hsla,
    /// The colour of selected text; `None` keeps each run's own.
    pub selection_color: Option<Hsla>,
    /// The link (span index) to underline: the one under the pointer.
    pub hovered_link: Option<u32>,
}

type LinkHandler = Rc<dyn Fn(u32, &MouseUpEvent, &mut Window, &mut App)>;
type SlotBuilder = Box<dyn FnMut(Slot, &mut Window, &mut App) -> AnyElement>;

/// The element that shows a [`RichLayout`]: text, emoji, inline formulas, links that
/// underline under the pointer and can be clicked, and the caller's elements in the slots.
///
/// ```ignore
/// RichText::new(("message", id), layout)
///     .selection(selection_for_this_row, theme.selection_background, Some(theme.selection_text))
///     .on_link(|span, _, window, cx| open(span, window, cx))
///     .slot(|slot, window, cx| code_block(slot, window, cx))
/// ```
pub struct RichText {
    id: ElementId,
    layout: Rc<RichLayout>,
    paint: RichPaint,
    on_link: Option<LinkHandler>,
    slot: Option<SlotBuilder>,
}

impl RichText {
    pub fn new(id: impl Into<ElementId>, layout: Rc<RichLayout>) -> Self {
        Self {
            id: id.into(),
            layout,
            paint: RichPaint::default(),
            on_link: None,
            slot: None,
        }
    }

    /// The selected range to show and its colours.
    pub fn selection(mut self, selection: RichSelection, background: Hsla, color: Option<Hsla>) -> Self {
        self.paint.selection = selection;
        self.paint.selection_background = background;
        self.paint.selection_color = color;
        self
    }

    /// Called when a link is clicked (pressed and released on it) with its span index:
    /// `document.spans()[index]` says whether it is an invite, and its text is the URL.
    pub fn on_link(mut self, handler: impl Fn(u32, &MouseUpEvent, &mut Window, &mut App) + 'static) -> Self {
        self.on_link = Some(Rc::new(handler));
        self
    }

    /// Builds the element of each slot. It is laid out at the slot's width and height.
    pub fn slot(mut self, builder: impl FnMut(Slot, &mut Window, &mut App) -> AnyElement + 'static) -> Self {
        self.slot = Some(Box::new(builder));
        self
    }
}

pub struct RichTextPrepaint {
    hitbox: Hitbox,
    slots: Vec<AnyElement>,
}

impl Element for RichText {
    type RequestLayoutState = ();
    type PrepaintState = RichTextPrepaint;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(&mut self, _: Option<&GlobalElementId>, _: Option<&InspectorElementId>, window: &mut Window, cx: &mut App) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = px(self.layout.width).into();
        style.size.height = px(self.layout.height).into();
        style.flex_shrink = 0.;
        (window.request_layout(style, None, cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> RichTextPrepaint {
        let hitbox = window.insert_hitbox(bounds, HitboxBehavior::Normal);
        let mut slots = Vec::new();
        if let Some(builder) = &mut self.slot {
            for slot in self.layout.slots() {
                let mut element = builder(slot, window, cx);
                let space = size(
                    AvailableSpace::Definite(px(self.layout.width)),
                    AvailableSpace::Definite(px(slot.height)),
                );
                element.prepaint_as_root(point(bounds.origin.x, bounds.origin.y + px(slot.y)), space, window, cx);
                slots.push(element);
            }
        }
        RichTextPrepaint { hitbox, slots }
    }

    fn paint(
        &mut self,
        id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        prepaint: &mut RichTextPrepaint,
        window: &mut Window,
        cx: &mut App,
    ) {
        let layout = &self.layout;
        let origin = bounds.origin;
        let local = move |position: Point<Pixels>| (f32::from(position.x - origin.x), f32::from(position.y - origin.y));
        let link_at = {
            let layout = layout.clone();
            move |position: Point<Pixels>| {
                let (x, y) = local(position);
                layout.link_at(x, y)
            }
        };

        // The link under the pointer is underlined and shows the hand.
        let hovered = if prepaint.hitbox.is_hovered(window) && !layout.links.is_empty() {
            link_at(window.mouse_position())
        } else {
            None
        };
        if hovered.is_some() {
            window.set_cursor_style(CursorStyle::PointingHand, &prepaint.hitbox);
        }
        self.paint.hovered_link = hovered;
        layout.paint(bounds.origin, &self.paint, window, cx);
        for slot in &mut prepaint.slots {
            slot.paint(window, cx);
        }

        if layout.links.is_empty() {
            return;
        }
        // A frame only when the pointer moves onto or off a link.
        let view = window.current_view();
        let hitbox = prepaint.hitbox.clone();
        window.on_mouse_event({
            let link_at = link_at.clone();
            move |event: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                let now = hitbox.is_hovered(window).then(|| link_at(event.position)).flatten();
                if now != hovered {
                    cx.notify(view);
                }
            }
        });

        let (Some(handler), Some(id)) = (self.on_link.clone(), id) else { return };
        // The link the pointer went down on, kept across frames in element state.
        let pressed = window.with_element_state::<Rc<Cell<Option<u32>>>, _>(id, |state, _| {
            let state = state.unwrap_or_default();
            (state.clone(), state)
        });
        let hitbox = prepaint.hitbox.clone();
        window.on_mouse_event({
            let (link_at, pressed, hitbox) = (link_at.clone(), pressed.clone(), hitbox.clone());
            move |event: &MouseDownEvent, phase, window, _| {
                if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                    pressed.set(hitbox.is_hovered(window).then(|| link_at(event.position)).flatten());
                }
            }
        });
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Bubble || event.button != MouseButton::Left {
                return;
            }
            let released = hitbox.is_hovered(window).then(|| link_at(event.position)).flatten();
            if let (Some(pressed), Some(released)) = (pressed.take(), released)
                && pressed == released
            {
                handler(released, event, window, cx);
            }
        });
    }
}
