//! Port of KaTeX `src/functions/genfrac.ts`.

use crate::build_common::{VListElem, VListParam, make_line_span, make_span, make_vlist};
use crate::build_html::{build_group, make_null_delimiter};
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::delimiter::make_custom_sized_delim;
use crate::dom_tree::{CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::{Result, ierr};
use crate::parse_node::{Atom, GenFracNode, ParseNode, StyleStr, normalize_argument, type_error};
use crate::style::Style;
use crate::units::calculate_size;

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::GenFrac(g) = group else {
        unreachable!()
    };
    let style = options.style;
    let nstyle = style.frac_num();
    let dstyle = style.frac_den();
    let new_options = options.having_style(nstyle);
    let mut numerm = build_group(Some(&g.numer), &new_options, Some(options))?;
    if g.continued {
        let h_strut = 8.5 / options.font_metrics().pt_per_em;
        let d_strut = 3.5 / options.font_metrics().pt_per_em;
        let (h, d) = (numerm.height(), numerm.depth());
        numerm.set_height(if h < h_strut { h_strut } else { h });
        numerm.set_depth(if d < d_strut { d_strut } else { d });
    }
    let new_options = options.having_style(dstyle);
    let denomm = build_group(Some(&g.denom), &new_options, Some(options))?;
    let rule;
    let rule_width;
    let rule_spacing;
    if g.has_bar_line {
        let r = if let Some(bar) = &g.bar_size {
            let w = calculate_size(bar, options)?;
            make_line_span("frac-line", options, Some(w))
        } else {
            make_line_span("frac-line", options, None)
        };
        rule_width = r.height;
        rule_spacing = r.height;
        rule = Some(r);
    } else {
        rule = None;
        rule_width = 0.0;
        rule_spacing = options.font_metrics().default_rule_thickness;
    }
    let fm = options.font_metrics();
    let (mut num_shift, clearance, mut denom_shift);
    if style.size == Style::DISPLAY.size {
        num_shift = fm.num1;
        clearance = if rule_width > 0.0 {
            3.0 * rule_spacing
        } else {
            7.0 * rule_spacing
        };
        denom_shift = fm.denom1;
    } else {
        if rule_width > 0.0 {
            num_shift = fm.num2;
            clearance = rule_spacing;
        } else {
            num_shift = fm.num3;
            clearance = 3.0 * rule_spacing;
        }
        denom_shift = fm.denom2;
    }
    let mut frac = match rule {
        None => {
            let candidate = (num_shift - numerm.depth()) - (denomm.height() - denom_shift);
            if candidate < clearance {
                num_shift += 0.5 * (clearance - candidate);
                denom_shift += 0.5 * (clearance - candidate);
            }
            make_vlist(VListParam::IndividualShift(vec![
                VListElem::shifted(denomm, denom_shift),
                VListElem::shifted(numerm, -num_shift),
            ]))?
        }
        Some(rule) => {
            let axis = fm.axis_height;
            if (num_shift - numerm.depth()) - (axis + 0.5 * rule_width) < clearance {
                num_shift += clearance - ((num_shift - numerm.depth()) - (axis + 0.5 * rule_width));
            }
            if (axis - 0.5 * rule_width) - (denomm.height() - denom_shift) < clearance {
                denom_shift +=
                    clearance - ((axis - 0.5 * rule_width) - (denomm.height() - denom_shift));
            }
            let mid_shift = -(axis - 0.5 * rule_width);
            make_vlist(VListParam::IndividualShift(vec![
                VListElem::shifted(denomm, denom_shift),
                VListElem::shifted(HtmlNode::span(rule), mid_shift),
                VListElem::shifted(numerm, -num_shift),
            ]))?
        }
    };
    let new_options = options.having_style(style);
    frac.height *= new_options.size_multiplier / options.size_multiplier;
    frac.depth *= new_options.size_multiplier / options.size_multiplier;
    let delim_size = if style.size == Style::DISPLAY.size {
        fm.delim1
    } else if style.size == Style::SCRIPTSCRIPT.size {
        options.having_style(Style::SCRIPT).font_metrics().delim2
    } else {
        fm.delim2
    };
    let left_delim = match &g.left_delim {
        None => make_null_delimiter(options, vec!["mopen"]),
        Some(d) => HtmlNode::span(make_custom_sized_delim(
            d,
            delim_size,
            true,
            &options.having_style(style),
            g.mode,
            vec!["mopen"],
        )?),
    };
    let right_delim = if g.continued {
        make_span(vec![], vec![], None, CssStyle::new())
    } else {
        match &g.right_delim {
            None => make_null_delimiter(options, vec!["mclose"]),
            Some(d) => HtmlNode::span(make_custom_sized_delim(
                d,
                delim_size,
                true,
                &options.having_style(style),
                g.mode,
                vec!["mclose"],
            )?),
        }
    };
    let mut classes = vec!["mord"];
    classes.extend(new_options.sizing_classes(options));
    let mfrac = make_span(
        vec!["mfrac"],
        vec![HtmlNode::span(frac)],
        None,
        CssStyle::new(),
    );
    Ok(make_span(
        classes,
        vec![left_delim, mfrac, right_delim],
        Some(options),
        CssStyle::new(),
    ))
}

