//! The CSS layout of KaTeX's markup, as Chromium (LayoutNG) performs it.
//!
//! Geometry is integer LayoutUnits (1/64 device pixel: Chromium lays out in device pixels with
//! zoom-for-DSF), converted the way Blink converts each kind of length: `LayoutUnit(float)`
//! truncation for specified lengths, ceiling for text widths, half-leading floored to whole
//! pixels, border widths snapped to device pixels. Glyph positions inside a text run are
//! Chromium's float advances. Only the CSS KaTeX's markup uses is modelled (see `css.rs`).
//!
//! Three passes, each linear in the size of the markup: (1) the styled box arena (cascade once
//! per element), (2) memoized intrinsic (shrink-to-fit) widths, (3) one layout per box.

use std::borrow::Cow;

use crate::dom_tree::{CssProp, CssStyle, HtmlNode, Span, SvgChild, SvgNode, SymbolNode};
use crate::fonts::KatexFont;
use crate::layout::color::Color;
use crate::layout::css::{
    self, Anc, BorderStyle, Computed, Display, Len, Position, TextAlign, VAlign,
};
use crate::layout::text::{advance, ascent_descent, is_ignorable, select_face, shaping_size};
use crate::layout::{Glyph, PathItem};
use crate::parse_error::{Error, Result};

/// LayoutUnits per device pixel.
pub const LU: i32 = 64;

/// `LayoutUnit(float)`: truncation toward zero.
pub fn lu(px: f32) -> i32 {
    (px * LU as f32) as i32
}

/// `LayoutUnit::FromFloatCeil` (text widths).
pub fn lu_ceil_f64(px: f64) -> i32 {
    (px * LU as f64 - 1e-9).ceil() as i32
}

pub fn px(l: i32) -> f64 {
    l as f64 / LU as f64
}

/// Border width as Chromium lays out and paints it: whole device pixels (floor), at least one
/// when non-zero.
fn snap_border(w: f32) -> i32 {
    if w <= 0.0 {
        0
    } else {
        (w.floor().max(1.0) as i32) * LU
    }
}

fn resolve(l: Len, base: i32) -> Option<i32> {
    match l {
        Len::Auto => None,
        Len::Px(p) => Some(lu(p)),
        Len::Pct(p, extra) => Some(((base as f64) * p as f64 / 100.0) as i32 + lu(extra)),
    }
}

/// Like `resolve`, but percentages count as `auto` (intrinsic sizing: cyclic percentages).
fn resolve_px(l: Len) -> Option<i32> {
    match l {
        Len::Px(p) => Some(lu(p)),
        _ => None,
    }
}

fn borders(cs: &Computed) -> [i32; 4] {
    let b = |i: usize| {
        if cs.border[i].style != BorderStyle::None {
            snap_border(cs.border[i].width)
        } else {
            0
        }
    };
    [b(0), b(1), b(2), b(3)]
}

fn hor(cs: &Computed) -> i32 {
    let b = borders(cs);
    b[1] + b[3] + lu(cs.padding[1]) + lu(cs.padding[3])
}

fn ver(cs: &Computed) -> i32 {
    let b = borders(cs);
    b[0] + b[2] + lu(cs.padding[0]) + lu(cs.padding[2])
}

/// A laid-out box: border-box size, baseline, and its painting relative to its border-box
/// top-left corner.
#[derive(Default, Clone)]
pub struct Frag {
    pub w: i32,
    pub h: i32,
    /// Baseline from the border-box top (`None`: no line box inside).
    pub baseline: Option<i32>,
    /// Height of the in-flow content (before an explicit `height` applies).
    pub content_h: i32,
    pub ops: Vec<Op>,
    /// Absolutely positioned descendants still looking for a positioned ancestor (static
    /// positions relative to this fragment).
    pending: Vec<Pending>,
}

/// A painting operation relative to the owning fragment's origin.
#[derive(Clone)]
pub enum Op {
    /// Glyph pen position in device px.
    Glyph {
        x: f64,
        y: f64,
        g: Glyph,
    },
    Rect {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
        color: Color,
        dashed: bool,
    },
    Path {
        x: i32,
        y: i32,
        p: Box<PathItem>,
    },
    /// Translate the ops that follow until `Pop` (device px).
    Push {
        dx: f64,
        dy: f64,
    },
    Pop,
    /// Clip the ops that follow until `Unclip` to this rect (overflow: hidden).
    Clip {
        x: i32,
        y: i32,
        w: i32,
        h: i32,
    },
    Unclip,
}

impl Frag {
    fn place(&mut self, child: Frag, x: i32, y: i32) {
        for p in child.pending {
            self.pending.push(Pending {
                node: p.node,
                sx: p.sx + x,
                sy: p.sy + y,
            });
        }
        if child.ops.is_empty() {
            return;
        }
        self.ops.push(Op::Push {
            dx: px(x),
            dy: px(y),
        });
        self.ops.extend(child.ops);
        self.ops.push(Op::Pop);
    }
}

/// Ancestor facts the cascade's combinator selectors need.
#[derive(Clone, Copy, Default)]
struct Flags {
    in_mfrac: bool,
    in_overline: bool,
    in_underline: bool,
    in_accent: bool,
    in_mtable: bool,
    in_delim_mult: bool,
}

impl Flags {
    fn with(self, classes: &[&str]) -> Flags {
        let h = |c: &str| classes.contains(&c);
        Flags {
            in_mfrac: self.in_mfrac || h("mfrac"),
            in_overline: self.in_overline || h("katex-overline"),
            in_underline: self.in_underline || h("katex-underline"),
            in_accent: self.in_accent || h("katex-accent"),
            in_mtable: self.in_mtable || h("mtable"),
            in_delim_mult: self.in_delim_mult || (h("delimsizing") && h("mult")),
        }
    }
}

enum Kind<'a> {
    Elem,
    Text(Cow<'a, str>),
    Svg(&'a SvgNode),
}

/// One styled box in the arena.
struct Node<'a> {
    kind: Kind<'a>,
    /// The element's computed style (a text node carries its parent's).
    cs: Computed,
    parent: u32,
    children: Vec<u32>,
}

/// An out-of-flow (absolutely positioned) box waiting for its containing block.
#[derive(Clone)]
struct Pending {
    node: u32,
    /// Static position relative to the box being built (LU).
    sx: i32,
    sy: i32,
}

