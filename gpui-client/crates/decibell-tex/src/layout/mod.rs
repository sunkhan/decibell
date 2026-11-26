//! The layout KaTeX leaves to the browser: Chromium's CSS layout of KaTeX's markup with
//! `katex.css`, producing a positioned display list.
//!
//! Coordinates are CSS px relative to the formula's origin: x from its left edge, y down from
//! the baseline of its first line. `LayoutOptions::dsf` is the device scale Chromium would lay
//! out at; positions snap to its LayoutUnit grid (1/(64·dsf) CSS px) exactly as Chromium's do.

pub mod color;
pub mod css;
pub mod engine;
pub mod text;

use std::rc::Rc;

pub use color::Color;

use crate::build_tree::{build_html_tree, parse_tree};
use crate::dom_tree::HtmlNode;
use crate::fonts::KatexFont;
use crate::parse_error::Error;
use crate::settings::Settings;
use engine::{Engine, Frag, LU, Op, px};

/// Layout parameters.
#[derive(Clone, Copy, Debug)]
pub struct LayoutOptions {
    /// Font size of the surrounding text in CSS px (`.katex` is 1.21× this, as katex.css says).
    pub text_px: f32,
    /// Device pixels per CSS px. Chromium positions everything on a 1/(64·dsf) px grid; text
    /// advances are fractional at any scale.
    pub dsf: f32,
    /// `maxSize` (em), `maxExpand`: Decibell's adversarial-input ceilings.
    pub max_size: f64,
    pub max_expand: u32,
}

impl Default for LayoutOptions {
    fn default() -> LayoutOptions {
        LayoutOptions {
            text_px: 16.0,
            dsf: 2.0,
            max_size: 16.0,
            max_expand: 256,
        }
    }
}

/// One glyph to draw.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Glyph {
    pub ch: char,
    /// The KaTeX face Chromium selects (load lazily; see [`KatexFont::file_name`]).
    pub font: KatexFont,
    /// Pen position: left edge of the advance, on the baseline (CSS px).
    pub x: f32,
    pub y: f32,
    /// Font size in CSS px (the size FreeType renders at: Chromium truncates to 1/64 device px).
    pub size: f32,
    pub color: Color,
    /// Chromium emboldens / slants a face that lacks the requested weight / style.
    pub synthetic_bold: bool,
    pub synthetic_italic: bool,
    /// The face has no glyph for `ch`: Chromium draws it with a system font (the advance used
    /// here is a guess).
    pub fallback: bool,
    /// A `text-shadow` copy (`\pmb`).
    pub shadow: bool,
    /// Text CSS generates (`.eqn-num::before`'s "(n)"), not in KaTeX's markup.
    pub generated: bool,
}

/// SVG path data: a static KaTeX path, or one computed for this formula (sqrt, tall delimiters).
#[derive(Clone, Debug, PartialEq)]
pub enum PathData {
    Static(&'static str),
    Owned(Rc<str>),
}

impl PathData {
    pub fn as_str(&self) -> &str {
        match self {
            PathData::Static(s) => s,
            PathData::Owned(s) => s,
        }
    }
}

/// An `<svg>` element: fill `paths` (in viewBox units) mapped by `transform`, and stroke
/// `lines`, clipped to `clip`.
#[derive(Clone, Debug, PartialEq)]
pub struct PathItem {
    pub paths: Vec<PathData>,
    /// `\cancel` strokes: x1, y1, x2, y2 (CSS px from the viewport origin), stroke width (CSS px).
    pub lines: Vec<[f32; 5]>,
    /// viewBox → CSS px from the viewport origin: `px = (sx·u + tx, sy·v + ty)`.
    pub transform: [f64; 4],
    /// The viewport (CSS px).
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    /// Visible area (`overflow: hidden` ancestors), CSS px: x, y, w, h.
    pub clip: Option<[f32; 4]>,
    pub color: Color,
}

/// A display-list item.
#[derive(Clone, Debug, PartialEq)]
pub enum Item {
    Glyph(Glyph),
    /// A filled rectangle (rules, fraction bars, borders, backgrounds), CSS px.
    Rect {
        x: f32,
        y: f32,
        w: f32,
        h: f32,
        color: Color,
        dashed: bool,
    },
    Path(Box<PathItem>),
}

/// A `.katex-base`: KaTeX's unbreakable unit. Inline formulas may wrap between bases.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Base {
    pub x: f32,
    pub width: f32,
    pub line: u16,
    /// Display-list items `items[start..end]` belong to this base.
    pub start: u32,
    pub end: u32,
}

