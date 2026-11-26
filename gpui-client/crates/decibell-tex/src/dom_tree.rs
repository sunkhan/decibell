//! Port of KaTeX `src/domTree.ts` and `src/tree.ts` (HTML side only): the box tree the
//! builders produce, with `toMarkup` (used to diff the port against KaTeX's own output).

use std::rc::Rc;

use crate::options::Options;
use crate::svg_geometry;
use crate::unicode_scripts::script_from_codepoint;
use crate::utils::{escape_into, first_code_unit, write_js_num};

/// A CSS property KaTeX sets (`CssStyle`'s keys).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum CssProp {
    BackgroundColor,
    BorderBottomWidth,
    BorderColor,
    BorderRightStyle,
    BorderRightWidth,
    BorderTopWidth,
    BorderStyle,
    BorderWidth,
    Bottom,
    Color,
    Height,
    Left,
    Margin,
    MarginLeft,
    MarginRight,
    MarginTop,
    MinWidth,
    PaddingLeft,
    Position,
    TextShadow,
    Top,
    Width,
    VerticalAlign,
}

impl CssProp {
    /// The hyphenated name `toMarkup` writes.
    pub fn css_name(self) -> &'static str {
        use CssProp::*;
        match self {
            BackgroundColor => "background-color",
            BorderBottomWidth => "border-bottom-width",
            BorderColor => "border-color",
            BorderRightStyle => "border-right-style",
            BorderRightWidth => "border-right-width",
            BorderTopWidth => "border-top-width",
            BorderStyle => "border-style",
            BorderWidth => "border-width",
            Bottom => "bottom",
            Color => "color",
            Height => "height",
            Left => "left",
            Margin => "margin",
            MarginLeft => "margin-left",
            MarginRight => "margin-right",
            MarginTop => "margin-top",
            MinWidth => "min-width",
            PaddingLeft => "padding-left",
            Position => "position",
            TextShadow => "text-shadow",
            Top => "top",
            Width => "width",
            VerticalAlign => "vertical-align",
        }
    }
}

/// A style value: an `em` length as `makeEm` writes it (already rounded), or a string.
#[derive(Clone, Debug, PartialEq)]
pub enum CssVal {
    Em(f64),
    Str(Rc<str>),
    Static(&'static str),
}

impl CssVal {
    pub fn write(&self, out: &mut String) {
        match self {
            CssVal::Em(v) => {
                write_js_num(out, *v);
                out.push_str("em");
            }
            CssVal::Str(s) => out.push_str(s),
            CssVal::Static(s) => out.push_str(s),
        }
    }

    pub fn to_string_value(&self) -> String {
        let mut s = String::new();
        self.write(&mut s);
        s
    }

    /// The em value, if this is one.
    pub fn em(&self) -> Option<f64> {
        match self {
            CssVal::Em(v) => Some(*v),
            _ => None,
        }
    }

    fn same(&self, other: &CssVal) -> bool {
        match (self, other) {
            (CssVal::Em(a), CssVal::Em(b)) => a == b,
            (CssVal::Em(_), _) | (_, CssVal::Em(_)) => {
                self.to_string_value() == other.to_string_value()
            }
            _ => self.str_ref() == other.str_ref(),
        }
    }

    fn str_ref(&self) -> &str {
        match self {
            CssVal::Str(s) => s,
            CssVal::Static(s) => s,
            CssVal::Em(_) => "",
        }
    }
}

/// `CssStyle`: an insertion-ordered property map (JS object key order).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CssStyle(pub Vec<(CssProp, CssVal)>);

impl CssStyle {
    pub fn new() -> CssStyle {
        CssStyle(Vec::new())
    }

    pub fn get(&self, p: CssProp) -> Option<&CssVal> {
        self.0.iter().find(|e| e.0 == p).map(|e| &e.1)
    }

    /// `style[p] = v` (an existing key keeps its position).
    pub fn set(&mut self, p: CssProp, v: CssVal) {
        if let Some(e) = self.0.iter_mut().find(|e| e.0 == p) {
            e.1 = v;
        } else {
            self.0.push((p, v));
        }
    }

