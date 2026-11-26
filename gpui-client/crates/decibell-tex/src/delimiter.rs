//! Port of KaTeX `src/delimiter.ts`: delimiters of various sizes (TeXbook pp. 441–442).
//!
//! `makeSmallDelim` uses Main-Regular restyled to text/script/scriptscript, `makeLargeDelim`
//! one of the Size1–4 fonts in textstyle, `makeStackedDelim` stacks pieces (or draws one tall
//! SVG). The exported entry points pick between them.

use crate::build_common::{
    VListChild, VListElem, VListParam, make_span_s, make_svg_span, make_symbol, make_vlist,
};
use crate::data::font_metrics_data;
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode, Span, SvgChild, SvgNode};
use crate::font_metrics::{CharacterMetrics, get_character_metrics};
use crate::fonts::FontName;
use crate::options::{Options, size_class};
use crate::parse_error::{Result, ierr, perr};
use crate::parse_node::Mode;
use crate::style::Style;
use crate::svg_geometry::{inner_path, sqrt_path, tall_delim};
use crate::symbols;
use crate::units::{make_em, make_em_str};
use crate::utils::{first_code_unit, js_num};

/// `makeSqrtImage`'s result.
pub struct SqrtImage {
    pub span: Span,
    pub rule_width: f64,
    pub advance_width: f64,
}

/// `sizeToMaxHeight`.
pub const SIZE_TO_MAX_HEIGHT: [f64; 5] = [0.0, 1.2, 1.8, 2.4, 3.0];

/// JS `Math.round` (ties toward +∞).
fn js_round(x: f64) -> f64 {
    let f = x.floor();
    if x - f >= 0.5 { f + 1.0 } else { f }
}

fn size_font(size: u8) -> FontName {
    match size {
        1 => FontName::Size1Regular,
        2 => FontName::Size2Regular,
        3 => FontName::Size3Regular,
        _ => FontName::Size4Regular,
    }
}

/// `getMetrics(symbol, font, mode)`: metrics after the math-mode `replace`.
fn get_metrics(symbol: &str, font: FontName, mode: Mode) -> Result<CharacterMetrics> {
    let replace = symbols::get(Mode::Math, symbol).and_then(|i| i.replace);
    match get_character_metrics(replace.unwrap_or(symbol), font, mode) {
        Some(m) => Ok(m),
        None => ierr(format!(
            "Unsupported symbol {} and font size {}.",
            symbol,
            font.as_str()
        )),
    }
}

/// `styleWrap(delim, toStyle, options, classes)`.
fn style_wrap(
    delim: HtmlNode,
    to_style: Style,
    options: &Options,
    classes: Vec<&'static str>,
) -> Span {
    let new_options = options.having_base_style(Some(to_style));
    let mut c = classes;
    c.extend(new_options.sizing_classes(options));
    let mut span = make_span_s(c, vec![delim], Some(options), CssStyle::new());
    let mult = new_options.size_multiplier / options.size_multiplier;
    span.height *= mult;
    span.depth *= mult;
    span.max_font_size = new_options.size_multiplier;
    span
}

/// `centerSpan(span, options, style)`.
fn center_span(span: &mut Span, options: &Options, style: Style) {
    let new_options = options.having_base_style(Some(style));
    let shift = (1.0 - options.size_multiplier / new_options.size_multiplier)
        * options.font_metrics().axis_height;
    span.classes.push("delimcenter");
    span.style.set(CssProp::Top, make_em(shift));
    span.height -= shift;
    span.depth += shift;
}

/// `makeSmallDelim(delim, style, center, options, mode, classes)`.
fn make_small_delim(
    delim: &str,
    style: Style,
    center: bool,
    options: &Options,
    mode: Mode,
    classes: Vec<&'static str>,
) -> Span {
    let text = make_symbol(delim, FontName::MainRegular, mode, Some(options), vec![]);
    let mut span = style_wrap(HtmlNode::Symbol(Box::new(text)), style, options, classes);
    if center {
        center_span(&mut span, options, style);
    }
    span
}

/// `makeLargeDelim(delim, size, center, options, mode, classes)`.
fn make_large_delim(
    delim: &str,
    size: u8,
    center: bool,
    options: &Options,
    mode: Mode,
    classes: Vec<&'static str>,
) -> Span {
    let inner = make_symbol(delim, size_font(size), mode, Some(options), vec![]);
    let sized = make_span_s(
        vec!["delimsizing", size_class(size)],
        vec![HtmlNode::Symbol(Box::new(inner))],
        Some(options),
        CssStyle::new(),
    );
    let mut span = style_wrap(HtmlNode::span(sized), Style::TEXT, options, classes);
    if center {
        center_span(&mut span, options, Style::TEXT);
    }
    span
}