/// A line of the formula (more than one after `\\` / `\newline`).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MathLine {
    pub baseline: f32,
    pub width: f32,
    /// Extent of the line box above / below its baseline (Chromium's line height).
    pub ascent: f32,
    pub descent: f32,
}

/// A laid-out formula.
#[derive(Clone, Debug, PartialEq)]
pub struct MathLayout {
    pub items: Vec<Item>,
    pub lines: Vec<MathLine>,
    pub bases: Vec<Base>,
    /// Width of the widest line (CSS px).
    pub width: f32,
    /// A display equation's tag: its items are in `tag_items`, laid out from x = 0 on the first
    /// baseline; Chromium places it at the container's right edge (`right: 0`).
    pub tag_items: Vec<Item>,
    pub tag_width: f32,
    pub display: bool,
}

/// The result of laying out a formula.
#[derive(Clone, Debug, PartialEq)]
pub enum Rendered {
    Math(MathLayout),
    /// A KaTeX ParseError: draw the raw TeX in the surrounding font in the error colour (as
    /// KaTeX's `katex-error` span does). `message` is KaTeX's (shown as its `title`).
    Error {
        message: String,
    },
    /// A non-ParseError KaTeX failure (or a formula beyond the port's nesting bound): Decibell
    /// shows the raw TeX as inline code.
    Internal {
        message: String,
    },
}

/// Parses, builds and lays out `tex`.
pub fn layout(tex: &str, display: bool, opts: &LayoutOptions) -> Rendered {
    let settings = Settings {
        display_mode: display,
        error_color: color::ERROR_COLOR_SENTINEL.to_string(),
        max_size: opts.max_size,
        max_expand: opts.max_expand,
        ..Default::default()
    };
    let tree = match parse_tree(tex, &settings).and_then(|t| build_html_tree(&t, &settings)) {
        Ok(t) => t,
        Err(Error::Parse(p)) => {
            return Rendered::Error {
                message: p.to_js_string(),
            };
        }
        Err(Error::Internal(m)) => return Rendered::Internal { message: m },
    };
    match layout_tree(&tree, display, opts) {
        Ok(m) => Rendered::Math(m),
        Err(Error::Parse(p)) => Rendered::Error {
            message: p.to_js_string(),
        },
        Err(Error::Internal(m)) => Rendered::Internal { message: m },
    }
}

/// Finds `.katex-html` under the root span.
fn katex_html(span: &crate::dom_tree::Span) -> Option<&crate::dom_tree::Span> {
    if span.has_class("katex-html") {
        return Some(span);
    }
    for c in &span.children {
        if let HtmlNode::Span(s) = c
            && let Some(h) = katex_html(s)
        {
            return Some(h);
        }
    }
    None
}

/// Lays out an already built tree (the `.katex` / `.katex-display` span).
pub fn layout_tree(
    tree: &crate::dom_tree::Span,
    display: bool,
    opts: &LayoutOptions,
) -> Result<MathLayout, Error> {
    css::DSF_HINT.with(|d| d.set(opts.dsf));
    css::DISPLAY_MODE.with(|d| d.set(display));
    let html = katex_html(tree).ok_or_else(|| Error::Internal("no .katex-html".into()))?;
    let root = css::Computed::root(opts.text_px, opts.dsf);
    let (mut engine, html_id) = Engine::new(html, &root, display, opts.dsf)?;
    let fl = engine.formula(html_id, display)?;
    let dsf = opts.dsf as f64;
    let origin_y = fl.first_baseline;
    let mut out = MathLayout {
        items: Vec::new(),
        lines: Vec::new(),
        bases: Vec::new(),
        width: 0.0,
        tag_items: Vec::new(),
        tag_width: 0.0,
        display,
    };
    for (li, line) in fl.lines.iter().enumerate() {
        out.lines.push(MathLine {
            baseline: (px(line.top + line.ascent - origin_y) / dsf) as f32,
            width: (px(line.width) / dsf) as f32,
            ascent: (px(line.ascent) / dsf) as f32,
            descent: (px(line.descent) / dsf) as f32,
        });
        out.width = out.width.max((px(line.width) / dsf) as f32);
        let mut items = Vec::new();
        flatten(&line.frag, 0.0, -px(origin_y), dsf, &mut items);
        // Assign items to bases by x (bases are disjoint, left to right).
        let start_index = out.items.len();
        let mut per_base: Vec<Vec<Item>> = vec![Vec::new(); line.bases.len().max(1)];
        for it in items {
            let x = item_x(&it) * dsf as f32 * LU as f32;
            let bi = line
                .bases
                .iter()
                .rposition(|b| (b.0 as f32) <= x + 0.5)
                .unwrap_or(0);
            let last = per_base.len() - 1;
            per_base[bi.min(last)].push(it);
        }
        let mut cursor = start_index;
        for (bi, b) in line.bases.iter().enumerate() {
            let its = std::mem::take(&mut per_base[bi]);
            let n = its.len();
            out.items.extend(its);
            out.bases.push(Base {
                x: (px(b.0) / dsf) as f32,
                width: (px(b.1 - b.0) / dsf) as f32,
                line: li as u16,
                start: cursor as u32,
                end: (cursor + n) as u32,
            });
            cursor += n;
        }
        if line.bases.is_empty() {
            out.items.extend(per_base.into_iter().flatten());
        }
    }
    if let Some(tag) = &fl.tag {
        let mut items = Vec::new();
        let by = tag.baseline.unwrap_or(0);
        flatten(tag, 0.0, -px(by), dsf, &mut items);
        out.tag_items = items;
        out.tag_width = (px(tag.w) / dsf) as f32;
    }
    Ok(out)
}

