//! Port of KaTeX `src/functions/accent.ts`.

use crate::build_common::{
    OrdType, VListChild, VListElem, VListParam, make_ord, make_span, make_span_s, make_vlist,
    static_svg,
};
use crate::build_html::build_group;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode, SymbolNode};
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::{AccentNode, Mode, ParseNode, SupSubNode, normalize_argument, type_error};
use crate::stretchy::stretchy_svg;
use crate::units::{make_em, make_em_str};
use crate::utils::is_character_box;

/// `getBaseSymbol(group)`: the symbol at the bottom of a chain of single-child nodes.
fn get_base_symbol(group: &HtmlNode) -> Option<&SymbolNode> {
    if let HtmlNode::Symbol(s) = group {
        return Some(s);
    }
    match group.children() {
        Some(c) if c.len() == 1 => get_base_symbol(&c[0]),
        _ => None,
    }
}

/// `htmlBuilder` (an `HtmlBuilderSupSub`: also called by supsub for an accent with scripts).
pub fn html_builder(grp: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let group: &AccentNode;
    let accent_node: &ParseNode;
    let base: &ParseNode;
    let mut sup_sub_group = None;
    match grp {
        ParseNode::SupSub(ss) => {
            group = match ss.base.as_deref() {
                Some(n @ ParseNode::Accent(a)) => {
                    accent_node = n;
                    a
                }
                Some(other) => return type_error("accent", other),
                None => return ierr("Expected node of type accent, but got undefined"),
            };
            base = &group.base;
            // KaTeX swaps the accent's base in as the supsub's base while building it.
            let swapped = ParseNode::SupSub(SupSubNode {
                mode: ss.mode,
                base: Some(group.base.clone()),
                sup: ss.sup.clone(),
                sub: ss.sub.clone(),
            });
            match build_group(Some(&swapped), options, None)? {
                HtmlNode::Span(s) => sup_sub_group = Some(s),
                _ => return ierr("Expected span<HtmlDomNode> but got [object Object]."),
            }
        }
        ParseNode::Accent(a) => {
            group = a;
            accent_node = grp;
            base = &a.base;
        }
        other => return type_error("accent", other),
    }

    let body = build_group(Some(base), &options.having_cramped_style(), None)?;
    let must_shift = group.is_shifty && is_character_box(base);
    let mut skew = 0.0;
    if must_shift {
        skew = get_base_symbol(&body).map(|s| s.skew).unwrap_or(0.0);
    }
    let accent_below = group.label == "\\c";
    let mut clearance = if accent_below {
        body.height() + body.depth()
    } else {
        body.height().min(options.font_metrics().x_height)
    };

    let accent_body = if !group.is_stretchy {
        let accent;
        let width;
        if group.label == "\\vec" {
            accent = HtmlNode::span(static_svg("vec", options)?);
            width = 0.471; // svgData.vec[1]
        } else {
            let mut sym = match make_ord(&group.label, group.mode, OrdType::TextOrd, options)? {
                HtmlNode::Symbol(s) => s,
                _ => return ierr("Expected symbolNode but got [object Object]."),
            };
            sym.italic = 0.0;
            width = sym.width;
            if accent_below {
                clearance += sym.depth;
            }
            accent = HtmlNode::Symbol(sym);
        }
        let mut accent_body = make_span_s(vec!["accent-body"], vec![accent], None, CssStyle::new());
        let accent_full = group.label == "\\textcircled";
        if accent_full {
            accent_body.classes.push("accent-full");
            clearance = body.height();
        }
        let mut left = skew;
        if !accent_full {
            left -= width / 2.0;
        }
        accent_body.style.set(CssProp::Left, make_em(left));
        if group.label == "\\textcircled" {
            accent_body.style.set(CssProp::Top, CssVal::Static(".2em"));
        }
        make_vlist(VListParam::FirstBaseline(vec![
            VListChild::elem(body),
            VListChild::Kern(-clearance),
            VListChild::elem(HtmlNode::span(accent_body)),
        ]))?
    } else {
        let accent_body = stretchy_svg(accent_node, options)?;
        let mut e = VListElem::new(HtmlNode::span(accent_body));
        e.wrapper_classes = vec!["svg-align"];
        if skew > 0.0 {
            e.wrapper_style = CssStyle::new()
                .with(
                    CssProp::Width,
                    CssVal::Str(format!("calc(100% - {})", make_em_str(2.0 * skew)).into()),
                )
                .with(CssProp::MarginLeft, make_em(2.0 * skew));
        }
        make_vlist(VListParam::FirstBaseline(vec![
            VListChild::elem(body),
            VListChild::Elem(e),
        ]))?
    };

    let accent_wrap = make_span(
        vec!["mord", "katex-accent"],
        vec![HtmlNode::span(accent_body)],
        Some(options),
        CssStyle::new(),
    );
    match sup_sub_group {
        Some(mut ssg) => {
            let h = accent_wrap.height();
            ssg.children[0] = accent_wrap;
            ssg.height = h.max(ssg.height);
            ssg.classes[0] = "mord";
            Ok(HtmlNode::Span(ssg))
        }
        None => Ok(accent_wrap),
    }
}