/// One item on a line before vertical placement.
enum LineItem {
    Atomic {
        frag: Frag,
        x: i32,
        ascent: i32,
        descent: i32,
        shift: i32,
        rel: (i32, i32),
    },
    Text {
        glyphs: Vec<(f64, Glyph)>,
        x: i32,
        shift: i32,
        rel: (i32, i32),
    },
    /// An inline box's own metrics (it takes part in the line height).
    Strut {
        ascent: i32,
        descent: i32,
        shift: i32,
    },
    /// An inline element's border / background over its content area.
    Deco {
        x0: i32,
        x1: i32,
        top: i32,
        bottom: i32,
        shift: i32,
        rel: (i32, i32),
        node: u32,
    },
    Abs {
        node: u32,
        x: i32,
        rel: (i32, i32),
    },
}

struct Line {
    width: i32,
    items: Vec<LineItem>,
    has_content: bool,
}

impl Line {
    fn new() -> Line {
        Line {
            width: 0,
            items: Vec::new(),
            has_content: false,
        }
    }
}

enum Segment {
    Run(Vec<u32>),
    Block(u32),
}

/// A laid-out line of the formula (LU, relative to the formula top).
pub struct LineBox {
    pub top: i32,
    pub ascent: i32,
    pub descent: i32,
    pub width: i32,
    pub frag: Frag,
    /// (x0, x1) of each `.katex-base` on the line.
    pub bases: Vec<(i32, i32)>,
}

#[derive(Default)]
pub struct FormulaLayout {
    pub lines: Vec<LineBox>,
    pub tag: Option<Frag>,
    pub first_baseline: i32,
}

/// The style a `SymbolNode` element carries (its italic correction is a `margin-right`
/// written before its other styles).
fn symbol_style(s: &SymbolNode) -> CssStyle {
    let mut st = CssStyle::new();
    if s.italic > 0.0 {
        st.0.push((CssProp::MarginRight, crate::units::make_em(s.italic)));
    }
    st.0.extend(s.style.0.iter().cloned());
    st
}

fn symbol_is_element(s: &SymbolNode) -> bool {
    !s.classes.is_empty() || s.italic > 0.0 || !s.style.is_empty()
}

/// Calibrated against Chromium: the baseline of a two-row `.vlist-t` (one with a depth row) is
/// one LayoutUnit below the first row's bottom.
pub const TWO_ROW_FUDGE: i32 = 0;

pub struct Engine<'a> {
    pub dsf: f32,
    nodes: Vec<Node<'a>>,
    intrinsic: Vec<Option<i32>>,
    eqn: u32,
    /// CSS-generated text nodes (`.eqn-num::before`).
    generated: Vec<u32>,
}

struct Ctx<'c> {
    parent_classes: &'c [&'static str],
    grand: &'c [&'static str],
    flags: Flags,
    katex_html: bool,
}

impl<'a> Engine<'a> {
    /// Builds the styled arena for `.katex-html` (`html`) under the root style; returns the
    /// engine and the index of the `.katex-html` node.
    pub fn new(
        html: &'a Span,
        root: &Computed,
        display: bool,
        dsf: f32,
    ) -> Result<(Engine<'a>, u32)> {
        let mut e = Engine {
            dsf,
            nodes: Vec::new(),
            intrinsic: Vec::new(),
            eqn: 0,
            generated: Vec::new(),
        };
        let anc = Anc {
            display_html: display,
            ..Default::default()
        };
        let cs = css::compute(&html.classes, &html.style, root, &anc, false);
        e.nodes.push(Node {
            kind: Kind::Elem,
            cs,
            parent: 0,
            children: Vec::new(),
        });
        let ctx = Ctx {
            parent_classes: &html.classes,
            grand: &[],
            flags: Flags::default(),
            katex_html: true,
        };
        let kids = e.build_children(&html.children, 0, &ctx)?;
        e.nodes[0].children = kids;
        e.intrinsic = vec![None; e.nodes.len()];
        Ok((e, 0))
    }

    fn push(&mut self, kind: Kind<'a>, cs: Computed, parent: u32) -> u32 {
        self.nodes.push(Node {
            kind,
            cs,
            parent,
            children: Vec::new(),
        });
        (self.nodes.len() - 1) as u32
    }

    fn build_children(
        &mut self,
        children: &'a [HtmlNode],
        parent: u32,
        ctx: &Ctx,
    ) -> Result<Vec<u32>> {
        let _d = LayoutDepth::enter()?;
        let mut out = Vec::with_capacity(children.len());
        for c in children {
            match c {
                HtmlNode::Fragment(f) => {
                    out.extend(self.build_children(&f.children, parent, ctx)?)
                }
                HtmlNode::Symbol(s) if !symbol_is_element(s) => {
                    // Adjacent text in the markup is one DOM text node (one shaped run).
                    if let Some(&last) = out.last() {
                        let generated = self.generated_text(last);
                        if let Kind::Text(t) = &mut self.nodes[last as usize].kind
                            && !generated
                        {
                            let mut joined = t.to_string();
                            joined.push_str(&s.text);
                            *t = Cow::Owned(joined);
                            continue;
                        }
                    }
                    let cs = self.nodes[parent as usize].cs.clone();
                    out.push(self.push(Kind::Text(Cow::Borrowed(&s.text)), cs, parent));
                }
                HtmlNode::Symbol(s) => {
                    let anc = self.anc(ctx);
                    let st = symbol_style(s);
                    let cs = css::compute(
                        &s.classes,
                        &st,
                        &self.nodes[parent as usize].cs,
                        &anc,
                        false,
                    );
                    let id = self.push(Kind::Elem, cs.clone(), parent);
                    let mut kids = Vec::new();
                    if cs.eqn_num {
                        kids.push(self.eqn_text(&cs, id));
                    }
                    kids.push(self.push(Kind::Text(Cow::Borrowed(&s.text)), cs, id));
                    self.nodes[id as usize].children = kids;
                    out.push(id);
                }
                HtmlNode::Svg(svg) => {
                    let anc = self.anc(ctx);
                    let cs = css::compute(
                        &[],
                        &CssStyle::new(),
                        &self.nodes[parent as usize].cs,
                        &anc,
                        true,
                    );
                    out.push(self.push(Kind::Svg(svg), cs, parent));
                }
                HtmlNode::Span(sp) | HtmlNode::Anchor(sp) => {
                    let anc = self.anc(ctx);
                    let cs = css::compute(
                        &sp.classes,
                        &sp.style,
                        &self.nodes[parent as usize].cs,
                        &anc,
                        false,
                    );
                    let id = self.push(Kind::Elem, cs.clone(), parent);
                    let mut kids = Vec::new();
                    if cs.eqn_num {
                        kids.push(self.eqn_text(&cs, id));
                    }
                    let sub = Ctx {
                        parent_classes: &sp.classes,
                        grand: ctx.parent_classes,
                        flags: ctx.flags.with(&sp.classes),
                        katex_html: sp.has_class("katex-html"),
                    };
                    kids.extend(self.build_children(&sp.children, id, &sub)?);
                    self.nodes[id as usize].children = kids;
                    out.push(id);
                }
            }
        }
        Ok(out)
    }