fn item_x(it: &Item) -> f32 {
    match it {
        Item::Glyph(g) => g.x,
        Item::Rect { x, .. } => *x,
        Item::Path(p) => p.x,
    }
}

/// Walks a fragment's ops into absolute display items (CSS px).
fn flatten(frag: &Frag, x0: f64, y0: f64, dsf: f64, out: &mut Vec<Item>) {
    let mut stack: Vec<(f64, f64)> = vec![(x0, y0)];
    let mut clips: Vec<Option<[f64; 4]>> = vec![None];
    for op in &frag.ops {
        let (ox, oy) = *stack.last().unwrap();
        let clip = *clips.last().unwrap();
        match op {
            Op::Push { dx, dy } => stack.push((ox + dx, oy + dy)),
            Op::Pop => {
                stack.pop();
            }
            Op::Clip { x, y, w, h } => {
                let r = [ox + px(*x), oy + px(*y), px(*w), px(*h)];
                let r = match clip {
                    None => r,
                    Some(c) => intersect(c, r),
                };
                clips.push(Some(r));
            }
            Op::Unclip => {
                clips.pop();
            }
            Op::Glyph { x, y, g } => {
                if g.color == Color::TRANSPARENT {
                    continue;
                }
                let mut g = *g;
                g.x = ((ox + x) / dsf) as f32;
                g.y = ((oy + y) / dsf) as f32;
                out.push(Item::Glyph(g));
            }
            Op::Rect {
                x,
                y,
                w,
                h,
                color,
                dashed,
            } => {
                if *color == Color::TRANSPARENT {
                    continue;
                }
                let mut r = [ox + px(*x), oy + px(*y), px(*w), px(*h)];
                if let Some(c) = clip {
                    r = intersect(c, r);
                }
                if r[2] <= 0.0 || r[3] <= 0.0 {
                    continue;
                }
                out.push(Item::Rect {
                    x: (r[0] / dsf) as f32,
                    y: (r[1] / dsf) as f32,
                    w: (r[2] / dsf) as f32,
                    h: (r[3] / dsf) as f32,
                    color: *color,
                    dashed: *dashed,
                });
            }
            Op::Path { x, y, p } => {
                if p.color == Color::TRANSPARENT {
                    continue;
                }
                let mut p = (**p).clone();
                p.x = ((ox + px(*x)) / dsf) as f32;
                p.y = ((oy + px(*y)) / dsf) as f32;
                p.clip = clip.map(|c| {
                    [
                        (c[0] / dsf) as f32,
                        (c[1] / dsf) as f32,
                        (c[2] / dsf) as f32,
                        (c[3] / dsf) as f32,
                    ]
                });
                out.push(Item::Path(Box::new(p)));
            }
        }
    }
}

fn intersect(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let x0 = a[0].max(b[0]);
    let y0 = a[1].max(b[1]);
    let x1 = (a[0] + a[2]).min(b[0] + b[2]);
    let y1 = (a[1] + a[3]).min(b[1] + b[3]);
    [x0, y0, (x1 - x0).max(0.0), (y1 - y0).max(0.0)]
}
