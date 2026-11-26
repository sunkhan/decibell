//! Port of KaTeX `src/buildCommon.ts`.

use crate::dom_tree::{
    CssProp, CssStyle, CssVal, Fragment, HtmlNode, Span, SvgChild, SvgNode, SymbolNode,
};
use crate::font_metrics::{CharacterMetrics, get_character_metrics};
use crate::fonts::FontName;
use crate::options::Options;
use crate::parse_error::{Error, ParseError, Result, ierr};
use crate::parse_node::{Measurement, Mode};
use crate::symbols::{self, SymbolFont};
use crate::units::{calculate_size, make_em, make_em_str};
use crate::utils::{first_code_unit, js_num};
use crate::wide_character::wide_character_font;

/// `lookupSymbol(value, fontName, mode)`.
pub fn lookup_symbol(
    value: &str,
    font: FontName,
    mode: Mode,
) -> (String, Option<CharacterMetrics>) {
    let mut value = value;
    if let Some(info) = symbols::get(mode, value)
        && let Some(r) = info.replace
    {
        value = r;
    }
    (value.to_string(), get_character_metrics(value, font, mode))
}

/// `makeSymbol(value, fontName, mode, options, classes)`.
pub fn make_symbol(
    value: &str,
    font: FontName,
    mode: Mode,
    options: Option<&Options>,
    classes: Vec<&'static str>,
) -> SymbolNode {
    let (value, metrics) = lookup_symbol(value, font, mode);
    let mut node = if let Some(m) = metrics {
        let mut italic = m.italic;
        if mode == Mode::Text || options.map(|o| o.font == "mathit").unwrap_or(false) {
            italic = 0.0;
        }
        SymbolNode::new(
            value,
            m.height,
            m.depth,
            italic,
            m.skew,
            m.width,
            classes,
            CssStyle::new(),
        )
    } else {
        SymbolNode::new(value, 0.0, 0.0, 0.0, 0.0, 0.0, classes, CssStyle::new())
    };
    if let Some(o) = options {
        node.max_font_size = o.size_multiplier;
        if o.style.is_tight() {
            node.classes.push("mtight");
        }
        if let Some(c) = o.get_color() {
            node.style.set(CssProp::Color, CssVal::Str(c));
        }
    }
    node
}

fn concat(mut a: Vec<&'static str>, b: &[&'static str]) -> Vec<&'static str> {
    a.extend_from_slice(b);
    a
}

/// `mathsym(value, mode, options, classes)`.
pub fn mathsym(
    value: &str,
    mode: Mode,
    options: &Options,
    classes: Vec<&'static str>,
) -> Result<SymbolNode> {
    if options.font == "boldsymbol" && lookup_symbol(value, FontName::MainBold, mode).1.is_some() {
        return Ok(make_symbol(
            value,
            FontName::MainBold,
            mode,
            Some(options),
            concat(classes, &["mathbf"]),
        ));
    }
    let font = match symbols::get(mode, value) {
        Some(info) => info.font,
        None if value == "\\" => SymbolFont::Main,
        // `symbols[mode][value].font` on an undefined entry throws a TypeError.
        None => {
            return ierr(format!(
                "Cannot read properties of undefined (reading 'font') for {}",
                value
            ));
        }
    };
    if value == "\\" || font == SymbolFont::Main {
        Ok(make_symbol(
            value,
            FontName::MainRegular,
            mode,
            Some(options),
            classes,
        ))
    } else {
        Ok(make_symbol(
            value,
            FontName::AmsRegular,
            mode,
            Some(options),
            concat(classes, &["amsrm"]),
        ))
    }
}

/// `boldSymbol(value, mode, type)`.
fn bold_symbol(value: &str, mode: Mode, is_textord: bool) -> (FontName, &'static str) {
    if !is_textord
        && lookup_symbol(value, FontName::MathBoldItalic, mode)
            .1
            .is_some()
    {
        (FontName::MathBoldItalic, "boldsymbol")
    } else {
        (FontName::MainBold, "mathbf")
    }
}