/// `makeGlyphSpan(symbol, font, mode)`.
fn make_glyph_span(symbol: &str, font: FontName, mode: Mode) -> VListChild {
    let size_class = if font == FontName::Size1Regular {
        "delim-size1"
    } else {
        "delim-size4"
    };
    let sym = make_symbol(symbol, font, mode, None, vec![]);
    let wrap = make_span_s(
        vec![],
        vec![HtmlNode::Symbol(Box::new(sym))],
        None,
        CssStyle::new(),
    );
    let corner = make_span_s(
        vec!["delimsizinginner", size_class],
        vec![HtmlNode::span(wrap)],
        None,
        CssStyle::new(),
    );
    VListChild::Elem(VListElem::new(HtmlNode::span(corner)))
}

/// `fontMetricsData[font][code][4]` (raw table width, no fallbacks).
fn raw_width(font: FontName, code: u16) -> Option<f64> {
    let t = font_metrics_data::table(font);
    t.binary_search_by_key(&code, |e| e.0)
        .ok()
        .map(|i| t[i].1[4] as f64 / 100000.0)
}

/// `makeInner(ch, height, options)`.
fn make_inner(ch: &str, height: f64, options: &Options) -> Result<VListChild> {
    let code = first_code_unit(ch).unwrap_or(0);
    let width = match raw_width(FontName::Size4Regular, code)
        .or_else(|| raw_width(FontName::Size1Regular, code))
    {
        Some(w) => w,
        None => return ierr("Cannot read properties of undefined (reading '4')"),
    };
    let vb_h = js_round(1000.0 * height);
    let path = SvgChild::Path {
        name: "inner",
        alternate: Some(inner_path(ch, vb_h)),
    };
    let svg = SvgNode {
        children: vec![path],
        attributes: vec![
            ("width", make_em_str(width)),
            ("height", make_em_str(height)),
            ("style", format!("width:{}", make_em_str(width))),
            (
                "viewBox",
                format!("0 0 {} {}", js_num(1000.0 * width), js_num(vb_h)),
            ),
            ("preserveAspectRatio", "xMinYMin".to_string()),
        ],
        style: CssStyle::new(),
    };
    let mut span = make_svg_span(vec![], vec![svg], Some(options), CssStyle::new());
    span.height = height;
    span.style.set(CssProp::Height, make_em(height));
    span.style.set(CssProp::Width, make_em(width));
    Ok(VListChild::Elem(VListElem::new(HtmlNode::span(span))))
}

const LAP_IN_EMS: f64 = 0.008;

fn lap() -> VListChild {
    VListChild::Kern(-LAP_IN_EMS)
}

