//! Port of KaTeX `src/functions/op.ts` (HTML side).

use crate::build_common::{
    VListElem, VListParam, make_span, make_symbol, make_vlist, mathsym, static_svg,
};
use crate::build_html::{RealGroup, build_expression};
use crate::define_function::{FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, CssVal, HtmlNode};
use crate::fonts::FontName;
use crate::functions::assemble_sup_sub::assemble_sup_sub;
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{Mode, OpNode, ParseNode, ordargument, type_error};
use crate::style::Style;
use crate::units::make_em;

/// `htmlBuilder` (HtmlBuilderSupSub): an `op` node, or a `supsub` whose base is one.
pub fn html_builder(grp: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let (group, sup_group, sub_group, has_limits): (
        &OpNode,
        Option<&ParseNode>,
        Option<&ParseNode>,
        bool,
    ) = match grp {
        ParseNode::SupSub(s) => match s.base.as_deref() {
            Some(ParseNode::Op(o)) => (o, s.sup.as_deref(), s.sub.as_deref(), true),
            Some(other) => return type_error("op", other),
            None => return crate::parse_error::ierr("Expected node of type op, but got undefined"),
        },
        ParseNode::Op(o) => (o, None, None, false),
        other => return type_error("op", other),
    };
    let style = options.style;
    let name = group.name.as_deref();
    let large = style.size == Style::DISPLAY.size && group.symbol && name != Some("\\smallint");
    let mut base: HtmlNode;
    // The name as KaTeX leaves it after the \oiint juggling (restored, so unchanged).
    if group.symbol {
        let font = if large {
            FontName::Size2Regular
        } else {
            FontName::Size1Regular
        };
        let mut stash = "";
        let mut sym_name = name.unwrap_or("");
        if sym_name == "\\oiint" || sym_name == "\\oiiint" {
            stash = &sym_name[1..];
            sym_name = if stash == "oiint" {
                "\\iint"
            } else {
                "\\iiint"
            };
        }
        let sym = make_symbol(
            sym_name,
            font,
            Mode::Math,
            Some(options),
            vec![
                "mop",
                "op-symbol",
                if large { "large-op" } else { "small-op" },
            ],
        );
        let symbol_italic = sym.italic;
        base = HtmlNode::Symbol(Box::new(sym));
        if !stash.is_empty() {
            let oval_name = match (stash, large) {
                ("oiint", false) => "oiintSize1",
                ("oiint", true) => "oiintSize2",
                ("oiiint", false) => "oiiintSize1",
                _ => "oiiintSize2",
            };
            let oval = static_svg(oval_name, options)?;
            let mut v = make_vlist(VListParam::IndividualShift(vec![
                VListElem::shifted(base, 0.0),
                VListElem::shifted(HtmlNode::span(oval), if large { 0.08 } else { 0.0 }),
            ]))?;
            v.classes.insert(0, "mop");
            v.italic = symbol_italic;
            base = HtmlNode::span(v);
        }
    } else if let Some(body) = &group.body {
        let mut inner = build_expression(body, options, RealGroup::Yes, (None, None))?;
        if inner.len() == 1 && matches!(inner[0], HtmlNode::Symbol(_)) {
            let mut b = inner.pop().unwrap();
            if let HtmlNode::Symbol(s) = &mut b {
                if s.classes.is_empty() {
                    s.classes.push("mop");
                } else {
                    s.classes[0] = "mop";
                }
            }
            base = b;
        } else {
            base = make_span(vec!["mop"], inner, Some(options), CssStyle::new());
        }
    } else {
        let n = name.unwrap_or("");
        let mut output = Vec::new();
        for c in n.chars().skip(1) {
            output.push(HtmlNode::Symbol(Box::new(mathsym(
                &c.to_string(),
                group.mode,
                options,
                vec![],
            )?)));
        }
        base = make_span(vec!["mop"], output, Some(options), CssStyle::new());
    }
    let mut base_shift = 0.0;
    let mut slant = 0.0;
    let is_oiint = matches!(name, Some("\\oiint") | Some("\\oiiint"));
    if (matches!(base, HtmlNode::Symbol(_)) || is_oiint) && !group.suppress_base_shift {
        base_shift = (base.height() - base.depth()) / 2.0 - options.font_metrics().axis_height;
        slant = match &base {
            HtmlNode::Symbol(s) => s.italic,
            HtmlNode::Span(s) => s.italic,
            _ => 0.0,
        };
    }
    if has_limits {
        Ok(HtmlNode::span(assemble_sup_sub(
            base, sup_group, sub_group, options, style, slant, base_shift,
        )?))
    } else {
        if base_shift != 0.0 {
            let st = base.style_mut();
            st.set(CssProp::Position, CssVal::Static("relative"));
            st.set(CssProp::Top, make_em(base_shift));
        }
        Ok(base)
    }
}