    /// `.eqn-num::before { content: "(" counter(katexEqnNo) ")" }`. Chromium's counter runs
    /// across the whole page; here it restarts at 1 for every formula.
    fn eqn_text(&mut self, cs: &Computed, parent: u32) -> u32 {
        self.eqn += 1;
        let t = format!("({})", self.eqn);
        let id = self.push(Kind::Text(Cow::Owned(t)), cs.clone(), parent);
        self.generated.push(id);
        id
    }

    fn generated_text(&self, i: u32) -> bool {
        self.generated.contains(&i)
    }

    fn anc<'c>(&self, ctx: &'c Ctx) -> Anc<'c> {
        let f = ctx.flags;
        Anc {
            parent: ctx.parent_classes,
            grandparent: ctx.grand,
            in_mfrac: f.in_mfrac,
            in_overline: f.in_overline,
            in_underline: f.in_underline,
            in_accent: f.in_accent,
            in_mtable: f.in_mtable,
            in_delim_mult: f.in_delim_mult,
            parent_is_katex_html: ctx.katex_html,
            display_html: false,
        }
    }

    fn cs(&self, i: u32) -> &Computed {
        &self.nodes[i as usize].cs
    }

    fn is_elem(&self, i: u32) -> bool {
        matches!(self.nodes[i as usize].kind, Kind::Elem)
    }

    fn is_out_of_flow(&self, i: u32) -> bool {
        match self.nodes[i as usize].kind {
            Kind::Svg(_) => true,
            Kind::Elem => self.cs(i).position == Position::Absolute,
            Kind::Text(_) => false,
        }
    }

    fn is_block_level(&self, i: u32) -> bool {
        self.is_elem(i) && !self.is_out_of_flow(i) && self.cs(i).display == Display::Block
    }

    /// Line-box metrics of an inline box with style `cs`: (ascent, descent) in LU with the
    /// half-leading of `line-height` (LayoutNG `FontHeight::AddLeading`: the half-leading is
    /// floored to whole pixels and added to the ascent; the descent takes the rest).
    fn strut(&self, cs: &Computed) -> (i32, i32) {
        let face = select_face(cs.inh.family, cs.inh.bold, cs.inh.italic);
        let (a, d) = ascent_descent(face.font, cs.inh.font_px);
        let line_height = lu(cs.inh.line_height * cs.inh.font_px);
        let half = (line_height - (a + d) * LU) / 2;
        let ascent = a * LU + half.div_euclid(LU) * LU;
        (ascent, line_height - ascent)
    }

    /// Glyphs of `text` in style `cs` from pen x 0 on the baseline, and the run's float width.
    fn shape(&self, text: &str, cs: &Computed) -> (Vec<(f64, Glyph)>, f64) {
        let face = select_face(cs.inh.family, cs.inh.bold, cs.inh.italic);
        let size = shaping_size(cs.inh.font_px);
        let mut x = 0.0f64;
        let mut out = Vec::with_capacity(text.len());
        for c in text.chars() {
            if is_ignorable(c) {
                continue;
            }
            let (adv, fallback) = match advance(face.font, c, size) {
                Some(a) => (a, false),
                // Not in the face: Chromium uses a system font; KaTeX_Main's advance (or half
                // an em) is the best guess, and the painter must fall back too.
                None => (
                    advance(KatexFont::MainRegular, c, size).unwrap_or(size * 0.5),
                    true,
                ),
            };
            out.push((
                x,
                Glyph {
                    ch: c,
                    font: face.font,
                    x: 0.0,
                    y: 0.0,
                    size: (size / self.dsf as f64) as f32,
                    color: cs.inh.color,
                    synthetic_bold: face.synthetic_bold,
                    synthetic_italic: face.synthetic_italic,
                    fallback,
                    shadow: false,
                    generated: false,
                },
            ));
            x += adv;
        }
        (out, x)
    }

    fn text_width(&self, i: u32) -> i32 {
        let Kind::Text(t) = &self.nodes[i as usize].kind else {
            return 0;
        };
        let (_, w) = self.shape(t, self.cs(i));
        lu_ceil_f64(w)
    }

    // ---------------------------------------------------------------- intrinsic widths

    /// Max-content width of a run of inline-level children: (width, has a line box).
    fn inline_width(&mut self, kids: &[u32]) -> Result<(i32, bool)> {
        let mut w = 0;
        let mut content = false;
        for &k in kids {
            if self.is_out_of_flow(k) {
                continue;
            }
            match &self.nodes[k as usize].kind {
                Kind::Text(_) => {
                    w += self.text_width(k);
                    content = true;
                }
                Kind::Svg(_) => {}
                Kind::Elem => {
                    let cs = self.cs(k).clone();
                    let ml = resolve_px(cs.margin[3]).unwrap_or(0);
                    let mr = resolve_px(cs.margin[1]).unwrap_or(0);
                    if cs.display == Display::Inline {
                        let h = hor(&cs);
                        let kids = self.nodes[k as usize].children.clone();
                        let (iw, ic) = self.inline_width(&kids)?;
                        w += ml + h + iw + mr;
                        content |= ic || ml != 0 || mr != 0 || h != 0;
                    } else {
                        w += ml + self.intrinsic(k)? + mr;
                        content = true;
                    }
                }
            }
        }
        Ok((w, content))
    }

    fn segments(&self, i: u32) -> Vec<Segment> {
        let mut segs = vec![Segment::Run(Vec::new())];
        for &k in &self.nodes[i as usize].children {
            if self.is_block_level(k) {
                segs.push(Segment::Block(k));
                segs.push(Segment::Run(Vec::new()));
            } else if let Some(Segment::Run(r)) = segs.last_mut() {
                r.push(k);
            }
        }
        segs
    }

    /// Max-content width of a block container's content box.
    fn content_intrinsic(&mut self, i: u32) -> Result<i32> {
        let mut w = 0;
        for seg in self.segments(i) {
            match seg {
                Segment::Run(r) => w = w.max(self.inline_width(&r)?.0),
                Segment::Block(b) => {
                    let cs = self.cs(b).clone();
                    let ml = resolve_px(cs.margin[3]).unwrap_or(0);
                    let mr = resolve_px(cs.margin[1]).unwrap_or(0);
                    w = w.max(self.intrinsic(b)? + ml + mr);
                }
            }
        }
        Ok(w)
    }

    /// Clamps a content-box width by min-/max-width (percentages resolved against `cb`, or
    /// ignored when `cb` is None).
    fn clamp_content(cs: &Computed, mut content: i32, cb: Option<i32>) -> i32 {
        let h = hor(cs);
        let r = |l: Len| match cb {
            Some(c) => resolve(l, c),
            None => resolve_px(l),
        };
        if let Some(maxw) = r(cs.max_width) {
            content = content.min(if cs.border_box { maxw - h } else { maxw });
        }
        if let Some(minw) = r(cs.min_width) {
            content = content.max(if cs.border_box { minw - h } else { minw });
        }
        content.max(0)
    }

    /// Shrink-to-fit border-box width of an atomic or block box (memoized).
    fn intrinsic(&mut self, i: u32) -> Result<i32> {
        if let Some(w) = self.intrinsic[i as usize] {
            return Ok(w);
        }
        let _d = LayoutDepth::enter()?;
        let cs = self.cs(i).clone();
        let w = match cs.display {
            Display::InlineTable => self.table_columns(i)?.iter().sum(),
            Display::InlineFlexRow | Display::InlineFlexColumn => self.flex_intrinsic(i)?,
            Display::TableCell => {
                let c = self.content_intrinsic(i)?;
                let mut w = c + hor(&cs);
                if let Some(x) = resolve_px(cs.width) {
                    w = w.max(x);
                }
                if let Some(x) = resolve_px(cs.min_width) {
                    w = w.max(x);
                }
                w
            }
            _ => {
                let h = hor(&cs);
                let content = match resolve_px(cs.width) {
                    Some(w) => {
                        if cs.border_box {
                            w - h
                        } else {
                            w
                        }
                    }
                    None => self.content_intrinsic(i)?,
                };
                Self::clamp_content(&cs, content, None) + h
            }
        };
        self.intrinsic[i as usize] = Some(w);
        Ok(w)
    }

    fn elem_children(&self, i: u32) -> Vec<u32> {
        self.nodes[i as usize]
            .children
            .iter()
            .copied()
            .filter(|&k| self.is_elem(k))
            .collect()
    }

    /// Column widths of a `.vlist-t` (auto table layout over max-content cell widths).
    fn table_columns(&mut self, i: u32) -> Result<Vec<i32>> {
        let mut cols: Vec<i32> = Vec::new();
        for row in self.elem_children(i) {
            for (ci, cell) in self.elem_children(row).into_iter().enumerate() {
                let w = self.intrinsic(cell)?;
                if cols.len() <= ci {
                    cols.push(0);
                }
                cols[ci] = cols[ci].max(w);
            }
        }
        Ok(cols)
    }

    fn flex_intrinsic(&mut self, i: u32) -> Result<i32> {
        let cs = self.cs(i).clone();
        if let Some(w) = resolve_px(cs.width) {
            return Ok(w);
        }
        let column = cs.display == Display::InlineFlexColumn;
        let mut w = 0;
        for k in self.elem_children(i) {
            let iw = self.intrinsic(k)?;
            w = if column { w.max(iw) } else { w + iw };
        }
        if let Some(m) = resolve_px(cs.max_width) {
            w = w.min(m);
        }
        Ok(w)
    }

    // ---------------------------------------------------------------- layout

    /// `position: relative` offsets. Chromium rounds these to the LayoutUnit grid (measured:
    /// two boxes in one table cell are only consistent with rounding).
    fn rel_offset(cs: &Computed, cb_w: i32, cb_h: i32) -> (i32, i32) {
        if cs.position != Position::Relative {
            return (0, 0);
        }
        let r = |l: Len, base: i32| match l {
            Len::Px(p) => Some((p * LU as f32).round() as i32),
            other => resolve(other, base),
        };
        let dx = match (r(cs.offsets[3], cb_w), r(cs.offsets[1], cb_w)) {
            (Some(l), _) => l,
            (None, Some(rt)) => -rt,
            _ => 0,
        };
        let dy = match (r(cs.offsets[0], cb_h), r(cs.offsets[2], cb_h)) {
            (Some(t), _) => t,
            (None, Some(b)) => -b,
            _ => 0,
        };
        (dx, dy)
    }

    /// Lays out inline-level `kids` onto `line` (container content width `cb_w`).
    fn line_items(
        &mut self,
        kids: &[u32],
        line: &mut Line,
        shift: i32,
        rel: (i32, i32),
        cb_w: i32,
    ) -> Result<()> {
        for &k in kids {
            if self.is_out_of_flow(k) {
                line.items.push(LineItem::Abs {
                    node: k,
                    x: line.width,
                    rel,
                });
                continue;
            }
            match &self.nodes[k as usize].kind {
                Kind::Text(t) => {
                    let generated = self.generated_text(k);
                    let (mut glyphs, w) = self.shape(t, self.cs(k));
                    for g in glyphs.iter_mut() {
                        g.1.generated = generated;
                    }
                    line.has_content = true;
                    let x = line.width;
                    line.width += lu_ceil_f64(w);
                    if let Some((dx, dy)) = self.cs(k).text_shadow {
                        let shadow = glyphs
                            .iter()
                            .map(|(gx, g)| {
                                let mut g = *g;
                                g.shadow = true;
                                (*gx + dx as f64, g)
                            })
                            .collect();
                        line.items.push(LineItem::Text {
                            glyphs: shadow,
                            x,
                            shift: shift - lu(dy),
                            rel,
                        });
                    }
                    line.items.push(LineItem::Text {
                        glyphs,
                        x,
                        shift,
                        rel,
                    });
                }
                Kind::Svg(_) => {}
                Kind::Elem => {
                    let cs = self.cs(k).clone();
                    if cs.display == Display::Inline {
                        self.inline_element(k, &cs, line, shift, rel, cb_w)?;
                    } else {
                        let frag = self.layout_atomic(k, cb_w)?;
                        self.push_atomic(frag, &cs, line, shift, rel, cb_w);
                    }
                }
            }
        }
        Ok(())
    }

    fn inline_element(
        &mut self,
        k: u32,
        cs: &Computed,
        line: &mut Line,
        shift: i32,
        rel: (i32, i32),
        cb_w: i32,
    ) -> Result<()> {
        let _d = LayoutDepth::enter()?;
        let shift = match cs.valign {
            VAlign::Px(v) => shift + lu(v),
            _ => shift,
        };
        let r = Self::rel_offset(cs, cb_w, 0);
        let rel = (rel.0 + r.0, rel.1 + r.1);
        let ml = resolve(cs.margin[3], cb_w).unwrap_or(0);
        let mr = resolve(cs.margin[1], cb_w).unwrap_or(0);
        let [_, br, _, bl] = borders(cs);
        line.width += ml;
        let x0 = line.width;
        line.width += bl + lu(cs.padding[3]);
        let (a, d) = self.strut(cs);
        line.items.push(LineItem::Strut {
            ascent: a,
            descent: d,
            shift,
        });
        if ml != 0 || mr != 0 || bl != 0 || br != 0 || cs.padding[1] != 0.0 || cs.padding[3] != 0.0
        {
            line.has_content = true;
        }
        let kids = self.nodes[k as usize].children.clone();
        self.line_items(&kids, line, shift, rel, cb_w)?;
        line.width += lu(cs.padding[1]) + br;
        let x1 = line.width;
        line.width += mr;
        if cs.background.is_some() || borders(cs).iter().any(|b| *b > 0) {
            let face = select_face(cs.inh.family, cs.inh.bold, cs.inh.italic);
            let (a, d) = ascent_descent(face.font, cs.inh.font_px);
            line.items.push(LineItem::Deco {
                x0,
                x1,
                top: a * LU,
                bottom: d * LU,
                shift,
                rel,
                node: k,
            });
        }
        Ok(())
    }

    fn push_atomic(
        &mut self,
        frag: Frag,
        cs: &Computed,
        line: &mut Line,
        shift: i32,
        rel: (i32, i32),
        cb_w: i32,
    ) {
        let mt = resolve(cs.margin[0], cb_w).unwrap_or(0);
        let mb = resolve(cs.margin[2], cb_w).unwrap_or(0);
        let ml = resolve(cs.margin[3], cb_w).unwrap_or(0);
        let mr = resolve(cs.margin[1], cb_w).unwrap_or(0);
        // Baseline: the last line box's, else (or with overflow other than visible) the
        // bottom margin edge.
        let base = if cs.overflow_hidden {
            None
        } else {
            frag.baseline
        };
        let (ascent, descent) = match base {
            Some(b) => (b, frag.h - b),
            None => (frag.h, 0),
        };
        let own = match cs.valign {
            VAlign::Px(v) => lu(v),
            _ => 0,
        };
        let r = Self::rel_offset(cs, cb_w, 0);
        line.width += ml;
        let x = line.width;
        line.width += frag.w + mr;
        line.has_content = true;
        line.items.push(LineItem::Atomic {
            frag,
            x,
            ascent: ascent + mt,
            descent: descent + mb,
            shift: shift + own,
            rel: (rel.0 + r.0, rel.1 + r.1 + mt),
        });
    }

    /// Line box metrics: (ascent, descent) over all items, including the container's strut.
    fn line_metrics(line: &Line, strut: (i32, i32)) -> (i32, i32) {
        let (mut asc, mut desc) = strut;
        for it in &line.items {
            match it {
                LineItem::Atomic {
                    ascent,
                    descent,
                    shift,
                    ..
                }
                | LineItem::Strut {
                    ascent,
                    descent,
                    shift,
                } => {
                    asc = asc.max(ascent + shift);
                    desc = desc.max(descent - shift);
                }
                _ => {}
            }
        }
        (asc, desc)
    }

    /// Places a line's items with its top at `line_top`; returns the baseline y.
    fn emit_line(
        &self,
        line: Line,
        frag: &mut Frag,
        x_off: i32,
        line_top: i32,
        asc: i32,
        pending: &mut Vec<Pending>,
    ) -> i32 {
        let baseline = line_top + asc;
        for it in line.items {
            match it {
                LineItem::Atomic {
                    frag: f,
                    x,
                    ascent,
                    shift,
                    rel,
                    ..
                } => {
                    // `rel.1` already includes the margin-top (the border box starts below it).
                    let top = baseline - shift - ascent;
                    frag.place(f, x_off + x + rel.0, top + rel.1);
                }
                LineItem::Text {
                    glyphs,
                    x,
                    shift,
                    rel,
                } => {
                    let bx = px(x_off + x + rel.0);
                    let by = px(baseline - shift + rel.1);
                    for (gx, g) in glyphs {
                        frag.ops.push(Op::Glyph {
                            x: bx + gx,
                            y: by,
                            g,
                        });
                    }
                }
                LineItem::Deco {
                    x0,
                    x1,
                    top,
                    bottom,
                    shift,
                    rel,
                    node,
                } => {
                    let y0 = baseline - shift - top + rel.1;
                    let y1 = baseline - shift + bottom + rel.1;
                    let cs = self.cs(node).clone();
                    paint_box(frag, &cs, x_off + x0 + rel.0, y0, x1 - x0, y1 - y0);
                }
                LineItem::Strut { .. } => {}
                LineItem::Abs { node, x, rel } => {
                    pending.push(Pending {
                        node,
                        sx: x_off + x + rel.0,
                        sy: line_top + rel.1,
                    });
                }
            }
        }
        baseline
    }

    fn layout_atomic(&mut self, i: u32, cb_w: i32) -> Result<Frag> {
        match self.cs(i).display {
            Display::InlineTable => self.layout_table(i),
            Display::InlineFlexRow | Display::InlineFlexColumn => self.layout_flex(i, cb_w),
            _ => self.layout_block(i, cb_w, None, 0),
        }
    }

    /// A block container (inline-block, block, table cell, flex item, abs box).
    /// `forced`: a border-box width imposed by the parent. `cb_h`: for percentage heights.
    fn layout_block(&mut self, i: u32, cb_w: i32, forced: Option<i32>, cb_h: i32) -> Result<Frag> {
        let _d = LayoutDepth::enter()?;
        let cs = self.cs(i).clone();
        let [bt, br, bb, bl] = borders(&cs);
        let (pt, pl) = (lu(cs.padding[0]), lu(cs.padding[3]));
        let h_ = hor(&cs);
        let v_ = ver(&cs);
        let content_w = match (forced, resolve(cs.width, cb_w)) {
            (Some(f), _) => f - h_,
            (None, Some(w)) => {
                Self::clamp_content(&cs, if cs.border_box { w - h_ } else { w }, Some(cb_w))
            }
            (None, None) => {
                let c = self.content_intrinsic(i)?;
                Self::clamp_content(&cs, c, Some(cb_w))
            }
        };
        let content_w = content_w.max(0);
        let mut frag = Frag {
            w: content_w + h_,
            ..Default::default()
        };
        let mut pending: Vec<Pending> = Vec::new();
        let mut y = bt + pt;
        let positioned = cs.position != Position::Static;
        let mut last_baseline = None;
        let strut = self.strut(&cs);
        for seg in self.segments(i) {
            match seg {
                Segment::Run(kids) => {
                    let mut line = Line::new();
                    self.line_items(&kids, &mut line, 0, (0, 0), content_w)?;
                    let any_abs = line
                        .items
                        .iter()
                        .any(|it| matches!(it, LineItem::Abs { .. }));
                    if !line.has_content && !any_abs {
                        continue;
                    }
                    let (asc, desc) = Self::line_metrics(&line, strut);
                    let free = content_w - line.width;
                    // Only out-of-flow boxes: no line box; their static position is the start.
                    let off = if !line.has_content {
                        0
                    } else {
                        match cs.inh.text_align {
                            TextAlign::Left => 0,
                            TextAlign::Center => {
                                if free > 0 {
                                    free / 2
                                } else {
                                    0
                                }
                            }
                            TextAlign::Right => {
                                if free > 0 {
                                    free
                                } else {
                                    0
                                }
                            }
                        }
                    };
                    let has_content = line.has_content;
                    let b = self.emit_line(line, &mut frag, bl + pl + off, y, asc, &mut pending);
                    if has_content {
                        last_baseline = Some(b);
                        y += asc + desc;
                    }
                }
                Segment::Block(b) => {
                    let bcs = self.cs(b).clone();
                    let ml = resolve(bcs.margin[3], content_w).unwrap_or(0);
                    let mr = resolve(bcs.margin[1], content_w).unwrap_or(0);
                    let mt = resolve(bcs.margin[0], content_w).unwrap_or(0);
                    let mb = resolve(bcs.margin[2], content_w).unwrap_or(0);
                    let bw = match resolve(bcs.width, content_w) {
                        Some(w) => {
                            if bcs.border_box {
                                w
                            } else {
                                w + hor(&bcs)
                            }
                        }
                        None => content_w - ml - mr,
                    };
                    let f = self.layout_block(b, content_w, Some(bw), 0)?;
                    let r = Self::rel_offset(&bcs, content_w, 0);
                    y += mt;
                    let h = f.h;
                    if let Some(fb) = f.baseline {
                        last_baseline = Some(y + fb);
                    }
                    frag.place(f, bl + pl + ml + r.0, y + r.1);
                    y += h + mb;
                }
            }
        }
        let content_h = y - (bt + pt);
        let mut ch = match resolve(cs.height, cb_h) {
            Some(h) => {
                if cs.border_box {
                    h - v_
                } else {
                    h
                }
            }
            None => content_h,
        };
        if let Some(minh) = resolve(cs.min_height, cb_h) {
            ch = ch.max(minh);
        }
        frag.h = ch.max(0) + v_;
        frag.content_h = content_h;
        frag.baseline = last_baseline;
        // Own background / borders under the content.
        let mut own = Frag::default();
        paint_box(&mut own, &cs, 0, 0, frag.w, frag.h);
        if !own.ops.is_empty() {
            own.ops.append(&mut frag.ops);
            frag.ops = own.ops;
        }
        pending.append(&mut frag.pending);
        if positioned {
            self.resolve_pending(&mut frag, pending, [bt, br, bb, bl])?;
        } else {
            frag.pending = pending;
        }
        if cs.overflow_hidden {
            let mut ops = vec![Op::Clip {
                x: 0,
                y: 0,
                w: frag.w,
                h: frag.h,
            }];
            ops.append(&mut frag.ops);
            ops.push(Op::Unclip);
            frag.ops = ops;
        }
        Ok(frag)
    }

    /// Lays out absolutely positioned boxes against `frag`'s padding box (their containing
    /// block in KaTeX's markup is the positioned parent).
    fn resolve_pending(
        &mut self,
        frag: &mut Frag,
        pending: Vec<Pending>,
        b: [i32; 4],
    ) -> Result<()> {
        let [bt, br, bb, bl] = b;
        for p in pending {
            let cb_w = (frag.w - bl - br).max(0);
            let cb_h = (frag.h - bt - bb).max(0);
            if let Kind::Svg(svg) = self.nodes[p.node as usize].kind {
                // `.katex svg`: width 100%, height inherited from the parent element.
                let parent = self.nodes[p.node as usize].parent;
                let h = match self.cs(parent).height {
                    Len::Px(v) => lu(v),
                    _ => cb_h,
                };
                let cs = self.cs(p.node).clone();
                // An inline `style="width:…"` (staticSvg) overrides `.katex svg { width: 100% }`.
                let w = svg
                    .attr("style")
                    .and_then(|st| st.split(';').find_map(|d| d.trim().strip_prefix("width:")))
                    .and_then(|v| v.trim().strip_suffix("em"))
                    .and_then(|v| v.parse::<f64>().ok())
                    .map(|em| lu(cs.em(em)))
                    .unwrap_or(cb_w);
                let mut item = svg_item(svg, px(w), px(h), self.dsf);
                item.color = cs.inh.color;
                for l in item.lines.iter_mut() {
                    l[4] *= cs.inh.font_px / self.dsf;
                }
                frag.ops.push(Op::Path {
                    x: p.sx,
                    y: p.sy,
                    p: Box::new(item),
                });
                continue;
            }
            let cs = self.cs(p.node).clone();
            let forced =
                resolve(cs.width, cb_w).map(|w| if cs.border_box { w } else { w + hor(&cs) });
            let f = self.layout_block(p.node, cb_w, forced, cb_h)?;
            let x = match (resolve(cs.offsets[3], cb_w), resolve(cs.offsets[1], cb_w)) {
                (Some(l), _) => bl + l,
                (None, Some(r)) => bl + cb_w - r - f.w,
                _ => p.sx,
            };
            let y = match (resolve(cs.offsets[0], cb_h), resolve(cs.offsets[2], cb_h)) {
                (Some(t), _) => bt + t,
                (None, Some(b)) => bt + cb_h - b - f.h,
                _ => p.sy,
            };
            frag.place(f, x, y);
        }
        Ok(())
    }

    /// `.vlist-t` (inline-table): rows of bottom-aligned `.vlist` / `.vlist-s` cells.
    fn layout_table(&mut self, i: u32) -> Result<Frag> {
        let _d = LayoutDepth::enter()?;
        let cols = self.table_columns(i)?;
        let rows = self.elem_children(i);
        let mut frag = Frag {
            w: cols.iter().sum(),
            ..Default::default()
        };
        let mut y = 0;
        let mut baseline = None;
        for (ri, &row) in rows.iter().enumerate() {
            let cells = self.elem_children(row);
            let mut laid = Vec::with_capacity(cells.len());
            let mut row_h = 0;
            for (ci, &cell) in cells.iter().enumerate() {
                let w = cols.get(ci).copied().unwrap_or(0);
                let f = self.layout_block(cell, w, Some(w), 0)?;
                row_h = row_h.max(f.h);
                laid.push(f);
            }
            let mut x = 0;
            for (ci, f) in laid.into_iter().enumerate() {
                // vertical-align: bottom: the content block sits at the row bottom.
                let dy = row_h - f.content_h;
                let wrap = Frag {
                    ops: f.ops,
                    pending: f.pending,
                    ..Default::default()
                };
                frag.place(wrap, x, y + dy);
                x += cols.get(ci).copied().unwrap_or(0);
            }
            y += row_h;
            if ri == 0 {
                baseline = Some(y);
            }
        }
        frag.h = y;
        frag.content_h = y;
        frag.baseline = baseline.map(|b| {
            if rows.len() == 2 {
                b + TWO_ROW_FUDGE
            } else {
                b
            }
        });
        Ok(frag)
    }

    /// `katex-vbox` / `katex-thinbox` (inline-flex): items are the blockified children.
    fn layout_flex(&mut self, i: u32, _cb_w: i32) -> Result<Frag> {
        let _d = LayoutDepth::enter()?;
        let cs = self.cs(i).clone();
        let column = cs.display == Display::InlineFlexColumn;
        let w_box = self.flex_intrinsic(i)?;
        let mut frag = Frag::default();
        let (mut main, mut cross) = (0, 0);
        let mut baseline = None;
        for k in self.elem_children(i) {
            let kcs = self.cs(k).clone();
            let f = match kcs.display {
                Display::InlineFlexRow | Display::InlineFlexColumn => self.layout_flex(k, w_box)?,
                Display::InlineTable => self.layout_table(k)?,
                _ => {
                    let w = self.intrinsic(k)?;
                    self.layout_block(k, w_box, Some(w), 0)?
                }
            };
            let (fw, fh) = (f.w, f.h);
            if baseline.is_none() {
                baseline = f.baseline.map(|b| if column { main + b } else { b });
            }
            let r = Self::rel_offset(&kcs, w_box, 0);
            if column {
                frag.place(f, r.0, main + r.1);
                main += fh;
                cross = cross.max(fw);
            } else {
                frag.place(f, main + r.0, r.1);
                main += fw;
                cross = cross.max(fh);
            }
        }
        frag.w = w_box;
        frag.h = if column { main } else { cross };
        frag.content_h = frag.h;
        frag.baseline = baseline;
        Ok(frag)
    }

    /// Lays out the `.katex-html` contents: one line per `\\`, the display tag apart.
    pub fn formula(&mut self, html: u32, display: bool) -> Result<FormulaLayout> {
        let cs = self.cs(html).clone();
        // Lines split at `.katex-newline` blocks; each newline's margin-top (`\\[gap]`)
        // separates the lines around it.
        let mut runs: Vec<(Vec<u32>, i32)> = vec![(Vec::new(), 0)];
        let mut tag = None;
        for k in self.nodes[html as usize].children.clone() {
            if !self.is_elem(k) {
                runs.last_mut().unwrap().0.push(k);
                continue;
            }
            let kcs = self.cs(k).clone();
            if kcs.display == Display::Block {
                let mt = resolve(kcs.margin[0], 0).unwrap_or(0);
                runs.push((Vec::new(), mt));
                continue;
            }
            if display && kcs.position == Position::Absolute {
                tag = Some(k);
                continue;
            }
            runs.last_mut().unwrap().0.push(k);
        }
        let strut = self.strut(&cs);
        let mut out = FormulaLayout::default();
        let mut y = 0;
        let mut first = None;
        for (run, gap) in runs {
            y += gap;
            let mut line = Line::new();
            let mut bases = Vec::new();
            for &k in &run {
                let x0 = line.width;
                self.line_items(&[k], &mut line, 0, (0, 0), 0)?;
                bases.push((x0, line.width));
            }
            if !line.has_content {
                continue; // no line box
            }
            let (asc, desc) = Self::line_metrics(&line, strut);
            let width = line.width;
            let mut frag = Frag::default();
            let mut pending = Vec::new();
            let b = self.emit_line(line, &mut frag, 0, y, asc, &mut pending);
            pending.append(&mut frag.pending);
            let mut holder = Frag {
                w: width,
                h: asc + desc,
                ..Default::default()
            };
            self.resolve_pending(&mut holder, pending, [0; 4])?;
            frag.ops.extend(holder.ops);
            if first.is_none() {
                first = Some(b);
            }
            out.lines.push(LineBox {
                top: y,
                ascent: asc,
                descent: desc,
                width,
                frag,
                bases,
            });
            y += asc + desc;
        }
        out.first_baseline = first.unwrap_or(0);
        if let Some(t) = tag {
            let w = self.intrinsic(t)?;
            out.tag = Some(self.layout_block(t, w, Some(w), 0)?);
        }
        Ok(out)
    }
}