    pub fn with(mut self, p: CssProp, v: CssVal) -> CssStyle {
        self.set(p, v);
        self
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    /// `cssStyleToString`.
    pub fn write(&self, out: &mut String) {
        for (p, v) in &self.0 {
            out.push_str(p.css_name());
            out.push(':');
            v.write(out);
            out.push(';');
        }
    }

    /// Key-by-key equality in both directions (`canCombine`).
    pub fn same_as(&self, other: &CssStyle) -> bool {
        let check = |a: &CssStyle, b: &CssStyle| {
            a.0.iter().all(|(p, v)| match b.get(*p) {
                Some(w) => v.same(w),
                None => false,
            })
        };
        check(self, other) && check(other, self)
    }
}

/// `Span` (and `Anchor`, whose markup tag is `a`).
#[derive(Clone, Debug, Default)]
pub struct Span {
    pub classes: Vec<&'static str>,
    pub children: Vec<HtmlNode>,
    pub attributes: Vec<(&'static str, String)>,
    pub height: f64,
    pub depth: f64,
    pub width: Option<f64>,
    pub max_font_size: f64,
    pub style: CssStyle,
    pub italic: f64,
    /// `isMiddle` (functions/delimsizing.ts): set on a `\middle` delimiter so the enclosing
    /// `\left…\right` rebuilds it at the final height. Never written to markup.
    pub is_middle: Option<Box<IsMiddle>>,
}

/// The `isMiddle` payload: the delimiter and the options it was built with.
#[derive(Clone, Debug)]
pub struct IsMiddle {
    pub delim: String,
    pub options: Options,
}

impl Span {
    /// `new Span(classes, children, options, style)` (no sizing from children).
    pub fn new(
        classes: Vec<&'static str>,
        children: Vec<HtmlNode>,
        options: Option<&Options>,
        style: CssStyle,
    ) -> Span {
        let mut s = Span {
            classes,
            children,
            style,
            ..Default::default()
        };
        if let Some(o) = options {
            if o.style.is_tight() {
                s.classes.push("mtight");
            }
            if let Some(c) = o.get_color() {
                s.style.set(CssProp::Color, CssVal::Str(c));
            }
        }
        s
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.classes.contains(&c)
    }

    pub fn set_attribute(&mut self, name: &'static str, value: impl Into<String>) {
        let value = value.into();
        if let Some(e) = self.attributes.iter_mut().find(|e| e.0 == name) {
            e.1 = value;
        } else {
            self.attributes.push((name, value));
        }
    }
}

/// `DocumentFragment`.
#[derive(Clone, Debug, Default)]
pub struct Fragment {
    pub children: Vec<HtmlNode>,
    pub height: f64,
    pub depth: f64,
    pub max_font_size: f64,
    /// KaTeX gives fragments `classes` and `style` that are never written out.
    pub classes: Vec<&'static str>,
    pub style: CssStyle,
}

/// `SymbolNode`.
#[derive(Clone, Debug, Default)]
pub struct SymbolNode {
    pub text: String,
    pub height: f64,
    pub depth: f64,
    pub italic: f64,
    pub skew: f64,
    pub width: f64,
    pub max_font_size: f64,
    pub classes: Vec<&'static str>,
    pub style: CssStyle,
}

impl SymbolNode {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        text: impl Into<String>,
        height: f64,
        depth: f64,
        italic: f64,
        skew: f64,
        width: f64,
        classes: Vec<&'static str>,
        style: CssStyle,
    ) -> SymbolNode {
        let mut text = text.into();
        let mut classes = classes;
        if let Some(cu) = first_code_unit(&text)
            && let Some(script) = script_from_codepoint(cu as u32)
        {
            classes.push(fallback_class(script));
        }
        // iCombinations
        if text.contains(['î', 'ï', 'í', 'ì']) {
            text = match text.as_str() {
                "î" => "\u{0131}\u{0302}".to_string(),
                "ï" => "\u{0131}\u{0308}".to_string(),
                "í" => "\u{0131}\u{0301}".to_string(),
                "ì" => "\u{0131}\u{0300}".to_string(),
                // KaTeX maps through the table and gets `undefined` for longer strings.
                _ => "undefined".to_string(),
            };
        }
        SymbolNode {
            text,
            height,
            depth,
            italic,
            skew,
            width,
            max_font_size: 0.0,
            classes,
            style,
        }
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.classes.contains(&c)
    }
}

/// `script + "_fallback"`.
fn fallback_class(script: &str) -> &'static str {
    match script {
        "latin" => "latin_fallback",
        "cyrillic" => "cyrillic_fallback",
        "armenian" => "armenian_fallback",
        "brahmic" => "brahmic_fallback",
        "georgian" => "georgian_fallback",
        "cjk" => "cjk_fallback",
        "hangul" => "hangul_fallback",
        _ => "unknown_fallback",
    }
}

/// `SvgNode` children.
#[derive(Clone, Debug)]
pub enum SvgChild {
    /// `PathNode`: a named path from `svgGeometry.path`, or an explicit `alternate`.
    Path {
        name: &'static str,
        alternate: Option<String>,
    },
    /// `LineNode`.
    Line {
        attributes: Vec<(&'static str, String)>,
    },
}

impl SvgChild {
    /// The path data a `PathNode` writes.
    pub fn path_data(&self) -> Option<&str> {
        match self {
            SvgChild::Path {
                alternate: Some(a), ..
            } => Some(a),
            SvgChild::Path {
                name,
                alternate: None,
            } => Some(svg_geometry::path(name).unwrap_or("undefined")),
            SvgChild::Line { .. } => None,
        }
    }
}

/// `SvgNode`.
#[derive(Clone, Debug, Default)]
pub struct SvgNode {
    pub children: Vec<SvgChild>,
    pub attributes: Vec<(&'static str, String)>,
    /// Not part of KaTeX's SvgNode; a sink so `style_mut` is total.
    pub style: CssStyle,
}

impl SvgNode {
    pub fn attr(&self, name: &str) -> Option<&str> {
        self.attributes
            .iter()
            .find(|e| e.0 == name)
            .map(|e| e.1.as_str())
    }
}

/// `HtmlDomNode` (plus the SVG nodes a `Span<SvgNode>` holds).
#[derive(Clone, Debug)]
pub enum HtmlNode {
    Span(Box<Span>),
    Anchor(Box<Span>),
    Fragment(Box<Fragment>),
    Symbol(Box<SymbolNode>),
    Svg(Box<SvgNode>),
}

impl HtmlNode {
    pub fn span(s: Span) -> HtmlNode {
        HtmlNode::Span(Box::new(s))
    }