/// `makeStackedDelim(delim, heightTotal, center, options, mode, classes)`.
fn make_stacked_delim(
    delim: &str,
    height_total: f64,
    center: bool,
    options: &Options,
    mode: Mode,
    classes: Vec<&'static str>,
) -> Result<Span> {
    let mut top = delim;
    let mut repeat = delim;
    let mut bottom = delim;
    let mut middle: Option<&str> = None;
    let mut svg_label = "";
    let mut view_box_width = 0.0f64;
    let mut font = FontName::Size1Regular;
    match delim {
        "\\uparrow" => {
            repeat = "\u{23d0}";
            bottom = "\u{23d0}";
        }
        "\\Uparrow" => {
            repeat = "\u{2016}";
            bottom = "\u{2016}";
        }
        "\\downarrow" => {
            top = "\u{23d0}";
            repeat = "\u{23d0}";
        }
        "\\Downarrow" => {
            top = "\u{2016}";
            repeat = "\u{2016}";
        }
        "\\updownarrow" => {
            top = "\\uparrow";
            repeat = "\u{23d0}";
            bottom = "\\downarrow";
        }
        "\\Updownarrow" => {
            top = "\\Uparrow";
            repeat = "\u{2016}";
            bottom = "\\Downarrow";
        }
        "|" | "\\lvert" | "\\rvert" | "\\vert" => {
            repeat = "\u{2223}";
            svg_label = "vert";
            view_box_width = 333.0;
        }
        "\\|" | "\\lVert" | "\\rVert" | "\\Vert" => {
            repeat = "\u{2225}";
            svg_label = "doublevert";
            view_box_width = 556.0;
        }
        "[" | "\\lbrack" => {
            top = "\u{23a1}";
            repeat = "\u{23a2}";
            bottom = "\u{23a3}";
            font = FontName::Size4Regular;
            svg_label = "lbrack";
            view_box_width = 667.0;
        }
        "]" | "\\rbrack" => {
            top = "\u{23a4}";
            repeat = "\u{23a5}";
            bottom = "\u{23a6}";
            font = FontName::Size4Regular;
            svg_label = "rbrack";
            view_box_width = 667.0;
        }
        "\\lfloor" | "\u{230a}" => {
            repeat = "\u{23a2}";
            top = "\u{23a2}";
            bottom = "\u{23a3}";
            font = FontName::Size4Regular;
            svg_label = "lfloor";
            view_box_width = 667.0;
        }
        "\\lceil" | "\u{2308}" => {
            top = "\u{23a1}";
            repeat = "\u{23a2}";
            bottom = "\u{23a2}";
            font = FontName::Size4Regular;
            svg_label = "lceil";
            view_box_width = 667.0;
        }
        "\\rfloor" | "\u{230b}" => {
            repeat = "\u{23a5}";
            top = "\u{23a5}";
            bottom = "\u{23a6}";
            font = FontName::Size4Regular;
            svg_label = "rfloor";
            view_box_width = 667.0;
        }
        "\\rceil" | "\u{2309}" => {
            top = "\u{23a4}";
            repeat = "\u{23a5}";
            bottom = "\u{23a5}";
            font = FontName::Size4Regular;
            svg_label = "rceil";
            view_box_width = 667.0;
        }
        "(" | "\\lparen" => {
            top = "\u{239b}";
            repeat = "\u{239c}";
            bottom = "\u{239d}";
            font = FontName::Size4Regular;
            svg_label = "lparen";
            view_box_width = 875.0;
        }
        ")" | "\\rparen" => {
            top = "\u{239e}";
            repeat = "\u{239f}";
            bottom = "\u{23a0}";
            font = FontName::Size4Regular;
            svg_label = "rparen";
            view_box_width = 875.0;
        }
        "\\{" | "\\lbrace" => {
            top = "\u{23a7}";
            middle = Some("\u{23a8}");
            bottom = "\u{23a9}";
            repeat = "\u{23aa}";
            font = FontName::Size4Regular;
        }
        "\\}" | "\\rbrace" => {
            top = "\u{23ab}";
            middle = Some("\u{23ac}");
            bottom = "\u{23ad}";
            repeat = "\u{23aa}";
            font = FontName::Size4Regular;
        }
        "\\lgroup" | "\u{27ee}" => {
            top = "\u{23a7}";
            bottom = "\u{23a9}";
            repeat = "\u{23aa}";
            font = FontName::Size4Regular;
        }
        "\\rgroup" | "\u{27ef}" => {
            top = "\u{23ab}";
            bottom = "\u{23ad}";
            repeat = "\u{23aa}";
            font = FontName::Size4Regular;
        }
        "\\lmoustache" | "\u{23b0}" => {
            top = "\u{23a7}";
            bottom = "\u{23ad}";
            repeat = "\u{23aa}";
            font = FontName::Size4Regular;
        }
        "\\rmoustache" | "\u{23b1}" => {
            top = "\u{23ab}";
            bottom = "\u{23a9}";
            repeat = "\u{23aa}";
            font = FontName::Size4Regular;
        }
        _ => {}
    }

    let top_m = get_metrics(top, font, mode)?;
    let top_ht = top_m.height + top_m.depth;
    let repeat_m = get_metrics(repeat, font, mode)?;
    let repeat_ht = repeat_m.height + repeat_m.depth;
    let bottom_m = get_metrics(bottom, font, mode)?;
    let bottom_ht = bottom_m.height + bottom_m.depth;
    let mut middle_ht = 0.0;
    let mut middle_factor = 1.0;
    if let Some(m) = middle {
        let mm = get_metrics(m, font, mode)?;
        middle_ht = mm.height + mm.depth;
        middle_factor = 2.0;
    }
    let min_height = top_ht + bottom_ht + middle_ht;
    let repeat_count =
        0.0f64.max(((height_total - min_height) / (middle_factor * repeat_ht)).ceil());
    let real_height_total = min_height + repeat_count * middle_factor * repeat_ht;
    let mut axis_height = options.font_metrics().axis_height;
    if center {
        axis_height *= options.size_multiplier;
    }
    let depth = real_height_total / 2.0 - axis_height;

    let mut stack: Vec<VListChild> = Vec::new();
    if !svg_label.is_empty() {
        let mid_height = real_height_total - top_ht - bottom_ht;
        let view_box_height = js_round(real_height_total * 1000.0);
        let Some(path_str) = tall_delim(svg_label, js_round(mid_height * 1000.0)) else {
            return ierr(format!("Unknown stretchy delimiter: {}", svg_label));
        };
        let width = make_em_str(view_box_width / 1000.0);
        let height = make_em_str(view_box_height / 1000.0);
        let svg = SvgNode {
            children: vec![SvgChild::Path {
                name: svg_label,
                alternate: Some(path_str),
            }],
            attributes: vec![
                ("width", width.clone()),
                ("height", height.clone()),
                (
                    "viewBox",
                    format!("0 0 {} {}", js_num(view_box_width), js_num(view_box_height)),
                ),
            ],
            style: CssStyle::new(),
        };
        let mut wrapper = make_svg_span(vec![], vec![svg], Some(options), CssStyle::new());
        wrapper.height = view_box_height / 1000.0;
        wrapper.style.set(CssProp::Width, CssVal::Str(width.into()));
        wrapper
            .style
            .set(CssProp::Height, CssVal::Str(height.into()));
        stack.push(VListChild::Elem(VListElem::new(HtmlNode::span(wrapper))));
    } else {
        stack.push(make_glyph_span(bottom, font, mode));
        stack.push(lap());
        match middle {
            None => {
                let inner_height = real_height_total - top_ht - bottom_ht + 2.0 * LAP_IN_EMS;
                stack.push(make_inner(repeat, inner_height, options)?);
            }
            Some(m) => {
                let inner_height =
                    (real_height_total - top_ht - bottom_ht - middle_ht) / 2.0 + 2.0 * LAP_IN_EMS;
                stack.push(make_inner(repeat, inner_height, options)?);
                stack.push(lap());
                stack.push(make_glyph_span(m, font, mode));
                stack.push(lap());
                stack.push(make_inner(repeat, inner_height, options)?);
            }
        }
        stack.push(lap());
        stack.push(make_glyph_span(top, font, mode));
    }

    let new_options = options.having_base_style(Some(Style::TEXT));
    let inner = make_vlist(VListParam::Bottom(depth, stack))?;
    let mult = make_span_s(
        vec!["delimsizing", "mult"],
        vec![HtmlNode::span(inner)],
        Some(&new_options),
        CssStyle::new(),
    );
    Ok(style_wrap(
        HtmlNode::span(mult),
        Style::TEXT,
        options,
        classes,
    ))
}