/// Background and borders of a box (border box at x, y, w, h).
fn paint_box(frag: &mut Frag, cs: &Computed, x: i32, y: i32, w: i32, h: i32) {
    if let Some(bg) = cs.background
        && bg != Color::TRANSPARENT
    {
        frag.ops.push(Op::Rect {
            x,
            y,
            w,
            h,
            color: bg,
            dashed: false,
        });
    }
    let [bt, br, bb, bl] = borders(cs);
    let col = |i: usize| cs.border[i].color.unwrap_or(cs.inh.color);
    let dashed = |i: usize| cs.border[i].style == BorderStyle::Dashed;
    if bt > 0 {
        frag.ops.push(Op::Rect {
            x,
            y,
            w,
            h: bt,
            color: col(0),
            dashed: dashed(0),
        });
    }
    if bb > 0 {
        frag.ops.push(Op::Rect {
            x,
            y: y + h - bb,
            w,
            h: bb,
            color: col(2),
            dashed: dashed(2),
        });
    }
    if bl > 0 {
        frag.ops.push(Op::Rect {
            x,
            y,
            w: bl,
            h,
            color: col(3),
            dashed: dashed(3),
        });
    }
    if br > 0 {
        frag.ops.push(Op::Rect {
            x: x + w - br,
            y,
            w: br,
            h,
            color: col(1),
            dashed: dashed(1),
        });
    }
}