/// `fontMap[font].fontName`.
pub fn font_map(font: &str) -> Option<FontName> {
    Some(match font {
        "mathbf" => FontName::MainBold,
        "mathrm" => FontName::MainRegular,
        "textit" => FontName::MainItalic,
        "mathit" => FontName::MainItalic,
        "mathnormal" => FontName::MathItalic,
        "mathsfit" => FontName::SansSerifItalic,
        "mathbb" => FontName::AmsRegular,
        "mathcal" => FontName::CaligraphicRegular,
        "mathfrak" => FontName::FrakturRegular,
        "mathscr" => FontName::ScriptRegular,
        "mathsf" => FontName::SansSerifRegular,
        "mathtt" => FontName::TypewriterRegular,
        _ => return None,
    })
}

/// `retrieveTextFontName(fontFamily, fontWeight, fontShape)` as KaTeX's string.
pub fn retrieve_text_font_name(font_family: &str, font_weight: &str, font_shape: &str) -> String {
    let base = match font_family {
        "amsrm" => "AMS",
        "textrm" => "Main",
        "textsf" => "SansSerif",
        "texttt" => "Typewriter",
        other => other,
    };
    let st = if font_weight == "textbf" && font_shape == "textit" {
        "BoldItalic"
    } else if font_weight == "textbf" {
        "Bold"
    } else if font_shape == "textit" {
        "Italic"
    } else {
        "Regular"
    };
    format!("{}-{}", base, st)
}

/// A font name KaTeX would look metrics up for; unknown names throw a plain `Error` there.
pub fn metric_font(name: &str) -> Result<FontName> {
    match FontName::parse(name) {
        Some(f) => Ok(f),
        None => ierr(format!("Font metrics not found for font: {}.", name)),
    }
}

/// The node types `makeOrd` accepts.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum OrdType {
    MathOrd,
    TextOrd,
}

/// `makeOrd(group, options)` for a `spacing`/`mathord`/`textord` node (`type` already
/// mapped: anything but mathord is textord).
pub fn make_ord(text: &str, mode: Mode, ord_type: OrdType, options: &Options) -> Result<HtmlNode> {
    let classes: Vec<&'static str> = vec!["mord"];
    let font = options.font;
    let font_family = options.font_family;
    let font_weight = options.font_weight;
    let font_shape = options.font_shape;
    let use_font = mode == Mode::Math || (mode == Mode::Text && !font.is_empty());
    let font_or_family = if use_font { font } else { font_family };
    if first_code_unit(text) == Some(0xD835) {
        let (wide_font, class) =
            wide_character_font(text, mode).map_err(|m| Error::Parse(ParseError::new(m, None)))?;
        if let Some(wf) = wide_font {
            return Ok(HtmlNode::Symbol(Box::new(make_symbol(
                text,
                wf,
                mode,
                Some(options),
                concat(classes, &[class]),
            ))));
        }
    }
    if !font_or_family.is_empty() {
        let font_name;
        let font_classes: Vec<&'static str>;
        if font_or_family == "boldsymbol" {
            let (f, c) = bold_symbol(text, mode, ord_type == OrdType::TextOrd);
            font_name = f;
            font_classes = vec![c];
        } else if use_font {
            font_name = match font_map(font) {
                Some(f) => f,
                None => {
                    return ierr(format!(
                        "Cannot read properties of undefined (reading 'fontName') for {}",
                        font
                    ));
                }
            };
            font_classes = vec![font];
        } else {
            font_name = metric_font(&retrieve_text_font_name(
                font_family,
                font_weight,
                font_shape,
            ))?;
            font_classes = vec![font_family, font_weight, font_shape];
        }
        if lookup_symbol(text, font_name, mode).1.is_some() {
            return Ok(HtmlNode::Symbol(Box::new(make_symbol(
                text,
                font_name,
                mode,
                Some(options),
                concat(classes, &font_classes),
            ))));
        } else if symbols::is_ligature(text) && font_name.as_str().starts_with("Typewriter") {
            let parts: Vec<HtmlNode> = text
                .chars()
                .map(|c| {
                    HtmlNode::Symbol(Box::new(make_symbol(
                        &c.to_string(),
                        font_name,
                        mode,
                        Some(options),
                        concat(classes.clone(), &font_classes),
                    )))
                })
                .collect();
            return Ok(make_fragment(parts));
        }
    }
    match ord_type {
        OrdType::MathOrd => Ok(HtmlNode::Symbol(Box::new(make_symbol(
            text,
            FontName::MathItalic,
            mode,
            Some(options),
            concat(classes, &["mathnormal"]),
        )))),
        OrdType::TextOrd => {
            let font = symbols::get(mode, text).map(|i| i.font);
            if font == Some(SymbolFont::Ams) {
                let font_name =
                    metric_font(&retrieve_text_font_name("amsrm", font_weight, font_shape))?;
                Ok(HtmlNode::Symbol(Box::new(make_symbol(
                    text,
                    font_name,
                    mode,
                    Some(options),
                    concat(classes, &["amsrm", font_weight, font_shape]),
                ))))
            } else {
                let font_name =
                    metric_font(&retrieve_text_font_name("textrm", font_weight, font_shape))?;
                Ok(HtmlNode::Symbol(Box::new(make_symbol(
                    text,
                    font_name,
                    mode,
                    Some(options),
                    concat(classes, &[font_weight, font_shape]),
                ))))
            }
        }
    }
}