const VB_PAD: f64 = 80.0;
const EM_PAD: f64 = 0.08;

/// `sqrtSvg(sqrtName, height, viewBoxHeight, extraVinculum, options)`.
fn sqrt_svg(
    sqrt_name: &'static str,
    height: f64,
    view_box_height: f64,
    extra_vinculum: f64,
    options: &Options,
) -> Span {
    let path = sqrt_path(sqrt_name, extra_vinculum, view_box_height);
    let svg = SvgNode {
        children: vec![SvgChild::Path {
            name: sqrt_name,
            alternate: Some(path),
        }],
        attributes: vec![
            ("width", "400em".to_string()),
            ("height", make_em_str(height)),
            ("viewBox", format!("0 0 400000 {}", js_num(view_box_height))),
            ("preserveAspectRatio", "xMinYMin slice".to_string()),
        ],
        style: CssStyle::new(),
    };
    make_svg_span(vec!["hide-tail"], vec![svg], Some(options), CssStyle::new())
}

/// `makeSqrtImage(height, options)`.
pub fn make_sqrt_image(height: f64, options: &Options) -> Result<SqrtImage> {
    let new_options = options.having_base_sizing();
    let delim = traverse_sequence(
        "\\surd",
        height * new_options.size_multiplier,
        STACK_LARGE_SEQUENCE,
        &new_options,
    )?;
    let mut size_multiplier = new_options.size_multiplier;
    let extra_vinculum =
        0.0f64.max(options.min_rule_thickness - options.font_metrics().sqrt_rule_thickness);
    let (mut span, span_height, tex_height, advance_width) = match delim {
        Delim::Small(_) => {
            let view_box_height = 1000.0 + 1000.0 * extra_vinculum + VB_PAD;
            if height < 1.0 {
                size_multiplier = 1.0; // mimic a \textfont radical
            } else if height < 1.4 {
                size_multiplier = 0.7; // mimic a \scriptfont radical
            }
            let span_height = (1.0 + extra_vinculum + EM_PAD) / size_multiplier;
            let tex_height = (1.00 + extra_vinculum) / size_multiplier;
            let mut span = sqrt_svg(
                "sqrtMain",
                span_height,
                view_box_height,
                extra_vinculum,
                options,
            );
            span.style.set(CssProp::MinWidth, CssVal::Static("0.853em"));
            (span, span_height, tex_height, 0.833 / size_multiplier)
        }
        Delim::Large(size) => {
            let name = match size {
                1 => "sqrtSize1",
                2 => "sqrtSize2",
                3 => "sqrtSize3",
                _ => "sqrtSize4",
            };
            let max_h = SIZE_TO_MAX_HEIGHT[size as usize];
            let view_box_height = (1000.0 + VB_PAD) * max_h;
            let tex_height = (max_h + extra_vinculum) / size_multiplier;
            let span_height = (max_h + extra_vinculum + EM_PAD) / size_multiplier;
            let mut span = sqrt_svg(name, span_height, view_box_height, extra_vinculum, options);
            span.style.set(CssProp::MinWidth, CssVal::Static("1.02em"));
            (span, span_height, tex_height, 1.0 / size_multiplier)
        }
        Delim::Stack => {
            let span_height = height + extra_vinculum + EM_PAD;
            let tex_height = height + extra_vinculum;
            // KaTeX adds `extraVinculum` unscaled here; kept as is.
            let view_box_height = (1000.0 * height + extra_vinculum).floor() + VB_PAD;
            let mut span = sqrt_svg(
                "sqrtTall",
                span_height,
                view_box_height,
                extra_vinculum,
                options,
            );
            span.style.set(CssProp::MinWidth, CssVal::Static("0.742em"));
            (span, span_height, tex_height, 1.056)
        }
    };
    span.height = tex_height;
    span.style.set(CssProp::Height, make_em(span_height));
    Ok(SqrtImage {
        span,
        advance_width,
        rule_width: (options.font_metrics().sqrt_rule_thickness + extra_vinculum) * size_multiplier,
    })
}

