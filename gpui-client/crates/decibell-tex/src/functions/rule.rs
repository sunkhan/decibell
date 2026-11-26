//! Port of KaTeX `src/functions/rule.ts`.

use crate::build_common::make_span_s;
use crate::define_function::{ArgType, FunctionContext, FunctionSpec};
use crate::dom_tree::{CssProp, CssStyle, HtmlNode};
use crate::options::Options;
use crate::parse_error::Result;
use crate::parse_node::{ParseNode, type_error};
use crate::units::{calculate_size, make_em};

fn handler(
    ctx: &mut FunctionContext,
    args: Vec<ParseNode>,
    opt_args: Vec<Option<ParseNode>>,
) -> Result<ParseNode> {
    let shift = match opt_args.into_iter().next().flatten() {
        Some(ParseNode::Size { value, .. }) => Some(value),
        Some(other) => return type_error("size", &other),
        None => None,
    };
    let mut it = args.into_iter();
    let w = it.next().unwrap();
    let ParseNode::Size { value: width, .. } = w else {
        return type_error("size", &w);
    };
    let h = it.next().unwrap();
    let ParseNode::Size { value: height, .. } = h else {
        return type_error("size", &h);
    };
    Ok(ParseNode::Rule {
        mode: ctx.parser.mode,
        shift,
        width,
        height,
    })
}

pub fn html_builder(group: &ParseNode, options: &Options) -> Result<HtmlNode> {
    let ParseNode::Rule {
        shift,
        width,
        height,
        ..
    } = group
    else {
        unreachable!()
    };
    let mut rule = make_span_s(
        vec!["mord", "katex-rule"],
        vec![],
        Some(options),
        CssStyle::new(),
    );
    let width = calculate_size(width, options)?;
    let height = calculate_size(height, options)?;
    let shift = match shift {
        Some(s) => calculate_size(s, options)?,
        None => 0.0,
    };
    rule.style.set(CssProp::BorderRightWidth, make_em(width));
    rule.style.set(CssProp::BorderTopWidth, make_em(height));
    rule.style.set(CssProp::Bottom, make_em(shift));
    rule.width = Some(width);
    rule.height = height + shift;
    rule.depth = -shift;
    rule.max_font_size = height * 1.125 * options.size_multiplier;
    Ok(HtmlNode::span(rule))
}

pub static SPECS: &[FunctionSpec] = &[FunctionSpec {
    node_type: "rule",
    names: &["\\rule"],
    num_args: 2,
    num_optional_args: 1,
    allowed_in_text: true,
    allowed_in_math: true,
    arg_types: &[ArgType::Size, ArgType::Size, ArgType::Size],
    handler: Some(handler),
    ..FunctionSpec::DEFAULT
}];