/// `createClass(classes)` equality, as `canCombine` compares.
fn class_string_eq(a: &[&str], b: &[&str]) -> bool {
    let mut ia = a.iter().filter(|c| !c.is_empty());
    let mut ib = b.iter().filter(|c| !c.is_empty());
    loop {
        match (ia.next(), ib.next()) {
            (None, None) => return true,
            (Some(x), Some(y)) if x == y => continue,
            _ => return false,
        }
    }
}

/// `canCombine(prev, next)`.
fn can_combine(prev: &SymbolNode, next: &SymbolNode) -> bool {
    if !class_string_eq(&prev.classes, &next.classes)
        || prev.skew != next.skew
        || prev.max_font_size != next.max_font_size
        || (prev.italic != 0.0 && prev.has_class("mathnormal"))
    {
        return false;
    }
    if prev.classes.len() == 1 {
        let cls = prev.classes[0];
        if cls == "mbin" || cls == "mord" {
            return false;
        }
    }
    prev.style.same_as(&next.style)
}

/// `tryCombineChars(chars)`.
pub fn try_combine_chars(chars: &mut Vec<HtmlNode>) {
    let mut i = 0usize;
    while i + 1 < chars.len() {
        let combine = match (&chars[i], &chars[i + 1]) {
            (HtmlNode::Symbol(p), HtmlNode::Symbol(n)) => can_combine(p, n),
            _ => false,
        };
        if combine {
            let next = chars.remove(i + 1);
            if let (HtmlNode::Symbol(p), HtmlNode::Symbol(n)) = (&mut chars[i], next) {
                p.text.push_str(&n.text);
                p.height = p.height.max(n.height);
                p.depth = p.depth.max(n.depth);
                p.italic = n.italic;
            }
            // `i--` then `i++`: re-test the same position.
        } else {
            i += 1;
        }
    }
}

/// `sizeElementFromChildren` → (height, depth, maxFontSize).
pub fn size_from_children(children: &[HtmlNode]) -> (f64, f64, f64) {
    let (mut h, mut d, mut m) = (0.0f64, 0.0f64, 0.0f64);
    for c in children {
        if c.height() > h {
            h = c.height();
        }
        if c.depth() > d {
            d = c.depth();
        }
        if c.max_font_size() > m {
            m = c.max_font_size();
        }
    }
    (h, d, m)
}