/// `singleCharBigOps`.
fn single_char_big_op(c: &str) -> &'static str {
    match c {
        "\u{220F}" => "\\prod",
        "\u{2210}" => "\\coprod",
        "\u{2211}" => "\\sum",
        "\u{22c0}" => "\\bigwedge",
        "\u{22c1}" => "\\bigvee",
        "\u{22c2}" => "\\bigcap",
        "\u{22c3}" => "\\bigcup",
        "\u{2a00}" => "\\bigodot",
        "\u{2a01}" => "\\bigoplus",
        "\u{2a02}" => "\\bigotimes",
        "\u{2a04}" => "\\biguplus",
        "\u{2a06}" => "\\bigsqcup",
        _ => "undefined",
    }
}

/// `singleCharIntegrals`.
fn single_char_integral(c: &str) -> &'static str {
    match c {
        "\u{222b}" => "\\int",
        "\u{222c}" => "\\iint",
        "\u{222d}" => "\\iiint",
        "\u{222e}" => "\\oint",
        "\u{222f}" => "\\oiint",
        "\u{2230}" => "\\oiiint",
        _ => "undefined",
    }
}

fn op_node(
    mode: Mode,
    limits: bool,
    symbol: bool,
    name: Option<String>,
    body: Option<Vec<ParseNode>>,
) -> ParseNode {
    ParseNode::Op(Box::new(OpNode {
        mode,
        limits,
        always_handle_sup_sub: false,
        suppress_base_shift: false,
        parent_is_sup_sub: false,
        symbol,
        name,
        body,
    }))
}

/// One UTF-16 unit long (`fName.length === 1`): all KaTeX's single-character names are BMP.
fn is_single_unit(s: &str) -> bool {
    let mut it = s.chars();
    matches!((it.next(), it.next()), (Some(c), None) if (c as u32) <= 0xFFFF)
}

fn big_op_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut f = ctx.func_name;
    if is_single_unit(f) {
        f = single_char_big_op(f);
    }
    Ok(op_node(
        ctx.parser.mode,
        true,
        true,
        Some(f.to_string()),
        None,
    ))
}

fn mathop_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let body = args.into_iter().next().unwrap();
    Ok(op_node(
        ctx.parser.mode,
        false,
        false,
        None,
        Some(ordargument(body)),
    ))
}

fn named_op_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(op_node(
        ctx.parser.mode,
        false,
        false,
        Some(ctx.func_name.to_string()),
        None,
    ))
}

fn named_limits_op_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    Ok(op_node(
        ctx.parser.mode,
        true,
        false,
        Some(ctx.func_name.to_string()),
        None,
    ))
}

fn integral_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut f = ctx.func_name;
    if is_single_unit(f) {
        f = single_char_integral(f);
    }
    Ok(op_node(
        ctx.parser.mode,
        false,
        true,
        Some(f.to_string()),
        None,
    ))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "op",
        names: &[
            "\\coprod",
            "\\bigvee",
            "\\bigwedge",
            "\\biguplus",
            "\\bigcap",
            "\\bigcup",
            "\\intop",
            "\\prod",
            "\\sum",
            "\\bigotimes",
            "\\bigoplus",
            "\\bigodot",
            "\\bigsqcup",
            "\\smallint",
            "\u{220F}",
            "\u{2210}",
            "\u{2211}",
            "\u{22c0}",
            "\u{22c1}",
            "\u{22c2}",
            "\u{22c3}",
            "\u{2a00}",
            "\u{2a01}",
            "\u{2a02}",
            "\u{2a04}",
            "\u{2a06}",
        ],
        num_args: 0,
        handler: Some(big_op_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "op",
        names: &["\\mathop"],
        num_args: 1,
        primitive: true,
        handler: Some(mathop_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "op",
        names: &[
            "\\arcsin", "\\arccos", "\\arctan", "\\arctg", "\\arcctg", "\\arg", "\\ch", "\\cos",
            "\\cosec", "\\cosh", "\\cot", "\\cotg", "\\coth", "\\csc", "\\ctg", "\\cth", "\\deg",
            "\\dim", "\\exp", "\\hom", "\\ker", "\\lg", "\\ln", "\\log", "\\sec", "\\sin",
            "\\sinh", "\\sh", "\\tan", "\\tanh", "\\tg", "\\th",
        ],
        num_args: 0,
        handler: Some(named_op_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "op",
        names: &[
            "\\det", "\\gcd", "\\inf", "\\lim", "\\max", "\\min", "\\Pr", "\\sup",
        ],
        num_args: 0,
        handler: Some(named_limits_op_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "op",
        names: &[
            "\\int", "\\iint", "\\iiint", "\\oint", "\\oiint", "\\oiiint", "\u{222b}", "\u{222c}",
            "\u{222d}", "\u{222e}", "\u{222f}", "\u{2230}",
        ],
        num_args: 0,
        allowed_in_argument: true,
        handler: Some(integral_handler),
        ..FunctionSpec::DEFAULT
    },
];