/// `wrapWithStyle(frac, style)`.
fn wrap_with_style(frac: GenFracNode, style: Option<StyleStr>) -> ParseNode {
    let mode = frac.mode;
    let node = ParseNode::GenFrac(Box::new(frac));
    match style {
        None => node,
        Some(style) => ParseNode::Styling {
            mode,
            style,
            reset_font: false,
            body: vec![node],
        },
    }
}

fn genfrac_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let numer = it.next().unwrap();
    let denom = it.next().unwrap();
    let (has_bar_line, left_delim, right_delim) = match ctx.func_name {
        "\\cfrac" | "\\dfrac" | "\\frac" | "\\tfrac" => (true, None, None),
        "\\\\atopfrac" => (false, None, None),
        "\\dbinom" | "\\binom" | "\\tbinom" => (false, Some("("), Some(")")),
        "\\\\bracefrac" => (false, Some("\\{"), Some("\\}")),
        "\\\\brackfrac" => (false, Some("["), Some("]")),
        _ => return ierr("Unrecognized genfrac command"),
    };
    let continued = ctx.func_name == "\\cfrac";
    let style = if continued || ctx.func_name.starts_with("\\d") {
        Some(StyleStr::Display)
    } else if ctx.func_name.starts_with("\\t") {
        Some(StyleStr::Text)
    } else {
        None
    };
    Ok(wrap_with_style(
        GenFracNode {
            mode: ctx.parser.mode,
            continued,
            numer: Box::new(numer),
            denom: Box::new(denom),
            has_bar_line,
            left_delim: left_delim.map(String::from),
            right_delim: right_delim.map(String::from),
            bar_size: None,
        },
        style,
    ))
}

fn infix_handler(
    ctx: &mut FunctionContext,
    _args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let replace_with = match ctx.func_name {
        "\\over" => "\\frac",
        "\\choose" => "\\binom",
        "\\atop" => "\\\\atopfrac",
        "\\brace" => "\\\\bracefrac",
        "\\brack" => "\\\\brackfrac",
        _ => return ierr("Unrecognized infix genfrac command"),
    };
    Ok(ParseNode::Infix {
        mode: ctx.parser.mode,
        replace_with: replace_with.into(),
        size: None,
        token: ctx.token.clone().map(Box::new),
    })
}

/// `delimFromValue(delimString)`.
fn delim_from_value(s: &str) -> Option<String> {
    if !s.is_empty() && s != "." {
        Some(s.to_string())
    } else {
        None
    }
}