    pub fn height(&self) -> f64 {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => s.height,
            HtmlNode::Fragment(f) => f.height,
            HtmlNode::Symbol(s) => s.height,
            HtmlNode::Svg(_) => 0.0,
        }
    }

    pub fn depth(&self) -> f64 {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => s.depth,
            HtmlNode::Fragment(f) => f.depth,
            HtmlNode::Symbol(s) => s.depth,
            HtmlNode::Svg(_) => 0.0,
        }
    }

    pub fn max_font_size(&self) -> f64 {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => s.max_font_size,
            HtmlNode::Fragment(f) => f.max_font_size,
            HtmlNode::Symbol(s) => s.max_font_size,
            HtmlNode::Svg(_) => 0.0,
        }
    }

    pub fn set_height(&mut self, h: f64) {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => s.height = h,
            HtmlNode::Fragment(f) => f.height = h,
            HtmlNode::Symbol(s) => s.height = h,
            HtmlNode::Svg(_) => {}
        }
    }

    pub fn set_depth(&mut self, d: f64) {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => s.depth = d,
            HtmlNode::Fragment(f) => f.depth = d,
            HtmlNode::Symbol(s) => s.depth = d,
            HtmlNode::Svg(_) => {}
        }
    }

    pub fn set_max_font_size(&mut self, m: f64) {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => s.max_font_size = m,
            HtmlNode::Fragment(f) => f.max_font_size = m,
            HtmlNode::Symbol(s) => s.max_font_size = m,
            HtmlNode::Svg(_) => {}
        }
    }

    pub fn classes(&self) -> &[&'static str] {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => &s.classes,
            HtmlNode::Symbol(s) => &s.classes,
            HtmlNode::Fragment(f) => &f.classes,
            HtmlNode::Svg(_) => &[],
        }
    }

    pub fn classes_mut(&mut self) -> Option<&mut Vec<&'static str>> {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => Some(&mut s.classes),
            HtmlNode::Symbol(s) => Some(&mut s.classes),
            HtmlNode::Fragment(f) => Some(&mut f.classes),
            HtmlNode::Svg(_) => None,
        }
    }

    pub fn has_class(&self, c: &str) -> bool {
        self.classes().contains(&c)
    }

    /// `classes[0]` (or "" when there is none).
    pub fn first_class(&self) -> &'static str {
        self.classes().first().copied().unwrap_or("")
    }

    pub fn style(&self) -> Option<&CssStyle> {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => Some(&s.style),
            HtmlNode::Symbol(s) => Some(&s.style),
            HtmlNode::Fragment(f) => Some(&f.style),
            HtmlNode::Svg(_) => None,
        }
    }

    /// `node.style` for mutation (a fragment's style is never written out, as in KaTeX).
    pub fn style_mut(&mut self) -> &mut CssStyle {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => &mut s.style,
            HtmlNode::Symbol(s) => &mut s.style,
            HtmlNode::Fragment(f) => &mut f.style,
            HtmlNode::Svg(s) => &mut s.style,
        }
    }

    pub fn children(&self) -> Option<&Vec<HtmlNode>> {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => Some(&s.children),
            HtmlNode::Fragment(f) => Some(&f.children),
            _ => None,
        }
    }

    pub fn children_mut(&mut self) -> Option<&mut Vec<HtmlNode>> {
        match self {
            HtmlNode::Span(s) | HtmlNode::Anchor(s) => Some(&mut s.children),
            HtmlNode::Fragment(f) => Some(&mut f.children),
            _ => None,
        }
    }

    pub fn as_span(&self) -> Option<&Span> {
        match self {
            HtmlNode::Span(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_span_mut(&mut self) -> Option<&mut Span> {
        match self {
            HtmlNode::Span(s) => Some(s),
            _ => None,
        }
    }

    pub fn as_symbol(&self) -> Option<&SymbolNode> {
        match self {
            HtmlNode::Symbol(s) => Some(s),
            _ => None,
        }
    }

    pub fn is_fragment(&self) -> bool {
        matches!(self, HtmlNode::Fragment(_))
    }

    /// `toMarkup()`.
    pub fn to_markup(&self, out: &mut String) {
        match self {
            HtmlNode::Span(s) => span_markup(s, "span", out),
            HtmlNode::Anchor(s) => span_markup(s, "a", out),
            HtmlNode::Fragment(f) => {
                for c in &f.children {
                    c.to_markup(out);
                }
            }
            HtmlNode::Symbol(s) => symbol_markup(s, out),
            HtmlNode::Svg(s) => svg_markup(s, out),
        }
    }
}

fn write_classes(classes: &[&str], out: &mut String) {
    let mut first = true;
    let mut joined = String::new();
    for c in classes.iter().filter(|c| !c.is_empty()) {
        if !first {
            joined.push(' ');
        }
        joined.push_str(c);
        first = false;
    }
    escape_into(out, &joined);
}

fn span_markup(s: &Span, tag: &str, out: &mut String) {
    out.push('<');
    out.push_str(tag);
    if !s.classes.is_empty() {
        out.push_str(" class=\"");
        write_classes(&s.classes, out);
        out.push('"');
    }
    if !s.style.is_empty() {
        let mut st = String::new();
        s.style.write(&mut st);
        out.push_str(" style=\"");
        escape_into(out, &st);
        out.push('"');
    }
    for (k, v) in &s.attributes {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_into(out, v);
        out.push('"');
    }
    out.push('>');
    for c in &s.children {
        c.to_markup(out);
    }
    out.push_str("</");
    out.push_str(tag);
    out.push('>');
}

fn symbol_markup(s: &SymbolNode, out: &mut String) {
    let mut needs_span = false;
    let mut markup = String::from("<span");
    if !s.classes.is_empty() {
        needs_span = true;
        markup.push_str(" class=\"");
        write_classes(&s.classes, &mut markup);
        markup.push('"');
    }
    let mut styles = String::new();
    if s.italic > 0.0 {
        styles.push_str("margin-right:");
        crate::units::make_em(s.italic).write(&mut styles);
        styles.push(';');
    }
    s.style.write(&mut styles);
    if !styles.is_empty() {
        needs_span = true;
        markup.push_str(" style=\"");
        escape_into(&mut markup, &styles);
        markup.push('"');
    }
    if needs_span {
        out.push_str(&markup);
        out.push('>');
        escape_into(out, &s.text);
        out.push_str("</span>");
    } else {
        escape_into(out, &s.text);
    }
}

fn svg_markup(s: &SvgNode, out: &mut String) {
    out.push_str("<svg xmlns=\"http://www.w3.org/2000/svg\"");
    for (k, v) in &s.attributes {
        out.push(' ');
        out.push_str(k);
        out.push_str("=\"");
        escape_into(out, v);
        out.push('"');
    }
    out.push('>');
    for c in &s.children {
        match c {
            SvgChild::Path { .. } => {
                out.push_str("<path d=\"");
                escape_into(out, c.path_data().unwrap());
                out.push_str("\"/>");
            }
            SvgChild::Line { attributes } => {
                out.push_str("<line");
                for (k, v) in attributes {
                    out.push(' ');
                    out.push_str(k);
                    out.push_str("=\"");
                    escape_into(out, v);
                    out.push('"');
                }
                out.push_str("/>");
            }
        }
    }
    out.push_str("</svg>");
}