/// `NON_STRETCHY_ACCENT_REGEX.test(funcName)` (an unanchored alternation of literals).
fn is_non_stretchy(name: &str) -> bool {
    const NON_STRETCHY: [&str; 11] = [
        "\\acute",
        "\\grave",
        "\\ddot",
        "\\tilde",
        "\\bar",
        "\\breve",
        "\\check",
        "\\hat",
        "\\vec",
        "\\dot",
        "\\mathring",
    ];
    NON_STRETCHY.iter().any(|a| name.contains(a))
}

fn math_accent_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let base = normalize_argument(args.into_iter().next().unwrap());
    let is_stretchy = !is_non_stretchy(ctx.func_name);
    let is_shifty = !is_stretchy
        || ctx.func_name == "\\widehat"
        || ctx.func_name == "\\widetilde"
        || ctx.func_name == "\\widecheck";
    Ok(ParseNode::Accent(Box::new(AccentNode {
        mode: ctx.parser.mode,
        label: ctx.func_name.to_string(),
        is_stretchy,
        is_shifty,
        base: Box::new(base),
    })))
}

fn text_accent_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let base = args.into_iter().next().unwrap();
    let mut mode = ctx.parser.mode;
    if mode == Mode::Math {
        mode = Mode::Text;
    }
    Ok(ParseNode::Accent(Box::new(AccentNode {
        mode,
        label: ctx.func_name.to_string(),
        is_stretchy: false,
        is_shifty: true,
        base: Box::new(base),
    })))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "accent",
        names: &[
            "\\acute",
            "\\grave",
            "\\ddot",
            "\\tilde",
            "\\bar",
            "\\breve",
            "\\check",
            "\\hat",
            "\\vec",
            "\\dot",
            "\\mathring",
            "\\widecheck",
            "\\widehat",
            "\\widetilde",
            "\\overrightarrow",
            "\\overleftarrow",
            "\\Overrightarrow",
            "\\overleftrightarrow",
            "\\overgroup",
            "\\overlinesegment",
            "\\overleftharpoon",
            "\\overrightharpoon",
        ],
        num_args: 1,
        handler: Some(math_accent_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "accent",
        names: &[
            "\\'",
            "\\`",
            "\\^",
            "\\~",
            "\\=",
            "\\u",
            "\\.",
            "\\\"",
            "\\c",
            "\\r",
            "\\H",
            "\\v",
            "\\textcircled",
        ],
        num_args: 1,
        allowed_in_text: true,
        allowed_in_math: true,
        arg_types: &[ArgType::Primitive],
        handler: Some(text_accent_handler),
        ..FunctionSpec::DEFAULT
    },
];