/// `stackLargeDelimiters`.
fn is_stack_large(d: &str) -> bool {
    matches!(
        d,
        "(" | "\\lparen"
            | ")"
            | "\\rparen"
            | "["
            | "\\lbrack"
            | "]"
            | "\\rbrack"
            | "\\{"
            | "\\lbrace"
            | "\\}"
            | "\\rbrace"
            | "\\lfloor"
            | "\\rfloor"
            | "\u{230a}"
            | "\u{230b}"
            | "\\lceil"
            | "\\rceil"
            | "\u{2308}"
            | "\u{2309}"
            | "\\surd"
    )
}

/// `stackAlwaysDelimiters`.
fn is_stack_always(d: &str) -> bool {
    matches!(
        d,
        "\\uparrow"
            | "\\downarrow"
            | "\\updownarrow"
            | "\\Uparrow"
            | "\\Downarrow"
            | "\\Updownarrow"
            | "|"
            | "\\|"
            | "\\vert"
            | "\\Vert"
            | "\\lvert"
            | "\\rvert"
            | "\\lVert"
            | "\\rVert"
            | "\\lgroup"
            | "\\rgroup"
            | "\u{27ee}"
            | "\u{27ef}"
            | "\\lmoustache"
            | "\\rmoustache"
            | "\u{23b0}"
            | "\u{23b1}"
    )
}

/// `stackNeverDelimiters`.
fn is_stack_never(d: &str) -> bool {
    matches!(
        d,
        "<" | ">" | "\\langle" | "\\rangle" | "/" | "\\backslash" | "\\lt" | "\\gt"
    )
}

/// `<` / `>` (and their aliases) become `\langle` / `\rangle` in delimiters.
fn normalize_angle(delim: &str) -> &str {
    match delim {
        "<" | "\\lt" | "\u{27e8}" => "\\langle",
        ">" | "\\gt" | "\u{27e9}" => "\\rangle",
        other => other,
    }
}

/// `makeSizedDelim(delim, size, options, mode, classes)`.
pub fn make_sized_delim(
    delim: &str,
    size: u8,
    options: &Options,
    mode: Mode,
    classes: Vec<&'static str>,
) -> Result<Span> {
    let delim = normalize_angle(delim);
    if is_stack_large(delim) || is_stack_never(delim) {
        Ok(make_large_delim(delim, size, false, options, mode, classes))
    } else if is_stack_always(delim) {
        make_stacked_delim(
            delim,
            SIZE_TO_MAX_HEIGHT[size as usize],
            false,
            options,
            mode,
            classes,
        )
    } else {
        perr(format!("Illegal delimiter: '{}'", delim), None)
    }
}