/// `makeSpan(classes, children, options, style)` as a [`Span`].
pub fn make_span_s(
    classes: Vec<&'static str>,
    children: Vec<HtmlNode>,
    options: Option<&Options>,
    style: CssStyle,
) -> Span {
    let mut span = Span::new(classes, children, options, style);
    let (h, d, m) = size_from_children(&span.children);
    span.height = h;
    span.depth = d;
    span.max_font_size = m;
    span
}

/// `makeSpan(classes, children, options, style)`.
pub fn make_span(
    classes: Vec<&'static str>,
    children: Vec<HtmlNode>,
    options: Option<&Options>,
    style: CssStyle,
) -> HtmlNode {
    HtmlNode::span(make_span_s(classes, children, options, style))
}

/// `makeSvgSpan(classes, children, options, style)` (no sizing from children).
pub fn make_svg_span(
    classes: Vec<&'static str>,
    children: Vec<SvgNode>,
    options: Option<&Options>,
    style: CssStyle,
) -> Span {
    Span::new(
        classes,
        children
            .into_iter()
            .map(|c| HtmlNode::Svg(Box::new(c)))
            .collect(),
        options,
        style,
    )
}

/// `makeLineSpan(className, options, thickness)`.
pub fn make_line_span(class_name: &'static str, options: &Options, thickness: Option<f64>) -> Span {
    let mut line = make_span_s(vec![class_name], vec![], Some(options), CssStyle::new());
    let t = match thickness {
        Some(t) if t != 0.0 => t,
        _ => options.font_metrics().default_rule_thickness,
    };
    line.height = t.max(options.min_rule_thickness);
    line.style
        .set(CssProp::BorderBottomWidth, make_em(line.height));
    line.max_font_size = 1.0;
    line
}

/// `makeAnchor(href, classes, children, options)`.
pub fn make_anchor(
    href: &str,
    classes: Vec<&'static str>,
    children: Vec<HtmlNode>,
    options: &Options,
) -> HtmlNode {
    let mut a = Span::new(classes, children, Some(options), CssStyle::new());
    a.set_attribute("href", href);
    let (h, d, m) = size_from_children(&a.children);
    a.height = h;
    a.depth = d;
    a.max_font_size = m;
    HtmlNode::Anchor(Box::new(a))
}

/// `makeFragment(children)`.
pub fn make_fragment(children: Vec<HtmlNode>) -> HtmlNode {
    let (h, d, m) = size_from_children(&children);
    HtmlNode::Fragment(Box::new(Fragment {
        children,
        height: h,
        depth: d,
        max_font_size: m,
        ..Default::default()
    }))
}

/// `wrapFragment(group, options)`.
pub fn wrap_fragment(group: HtmlNode, options: &Options) -> HtmlNode {
    if group.is_fragment() {
        make_span(vec![], vec![group], Some(options), CssStyle::new())
    } else {
        group
    }
}

/// A `VListElem` / `VListElemAndShift`.
pub struct VListElem {
    pub elem: HtmlNode,
    /// Only for `individualShift`.
    pub shift: f64,
    pub margin_left: Option<CssVal>,
    pub margin_right: Option<CssVal>,
    pub wrapper_classes: Vec<&'static str>,
    pub wrapper_style: CssStyle,
}

impl VListElem {
    pub fn new(elem: HtmlNode) -> VListElem {
        VListElem {
            elem,
            shift: 0.0,
            margin_left: None,
            margin_right: None,
            wrapper_classes: vec![],
            wrapper_style: CssStyle::new(),
        }
    }

    pub fn shifted(elem: HtmlNode, shift: f64) -> VListElem {
        VListElem {
            shift,
            ..VListElem::new(elem)
        }
    }
}

/// `VListChild`.
pub enum VListChild {
    Elem(VListElem),
    Kern(f64),
}