/// Nesting bound for the layout recursion (markup depth is several times the TeX nesting).
pub const MAX_LAYOUT_DEPTH: u32 = 1200;

thread_local! {
    static LAYOUT_DEPTH: std::cell::Cell<u32> = const { std::cell::Cell::new(0) };
}

struct LayoutDepth(());

impl LayoutDepth {
    fn enter() -> Result<LayoutDepth> {
        let d = LAYOUT_DEPTH.with(|d| {
            let v = d.get() + 1;
            d.set(v);
            v
        });
        if d > MAX_LAYOUT_DEPTH {
            LAYOUT_DEPTH.with(|c| c.set(c.get() - 1));
            return Err(Error::Internal(
                "decibell-tex: formula nested too deeply".into(),
            ));
        }
        Ok(LayoutDepth(()))
    }
}

impl Drop for LayoutDepth {
    fn drop(&mut self) {
        LAYOUT_DEPTH.with(|d| d.set(d.get().saturating_sub(1)));
    }
}

/// Maps an `<svg>` (viewBox + preserveAspectRatio) onto a `w`×`h` device-px viewport.
fn svg_item(svg: &SvgNode, w: f64, h: f64, dsf: f32) -> PathItem {
    let vb: Vec<f64> = svg
        .attr("viewBox")
        .unwrap_or("0 0 0 0")
        .split_whitespace()
        .filter_map(|v| v.parse().ok())
        .collect();
    let (vx, vy, vw, vh) = if vb.len() == 4 {
        (vb[0], vb[1], vb[2], vb[3])
    } else {
        (0.0, 0.0, w, h)
    };
    let par = svg.attr("preserveAspectRatio").unwrap_or("xMidYMid meet");
    let mut parts = par.split_whitespace();
    let align = parts.next().unwrap_or("xMidYMid");
    let slice = parts.next() == Some("slice");
    let (sx, sy, tx, ty) = if vw <= 0.0 || vh <= 0.0 {
        (1.0, 1.0, 0.0, 0.0)
    } else if align == "none" {
        (w / vw, h / vh, -vx * w / vw, -vy * h / vh)
    } else {
        let s = if slice {
            (w / vw).max(h / vh)
        } else {
            (w / vw).min(h / vh)
        };
        let ex = w - vw * s;
        let ey = h - vh * s;
        let ax = if align.starts_with("xMin") {
            0.0
        } else if align.starts_with("xMid") {
            ex / 2.0
        } else {
            ex
        };
        let ay = if align.ends_with("YMin") {
            0.0
        } else if align.ends_with("YMid") {
            ey / 2.0
        } else {
            ey
        };
        (s, s, ax - vx * s, ay - vy * s)
    };
    let d = dsf as f64;
    let mut item = PathItem {
        paths: Vec::new(),
        lines: Vec::new(),
        transform: [sx / d, sy / d, tx / d, ty / d],
        x: 0.0,
        y: 0.0,
        w: (w / d) as f32,
        h: (h / d) as f32,
        clip: None,
        color: Color::Current,
    };
    for c in &svg.children {
        match c {
            SvgChild::Path { name, alternate } => item.paths.push(match alternate {
                Some(a) => crate::layout::PathData::Owned(a.clone().into()),
                None => {
                    crate::layout::PathData::Static(crate::svg_geometry::path(name).unwrap_or(""))
                }
            }),
            SvgChild::Line { attributes } => {
                let get = |k: &str| {
                    attributes
                        .iter()
                        .find(|e| e.0 == k)
                        .map(|e| e.1.as_str())
                        .unwrap_or("0")
                };
                let pct = |v: &str, full: f64| -> f64 {
                    if let Some(p) = v.strip_suffix('%') {
                        p.parse::<f64>().unwrap_or(0.0) / 100.0 * full
                    } else {
                        v.parse().unwrap_or(0.0)
                    }
                };
                let sw = get("stroke-width");
                let swv = sw
                    .strip_suffix("em")
                    .and_then(|v| v.parse::<f64>().ok())
                    .unwrap_or(0.0);
                item.lines.push([
                    (pct(get("x1"), w) / d) as f32,
                    (pct(get("y1"), h) / d) as f32,
                    (pct(get("x2"), w) / d) as f32,
                    (pct(get("y2"), h) / d) as f32,
                    swv as f32, // em of the svg's font size; scaled by the caller below
                ]);
            }
        }
    }
    item
}