/// `Delimiter`: one step of a size sequence.
#[derive(Clone, Copy)]
enum Delim {
    Small(Style),
    Large(u8),
    Stack,
}

const STACK_NEVER_SEQUENCE: &[Delim] = &[
    Delim::Small(Style::SCRIPTSCRIPT),
    Delim::Small(Style::SCRIPT),
    Delim::Small(Style::TEXT),
    Delim::Large(1),
    Delim::Large(2),
    Delim::Large(3),
    Delim::Large(4),
];

const STACK_ALWAYS_SEQUENCE: &[Delim] = &[
    Delim::Small(Style::SCRIPTSCRIPT),
    Delim::Small(Style::SCRIPT),
    Delim::Small(Style::TEXT),
    Delim::Stack,
];

const STACK_LARGE_SEQUENCE: &[Delim] = &[
    Delim::Small(Style::SCRIPTSCRIPT),
    Delim::Small(Style::SCRIPT),
    Delim::Small(Style::TEXT),
    Delim::Large(1),
    Delim::Large(2),
    Delim::Large(3),
    Delim::Large(4),
    Delim::Stack,
];

/// `delimTypeToFont(type)`.
fn delim_type_to_font(t: Delim) -> FontName {
    match t {
        Delim::Small(_) => FontName::MainRegular,
        Delim::Large(n) => size_font(n),
        Delim::Stack => FontName::Size4Regular,
    }
}

/// `traverseSequence(delim, height, sequence, options)`.
fn traverse_sequence(
    delim: &str,
    height: f64,
    sequence: &[Delim],
    options: &Options,
) -> Result<Delim> {
    let start = 2.min(3 - options.style.size as usize);
    for &t in &sequence[start..] {
        if let Delim::Stack = t {
            break;
        }
        let metrics = get_metrics(delim, delim_type_to_font(t), Mode::Math)?;
        let mut height_depth = metrics.height + metrics.depth;
        if let Delim::Small(style) = t {
            let new_options = options.having_base_style(Some(style));
            height_depth *= new_options.size_multiplier;
        }
        if height_depth > height {
            return Ok(t);
        }
    }
    Ok(sequence[sequence.len() - 1])
}

/// `makeCustomSizedDelim(delim, height, center, options, mode, classes)`.
pub fn make_custom_sized_delim(
    delim: &str,
    height: f64,
    center: bool,
    options: &Options,
    mode: Mode,
    classes: Vec<&'static str>,
) -> Result<Span> {
    let delim = normalize_angle(delim);
    let sequence = if is_stack_never(delim) {
        STACK_NEVER_SEQUENCE
    } else if is_stack_large(delim) {
        STACK_LARGE_SEQUENCE
    } else {
        STACK_ALWAYS_SEQUENCE
    };
    match traverse_sequence(delim, height, sequence, options)? {
        Delim::Small(style) => Ok(make_small_delim(
            delim, style, center, options, mode, classes,
        )),
        Delim::Large(size) => Ok(make_large_delim(
            delim, size, center, options, mode, classes,
        )),
        Delim::Stack => make_stacked_delim(delim, height, center, options, mode, classes),
    }
}

/// `makeLeftRightDelim(delim, height, depth, options, mode, classes)`.
pub fn make_left_right_delim(
    delim: &str,
    height: f64,
    depth: f64,
    options: &Options,
    mode: Mode,
    classes: Vec<&'static str>,
) -> Result<Span> {
    let axis_height = options.font_metrics().axis_height * options.size_multiplier;
    let delimiter_factor = 901.0;
    let delimiter_extend = 5.0 / options.font_metrics().pt_per_em;
    let max_dist_from_axis = (height - axis_height).max(depth + axis_height);
    let total_height = (max_dist_from_axis / 500.0 * delimiter_factor)
        .max(2.0 * max_dist_from_axis - delimiter_extend);
    make_custom_sized_delim(delim, total_height, true, options, mode, classes)
}

#[cfg(test)]
mod tests {
    use super::js_round;

    #[test]
    fn js_math_round() {
        assert_eq!(js_round(2.5), 3.0);
        assert_eq!(js_round(-2.5), -2.0);
        assert_eq!(js_round(1.4999999), 1.0);
    }
}