impl VListChild {
    pub fn elem(elem: HtmlNode) -> VListChild {
        VListChild::Elem(VListElem::new(elem))
    }
}

/// `VListParam`.
pub enum VListParam {
    IndividualShift(Vec<VListElem>),
    Top(f64, Vec<VListChild>),
    Bottom(f64, Vec<VListChild>),
    Shift(f64, Vec<VListChild>),
    FirstBaseline(Vec<VListChild>),
}

/// `getVListChildrenAndDepth(params)`.
fn vlist_children_and_depth(params: VListParam) -> Result<(Vec<VListChild>, f64)> {
    match params {
        VListParam::IndividualShift(old) => {
            let mut children = Vec::with_capacity(old.len() * 2);
            let mut it = old.into_iter();
            let first = it
                .next()
                .ok_or_else(|| Error::Internal("empty vlist".into()))?;
            let depth = -first.shift - first.elem.depth();
            let mut curr_pos = depth;
            let mut prev_hd = first.elem.height() + first.elem.depth();
            children.push(VListChild::Elem(first));
            for c in it {
                let diff = -c.shift - curr_pos - c.elem.depth();
                let size = diff - prev_hd;
                curr_pos += diff;
                prev_hd = c.elem.height() + c.elem.depth();
                children.push(VListChild::Kern(size));
                children.push(VListChild::Elem(c));
            }
            Ok((children, depth))
        }
        VListParam::Top(pos, children) => {
            let mut bottom = pos;
            for c in &children {
                bottom -= match c {
                    VListChild::Kern(s) => *s,
                    VListChild::Elem(e) => e.elem.height() + e.elem.depth(),
                };
            }
            Ok((children, bottom))
        }
        VListParam::Bottom(pos, children) => Ok((children, -pos)),
        VListParam::Shift(pos, children) => {
            let depth = match children.first() {
                Some(VListChild::Elem(e)) => -e.elem.depth() - pos,
                _ => return ierr("First child must have type \"elem\"."),
            };
            Ok((children, depth))
        }
        VListParam::FirstBaseline(children) => {
            let depth = match children.first() {
                Some(VListChild::Elem(e)) => -e.elem.depth(),
                _ => return ierr("First child must have type \"elem\"."),
            };
            Ok((children, depth))
        }
    }
}