fn genfrac_full_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let a0 = it.next().unwrap();
    let a1 = it.next().unwrap();
    let a2 = it.next().unwrap();
    let a3 = it.next().unwrap();
    let numer = it.next().unwrap();
    let denom = it.next().unwrap();
    let left_delim = match normalize_argument(a0) {
        ParseNode::Atom(a) if a.family == Atom::Open => delim_from_value(&a.text),
        _ => None,
    };
    let right_delim = match normalize_argument(a1) {
        ParseNode::Atom(a) if a.family == Atom::Close => delim_from_value(&a.text),
        _ => None,
    };
    let ParseNode::Size {
        value, is_blank, ..
    } = a2
    else {
        return type_error("size", &a2);
    };
    let (has_bar_line, bar_size) = if is_blank {
        (true, None)
    } else {
        (value.number > 0.0, Some(value))
    };
    const STYL: [StyleStr; 4] = [
        StyleStr::Display,
        StyleStr::Text,
        StyleStr::Script,
        StyleStr::ScriptScript,
    ];
    // `stylArray[Number(text)]` (undefined → no style)
    let style_of = |t: &str| -> Option<StyleStr> {
        let n: f64 = if t.trim().is_empty() {
            0.0
        } else {
            t.trim().parse().unwrap_or(f64::NAN)
        };
        if (0.0..4.0).contains(&n) && n.fract() == 0.0 {
            Some(STYL[n as usize])
        } else {
            None
        }
    };
    let size = match &a3 {
        ParseNode::OrdGroup(g) => {
            if let Some(first) = g.body.first() {
                let ParseNode::TextOrd(t) = first else {
                    return type_error("textord", first);
                };
                style_of(&t.text)
            } else {
                None
            }
        }
        ParseNode::TextOrd(t) => style_of(&t.text),
        other => return type_error("textord", other),
    };
    Ok(wrap_with_style(
        GenFracNode {
            mode: ctx.parser.mode,
            continued: false,
            numer: Box::new(numer),
            denom: Box::new(denom),
            has_bar_line,
            left_delim,
            right_delim,
            bar_size,
        },
        size,
    ))
}

fn above_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let a = args.into_iter().next().unwrap();
    let ParseNode::Size { value, .. } = a else {
        return type_error("size", &a);
    };
    Ok(ParseNode::Infix {
        mode: ctx.parser.mode,
        replace_with: "\\\\abovefrac".into(),
        size: Some(value),
        token: ctx.token.clone().map(Box::new),
    })
}

fn abovefrac_handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    _opt: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let mut it = args.into_iter();
    let numer = it.next().unwrap();
    let infix = it.next().unwrap();
    let denom = it.next().unwrap();
    let ParseNode::Infix { size, .. } = infix else {
        return type_error("infix", &infix);
    };
    let Some(bar_size) = size else {
        return ierr("\\\\abovefrac expected size, but got undefined");
    };
    Ok(ParseNode::GenFrac(Box::new(GenFracNode {
        mode: ctx.parser.mode,
        continued: false,
        numer: Box::new(numer),
        denom: Box::new(denom),
        has_bar_line: bar_size.number > 0.0,
        bar_size: Some(bar_size),
        left_delim: None,
        right_delim: None,
    })))
}

pub static SPECS: &[FunctionSpec] = &[
    FunctionSpec {
        node_type: "genfrac",
        names: &[
            "\\cfrac",
            "\\dfrac",
            "\\frac",
            "\\tfrac",
            "\\dbinom",
            "\\binom",
            "\\tbinom",
            "\\\\atopfrac",
            "\\\\bracefrac",
            "\\\\brackfrac",
        ],
        num_args: 2,
        allowed_in_argument: true,
        handler: Some(genfrac_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "infix",
        names: &["\\over", "\\choose", "\\atop", "\\brace", "\\brack"],
        num_args: 0,
        infix: true,
        handler: Some(infix_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "genfrac",
        names: &["\\genfrac"],
        num_args: 6,
        allowed_in_argument: true,
        arg_types: &[
            ArgType::Math,
            ArgType::Math,
            ArgType::Size,
            ArgType::Text,
            ArgType::Math,
            ArgType::Math,
        ],
        handler: Some(genfrac_full_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "infix",
        names: &["\\above"],
        num_args: 1,
        arg_types: &[ArgType::Size],
        infix: true,
        handler: Some(above_handler),
        ..FunctionSpec::DEFAULT
    },
    FunctionSpec {
        node_type: "genfrac",
        names: &["\\\\abovefrac"],
        num_args: 3,
        arg_types: &[ArgType::Math, ArgType::Size, ArgType::Math],
        handler: Some(abovefrac_handler),
        ..FunctionSpec::DEFAULT
    },
];