/// `makeVList(params, options)`.
pub fn make_vlist(params: VListParam) -> Result<Span> {
    let (children, depth) = vlist_children_and_depth(params)?;
    let mut pstrut_size = 0.0f64;
    for c in &children {
        if let VListChild::Elem(e) = c {
            pstrut_size = pstrut_size.max(e.elem.max_font_size()).max(e.elem.height());
        }
    }
    pstrut_size += 2.0;
    let mut pstrut = make_span_s(vec!["pstrut"], vec![], None, CssStyle::new());
    pstrut.style.set(CssProp::Height, make_em(pstrut_size));
    let mut real_children = Vec::new();
    let mut min_pos = depth;
    let mut max_pos = depth;
    let mut curr_pos = depth;
    for c in children {
        match c {
            VListChild::Kern(size) => curr_pos += size,
            VListChild::Elem(e) => {
                let (eh, ed) = (e.elem.height(), e.elem.depth());
                let mut wrap = make_span_s(
                    e.wrapper_classes,
                    vec![HtmlNode::span(pstrut.clone()), e.elem],
                    None,
                    e.wrapper_style,
                );
                wrap.style
                    .set(CssProp::Top, make_em(-pstrut_size - curr_pos - ed));
                if let Some(ml) = e.margin_left {
                    wrap.style.set(CssProp::MarginLeft, ml);
                }
                if let Some(mr) = e.margin_right {
                    wrap.style.set(CssProp::MarginRight, mr);
                }
                real_children.push(HtmlNode::span(wrap));
                curr_pos += eh + ed;
            }
        }
        min_pos = min_pos.min(curr_pos);
        max_pos = max_pos.max(curr_pos);
    }
    let mut vlist = make_span_s(vec!["vlist"], real_children, None, CssStyle::new());
    vlist.style.set(CssProp::Height, make_em(max_pos));
    let rows = if min_pos < 0.0 {
        let empty = make_span(vec![], vec![], None, CssStyle::new());
        let mut depth_strut = make_span_s(vec!["vlist"], vec![empty], None, CssStyle::new());
        depth_strut.style.set(CssProp::Height, make_em(-min_pos));
        let top_strut = make_span(
            vec!["vlist-s"],
            vec![HtmlNode::Symbol(Box::new(SymbolNode::new(
                "\u{200b}",
                0.0,
                0.0,
                0.0,
                0.0,
                0.0,
                vec![],
                CssStyle::new(),
            )))],
            None,
            CssStyle::new(),
        );
        vec![
            make_span(
                vec!["vlist-r"],
                vec![HtmlNode::span(vlist), top_strut],
                None,
                CssStyle::new(),
            ),
            make_span(
                vec!["vlist-r"],
                vec![HtmlNode::span(depth_strut)],
                None,
                CssStyle::new(),
            ),
        ]
    } else {
        vec![make_span(
            vec!["vlist-r"],
            vec![HtmlNode::span(vlist)],
            None,
            CssStyle::new(),
        )]
    };
    let two = rows.len() == 2;
    let mut vtable = make_span_s(vec!["vlist-t"], rows, None, CssStyle::new());
    if two {
        vtable.classes.push("vlist-t2");
    }
    vtable.height = max_pos;
    vtable.depth = -min_pos;
    Ok(vtable)
}

/// `makeGlue(measurement, options)`.
pub fn make_glue(measurement: &Measurement, options: &Options) -> Result<HtmlNode> {
    let mut rule = make_span_s(vec!["mspace"], vec![], Some(options), CssStyle::new());
    let size = calculate_size(measurement, options)?;
    rule.style.set(CssProp::MarginRight, make_em(size));
    Ok(HtmlNode::span(rule))
}

/// `svgData`.
fn svg_data(value: &str) -> Option<(&'static str, f64, f64)> {
    Some(match value {
        "vec" => ("vec", 0.471, 0.714),
        "oiintSize1" => ("oiintSize1", 0.957, 0.499),
        "oiintSize2" => ("oiintSize2", 1.472, 0.659),
        "oiiintSize1" => ("oiiintSize1", 1.304, 0.499),
        "oiiintSize2" => ("oiiintSize2", 1.98, 0.659),
        _ => return None,
    })
}

/// `staticSvg(value, options)`.
pub fn static_svg(value: &str, options: &Options) -> Result<Span> {
    let Some((path_name, width, height)) = svg_data(value) else {
        return ierr(format!("svgData has no {}", value));
    };
    let svg = SvgNode {
        children: vec![SvgChild::Path {
            name: path_name,
            alternate: None,
        }],
        attributes: vec![
            ("width", make_em_str(width)),
            ("height", make_em_str(height)),
            ("style", format!("width:{}", make_em_str(width))),
            (
                "viewBox",
                format!("0 0 {} {}", js_num(1000.0 * width), js_num(1000.0 * height)),
            ),
            ("preserveAspectRatio", "xMinYMin".to_string()),
        ],
        style: CssStyle::new(),
    };
    let mut span = make_svg_span(
        vec!["katex-overlay"],
        vec![svg],
        Some(options),
        CssStyle::new(),
    );
    span.height = height;
    span.style.set(CssProp::Height, make_em(height));
    span.style.set(CssProp::Width, make_em(width));
    Ok(span)
}

/// Convenience: `Err` for a ParseError without location.
pub fn parse_error<T>(msg: impl Into<String>) -> Result<T> {
    Err(Error::Parse(ParseError::new(msg, None)))
}
